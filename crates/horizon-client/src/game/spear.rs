//! Spears (26.3): `PiercingWeapon`, the stab a full-charge attack makes along
//! the look, and `KineticWeapon`, the charge while the use is held, whose
//! damage follows how fast the holder closes in. The acting player is the
//! local one or a guest under `as_guest`; the server decides every hit.
use super::Game;
use horizon_client::collisions::SceneCollisions;
use horizon_client::{
    first_person::{Hand, SwingAnimation},
    items::{AttackRange, ItemInfo, Kinetic, KineticCondition},
    mobs::Damage,
};
use horizon_core::{
    geometry::{Aabb, Vec3, traverse_blocks},
    position::BlockPos,
};

/// `AttackRange.defaultFor` a player: no minimum, the interaction range.
const DEFAULT_RANGE: AttackRange = AttackRange {
    min_reach: 0.0,
    max_reach: 3.0,
    min_creative_reach: 0.0,
    max_creative_reach: 5.0,
    hitbox_margin: 0.0,
};

/// Where a stab or a charge reaches: from the eye along the look, between
/// `start` and `end` blocks (ProjectileUtil.getHitEntitiesAlong).
#[derive(Clone, Copy, Debug)]
pub(super) struct Reach {
    pub eye: [f64; 3],
    pub look: [f64; 3],
    pub start: f64,
    pub end: f64,
    pub margin: f64,
}
impl Reach {
    /// Whether the reach passes through `bounds`, grown by the hitbox margin.
    pub fn hits(&self, bounds: Aabb) -> bool {
        let (min, max) = (bounds.min_position(), bounds.max_position());
        let m = self.margin;
        let grown = Aabb::new(
            Vec3::new(min.x - m, min.y - m, min.z - m),
            Vec3::new(max.x + m, max.y + m, max.z + m),
        );
        let from = [0, 1, 2].map(|a| self.eye[a] + self.look[a] * self.start);
        super::ray_box(from, self.look, (self.end - self.start).max(0.0), grown).is_some()
    }
}

/// A stab that landed: what the caller needs to strike other players too.
/// What a spear's hit does to its target (`stabAttack`'s flags).
#[derive(Clone, Copy, Debug)]
pub(super) struct Effects {
    pub damage: bool,
    pub knockback: bool,
    pub dismount: bool,
}
impl Effects {
    /// The charge's conditions met `ticks` into it, at the holder's speed
    /// and how fast they close in (blocks a second); None when none is.
    pub fn of_charge(kinetic: &Kinetic, ticks: u32, attacker: f64, relative: f64) -> Option<Self> {
        let test =
            |c: &Option<KineticCondition>| c.is_some_and(|c| c.test(ticks, attacker, relative));
        let effects = Self {
            damage: test(&kinetic.damage),
            knockback: test(&kinetic.knockback),
            dismount: test(&kinetic.dismount),
        };
        (effects.damage || effects.knockback || effects.dismount).then_some(effects)
    }
}
/// A guest's charge this tick, for the players along it.
#[derive(Clone, Debug)]
pub(super) struct Charge {
    pub reach: Reach,
    pub ticks: u32,
    /// The holder's speed along the look, blocks a second.
    pub attacker: f64,
    pub kinetic: Kinetic,
}
pub(super) struct Stab {
    /// None when a block stands nearer than the minimum reach.
    pub reach: Option<Reach>,
    /// The damage against a player, at the attack strength before the stab.
    pub player_damage: f32,
    pub scale: f32,
}

impl Game<'_> {
    fn held_info(&self) -> Option<ItemInfo> {
        let held = self.held()?;
        self.res.items.get(&held.item).cloned()
    }
    /// Whether the held item stabs instead of hitting (`piercing_weapon`).
    pub(super) fn holds_piercing_weapon(&self) -> bool {
        self.held_info().is_some_and(|i| i.piercing.is_some())
    }
    /// The held item's `attack_animation` (SwingAnimation.DEFAULT without one).
    pub(super) fn held_attack_animation(&self) -> SwingAnimation {
        self.held_info().and_then(|i| i.attack_animation).map_or(
            SwingAnimation::DEFAULT,
            |(kind, duration)| SwingAnimation { kind, duration },
        )
    }
    /// `Player.cannotAttackWithItem`: the item needs more charge
    /// (`minimum_attack_charge`) than the ticks since the last attack give,
    /// `tolerance` ticks counted in its favour (5 on the server).
    pub(super) fn cannot_attack_with_held(&self, tolerance: u32) -> bool {
        let Some(info) = self.held_info() else {
            return false;
        };
        if info.minimum_attack_charge <= 0.0 {
            return false;
        }
        let delay = 20.0 / (4.0 + info.attack_speed).max(0.1);
        ((self.attack_ticks + tolerance) as f32 / delay) < info.minimum_attack_charge
    }
    /// The acting player's movement in the last tick: a guest's from their
    /// moves (`spear_movement`), the local player's own.
    fn acting_movement(&self) -> [f64; 3] {
        self.spear_movement.unwrap_or_else(|| {
            self.player
                .as_ref()
                .map_or([0.0; 3], |p| [p.velocity.x, p.velocity.y, p.velocity.z])
        })
    }
    /// ProjectileUtil.getHitEntitiesAlong's segment for the held item's
    /// `attack_range`: from the minimum reach to the maximum (plus the
    /// movement along the look), cut short by the first block in the way.
    /// None when a block stands nearer than the minimum reach.
    pub(super) fn spear_reach(&self) -> Option<Reach> {
        let info = self.held_info()?;
        let range = info.attack_range.unwrap_or(DEFAULT_RANGE);
        let eye = self.eye_position()?;
        let look = super::view_vector(self.yaw, self.pitch);
        let (min, max) = if self.creative {
            (range.min_creative_reach, range.max_creative_reach)
        } else {
            (range.min_reach, range.max_reach)
        };
        let movement = self.acting_movement();
        let along = (0..3).map(|a| movement[a] * look[a]).sum::<f64>().max(0.0);
        let mut end = f64::from(max) + along;
        // ClipContext.Block.COLLIDER: grass and flowers do not stop it.
        let to = [0, 1, 2].map(|a| eye[a] + look[a] * end);
        if let Some(d) = self.collider_distance(eye, to) {
            if d < f64::from(min) {
                return None;
            }
            end = end.min(d);
        }
        Some(Reach {
            eye,
            look,
            start: f64::from(min),
            end,
            margin: f64::from(range.hitbox_margin),
        })
    }
    /// How far from `from` towards `to` the first block's collision shape
    /// lies (`ClipContext.Block.COLLIDER`, no fluids).
    fn collider_distance(&self, from: [f64; 3], to: [f64; 3]) -> Option<f64> {
        let (from, to) = (
            Vec3::new(from[0], from[1], from[2]),
            Vec3::new(to[0], to[1], to[2]),
        );
        let mut scene = SceneCollisions {
            world: &self.world,
            binding: &self.collision,
            subject: None,
        };
        let hit = traverse_blocks(from, to, 64, |p: BlockPos| {
            let state = self.world.block(p.x, p.y, p.z);
            if self.res.facts.is_air(state) {
                return None;
            }
            let shape = self
                .collision
                .collision_shape(&mut scene, state, [p.x, p.y, p.z])
                .ok()?;
            shape.clip(from, to, p)
        });
        let hit = hit.ok().flatten()?;
        Some(hit.location.distance(from))
    }
    /// The living mobs a reach passes through (PiercingWeapon.canHitEntity).
    fn spear_mobs(&self, reach: &Reach) -> Vec<usize> {
        self.mobs
            .iter()
            .enumerate()
            .filter(|(_, m)| m.alive() && !m.removed && reach.hits(m.bounds()))
            .map(|(i, _)| i)
            .collect()
    }
    /// `Player.stabAttack` on the mob at `index`: its hurt (with the hit's
    /// knockback) when the stab deals damage, the extra knockback along the
    /// holder's facing, the dismount, then the weapon's wear and
    /// enchantments, the statistics and the exhaustion. False when nothing
    /// landed.
    fn spear_hit_mob(&mut self, index: usize, amount: f32, effects: Effects) -> bool {
        let Some(from) = self
            .player
            .as_ref()
            .map(|p| [p.position.x, p.position.y, p.position.z])
        else {
            return false;
        };
        let old_health = self.mobs[index].health;
        let hurt = effects.damage && self.hurt_by_player(index, amount, Damage::player(from));
        if effects.knockback {
            // causeExtraKnockback(target, 0.4): pushed the way the holder faces,
            // and the holder slows. The knockback enchantment's share comes
            // with the weapon's post-attack effects below.
            let yaw = f64::from(self.yaw.to_radians());
            self.mobs[index].knockback(0.4, yaw.sin(), -yaw.cos());
            if let Some(p) = self.player.as_mut() {
                p.velocity.x *= 0.6;
                p.velocity.z *= 0.6;
            }
        }
        let dismounted = effects.dismount && self.mobs[index].extra.mount.is_some();
        if dismounted {
            let m = &mut self.mobs[index];
            m.extra.mount = None;
            m.extra.passenger = false;
        }
        if !hurt && !effects.knockback && !dismounted {
            return false;
        }
        let actual = old_health - self.mobs[index].health;
        self.stats
            .custom("damage_dealt", (actual * 10.0).round() as i32);
        self.combat.last_hurt_mob = Some((self.mobs[index].id, self.world.game_time));
        self.last_hurt_by_player.insert(self.mobs[index].id);
        let queued = std::mem::take(&mut self.mobs[index].outbox);
        for event in queued {
            if let horizon_client::mobs::MobEvent::Sound(s, at, volume, pitch) = event {
                self.audio
                    .play(&format!("minecraft:{s}"), Some(at), volume, pitch);
            }
        }
        if hurt {
            self.weapon_hit(index);
        }
        if let Some(p) = self.player.as_mut()
            && !self.creative
        {
            p.health.food.exhaustion = (p.health.food.exhaustion + 0.1).min(40.0);
        }
        true
    }
    /// The stab's damage against `target` (`PiercingWeapon.attack` with
    /// `stabAttack` out of a use): the attack damage and enchantments scaled
    /// by the attack strength, never a critical hit.
    fn stab_damage(&mut self, target: &str, scale: f32) -> f32 {
        let damage =
            1.0 + self.held_info().map_or(0.0, |i| i.attack_damage) + self.effect_attack_bonus();
        damage * (0.2 + scale * scale * 0.8) + self.enchant_damage_bonus(target) * scale
    }
    /// `PiercingWeapon.attack` for the acting player: every mob along the
    /// reach is stabbed, the sounds play, and the attack strength restarts.
    /// None without a spear in hand.
    pub(super) fn spear_stab(&mut self) -> Option<Stab> {
        let info = self.held_info()?;
        let piercing = info.piercing.clone()?;
        let scale = self.attack_strength_scale();
        let player_damage = self.stab_damage("minecraft:player", scale);
        let reach = self.spear_reach();
        let mut hit = false;
        if let Some(reach) = reach {
            for index in self.spear_mobs(&reach) {
                let kind = format!(
                    "minecraft:{}",
                    self.mobs[index].kind.id.trim_start_matches("minecraft:")
                );
                let amount = self.stab_damage(&kind, scale);
                let effects = Effects {
                    damage: true,
                    knockback: piercing.deals_knockback,
                    dismount: piercing.dismounts,
                };
                hit |= self.spear_hit_mob(index, amount, effects);
            }
        }
        let at = self
            .player
            .as_ref()
            .map(|p| [p.position.x, p.position.y, p.position.z]);
        if hit && let Some(sound) = &piercing.hit_sound {
            self.audio.play(sound, at, 1.0, 1.0);
        }
        if let Some(sound) = &piercing.sound {
            self.audio.play(sound, at, 1.0, 1.0);
        }
        // swingAndResetAttackStrength with the item's stab.
        let animation = self.held_attack_animation();
        self.swing_hand(Hand::Main, animation);
        self.reset_attack_strength();
        Some(Stab {
            reach,
            player_damage,
            scale,
        })
    }
    /// `KineticWeapon.damageEntities`, each tick the acting player holds a
    /// kinetic weapon's use (`ticks_used` since it began): after its delay,
    /// every mob along the reach not struck in the last `contact_cooldown`
    /// ticks takes the charge when the holder closes in fast enough. Its
    /// damage is the base attack damage plus the relative speed (blocks a
    /// second) times the weapon's multiplier.
    pub(super) fn spear_charge_tick(&mut self, ticks_used: u32) {
        let Some(kinetic) = self.held_info().and_then(|i| i.kinetic) else {
            return;
        };
        if ticks_used < kinetic.delay_ticks {
            return;
        }
        let ticks = ticks_used - kinetic.delay_ticks;
        let Some(reach) = self.spear_reach() else {
            return;
        };
        let look = reach.look;
        let movement = self.acting_movement();
        let dot = |v: [f64; 3]| (0..3).map(|a| v[a] * look[a]).sum::<f64>();
        // KineticWeapon.getMotion: the known speed in blocks a second.
        let attacker = dot(movement) * 20.0;
        let who = self.lan_acting_uuid.unwrap_or(0);
        let now = self.world.game_time;
        self.spear_contacts.retain(|_, at| now - *at < 400);
        let mut hit = false;
        for index in self.spear_mobs(&reach) {
            let id = self.mobs[index].id;
            if self
                .spear_contacts
                .get(&(who, id))
                .is_some_and(|at| now - *at < kinetic.contact_cooldown_ticks as i64)
            {
                continue;
            }
            self.spear_contacts.insert((who, id), now);
            let m = &self.mobs[index];
            let target = dot([
                m.position.x - m.previous.x,
                m.position.y - m.previous.y,
                m.position.z - m.previous.z,
            ]) * 20.0;
            let relative = (attacker - target).max(0.0);
            let Some(effects) = Effects::of_charge(&kinetic, ticks, attacker, relative) else {
                continue;
            };
            let kind = format!("minecraft:{}", m.kind.id.trim_start_matches("minecraft:"));
            // stabAttack while using: no attack-strength scaling.
            let amount = kinetic.charge_damage(relative) + self.enchant_damage_bonus(&kind);
            hit |= self.spear_hit_mob(index, amount, effects);
        }
        if hit && let Some(sound) = &kinetic.hit_sound {
            let at = self
                .player
                .as_ref()
                .map(|p| [p.position.x, p.position.y, p.position.z]);
            self.audio.play(sound, at, 1.0, 1.0);
        }
        // Other players along it are the host's to strike (guest_charge).
        if self.lan_acting.is_some() {
            self.spear_charged = Some(Charge {
                reach,
                ticks,
                attacker,
                kinetic,
            });
        }
    }
    /// `Item.use` of a kinetic weapon: the use begins, with its sound.
    pub(super) fn start_spear_charge(&mut self, slot: usize) -> bool {
        let Some(kinetic) = self.held_info().and_then(|i| i.kinetic) else {
            return false;
        };
        if self.using.is_none() {
            self.using = Some((slot, 0));
            if let Some(sound) = &kinetic.sound {
                let at = self
                    .player
                    .as_ref()
                    .map(|p| [p.position.x, p.position.y, p.position.z]);
                self.audio.play(sound, at, 1.0, 1.0);
            }
        }
        true
    }
}
