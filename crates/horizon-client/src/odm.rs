//! Attack on Titan's omni-directional mobility gear (`/aot`): two hooks fired
//! from the hips at what the player looks at, each winding its cable in so
//! the player swings around its anchor and closes on it, burning gas (more
//! for the faster reel). An empty tank winches nothing: the cables only hold.
//! Let go and the momentum carries on; there is no gas without a cable.
//!
//! The gear works while the player holds the ODM blades (`BLADES`, the `aot`
//! content mod's item, which `/aot` hands out).
//!
//! This is the state and the physics only: the game supplies the world (what
//! a hook hits, where an entity is) and applies the velocity it returns, with
//! a lighter drag while the gear flies (`MotionPlayer::odm_glide`). The
//! server just lets a player with the gear fly and fall unhurt.

/// How far a hook reaches (blocks).
pub const RANGE: f64 = 64.0;
/// How far a hook flies in a tick.
pub const HOOK_SPEED: f64 = 5.0;
/// The two hooks aim this many degrees left and right of the crosshair, so
/// they bite apart.
pub const SPREAD: f32 = 3.0;
/// The ticks a released hook takes to wind back in.
pub const RETRACT: u32 = 4;
/// A cable stops winding this close to its anchor (blocks).
pub const ARRIVE: f64 = 2.0;
/// How fast a cable winds in on its own, and with gas (blocks a tick): the
/// winch draws the player along the cable at least this fast, lifting them
/// off the ground toward a high anchor.
pub const REEL: f64 = 0.6;
pub const GAS_REEL: f64 = 1.2;
/// With gas the winch also speeds them up past its own pace (blocks a
/// tick, each tick), up to the gear's top speed.
pub const GAS_PULL: f64 = 0.06;
/// While a cable winches, the gear carries this much of the player's
/// weight (gravity is 0.08 a tick): the pull runs along the cable.
pub const CARRY: f64 = 0.075;
/// Steering in the air with the movement keys.
pub const STEER: f64 = 0.015;
/// The gear never goes faster (blocks a tick, 36 a second).
pub const MAX_SPEED: f64 = 1.8;
/// The horizontal drag while the gear flies (vanilla's air is 0.91).
pub const DRAG: f64 = 0.985;
/// Gas: a full tank is 1. What each use costs, and the refill a tick on
/// the ground.
pub const REEL_COST: f64 = 0.003;
pub const GAS_REEL_COST: f64 = 0.006;
pub const REFILL: f64 = 0.01;
/// Hips: this high above the feet, this far to each side.
const HIP_HEIGHT: f64 = 0.85;
const HIP_SIDE: f64 = 0.3;

pub const LEFT: usize = 0;
pub const RIGHT: usize = 1;

/// The gear's blades (the `aot` content mod's item).
pub const BLADES: &str = "aot:odm_blades";

/// Whether `stack` is the blades.
pub fn is_blades(stack: Option<&crate::items::ItemStack>) -> bool {
    stack.is_some_and(|s| &*s.item == BLADES)
}

/// A pair of blades as `/aot` hands them out: they never wear
/// (`minecraft:unbreakable`).
pub fn blades() -> crate::items::ItemStack {
    let mut stack = crate::items::ItemStack::new(BLADES, 1);
    stack.data_mut().set_other(
        "minecraft:unbreakable",
        Some(horizon_world::nbt::Tag::Compound(Default::default())),
    );
    stack
}

/// What a hook can hit.
pub trait World {
    /// The first thing along `dir` from `from` within `max` blocks: how far,
    /// and the entity (its id and where it stands) when it is one.
    fn cast(
        &mut self,
        from: [f64; 3],
        dir: [f64; 3],
        max: f64,
    ) -> Option<(f64, Option<(u32, [f64; 3])>)>;
    /// Where entity `id` stands now, if it still does.
    fn entity(&mut self, id: u32) -> Option<[f64; 3]>;
}

/// Where a hook holds: a point on a block, or on an entity it follows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Anchor {
    Block([f64; 3]),
    Entity { id: u32, offset: [f64; 3] },
}
impl Anchor {
    pub fn position(&self, world: &mut impl World) -> Option<[f64; 3]> {
        match *self {
            Self::Block(p) => Some(p),
            Self::Entity { id, offset } => world.entity(id).map(|e| add(e, offset)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hook {
    Idle,
    /// On its way to `anchor`, `travelled` blocks out.
    Flying {
        anchor: Anchor,
        travelled: f64,
    },
    /// Fired at nothing: out to its range along `dir`, then back.
    Missed {
        dir: [f64; 3],
        travelled: f64,
    },
    /// Holding, its cable `length` long.
    Attached {
        anchor: Anchor,
        length: f64,
    },
    /// Winding back from `from`, `ticks` left.
    Retracting {
        from: [f64; 3],
        ticks: u32,
    },
}

/// The keys the gear reads this tick.
#[derive(Clone, Copy, Debug, Default)]
pub struct Controls {
    /// The left and right hook keys held.
    pub hooks: [bool; 2],
    /// The gas key held.
    pub gas: bool,
    /// The movement keys (`Keys::direction`: left, forward).
    pub steer: [f32; 2],
}

/// The player as the gear sees it.
#[derive(Clone, Copy, Debug)]
pub struct Body {
    pub feet: [f64; 3],
    pub eye: [f64; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub velocity: [f64; 3],
    pub on_ground: bool,
    pub in_fluid: bool,
}

/// What a tick of the gear did, for sounds and particles.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Event {
    Fire(usize),
    Attach(usize, [f64; 3]),
    Release(usize),
    /// The gas key with an empty tank.
    Empty,
}

/// The gear's tick: the velocity to move with, whether its flight (the
/// lighter drag) takes the move, whether gas streamed, and what happened.
#[derive(Clone, Debug)]
pub struct Step {
    pub velocity: [f64; 3],
    pub glide: bool,
    pub gas: bool,
    pub events: Vec<Event>,
}

#[derive(Clone, Debug)]
pub struct Gear {
    pub hooks: [Hook; 2],
    /// The tank, 0 to 1.
    pub gas: f64,
    held: [bool; 2],
    gas_held: bool,
    /// In flight since a cable took them off the ground: the
    /// lighter drag lasts until they land.
    flying: bool,
}
impl Default for Gear {
    fn default() -> Self {
        Self::new()
    }
}

impl Gear {
    pub fn new() -> Self {
        Self {
            hooks: [Hook::Idle; 2],
            gas: 1.0,
            held: [false; 2],
            gas_held: false,
            flying: false,
        }
    }

    /// Put away (the blades out of hand): both hooks in at once, the keys
    /// let go and the flight over; the tank keeps what it has.
    pub fn stow(&mut self) {
        self.hooks = [Hook::Idle; 2];
        self.held = [false; 2];
        self.gas_held = false;
        self.flying = false;
    }

    /// Whether `side`'s hook holds.
    pub fn attached(&self, side: usize) -> bool {
        matches!(self.hooks[side], Hook::Attached { .. })
    }

    /// Where `side`'s hook is (its cable runs from `hip` to here), if out.
    pub fn tip(&self, side: usize, hip: [f64; 3], world: &mut impl World) -> Option<[f64; 3]> {
        match self.hooks[side] {
            Hook::Idle => None,
            Hook::Flying { anchor, travelled } => {
                let at = anchor.position(world)?;
                let d = sub(at, hip);
                let length = len(d);
                Some(if length <= travelled {
                    at
                } else {
                    add(hip, scale(d, travelled / length))
                })
            }
            Hook::Missed { dir, travelled } => Some(add(hip, scale(dir, travelled))),
            Hook::Attached { anchor, .. } => anchor.position(world),
            Hook::Retracting { from, ticks } => {
                let left = f64::from(ticks) / f64::from(RETRACT);
                Some(add(hip, scale(sub(from, hip), left)))
            }
        }
    }

    /// One tick of the gear for `body` under `controls`.
    pub fn tick(&mut self, world: &mut impl World, body: &Body, controls: &Controls) -> Step {
        let mut events = Vec::new();
        let mut v = body.velocity;
        let center = add(body.feet, [0.0, HIP_HEIGHT, 0.0]);
        let hips = hips(body.feet, body.yaw);
        // The triggers: a press fires, letting go winds the hook back.
        for side in [LEFT, RIGHT] {
            let held = controls.hooks[side];
            let pressed = held && !self.held[side];
            let released = !held && self.held[side];
            self.held[side] = held;
            if pressed && matches!(self.hooks[side], Hook::Idle | Hook::Retracting { .. }) {
                let offset = if side == LEFT { -SPREAD } else { SPREAD };
                let dir = view_vector(body.yaw + offset, body.pitch);
                self.hooks[side] = match world.cast(body.eye, dir, RANGE) {
                    Some((distance, entity)) => {
                        let point = add(body.eye, scale(dir, distance));
                        let anchor = match entity {
                            Some((id, at)) => Anchor::Entity {
                                id,
                                offset: sub(point, at),
                            },
                            None => Anchor::Block(point),
                        };
                        Hook::Flying {
                            anchor,
                            travelled: 0.0,
                        }
                    }
                    None => Hook::Missed {
                        dir,
                        travelled: 0.0,
                    },
                };
                events.push(Event::Fire(side));
            } else if released && !matches!(self.hooks[side], Hook::Idle | Hook::Retracting { .. })
            {
                let from = self.tip(side, hips[side], world).unwrap_or(hips[side]);
                self.hooks[side] = Hook::Retracting {
                    from,
                    ticks: RETRACT,
                };
                events.push(Event::Release(side));
            }
        }
        // The hooks fly, bite, or come back.
        for side in [LEFT, RIGHT] {
            let hip = hips[side];
            let back = |from| Hook::Retracting {
                from,
                ticks: RETRACT,
            };
            self.hooks[side] = match self.hooks[side] {
                Hook::Idle => Hook::Idle,
                Hook::Flying { anchor, travelled } => match anchor.position(world) {
                    None => back(hip),
                    Some(at) => {
                        let travelled = travelled + HOOK_SPEED;
                        if travelled >= distance(hip, at) {
                            events.push(Event::Attach(side, at));
                            Hook::Attached {
                                anchor,
                                length: distance(center, at),
                            }
                        } else {
                            Hook::Flying { anchor, travelled }
                        }
                    }
                },
                Hook::Missed { dir, travelled } => {
                    let travelled = travelled + HOOK_SPEED;
                    if travelled >= RANGE {
                        back(add(hip, scale(dir, RANGE)))
                    } else {
                        Hook::Missed { dir, travelled }
                    }
                }
                Hook::Attached { anchor, length } => match anchor.position(world) {
                    // An entity gone, or out of reach: the cable lets go.
                    None => back(hip),
                    Some(at) if distance(center, at) > RANGE + 16.0 => back(at),
                    Some(_) => Hook::Attached { anchor, length },
                },
                Hook::Retracting { from, ticks } => {
                    if ticks <= 1 {
                        Hook::Idle
                    } else {
                        Hook::Retracting {
                            from,
                            ticks: ticks - 1,
                        }
                    }
                }
            };
        }
        let anchors: Vec<(usize, [f64; 3])> = [LEFT, RIGHT]
            .into_iter()
            .filter_map(|side| match self.hooks[side] {
                Hook::Attached { anchor, .. } => anchor.position(world).map(|at| (side, at)),
                _ => None,
            })
            .collect();
        let gas_pressed = controls.gas && !self.gas_held;
        self.gas_held = controls.gas;
        let has_gas = self.gas > 0.0;
        if gas_pressed && !has_gas && !anchors.is_empty() {
            events.push(Event::Empty);
        }
        let airborne = !body.on_ground;
        let mut gas = false;
        if !anchors.is_empty() {
            self.flying = true;
            // Standing, the ground holds the player up, not the cables.
            if body.on_ground {
                v[1] = v[1].max(0.0);
            }
            // The winch burns gas, the fast reel twice as much; with the
            // tank empty the cables only hold.
            let (rate, pull, cost) = if !has_gas {
                (0.0, 0.0, 0.0)
            } else if controls.gas {
                (GAS_REEL, GAS_PULL, GAS_REEL_COST)
            } else {
                (REEL, 0.0, REEL_COST)
            };
            let mut winching = false;
            for &(side, at) in &anchors {
                let Hook::Attached { length, .. } = &mut self.hooks[side] else {
                    continue;
                };
                let d = sub(at, center);
                let dist = len(d);
                if dist < 1.0e-6 {
                    continue;
                }
                // The cable winds in and never pays out: shorter than the
                // player is far, less what it winds this tick.
                *length = length.min(dist.max(ARRIVE));
                if dist > ARRIVE && rate > 0.0 {
                    winching = true;
                    *length = (*length - rate).max(ARRIVE);
                    // The winch: along the cable at least at its pace.
                    let n = scale(d, 1.0 / dist);
                    let inward = dot(v, n);
                    if inward < rate {
                        v = add(v, scale(n, rate - inward));
                    }
                    v = add(v, scale(n, pull));
                }
                // Taut: where this velocity would take them is held to the
                // cable's length, which keeps what goes around (the swing).
                let next = sub(at, add(center, v));
                let reach = len(next);
                if reach > *length {
                    v = add(v, scale(next, (reach - *length) / reach));
                }
            }
            if winching {
                v[1] += CARRY;
                self.gas -= cost;
                gas = controls.gas;
            }
        }
        self.gas = self.gas.clamp(0.0, 1.0);
        let glide = !anchors.is_empty() || (airborne && self.flying);
        if glide {
            v = add(v, scale(steer_vector(controls.steer, body.yaw), STEER));
        }
        let speed = len(v);
        if speed > MAX_SPEED {
            v = scale(v, MAX_SPEED / speed);
        }
        if body.on_ground && anchors.is_empty() {
            self.flying = false;
            // The tank refills on the ground with both hooks in.
            if self.hooks.iter().all(|h| *h == Hook::Idle) {
                self.gas = (self.gas + REFILL).min(1.0);
            }
        }
        Step {
            velocity: v,
            glide,
            gas,
            events,
        }
    }
}

/// The left and right hips of a player standing at `feet` facing `yaw`.
pub fn hips(feet: [f64; 3], yaw: f32) -> [[f64; 3]; 2] {
    let center = add(feet, [0.0, HIP_HEIGHT, 0.0]);
    let right = right_vector(yaw);
    [
        sub(center, scale(right, HIP_SIDE)),
        add(center, scale(right, HIP_SIDE)),
    ]
}

/// The player's right, facing `yaw` (south is 0, west 90).
pub fn right_vector(yaw: f32) -> [f64; 3] {
    let y = f64::from(yaw).to_radians();
    [-y.cos(), 0.0, -y.sin()]
}

/// `Entity.calculateViewVector`.
pub fn view_vector(yaw: f32, pitch: f32) -> [f64; 3] {
    let (y, p) = (f64::from(yaw).to_radians(), f64::from(pitch).to_radians());
    [-y.sin() * p.cos(), -p.sin(), y.cos() * p.cos()]
}

/// The movement keys turned to the world (`Entity.getInputVector`), unit
/// length at most.
fn steer_vector(steer: [f32; 2], yaw: f32) -> [f64; 3] {
    let (x, z) = (f64::from(steer[0]), f64::from(steer[1]));
    let length = (x * x + z * z).sqrt();
    if length < 1.0e-4 {
        return [0.0; 3];
    }
    let (x, z) = if length > 1.0 {
        (x / length, z / length)
    } else {
        (x, z)
    };
    let y = f64::from(yaw).to_radians();
    let (s, c) = (y.sin(), y.cos());
    [x * c - z * s, 0.0, z * c + x * s]
}

fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn scale(a: [f64; 3], s: f64) -> [f64; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn len(a: [f64; 3]) -> f64 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}
fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
    len(sub(a, b))
}
/// The `horizon:aot` payloads. From the player: `[1, flags, tips...]`, the
/// hooks out (flags bit 0 left, bit 1 right), holding (bits 3 and 4) and the
/// gas (bit 2), then each out hook's tip as three big-endian f32. To the
/// others: `[1, entity id (i32), flags, tips...]`.
pub mod wire {
    pub const CHANNEL: &str = "horizon:aot";
    pub const STATE: u8 = 1;
    pub const LEFT_OUT: u8 = 1;
    pub const RIGHT_OUT: u8 = 2;
    pub const GAS: u8 = 4;
    pub const LEFT_HOLDS: u8 = 8;
    pub const RIGHT_HOLDS: u8 = 16;

    /// A hooks' state: the flags and the tips of the hooks out.
    #[derive(Clone, Copy, Debug, Default, PartialEq)]
    pub struct State {
        pub flags: u8,
        pub tips: [Option<[f64; 3]>; 2],
    }
    impl State {
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.push(self.flags);
            for (side, bit) in [(0, LEFT_OUT), (1, RIGHT_OUT)] {
                if self.flags & bit != 0 {
                    let tip = self.tips[side].unwrap_or_default();
                    for v in tip {
                        out.extend_from_slice(&(v as f32).to_be_bytes());
                    }
                }
            }
        }
        /// None when malformed or not finite.
        pub fn decode(data: &[u8]) -> Option<Self> {
            let (&flags, mut rest) = data.split_first()?;
            if flags & !(LEFT_OUT | RIGHT_OUT | GAS | LEFT_HOLDS | RIGHT_HOLDS) != 0 {
                return None;
            }
            let mut tips = [None; 2];
            for (side, bit) in [(0, LEFT_OUT), (1, RIGHT_OUT)] {
                if flags & bit != 0 {
                    let mut tip = [0.0; 3];
                    for v in &mut tip {
                        let (bytes, tail) = rest.split_first_chunk::<4>()?;
                        let f = f32::from_be_bytes(*bytes);
                        if !f.is_finite() {
                            return None;
                        }
                        *v = f64::from(f);
                        rest = tail;
                    }
                    tips[side] = Some(tip);
                }
            }
            rest.is_empty().then_some(Self { flags, tips })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A flat world with a wall of blocks at `z = wall` from the ground up to
    /// `top`, and entities standing at fixed places.
    struct Wall {
        wall: f64,
        top: f64,
        entities: Vec<(u32, [f64; 3])>,
    }
    impl World for Wall {
        fn cast(
            &mut self,
            from: [f64; 3],
            dir: [f64; 3],
            max: f64,
        ) -> Option<(f64, Option<(u32, [f64; 3])>)> {
            if dir[2] <= 0.0 {
                return None;
            }
            let t = (self.wall - from[2]) / dir[2];
            let y = from[1] + dir[1] * t;
            (t <= max && (0.0..=self.top).contains(&y)).then_some((t, None))
        }
        fn entity(&mut self, id: u32) -> Option<[f64; 3]> {
            self.entities.iter().find(|e| e.0 == id).map(|e| e.1)
        }
    }

    fn body(feet: [f64; 3], velocity: [f64; 3], pitch: f32, on_ground: bool) -> Body {
        Body {
            feet,
            eye: add(feet, [0.0, 1.62, 0.0]),
            yaw: 0.0,
            pitch,
            velocity,
            on_ground,
            in_fluid: false,
        }
    }

    /// Runs the gear and a plain mover (gravity, the gear's drag, the
    /// ground at y = 0, the wall) for `ticks`.
    fn fly(
        gear: &mut Gear,
        world: &mut Wall,
        mut feet: [f64; 3],
        pitch: f32,
        controls: impl Fn(u32) -> Controls,
        ticks: u32,
    ) -> Vec<[f64; 3]> {
        let mut v = [0.0; 3];
        let mut path = Vec::new();
        for t in 0..ticks {
            let on_ground = feet[1] <= 0.0;
            let step = gear.tick(world, &body(feet, v, pitch, on_ground), &controls(t));
            v = step.velocity;
            feet = add(feet, v);
            if feet[1] < 0.0 {
                feet[1] = 0.0;
                v[1] = 0.0;
            }
            feet[2] = feet[2].min(world.wall - 0.3);
            v[1] -= 0.08;
            v = [v[0] * DRAG, v[1] * 0.98, v[2] * DRAG];
            path.push(feet);
        }
        path
    }

    #[test]
    fn a_hook_flies_out_bites_and_lets_go() {
        let mut world = Wall {
            wall: 20.0,
            top: 40.0,
            entities: vec![],
        };
        let mut gear = Gear::new();
        let b = body([0.0, 0.0, 0.0], [0.0; 3], -30.0, true);
        let hold = Controls {
            hooks: [true, false],
            ..Default::default()
        };
        let step = gear.tick(&mut world, &b, &hold);
        assert_eq!(step.events, vec![Event::Fire(LEFT)]);
        assert!(matches!(gear.hooks[LEFT], Hook::Flying { .. }));
        let mut attached = false;
        for _ in 0..10 {
            let step = gear.tick(&mut world, &b, &hold);
            attached |= step
                .events
                .iter()
                .any(|e| matches!(e, Event::Attach(LEFT, _)));
        }
        assert!(attached && gear.attached(LEFT));
        let step = gear.tick(&mut world, &b, &Controls::default());
        assert_eq!(step.events, vec![Event::Release(LEFT)]);
        for _ in 0..RETRACT {
            gear.tick(&mut world, &b, &Controls::default());
        }
        assert_eq!(gear.hooks[LEFT], Hook::Idle);
    }

    #[test]
    fn a_miss_flies_to_its_range_and_comes_back() {
        let mut world = Wall {
            wall: 1000.0,
            top: 40.0,
            entities: vec![],
        };
        let mut gear = Gear::new();
        let b = body([0.0, 0.0, 0.0], [0.0; 3], 0.0, true);
        let hold = Controls {
            hooks: [false, true],
            ..Default::default()
        };
        gear.tick(&mut world, &b, &hold);
        assert!(matches!(gear.hooks[RIGHT], Hook::Missed { .. }));
        for _ in 0..(RANGE / HOOK_SPEED).ceil() as u32 {
            gear.tick(&mut world, &b, &hold);
        }
        assert!(matches!(gear.hooks[RIGHT], Hook::Retracting { .. }));
    }

    #[test]
    fn the_cable_lifts_and_pulls_the_player_to_the_wall() {
        let mut world = Wall {
            wall: 30.0,
            top: 40.0,
            entities: vec![],
        };
        let mut gear = Gear::new();
        let hooks = |_| Controls {
            hooks: [true, true],
            ..Default::default()
        };
        let path = fly(&mut gear, &mut world, [0.0, 0.0, 0.0], -30.0, hooks, 120);
        let end = path.last().copied().unwrap();
        // Up the wall near where the hooks bit (about 17 up at 30 out).
        assert!(end[2] > 25.0, "{end:?}");
        assert!(end[1] > 10.0, "{end:?}");
        // Gas gets there sooner.
        let mut gear = Gear::new();
        let gas = |_| Controls {
            hooks: [true, true],
            gas: true,
            ..Default::default()
        };
        let quick = fly(&mut gear, &mut world, [0.0, 0.0, 0.0], -30.0, gas, 120);
        let arrive = |p: &[[f64; 3]]| p.iter().position(|f| f[2] > 25.0).unwrap();
        assert!(arrive(&quick) < arrive(&path));
        assert!(gear.gas < 1.0);
    }

    #[test]
    fn a_high_anchor_lifts_the_player_off_the_ground() {
        let mut world = Wall {
            wall: 50.0,
            top: 60.0,
            entities: vec![],
        };
        let mut gear = Gear::new();
        let hooks = |_| Controls {
            hooks: [true, true],
            ..Default::default()
        };
        let path = fly(&mut gear, &mut world, [0.0, 0.0, 0.0], -25.0, hooks, 40);
        // The hooks bite after about 11 ticks; a few later the player is up.
        assert!(path[20][1] > 1.5, "{:?}", &path[..21]);
        assert!(path[39][1] > path[20][1]);
    }

    #[test]
    fn letting_go_mid_swing_keeps_the_momentum() {
        let mut world = Wall {
            wall: 60.0,
            top: 60.0,
            entities: vec![],
        };
        let mut gear = Gear::new();
        // Hold a while, then let go and see how far the momentum carries.
        let controls = |t| Controls {
            hooks: [t < 22; 2],
            gas: t < 22,
            ..Default::default()
        };
        let path = fly(&mut gear, &mut world, [0.0, 10.0, 0.0], -10.0, controls, 34);
        let released = path[22];
        let later = path[33];
        assert!(later[2] - released[2] > 5.0, "{released:?} {later:?}");
    }

    #[test]
    fn a_hook_rides_an_entity_and_lets_go_when_it_leaves() {
        struct Titan(Option<[f64; 3]>);
        impl World for Titan {
            fn cast(
                &mut self,
                _: [f64; 3],
                _: [f64; 3],
                _: f64,
            ) -> Option<(f64, Option<(u32, [f64; 3])>)> {
                Some((10.0, Some((7, self.0?))))
            }
            fn entity(&mut self, id: u32) -> Option<[f64; 3]> {
                (id == 7).then_some(self.0?)
            }
        }
        let mut world = Titan(Some([0.0, 0.0, 10.0]));
        let mut gear = Gear::new();
        let b = body([0.0, 0.0, 0.0], [0.0; 3], 0.0, true);
        let hold = Controls {
            hooks: [true, false],
            ..Default::default()
        };
        for _ in 0..4 {
            gear.tick(&mut world, &b, &hold);
        }
        assert!(gear.attached(LEFT));
        // The anchor follows the entity, where on it the hook bit.
        let hip = hips(b.feet, b.yaw)[LEFT];
        let before = gear.tip(LEFT, hip, &mut world).unwrap();
        world.0 = Some([5.0, 0.0, 10.0]);
        let tip = gear.tip(LEFT, hip, &mut world).unwrap();
        assert!(
            (tip[0] - before[0] - 5.0).abs() < 1e-9,
            "{before:?} {tip:?}"
        );
        world.0 = None;
        gear.tick(&mut world, &b, &hold);
        assert!(matches!(gear.hooks[LEFT], Hook::Retracting { .. }));
    }

    #[test]
    fn no_gas_in_the_open_air() {
        let mut world = Wall {
            wall: 1000.0,
            top: 0.0,
            entities: vec![],
        };
        let mut gear = Gear::new();
        let up = body([0.0, 20.0, 0.0], [0.0, 0.1, 0.2], 0.0, false);
        let space = Controls {
            gas: true,
            steer: [0.0, 1.0],
            ..Default::default()
        };
        for _ in 0..5 {
            let step = gear.tick(&mut world, &up, &space);
            assert_eq!(step.velocity, up.velocity);
            assert!(!step.glide && !step.gas && step.events.is_empty());
        }
        assert_eq!(gear.gas, 1.0);
    }

    #[test]
    fn the_winch_burns_gas_and_an_empty_tank_only_holds() {
        let mut world = Wall {
            wall: 30.0,
            top: 40.0,
            entities: vec![],
        };
        let mut gear = Gear::new();
        let hooks = |_| Controls {
            hooks: [true, true],
            ..Default::default()
        };
        fly(&mut gear, &mut world, [0.0, 0.0, 0.0], -30.0, hooks, 30);
        assert!(gear.gas < 1.0 && gear.gas > 0.9, "{}", gear.gas);
        // Empty: the cables hold but winch nothing, so the player hangs.
        let mut gear = Gear::new();
        gear.gas = 0.0;
        let path = fly(&mut gear, &mut world, [0.0, 0.0, 0.0], -30.0, hooks, 60);
        assert!(
            path.iter().all(|f| f[1] < 0.5 && f[2] < 1.0),
            "{:?}",
            path.last()
        );
        // Space with an empty tank while hooked clicks.
        let b = body([0.0, 0.0, 0.0], [0.0; 3], -30.0, true);
        let step = gear.tick(
            &mut world,
            &b,
            &Controls {
                hooks: [true, true],
                gas: true,
                ..Default::default()
            },
        );
        assert_eq!(step.events, vec![Event::Empty]);
        // The ground fills the tank again.
        let mut gear = Gear::new();
        gear.gas = 0.0;
        for _ in 0..100 {
            gear.tick(&mut world, &b, &Controls::default());
        }
        assert_eq!(gear.gas, 1.0);
    }

    #[test]
    fn a_plain_jump_keeps_the_usual_drag() {
        let mut world = Wall {
            wall: 1000.0,
            top: 0.0,
            entities: vec![],
        };
        let mut gear = Gear::new();
        let step = gear.tick(
            &mut world,
            &body([0.0, 0.5, 0.0], [0.0, 0.3, 0.2], 0.0, false),
            &Controls::default(),
        );
        assert!(!step.glide);
    }

    #[test]
    fn the_speed_is_capped() {
        let mut world = Wall {
            wall: 1000.0,
            top: 0.0,
            entities: vec![],
        };
        let mut gear = Gear::new();
        let step = gear.tick(
            &mut world,
            &body([0.0, 30.0, 0.0], [0.0, -5.0, 0.0], 0.0, false),
            &Controls::default(),
        );
        assert!(len(step.velocity) <= MAX_SPEED + 1e-9);
    }

    #[test]
    fn the_wire_state_round_trips_and_rejects_junk() {
        use wire::*;
        let state = State {
            flags: LEFT_OUT | GAS | LEFT_HOLDS,
            tips: [Some([1.5, 64.0, -3.25]), None],
        };
        let mut data = Vec::new();
        state.encode(&mut data);
        assert_eq!(data.len(), 13);
        assert_eq!(State::decode(&data), Some(state));
        assert_eq!(State::decode(&data[..12]), None);
        assert_eq!(State::decode(&[0x80]), None);
        let mut nan = vec![LEFT_OUT];
        nan.extend_from_slice(&f32::NAN.to_be_bytes());
        nan.extend_from_slice(&[0; 8]);
        assert_eq!(State::decode(&nan), None);
    }

    #[test]
    fn the_hips_are_either_side_of_the_facing() {
        // Facing south (+z), the right hand is west (-x).
        let [left, right] = hips([0.0, 0.0, 0.0], 0.0);
        assert!(left[0] > 0.0 && right[0] < 0.0);
        let s = steer_vector([1.0, 0.0], 0.0);
        assert!(s[0] > 0.9, "left is east facing south: {s:?}");
    }
}
