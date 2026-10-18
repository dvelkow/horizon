//! `/aot` on a dedicated or LAN server: the Attack on Titan gear. The command
//! puts a pair of ODM blades (the `aot` content mod's item) in the player's
//! empty hand; while they hold them, their client runs the gear
//! (`horizon_client::odm`, game/odm.rs), the server spares them fall damage
//! and the flying kick (lan_actions.rs), and relays their cables to the
//! others near (`horizon:aot`).
use super::{Game, lan::LanHost, lan_tpa::tell};
use horizon_client::{
    lan::guest::Guest,
    odm::{self, wire},
};
use horizon_protocol::packets::{configuration::CustomPayload, play::Clientbound as C};

/// How far away others see a player's cables (blocks).
const RELAY: f64 = 160.0;

/// Whether the guest holds the blades: the gear is theirs while they do.
pub(super) fn holds_blades(g: &Guest) -> bool {
    odm::is_blades(g.inventory.get(g.selected).and_then(Option::as_ref))
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
    /// `/aot` for guest `conn`: the blades into their empty hand; false when
    /// `words` is not it.
    pub(super) fn aot_command(&mut self, host: &mut LanHost, conn: u32, words: &[&str]) -> bool {
        let words: Vec<&str> = words.iter().copied().filter(|w| !w.is_empty()).collect();
        if words.first() != Some(&"aot") {
            return false;
        }
        if host.hub.guest(conn).is_none_or(|g| g.dead) {
            return true;
        }
        let answer = self.as_guest(host, conn, |g| g.odm_give_blades());
        if let Some((text, held)) = answer {
            tell(host, conn, text, if held { "green" } else { "red" });
        }
        true
    }

    /// A guest's cables (`horizon:aot`): checked, then relayed to the
    /// players near. Only a blade holder's, well formed, with the hooks in
    /// reach.
    pub(super) fn guest_aot_state(&mut self, host: &mut LanHost, conn: u32, data: &[u8]) {
        let Some(g) = host.hub.guest(conn) else {
            return;
        };
        if data.first() != Some(&wire::STATE) || data.len() > 32 {
            return;
        }
        let Some(state) = wire::State::decode(&data[1..]) else {
            return;
        };
        // Putting the blades away clears the cables for everyone.
        if state.flags != 0 && !holds_blades(g) {
            return;
        }
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
