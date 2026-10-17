//! Browser fetch transport. Authentication uses only the official HTTPS
//! discovery service; neither credentials nor request bodies are logged.
use super::{AuthSession, JoinError};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::JsFuture;

struct Timeout {
    window: web_sys::Window,
    id: i32,
    _callback: Closure<dyn FnMut()>,
}
impl Drop for Timeout {
    fn drop(&mut self) {
        self.window.clear_timeout_with_handle(self.id);
    }
}

async fn request(
    url: &str,
    body: Option<&str>,
    maximum: usize,
) -> Result<(u16, Vec<u8>), JoinError> {
    let unavailable = |_| JoinError::Unavailable;
    let window = web_sys::window().ok_or(JoinError::Unavailable)?;
    let controller = web_sys::AbortController::new().map_err(unavailable)?;
    let abort = controller.clone();
    let callback = Closure::wrap(Box::new(move || abort.abort()) as Box<dyn FnMut()>);
    let id = window
        .set_timeout_with_callback_and_timeout_and_arguments_0(
            callback.as_ref().unchecked_ref(),
            15_000,
        )
        .map_err(unavailable)?;
    let _timeout = Timeout {
        window: window.clone(),
        id,
        _callback: callback,
    };
    let init = web_sys::RequestInit::new();
    init.set_signal(Some(&controller.signal()));
    init.set_credentials(web_sys::RequestCredentials::Omit);
    init.set_redirect(web_sys::RequestRedirect::Error);
    init.set_mode(web_sys::RequestMode::Cors);
    if let Some(body) = body {
        init.set_method("POST");
        init.set_body(&JsValue::from_str(body));
    }
    let request = web_sys::Request::new_with_str_and_init(url, &init).map_err(unavailable)?;
    if body.is_some() {
        request
            .headers()
            .set("Content-Type", "application/json")
            .map_err(unavailable)?;
    }
    let response = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(unavailable)?
        .dyn_into::<web_sys::Response>()
        .map_err(unavailable)?;
    let status = response.status();
    if response
        .headers()
        .get("Content-Length")
        .ok()
        .flatten()
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|size| size > maximum)
    {
        controller.abort();
        return Err(JoinError::Unavailable);
    }
    let Some(stream) = response.body() else {
        return Ok((status, Vec::new()));
    };
    let reader = web_sys::ReadableStreamDefaultReader::new(&stream).map_err(unavailable)?;
    let mut bytes = Vec::new();
    loop {
        let chunk = JsFuture::from(reader.read()).await.map_err(unavailable)?;
        if js_sys::Reflect::get(&chunk, &JsValue::from_str("done"))
            .map_err(unavailable)?
            .as_bool()
            == Some(true)
        {
            break;
        }
        let value =
            js_sys::Reflect::get(&chunk, &JsValue::from_str("value")).map_err(unavailable)?;
        let chunk = js_sys::Uint8Array::new(&value);
        if chunk.length() as usize > maximum.saturating_sub(bytes.len()) {
            controller.abort();
            reader.release_lock();
            return Err(JoinError::Unavailable);
        }
        bytes.extend(chunk.to_vec());
    }
    reader.release_lock();
    Ok((status, bytes))
}

/// Bounded resource downloads; a CORS or network refusal returns no resource.
pub(crate) async fn get_bytes(url: &str, maximum: usize) -> Option<Vec<u8>> {
    let (status, bytes) = request(url, None, maximum).await.ok()?;
    (200..300).contains(&status).then_some(bytes)
}

/// Complete the same online-mode join as native, without blocking the UI.
/// The official services must permit CORS; this never sends tokens to a proxy.
pub async fn join_server(session: &AuthSession, server_hash: &str) -> Result<(), JoinError> {
    let bytes = get_bytes(super::DEFAULT_DISCOVERY, 1 << 20)
        .await
        .ok_or(JoinError::Unavailable)?;
    let document: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| JoinError::Unavailable)?;
    let endpoint = super::endpoint(&document, "session", "join").ok_or(JoinError::Unavailable)?;
    let url = web_sys::Url::new(&endpoint).map_err(|_| JoinError::Unavailable)?;
    if url.protocol() != "https:"
        || !matches!(
            url.hostname().as_str(),
            "api.minecraftservices.com" | "sessionserver.mojang.com"
        )
        || !url.username().is_empty()
        || !url.password().is_empty()
    {
        return Err(JoinError::Unavailable);
    }
    let (status, bytes) = request(
        &endpoint,
        Some(&super::join_request(session, server_hash)),
        1 << 20,
    )
    .await?;
    if (200..300).contains(&status) {
        Ok(())
    } else {
        Err(JoinError::from_response(
            status,
            &String::from_utf8_lossy(&bytes),
        ))
    }
}
