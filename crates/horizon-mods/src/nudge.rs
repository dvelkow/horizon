//! What the `movement` capability may do to the player (horizon.wit). Mods
//! ask; the game bounds what they asked with [`Nudge`] before the player's
//! next movement step, so a mod can shape a jump but cannot fly or hover.

/// The fastest upward speed a nudge may set, in blocks per tick. A jump
/// starts at 0.42.
pub const MAX_RISE: f64 = 0.6;
/// The fastest downward speed: vanilla's terminal falling speed.
pub const MAX_DROP: f64 = 3.92;
/// The most one tick's pushes may add to the horizontal velocity.
pub const MAX_PUSH: f64 = 0.1;
/// Pushes never take the player past this horizontal speed, or past their
/// current speed when that is higher (a sprint jump peaks near 0.36).
pub const MAX_SPEED: f64 = 0.6;
/// Ticks of lift (a vertical speed above the player's own) per airtime:
/// standing on the ground again refills them.
pub const LIFT_TICKS: u32 = 16;

/// The nudges mods asked for since the game last applied them.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Nudge {
    /// `set-vertical-speed`: the latest call wins.
    pub vertical: Option<f64>,
    /// `push`: the calls add up.
    pub push: [f64; 2],
}

impl Nudge {
    pub fn is_empty(&self) -> bool {
        self.vertical.is_none() && self.push == [0.0, 0.0]
    }
    pub fn set_vertical(&mut self, vy: f64) {
        if vy.is_finite() {
            self.vertical = Some(vy.clamp(-MAX_DROP, MAX_RISE));
        }
    }
    pub fn add_push(&mut self, dx: f64, dz: f64) {
        if dx.is_finite() && dz.is_finite() {
            self.push = clamp_length([self.push[0] + dx, self.push[1] + dz], MAX_PUSH);
        }
    }
    /// `later` after this one: its vertical speed wins, the pushes add.
    pub fn merge(&mut self, later: Nudge) {
        if let Some(vy) = later.vertical {
            self.set_vertical(vy);
        }
        self.add_push(later.push[0], later.push[1]);
    }
    /// `velocity` (x, y, z in blocks per tick) after the nudge, and whether
    /// it used a tick of lift. A vertical speed below the player's own
    /// always applies; one above it only when `may_lift`.
    pub fn apply(&self, velocity: [f64; 3], may_lift: bool) -> ([f64; 3], bool) {
        let [x, y, z] = velocity;
        let mut out = velocity;
        let mut lifted = false;
        if let Some(vy) = self.vertical {
            if vy <= y {
                out[1] = vy;
            } else if may_lift {
                out[1] = vy;
                lifted = true;
            }
        }
        if self.push != [0.0, 0.0] {
            let [px, pz] = clamp_length(self.push, MAX_PUSH);
            let cap = x.hypot(z).max(MAX_SPEED);
            [out[0], out[2]] = clamp_length([x + px, z + pz], cap);
        }
        (out, lifted)
    }
}

fn clamp_length([x, z]: [f64; 2], max: f64) -> [f64; 2] {
    let length = x.hypot(z);
    if length > max {
        [x * max / length, z * max / length]
    } else {
        [x, z]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vertical_speed_is_bounded_and_latest_wins() {
        let mut n = Nudge::default();
        n.set_vertical(5.0);
        assert_eq!(n.vertical, Some(MAX_RISE));
        n.set_vertical(f64::NAN);
        assert_eq!(n.vertical, Some(MAX_RISE));
        let mut later = Nudge::default();
        later.set_vertical(-10.0);
        n.merge(later);
        assert_eq!(n.vertical, Some(-MAX_DROP));
    }

    #[test]
    fn lift_needs_the_allowance_but_slowing_does_not() {
        let mut n = Nudge::default();
        n.set_vertical(0.0);
        // Falling: holding still is lift.
        assert_eq!(n.apply([0.0, -0.3, 0.0], true), ([0.0, 0.0, 0.0], true));
        assert_eq!(n.apply([0.0, -0.3, 0.0], false), ([0.0, -0.3, 0.0], false));
        // Rising: stopping is not.
        assert_eq!(n.apply([0.0, 0.3, 0.0], false), ([0.0, 0.0, 0.0], false));
    }

    #[test]
    fn pushes_add_up_to_a_bound_and_never_past_the_speed_cap() {
        let mut n = Nudge::default();
        for _ in 0..10 {
            n.add_push(0.05, 0.0);
        }
        assert!((n.push[0] - MAX_PUSH).abs() < 1e-12);
        let (v, _) = n.apply([0.55, 0.0, 0.0], false);
        assert!((v[0] - MAX_SPEED).abs() < 1e-12);
        // Faster than the cap (knockback): no gain, but braking applies.
        let (v, _) = n.apply([0.8, 0.0, 0.0], false);
        assert!((v[0] - 0.8).abs() < 1e-12);
        let mut brake = Nudge::default();
        brake.add_push(-0.1, 0.0);
        let (v, _) = brake.apply([0.8, 0.0, 0.0], false);
        assert!((v[0] - 0.7).abs() < 1e-12);
    }
}
