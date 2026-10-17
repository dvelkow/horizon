//! Horizon mods in the game (`docs/mods.md`): loading the ones turned on,
//! their tick, frame, chat and command hooks, their HUD panels, what they
//! print and their nudges to the player's movement. With no mod on, `horizon_mods::Mods` holds no engine and
//! every hook here returns after one check.
use super::{Game, Screen};
use horizon_client::{
    command::{Dispatcher, vanilla},
    extras::Corner,
    extras_hud::{self, Corners},
    hud::HudBatch,
    mod_list::{self, Installed},
    motion::PlayerMotion,
    text::{self, Text},
};
use horizon_core::geometry::Vec3;
use horizon_mods::{Clock, Mods, Motion, Nudge, Output, Player, Snapshot, Status, nudge};
#[cfg(target_arch = "wasm32")]
use horizon_platform as std;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Default)]
pub(super) struct ModsState {
    pub(super) mods: Mods,
    /// `--dev-mod PATH`: always on, reloaded when rebuilt.
    pub(super) dev: Vec<PathBuf>,
    /// When dev mods were last checked for a rebuild.
    checked: Option<Instant>,
    /// The chat's command tree while a running mod owns commands.
    tree: Option<Dispatcher>,
    #[cfg(not(target_arch = "wasm32"))]
    server: bool,
    #[cfg(not(target_arch = "wasm32"))]
    approved_dir: Option<PathBuf>,
    #[cfg(not(target_arch = "wasm32"))]
    approval_error: Option<String>,
    /// The approval mirror's content mods since they first differed from the
    /// ones the world started with, and whether players were told.
    #[cfg(not(target_arch = "wasm32"))]
    content_change: Option<(Vec<(String, [u8; 32])>, Instant, bool)>,
    /// Save and stop now, to be started again with them.
    #[cfg(not(target_arch = "wasm32"))]
    restart: bool,
    /// Ticks of lift mods' nudges used since the player last stood on the
    /// ground (`nudge::LIFT_TICKS` per airtime).
    pub(super) lift_used: u32,
}
impl ModsState {
    #[cfg(not(target_arch = "wasm32"))]
    fn restart_due(&self) -> bool {
        self.restart
    }
    /// A browser hosts no world.
    #[cfg(target_arch = "wasm32")]
    fn restart_due(&self) -> bool {
        false
    }
    /// The chat's command tree: vanilla's, plus the running mods' commands.
    pub(super) fn dispatcher(&self) -> &Dispatcher {
        self.tree
            .as_ref()
            .unwrap_or_else(|| vanilla::game_dispatcher())
    }
}
/// How often dev mods are checked for a rebuild.
const RELOAD_CHECK: Duration = Duration::from_secs(1);
/// A changed set of approved content mods must hold this long before players
/// are told (a publication settles), then the world restarts after the notice.
#[cfg(not(target_arch = "wasm32"))]
const CONTENT_SETTLE: Duration = Duration::from_secs(2);
#[cfg(not(target_arch = "wasm32"))]
const CONTENT_NOTICE: Duration = Duration::from_secs(5);

/// Mods' `movement` nudges, for the player's next movement step.
pub(super) struct PendingNudge {
    nudge: Nudge,
    may_lift: bool,
}
impl PendingNudge {
    /// True when it used a tick of lift.
    pub(super) fn apply(self, player: &mut PlayerMotion) -> bool {
        let v = player.velocity;
        let ([x, y, z], lifted) = self.nudge.apply([v.x, v.y, v.z], self.may_lift);
        player.velocity = Vec3::new(x, y, z);
        lifted
    }
}

/// A vanilla command name: mods may not take it.
fn vanilla_command(name: &str) -> bool {
    let d = vanilla::game_dispatcher();
    d.nodes[0]
        .children
        .iter()
        .any(|&c| d.nodes[c].name() == name)
}

#[cfg(not(target_arch = "wasm32"))]
fn server_command(name: &str) -> bool {
    vanilla_command(name)
        || matches!(
            name,
            "stop"
                | "save-all"
                | "save-off"
                | "save-on"
                | "list"
                | "op"
                | "deop"
                | "ban"
                | "ban-ip"
                | "banlist"
                | "pardon"
                | "pardon-ip"
                | "kick"
                | "whitelist"
                | "say"
        )
}

/// `root` with content mods layered over it (docs/modpacks.md): their
/// blocks, items, data and assets, mods in id order, so the same set always
/// gives the same block-state ids. Read once at start, before the resources
/// load; a mod whose content is refused is left out, with the reason logged.
///
/// A hosted world's game (`approved`: a dedicated server's approval mirror,
/// or a browser's verified copies of it) layers exactly the world's approved
/// content mods and no other: the server and every player must have the same
/// blocks. Otherwise the content mods turned on in `horizon-mods/` and the
/// dev mods. Also returns the approved ones read, with their packages'
/// SHA-256, for the dedicated server's restart check (natively).
pub(super) fn content_root(
    root: &std::path::Path,
    enabled: &std::collections::BTreeSet<String>,
    dev: &[std::path::PathBuf],
    approved: Option<&std::path::Path>,
) -> (std::path::PathBuf, Vec<(String, [u8; 32])>) {
    use horizon_client::mod_content::{self, ContentMod};
    use horizon_mods::{Capability, Content, Package};
    let here = std::env::current_dir().unwrap_or_default();
    let cache = here.join(".horizon-cache");
    let (paths, loaded) = match approved {
        Some(dir) => approved_packages(dir, &cache).unwrap_or_else(|e| {
            log::warn!("Horizon: approved content mods not read: {e}");
            Default::default()
        }),
        None => {
            let mut paths = horizon_mods::find(&here.join(mod_list::DIR));
            paths.retain(|p| !dev.contains(p));
            paths.extend(dev.iter().cloned());
            (paths, Vec::new())
        }
    };
    let mut mods: Vec<ContentMod> = Vec::new();
    for path in &paths {
        let Ok(package) = Package::read(path) else {
            continue;
        };
        let m = &package.manifest;
        let on = approved.is_some() || enabled.contains(&m.id) || dev.contains(path);
        if !on
            || !m.capabilities.contains(&Capability::Registries)
            || mods.iter().any(|c| c.id == m.id)
        {
            continue;
        }
        match Content::read(path, m) {
            Ok(content) => mods.push(ContentMod {
                id: m.id.clone(),
                content,
            }),
            Err(e) => log::warn!("Horizon: mod {} content not loaded: {e}", m.id),
        }
    }
    mods.sort_by(|a, b| a.id.cmp(&b.id));
    let built = mod_content::resource_root(root, &mods, &cache).or_else(|e| {
        // One mod that does not fit (a template the game lacks, too many
        // states) must not take the others' blocks with it: leave out each
        // that fails on its own, the same way on the server and in browsers.
        log::warn!("Horizon: content mods do not load together: {e}");
        mods.retain(
            |m| match mod_content::check(root, std::slice::from_ref(m)) {
                Ok(()) => true,
                Err(e) => {
                    log::warn!("Horizon: mod {} content left out: {e}", m.id);
                    false
                }
            },
        );
        mod_content::resource_root(root, &mods, &cache)
    });
    let root = match built {
        Ok(out) => {
            if !mods.is_empty() {
                let ids: Vec<&str> = mods.iter().map(|m| m.id.as_str()).collect();
                log::info!(
                    "Horizon: content from {} mod(s) in {}: {}",
                    mods.len(),
                    out.display(),
                    ids.join(", ")
                );
            }
            out
        }
        Err(e) => {
            log::warn!("Horizon: content mods not loaded: {e}");
            root.to_owned()
        }
    };
    (root, loaded)
}

/// The approval mirror's content mods, copied into `cache` first: the
/// portal may replace a package while the resources are built from it, and
/// a copy stays one version. The digests are of the bytes read, as
/// `Mods::approved_content` takes them from the mirror; they name what the
/// mirror held even when the content does not load, so a world cannot
/// restart over and over for the same set. A mirror not made yet is empty.
#[cfg(not(target_arch = "wasm32"))]
fn approved_packages(
    dir: &std::path::Path,
    cache: &std::path::Path,
) -> std::result::Result<(Vec<PathBuf>, Vec<(String, [u8; 32])>), String> {
    if !dir.exists() {
        return Ok(Default::default());
    }
    let copies = cache.join("approved-content");
    let listed = copy_packages(dir, &copies).and_then(|()| digests(&copies));
    // Copies are a safeguard; without room for them, read the mirror itself.
    listed.or_else(|e| {
        log::warn!("Horizon: approved mods not copied ({e}); reading the mirror itself");
        digests(dir)
    })
}

/// The content mods among the packages in `dir`, with their digests.
#[cfg(not(target_arch = "wasm32"))]
fn digests(
    dir: &std::path::Path,
) -> std::result::Result<(Vec<PathBuf>, Vec<(String, [u8; 32])>), String> {
    let mut paths = Vec::new();
    let mut loaded = Vec::new();
    for (id, path) in horizon_mods::content::approved(dir)? {
        let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
        loaded.push((id, horizon_mods::package_digest(&bytes)));
        paths.push(path);
    }
    Ok((paths, loaded))
}

/// The mirror's packages, copied into `copies`.
#[cfg(not(target_arch = "wasm32"))]
fn copy_packages(
    dir: &std::path::Path,
    copies: &std::path::Path,
) -> std::result::Result<(), String> {
    if copies.exists() {
        std::fs::remove_dir_all(copies).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(copies).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        let package = std::path::Path::new(&name)
            .extension()
            .is_some_and(|e| e == "hmod");
        // Regular files of the size the mirror's scan reads.
        let file = entry
            .metadata()
            .is_ok_and(|m| m.is_file() && m.len() <= 64 << 20);
        if package && file && entry.file_type().is_ok_and(|t| t.is_file()) {
            // One read, so the copy is one version of the file.
            match std::fs::read(entry.path()) {
                Ok(bytes) => {
                    std::fs::write(copies.join(&name), bytes).map_err(|e| e.to_string())?
                }
                Err(e) => log::warn!("Horizon: approved mod {}: {e}", name.to_string_lossy()),
            }
        }
    }
    Ok(())
}

/// The browser's copies are verified and immutable (crate::browser).
#[cfg(target_arch = "wasm32")]
fn approved_packages(
    dir: &std::path::Path,
    _cache: &std::path::Path,
) -> std::result::Result<(Vec<PathBuf>, Vec<(String, [u8; 32])>), String> {
    let found = horizon_mods::content::approved(dir)?;
    Ok((
        found.into_iter().map(|(_, path)| path).collect(),
        Vec::new(),
    ))
}

impl Game<'_> {
    /// Reads `horizon-mods/` and the dev mods, and starts the ones turned on.
    pub(super) fn start_mods(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        if self.options.dedicated.is_some() {
            self.start_server_mods();
            return;
        }
        let dir = Self::options_path().with_file_name(mod_list::DIR);
        let dev = &self.mods.dev;
        let mut paths = horizon_mods::find(&dir);
        paths.retain(|p| !dev.contains(p));
        paths.extend(dev.iter().cloned());
        // A dev mod is on whatever the list says.
        let dev_ids: Vec<String> = dev
            .iter()
            .filter_map(|p| horizon_mods::Package::read(p).ok())
            .map(|p| p.manifest.id)
            .collect();
        let enabled = &self.settings.mods.enabled;
        let on = |id: &str| enabled.contains(id) || dev_ids.iter().any(|d| d == id);
        let mods = Mods::load(&paths, on, vanilla_command);
        self.mods.mods = mods;
        self.list_mods();
        self.sync_mod_commands();
        self.show_mod_output();
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn start_server_mods(&mut self) {
        self.mods.server = true;
        self.mods.approved_dir = self.options.approved_mods.clone();
        self.mods.checked = None;
        if self.mods.approved_dir.is_some() {
            self.mods.mods = Mods::load_server(&[], |_| true, server_command);
            self.poll_server_mods();
        } else {
            let dir = Self::options_path().with_file_name(mod_list::DIR);
            let mut paths = horizon_mods::find(&dir);
            paths.retain(|path| !self.mods.dev.contains(path));
            paths.extend(self.mods.dev.iter().cloned());
            let dev_ids: Vec<_> = self
                .mods
                .dev
                .iter()
                .filter_map(|path| horizon_mods::Package::read(path).ok())
                .map(|package| package.manifest.id)
                .collect();
            let enabled = &self.settings.mods.enabled;
            self.mods.mods = Mods::load_server(
                &paths,
                |id| enabled.contains(id) || dev_ids.iter().any(|dev| dev == id),
                server_command,
            );
            self.sync_mod_commands();
            self.show_mod_output();
        }
    }

    /// Poll even while an empty dedicated world is paused: a newly approved
    /// mod must be available before the next player joins.
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn poll_server_mods(&mut self) {
        if !self.mods.server
            || self
                .mods
                .checked
                .is_some_and(|at| at.elapsed() < RELOAD_CHECK)
        {
            return;
        }
        self.mods.checked = Some(Instant::now());
        let changed = if let Some(dir) = &self.mods.approved_dir {
            match self.mods.mods.sync_server_directory(dir, server_command) {
                Ok(changed) => {
                    self.mods.approval_error = None;
                    self.watch_content();
                    changed
                }
                Err(error) => {
                    if self.mods.approval_error.as_ref() != Some(&error) {
                        super::dedicated::log("WARN", &format!("Approved mod mirror: {error}"));
                    }
                    self.mods.approval_error = Some(error);
                    false
                }
            }
        } else if !self.mods.dev.is_empty() {
            self.mods.mods.reload_changed(server_command)
        } else {
            false
        };
        if changed {
            self.sync_mod_commands();
            super::dedicated::log("INFO", "Approved server mods updated");
        }
        self.show_mod_output();
    }

    /// The approval mirror's content mods differ from the ones the world
    /// started with: blocks and items cannot join a running world. Once the
    /// change holds for `CONTENT_SETTLE` players are told, and after
    /// `CONTENT_NOTICE` the world saves and the server stops for horizon-server
    /// to start it again with them. Open pages reload into them meanwhile:
    /// the portal's release changes with its content (web/server).
    #[cfg(not(target_arch = "wasm32"))]
    fn watch_content(&mut self) {
        // A package that does not read is no change: wait for it.
        let Some(now) = self.mods.mods.approved_content() else {
            return;
        };
        let tell = |game: &Self, message: &str| {
            super::dedicated::log("INFO", message);
            if let Some(host) = &game.lan {
                host.hub
                    .system_message(horizon_protocol::text::literal(message));
            }
        };
        if now == self.options.approved_content {
            let told = self.mods.content_change.take().is_some_and(|c| c.2);
            if told {
                tell(
                    self,
                    "The world's blocks and items are back as they were: no restart",
                );
            }
            return;
        }
        if !matches!(&self.mods.content_change, Some((set, _, _)) if *set == now) {
            self.mods.content_change = Some((now, Instant::now(), false));
            return;
        }
        let Some((_, since, told)) = self.mods.content_change.as_mut() else {
            return;
        };
        let elapsed = since.elapsed();
        let first = !*told && elapsed >= CONTENT_SETTLE;
        let due = *told && elapsed >= CONTENT_SETTLE + CONTENT_NOTICE;
        *told |= first;
        if first {
            tell(
                self,
                &format!(
                    "The world's blocks and items changed: it restarts in {} seconds to load them, and you rejoin right after",
                    CONTENT_NOTICE.as_secs()
                ),
            );
        } else if due {
            let ids: Vec<&str> = now.iter().map(|(id, _)| id.as_str()).collect();
            let ids = if ids.is_empty() {
                "none".to_owned()
            } else {
                ids.join(", ")
            };
            super::dedicated::log(
                "INFO",
                &format!("Restarting to load the approved content mods: {ids}"),
            );
            self.mods.restart = true;
        }
    }

    /// Whether the dedicated server should save and stop now, to be started
    /// again with its changed approved content mods.
    pub(super) fn mods_restart_due(&self) -> bool {
        self.mods.restart_due()
    }

    /// The Mods page's rows, from what loaded.
    fn list_mods(&mut self) {
        let dev = &self.mods.dev;
        self.settings.mods.installed = self
            .mods
            .mods
            .entries()
            .iter()
            .map(|e| {
                let file = e.path.file_name().unwrap_or_default().to_string_lossy();
                let problem = match &e.status {
                    Status::Failed(reason) => Some(format!("Stopped: {reason}")),
                    _ => None,
                };
                let Some(m) = &e.manifest else {
                    return Installed {
                        id: String::new(),
                        title: file.into_owned(),
                        description: String::new(),
                        problem,
                        dev: false,
                    };
                };
                // What it may do first: the row cuts long descriptions.
                let may: Vec<&str> = m.capabilities.iter().map(|c| c.describe()).collect();
                let may = if may.is_empty() {
                    "Asks for nothing.".to_owned()
                } else {
                    format!("May {}.", may.join(", "))
                };
                Installed {
                    id: m.id.clone(),
                    title: format!("{} {}", m.name, m.version),
                    description: format!("{may} {}", m.description).trim().to_owned(),
                    problem,
                    dev: dev.contains(&e.path),
                }
            })
            .collect();
    }
    /// The chat's tree after the running mods changed.
    fn sync_mod_commands(&mut self) {
        let mut names = self.mods.mods.commands().peekable();
        self.mods.tree = names
            .peek()
            .is_some()
            .then(|| vanilla::game_dispatcher().with_mod_commands(names));
    }
    /// After the options changed: restart the mods if the list of the ones
    /// turned on did.
    pub(super) fn sync_mods(&mut self) {
        let running: Vec<&str> = self
            .mods
            .mods
            .entries()
            .iter()
            .filter(|e| e.status != Status::Off)
            .filter_map(|e| Some(e.manifest.as_ref()?.id.as_str()))
            .collect();
        let wanted: Vec<&str> = self
            .settings
            .mods
            .installed
            .iter()
            .enumerate()
            .filter(|&(i, _)| self.settings.mods.is_on(i))
            .map(|(_, m)| m.id.as_str())
            .collect();
        if running != wanted {
            self.start_mods();
        }
    }
    /// What mods read this tick.
    fn mod_snapshot(&self) -> Snapshot {
        #[cfg(not(target_arch = "wasm32"))]
        if self.mods.server {
            return Snapshot {
                player: None,
                clock: Some(Clock {
                    game_time: self.world.game_time.max(0) as u64,
                    day_time: self.world.day_time.max(0) as u64,
                }),
                motion: None,
            };
        }
        let player = self.player.as_ref().map(|p| Player {
            x: p.position.x,
            y: p.position.y,
            z: p.position.z,
            yaw: self.yaw,
            pitch: self.pitch,
            health: p.health.health(),
            food: p.health.food.food.max(0) as u32,
            dimension: self.dim.id().to_owned(),
        });
        let clock = player.is_some().then(|| Clock {
            game_time: self.world.game_time.max(0) as u64,
            day_time: self.world.day_time.max(0) as u64,
        });
        let steerable = self.mod_steerable();
        let motion = self.player.as_ref().map(|p| Motion {
            vx: p.velocity.x,
            vy: p.velocity.y,
            vz: p.velocity.z,
            on_ground: p.ground.on_ground,
            jumping: self.screen == Screen::Playing
                && !self.remote.as_ref().is_some_and(|r| r.chat_input.is_some())
                && self.keys.contains(&winit::keyboard::KeyCode::Space),
            steerable,
            lift_left: if p.ground.on_ground || !steerable {
                nudge::LIFT_TICKS
            } else {
                nudge::LIFT_TICKS.saturating_sub(self.mods.lift_used)
            },
        });
        Snapshot {
            player,
            clock,
            motion,
        }
    }
    /// Whether nudges reach the player: not while the game's own movement
    /// rules own them (flight, gliding, fluids, riding, sleep, death).
    fn mod_steerable(&self) -> bool {
        self.player.as_ref().is_some_and(|p| {
            self.riding.is_none()
                && self.sleeping.is_none()
                && !p.spectator
                && !p.flying
                && !p.fall_flying
                && !p.swimming
                && !p.fluids.was_touching_water
                && !p.is_in_lava()
                && !p.health.dead_or_dying()
        })
    }
    /// Before the player's movement step: what mods' nudges asked since the
    /// last one, or None when there is nothing to apply. Standing on the
    /// ground, or leaving the mods' reach, refills the lift allowance.
    pub(super) fn take_mod_nudge(&mut self) -> Option<PendingNudge> {
        let nudge = self.mods.mods.take_nudge();
        let grounded = self.player.as_ref().is_some_and(|p| p.ground.on_ground);
        let steerable = self.mod_steerable();
        if grounded || !steerable {
            self.mods.lift_used = 0;
        }
        (steerable && !nudge.is_empty()).then(|| PendingNudge {
            nudge,
            may_lift: self.mods.lift_used < nudge::LIFT_TICKS,
        })
    }
    /// A game tick.
    pub(super) fn tick_mods(&mut self) {
        if !self.mods.mods.is_running() {
            return;
        }
        let snapshot = self.mod_snapshot();
        self.mods.mods.set_snapshot(snapshot);
        self.mods.mods.tick(self.ticks);
        self.show_mod_output();
    }
    /// Once per frame: the frame hook, and dev mods' rebuilds.
    pub(super) fn mods_frame(&mut self, elapsed: Duration) {
        if (cfg!(target_arch = "wasm32") || !self.mods.dev.is_empty())
            && self
                .mods
                .checked
                .is_none_or(|t| t.elapsed() >= RELOAD_CHECK)
        {
            self.mods.checked = Some(Instant::now());
            if self.mods.mods.reload_changed(vanilla_command) {
                self.list_mods();
                self.sync_mod_commands();
                self.system_message(Text::literal("Mods reloaded").with_color(text::GRAY));
            }
        }
        if !self.mods.mods.is_running() {
            return;
        }
        self.mods.mods.frame(elapsed.as_secs_f32());
        self.show_mod_output();
    }
    /// `/command` typed in chat: true when a mod owns it.
    pub(super) fn mod_command(&mut self, command: &str) -> bool {
        if !self.mods.mods.is_running() {
            return false;
        }
        // The command may follow a teleport in the same tick.
        let snapshot = self.mod_snapshot();
        self.mods.mods.set_snapshot(snapshot);
        if !self.mods.mods.command(command) {
            return false;
        }
        self.show_mod_output();
        true
    }
    /// A chat line the player received.
    pub(super) fn mods_chat(&mut self, line: &str) {
        let snapshot = self.mod_snapshot();
        self.mods.mods.set_snapshot(snapshot);
        self.mods.mods.chat(line);
        self.show_mod_output();
    }
    /// Chat lines, command feedback and stopped mods, into the chat.
    fn show_mod_output(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        if self.mods.server {
            for output in self.mods.mods.drain() {
                let (message, error) = match output {
                    Output::Chat { name, text } => (format!("[{name}] {text}"), false),
                    Output::Feedback { text, error } => (text, error),
                    Output::Stopped { name, reason } => {
                        self.sync_mod_commands();
                        (format!("Mod {name} stopped: {reason}"), true)
                    }
                };
                super::dedicated::log(if error { "WARN" } else { "INFO" }, &message);
                if let Some(host) = &self.lan {
                    host.hub
                        .system_message(horizon_protocol::text::literal(&message));
                }
            }
            return;
        }
        for output in self.mods.mods.drain() {
            let line = match output {
                Output::Chat { name, text } => {
                    let mut line = Text::literal(format!("[{name}] ")).with_color(text::GRAY);
                    line.push(Text::literal(text));
                    line
                }
                Output::Feedback { text, error: false } => Text::literal(text),
                Output::Feedback { text, error: true } => Text::literal(text).with_color(text::RED),
                Output::Stopped { name, reason } => {
                    self.list_mods();
                    self.sync_mod_commands();
                    Text::literal(format!("Mod {name} stopped: {reason}")).with_color(text::RED)
                }
            };
            // Not through system_message: a mod must not hear its own lines.
            // On a server the chat on screen is the connection's.
            if self.remote.is_some() {
                self.chat_line(line.plain());
                continue;
            }
            let focused = self.screen == Screen::Chat;
            self.ui
                .chat
                .add(&self.atlas, line, None, self.ticks, focused);
        }
    }
    /// The mods' HUD panels, stacked with the extras in their corners.
    pub(super) fn draw_mod_panels(&self, batch: &mut HudBatch, corners: &mut Corners) {
        for panel in self.mods.mods.panels() {
            let corner = match panel.corner {
                horizon_mods::Corner::TopLeft => Corner::TopLeft,
                horizon_mods::Corner::TopRight => Corner::TopRight,
                horizon_mods::Corner::BottomLeft => Corner::BottomLeft,
                horizon_mods::Corner::BottomRight => Corner::BottomRight,
            };
            // F3 takes the top left, as it does from the extras.
            if self.debug && corner == Corner::TopLeft {
                continue;
            }
            let lines: Vec<(&str, u32)> = panel
                .lines
                .iter()
                .map(|l| (l.text.as_str(), l.color))
                .collect();
            extras_hud::draw_lines(batch, &self.atlas, corners, corner, &lines);
        }
    }
}
