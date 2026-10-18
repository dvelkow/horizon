//! `SpearAnimations` (26.3): how a spear is held and charged. In first person
//! the charge raises the spear level, sways it once the dismount window
//! closes, lowers it as the knockback one does and raises it back when the
//! damage one ends; in third person the arm follows the head with the spear
//! couched. A kinetic hit (entity event 2) jolts it back.
#[cfg(target_arch = "wasm32")]
use horizon_platform as std;

use crate::items::Kinetic;
use glam::{Mat4, Vec3};
use horizon_core::geometry::trig::Mth;

/// `net.minecraft.util.Ease`, the curves the poses follow.
mod ease {
    pub fn in_quad(x: f32) -> f32 {
        x * x
    }
    pub fn out_cubic(x: f32) -> f32 {
        1.0 - (1.0 - x).powi(3)
    }
    pub fn out_quart(x: f32) -> f32 {
        1.0 - (1.0 - x).powi(4)
    }
    pub fn out_circ(x: f32) -> f32 {
        (1.0 - (x - 1.0) * (x - 1.0)).sqrt()
    }
    pub fn in_circ(x: f32) -> f32 {
        -(1.0 - x * x).sqrt() + 1.0
    }
    pub fn in_out_sine(x: f32) -> f32 {
        use horizon_core::geometry::trig::Mth;
        -((std::f32::consts::PI * x).mcos() - 1.0) / 2.0
    }
    pub fn out_back(x: f32) -> f32 {
        1.0 + 2.70158 * (x - 1.0).powi(3) + 1.70158 * (x - 1.0).powi(2)
    }
    pub fn in_out_back(x: f32) -> f32 {
        if x < 0.5 {
            return 4.0 * x * x * (7.189819 * x - 2.5949094) / 2.0;
        }
        let dt = 2.0 * x - 2.0;
        (dt * dt * (3.5949094 * dt + 2.5949094) + 2.0) / 2.0
    }
    pub fn in_out_expo(x: f32) -> f32 {
        if x < 0.5 {
            if x == 0.0 {
                0.0
            } else {
                (2f64.powf(20.0 * f64::from(x) - 10.0) / 2.0) as f32
            }
        } else if x == 1.0 {
            1.0
        } else {
            ((2.0 - 2f64.powf(-20.0 * f64::from(x) + 10.0)) / 2.0) as f32
        }
    }
    pub fn in_out_elastic(x: f32) -> f32 {
        if x == 0.0 || x == 1.0 {
            return x;
        }
        let x = f64::from(x);
        let sin = ((20.0 * x - 11.125) * f64::from(std::f32::consts::PI * 4.0 / 9.0)).sin();
        (if x < 0.5 {
            -(2f64.powf(20.0 * x - 10.0) * sin) / 2.0
        } else {
            2f64.powf(-20.0 * x + 10.0) * sin / 2.0 + 1.0
        }) as f32
    }
}

/// `progress`: how far `time` is from `start` to `end`, within 0..1.
fn progress(time: f32, start: f32, end: f32) -> f32 {
    let v = (time - start) / (end - start);
    if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) }
}

/// The parts of a `kinetic_weapon` the poses read: its delay, the three
/// windows (ticks) and how far the spear reaches forward.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Timings {
    pub delay: f32,
    pub dismount: f32,
    pub knockback: f32,
    pub damage: f32,
    pub forward_movement: f32,
}
impl Timings {
    pub fn of(k: &Kinetic) -> Self {
        let window = |c: Option<crate::items::KineticCondition>| {
            c.map_or(0.0, |c| c.max_duration_ticks as f32)
        };
        Self {
            delay: k.delay_ticks as f32,
            dismount: window(k.dismount),
            knockback: window(k.knockback),
            damage: window(k.damage),
            forward_movement: k.forward_movement,
        }
    }
}

/// `SpearAnimations.UseParams.fromKineticWeapon` at `time` ticks into the use.
#[derive(Clone, Copy, Debug)]
struct UseParams {
    raise: f32,
    raise_start: f32,
    raise_middle: f32,
    raise_end: f32,
    sway: f32,
    lower: f32,
    raise_back: f32,
    sway_intensity: f32,
    sway_slow: f32,
    sway_fast: f32,
}
impl UseParams {
    fn new(t: &Timings, time: f32) -> Self {
        let finish_raising = t.delay;
        let finish_swaying = t.dismount + finish_raising;
        let start_swaying = finish_swaying - 20.0;
        let finish_lowering = t.knockback + finish_raising;
        let start_lowering = finish_lowering - 40.0;
        let finish_raising_back = t.damage + finish_raising;
        let raise = progress(time, 0.0, finish_raising);
        let sway = progress(time, start_swaying, finish_swaying);
        let raise_back = progress(time, finish_raising_back - 5.0, finish_raising_back);
        let sway_intensity = 2.0 * ease::out_circ(sway) - 2.0 * ease::in_circ(raise_back);
        let deg = std::f32::consts::PI / 180.0;
        Self {
            raise,
            raise_start: progress(raise, 0.0, 0.5),
            raise_middle: progress(raise, 0.5, 0.8),
            raise_end: progress(raise, 0.8, 1.0),
            sway,
            lower: ease::out_cubic(ease::in_out_elastic(progress(
                time - 20.0,
                start_lowering,
                finish_lowering,
            ))),
            raise_back,
            sway_intensity,
            sway_slow: (time * 19.0 * deg).msin() * sway_intensity,
            sway_fast: (time * 30.0 * deg).msin() * sway_intensity,
        }
    }
}

/// `hitFeedbackAmount`: the jolt back after a kinetic hit, `ticks` since it.
pub fn hit_feedback(ticks: f32) -> f32 {
    0.4 * (ease::out_quart(progress(ticks, 1.0, 3.0))
        - ease::in_out_sine(progress(ticks, 3.0, 10.0)))
}

/// `PoseStack.rotateAround`.
fn around(rotation: Mat4, x: f32, y: f32, z: f32) -> Mat4 {
    let pivot = Vec3::new(x, y, z);
    Mat4::from_translation(pivot) * rotation * Mat4::from_translation(-pivot)
}

/// `FirstPersonHandsAndItemsRenderer` for the SPEAR use animation with
/// `SpearAnimations.firstPersonUse`: the pose of the spear in use on the
/// `left` or right arm, `time` ticks held and `since_hit` ticks after a
/// kinetic hit (0 without one).
pub fn first_person_use(t: &Timings, time: f32, since_hit: f32, left: bool) -> Mat4 {
    let p = UseParams::new(t, time);
    let invert = if left { -1.0 } else { 1.0 };
    let mut m = Mat4::from_translation(Vec3::new(invert * 0.56, -0.52, -0.72));
    m *= Mat4::from_translation(Vec3::new(
        invert * (p.raise * 0.15 + p.raise_end * -0.05 + p.sway * -0.1 + p.sway_slow * 0.005),
        p.raise * -0.075 + p.raise_middle * 0.075 + p.sway_fast * 0.01,
        p.raise_start * 0.05 + p.raise_end * -0.05 + p.sway_slow * 0.005,
    ));
    let pitch = -65.0 * ease::in_out_back(p.raise) - 35.0 * p.lower
        + 100.0 * p.raise_back
        + -0.5 * p.sway_fast;
    m *= around(Mat4::from_rotation_x(pitch.to_radians()), 0.0, 0.1, 0.0);
    let yaw = invert * (-90.0 * progress(p.raise, 0.5, 0.55) + 90.0 * p.sway + 2.0 * p.sway_slow);
    m *= around(
        Mat4::from_rotation_y((-yaw).to_radians()),
        invert * 0.15,
        0.0,
        0.0,
    );
    m *= Mat4::from_translation(Vec3::new(0.0, -hit_feedback(since_hit), 0.0));
    m
}

/// `SpearAnimations.firstPersonAttack`: the stab on the `left` or right arm,
/// `attack` of the way through the swing, after the arm's own offset.
pub fn first_person_attack(attack: f32, left: bool) -> Mat4 {
    let invert = if left { -1.0 } else { 1.0 };
    let starting = ease::in_out_sine(progress(attack, 0.0, 0.05));
    let middle = ease::out_back(progress(attack, 0.05, 0.2));
    let ending = ease::in_out_expo(progress(attack, 0.4, 1.0));
    Mat4::from_translation(Vec3::new(
        invert * 0.1 * (starting - middle),
        -0.075 * (starting - ending),
        0.65 * (starting - middle),
    )) * Mat4::from_rotation_x((-70.0 * (starting - ending)).to_radians())
        * Mat4::from_translation(Vec3::new(0.0, 0.0, -0.25 * (ending - middle)))
}

/// `SpearAnimations.thirdPersonHandUse`: the arm holding a spear, as
/// (x, y, z) rotations, from the head's x and y rotation, the arm's own z
/// rotation and, while charging, the use's timings and ticks.
pub fn third_person_hand(
    head: (f32, f32),
    z_rot: f32,
    left: bool,
    flying_or_swimming: bool,
    using: Option<(&Timings, f32)>,
) -> (f32, f32, f32) {
    use std::f32::consts::PI;
    let invert = if left { -1.0 } else { 1.0 };
    let deg = PI / 180.0;
    let mut y = -0.1 * invert + head.1;
    let mut x = -PI / 2.0 + head.0 + 0.8;
    let mut z = z_rot;
    if flying_or_swimming {
        x -= 0.959_931_1;
    }
    y = deg * (y / deg).clamp(-60.0, 60.0);
    x = deg * (x / deg).clamp(-120.0, 30.0);
    if let Some((t, ticks)) = using.filter(|(_, ticks)| *ticks > 0.0) {
        let p = UseParams::new(t, ticks);
        y += -invert * p.sway_fast * deg * p.sway_intensity;
        z += -invert * p.sway_slow * deg * p.sway_intensity * 0.5;
        x += deg
            * (-40.0 * p.raise_start
                + 30.0 * p.raise_middle
                + -20.0 * p.raise_end
                + 20.0 * p.lower
                + 10.0 * p.raise_back
                + 0.6 * p.sway_slow * p.sway_intensity);
    }
    (x, y, z)
}

/// `SpearAnimations.thirdPersonUseItem`: the spear in use in a third-person
/// hand, after the hand's offset; `swing` is the attack animation.
pub fn third_person_item(t: &Timings, time: f32, swing: f32, since_hit: f32, left: bool) -> Mat4 {
    if time == 0.0 {
        return Mat4::IDENTITY;
    }
    let attack = ease::in_quad(progress(swing, 0.05, 0.2));
    let retract = ease::in_out_expo(progress(swing, 0.4, 1.0));
    let p = UseParams::new(t, time);
    let invert = if left { -1.0 } else { 1.0 };
    let raise = 1.0 - ease::out_back(1.0 - p.raise);
    let jolt = hit_feedback(since_hit);
    let mut m = Mat4::from_translation(Vec3::new(
        0.0,
        -jolt * 0.4,
        -t.forward_movement * (raise - p.raise_back) + jolt,
    ));
    let pitch = 70.0 * (p.raise - p.raise_back) - 40.0 * (attack - retract);
    m *= around(
        Mat4::from_rotation_x((-pitch).to_radians()),
        0.0,
        -0.03125,
        0.125,
    );
    let yaw = invert * 90.0 * (p.raise - p.sway + 3.0 * retract + attack);
    m *= around(Mat4::from_rotation_y(yaw.to_radians()), 0.0, 0.0, 0.125);
    m
}

/// `SpearAnimations.thirdPersonAttackHand`: what a stab adds to the
/// swinging arm's x rotation, `swing` of the way through it (the body's
/// turn is taken back off the arms by the caller).
pub fn third_person_attack_arm(swing: f32) -> f32 {
    let prepare = ease::in_out_sine(progress(swing, 0.0, 0.05));
    let attack = ease::in_quad(progress(swing, 0.05, 0.2));
    let retract = ease::in_out_expo(progress(swing, 0.4, 1.0));
    (90.0 * prepare - 120.0 * attack + 30.0 * retract).to_radians()
}

/// `SpearAnimations.thirdPersonAttackItem`: the stabbing spear in a
/// third-person hand, after the hand's offset.
pub fn third_person_attack_item(swing: f32, forward_movement: f32) -> Mat4 {
    if swing <= 0.0 {
        return Mat4::IDENTITY;
    }
    let attack = ease::in_quad(progress(swing, 0.05, 0.2));
    let retract = ease::in_out_expo(progress(swing, 0.4, 1.0));
    around(
        Mat4::from_rotation_x((-70.0 * (attack - retract)).to_radians()),
        0.0,
        -0.125,
        0.125,
    ) * Mat4::from_translation(Vec3::new(0.0, forward_movement * (attack - retract), 0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iron() -> Timings {
        Timings {
            delay: 12.0,
            dismount: 50.0,
            knockback: 135.0,
            damage: 225.0,
            forward_movement: 0.38,
        }
    }

    #[test]
    fn the_charge_raises_then_lowers_and_raises_back() {
        let t = iron();
        let start = UseParams::new(&t, 0.0);
        assert_eq!(
            (start.raise, start.lower, start.raise_back),
            (0.0, 0.0, 0.0)
        );
        let raised = UseParams::new(&t, 12.0);
        assert_eq!(raised.raise, 1.0);
        assert!(raised.sway_intensity.abs() < 1e-6);
        // The sway peaks at the end of the dismount window (62 ticks).
        assert!((UseParams::new(&t, 62.0).sway - 1.0).abs() < 1e-6);
        // Lowered once the knockback window (147 ticks, 20 late) is over.
        assert!((UseParams::new(&t, 170.0).lower - 1.0).abs() < 1e-3);
        assert_eq!(UseParams::new(&t, 237.0).raise_back, 1.0);
    }

    #[test]
    fn hit_feedback_jolts_and_settles() {
        assert_eq!(hit_feedback(0.0), 0.0);
        assert!((hit_feedback(3.0) - 0.4).abs() < 1e-6);
        assert!(hit_feedback(10.0).abs() < 1e-6);
    }

    #[test]
    fn the_held_spear_follows_the_head() {
        let (x, y, _) = third_person_hand((0.0, 0.0), 0.0, false, false, None);
        assert!((x - (-std::f32::consts::FRAC_PI_2 + 0.8)).abs() < 1e-6);
        assert!((y + 0.1).abs() < 1e-6);
        // Looking straight up clamps the arm at 120 degrees.
        let (x, ..) = third_person_hand((-std::f32::consts::PI, 0.0), 0.0, false, false, None);
        assert!((x - (-120f32).to_radians()).abs() < 1e-5);
    }
}
