//! The `/aot` gear in play (its physics are `horizon_client::odm`): each tick
//! before the move, the hooks' casts against the blocks and the mobs and
//! players around, the gear's velocity, its sounds and gas; the cables drawn
//! for this player and for the others the server relays (`horizon:aot`); and
//! the HUD's range and gas gauge by the crosshair.
use super::{Game, Screen};
use horizon_client::{
    gizmos::Line,
    hud::HudBatch,
    odm::{self, Body, Controls, Event, Gear, Hook, wire},
    particles::{ParticleOptions, ParticleType},
    text::Text,
};
use horizon_core::{geometry::Vec3, random::RandomSource};
#[cfg(target_arch = "wasm32")]
use horizon_platform as std;
use horizon_protocol::packets::{configuration::CustomPayload, play::Serverbound as S};
use std::collections::HashMap;
use winit::keyboard::KeyCode;

/// What `/aot` says when the gear goes on (the server sends the same).
pub(super) const HELP: &str = "ODM gear on: hold Q / E for the left / right hook, Space for gas \
     (a burst in the air, or a fast reel while hooked). Shift+E opens the inventory. No fall \
     damage. Type /aot again to take it off.";
pub(super) const OFF: &str = "ODM gear off.";

/// The cable and the hook's head.
const CABLE: u32 = 0xFF6A_7078;
const HEAD: u32 = 0xFFB8_BEC6;

/// Another player's gear, as the server last relayed it.
struct Other {
    state: wire::State,
    /// Ticks since it came.
    age: u32,
}

#[derive(Default)]
pub(super) struct OdmState {
    /// The gear, while `/aot` is on.
    pub gear: Option<Gear>,
    /// This player's hook tips last tick and this tick (drawn between).
    tips: [[Option<[f64; 3]>; 2]; 2],
    /// Gas streamed this tick.
    gas: bool,
    /// How far a hook fired at the crosshair would fly, when it would bite.
    aim: Option<f64>,
    /// Other players' cables, by entity id.
    others: HashMap<i32, Other>,
    /// The state last sent to the server, and the tick it went.
    sent: Option<(Vec<u8>, u64)>,
    ticks: u64,
}

/// What a hook flies through: the blocks' collision shapes and the mobs and
/// players around.
struct Surroundings<'a, 'g> {
    game: &'a Game<'g>,
}
impl odm::World for Surroundings<'_, '_> {
    fn cast(
        &mut self,
        from: [f64; 3],
        dir: [f64; 3],
        max: f64,
    ) -> Option<(f64, Option<(u32, [f64; 3])>)> {
        let to = [
            from[0] + dir[0] * max,
            from[1] + dir[1] * max,
            from[2] + dir[2] * max,
        ];
        let block = self.game.collider_distance(from, to);
        let reach = block.unwrap_or(max);
        let entity = self
            .game
            .mobs
            .iter()
            .filter(|m| m.alive() && !m.removed)
            .filter_map(|m| Some((super::ray_box(from, dir, reach, m.bounds())?, m)))
            // Not something the player stands in.
            .filter(|(t, _)| *t > 0.5)
            .min_by(|a, b| a.0.total_cmp(&b.0));
        match entity {
            Some((t, m)) => Some((t, Some((m.id, [m.position.x, m.position.y, m.position.z])))),
            None => block.map(|t| (t, None)),
        }
    }
    fn entity(&mut self, id: u32) -> Option<[f64; 3]> {
        self.game
            .mobs
            .iter()
            .find(|m| m.id == id && m.alive() && !m.removed)
            .map(|m| [m.position.x, m.position.y, m.position.z])
    }
}

impl Game<'_> {
    /// The gear goes on or off (the server's `horizon:aot`, or `/aot` in a
    /// world of one's own).
    pub(super) fn set_odm(&mut self, on: bool) {
        if on {
            if self.odm.gear.is_none() {
                self.odm.gear = Some(Gear::new());
            }
        } else {
            self.odm.gear = None;
            self.odm.tips = Default::default();
            self.odm.aim = None;
            if let Some(p) = self.player.as_mut() {
                p.odm_glide = false;
            }
        }
    }

    /// `/aot` typed in a world of one's own.
    pub(super) fn local_odm_command(&mut self) {
        let on = self.odm.gear.is_none();
        self.set_odm(on);
        self.system_message(Text::literal(if on { HELP } else { OFF }));
    }

    /// Whether the gear takes this key press: its hooks hold on Q and E
    /// (Shift+E still opens the inventory).
    pub(super) fn odm_takes_key(&self, key: KeyCode) -> bool {
        self.odm.gear.is_some()
            && self.screen == Screen::Playing
            && (key == KeyCode::KeyQ
                || key == KeyCode::KeyE && !self.keys.contains(&KeyCode::ShiftLeft))
    }

    /// The gear's tick, before the player's move: its velocity, whether its
    /// flight takes the move, and no fall.
    pub(super) fn odm_step(&mut self) {
        self.odm.ticks += 1;
        let Some(mut gear) = self.odm.gear.take() else {
            if let Some(p) = self.player.as_mut() {
                p.odm_glide = false;
            }
            return;
        };
        let Some(p) = self.player.as_ref() else {
            self.odm.gear = Some(gear);
            return;
        };
        // Creative flight is its own: the hooks let go and the gear rests.
        let flying = p.flying;
        let playing = !p.health.dead_or_dying()
            && !flying
            && self.screen == Screen::Playing
            && !self.remote_chat_open()
            && self.sleeping.is_none()
            && self.riding.is_none();
        let held = |k| playing && self.keys.contains(&k);
        let controls = Controls {
            hooks: [
                held(KeyCode::KeyQ),
                held(KeyCode::KeyE) && !held(KeyCode::ShiftLeft),
            ],
            gas: held(KeyCode::Space),
            steer: horizon_client::input::keyboard_direction(
                held(KeyCode::KeyW),
                held(KeyCode::KeyS),
                held(KeyCode::KeyA),
                held(KeyCode::KeyD),
            ),
        };
        let feet = [p.position.x, p.position.y, p.position.z];
        let body = Body {
            feet,
            eye: [
                feet[0],
                feet[1] + f64::from(p.dimensions.eye_height),
                feet[2],
            ],
            yaw: self.yaw,
            pitch: self.pitch,
            velocity: [p.velocity.x, p.velocity.y, p.velocity.z],
            on_ground: p.ground.on_ground,
            in_fluid: p.fluids.was_touching_water || p.is_in_lava(),
        };
        let mut world = Surroundings { game: self };
        let step = gear.tick(&mut world, &body, &controls);
        let look = odm::view_vector(self.yaw, self.pitch);
        let aim = odm::World::cast(&mut world, body.eye, look, odm::RANGE).map(|hit| hit.0);
        let hips = odm::hips(feet, self.yaw);
        let tips = [odm::LEFT, odm::RIGHT].map(|side| gear.tip(side, hips[side], &mut world));
        let hooked = gear.attached(odm::LEFT) || gear.attached(odm::RIGHT);
        self.odm.tips = [self.odm.tips[1], tips];
        self.odm.aim = aim;
        self.odm.gas = step.gas;
        if let Some(p) = self.player.as_mut() {
            p.velocity = Vec3::new(step.velocity[0], step.velocity[1], step.velocity[2]);
            p.odm_glide = step.glide && !flying;
            p.fall_distance = 0.0;
        }
        self.odm.gear = Some(gear);
        self.odm_effects(&step, hips, feet, hooked);
    }

    /// The gear's sounds and gas.
    fn odm_effects(&mut self, step: &odm::Step, hips: [[f64; 3]; 2], feet: [f64; 3], hooked: bool) {
        let at = Some([feet[0], feet[1] + 1.0, feet[2]]);
        for event in &step.events {
            match *event {
                Event::Fire(side) => {
                    self.audio
                        .play("minecraft:item.crossbow.shoot", Some(hips[side]), 0.6, 1.6);
                }
                Event::Attach(..) => {
                    self.audio
                        .play("minecraft:item.trident.hit_ground", at, 0.7, 1.4);
                }
                Event::Release(_) => {
                    self.audio
                        .play("minecraft:entity.fishing_bobber.retrieve", at, 0.6, 1.5);
                }
                Event::Burst(dir) => {
                    self.audio
                        .play("minecraft:entity.wind_charge.wind_burst", at, 0.5, 1.4);
                    let behind = [
                        feet[0] - dir[0] * 0.5,
                        feet[1] + 0.9 - dir[1] * 0.5,
                        feet[2] - dir[2] * 0.5,
                    ];
                    for _ in 0..10 {
                        let spread = self.odm_spread(0.25);
                        let v = [
                            -dir[0] * 0.2 + spread[0],
                            -dir[1] * 0.2 + spread[1],
                            -dir[2] * 0.2 + spread[2],
                        ];
                        self.particles.add(
                            &ParticleOptions::simple(ParticleType::Cloud),
                            behind,
                            v,
                        );
                    }
                }
                Event::Empty => {
                    self.audio
                        .play("minecraft:block.dispenser.fail", at, 0.6, 1.8);
                }
            }
        }
        if step.gas {
            if self.odm.ticks.is_multiple_of(4) {
                self.audio
                    .play("minecraft:block.fire.extinguish", at, 0.15, 1.9);
            }
            self.odm_gas_puffs(hips, step.velocity);
        } else if hooked && self.odm.ticks.is_multiple_of(6) {
            self.audio
                .play("minecraft:item.crossbow.loading_middle", at, 0.2, 1.9);
        }
    }

    /// The gas streaming from the tanks at the hips, left behind.
    fn odm_gas_puffs(&mut self, hips: [[f64; 3]; 2], velocity: [f64; 3]) {
        for hip in hips {
            let spread = self.odm_spread(0.03);
            let v = [
                -velocity[0] * 0.2 + spread[0],
                -velocity[1] * 0.2 + spread[1],
                -velocity[2] * 0.2 + spread[2],
            ];
            self.particles
                .add(&ParticleOptions::simple(ParticleType::WhiteSmoke), hip, v);
        }
    }

    fn odm_spread(&mut self, amount: f64) -> [f64; 3] {
        let r = self.particles.random();
        [0; 3].map(|_| (r.next_double() - 0.5) * 2.0 * amount)
    }

    /// After the player's move: still no fall, and the cables for the
    /// others (on a server).
    pub(super) fn odm_after_step(&mut self) {
        let state = match self.odm.gear.as_ref() {
            Some(gear) => {
                if let Some(p) = self.player.as_mut() {
                    p.fall_distance = 0.0;
                }
                let tips = self.odm.tips[1];
                let mut flags = 0;
                for (side, out, holds) in [
                    (odm::LEFT, wire::LEFT_OUT, wire::LEFT_HOLDS),
                    (odm::RIGHT, wire::RIGHT_OUT, wire::RIGHT_HOLDS),
                ] {
                    if tips[side].is_some() {
                        flags |= out;
                    }
                    if gear.attached(side) {
                        flags |= holds;
                    }
                }
                if self.odm.gas {
                    flags |= wire::GAS;
                }
                wire::State { flags, tips }
            }
            None => wire::State::default(),
        };
        self.odm_send(state);
    }

    /// Sends the cables when they changed, and every half second while out.
    fn odm_send(&mut self, state: wire::State) {
        if self.remote.is_none() {
            return;
        }
        let mut data = vec![wire::STATE];
        state.encode(&mut data);
        let now = self.odm.ticks;
        let due = match &self.odm.sent {
            None => state.flags != 0,
            Some((last, at)) => *last != data || (state.flags != 0 && now - at >= 10),
        };
        if !due {
            return;
        }
        self.odm.sent = Some((data.clone(), now));
        self.send(S::CustomPayload(CustomPayload {
            channel: wire::CHANNEL.into(),
            data,
        }));
    }

    /// A `horizon:aot` payload from the server: the gear on or off, or
    /// another player's cables.
    pub(super) fn remote_odm_payload(&mut self, data: &[u8]) {
        match data.first() {
            Some(&wire::MODE) => self.set_odm(data.get(1).is_some_and(|&on| on != 0)),
            Some(&wire::STATE) if data.len() >= 5 => {
                let id = i32::from_be_bytes([data[1], data[2], data[3], data[4]]);
                let Some(state) = wire::State::decode(&data[5..]) else {
                    return;
                };
                let was = self.odm.others.get(&id).map_or(0, |o| o.state.flags);
                let at = self
                    .mobs
                    .iter()
                    .find(|m| m.id as i32 == id)
                    .map(|m| [m.position.x, m.position.y + 1.0, m.position.z]);
                if let Some(at) = at {
                    let out = wire::LEFT_OUT | wire::RIGHT_OUT;
                    let holds = wire::LEFT_HOLDS | wire::RIGHT_HOLDS;
                    if state.flags & !was & out != 0 {
                        self.audio
                            .play("minecraft:item.crossbow.shoot", Some(at), 0.6, 1.6);
                    }
                    if state.flags & !was & holds != 0 {
                        self.audio
                            .play("minecraft:item.trident.hit_ground", Some(at), 0.7, 1.4);
                    }
                    if was & !state.flags & out != 0 {
                        self.audio.play(
                            "minecraft:entity.fishing_bobber.retrieve",
                            Some(at),
                            0.6,
                            1.5,
                        );
                    }
                }
                if state.flags == 0 {
                    self.odm.others.remove(&id);
                } else {
                    self.odm.others.insert(id, Other { state, age: 0 });
                }
            }
            _ => {}
        }
    }

    /// The other players' gear each tick: their gas, and cables that stop
    /// coming (a player gone) are dropped.
    pub(super) fn tick_odm_others(&mut self) {
        if self.odm.others.is_empty() {
            return;
        }
        let mut puffs = Vec::new();
        let mobs = &self.mobs;
        self.odm.others.retain(|id, o| {
            o.age += 1;
            let Some(m) = mobs.iter().find(|m| m.id as i32 == *id) else {
                return false;
            };
            if o.state.flags & wire::GAS != 0 {
                let feet = [m.position.x, m.position.y, m.position.z];
                let v = [m.velocity.x, m.velocity.y, m.velocity.z];
                puffs.push((odm::hips(feet, m.body_yaw), v));
            }
            o.age < 40
        });
        for (hips, v) in puffs {
            self.odm_gas_puffs(hips, v);
        }
    }

    /// The cables, this player's and the others', to draw this frame.
    pub(super) fn odm_lines(&self, alpha: f64, size: [u32; 2]) -> Vec<Line> {
        let mut lines = Vec::new();
        if self.odm.gear.is_none() && self.odm.others.is_empty() {
            return lines;
        }
        let width = horizon_client::gfx::outline_width(size[0]) * 1.6;
        let a = alpha as f32;
        if self.odm.gear.is_some()
            && let Some(p) = self.player.as_ref()
        {
            let feet = self.previous_position.lerp(p.position, alpha);
            let starts = if self.camera_type.first_person() {
                // From below and beside the view, where the hips would be.
                let eye_height =
                    f64::from(horizon_client::camera::lerp(a, self.previous_eye, self.eye));
                let eye = [feet.x, feet.y + eye_height, feet.z];
                let f = odm::view_vector(self.yaw, self.pitch);
                let r = odm::right_vector(self.yaw);
                let u = cross(r, f);
                [-1.0, 1.0].map(|side| {
                    [0, 1, 2].map(|k| eye[k] + f[k] * 0.45 + r[k] * side * 0.3 - u[k] * 0.3)
                })
            } else {
                let body = self.player_model.as_ref().map_or(self.yaw, |m| {
                    horizon_client::camera::lerp(a, m.previous_body_yaw, m.body_yaw)
                });
                odm::hips([feet.x, feet.y, feet.z], body)
            };
            for side in [odm::LEFT, odm::RIGHT] {
                let tip = match self.odm.tips[0][side].zip(self.odm.tips[1][side]) {
                    Some((from, to)) => {
                        Some([0, 1, 2].map(|k| from[k] + (to[k] - from[k]) * alpha))
                    }
                    None => self.odm.tips[1][side],
                };
                if let Some(tip) = tip {
                    cable(&mut lines, starts[side], tip, width);
                }
            }
        }
        for (id, o) in &self.odm.others {
            let Some(m) = self.mobs.iter().find(|m| m.id as i32 == *id) else {
                continue;
            };
            let feet = m.previous.lerp(m.position, alpha);
            let body = horizon_client::camera::lerp(a, m.previous_body_yaw, m.body_yaw);
            let hips = odm::hips([feet.x, feet.y, feet.z], body);
            for side in [odm::LEFT, odm::RIGHT] {
                if let Some(tip) = o.state.tips[side] {
                    cable(&mut lines, hips[side], tip, width);
                }
            }
        }
        lines
    }

    /// By the crosshair: how far the hook would fly (a red dash when nothing
    /// is in reach), and a dot each side for a hook out (green while it
    /// holds). The gas gauge sits in the top right corner, below any effect
    /// icons.
    pub(super) fn draw_odm_hud(&self, batch: &mut HudBatch, w: f32, h: f32) {
        let Some(gear) = self.odm.gear.as_ref() else {
            return;
        };
        if self.screen != Screen::Playing {
            return;
        }
        let atlas = &self.atlas;
        let (cx, cy) = ((w / 2.0).floor(), (h / 2.0).floor());
        let (label, color) = match self.odm.aim {
            Some(d) => (format!("{}", d.round() as i32), 0xFFFF_FFFF),
            None => ("-".to_owned(), 0xFFFF_5555),
        };
        let tw = HudBatch::text_width(atlas, &label);
        batch.text(
            atlas,
            &label,
            cx - (tw / 2.0).floor(),
            cy + 9.0,
            color,
            true,
        );
        for side in [odm::LEFT, odm::RIGHT] {
            let color = match gear.hooks[side] {
                Hook::Attached { .. } => 0xFF55_FF55,
                Hook::Flying { .. } | Hook::Missed { .. } => 0xFFFF_FFFF,
                _ => continue,
            };
            let x = if side == odm::LEFT {
                cx - 12.0
            } else {
                cx + 10.0
            };
            batch.fill(atlas, x, cy - 1.0, 2.0, 2.0, color);
        }
        // Hud.extractEffects' rows: the helpful icons at the top, the
        // harmful ones below them.
        let icons: Vec<horizon_client::hud_overlays::EffectIcon> = self
            .potions
            .effects
            .list
            .iter()
            .map(horizon_client::hud_overlays::EffectIcon::from)
            .filter(|i| i.show_icon)
            .collect();
        let rows = if icons.iter().any(|i| !i.beneficial) {
            2.0
        } else if icons.is_empty() {
            0.0
        } else {
            1.0
        };
        let width = 50.0;
        let (x, y) = (w - width - 4.0, 5.0 + rows * 26.0);
        let label = "Gas";
        let lw = HudBatch::text_width(atlas, label);
        batch.text(atlas, label, x - lw - 4.0, y - 1.0, 0xFFFF_FFFF, true);
        batch.fill(atlas, x - 1.0, y, width + 2.0, 6.0, 0x9000_0000);
        let fill = if gear.gas < 0.2 {
            0xFFFF_5555
        } else {
            0xFFDD_EEFF
        };
        batch.fill(
            atlas,
            x,
            y + 1.0,
            (width * gear.gas as f32).round(),
            4.0,
            fill,
        );
    }
}

/// A taut cable from `from` to `tip`, and the hook's head crossed at its end.
fn cable(lines: &mut Vec<Line>, from: [f64; 3], tip: [f64; 3], width: f32) {
    lines.push(Line {
        start: from,
        end: tip,
        color: CABLE,
        width,
    });
    let d = [tip[0] - from[0], tip[1] - from[1], tip[2] - from[2]];
    let length = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    if length < 0.5 {
        return;
    }
    let d = d.map(|v| v / length);
    let side = normalized(cross(d, [0.0, 1.0, 0.0])).unwrap_or([1.0, 0.0, 0.0]);
    let up = cross(side, d);
    for axis in [side, up] {
        lines.push(Line {
            start: [0, 1, 2].map(|k| tip[k] - axis[k] * 0.18 - d[k] * 0.1),
            end: [0, 1, 2].map(|k| tip[k] + axis[k] * 0.18 - d[k] * 0.1),
            color: HEAD,
            width,
        });
    }
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalized(a: [f64; 3]) -> Option<[f64; 3]> {
    let l = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt();
    (l > 1.0e-6).then(|| a.map(|v| v / l))
}
