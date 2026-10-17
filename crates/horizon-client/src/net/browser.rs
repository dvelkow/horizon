//! Browser networking uses asynchronous WebSocket events, never blocking reads.
//! The gateway forwards the binary messages as a stream to its fixed TCP server.
use super::{Event, brand, json_text};
use crate::auth::AuthSession;
#[cfg(target_arch = "wasm32")]
use horizon_platform as std;
use horizon_protocol::{
    PROTOCOL_VERSION,
    connection::{
        Counters,
        browser_transport::{MAX_BUFFERED_BYTES, PacketDecoder, PacketEncoder},
    },
    crypto::{generate_secret, offline_uuid},
    packets::{Empty, configuration as cfg, handshake, login, play},
    registry::Dynamic,
    rsa::RsaPublicKey,
    servers::parse_address,
    text::{Language, plain},
};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::Ordering,
        mpsc::{Receiver, Sender, channel},
    },
};
use wasm_bindgen::{JsCast, closure::Closure};
use web_sys::{BinaryType, CloseEvent, MessageEvent, WebSocket};

const MAX_QUEUED_EVENTS: usize = 4096;
const MAX_QUEUED_BYTES: usize = 16 << 20;
const MAX_SEND_BUFFER: u32 = 8 << 20;

fn send_bytes(socket: &WebSocket, bytes: &[u8]) -> Result<(), String> {
    // Rust's memory is shared with the streaming Workers. WebSocket.send does
    // not accept SharedArrayBuffer-backed views, so copy into an owned buffer.
    let buffer = js_sys::Uint8Array::new_with_length(bytes.len() as u32);
    buffer.copy_from(bytes);
    socket
        .send_with_array_buffer(&buffer.buffer())
        .map_err(|error| format!("Could not send to the game gateway: {error:?}"))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Connecting,
    Login,
    Configuration,
    Play,
    Closed,
}

struct State {
    socket: WebSocket,
    decoder: PacketDecoder,
    encoder: PacketEncoder,
    phase: Phase,
    send: Sender<(Event, usize)>,
    queued_events: usize,
    queued_bytes: usize,
    counters: Arc<Counters>,
    host: String,
    port: u16,
    name: String,
    uuid: u128,
    view_distance: i8,
    lang: Arc<Language>,
    registries: Dynamic,
    configuration_bytes: usize,
    joined: bool,
    timeout: Option<i32>,
}

struct Handlers {
    _open: Closure<dyn FnMut(web_sys::Event)>,
    _message: Closure<dyn FnMut(MessageEvent)>,
    _close: Closure<dyn FnMut(CloseEvent)>,
    _error: Closure<dyn FnMut(web_sys::Event)>,
    _timeout: Closure<dyn FnMut()>,
}

pub struct Session {
    events: Receiver<(Event, usize)>,
    state: Option<Rc<RefCell<State>>>,
    handlers: Option<Handlers>,
    pub address: String,
    pub name: String,
    pub counters: Arc<Mutex<Option<Arc<Counters>>>>,
}

impl Session {
    /// Browser play connects to the hosted world; a native in-process pipe
    /// cannot be driven by this asynchronous WebSocket session.
    pub fn connect_local(
        _pipe: horizon_protocol::connection::Pipe,
        name: &str,
        _view_distance: i8,
        _lang: Arc<Language>,
    ) -> Self {
        let (send, events) = channel();
        let _ = send.send((
            Event::Disconnected {
                reason:
                    "Local integrated worlds are unavailable in the browser. Join the hosted world."
                        .to_owned(),
                from_server: true,
            },
            0,
        ));
        Self {
            events,
            state: None,
            handlers: None,
            address: "local".to_owned(),
            name: name.to_owned(),
            counters: Arc::new(Mutex::new(None)),
        }
    }

    pub fn connect(
        address: &str,
        name: &str,
        auth: Option<Arc<AuthSession>>,
        view_distance: i8,
        lang: Arc<Language>,
    ) -> Self {
        let (send, events) = channel();
        let counters = Arc::new(Counters::default());
        let mut session = Self {
            events,
            state: None,
            handlers: None,
            address: address.to_owned(),
            name: name.to_owned(),
            counters: Arc::new(Mutex::new(Some(counters.clone()))),
        };
        let opened = endpoint(address).and_then(|(url, host, port)| {
            let socket =
                WebSocket::new(&url).map_err(|_| "Could not open the game WebSocket".to_owned())?;
            Ok((socket, host, port))
        });
        let (socket, host, port) = match opened {
            Ok(value) => value,
            Err(reason) => {
                let _ = send.send((
                    Event::Disconnected {
                        reason,
                        from_server: true,
                    },
                    0,
                ));
                return session;
            }
        };
        socket.set_binary_type(BinaryType::Arraybuffer);
        let state = Rc::new(RefCell::new(State {
            socket: socket.clone(),
            decoder: PacketDecoder::default(),
            encoder: PacketEncoder::default(),
            phase: Phase::Connecting,
            send,
            queued_events: 0,
            queued_bytes: 0,
            counters,
            host,
            port,
            name: name.to_owned(),
            uuid: auth.as_ref().map_or_else(|| offline_uuid(name), |a| a.uuid),
            view_distance,
            lang,
            registries: Dynamic::default(),
            configuration_bytes: 0,
            joined: false,
            timeout: None,
        }));
        let opened = state.clone();
        let open = Closure::wrap(Box::new(move |_: web_sys::Event| {
            let mut state = opened.borrow_mut();
            if state.phase != Phase::Connecting {
                return;
            }
            if let Err(reason) = state.open() {
                state.finish(reason, false);
            }
        }) as Box<dyn FnMut(web_sys::Event)>);
        socket.set_onopen(Some(open.as_ref().unchecked_ref()));

        let incoming = state.clone();
        let message = Closure::wrap(Box::new(move |event: MessageEvent| {
            let mut state = incoming.borrow_mut();
            if state.phase == Phase::Closed {
                return;
            }
            let result = event
                .data()
                .dyn_into::<js_sys::ArrayBuffer>()
                .map_err(|_| "The game gateway sent a non-binary message".to_owned())
                .and_then(|buffer| {
                    if buffer.byte_length() as usize > MAX_BUFFERED_BYTES {
                        return Err("Game gateway message exceeded its size limit".to_owned());
                    }
                    state.receive(&js_sys::Uint8Array::new(&buffer).to_vec())
                });
            if let Err(reason) = result {
                state.finish(reason, false);
            }
        }) as Box<dyn FnMut(MessageEvent)>);
        socket.set_onmessage(Some(message.as_ref().unchecked_ref()));

        let closed = state.clone();
        let close = Closure::wrap(Box::new(move |event: CloseEvent| {
            closed.borrow_mut().finish(
                format!("Game connection closed (code {})", event.code()),
                false,
            );
        }) as Box<dyn FnMut(CloseEvent)>);
        socket.set_onclose(Some(close.as_ref().unchecked_ref()));
        let errored = state.clone();
        let error = Closure::wrap(Box::new(move |_: web_sys::Event| {
            errored
                .borrow_mut()
                .finish("Could not reach the game gateway".to_owned(), false);
        }) as Box<dyn FnMut(web_sys::Event)>);
        socket.set_onerror(Some(error.as_ref().unchecked_ref()));
        let timed = state.clone();
        let timeout = Closure::wrap(Box::new(move || {
            let mut state = timed.borrow_mut();
            state.timeout = None;
            if !state.joined {
                state.finish("Timed out joining the game".to_owned(), false);
            }
        }) as Box<dyn FnMut()>);
        if let Some(window) = web_sys::window() {
            state.borrow_mut().timeout = window
                .set_timeout_with_callback_and_timeout_and_arguments_0(
                    timeout.as_ref().unchecked_ref(),
                    20_000,
                )
                .ok();
        }
        session.state = Some(state);
        session.handlers = Some(Handlers {
            _open: open,
            _message: message,
            _close: close,
            _error: error,
            _timeout: timeout,
        });
        session
    }

    pub fn poll(&self) -> Vec<Event> {
        let mut bytes = 0;
        let events: Vec<_> = self
            .events
            .try_iter()
            .map(|(event, size)| {
                bytes += size;
                event
            })
            .collect();
        if let Some(state) = &self.state {
            let mut state = state.borrow_mut();
            state.queued_events = state.queued_events.saturating_sub(events.len());
            state.queued_bytes = state.queued_bytes.saturating_sub(bytes);
        }
        events
    }

    pub fn send(&self, packet: play::Serverbound) {
        if let Some(state) = &self.state {
            let mut state = state.borrow_mut();
            if state.phase == Phase::Play
                && let Err(reason) = state.write(&packet.encode())
            {
                state.finish(reason, false);
            }
        }
    }

    pub fn disconnect(&mut self) {
        if let Some(state) = self.state.take() {
            let mut state = state.borrow_mut();
            state.socket.set_onopen(None);
            state.socket.set_onmessage(None);
            state.socket.set_onclose(None);
            state.socket.set_onerror(None);
            state.finish("Disconnected".to_owned(), false);
        }
        self.handlers = None;
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.disconnect();
    }
}

impl State {
    fn event(&mut self, event: Event, size: usize) -> Result<(), String> {
        if self.queued_events >= MAX_QUEUED_EVENTS
            || size > MAX_QUEUED_BYTES.saturating_sub(self.queued_bytes)
        {
            return Err("Game receive queue exceeded its limit".to_owned());
        }
        self.send
            .send((event, size))
            .map_err(|_| "Disconnected".to_owned())?;
        self.queued_events += 1;
        self.queued_bytes += size;
        Ok(())
    }

    fn finish(&mut self, reason: String, from_server: bool) {
        if self.phase == Phase::Closed {
            return;
        }
        self.phase = Phase::Closed;
        if let Some(timer) = self.timeout.take()
            && let Some(window) = web_sys::window()
        {
            window.clear_timeout_with_handle(timer);
        }
        let _ = self.socket.close();
        let _ = self.send.send((
            Event::Disconnected {
                reason,
                from_server,
            },
            0,
        ));
    }

    fn write(&mut self, body: &[u8]) -> Result<(), String> {
        let bytes = self.encoder.encode(body).map_err(|e| e.to_string())?;
        if bytes.len() > MAX_SEND_BUFFER.saturating_sub(self.socket.buffered_amount()) as usize {
            return Err("Game send queue exceeded its limit".to_owned());
        }
        send_bytes(&self.socket, &bytes)?;
        self.counters.sent.fetch_add(1, Ordering::Relaxed);
        self.counters
            .bytes_out
            .fetch_add(bytes.len() as i64, Ordering::Relaxed);
        Ok(())
    }

    fn open(&mut self) -> Result<(), String> {
        self.phase = Phase::Login;
        self.event(Event::Status("Logging in...".to_owned()), 0)?;
        self.write(
            &handshake::Serverbound::Intention(handshake::Intention {
                protocol: PROTOCOL_VERSION,
                host: self.host.clone(),
                port: self.port,
                intent: handshake::LOGIN,
            })
            .encode(),
        )?;
        self.write(
            &login::Serverbound::Hello(login::ServerHello {
                name: self.name.clone(),
                uuid: self.uuid,
            })
            .encode(),
        )
    }

    fn receive(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.counters
            .bytes_in
            .fetch_add(bytes.len() as i64, Ordering::Relaxed);
        self.decoder.push(bytes).map_err(|e| e.to_string())?;
        let mut count = 0;
        while self.phase != Phase::Closed {
            let Some((id, payload)) = self.decoder.next_packet().map_err(|e| e.to_string())? else {
                break;
            };
            count += 1;
            if count > MAX_QUEUED_EVENTS {
                return Err("Too many packets in one gateway message".to_owned());
            }
            self.counters.received.fetch_add(1, Ordering::Relaxed);
            match self.phase {
                Phase::Login => self.login(id, &payload)?,
                Phase::Configuration => self.configure(id, &payload)?,
                Phase::Play => self.play(id, &payload)?,
                _ => return Err("Unexpected packet before the connection opened".to_owned()),
            }
        }
        Ok(())
    }

    fn login(&mut self, id: i32, payload: &[u8]) -> Result<(), String> {
        match login::Clientbound::decode(id, payload).map_err(|e| e.to_string())? {
            login::Clientbound::Compression(packet) => {
                let threshold = usize::try_from(packet.threshold).ok();
                self.decoder.threshold = threshold;
                self.encoder.threshold = threshold;
            }
            login::Clientbound::LoginFinished(packet) => {
                self.name = packet.profile.name;
                self.uuid = packet.profile.uuid;
                self.write(&login::Serverbound::LoginAcknowledged(Empty).encode())?;
                self.phase = Phase::Configuration;
                self.event(Event::Status("Joining world...".to_owned()), 0)?;
                self.write(
                    &cfg::Serverbound::ClientInformation(cfg::ClientInformation::new(
                        self.view_distance,
                    ))
                    .encode(),
                )?;
                self.write(
                    &cfg::Serverbound::CustomPayload(cfg::CustomPayload {
                        channel: "minecraft:brand".to_owned(),
                        data: brand(),
                    })
                    .encode(),
                )?;
            }
            login::Clientbound::Disconnect(packet) => {
                let reason = serde_json::from_str(&packet.reason_json)
                    .map(|value| json_text(&value, &self.lang))
                    .unwrap_or(packet.reason_json);
                self.finish(reason, true);
            }
            login::Clientbound::Hello(packet) => {
                if packet.should_authenticate {
                    self.finish("This browser gateway does not provide Microsoft session authentication. Join its configured private world.".to_owned(), true);
                    return Ok(());
                }
                let key = RsaPublicKey::from_der(&packet.public_key).map_err(|e| e.to_string())?;
                let secret = generate_secret().map_err(|e| e.to_string())?;
                self.write(
                    &login::Serverbound::Key(login::Key {
                        key: key.encrypt(&secret).map_err(|e| e.to_string())?,
                        challenge: key.encrypt(&packet.challenge).map_err(|e| e.to_string())?,
                    })
                    .encode(),
                )?;
                self.decoder.enable_encryption(&secret);
                self.encoder.enable_encryption(&secret);
            }
            login::Clientbound::CustomQuery(packet) => self.write(
                &login::Serverbound::CustomQueryAnswer(login::CustomQueryAnswer {
                    transaction: packet.transaction,
                    payload: None,
                })
                .encode(),
            )?,
            login::Clientbound::CookieRequest(packet) => self.write(
                &login::Serverbound::CookieResponse(login::CookieResponse {
                    key: packet.key,
                    payload: None,
                })
                .encode(),
            )?,
            login::Clientbound::Unknown(..) => {}
        }
        Ok(())
    }

    fn brand(&mut self, packet: &cfg::CustomPayload) -> Result<(), String> {
        if packet.channel == "minecraft:brand"
            && let Ok(value) = horizon_protocol::buf::Reader::new(&packet.data).string(32767)
        {
            self.event(Event::Brand(value), packet.data.len())?;
        }
        Ok(())
    }

    fn configure(&mut self, id: i32, payload: &[u8]) -> Result<(), String> {
        if payload.len() > MAX_QUEUED_BYTES.saturating_sub(self.configuration_bytes) {
            return Err("Server configuration exceeded its size limit".to_owned());
        }
        self.configuration_bytes += payload.len();
        let reply = match cfg::Clientbound::decode(id, payload).map_err(|e| e.to_string())? {
            cfg::Clientbound::KeepAlive(packet) => Some(cfg::Serverbound::KeepAlive(packet)),
            cfg::Clientbound::Ping(packet) => Some(cfg::Serverbound::Pong(packet)),
            cfg::Clientbound::CustomPayload(packet) => {
                self.brand(&packet)?;
                None
            }
            cfg::Clientbound::SelectKnownPacks(_) => {
                Some(cfg::Serverbound::SelectKnownPacks(cfg::SelectKnownPacks {
                    packs: vec![],
                }))
            }
            cfg::Clientbound::RegistryData(packet) => {
                self.registries.add(packet);
                None
            }
            cfg::Clientbound::CodeOfConduct(_) => {
                Some(cfg::Serverbound::AcceptCodeOfConduct(Empty))
            }
            cfg::Clientbound::ResourcePackPush(packet) => {
                Some(cfg::Serverbound::ResourcePack(cfg::ResourcePackResponse {
                    id: packet.id,
                    action: 1,
                }))
            }
            cfg::Clientbound::CookieRequest(packet) => {
                Some(cfg::Serverbound::CookieResponse(cfg::CookieResponse {
                    key: packet.key,
                    payload: None,
                }))
            }
            cfg::Clientbound::Disconnect(packet) => {
                self.finish(plain(&packet.reason, &self.lang), true);
                None
            }
            cfg::Clientbound::FinishConfiguration(_) => {
                self.write(&cfg::Serverbound::FinishConfiguration(Empty).encode())?;
                let registries = Arc::new(std::mem::take(&mut self.registries));
                let event = if self.joined {
                    Event::Reconfigured(registries)
                } else {
                    Event::Joined {
                        registries,
                        uuid: self.uuid,
                        name: self.name.clone(),
                    }
                };
                self.event(event, self.configuration_bytes)?;
                self.configuration_bytes = 0;
                self.joined = true;
                self.phase = Phase::Play;
                if let Some(timer) = self.timeout.take()
                    && let Some(window) = web_sys::window()
                {
                    window.clear_timeout_with_handle(timer);
                }
                None
            }
            _ => None,
        };
        if let Some(reply) = reply {
            self.write(&reply.encode())?;
        }
        Ok(())
    }

    fn play(&mut self, id: i32, payload: &[u8]) -> Result<(), String> {
        let packet = match play::Clientbound::decode(id, payload) {
            Ok(packet) => packet,
            Err(error) => return self.event(Event::DecodeError(error.to_string()), payload.len()),
        };
        let reply = match packet {
            play::Clientbound::KeepAlive(packet) => Some(play::Serverbound::KeepAlive(packet)),
            play::Clientbound::Ping(packet) => Some(play::Serverbound::Pong(packet)),
            play::Clientbound::BundleDelimiter(_) => None,
            play::Clientbound::ResourcePackPush(packet) => {
                Some(play::Serverbound::ResourcePack(cfg::ResourcePackResponse {
                    id: packet.id,
                    action: 1,
                }))
            }
            play::Clientbound::StartConfiguration(_) => {
                self.phase = Phase::Configuration;
                self.registries = Dynamic::default();
                self.configuration_bytes = 0;
                Some(play::Serverbound::ConfigurationAcknowledged(Empty))
            }
            play::Clientbound::Disconnect(packet) => {
                self.finish(plain(&packet.reason, &self.lang), true);
                None
            }
            play::Clientbound::CustomPayload(packet) if packet.channel == "minecraft:brand" => {
                self.brand(&packet)?;
                None
            }
            packet => {
                self.event(Event::Packet(Box::new(packet)), payload.len())?;
                None
            }
        };
        if let Some(reply) = reply {
            self.write(&reply.encode())?;
        }
        Ok(())
    }
}

fn endpoint(address: &str) -> Result<(String, String, u16), String> {
    let location = web_sys::window().ok_or("No browser window")?.location();
    let hostname = location
        .hostname()
        .map_err(|_| "Cannot read the game host")?;
    if address.starts_with("friend:") {
        return Err(
            "Native friend invite tunnels are not available in the browser. Use the hosted world."
                .to_owned(),
        );
    }
    if address.starts_with("wss://") || address.starts_with("ws://") {
        if location.protocol().ok().as_deref() == Some("https:") && address.starts_with("ws://") {
            return Err("An HTTPS game requires a secure WebSocket connection".to_owned());
        }
        return Ok((address.to_owned(), hostname, 25565));
    }
    let scheme = if location.protocol().ok().as_deref() == Some("https:") {
        "wss"
    } else {
        "ws"
    };
    let host = location.host().map_err(|_| "Cannot read the game host")?;
    let (target, port) = parse_address(address).unwrap_or((hostname, 25565));
    Ok((format!("{scheme}://{host}/game"), target, port))
}

pub fn shown_address(address: &str) -> String {
    if address.starts_with("friend:") {
        "Friend's world".to_owned()
    } else {
        address.split('?').next().unwrap_or(address).to_owned()
    }
}

pub fn friends_relay_setting() -> Option<String> {
    web_sys::window()?
        .local_storage()
        .ok()??
        .get_item("horizon.friendsRelay")
        .ok()?
}

pub fn save_friends_relay(relay: &str) {
    if let Some(storage) =
        web_sys::window().and_then(|window| window.local_storage().ok().flatten())
    {
        let _ = if relay.is_empty() {
            storage.remove_item("horizon.friendsRelay")
        } else {
            storage.set_item("horizon.friendsRelay", relay)
        };
    }
}

/// Retained only for callers that have not yet adopted `ping_async`. Blocking
/// a browser event loop would prevent the WebSocket response from arriving.
pub fn ping(
    _address: &str,
    _timeout: std::time::Duration,
) -> Result<(serde_json::Value, u128), String> {
    Err("Server status requires an asynchronous request in the browser".to_owned())
}

/// Query the same gateway with the ordinary Minecraft status protocol.
pub async fn ping_async(
    address: &str,
    timeout: std::time::Duration,
) -> Result<(serde_json::Value, u128), String> {
    use horizon_platform::time::Instant;
    use horizon_protocol::packets::status;
    use wasm_bindgen::JsValue;

    type StatusResult = Result<(serde_json::Value, u128), String>;
    struct Probe {
        socket: WebSocket,
        decoder: PacketDecoder,
        encoder: PacketEncoder,
        wake: Option<js_sys::Function>,
        result: Option<StatusResult>,
        json: Option<serde_json::Value>,
        started: Option<Instant>,
        timer: Option<i32>,
    }
    impl Probe {
        fn finish(&mut self, result: StatusResult) {
            if self.result.is_some() {
                return;
            }
            self.result = Some(result);
            if let Some(wake) = self.wake.take() {
                let _ = wake.call0(&JsValue::UNDEFINED);
            }
        }
        fn write(&mut self, packet: &[u8]) -> Result<(), String> {
            let bytes = self.encoder.encode(packet).map_err(|e| e.to_string())?;
            send_bytes(&self.socket, &bytes)
        }
        fn receive(&mut self, event: MessageEvent) -> Result<(), String> {
            let buffer = event
                .data()
                .dyn_into::<js_sys::ArrayBuffer>()
                .map_err(|_| "The status gateway sent non-binary data")?;
            if buffer.byte_length() > 1 << 20 {
                return Err("Server status exceeded its size limit".to_owned());
            }
            self.decoder
                .push(&js_sys::Uint8Array::new(&buffer).to_vec())
                .map_err(|e| e.to_string())?;
            let mut count = 0;
            while let Some((id, payload)) = self.decoder.next_packet().map_err(|e| e.to_string())? {
                count += 1;
                if count > 2 {
                    return Err("Unexpected additional status packets".to_owned());
                }
                match status::Clientbound::decode(id, &payload).map_err(|e| e.to_string())? {
                    status::Clientbound::StatusResponse(packet) if self.json.is_none() => {
                        self.json =
                            Some(serde_json::from_str(&packet.json).map_err(|e| e.to_string())?);
                        self.started = Some(Instant::now());
                        self.write(
                            &status::Serverbound::PingRequest(status::Ping { time: 1 }).encode(),
                        )?;
                    }
                    status::Clientbound::PongResponse(packet)
                        if packet.time == 1 && self.started.is_some() =>
                    {
                        let elapsed = self
                            .started
                            .take()
                            .expect("checked above")
                            .elapsed()
                            .as_millis();
                        let json = self.json.take().ok_or("Status response missing")?;
                        self.finish(Ok((json, elapsed)));
                        break;
                    }
                    _ => return Err("Unexpected server status response".to_owned()),
                }
            }
            Ok(())
        }
    }
    // Own the closures for the lifetime of the future, including cancellation.
    struct Guard {
        probe: Rc<RefCell<Probe>>,
        _handlers: Handlers,
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            let mut probe = self.probe.borrow_mut();
            probe.socket.set_onopen(None);
            probe.socket.set_onmessage(None);
            probe.socket.set_onclose(None);
            probe.socket.set_onerror(None);
            if let Some(timer) = probe.timer.take()
                && let Some(window) = web_sys::window()
            {
                window.clear_timeout_with_handle(timer);
            }
            let _ = probe.socket.close();
        }
    }
    let (url, host, port) = endpoint(address)?;
    let socket = WebSocket::new(&url).map_err(|_| "Could not open the status WebSocket")?;
    socket.set_binary_type(BinaryType::Arraybuffer);
    let probe = Rc::new(RefCell::new(Probe {
        socket: socket.clone(),
        decoder: PacketDecoder::default(),
        encoder: PacketEncoder::default(),
        wake: None,
        result: None,
        json: None,
        started: None,
        timer: None,
    }));
    let pending = probe.clone();
    let signal = js_sys::Promise::new(&mut move |resolve, _reject| {
        pending.borrow_mut().wake = Some(resolve);
    });
    let opened = probe.clone();
    let open = Closure::wrap(Box::new(move |_: web_sys::Event| {
        let mut probe = opened.borrow_mut();
        let result = probe
            .write(
                &handshake::Serverbound::Intention(handshake::Intention {
                    protocol: PROTOCOL_VERSION,
                    host: host.clone(),
                    port,
                    intent: handshake::STATUS,
                })
                .encode(),
            )
            .and_then(|_| probe.write(&status::Serverbound::StatusRequest(Empty).encode()));
        if let Err(reason) = result {
            probe.finish(Err(reason));
        }
    }) as Box<dyn FnMut(web_sys::Event)>);
    socket.set_onopen(Some(open.as_ref().unchecked_ref()));
    let incoming = probe.clone();
    let message = Closure::wrap(Box::new(move |event: MessageEvent| {
        let mut probe = incoming.borrow_mut();
        if probe.result.is_none()
            && let Err(reason) = probe.receive(event)
        {
            probe.finish(Err(reason));
        }
    }) as Box<dyn FnMut(MessageEvent)>);
    socket.set_onmessage(Some(message.as_ref().unchecked_ref()));
    let closed = probe.clone();
    let close = Closure::wrap(Box::new(move |_: CloseEvent| {
        closed
            .borrow_mut()
            .finish(Err("Status connection closed".to_owned()));
    }) as Box<dyn FnMut(CloseEvent)>);
    socket.set_onclose(Some(close.as_ref().unchecked_ref()));
    let errored = probe.clone();
    let error = Closure::wrap(Box::new(move |_: web_sys::Event| {
        errored
            .borrow_mut()
            .finish(Err("Could not reach the status gateway".to_owned()));
    }) as Box<dyn FnMut(web_sys::Event)>);
    socket.set_onerror(Some(error.as_ref().unchecked_ref()));
    let timed = probe.clone();
    let timer = Closure::wrap(Box::new(move || {
        timed
            .borrow_mut()
            .finish(Err("Server status timed out".to_owned()));
    }) as Box<dyn FnMut()>);
    if let Some(window) = web_sys::window() {
        probe.borrow_mut().timer = window
            .set_timeout_with_callback_and_timeout_and_arguments_0(
                timer.as_ref().unchecked_ref(),
                timeout.as_millis().min(i32::MAX as u128) as i32,
            )
            .ok();
    }
    let _guard = Guard {
        probe: probe.clone(),
        _handlers: Handlers {
            _open: open,
            _message: message,
            _close: close,
            _error: error,
            _timeout: timer,
        },
    };
    wasm_bindgen_futures::JsFuture::from(signal)
        .await
        .map_err(|_| "Status request interrupted")?;
    let result = probe
        .borrow_mut()
        .result
        .take()
        .ok_or("Status response missing")?;
    result
}
