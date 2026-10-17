//! Loading and calling mods with wasmtime's component model.
use crate::manifest::{Manifest, Package, Side};
#[cfg(target_arch = "wasm32")]
use horizon_platform as std;
#[allow(unused_imports)]
use horizon_platform::PathExt;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};
use wasmtime::{
    Config, Engine, Store, StoreLimits, StoreLimitsBuilder, Trap,
    component::{Component, HasSelf, Linker},
};

mod bindings {
    wasmtime::component::bindgen!({
        path: "../horizon-mod-sdk/wit",
        world: "horizon-mod",
    });
}
pub use bindings::horizon::mods::{
    game::{Clock, Player},
    hud::{Corner, Line},
    movement::Motion,
};
use bindings::{
    HorizonMod,
    exports::horizon::mods::events::Registration,
    horizon::mods::{chat, game, hud, log as mod_log, movement},
};
use crate::Nudge;

/// A mod's linear memory cap.
const MEMORY: usize = 64 << 20;
/// Fuel (about one unit per WebAssembly instruction) per call: a bound on
/// runaway loops, far above the hooks' time budgets.
const FUEL_INIT: u64 = 2_000_000_000;
const FUEL_TICK: u64 = 50_000_000;
const FUEL_FRAME: u64 = 10_000_000;
const FUEL_EVENT: u64 = 100_000_000;
/// Chat lines a mod may show per call, HUD lines and their length.
const CHAT_PER_CALL: usize = 8;
const HUD_LINES: usize = 16;
const LINE_CHARS: usize = 256;

/// What mods may read of the game, copied once per tick by the game.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub player: Option<Player>,
    pub clock: Option<Clock>,
    pub motion: Option<Motion>,
}

/// What mods asked the game to show since the last [`Mods::drain`].
#[derive(Clone, Debug, PartialEq)]
pub enum Output {
    /// `chat.show`: a line after the mod's name.
    Chat { name: String, text: String },
    /// A mod command's feedback, red when `error`.
    Feedback { text: String, error: bool },
    /// The mod was turned off: it trapped, overran or failed to start.
    Stopped { name: String, reason: String },
}

/// A mod's text panel, drawn every frame until it changes.
#[derive(Clone, Debug)]
pub struct Panel {
    pub corner: Corner,
    pub lines: Vec<Line>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// Installed and turned off by the player.
    Off,
    Running,
    /// Could not load or start, or stopped while running.
    Failed(String),
}

/// One installed mod, for the Mods page.
pub struct Entry {
    pub path: PathBuf,
    /// None when the manifest could not be read (`status` says why).
    pub manifest: Option<Manifest>,
    pub status: Status,
    /// The mod's average time per call of its busiest hook.
    pub cost: Duration,
    run: Option<Running>,
    modified: Option<SystemTime>,
    fingerprint: Option<[u8; 32]>,
    /// In an approval mirror: whether these bytes read as a package, and the
    /// id of the content mod they are (`content::approved_id`), whether or
    /// not its code started.
    readable: bool,
    content: Option<String>,
}

struct Running {
    store: Store<State>,
    exports: HorizonMod,
    hooks: Registration,
}

struct State {
    side: Side,
    name: String,
    id: String,
    snapshot: Arc<Snapshot>,
    out: Vec<Output>,
    panel: Option<Panel>,
    nudge: Nudge,
    limits: StoreLimits,
}

/// The installed mods and the engine running them. `Mods::default()` is
/// the state with none: no engine, and every hook a no-op.
#[derive(Default)]
pub struct Mods {
    side: Side,
    engine: Option<(Engine, Linker<State>)>,
    entries: Vec<Entry>,
    snapshot: Arc<Snapshot>,
    out: Vec<Output>,
    nudge: Nudge,
    /// The approval mirror held a package that did not read at the last sync.
    unsettled: bool,
}

/// The `.hmod` files and mod folders in `dir`, by name.
pub fn find(dir: &Path) -> Vec<PathBuf> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = read
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|e| e == "hmod")
                || p.join(crate::manifest::MANIFEST).platform_is_file()
        })
        .collect();
    found.sort();
    found
}

impl Mods {
    /// Reads every mod at `paths` and starts those `enabled` says are on.
    /// A command that `reserved` (vanilla's commands) or an earlier mod
    /// already uses is refused.
    pub fn load(
        paths: &[PathBuf],
        enabled: impl Fn(&str) -> bool,
        reserved: impl Fn(&str) -> bool,
    ) -> Mods {
        Self::load_for(Side::Client, paths, enabled, reserved)
    }

    /// Native server instances accept Server and Both packages. Their game
    /// snapshot has a clock and no implicit local player.
    pub fn load_server(
        paths: &[PathBuf],
        enabled: impl Fn(&str) -> bool,
        reserved: impl Fn(&str) -> bool,
    ) -> Mods {
        Self::load_for(Side::Server, paths, enabled, reserved)
    }

    fn load_for(
        side: Side,
        paths: &[PathBuf],
        enabled: impl Fn(&str) -> bool,
        reserved: impl Fn(&str) -> bool,
    ) -> Mods {
        let mut mods = Mods {
            side,
            ..Mods::default()
        };
        for path in paths {
            let modified = modified(path);
            let mut entry = Entry {
                path: path.clone(),
                manifest: None,
                status: Status::Off,
                cost: Duration::ZERO,
                run: None,
                modified,
                fingerprint: None,
                readable: false,
                content: None,
            };
            match Package::read(path) {
                Err(e) => entry.status = Status::Failed(e),
                Ok(package) => {
                    let on = enabled(&package.manifest.id) && mods.accepts(package.manifest.side);
                    entry.manifest = Some(package.manifest.clone());
                    if on {
                        // Content is read before the world loads; a mod whose
                        // content is refused stays off, with the reason.
                        let content = crate::Content::read(path, &package.manifest);
                        entry.status =
                            match content.and_then(|_| mods.start(&package, &reserved, None)) {
                                Ok(run) => {
                                    entry.run = run;
                                    Status::Running
                                }
                                Err(e) => Status::Failed(e),
                            };
                    }
                }
            }
            if let (Status::Failed(reason), Some(m)) = (&entry.status, &entry.manifest) {
                log::warn!("Horizon: mod {} did not start: {reason}", m.id);
            }
            mods.entries.push(entry);
        }
        mods
    }

    fn accepts(&self, side: Side) -> bool {
        side == Side::Both || side == self.side
    }

    fn engine(&mut self) -> Result<&(Engine, Linker<State>), String> {
        if self.engine.is_none() {
            let mut config = Config::new();
            config.wasm_component_model(true).consume_fuel(true);
            let engine = Engine::new(&config).map_err(|e| e.to_string())?;
            let mut linker = Linker::new(&engine);
            HorizonMod::add_to_linker::<State, HasSelf<State>>(&mut linker, |s| s)
                .map_err(|e| e.to_string())?;
            self.engine = Some((engine, linker));
        }
        Ok(self.engine.as_ref().expect("made above"))
    }

    /// Starts a mod's code; `Ok(None)` for a content-only mod, which has
    /// nothing to run (the game reads its content before the world loads).
    fn start(
        &mut self,
        package: &Package,
        reserved: &dyn Fn(&str) -> bool,
        replacing: Option<usize>,
    ) -> Result<Option<Running>, String> {
        let m = &package.manifest;
        if !self.accepts(m.side) {
            return Err(format!(
                "mod side {:?} does not run on {:?}",
                m.side, self.side
            ));
        }
        if self.entries.iter().enumerate().any(|(index, entry)| {
            Some(index) != replacing
                && entry.run.is_some()
                && entry
                    .manifest
                    .as_ref()
                    .is_some_and(|manifest| manifest.id == m.id)
        }) {
            return Err(format!("mod {} is already running", m.id));
        }
        let Some(wasm) = &package.wasm else {
            return Ok(None);
        };
        let taken: Vec<String> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(index, _)| Some(*index) != replacing)
            .filter_map(|(_, e)| e.run.as_ref())
            .flat_map(|r| r.hooks.commands.iter().cloned())
            .collect();
        let snapshot = self.snapshot.clone();
        let side = self.side;
        let (engine, linker) = self.engine()?;
        let component =
            Component::new(engine, wasm).map_err(|e| format!("not a component: {e}"))?;
        let ty = component.component_type();
        let imports: Vec<String> = ty
            .imports(engine)
            .map(|(name, _)| name.to_string())
            .collect();
        m.check_imports(imports.iter().map(String::as_str))?;
        let state = State {
            side,
            name: m.name.clone(),
            id: m.id.clone(),
            snapshot,
            out: Vec::new(),
            panel: None,
            nudge: Nudge::default(),
            limits: StoreLimitsBuilder::new()
                .memory_size(MEMORY)
                .instances(16)
                .build(),
        };
        let mut store = Store::new(engine, state);
        store.limiter(|s| &mut s.limits);
        store.set_fuel(FUEL_INIT).map_err(|e| e.to_string())?;
        let exports =
            HorizonMod::instantiate(&mut store, &component, linker).map_err(|e| reason(&e))?;
        let mut hooks = exports
            .horizon_mods_events()
            .call_init(&mut store)
            .map_err(|e| reason(&e))??;
        hooks.commands.retain(|c| {
            let free = !reserved(c) && !taken.contains(c);
            if !free {
                log::warn!("Horizon: mod {} cannot take /{c}: already a command", m.id);
            }
            free
        });
        self.out.append(&mut store.data_mut().out);
        // There is no movement step to nudge before the first tick.
        store.data_mut().nudge = Nudge::default();
        Ok(Some(Running {
            store,
            exports,
            hooks,
        }))
    }

    /// True when at least one mod runs: the game skips its mod work otherwise.
    pub fn is_running(&self) -> bool {
        self.entries.iter().any(|e| e.run.is_some())
    }
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }
    /// What mods read until the next snapshot.
    pub fn set_snapshot(&mut self, snapshot: Snapshot) {
        self.snapshot = Arc::new(snapshot);
    }

    /// Calls `f` on every running mod whose hooks `wants`, with `fuel`.
    fn each(
        &mut self,
        fuel: u64,
        wants: impl Fn(&Registration) -> bool,
        mut f: impl FnMut(&HorizonMod, &mut Store<State>) -> wasmtime::Result<()>,
    ) {
        for i in 0..self.entries.len() {
            if self.entries[i]
                .run
                .as_ref()
                .is_some_and(|r| wants(&r.hooks))
            {
                self.call(i, fuel, &mut f);
            }
        }
    }

    /// One call into mod `i`; a trap or an overrun turns the mod off.
    fn call<R>(
        &mut self,
        i: usize,
        fuel: u64,
        f: impl FnOnce(&HorizonMod, &mut Store<State>) -> wasmtime::Result<R>,
    ) -> Option<R> {
        let entry = &mut self.entries[i];
        let run = entry.run.as_mut()?;
        run.store.data_mut().snapshot = self.snapshot.clone();
        let started = Instant::now();
        let result = run
            .store
            .set_fuel(fuel)
            .and_then(|()| f(&run.exports, &mut run.store));
        // An average over about the last 64 calls.
        entry.cost = (entry.cost * 63 + started.elapsed()) / 64;
        self.out.append(&mut run.store.data_mut().out);
        let nudge = std::mem::take(&mut run.store.data_mut().nudge);
        match result {
            Ok(r) => {
                self.nudge.merge(nudge);
                Some(r)
            }
            Err(e) => {
                let reason = reason(&e);
                log::warn!("Horizon: mod {} stopped: {reason}", run.store.data().id);
                let name = run.store.data().name.clone();
                self.out.push(Output::Stopped {
                    name,
                    reason: reason.clone(),
                });
                entry.status = Status::Failed(reason);
                entry.run = None;
                None
            }
        }
    }

    /// A game tick.
    pub fn tick(&mut self, tick: u64) {
        self.each(
            FUEL_TICK,
            |h| h.tick,
            |m, s| m.horizon_mods_events().call_tick(s, tick),
        );
    }
    /// A rendered frame, `seconds` after the last.
    pub fn frame(&mut self, seconds: f32) {
        self.each(
            FUEL_FRAME,
            |h| h.frame,
            |m, s| m.horizon_mods_events().call_frame(s, seconds),
        );
    }
    /// A chat line the player received, as plain text.
    pub fn chat(&mut self, text: &str) {
        self.each(
            FUEL_EVENT,
            |h| h.chat,
            |m, s| m.horizon_mods_events().call_chat(s, text),
        );
    }
    /// Whether a running mod owns `/name`.
    pub fn has_command(&self, name: &str) -> bool {
        self.command_owner(name).is_some()
    }
    fn command_owner(&self, name: &str) -> Option<usize> {
        self.entries.iter().position(|e| {
            e.run
                .as_ref()
                .is_some_and(|r| r.hooks.commands.iter().any(|c| c == name))
        })
    }
    /// Runs `line` (without its slash) if a mod owns its command; false
    /// when none does.
    pub fn command(&mut self, line: &str) -> bool {
        let (name, args) = line.split_once(' ').unwrap_or((line, ""));
        let Some(i) = self.command_owner(name) else {
            return false;
        };
        let result = self.call(i, FUEL_EVENT, |m, s| {
            m.horizon_mods_events().call_command(s, name, args)
        });
        match result {
            Some(Ok(Some(text))) => self.out.push(Output::Feedback { text, error: false }),
            Some(Err(text)) => self.out.push(Output::Feedback { text, error: true }),
            Some(Ok(None)) | None => {}
        }
        true
    }
    /// The commands running mods own, for suggestions.
    pub fn commands(&self) -> impl Iterator<Item = &str> {
        self.entries
            .iter()
            .filter_map(|e| e.run.as_ref())
            .flat_map(|r| r.hooks.commands.iter().map(String::as_str))
    }

    /// The running mods' HUD panels.
    pub fn panels(&self) -> impl Iterator<Item = &Panel> {
        self.entries
            .iter()
            .filter_map(|e| e.run.as_ref()?.store.data().panel.as_ref())
    }
    /// What mods asked to show since the last call.
    pub fn drain(&mut self) -> Vec<Output> {
        std::mem::take(&mut self.out)
    }
    /// The `movement` nudges mods asked for since the last call, for the
    /// player's next movement step.
    pub fn take_nudge(&mut self) -> Nudge {
        std::mem::take(&mut self.nudge)
    }

    /// Reload changed development files. A candidate must validate and finish
    /// init before it replaces an active instance; failed rebuilds keep it live.
    pub fn reload_changed(&mut self, reserved: impl Fn(&str) -> bool) -> bool {
        let mut any = false;
        for i in 0..self.entries.len() {
            let entry = &self.entries[i];
            let now = modified(&entry.path);
            if now == entry.modified || entry.status == Status::Off {
                continue;
            }
            self.entries[i].modified = now;
            let path = self.entries[i].path.clone();
            let candidate = Package::read(&path).and_then(|package| {
                crate::Content::read(&path, &package.manifest)?;
                self.start(&package, &reserved, Some(i))
                    .map(|run| (package.manifest, run))
            });
            any |= self.replace_candidate(i, candidate);
        }
        any
    }

    fn replace_candidate(
        &mut self,
        index: usize,
        candidate: Result<(Manifest, Option<Running>), String>,
    ) -> bool {
        let entry = &mut self.entries[index];
        match candidate {
            Ok((manifest, run)) => {
                log::info!("Horizon: mod {} loaded on {:?}", manifest.id, self.side);
                entry.manifest = Some(manifest);
                entry.run = run;
                entry.status = Status::Running;
                entry.cost = Duration::ZERO;
                true
            }
            Err(error) => {
                log::warn!(
                    "Horizon: mod {} update rejected: {error}; previous instance retained",
                    entry.path.display()
                );
                if entry.run.is_none() {
                    entry.status = Status::Failed(error);
                }
                false
            }
        }
    }

    /// Reconcile the private approval mirror. The publisher writes a temporary
    /// file then atomically renames it to `<id>.hmod`; uploads never live here.
    /// Directory/read failures preserve the last known working set. Removal
    /// from a successfully listed directory withdraws that mod.
    pub fn sync_server_directory(
        &mut self,
        directory: &Path,
        reserved: impl Fn(&str) -> bool,
    ) -> Result<bool, String> {
        if self.side != Side::Server {
            return Err("approval mirror requires a server runtime".into());
        }
        let mut paths = Vec::new();
        for entry in std::fs::read_dir(directory).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            if entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "hmod")
            {
                if !entry.file_type().map_err(|e| e.to_string())?.is_file() {
                    return Err("approved mods must be regular .hmod files".into());
                }
                paths.push(entry.path());
            }
        }
        if paths.len() > 256 {
            return Err("approved mod directory exceeds 256 packages".into());
        }
        paths.sort();
        let mut changed = false;
        let mut unsettled = false;
        for path in &paths {
            let fingerprint = match fingerprint(path) {
                Ok(hash) => hash,
                Err(error) => {
                    log::warn!("Horizon: approved mod read failed: {error}");
                    unsettled = true;
                    continue;
                }
            };
            let index = self
                .entries
                .iter()
                .position(|entry| entry.path == *path)
                .unwrap_or_else(|| {
                    self.entries.push(Entry {
                        path: path.clone(),
                        manifest: None,
                        status: Status::Off,
                        cost: Duration::ZERO,
                        run: None,
                        modified: modified(path),
                        fingerprint: None,
                        readable: false,
                        content: None,
                    });
                    self.entries.len() - 1
                });
            if self.entries[index].fingerprint == Some(fingerprint) {
                continue;
            }
            let package = Package::read(path);
            self.entries[index].readable = package.is_ok();
            self.entries[index].content = package
                .as_ref()
                .ok()
                .and_then(|p| crate::content::approved_id(path, &p.manifest));
            let candidate = package.and_then(|package| {
                if path.file_stem().and_then(|name| name.to_str())
                    != Some(package.manifest.id.as_str())
                {
                    return Err("approved filename must match the manifest id".into());
                }
                // Content (API 0.2) is read before the world loads; a change
                // to it restarts the world (Mods::approved_content). Its code
                // goes live here like any other.
                crate::Content::read(path, &package.manifest)?;
                // Recheck after parsing so an atomic publish racing this scan
                // cannot associate one package's bytes with another's hash.
                if self::fingerprint(path)? != fingerprint {
                    return Err(
                        "approved package changed during validation; retrying next scan".into(),
                    );
                }
                // The players' half of a content mod: the server keeps its
                // blocks and items, not its code.
                if !self.accepts(package.manifest.side)
                    && package
                        .manifest
                        .capabilities
                        .contains(&crate::Capability::Registries)
                {
                    return Ok((package.manifest, None));
                }
                self.start(&package, &reserved, Some(index))
                    .map(|run| (package.manifest, run))
            });
            // The digest only suppresses identical attempts; it grants no trust.
            // Manifest, content, import and init/fuel checks still run above.
            self.entries[index].fingerprint = Some(fingerprint);
            changed |= self.replace_candidate(index, candidate);
        }
        let before = self.entries.len();
        self.entries
            .retain(|entry| paths.binary_search(&entry.path).is_ok());
        self.unsettled = unsettled || self.entries.iter().any(|e| !e.readable);
        Ok(changed || self.entries.len() != before)
    }

    /// The content mods in the approval mirror as of the last
    /// [`Mods::sync_server_directory`], by id, with their packages' SHA-256.
    /// Blocks and items cannot join a running world: when this differs from
    /// the set the world started with, the dedicated server restarts. None
    /// while a package there does not read: a damaged file is not a removal,
    /// and restarting without its blocks would turn them into air.
    pub fn approved_content(&self) -> Option<Vec<(String, [u8; 32])>> {
        if self.unsettled {
            return None;
        }
        let mut out: Vec<_> = self
            .entries
            .iter()
            .filter_map(|e| Some((e.content.clone()?, e.fingerprint?)))
            .collect();
        out.sort();
        Some(out)
    }
}

/// The SHA-256 of a package's bytes, as [`Mods::approved_content`] lists it.
pub fn package_digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn fingerprint(path: &Path) -> Result<[u8; 32], String> {
    const MAX_PACKAGE: u64 = 64 << 20;
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(MAX_PACKAGE + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_PACKAGE {
        return Err("approved package exceeds 64 MiB".into());
    }
    Ok(Sha256::digest(bytes).into())
}

fn modified(path: &Path) -> Option<SystemTime> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.is_dir() {
        // A folder mod changes when its component does.
        return std::fs::metadata(path.join(crate::manifest::WASM))
            .ok()?
            .modified()
            .ok();
    }
    meta.modified().ok()
}

/// A trap or error, as the player reads it.
fn reason(e: &wasmtime::Error) -> String {
    match e.downcast_ref::<Trap>() {
        Some(Trap::OutOfFuel) => "it took too long (an endless loop?)".into(),
        Some(Trap::UnreachableCodeReached) => "it crashed (a panic)".into(),
        Some(trap) => format!("it crashed: {trap}"),
        None => format!("{e:#}"),
    }
}

fn clip(text: String) -> String {
    match text.char_indices().nth(LINE_CHARS) {
        Some((at, _)) => text[..at].to_string(),
        None => text,
    }
}

impl mod_log::Host for State {
    fn write(&mut self, level: mod_log::Level, message: String) {
        let level = match level {
            mod_log::Level::Debug => log::Level::Debug,
            mod_log::Level::Info => log::Level::Info,
            mod_log::Level::Warn => log::Level::Warn,
            mod_log::Level::Error => log::Level::Error,
        };
        log::log!(level, "[mod {}] {}", self.id, clip(message));
    }
}
impl game::Host for State {
    fn get_player(&mut self) -> Option<Player> {
        if self.side == Side::Server {
            None
        } else {
            self.snapshot.player.clone()
        }
    }
    fn get_clock(&mut self) -> Option<Clock> {
        self.snapshot.clock
    }
}
impl chat::Host for State {
    fn show(&mut self, text: String) {
        let shown = self
            .out
            .iter()
            .filter(|o| matches!(o, Output::Chat { .. }))
            .count();
        if shown < CHAT_PER_CALL {
            let name = self.name.clone();
            self.out.push(Output::Chat {
                name,
                text: clip(text),
            });
        }
    }
}
impl movement::Host for State {
    fn get_motion(&mut self) -> Option<Motion> {
        if self.side == Side::Server {
            None
        } else {
            self.snapshot.motion
        }
    }
    fn set_vertical_speed(&mut self, vy: f64) {
        if self.side != Side::Server {
            self.nudge.set_vertical(vy);
        }
    }
    fn push(&mut self, dx: f64, dz: f64) {
        if self.side != Side::Server {
            self.nudge.add_push(dx, dz);
        }
    }
}
impl hud::Host for State {
    fn set_panel(&mut self, corner: Corner, lines: Vec<Line>) {
        if self.side == Side::Server {
            return;
        }
        let lines = lines
            .into_iter()
            .take(HUD_LINES)
            .map(|l| Line {
                text: clip(l.text),
                color: l.color & 0xFF_FFFF,
            })
            .collect();
        self.panel = Some(Panel { corner, lines });
    }
    fn clear(&mut self) {
        self.panel = None;
    }
}
