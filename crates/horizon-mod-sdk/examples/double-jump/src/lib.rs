//! Example Horizon mod (`docs/mods.md`): press jump again in the air for a
//! second jump that moves differently from the first. It catches you for
//! two ticks (a stall that bleeds some speed), bursts upward with a speed
//! that climbs tick by tick and a dash along your heading, then floats as
//! it tops out before vanilla gravity takes over. `/doublejump` turns it
//! off or on.
//!
//! Once per airtime; standing on the ground again re-arms it. The curve
//! adds about 1.6 blocks, so used anywhere in a normal jump the player tops
//! out near 2.85 blocks: under vanilla's 3-block fall-damage line on flat
//! ground. Every block fallen before it still counts, so it never saves
//! anyone from a fall.
use horizon_mod_sdk::{
    Mod, Registration,
    movement::{self, Motion},
};

/// Blocks per tick to set on each tick of the second jump.
const CATCH: [f64; 2] = [0.03, 0.0];
const BURST: [f64; 4] = [0.18, 0.28, 0.33, 0.34];
const FLOAT: [f64; 5] = [0.22, 0.13, 0.06, 0.02, -0.01];
const STEPS: usize = CATCH.len() + BURST.len() + FLOAT.len();
/// The catch takes this share of the horizontal speed (the game caps one
/// tick's push at 0.1).
const BRAKE: f64 = 0.35;
/// The dash on the burst's first two ticks, along the heading.
const DASH: [f64; 2] = [0.07, 0.05];
/// Ticks of the game's lift allowance the curve uses (vertical speeds above
/// the player's own); with fewer left it would be cut short, so it waits.
const LIFT_NEEDED: u32 = 9;
/// Slower than this, there is no heading to dash along: straight up.
const STILL: f64 = 0.03;

fn vertical(step: usize) -> f64 {
    let mut curve = CATCH.iter().chain(&BURST).chain(&FLOAT);
    curve.nth(step).copied().unwrap_or(0.0)
}

/// What to ask the game for this tick.
#[derive(Debug, PartialEq)]
struct Nudge {
    vy: f64,
    push: [f64; 2],
}

struct DoubleJump {
    on: bool,
    was_jumping: bool,
    /// Used since the player last stood on the ground.
    used: bool,
    /// The second jump's tick, while it runs.
    step: Option<usize>,
    /// The unit heading it dashes along, from the speed when it started.
    heading: [f64; 2],
}

impl DoubleJump {
    fn new() -> Self {
        DoubleJump {
            on: true,
            was_jumping: false,
            used: false,
            step: None,
            heading: [0.0, 0.0],
        }
    }

    /// One tick: the player's motion in, the nudge (if any) out.
    fn advance(&mut self, m: &Motion) -> Option<Nudge> {
        let pressed = m.jumping && !self.was_jumping;
        self.was_jumping = m.jumping;
        if m.on_ground || !m.steerable {
            // Landed (which re-arms it), or in water, lava or flight.
            if m.on_ground {
                self.used = false;
            }
            self.step = None;
            return None;
        }
        if self.on && pressed && !self.used && m.lift_left >= LIFT_NEEDED {
            self.used = true;
            self.step = Some(0);
            let speed = m.vx.hypot(m.vz);
            self.heading = if speed > STILL {
                [m.vx / speed, m.vz / speed]
            } else {
                [0.0, 0.0]
            };
        }
        let step = self.step?;
        self.step = (step + 1 < STEPS).then_some(step + 1);
        let speed = m.vx.hypot(m.vz);
        let push = match step {
            // The catch: bleed some of the run-up.
            0 if speed > STILL => {
                let brake = (BRAKE * speed).min(0.1);
                [-m.vx / speed * brake, -m.vz / speed * brake]
            }
            // The burst's first ticks: dash along the heading.
            2 | 3 => {
                let dash = DASH[step - 2];
                [self.heading[0] * dash, self.heading[1] * dash]
            }
            _ => [0.0, 0.0],
        };
        Some(Nudge {
            vy: vertical(step),
            push,
        })
    }
}

impl Mod for DoubleJump {
    fn init() -> Result<(Self, Registration), String> {
        let hooks = Registration::new().command("doublejump").on_tick();
        Ok((DoubleJump::new(), hooks))
    }

    fn tick(&mut self, _tick: u64) {
        let Some(m) = movement::get_motion() else {
            self.step = None;
            return;
        };
        if let Some(n) = self.advance(&m) {
            movement::set_vertical_speed(n.vy);
            if n.push != [0.0, 0.0] {
                movement::push(n.push[0], n.push[1]);
            }
        }
    }

    fn command(&mut self, _name: &str, _args: &str) -> Result<Option<String>, String> {
        self.on = !self.on;
        self.step = None;
        let state = if self.on { "on" } else { "off" };
        Ok(Some(format!("Double jump {state}")))
    }
}

horizon_mod_sdk::export_mod!(DoubleJump);

#[cfg(test)]
mod tests {
    use super::*;

    fn air(vx: f64, vy: f64, jumping: bool) -> Motion {
        Motion {
            vx,
            vy,
            vz: 0.0,
            on_ground: false,
            jumping,
            steerable: true,
            lift_left: 16,
        }
    }
    fn ground(jumping: bool) -> Motion {
        Motion {
            on_ground: true,
            ..air(0.0, 0.0, jumping)
        }
    }

    #[test]
    fn holding_jump_from_the_ground_does_nothing() {
        let mut dj = DoubleJump::new();
        assert_eq!(dj.advance(&ground(true)), None);
        for _ in 0..10 {
            assert_eq!(dj.advance(&air(0.3, 0.2, true)), None);
        }
    }

    #[test]
    fn a_fresh_press_in_the_air_plays_the_whole_curve_once() {
        let mut dj = DoubleJump::new();
        dj.advance(&ground(true));
        dj.advance(&air(0.4, 0.3, false));
        let mut rise = 0.0;
        let mut ticks = 0;
        let mut pressed = true;
        while let Some(n) = dj.advance(&air(0.4, 0.0, pressed)) {
            rise += n.vy;
            ticks += 1;
            pressed = false;
        }
        assert_eq!(ticks, STEPS);
        // 1.25 for the first jump plus this stays under the 3 blocks
        // vanilla falls without damage.
        assert!(rise > 1.5 && rise + 1.2522 < 2.9, "rise {rise}");
        // Released and pressed again in the same airtime: nothing.
        dj.advance(&air(0.4, -0.3, false));
        assert_eq!(dj.advance(&air(0.4, -0.3, true)), None);
        // Landing re-arms it.
        dj.advance(&ground(false));
        dj.advance(&air(0.4, 0.3, false));
        assert!(dj.advance(&air(0.4, 0.2, true)).is_some());
    }

    #[test]
    fn it_catches_then_bursts_then_floats() {
        let speeds: Vec<f64> = (0..STEPS).map(vertical).collect();
        // The catch holds still...
        assert!(speeds[..2].iter().all(|v| *v < 0.05));
        // ...the burst speeds up every tick...
        assert!(speeds[2..6].windows(2).all(|w| w[1] > w[0]));
        // ...and the float slows down every tick, to a gentle drop.
        assert!(speeds[6..].windows(2).all(|w| w[1] < w[0]));
        assert!(*speeds.last().unwrap() < 0.0);
    }

    #[test]
    fn the_catch_brakes_and_the_burst_dashes_along_the_heading() {
        let mut dj = DoubleJump::new();
        dj.advance(&air(0.0, 0.1, false));
        let catch = dj.advance(&air(0.0, 0.1, true).with_vz(0.4)).unwrap();
        assert!(catch.push[1] < 0.0 && catch.push[1] >= -0.1);
        dj.advance(&air(0.0, 0.0, true).with_vz(0.3));
        let burst = dj.advance(&air(0.0, 0.0, true).with_vz(0.3)).unwrap();
        assert_eq!(burst.push, [0.0, DASH[0]]);
        // Standing still: straight up.
        let mut dj = DoubleJump::new();
        dj.advance(&air(0.0, 0.1, false));
        for _ in 0..4 {
            let n = dj.advance(&air(0.0, 0.0, true)).unwrap();
            assert_eq!(n.push, [0.0, 0.0]);
        }
    }

    #[test]
    fn water_landing_or_the_command_cuts_it_short() {
        let mut dj = DoubleJump::new();
        dj.advance(&air(0.0, 0.1, false));
        assert!(dj.advance(&air(0.0, 0.0, true)).is_some());
        let wet = Motion {
            steerable: false,
            ..air(0.0, 0.0, false)
        };
        assert_eq!(dj.advance(&wet), None);
        assert_eq!(dj.advance(&air(0.0, 0.0, false)), None);
        // Low on the game's lift allowance: it waits for the ground.
        let mut dj = DoubleJump::new();
        dj.advance(&air(0.0, 0.1, false));
        let spent = Motion {
            lift_left: 4,
            ..air(0.0, 0.0, true)
        };
        assert_eq!(dj.advance(&spent), None);
    }

    trait WithVz {
        fn with_vz(self, vz: f64) -> Self;
    }
    impl WithVz for Motion {
        fn with_vz(mut self, vz: f64) -> Self {
            self.vz = vz;
            self
        }
    }
}
