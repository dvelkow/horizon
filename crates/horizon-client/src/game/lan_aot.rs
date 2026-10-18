//! `/aot` on a dedicated or LAN server: the Attack on Titan gear for the
//! player who types it (`horizon_client::odm` runs on their client, see
//! game/odm.rs). The server lets them fly and land unhurt (lan_actions.rs),
//! keeps who wears it beside the world in `horizon-aot.json`, and relays each
//! player's cables to the others near (`horizon:aot`).
use super::{Game, lan::LanHost, lan_tpa::tell};
use horizon_client::odm::{self, wire};
#[cfg(target_arch = "wasm32")]
use horizon_platform as std;
use horizon_protocol::packets::{configuration::CustomPayload, play::Clientbound as C};
use std::collections::BTreeSet;

/// The players wearing the gear (UUIDs in hex), in the world's folder.
const ROSTER: &str = "horizon-aot.json";
/// How far away others see a player's cables (blocks).
const RELAY: f64 = 160.0;

/// Who wears the gear, read from the world's folder on first use.
#[derive(Default)]
pub(super) struct Roster {
    loaded: bool,
    wearing: BTreeSet<String>,
}

impl Roster {
    fn load(&mut self, root: &std::path::Path) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        if let Ok(text) = std::fs::read_to_string(root.join(ROSTER)) {
            self.wearing = serde_json::from_str(&text).unwrap_or_default();
        }
    }
    /// Write then rename, so a crash never leaves half a file.
    fn save(&self, root: &std::path::Path) -> std::io::Result<()> {
        let text = serde_json::to_string_pretty(&self.wearing).map_err(std::io::Error::other)?;
        let partial = root.join(format!("{ROSTER}.partial"));
        std::fs::write(&partial, text)?;
        std::fs::rename(partial, root.join(ROSTER))
    }
}

/// The gear on or off, for its wearer.
fn mode(on: bool) -> C {
    C::CustomPayload(CustomPayload {
        channel: wire::CHANNEL.into(),
        data: vec![wire::MODE, u8::from(on)],
    })
}

/// A player's cables (`state` as they sent it) for the others.
fn cables(entity: i32, state: &[u8]) -> C {
    let mut data = vec![wire::STATE];
    data.extend_from_slice(&entity.to_be_bytes());
    data.extend_from_slice(state);
    C::CustomPayload(CustomPayload {
        channel: wire::CHANNEL.into(),
        data,
    })
}

impl Game<'_> {
    /// `/aot` for guest `conn`: the gear on or off; false when `words` is not it.
    pub(super) fn aot_command(&mut self, host: &mut LanHost, conn: u32, words: &[&str]) -> bool {
        let words: Vec<&str> = words.iter().copied().filter(|w| !w.is_empty()).collect();
        if words.first() != Some(&"aot") {
            return false;
        }
        let Some(g) = host.hub.guest_mut(conn) else {
            return true;
        };
        g.aot = !g.aot;
        let (on, key, entity, at) = (g.aot, format!("{:032x}", g.uuid), g.entity_id, g.position);
        host.aot.load(&self.world_root);
        if on {
            host.aot.wearing.insert(key);
        } else {
            host.aot.wearing.remove(&key);
        }
        if let Err(e) = host.aot.save(&self.world_root) {
            log::warn!("Horizon: could not save {ROSTER}: {e}");
        }
        host.hub.send_to(conn, &mode(on));
        if !on {
            // Their cables go for the others at once.
            host.hub
                .broadcast_near(at, RELAY, &cables(entity, &[0]), Some(conn));
        }
        let (text, color) = if on {
            (super::odm::HELP, "green")
        } else {
            (super::odm::OFF, "yellow")
        };
        tell(host, conn, text, color);
        true
    }

    /// A player who wore the gear when they left gets it back as they join.
    pub(super) fn aot_joined(&mut self, host: &mut LanHost, conn: u32) {
        host.aot.load(&self.world_root);
        let Some(g) = host.hub.guest_mut(conn) else {
            return;
        };
        if host.aot.wearing.contains(&format!("{:032x}", g.uuid)) {
            g.aot = true;
            host.hub.send_to(conn, &mode(true));
        }
    }

    /// A guest's cables (`horizon:aot`): checked, then relayed to the
    /// players near. Only a wearer's, well formed, with the hooks in reach.
    pub(super) fn guest_aot_state(&mut self, host: &mut LanHost, conn: u32, data: &[u8]) {
        let Some(g) = host.hub.guest(conn) else {
            return;
        };
        if !g.aot || data.first() != Some(&wire::STATE) || data.len() > 32 {
            return;
        }
        let Some(state) = wire::State::decode(&data[1..]) else {
            return;
        };
        let reach = odm::RANGE + 32.0;
        let near =
            state.tips.iter().flatten().all(|t| {
                (0..3).map(|k| (t[k] - g.position[k]).powi(2)).sum::<f64>() <= reach * reach
            });
        if near {
            host.hub.broadcast_near(
                g.position,
                RELAY,
                &cables(g.entity_id, &data[1..]),
                Some(conn),
            );
        }
    }
}
