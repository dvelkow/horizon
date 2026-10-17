//! `/tpa PLAYER`, `/tpaccept` (also `/tpaaccept`) and `/tpdeny` on a
//! dedicated or LAN server: one player asks to teleport to another, who
//! accepts or declines. Like `/list`, every player may use them: they need no
//! operator level, and nobody moves without the other player's yes.
use super::lan::LanHost;
use horizon_core::geometry::Vec3;
use horizon_protocol::packets::play::{self, Clientbound as C};
use horizon_world::nbt::{JavaString, Tag};
use std::collections::HashMap;

/// How long a request waits for an answer: a minute of server ticks.
const EXPIRES: u64 = 60 * 20;
/// Between two requests from one player: five seconds.
const COOLDOWN: u64 = 5 * 20;

/// The open requests, by the UUID of the player asked (the latest wins).
#[derive(Default)]
pub(super) struct Requests {
    pending: HashMap<u128, Request>,
    last_asked: HashMap<u128, u64>,
}
#[derive(Clone, Debug, PartialEq)]
struct Request {
    from: u128,
    from_name: String,
    at: u64,
}
#[derive(Debug, PartialEq)]
enum Refusal {
    Yourself,
    TooSoon(u64),
}

impl Requests {
    fn ask(&mut self, from: u128, from_name: &str, to: u128, now: u64) -> Result<(), Refusal> {
        if from == to {
            return Err(Refusal::Yourself);
        }
        if let Some(&last) = self.last_asked.get(&from)
            && now < last + COOLDOWN
        {
            return Err(Refusal::TooSoon((last + COOLDOWN - now).div_ceil(20)));
        }
        self.last_asked.insert(from, now);
        self.pending.insert(
            to,
            Request {
                from,
                from_name: from_name.to_owned(),
                at: now,
            },
        );
        Ok(())
    }
    /// The request waiting for `to`, if it has not expired; answering it
    /// closes it.
    fn answer(&mut self, to: u128, now: u64) -> Option<Request> {
        self.pending
            .remove(&to)
            .filter(|r| now < r.at + EXPIRES)
    }
}

/// A line in one player's chat, in a vanilla chat colour.
pub(super) fn tell(host: &LanHost, conn: u32, text: &str, color: &str) {
    let content = Tag::compound([
        ("text", Tag::String(JavaString::from(text))),
        ("color", Tag::String(JavaString::from(color))),
    ]);
    host.hub.send_to(
        conn,
        &C::SystemChat(play::SystemChat {
            content,
            overlay: false,
        }),
    );
}

/// Runs a teleport-request command for guest `conn`; false when `words`
/// is not one.
pub(super) fn command(host: &mut LanHost, conn: u32, words: &[&str]) -> bool {
    let words: Vec<&str> = words.iter().copied().filter(|w| !w.is_empty()).collect();
    let Some(&name) = words.first() else {
        return false;
    };
    let accept = matches!(name, "tpaccept" | "tpaaccept" | "tpyes");
    if !accept && !matches!(name, "tpa" | "tpdeny" | "tpno") {
        return false;
    }
    let Some(me) = host.hub.guests.iter().position(|g| g.conn == conn) else {
        return true;
    };
    let (my_uuid, my_name) = {
        let g = &host.hub.guests[me];
        (g.uuid, g.name.clone())
    };
    let now = host.hub.ticks;
    if name == "tpa" {
        let Some(target) = words.get(1).filter(|t| !t.is_empty() && words.len() == 2) else {
            tell(host, conn, "Usage: /tpa <player>", "red");
            return true;
        };
        let Some(to) = host
            .hub
            .guests
            .iter()
            .position(|g| g.name.eq_ignore_ascii_case(target))
        else {
            tell(host, conn, &format!("No player named {target} is online."), "red");
            return true;
        };
        let (to_uuid, to_name, to_conn) = {
            let g = &host.hub.guests[to];
            (g.uuid, g.name.clone(), g.conn)
        };
        match host.tpa.ask(my_uuid, &my_name, to_uuid, now) {
            Err(Refusal::Yourself) => tell(host, conn, "You can't teleport to yourself.", "red"),
            Err(Refusal::TooSoon(seconds)) => tell(
                host,
                conn,
                &format!("Wait {seconds} s before asking again."),
                "red",
            ),
            Ok(()) => {
                tell(
                    host,
                    conn,
                    &format!("Asked {to_name} to let you teleport to them. It expires in 60 s."),
                    "gray",
                );
                tell(
                    host,
                    to_conn,
                    &format!(
                        "{my_name} wants to teleport to you. Type /tpaccept to accept or /tpdeny to decline (60 s)."
                    ),
                    "gold",
                );
            }
        }
        return true;
    }
    let Some(request) = host.tpa.answer(my_uuid, now) else {
        tell(host, conn, "You have no teleport request to answer.", "red");
        return true;
    };
    let Some(from) = host
        .hub
        .guests
        .iter()
        .position(|g| g.uuid == request.from)
    else {
        tell(
            host,
            conn,
            &format!("{} is no longer online.", request.from_name),
            "red",
        );
        return true;
    };
    let from_conn = host.hub.guests[from].conn;
    if !accept {
        tell(host, conn, &format!("Declined {}'s request.", request.from_name), "gray");
        tell(host, from_conn, &format!("{my_name} declined your teleport request."), "red");
        return true;
    }
    if host.hub.guests[from].dead || host.hub.guests[me].dead {
        tell(host, conn, "Nobody can teleport while one of you is dead.", "red");
        return true;
    }
    let to = host.hub.guests[me].position;
    teleport(host, from, to);
    tell(host, conn, &format!("{} is on the way.", request.from_name), "green");
    tell(host, from_conn, &format!("Teleporting to {my_name}."), "green");
    true
}

/// Moves guest `i` to `to`, still and with no fall to land from.
pub(super) fn teleport(host: &mut LanHost, i: usize, to: [f64; 3]) {
    if let Some(m) = host.hub.guests[i].motion.as_mut() {
        m.position = Vec3::new(to[0], to[1], to[2]);
        m.velocity = Vec3::new(0.0, 0.0, 0.0);
        m.fall_distance = 0.0;
    }
    host.hub.teleport(i, Some(to));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_waits_a_minute_for_its_answer() {
        let mut r = Requests::default();
        assert_eq!(r.ask(1, "Ann", 2, 100), Ok(()));
        let request = r.answer(2, 100 + EXPIRES - 1).unwrap();
        assert_eq!((request.from, request.from_name.as_str()), (1, "Ann"));
        // Answered: gone.
        assert_eq!(r.answer(2, 200), None);
        // Expired.
        r.ask(1, "Ann", 2, 10_000).unwrap();
        assert_eq!(r.answer(2, 10_000 + EXPIRES), None);
    }

    #[test]
    fn nobody_asks_themselves_or_floods_requests() {
        let mut r = Requests::default();
        assert_eq!(r.ask(1, "Ann", 1, 0), Err(Refusal::Yourself));
        r.ask(1, "Ann", 2, 0).unwrap();
        assert_eq!(r.ask(1, "Ann", 3, 20), Err(Refusal::TooSoon(4)));
        assert_eq!(r.ask(1, "Ann", 3, COOLDOWN), Ok(()));
        // The latest request to a player replaces the one before.
        r.ask(4, "Bob", 3, COOLDOWN).unwrap();
        assert_eq!(r.answer(3, COOLDOWN + 1).unwrap().from_name, "Bob");
    }
}
