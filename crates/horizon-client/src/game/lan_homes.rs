//! `/sethome`, `/home`, `/spawn` and `/setspawn` on a dedicated or LAN
//! server. Every player keeps one home, saved beside the world in
//! `horizon-homes.json`, and may go back to it or to the world spawn without
//! an operator level; only the named player moves the spawn.
use super::{
    Game,
    lan::LanHost,
    lan_tpa::{teleport, tell},
};
use horizon_core::geometry::Vec3;
#[cfg(target_arch = "wasm32")]
use horizon_platform as std;
use std::collections::HashMap;

/// The homes file in the world's folder: player UUID (hex) to position.
const HOMES: &str = "horizon-homes.json";
/// The one player who may move the world spawn with `/setspawn`.
const SPAWN_SETTER: &str = "dobsa";

/// Every player's home, read from the world's folder on first use.
#[derive(Default)]
pub(super) struct Homes {
    loaded: bool,
    by_player: HashMap<String, [f64; 3]>,
}

impl Homes {
    fn load(&mut self, root: &std::path::Path) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        if let Ok(text) = std::fs::read_to_string(root.join(HOMES)) {
            self.by_player = serde_json::from_str(&text).unwrap_or_default();
        }
    }
    /// Write then rename, so a crash never leaves half a file.
    fn save(&self, root: &std::path::Path) -> std::io::Result<()> {
        let text = serde_json::to_string_pretty(&self.by_player).map_err(std::io::Error::other)?;
        let partial = root.join(format!("{HOMES}.partial"));
        std::fs::write(&partial, text)?;
        std::fs::rename(partial, root.join(HOMES))
    }
}

impl Game<'_> {
    /// Runs a home or spawn command for guest `conn`; false when `words` is
    /// not one.
    pub(super) fn home_command(&mut self, host: &mut LanHost, conn: u32, words: &[&str]) -> bool {
        let words: Vec<&str> = words.iter().copied().filter(|w| !w.is_empty()).collect();
        let Some(&name) = words.first() else {
            return false;
        };
        if !matches!(name, "sethome" | "home" | "spawn" | "setspawn") {
            return false;
        }
        let Some(i) = host.hub.guests.iter().position(|g| g.conn == conn) else {
            return true;
        };
        let (key, player, at, dead) = {
            let g = &host.hub.guests[i];
            (format!("{:032x}", g.uuid), g.name.clone(), g.position, g.dead)
        };
        if dead {
            tell(host, conn, "Respawn first.", "red");
            return true;
        }
        let shown = |p: [f64; 3]| format!("{}, {}, {}", p[0].floor(), p[1].floor(), p[2].floor());
        match name {
            "sethome" => {
                host.homes.load(&self.world_root);
                host.homes.by_player.insert(key, at);
                match host.homes.save(&self.world_root) {
                    Ok(()) => tell(host, conn, &format!("Home set at {}.", shown(at)), "green"),
                    Err(e) => {
                        log::warn!("Horizon: could not save homes: {e}");
                        tell(host, conn, "Your home could not be saved.", "red");
                    }
                }
            }
            "home" => {
                host.homes.load(&self.world_root);
                match host.homes.by_player.get(&key).copied() {
                    Some(home) => {
                        teleport(host, i, home);
                        tell(host, conn, "Welcome home.", "green");
                    }
                    None => tell(
                        host,
                        conn,
                        "You have no home yet: stand where you want it and type /sethome.",
                        "red",
                    ),
                }
            }
            "spawn" => {
                let spawn = self.spawn.map_or([0.5, 80.0, 0.5], |s| [s.x, s.y, s.z]);
                teleport(host, i, spawn);
                tell(host, conn, "Teleported to spawn.", "green");
            }
            _ => {
                if !player.eq_ignore_ascii_case(SPAWN_SETTER) {
                    tell(host, conn, "Only the host can move the spawn.", "red");
                    return true;
                }
                // SetWorldSpawnCommand: the level's spawn (saved with the world)
                // and the one respawning players use.
                let block = at.map(f64::floor);
                self.level.world_spawn = Some(block.map(|v| v as i32));
                self.level.spawn = Some(block);
                self.spawn = Some(Vec3::new(block[0] + 0.5, block[1], block[2] + 0.5));
                tell(host, conn, &format!("Spawn set at {}.", shown(at)), "green");
            }
        }
        true
    }
}
