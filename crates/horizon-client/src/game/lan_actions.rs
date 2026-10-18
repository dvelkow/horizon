//! What guests do in a hosted world. The host's gameplay code is written for
//! the one local player, so a guest's action runs through it with the guest's
//! state swapped in for the duration of the call (position, inventory, game
//! mode, experience, mining and item use): digging, placing, using items and
//! blocks, attacking and the commands all behave exactly as they do for the
//! host, and every world change they cause reaches all players through the
//! block log, the level events and the entity tracker.
use super::{Game, Hit, Mining, Screen, lan::LanHost};
use horizon_client::{
    command::vanilla,
    containers::BlockEntity,
    entities::Experience,
    items::ItemStack,
    lan::{
        guest::SPECTATOR,
        hub::TickCtx,
        menu::{Rules, Slots},
    },
    motion::{MotionEvent, MotionWorld, PlayerMotion},
};
use horizon_core::geometry::Vec3;
#[cfg(target_arch = "wasm32")]
use horizon_platform as std;
use horizon_protocol::{
    entity_data::{Value, serializer},
    packets::play::{self, Clientbound as C, Serverbound as S},
    text::literal,
};
use horizon_world::entity_move::EntityMoveWorld;
use winit::keyboard::KeyCode;

/// Per-guest state of the gameplay code that lives outside `Guest`.
#[derive(Default)]
pub(super) struct GuestPlay {
    mining: Option<Mining>,
    destroy_delay: u32,
    using: Option<(usize, u32)>,
    use_delay: u32,
    attack_ticks: u32,
    swing: horizon_client::first_person::SwingState,
    /// The block being dug and the face the client aimed at.
    digging: Option<([i32; 3], [i32; 3])>,
    /// The server tick the digging started (ServerPlayerGameMode.destroyProgressStart).
    dig_start: u64,
    /// A block the client stopped digging before the server's time was up: it breaks once
    /// its progress since `dig_start` is complete (ServerPlayerGameMode.hasDelayedDestroy).
    delayed: Option<([i32; 3], u64)>,
    last_progress: i32,
    /// Player.takeXpDelay: ticks before the next experience orb counts.
    pub(super) xp_delay: u32,
    /// A use that ended on its own last tick (eaten, drunk): the client hears
    /// of it (EntityEvent.USE_ITEM_COMPLETE) after its new food and health.
    use_finished: bool,
    /// Hand movements the client announced and the host animated.
    swung: bool,
    /// The trading screen the guest has open.
    pub(super) merchant: Option<super::lan_trading::GuestMerchant>,
    /// The inventory menu and the crafting table the guest crafts in.
    pub(super) crafting: super::lan_crafting::GuestCrafting,
    /// The workstation or brewing stand the guest has open.
    pub(super) stations: super::lan_stations::GuestStations,
    /// What the guest's body was given for its effects and enchantments.
    potions: super::potions::BodyPotions,
    /// `ServerPlayer.getStats` and the inventory `inventory_changed` last saw.
    pub(super) stats: super::stats::Stats,
    /// The guest's CombatTracker: what hurt them, for their death message.
    combat: super::combat::CombatTracker,
    last_inventory: u64,
}

struct ItemRules<'a>(&'a horizon_client::items::Items);
impl Rules for ItemRules<'_> {
    fn max_stack(&self, item: &str) -> u32 {
        self.0.get(item).map_or(64, |i| i.max_stack)
    }
    fn armor_index(&self, item: &str) -> Option<usize> {
        match self.0.get(item)?.equip_slot.as_deref()? {
            "head" => Some(0),
            "chest" => Some(1),
            "legs" => Some(2),
            "feet" => Some(3),
            _ => None,
        }
    }
}

fn direction(face: i32) -> [i32; 3] {
    match face {
        0 => [0, -1, 0],
        1 => [0, 1, 0],
        2 => [0, 0, -1],
        3 => [0, 0, 1],
        4 => [-1, 0, 0],
        _ => [1, 0, 0],
    }
}

impl Game<'_> {
    /// Run `f` as the guest `conn`: the host's player state is swapped for theirs.
    pub(super) fn as_guest<R>(
        &mut self,
        host: &mut LanHost,
        conn: u32,
        f: impl FnOnce(&mut Self) -> R,
    ) -> Option<R> {
        let i = host.hub.guests.iter().position(|g| g.conn == conn)?;
        if host.hub.guests[i].motion.is_none() {
            let p = Vec3::new(
                host.hub.guests[i].position[0],
                host.hub.guests[i].position[1],
                host.hub.guests[i].position[2],
            );
            host.hub.guests[i].motion = PlayerMotion::new(self.motion, p).ok().map(Box::new);
        }
        let mut play = host.play.remove(&conn).unwrap_or_default();
        let g = &mut host.hub.guests[i];
        // The guest's body.
        let Some(mut motion) = g.motion.take() else {
            host.play.insert(conn, play);
            return None;
        };
        self.potions.swap_player(&mut g.effects, &mut play.potions);
        motion.position = Vec3::new(g.position[0], g.position[1], g.position[2]);
        motion.ground.on_ground = g.on_ground;
        motion.yaw = g.yaw;
        motion.pitch = g.pitch;
        motion.sprinting = g.sprinting;
        motion.may_fly = g.creative() || g.game_mode == SPECTATOR;
        motion.invulnerable = motion.may_fly;
        let _ = motion.set_health(g.health, false);
        motion.health.food.food = g.food;
        motion.health.food.saturation = g.saturation;
        let mut player = Some(*motion);
        std::mem::swap(&mut self.player, &mut player);
        let eye = (g.eye_height() as f32, self.eye);
        self.eye = eye.0;
        let (yaw, pitch) = (self.yaw, self.pitch);
        self.yaw = g.yaw;
        self.pitch = g.pitch;
        std::mem::swap(&mut self.inventory, &mut g.inventory);
        std::mem::swap(&mut self.armor, &mut g.armor);
        std::mem::swap(&mut self.offhand, &mut g.offhand);
        std::mem::swap(&mut self.carried, &mut g.carried);
        let selected = std::mem::replace(&mut self.selected, g.selected);
        let (mode, creative) = (self.mode, self.creative);
        self.set_mode_raw(g.game_mode);
        let xp = std::mem::replace(
            &mut self.experience,
            Experience {
                level: g.xp_level,
                progress: g.xp_progress,
                total: g.xp_total,
            },
        );
        let sneak = self.keys.contains(&KeyCode::ShiftLeft);
        if g.sneaking {
            self.keys.insert(KeyCode::ShiftLeft);
        } else {
            self.keys.remove(&KeyCode::ShiftLeft);
        }
        let screen = std::mem::replace(&mut self.screen, Screen::Playing);
        // The host's crafting grid stays theirs; a crafting table the guest
        // opens shows as the 3x3 size.
        let craft_size = std::mem::replace(&mut self.craft_size, 2);
        let craft_grid = std::mem::replace(&mut self.craft_grid, vec![None; 9]);
        let container = self.container.take();
        let mining = std::mem::replace(&mut self.mining, play.mining.take());
        let destroy_delay = std::mem::replace(&mut self.destroy_delay, play.destroy_delay);
        let using = std::mem::replace(&mut self.using, play.using.take());
        let use_delay = std::mem::replace(&mut self.use_delay, play.use_delay);
        let attack_ticks = std::mem::replace(&mut self.attack_ticks, play.attack_ticks);
        let swing = std::mem::replace(&mut self.swing, play.swing);
        let (left, right) = (self.left, self.right);
        self.left = play.digging.is_some();
        self.right = play.using.is_some() || self.using.is_some();
        let riding = self.riding.take();
        let host_station = self.station.take();
        // The guest's own ender inventory; one the host has open goes back
        // into the host's while the guest acts.
        let host_ender = self.ender_open;
        self.close_ender_chest();
        std::mem::swap(&mut self.ender_items, &mut g.ender_items);
        // The guest's advancements, recipe book and statistics.
        self.advancement_tree();
        std::mem::swap(&mut self.adv.progress, &mut g.advancements);
        std::mem::swap(&mut self.adv.changed, &mut g.advancements_changed);
        std::mem::swap(&mut self.adv.unlocked, &mut g.recipes_unlocked);
        std::mem::swap(&mut self.level.recipe_book, &mut g.recipe_book);
        std::mem::swap(&mut self.stats, &mut play.stats);
        std::mem::swap(&mut self.combat, &mut play.combat);
        let adv_loaded = std::mem::replace(&mut self.adv.loaded, true);
        let last_inventory = std::mem::replace(&mut self.adv.last_inventory, play.last_inventory);
        self.lan_acting = Some(conn);
        self.lan_acting_name = Some(g.name.clone());
        self.lan_acting_uuid = Some(g.uuid);
        // A totem in either of the guest's hands is ready for what hurts them.
        self.update_totem_ready();
        // A bed the guest uses is their own respawn point, not the world's.
        let world_spawn = (self.spawn, self.level.spawn, self.respawn_config.take());
        // Sleeping and the status line are the guest's while they act.
        let host_sleep = (self.sleeping.take(), self.message.take());

        let result = f(self);
        let slept = std::mem::replace(&mut self.sleeping, host_sleep.0);
        if let Some((text, _)) = std::mem::replace(&mut self.message, host_sleep.1) {
            // Player.displayClientMessage(…, true): the action bar.
            host.hub.send_to(
                conn,
                &C::SystemChat(play::SystemChat {
                    content: horizon_protocol::text::literal(&text),
                    overlay: true,
                }),
            );
        }
        if let Some((head, _)) = slept {
            self.guest_start_sleeping(host, i, head);
        }
        if self.spawn != world_spawn.0 {
            if let Some(bed) = self.spawn {
                let head = [
                    bed.x.floor() as i32,
                    bed.y.floor() as i32,
                    bed.z.floor() as i32,
                ];
                host.hub.guests[i].respawn_bed = Some(head);
            }
        }
        (self.spawn, self.level.spawn, self.respawn_config) = world_spawn;
        // LivingEntity.checkTotemDeathProtection: a totem saved the guest. It
        // is used up and everyone sees it (EntityEvent.PROTECTED_FROM_DEATH).
        if self.player.as_ref().is_some_and(|p| p.health.totem_used) {
            self.apply_totem();
            let g = &host.hub.guests[i];
            host.hub.entity_event(g.entity_id, 35, g.position);
        }
        self.lan_acting = None;
        self.lan_acting_name = None;
        self.lan_acting_uuid = None;
        // An ender chest the guest opened: the menu shows their inventory
        // itself (lan_open_container), not a stand-in block entity.
        self.close_ender_chest();

        // What the action opened: handed to the guest instead of the host.
        let opened = self.container.take();
        let opened_table = std::mem::replace(&mut self.craft_size, craft_size) == 3;
        self.craft_grid = craft_grid;
        let opened_station = std::mem::replace(&mut self.station, host_station);
        self.riding = riding;
        self.left = left;
        self.right = right;
        play.mining = std::mem::replace(&mut self.mining, mining);
        play.destroy_delay = std::mem::replace(&mut self.destroy_delay, destroy_delay);
        play.using = std::mem::replace(&mut self.using, using);
        play.use_delay = std::mem::replace(&mut self.use_delay, use_delay);
        play.attack_ticks = std::mem::replace(&mut self.attack_ticks, attack_ticks);
        play.swing = std::mem::replace(&mut self.swing, swing);
        self.container = container;
        self.screen = screen;
        if sneak {
            self.keys.insert(KeyCode::ShiftLeft);
        } else {
            self.keys.remove(&KeyCode::ShiftLeft);
        }
        let g = &mut host.hub.guests[i];
        std::mem::swap(&mut self.ender_items, &mut g.ender_items);
        std::mem::swap(&mut self.adv.progress, &mut g.advancements);
        std::mem::swap(&mut self.adv.changed, &mut g.advancements_changed);
        std::mem::swap(&mut self.adv.unlocked, &mut g.recipes_unlocked);
        std::mem::swap(&mut self.level.recipe_book, &mut g.recipe_book);
        std::mem::swap(&mut self.stats, &mut play.stats);
        std::mem::swap(&mut self.combat, &mut play.combat);
        self.adv.loaded = adv_loaded;
        play.last_inventory = std::mem::replace(&mut self.adv.last_inventory, last_inventory);
        let stat_events = std::mem::take(&mut play.stats.events);
        let name = g.name.clone();
        self.potions.swap_player(&mut g.effects, &mut play.potions);
        if let Some(key) = host_ender {
            self.world
                .block_entities
                .insert(key, BlockEntity::Chest(self.ender_items.clone()));
            self.ender_open = Some(key);
        }
        let new_xp = std::mem::replace(&mut self.experience, xp);
        g.xp_level = new_xp.level;
        g.xp_progress = new_xp.progress;
        g.xp_total = new_xp.total;
        let new_mode = self.mode_id();
        self.mode = mode;
        self.creative = creative;
        self.selected = std::mem::replace(&mut g.selected, selected);
        // `g.selected` now holds the host's value: swap the inventories back.
        std::mem::swap(&mut self.selected, &mut g.selected);
        std::mem::swap(&mut self.inventory, &mut g.inventory);
        std::mem::swap(&mut self.armor, &mut g.armor);
        std::mem::swap(&mut self.offhand, &mut g.offhand);
        std::mem::swap(&mut self.carried, &mut g.carried);
        self.yaw = yaw;
        self.pitch = pitch;
        self.eye = eye.1;
        std::mem::swap(&mut self.player, &mut player);
        if let Some(mut m) = player {
            g.health = m.health.health();
            g.food = m.health.food.food;
            g.saturation = m.health.food.saturation;
            // A teleport (commands) moves the guest.
            let at = [m.position.x, m.position.y, m.position.z];
            m.sprinting = false;
            let moved: f64 = (0..3).map(|k| (at[k] - g.position[k]).powi(2)).sum();
            g.motion = Some(Box::new(m));
            if moved > 0.0001 {
                host.hub.teleport(i, Some(at));
            }
        }
        let g = &mut host.hub.guests[i];
        if new_mode != g.game_mode {
            let conn = g.conn;
            host.hub.set_game_mode(conn, new_mode);
        }
        host.play.insert(conn, play);
        self.stat_criteria_changed(&name, stat_events);
        if let Some(pos) = opened
            && !self.lan_open_container(host, conn, pos)
            && !host.hub.guests[i].spectator()
        {
            // Nothing the guest can use opened: its lid closes again.
            self.container_openers_changed(pos, false);
        }
        if let Some(station) = opened_station {
            self.lan_station_opened(host, conn, station);
        }
        if opened_table {
            self.lan_open_crafting(host, conn);
        }
        Some(result)
    }

    /// The game mode fields without `set_mode`'s side effects on the host.
    fn set_mode_raw(&mut self, id: i32) {
        use horizon_client::command::args::GameMode;
        self.mode = match id {
            1 => GameMode::Creative,
            2 => GameMode::Adventure,
            3 => GameMode::Spectator,
            _ => GameMode::Survival,
        };
        self.creative = self.mode == GameMode::Creative;
    }

    /// Hand a container the guest opened over (ServerPlayer.openMenu);
    /// false when it is not one a guest can open.
    fn lan_open_container(&mut self, host: &mut LanHost, conn: u32, pos: (i32, i32, i32)) -> bool {
        let name = &self
            .res
            .facts
            .get(self.world.block(pos.0, pos.1, pos.2))
            .name;
        let (menu, title, slots) = if &**name == "minecraft:ender_chest" {
            // EnderChestBlock.useWithoutItem: the player's own inventory.
            (
                "minecraft:generic_9x3",
                "container.enderchest",
                Slots::Ender,
            )
        } else if let Some(BlockEntity::Furnace(f)) = self.world.block_entities.get(&pos) {
            // AbstractFurnaceBlock.openContainer: FurnaceMenu, BlastFurnaceMenu, SmokerMenu.
            let (menu, title) = match f.kind.recipe_type() {
                "minecraft:blasting" => ("minecraft:blast_furnace", "container.blast_furnace"),
                "minecraft:smoking" => ("minecraft:smoker", "container.smoker"),
                _ => ("minecraft:furnace", "container.furnace"),
            };
            (menu, title, Slots::Block(pos))
        } else if let Some(BlockEntity::Brewing(_)) = self.world.block_entities.get(&pos) {
            // BrewingStandBlock.openContainer: BrewingStandMenu (game/lan_stations.rs).
            (
                "minecraft:brewing_stand",
                "container.brewing",
                Slots::Block(pos),
            )
        } else if let Some((other, right)) = self.chest_partner(pos) {
            // ChestBlock.combine: the right half's slots come first.
            let halves = if right {
                Slots::Double(pos, other)
            } else {
                Slots::Double(other, pos)
            };
            ("minecraft:generic_9x6", "container.chestDouble", halves)
        } else {
            let Some(BlockEntity::Chest(items)) = self.world.block_entities.get(&pos) else {
                return false;
            };
            let (menu, title) = match items.len() {
                27 => ("minecraft:generic_9x3", "container.chest"),
                9 => ("minecraft:generic_3x3", "container.dispenser"),
                5 => ("minecraft:hopper", "container.hopper"),
                _ => return false,
            };
            let title = match &**name {
                "minecraft:barrel" => "container.barrel",
                "minecraft:dropper" => "container.dropper",
                n if n.ends_with("shulker_box") => "container.shulkerBox",
                _ => title,
            };
            (menu, title, Slots::Block(pos))
        };
        let Some(items) = host
            .hub
            .guest(conn)
            .and_then(|g| slots.read(&self.world, g))
        else {
            return false;
        };
        // ServerPlayer.openMenu closes the menu that was open.
        self.guest_container_closed(host, conn);
        self.lan_station_closed(host, conn, None);
        host.hub.open_container(
            conn,
            menu,
            horizon_protocol::text::translatable(title, &[]),
            pos,
            slots,
            items,
        );
        true
    }

    /// The guest's open container is closing (`AbstractContainerMenu.removed`):
    /// it no longer counts among the openers of the lid.
    pub(super) fn guest_container_closed(&mut self, host: &mut LanHost, conn: u32) {
        if let Some(g) = host.hub.guest(conn)
            && let Some(pos) = g.menu.as_ref().and_then(|m| m.pos)
            && !g.spectator()
        {
            self.container_openers_changed(pos, false);
        }
    }

    /// ServerPlayer.startSleepInBed for a guest the bed accepted: they lie on
    /// it (pose and sleeping position, their own client included) and count
    /// for skipping the night.
    fn guest_start_sleeping(&mut self, host: &mut LanHost, i: usize, head: [i32; 3]) {
        let g = &mut host.hub.guests[i];
        g.sleeping = true;
        g.sleep_pos = Some(head);
        g.sleep_ticks = 0;
        let (id, conn) = (g.entity_id, g.conn);
        let at = [
            f64::from(head[0]) + 0.5,
            f64::from(head[1]) + 0.5625,
            f64::from(head[2]) + 0.5,
        ];
        host.hub.guests[i].position = at;
        if let Some(m) = host.hub.guests[i].motion.as_mut() {
            m.position = Vec3::new(at[0], at[1], at[2]);
            m.velocity = Vec3::new(0.0, 0.0, 0.0);
        }
        host.hub.send_to(
            conn,
            &C::SetEntityData(play::SetEntityData {
                id,
                values: vec![
                    (6, serializer::POSE, Value::Id(2)),
                    (14, serializer::OPTIONAL_BLOCK_POS, Value::OptPos(Some(head))),
                ],
            }),
        );
    }

    /// Player.stopSleepInBed for a guest: they stand, everyone sees them wake
    /// (ClientboundAnimatePacket WAKE_UP).
    fn guest_wake(&mut self, host: &mut LanHost, i: usize) {
        let g = &mut host.hub.guests[i];
        g.sleeping = false;
        g.sleep_pos = None;
        g.sleep_ticks = 0;
        let (id, conn, at) = (g.entity_id, g.conn, g.position);
        host.hub
            .broadcast_near(at, 128.0, &C::Animate(play::Animate { id, action: 0 }), None);
        host.hub.send_to(
            conn,
            &C::SetEntityData(play::SetEntityData {
                id,
                values: vec![
                    (6, serializer::POSE, Value::Id(0)),
                    (14, serializer::OPTIONAL_BLOCK_POS, Value::OptPos(None)),
                ],
            }),
        );
    }

    /// ServerLevel.tick's sleeping: players leave beds that went, by dying or
    /// by their own choice; once enough of them (players_sleeping_percentage)
    /// have slept 100 ticks the night is skipped and everyone wakes.
    fn guest_sleep_tick(&mut self, host: &mut LanHost) {
        let woke: Vec<usize> = host
            .hub
            .guests
            .iter()
            .enumerate()
            .filter(|(_, g)| {
                g.sleep_pos.is_some_and(|b| {
                    !g.sleeping
                        || g.dead
                        || !self
                            .res
                            .facts
                            .get(self.world.block(b[0], b[1], b[2]))
                            .name
                            .ends_with("_bed")
                })
            })
            .map(|(i, _)| i)
            .collect();
        for i in woke {
            self.guest_wake(host, i);
        }
        for g in host.hub.guests.iter_mut().filter(|g| g.sleep_pos.is_some()) {
            g.sleep_ticks = (g.sleep_ticks + 1).min(100);
        }
        // SleepStatus.update: the players who are not spectators.
        let players: Vec<_> = host
            .hub
            .guests
            .iter()
            .filter(|g| g.placed && !g.spectator())
            .collect();
        let sleeping = players.iter().filter(|g| g.sleep_pos.is_some()).count();
        let deep = players.iter().filter(|g| g.sleep_ticks >= 100).count();
        let percentage = self.ui.rules.int("players_sleeping_percentage").max(0) as usize;
        let needed = (players.len() * percentage).div_ceil(100).max(1);
        if sleeping != host.sleepers {
            host.sleepers = sleeping;
            // ServerLevel.announceSleepStatus, on everyone's action bar.
            if sleeping > 0 {
                let text = if sleeping >= needed {
                    horizon_protocol::text::translatable("sleep.skipping_night", &[])
                } else {
                    horizon_protocol::text::translatable(
                        "sleep.players_sleeping",
                        &[&sleeping.to_string(), &needed.to_string()],
                    )
                };
                for g in &host.hub.guests {
                    g.send(&C::SystemChat(play::SystemChat {
                        content: text.clone(),
                        overlay: true,
                    }));
                }
            }
        }
        if sleeping == 0 || sleeping < needed || deep < needed {
            return;
        }
        // The morning: the next day's 0, clear skies, everyone up.
        if self.ui.rules.bool("advance_time") {
            let t = self.world.day_time + 24000;
            self.world.day_time = t - t.rem_euclid(24000);
        }
        if self.ui.rules.bool("advance_weather") && self.world.weather.raining {
            let w = &mut self.world.weather;
            w.raining = false;
            w.thundering = false;
            w.rain_time = 0;
            w.thunder_time = 0;
        }
        let asleep: Vec<usize> = (0..host.hub.guests.len())
            .filter(|&i| host.hub.guests[i].sleep_pos.is_some())
            .collect();
        for i in asleep {
            self.guest_wake(host, i);
        }
        host.sleepers = 0;
    }

    /// ExperienceOrb.playerTouch for a guest: everyone sees the orb fly to
    /// them (Player.take), mending repairs with it, the rest is experience.
    pub(super) fn guest_take_orb(
        &mut self,
        host: &mut LanHost,
        conn: u32,
        orb: &horizon_client::entities::ExperienceOrb,
    ) {
        let Some(id) = host.hub.guest(conn).map(|g| g.entity_id) else {
            return;
        };
        let at = [orb.position.x, orb.position.y, orb.position.z];
        host.hub.take_item(orb.id as i32, id, 1, at);
        let value = orb.value;
        let level_up = self
            .as_guest(host, conn, |g| {
                let gained = g.mending(value);
                gained > 0 && g.experience.give(gained as i32) && g.experience.level % 5 == 0
            })
            .unwrap_or(false);
        // Player.giveExperiencePoints: every fifth level is heard by those near.
        if level_up && let Some(g) = host.hub.guest(conn) {
            let level = g.xp_level;
            let volume = if level > 30 { 1.0 } else { level as f32 / 30.0 };
            host.hub.sound(
                "minecraft:entity.player.levelup",
                g.position,
                volume * 0.75,
                1.0,
                7,
                None,
            );
        }
    }

    /// A packet from a guest that the hub does not handle itself.
    pub(super) fn lan_guest_packet(&mut self, host: &mut LanHost, conn: u32, packet: S) {
        let Some(gi) = host.hub.guests.iter().position(|g| g.conn == conn) else {
            return;
        };
        if host.hub.guests[gi].dead && !matches!(packet, S::ClientCommand(_)) {
            return;
        }
        let spectator = host.hub.guests[gi].spectator();
        match packet {
            // The `/aot` gear's cables, for the others (game/lan_aot.rs).
            S::CustomPayload(p) if p.channel == horizon_client::odm::wire::CHANNEL => {
                self.guest_aot_state(host, conn, &p.data);
            }
            S::PlayerAction(a) => match a.action {
                0 => {
                    host.hub.ack(conn, a.sequence);
                    if spectator {
                        return;
                    }
                    let face = direction(i32::from(a.direction));
                    let play = host.play.entry(conn).or_default();
                    play.digging = Some((a.pos, face));
                    play.dig_start = host.hub.ticks;
                    play.last_progress = -1;
                    // The pause between two blocks is the client's (MultiPlayerGameMode's
                    // destroyDelay); the server digs from the START it receives.
                    play.destroy_delay = 0;
                    // handleBlockBreakAction START: a block a survival player breaks in
                    // one tick goes at once (destroyAndAck); others wait for STOP.
                    let state = self.world.block(a.pos[0], a.pos[1], a.pos[2]);
                    if !host.hub.guests[gi].creative()
                        && !self.res.facts.is_air(state)
                        && self
                            .as_guest(host, conn, |g| g.destroy_progress(a.pos))
                            .is_some_and(|p| p >= 1.0)
                    {
                        let play = host.play.entry(conn).or_default();
                        play.digging = None;
                        play.mining = None;
                        self.guest_destroy(host, conn, a.pos);
                    }
                }
                1 => {}
                3 => {
                    host.hub.ack(conn, a.sequence);
                    self.guest_stop_digging(host, conn, a.pos);
                }
                // ServerboundPlayerActionPacket STAB: a spear's attack.
                8 if !spectator => self.guest_stab(host, conn),
                2 => {
                    host.hub.ack(conn, a.sequence);
                    let play = host.play.entry(conn).or_default();
                    play.digging = None;
                    play.mining = None;
                    let state = self.world.block(a.pos[0], a.pos[1], a.pos[2]).0 as i32;
                    host.hub.send_to(
                        conn,
                        &C::BlockUpdate(play::BlockUpdate { pos: a.pos, state }),
                    );
                    host.hub.broadcast_near(
                        [
                            f64::from(a.pos[0]),
                            f64::from(a.pos[1]),
                            f64::from(a.pos[2]),
                        ],
                        64.0,
                        &C::BlockDestruction(play::BlockDestruction {
                            id: host.hub.guests[gi].entity_id,
                            pos: a.pos,
                            progress: 255,
                        }),
                        Some(conn),
                    );
                }
                4 | 5 => {
                    if !spectator {
                        self.as_guest(host, conn, |g| g.drop_held(a.action == 4));
                    }
                }
                6 => {
                    // RELEASE_USE_ITEM: bows shoot and tridents throw (or riptide),
                    // as in Game::tick_used_item; a spyglass just lowers.
                    self.as_guest(host, conn, |g| {
                        if let Some((slot, ticks)) = g.using.take() {
                            match g.stack_in(slot).map(|s| s.item.clone()).as_deref() {
                                Some("minecraft:bow") => g.release_bow(ticks),
                                Some("minecraft:trident") => g.release_trident(ticks),
                                Some("minecraft:spyglass") => g.stop_spyglass(),
                                _ => {}
                            }
                        }
                    });
                }
                // SWAP_ITEM_WITH_OFFHAND: the hands swap and the item in use stops
                // (ServerGamePacketListenerImpl.handlePlayerAction), not for spectators.
                7 if !spectator => {
                    let g = &mut host.hub.guests[gi];
                    let slot = g.selected;
                    std::mem::swap(&mut g.inventory[slot], &mut g.offhand);
                    if let Some(play) = host.play.get_mut(&conn) {
                        play.using = None;
                    }
                }
                _ => {}
            },
            S::SetCarriedItem(p) => {
                if (0..9).contains(&p.slot) {
                    host.hub.guests[gi].selected = p.slot as usize;
                    if let Some(play) = host.play.get_mut(&conn) {
                        play.using = None;
                    }
                }
            }
            S::UseItemOn(p) => {
                host.hub.ack(conn, p.sequence);
                if spectator {
                    return;
                }
                let face = direction(p.face);
                let hit = Hit {
                    pos: p.pos,
                    face,
                    point: [
                        f64::from(p.pos[0]) + f64::from(p.cursor[0]),
                        f64::from(p.pos[1]) + f64::from(p.cursor[1]),
                        f64::from(p.pos[2]) + f64::from(p.cursor[2]),
                    ],
                };
                self.as_guest(host, conn, |g| {
                    g.forced_hit = Some(hit);
                    let _ = g.use_item();
                    g.forced_hit = None;
                });
                // The client predicted a result: correct it with what really is.
                for pos in [
                    p.pos,
                    [p.pos[0] + face[0], p.pos[1] + face[1], p.pos[2] + face[2]],
                ] {
                    let state = self.world.block(pos[0], pos[1], pos[2]).0 as i32;
                    host.hub
                        .send_to(conn, &C::BlockUpdate(play::BlockUpdate { pos, state }));
                }
            }
            S::UseItem(p) => {
                host.hub.ack(conn, p.sequence);
                if spectator {
                    return;
                }
                // handleUseItem: the client's rotation, then the item alone. What it
                // looked at a moment ago (a block, a mob) is UseItemOn's or Interact's.
                self.as_guest(host, conn, |g| {
                    (g.yaw, g.pitch) = (p.yaw, p.pitch);
                    g.lan_item_only = true;
                    let _ = g.use_item();
                    g.lan_item_only = false;
                });
            }
            S::Attack(p) => self.guest_attack(host, conn, p.id),
            S::Interact(p) => {
                if let Some((position, eye)) = self.look_at_entity(host, gi, p.id) {
                    let _ = (position, eye);
                    self.as_guest(host, conn, |g| {
                        let _ = g.use_item();
                    });
                }
            }
            S::SetCreativeModeSlot(p) => {
                let g = &mut host.hub.guests[gi];
                if !g.creative() || !(0..46).contains(&p.slot) {
                    return;
                }
                let item = self.item_from_network(host, &p.item);
                host.hub.guests[gi].set_menu_slot(p.slot as usize, item);
            }
            S::ContainerClick(p) if self.lan_merchant_click(host, conn, &p) => {}
            S::ContainerClick(p) if self.lan_inventory_click(host, conn, &p) => {}
            S::ContainerClick(p) if self.lan_crafting_click(host, conn, &p) => {}
            S::ContainerClick(p) if self.lan_furnace_click(host, conn, &p) => {}
            S::ContainerClick(p) if self.lan_station_click(host, conn, &p) => {}
            S::ContainerClick(p) if self.lan_brewing_click(host, conn, &p) => {}
            S::ContainerButtonClick(p) => self.lan_station_button(host, conn, p.container, p.button),
            S::RenameItem(p) => self.lan_station_rename(host, conn, &p.name),
            S::SelectTrade(p) => self.lan_select_trade(host, conn, p.item),
            S::ContainerClose(p) if self.lan_merchant_closed(host, conn, Some(p.container)) => {
                self.place_back_carried(host, conn);
            }
            S::ContainerClose(p) if self.lan_crafting_closed(host, conn, Some(p.container)) => {
                self.place_back_carried(host, conn);
            }
            S::ContainerClose(p) if self.lan_station_closed(host, conn, Some(p.container)) => {}
            S::ContainerClick(p) => {
                let rules = ItemRules(&self.res.items);
                let drops = host.hub.container_click(conn, &p, &mut self.world, &rules);
                self.drop_guest_items(host, gi, drops);
            }
            S::ContainerClose(p) => {
                if host.hub.guests[gi]
                    .menu
                    .as_ref()
                    .is_some_and(|m| m.window == p.container)
                {
                    self.guest_container_closed(host, conn);
                }
                host.hub.container_closed(conn, p.container);
                self.place_back_carried(host, conn);
            }
            S::ChatCommand(c) => self.guest_command(host, conn, &c.command),
            // A swing at nothing (attack missed): the arm swings for everyone
            // else (handleAnimate); the attack strength is left alone.
            S::Punch(_) => {
                self.as_guest(host, conn, |g| g.swing_arm());
            }
            S::ChangeDifficulty(p) if host.hub.guests[gi].permission >= 2 => {
                if !self.level.difficulty_locked && (0..=3).contains(&p.difficulty) {
                    self.ui.difficulty = if self.level.hardcore {
                        3
                    } else {
                        p.difficulty as u8
                    };
                    self.sync_rules();
                    if self.ui.difficulty == 0 {
                        self.mobs.retain(|m| !m.hostile());
                    }
                    host.hub
                        .broadcast(&C::ChangeDifficulty(play::ChangeDifficulty {
                            difficulty: i32::from(self.ui.difficulty),
                            locked: self.level.difficulty_locked,
                        }));
                }
            }
            S::LockDifficulty(p) if host.hub.guests[gi].permission >= 2 => {
                self.level.difficulty_locked = p.locked;
                host.hub
                    .broadcast(&C::ChangeDifficulty(play::ChangeDifficulty {
                        difficulty: i32::from(self.ui.difficulty),
                        locked: self.level.difficulty_locked,
                    }));
            }
            S::ClientCommand(p) if p.action == 2 && host.hub.guests[gi].permission >= 2 => {
                host.hub.send_to(
                    conn,
                    &C::GameRuleValues(play::GameRuleValues {
                        entries: horizon_client::gamerules::RULES
                            .iter()
                            .map(|r| play::GameRuleEntry {
                                name: format!("minecraft:{}", r.name),
                                value: self.ui.rules.get(r.name).unwrap_or(r.default).to_string(),
                            })
                            .collect(),
                    }),
                );
            }
            S::SetGameRule(p) if host.hub.guests[gi].permission >= 2 => {
                for entry in p.entries {
                    if self.ui.rules.set_string(&entry.name, &entry.value) {
                        let message = horizon_client::text::Text::tr(
                            "commands.gamerule.set",
                            &[&entry.name, &entry.value],
                        );
                        let content =
                            horizon_client::lan::data::component_nbt(&message.component());
                        for g in &host.hub.guests {
                            if g.permission >= 2 {
                                g.send(&C::SystemChat(play::SystemChat {
                                    content: content.clone(),
                                    overlay: false,
                                }));
                            }
                        }
                        if self.options.dedicated.is_none() && self.allow_commands() {
                            self.system_message(message);
                        }
                    }
                }
                self.sync_rules();
            }
            S::ChangeGameMode(p) if host.hub.guests[gi].permission >= 2 => {
                if let Some(mode) = usize::try_from(p.mode)
                    .ok()
                    .and_then(|index| horizon_client::command::args::GameMode::ALL.get(index))
                {
                    let lines = self.as_guest(host, conn, |g| {
                        g.chat_capture = Some(Vec::new());
                        g.change_game_mode(*mode);
                        g.chat_capture
                            .take()
                            .unwrap_or_default()
                            .iter()
                            .map(|t| horizon_client::lan::data::component_nbt(&t.component()))
                            .collect::<Vec<_>>()
                    });
                    for content in lines.unwrap_or_default() {
                        host.hub.send_to(
                            conn,
                            &C::SystemChat(play::SystemChat {
                                content,
                                overlay: false,
                            }),
                        );
                    }
                }
            }
            S::Unknown(8, payload) => {
                // A signed command: the text comes first.
                if let Ok(text) = horizon_protocol::buf::Reader::new(&payload).string(32767) {
                    self.guest_command(host, conn, &text);
                }
            }
            S::Unknown(id, payload)
                if id == horizon_protocol::ids::play::serverbound::COMMAND_SUGGESTION =>
            {
                self.guest_suggestions(host, conn, &payload);
            }
            S::ClientCommand(c) if c.action == 0 => self.guest_respawn(host, conn),
            // ServerStatsCounter.sendStats.
            S::ClientCommand(c) if c.action == 1 => {
                let stats = host
                    .play
                    .get_mut(&conn)
                    .map(|p| p.stats.take_dirty())
                    .unwrap_or_default();
                host.hub.send_stats(conn, &stats);
            }
            S::PickItemFromBlock(p) => {
                let state = self.world.block(p.pos[0], p.pos[1], p.pos[2]);
                let item = self.res.facts.get(state).item.clone();
                let g = &mut host.hub.guests[gi];
                if g.creative() && &*item != "minecraft:air" {
                    let slot = g.selected;
                    g.inventory[slot] = Some(ItemStack::new(&item, 1));
                }
            }
            _ => {}
        }
    }

    fn item_from_network(
        &self,
        host: &LanHost,
        stack: &Option<horizon_protocol::item::Stack>,
    ) -> Option<ItemStack> {
        use horizon_protocol::item::{Component, component};
        let s = stack.as_ref()?;
        let name = host
            .hub
            .item_name(s.item)
            .unwrap_or_else(|| "minecraft:air".into());
        let mut out = ItemStack::new(&name, s.count.max(1) as u32);
        out.damage = s.damage().max(0) as u32;
        if let Some(Component::Enchantments(list)) = s.component(component::ENCHANTMENTS) {
            for (id, level) in list {
                if let Some(e) = host.hub.enchantment_name(*id) {
                    out.enchant(&e, *level as u32);
                }
            }
        }
        Some(out)
    }

    /// AbstractContainerMenu.removed: the stack on the cursor goes back into
    /// the inventory (Inventory.placeItemBackInInventory); only what does
    /// not fit is dropped.
    fn place_back_carried(&mut self, host: &mut LanHost, conn: u32) {
        let Some(gi) = host.hub.guests.iter().position(|g| g.conn == conn) else {
            return;
        };
        let Some(carried) = host.hub.guests[gi].carried.take() else {
            return;
        };
        if let Some(left) = self.as_guest(host, conn, |g| g.give(carried)).flatten() {
            self.drop_guest_items(host, gi, vec![left]);
        }
    }
    pub(super) fn drop_guest_items(&mut self, host: &LanHost, gi: usize, stacks: Vec<ItemStack>) {
        let g = &host.hub.guests[gi];
        let eye = [
            g.position[0],
            g.position[1] + g.eye_height() - 0.3,
            g.position[2],
        ];
        let (yaw, pitch) = (
            f64::from(g.yaw).to_radians(),
            f64::from(g.pitch).to_radians(),
        );
        for stack in stacks {
            let v = [
                -yaw.sin() * pitch.cos() * 0.3,
                -pitch.sin() * 0.3 + 0.1,
                yaw.cos() * pitch.cos() * 0.3,
            ];
            self.spawn_item(stack, eye, v, 40);
        }
    }

    /// Aim the swapped-in player at an entity; returns its centre and the eye.
    fn look_at_entity(
        &mut self,
        host: &mut LanHost,
        gi: usize,
        id: i32,
    ) -> Option<([f64; 3], [f64; 3])> {
        let m = self.mobs.iter().find(|m| m.id as i32 == id)?;
        let centre = [
            m.position.x,
            m.position.y + m.kind.height * 0.5,
            m.position.z,
        ];
        let g = &mut host.hub.guests[gi];
        let eye = [g.position[0], g.position[1] + g.eye_height(), g.position[2]];
        let d = [centre[0] - eye[0], centre[1] - eye[1], centre[2] - eye[2]];
        let horizontal = (d[0] * d[0] + d[2] * d[2]).sqrt();
        g.yaw = (-d[0].atan2(d[2])).to_degrees() as f32;
        g.pitch = (-d[1].atan2(horizontal)).to_degrees() as f32;
        Some((centre, eye))
    }

    fn guest_attack(&mut self, host: &mut LanHost, conn: u32, id: i32) {
        let Some(gi) = host.hub.guests.iter().position(|g| g.conn == conn) else {
            return;
        };
        if host.hub.guests[gi].spectator() {
            return;
        }
        // Moves the client sent before this attack are applied at the tick;
        // vanilla handles them first, so a jump's fall counts for the crit.
        let fall = pending_fall(&host.hub.guests[gi]);
        if let Some(victim) = Self::pvp_victim(host, conn, id) {
            if let Some(Some(mut strike)) =
                self.as_guest(host, conn, |g| g.with_pending_fall(fall, Self::pvp_strike))
            {
                strike.attacker = Some(host.hub.guests[gi].name.clone());
                self.pvp_land(host, victim, &strike);
            }
            return;
        }
        let (old_yaw, old_pitch) = (host.hub.guests[gi].yaw, host.hub.guests[gi].pitch);
        if self.look_at_entity(host, gi, id).is_none() {
            return;
        }
        self.as_guest(host, conn, |g| {
            g.with_pending_fall(fall, |g| {
                g.attack();
            });
        });
        // The mob now answers to this guest (game/lan_mobs.rs).
        if let Ok(id) = u32::try_from(id) {
            host.mob_players.insert(id, conn);
        }
        host.hub.guests[gi].yaw = old_yaw;
        host.hub.guests[gi].pitch = old_pitch;
    }

    /// ServerGamePacketListenerImpl's STAB: with the spear's full charge
    /// (five ticks' allowance), PiercingWeapon.attack along the guest's look:
    /// the mobs in reach, and the other players in it as player attacks.
    fn guest_stab(&mut self, host: &mut LanHost, conn: u32) {
        let Some(gi) = host.hub.guests.iter().position(|g| g.conn == conn) else {
            return;
        };
        let movement = host.hub.guests[gi].known_movement;
        let stab = self
            .as_guest(host, conn, |g| {
                if g.cannot_attack_with_held(5) {
                    return None;
                }
                g.spear_movement = Some(movement);
                let stab = g.spear_stab();
                g.spear_movement = None;
                stab
            })
            .flatten();
        let Some(stab) = stab else {
            return;
        };
        let Some(reach) = stab.reach else {
            return;
        };
        let (from, name) = {
            let g = &host.hub.guests[gi];
            (g.position, g.name.clone())
        };
        let victims: Vec<u32> = host
            .hub
            .guests
            .iter()
            .filter(|v| v.conn != conn && v.placed && !v.dead && !v.spectator())
            .filter(|v| {
                let height = if v.sneaking { 1.5 } else { 1.8 };
                let p = v.position;
                reach.hits(horizon_core::geometry::Aabb::new(
                    Vec3::new(p[0] - 0.3, p[1], p[2] - 0.3),
                    Vec3::new(p[0] + 0.3, p[1] + height, p[2] + 0.3),
                ))
            })
            .map(|v| v.conn)
            .collect();
        for victim in victims {
            let strike = super::lan_pvp::Strike::stab(stab.player_damage, stab.scale, from, name.clone());
            self.pvp_land(host, super::lan_pvp::Victim::Guest(victim), &strike);
        }
    }

    /// `KineticWeapon.damageEntities` on the other players a guest's spear
    /// charge reaches: each not struck in the cooldown takes the charge's
    /// damage when the guest closes in on them fast enough.
    fn guest_charge(&mut self, host: &mut LanHost, conn: u32, charge: super::spear::Charge) -> bool {
        let Some(g) = host.hub.guest(conn) else {
            return false;
        };
        let (from, name, who) = (g.position, g.name.clone(), g.uuid);
        let look = charge.reach.look;
        let now = self.world.game_time;
        let victims: Vec<(u32, u32, [f64; 3])> = host
            .hub
            .guests
            .iter()
            .filter(|v| v.conn != conn && v.placed && !v.dead && !v.spectator())
            .filter(|v| {
                let height = if v.sneaking { 1.5 } else { 1.8 };
                let p = v.position;
                charge.reach.hits(horizon_core::geometry::Aabb::new(
                    Vec3::new(p[0] - 0.3, p[1], p[2] - 0.3),
                    Vec3::new(p[0] + 0.3, p[1] + height, p[2] + 0.3),
                ))
            })
            .map(|v| (v.conn, v.entity_id as u32, v.known_movement))
            .collect();
        let mut struck = false;
        for (victim, id, moved) in victims {
            let key = (who, id);
            let cooldown = charge.kinetic.contact_cooldown_ticks as i64;
            if self.spear_contacts.get(&key).is_some_and(|at| now - *at < cooldown) {
                continue;
            }
            self.spear_contacts.insert(key, now);
            let target = (0..3).map(|a| moved[a] * look[a]).sum::<f64>() * 20.0;
            let relative = (charge.attacker - target).max(0.0);
            let Some(effects) = super::spear::Effects::of_charge(
                &charge.kinetic,
                charge.ticks,
                charge.attacker,
                relative,
            ) else {
                continue;
            };
            if effects.damage {
                let amount = charge.kinetic.charge_damage(relative);
                let strike = super::lan_pvp::Strike::stab(amount, 1.0, from, name.clone());
                self.pvp_land(host, super::lan_pvp::Victim::Guest(victim), &strike);
                struck = true;
            }
        }
        struck
    }

    /// Runs `f` (an attack) with the guest's fall distance as of the moves
    /// already received: `fall` is (landed since the last tick, fallen since).
    fn with_pending_fall<R>(&mut self, fall: (bool, f64), f: impl FnOnce(&mut Self) -> R) -> R {
        let (landed, fallen) = fall;
        let saved = self.player.as_ref().map(|p| p.fall_distance);
        let adjusted = self.player.as_mut().map(|p| {
            if landed {
                p.fall_distance = 0.0;
            }
            p.fall_distance += fallen;
            p.fall_distance
        });
        let result = f(self);
        // The moves add their fall again at the tick; what the attack itself
        // did to the fall (a mace's smash ends it) stays.
        if let (Some(p), Some(saved)) = (self.player.as_mut(), saved)
            && Some(p.fall_distance) == adjusted
        {
            p.fall_distance = saved;
        }
        result
    }

    /// Commands.performCommand for a guest with command access.
    fn guest_command(&mut self, host: &mut LanHost, conn: u32, command: &str) {
        // ListPlayersCommand needs no permission and lists every player.
        let words: Vec<&str> = command.trim_start_matches('/').split(' ').collect();
        if words[0] == "list" && (words.len() == 1 || words[1..] == ["uuids"]) {
            let me = (self.options.dedicated.is_none() && self.player.is_some())
                .then_some((host.host_uuid, host.host_name.as_str()));
            let content = host.hub.list_reply(me, words.len() == 2);
            host.hub.send_to(
                conn,
                &C::SystemChat(play::SystemChat {
                    content,
                    overlay: false,
                }),
            );
            return;
        }
        // Teleport requests need no permission either: the other player answers.
        if super::lan_tpa::command(host, conn, &words)
            || self.home_command(host, conn, &words)
            || self.aot_command(host, conn, &words)
        {
            return;
        }
        // An integrated server runs every player's commands at their own
        // permission level (`CommandSourceStack.permissions`).
        if host.hub.config.integrated {
            let Some(level) = host.hub.guest(conn).map(|g| g.permission) else {
                return;
            };
            let command = command.trim_start_matches('/').to_owned();
            let lines = self.as_guest(host, conn, |g| {
                let console = std::mem::replace(&mut g.ui.registries.permission, level);
                g.chat_capture = Some(Vec::new());
                g.run_command(&command);
                g.ui.registries.permission = console;
                g.chat_capture.take().unwrap_or_default()
            });
            for line in lines.unwrap_or_default() {
                host.hub.send_to(
                    conn,
                    &C::SystemChat(play::SystemChat {
                        content: horizon_client::lan::data::component_nbt(&line.component()),
                        overlay: false,
                    }),
                );
            }
            return;
        }
        // A LAN world follows its Allow Commands; a dedicated server leaves
        // it to the ops list (the guest's permission level).
        let allowed = self.options.dedicated.is_some() || self.allow_commands();
        if !host.settings.guest_commands || !allowed {
            host.hub.send_to(
                conn,
                &C::SystemChat(play::SystemChat {
                    content: horizon_protocol::text::translatable("command.unknown.command", &[]),
                    overlay: false,
                }),
            );
            return;
        }
        let command = command.trim_start_matches('/').to_owned();
        // The guest's own level (Commands.performCommand with the player's source): an
        // operator's on a dedicated server, never the host's.
        let permission = host.hub.guest(conn).map_or(0, |g| g.permission);
        let lines = self.as_guest(host, conn, |g| {
            g.chat_capture = Some(Vec::new());
            let level = std::mem::replace(&mut g.ui.registries.permission, permission);
            g.run_command(&command);
            g.ui.registries.permission = level;
            g.chat_capture
                .take()
                .unwrap_or_default()
                .iter()
                .map(|t| horizon_client::lan::data::component_nbt(&t.component()))
                .collect::<Vec<_>>()
        });
        for content in lines.unwrap_or_default() {
            host.hub.send_to(
                conn,
                &C::SystemChat(play::SystemChat {
                    content,
                    overlay: false,
                }),
            );
        }
    }

    /// ServerGamePacketListenerImpl.handleCustomCommandSuggestions: the
    /// completions of the command typed so far (after an optional slash), for
    /// the nodes the tree marks `ask_server`.
    fn guest_suggestions(&mut self, host: &mut LanHost, conn: u32, payload: &[u8]) {
        let mut r = horizon_protocol::buf::Reader::new(payload);
        let (Ok(id), Ok(text)) = (r.varint(), r.string(32500)) else {
            return;
        };
        let Some(permission) = host.hub.guest(conn).map(|g| g.permission) else {
            return;
        };
        let skip = usize::from(text.starts_with('/'));
        let command = &text[skip..];
        let d = vanilla::game_dispatcher();
        let level = std::mem::replace(&mut self.ui.registries.permission, permission);
        let parse = d.parse(command, &self.ui.registries);
        let ((start, end), suggestions) = d.suggestions(&parse, command.len(), &self.ui.registries);
        self.ui.registries.permission = level;
        let utf16 = |byte: usize| command[..byte.min(command.len())].encode_utf16().count() as i32;
        let mut w = horizon_protocol::buf::Writer::new();
        w.varint(id);
        w.varint(utf16(start) + skip as i32);
        w.varint(utf16(end) - utf16(start));
        w.varint(suggestions.len() as i32);
        for s in &suggestions {
            w.string(&s.text);
            w.bool(s.tooltip.is_some());
            if let Some(tip) = &s.tooltip {
                w.nbt(&literal(tip));
            }
        }
        host.hub.send_to(
            conn,
            &C::Unknown(
                horizon_protocol::ids::play::clientbound::COMMAND_SUGGESTIONS,
                w.data,
            ),
        );
    }

    fn guest_respawn(&mut self, host: &mut LanHost, conn: u32) {
        let world_spawn = self.spawn.map_or([0.5, 80.0, 0.5], |s| [s.x, s.y, s.z]);
        let Some(i) = host.hub.guests.iter().position(|g| g.conn == conn) else {
            return;
        };
        if !host.hub.guests[i].dead {
            return;
        }
        // ServerPlayer.findRespawnPositionAndUseSpawnBlock: their bed while it
        // stands (on it, as BedBlock.findStandUpPosition's fallback), else the
        // world spawn with vanilla's notice.
        let bed = host.hub.guests[i].respawn_bed.filter(|b| {
            self.res
                .facts
                .get(self.world.block(b[0], b[1], b[2]))
                .name
                .ends_with("_bed")
        });
        if bed.is_none() && host.hub.guests[i].respawn_bed.take().is_some() {
            super::lan_tpa::tell(
                host,
                conn,
                "You have no home bed or charged respawn anchor, or it was obstructed",
                "white",
            );
        }
        let spawn = bed.map_or(world_spawn, |b| {
            [
                f64::from(b[0]) + 0.5,
                f64::from(b[1]) + 0.5625,
                f64::from(b[2]) + 0.5,
            ]
        });
        self.respawn_level(host);
        let g = &mut host.hub.guests[i];
        g.dead = false;
        g.health = 20.0;
        g.food = 20;
        g.saturation = 5.0;
        g.sent_health = None;
        g.motion = None;
        let mut xp = Experience {
            level: g.xp_level,
            progress: g.xp_progress,
            total: g.xp_total,
        };
        xp.respawn(self.ui.rules.bool("keep_inventory") || g.game_mode == SPECTATOR);
        (g.xp_level, g.xp_progress, g.xp_total) = (xp.level, xp.progress, xp.total);
        // A new body: nothing given to the old one carries over.
        if let Some(play) = host.play.get_mut(&conn) {
            play.potions = Default::default();
        }
        let ctx = TickCtx {
            world: &mut self.world,
            host: None,
            entities: &[],
            next_entity: &mut self.next_entity,
            spawn,
            difficulty: i32::from(self.ui.difficulty),
            difficulty_locked: self.level.difficulty_locked,
            clock_rate: if self.ui.rules.bool("advance_time") {
                self.ui.clock.rate
            } else {
                0.0
            },
            tick_rate: self.clock.rate(),
            tick_frozen: self.clock.frozen(),
        };
        host.hub.respawn(i, spawn, &ctx);
    }

    /// Per tick, for every guest: dig, use items, pick up drops, die.
    pub(super) fn tick_guests(&mut self, host: &mut LanHost) {
        self.guest_sleep_tick(host);
        self.lan_merchant_tick(host);
        self.lan_stations_tick(host);
        self.announce_guest_advancements(host);
        let conns: Vec<u32> = host
            .hub
            .guests
            .iter()
            .filter(|g| g.placed && g.loaded)
            .map(|g| g.conn)
            .collect();
        for conn in conns {
            let Some(gi) = host.hub.guests.iter().position(|g| g.conn == conn) else {
                continue;
            };
            if host.hub.guests[gi].dead || self.on_server_trip(conn) {
                continue;
            }
            let play = host.play.entry(conn).or_default();
            if std::mem::take(&mut play.use_finished) {
                // ServerPlayer.completeUsingItem, sent once the hub has sent
                // what eating changed (sync_player_state ran this tick first).
                let id = host.hub.guests[gi].entity_id;
                host.hub.send_to(
                    conn,
                    &C::EntityEvent(play::EntityEvent { id, event: 9 }),
                );
            }
            let movement = host.hub.guests[gi].known_movement;
            let play = host.play.entry(conn).or_default();
            let digging = play.digging;
            let using = play.using.is_some();
            let delayed = play.destroy_delay > 0 || play.use_delay > 0 || play.attack_ticks < 100;
            if digging.is_some() || using || delayed {
                let finished = self.as_guest(host, conn, |g| {
                    g.attack_ticks = g.attack_ticks.saturating_add(1);
                    if let Some((pos, face)) = digging {
                        g.forced_hit = Some(Hit {
                            pos,
                            face,
                            point: [
                                f64::from(pos[0]) + 0.5,
                                f64::from(pos[1]) + 0.5,
                                f64::from(pos[2]) + 0.5,
                            ],
                        });
                    }
                    let _ = g.tick_mining();
                    g.forced_hit = None;
                    g.right = g.using.is_some();
                    let was_using = g.using.is_some();
                    // A spear's charge reads how they moved (game/spear.rs).
                    g.spear_movement = Some(movement);
                    g.tick_using();
                    g.spear_movement = None;
                    if g.use_delay > 0 {
                        g.use_delay -= 1;
                    }
                    // The client still holds the button: only finishing ends a use here.
                    (was_using && g.using.is_none(), g.spear_charged.take())
                });
                let (finished, charge) = finished.unzip();
                if finished == Some(true) {
                    host.play.entry(conn).or_default().use_finished = true;
                }
                if let Some(charge) = charge.flatten() {
                    let hit = charge.hit;
                    // KineticWeapon.damageEntities: the holder's EntityEvent 2
                    // (the spear's jolt) for everyone near once it struck.
                    if (self.guest_charge(host, conn, charge) || hit)
                        && let Some(g) = host.hub.guest(conn)
                    {
                        host.hub.entity_event(g.entity_id, 2, g.position);
                    }
                }
            }
            // LivingEntity.DATA_LIVING_ENTITY_FLAGS for the others: the hand in use.
            let used = host.play.get(&conn).and_then(|p| p.using);
            host.hub.guests[gi].using =
                used.map(|(slot, _)| slot == super::shield::OFFHAND_SLOT);
            self.guest_delayed_destroy(host, conn);
            self.guest_server_tick(host, conn);
            // Player.tick's statistics and ServerPlayer.tick's triggers, then
            // PlayerAdvancements.flushDirty.
            let sleeping = host.hub.guests[gi].sleeping;
            self.as_guest(host, conn, |g| {
                g.count_tick_stats(sleeping);
                g.tick_advancements();
            });
            host.hub.flush_progress(conn);
            self.guest_dig_feedback(host, conn);
            self.guest_pickups(host, gi);
            let g = &mut host.hub.guests[gi];
            if g.health <= 0.0 && !g.dead {
                g.dead = true;
                g.last_death = Some(g.position.map(|v| v.floor() as i32));
                let (id, at) = (g.entity_id, g.position);
                let reward = Experience {
                    level: g.xp_level,
                    progress: g.xp_progress,
                    total: g.xp_total,
                }
                .death_reward(self.ui.rules.bool("keep_inventory") || g.game_mode == SPECTATOR);
                // CombatTracker.getDeathMessage under the guest's name, on the
                // death screen and (show_death_messages) in everyone's chat.
                let name = g.name.clone();
                let below_world = at[1] < f64::from(horizon_client::chunk::MIN_Y - 64);
                let text = self
                    .as_guest(host, conn, |g| {
                        let me = horizon_client::text::Text::literal(name.clone());
                        if below_world {
                            horizon_client::text::Text::translate("death.attack.outOfWorld", &[me])
                        } else {
                            g.combat_death_message_as(me)
                        }
                    })
                    .unwrap_or_else(|| {
                        horizon_client::text::Text::translate(
                            "death.attack.generic",
                            &[horizon_client::text::Text::literal(name)],
                        )
                    });
                let message = horizon_client::lan::data::component_nbt(&text.component());
                host.hub.send_to(
                    conn,
                    &C::PlayerCombatKill(play::PlayerCombatKill {
                        id,
                        message: message.clone(),
                    }),
                );
                if self.ui.rules.bool("show_death_messages") {
                    host.hub.system_message(message);
                }
                // ServerPlayer.die: the guest's items spill where they fell.
                let chest = [at[0], at[1] + 1.32, at[2]];
                self.as_guest(host, conn, |g| g.drop_all_on_death(chest));
                // LivingEntity.die: the death animation, the dying player included.
                host.hub.entity_event(id, 3, at);
                self.spawn_orbs(at, reward);
            }
        }
    }

    /// `PlayerList.broadcastSystemMessage` of the advancements guests made
    /// (their own actions, commands and ticks).
    fn announce_guest_advancements(&mut self, host: &mut LanHost) {
        for text in std::mem::take(&mut self.adv.announced) {
            host.hub
                .system_message(horizon_client::lan::data::component_nbt(&text.component()));
            if self.options.dedicated.is_none() {
                self.system_message(text);
            }
        }
    }

    /// The guest's body on the server each tick: ServerPlayer.tick's damage
    /// clock and Entity.baseTick's fluid phase (burning), then for each move
    /// the guest made ServerGamePacketListenerImpl.handleMovePlayer's jump
    /// (`ServerPlayer.jumpFromGround`'s exhaustion),
    /// ServerPlayer.doCheckFallDamage (the fall distance grows while they go
    /// down and hurts them where they land, as the host's own landing does;
    /// water resets it, lava halves it) and checkMovementStatistics (the
    /// exhaustion of sprinting and swimming), then LivingEntity.baseTick's
    /// hazards (drowning, lava, fire blocks, suffocation, the void), and
    /// regeneration and the food tick (ServerPlayer.doTick). Their hurt
    /// sounds are heard at the guest.
    fn guest_server_tick(&mut self, host: &mut LanHost, conn: u32) {
        let Some(g) = host.hub.guest_mut(conn) else {
            return;
        };
        let moves = std::mem::take(&mut g.moves);
        // The `/aot` gear: flying and falling are theirs (game/lan_aot.rs).
        let aot = g.aot;
        g.known_movement = moves.iter().fold([0.0; 3], |a, (m, _)| {
            [a[0] + m[0], a[1] + m[1], a[2] + m[2]]
        });
        let from_ground = if moves.is_empty() {
            g.on_ground
        } else {
            g.moves_from_ground
        };
        // A double jump (the hosted world's mod): falling, then thrown up in
        // mid-air. Each forgives DOUBLE_JUMP_FALL_GRACE blocks of the fall so
        // far, or jumping again while falling hurt more than not jumping.
        let mut last_dy = g.last_move_dy;
        let boosts: Vec<bool> = moves
            .iter()
            .map(|(moved, on_ground)| {
                let boost = !on_ground && last_dy < 0.0 && moved[1] > DOUBLE_JUMP_RISE;
                last_dy = if *on_ground { 0.0 } else { moved[1] };
                boost
            })
            .collect();
        g.last_move_dy = last_dy;
        if g.spectator() {
            // Spectators' effects still run (LivingEntity.baseTick).
            if !g.effects.list.is_empty() {
                self.as_guest(host, conn, |game| game.tick_mob_effects());
            }
            return;
        }
        let in_wall = !g.sleeping && self.in_wall(g.position, g.eye_height());
        let at = g.position;
        // ServerGamePacketListenerImpl's flying check: floating (not falling,
        // off the ground, only air around) for more than 80 ticks kicks.
        if !moves.is_empty() {
            let allowed = g.creative()
                || g.fall_flying
                || g.sleeping
                || aot
                || g.effects.get("levitation").is_some();
            let height = g.eye_height() + 0.2;
            let allowed = allowed && !(g.creative() && host.hub.config.survival_only);
            let floating = !allowed
                && moves.iter().all(|(m, on_ground)| !on_ground && m[1] >= -0.03125)
                && self.only_air_around(at, height);
            let g = host.hub.guest_mut(conn).expect("found above");
            g.floating = if floating { g.floating + 1 } else { 0 };
            if g.floating > 80 {
                g.floating = 0;
                log::warn!("Horizon: {} was kicked for flying", g.name);
                host.hub.kick(
                    conn,
                    horizon_protocol::text::translatable("multiplayer.disconnect.flying", &[]),
                );
                return;
            }
        }
        let sounds = self.as_guest(host, conn, |game| {
            let (rules, motion) = (game.world.health_rules, game.motion);
            let health = &motion.health;
            let Some(player) = game.player.as_mut() else {
                return Vec::new();
            };
            let mut w = MotionWorld {
                entities: &[],
                player,
                scene: &mut game.world,
                assets: &game.pack.assets,
                collision_binding: game.collision,
                resources: game.motion,
            };
            w.player.health.advance_clock();
            let _ = w.fluid_base_tick();
            w.player.ground.on_ground = from_ground;
            let travelled: Vec<[f64; 3]> = moves.iter().map(|m| m.0).collect();
            for ((moved, on_ground), boost) in moves.into_iter().zip(boosts) {
                if boost {
                    let fall = &mut w.player.fall_distance;
                    *fall = (*fall - DOUBLE_JUMP_FALL_GRACE).max(0.0);
                }
                if aot {
                    w.player.fall_distance = 0.0;
                }
                let movement = Vec3::new(moved[0], moved[1], moved[2]);
                if w.player.ground.on_ground && !on_ground && moved[1] > 0.0 {
                    let sprinting = w.player.sprinting;
                    w.player
                        .health
                        .food
                        .add_exhaustion(if sprinting { 0.2 } else { 0.05 });
                }
                let landed = (|| -> horizon_client::Result<()> {
                    w.set_on_ground_with_movement(on_ground, false, movement)?;
                    let pos = w.legacy_on_position()?;
                    let state = w.block_state(pos)?;
                    w.check_fall_damage(moved[1], on_ground, state, pos)?;
                    w.check_movement_statistics(movement)
                })();
                if landed.is_err() {
                    break;
                }
            }
            let _ = w.tick_environment_damage();
            let _ = w.apply_fire_contact();
            let _ = w.server_hazards_tick(in_wall);
            let mut events = std::mem::take(&mut w.player.events);
            // ServerGamePacketListenerImpl.handleMovePlayer:
            // Player.checkMovementStatistics for each move.
            for d in travelled {
                game.movement_stats(Vec3::new(d[0], d[1], d[2]));
            }
            // LivingEntity.baseTick ends with tickEffects.
            game.tick_mob_effects();
            let Some(player) = game.player.as_mut() else {
                return Vec::new();
            };
            let _ = player.peaceful_regeneration(rules);
            let _ = player.food_tick(health, rules);
            events.append(&mut player.events);
            // CombatTracker.recordDamage, for the death message.
            game.record_combat(&events);
            events
                .into_iter()
                .filter_map(|e| match e {
                    MotionEvent::HurtSound(sound) => Some(sound),
                    _ => None,
                })
                .collect::<Vec<_>>()
        });
        for sound in sounds.into_iter().flatten() {
            self.player_hurt_sound(sound, at);
        }
    }

    /// `ServerGamePacketListenerImpl.noBlocksAround`: only air in the
    /// player's box grown by 0.0625 and reaching 0.55 lower.
    fn only_air_around(&self, at: [f64; 3], height: f64) -> bool {
        let (half, grow) = (0.3 + 0.0625, 0.0625);
        let range = |lo: f64, hi: f64| lo.floor() as i32..=hi.floor() as i32;
        range(at[0] - half, at[0] + half).all(|x| {
            range(at[1] - grow - 0.55, at[1] + height + grow).all(|y| {
                range(at[2] - half, at[2] + half)
                    .all(|z| self.res.facts.is_air(self.world.block(x, y, z)))
            })
        })
    }
    /// `Player.isInWall`: a suffocating block in the 0.48-wide box at the
    /// eyes. Suffocating is a full collision box that is also a full render
    /// cube (`BlockBehaviour.isSuffocating`'s default; glass and the like
    /// opt out), as the mobs' check has it.
    fn in_wall(&self, at: [f64; 3], eye_height: f64) -> bool {
        let (y, half) = ((at[1] + eye_height).floor() as i32, 0.24);
        let range = |v: f64| (v - half).floor() as i32..=(v + half).floor() as i32;
        range(at[0]).any(|x| {
            range(at[2]).any(|z| {
                let f = self.res.facts.get(self.world.block(x, y, z));
                f.full_collision && f.solid_render
            })
        })
    }

    /// ServerPlayerGameMode.handleBlockBreakAction, STOP_DESTROY_BLOCK: the client finished
    /// digging `pos` (and predicted the break). The server breaks it if its own count of the
    /// digging since START is at least 70 % done; otherwise it keeps digging alone and breaks
    /// it when done (`guest_delayed_destroy`), so the client's broken block is never left
    /// standing on the server.
    fn guest_stop_digging(&mut self, host: &mut LanHost, conn: u32, pos: [i32; 3]) {
        let ticks = host.hub.ticks;
        let play = host.play.entry(conn).or_default();
        let dug = play.digging.take().is_some_and(|(p, _)| p == pos);
        play.mining = None;
        let start = play.dig_start;
        let state = self.world.block(pos[0], pos[1], pos[2]);
        if !dug || self.res.facts.is_air(state) {
            return;
        }
        let progress = self
            .as_guest(host, conn, |g| g.destroy_progress(pos))
            .map_or(0.0, |p| p * (ticks - start + 1) as f32);
        if progress >= 0.7 {
            self.guest_destroy(host, conn, pos);
        } else {
            let play = host.play.entry(conn).or_default();
            if play.delayed.is_none() {
                play.delayed = Some((pos, start));
            }
        }
    }

    /// ServerPlayerGameMode.tick's delayed destroy: the block breaks once its progress since
    /// the digging started is complete, unless something else removed it first.
    fn guest_delayed_destroy(&mut self, host: &mut LanHost, conn: u32) {
        let Some((pos, start)) = host.play.get(&conn).and_then(|p| p.delayed) else {
            return;
        };
        let ticks = host.hub.ticks;
        let state = self.world.block(pos[0], pos[1], pos[2]);
        let progress = if self.res.facts.is_air(state) {
            None
        } else {
            self.as_guest(host, conn, |g| g.destroy_progress(pos))
                .map(|p| p * (ticks - start + 1) as f32)
        };
        match progress {
            Some(p) if p < 1.0 => {}
            done => {
                if let Some(play) = host.play.get_mut(&conn) {
                    play.delayed = None;
                }
                if done.is_some() {
                    self.guest_destroy(host, conn, pos);
                }
            }
        }
    }

    /// ServerPlayerGameMode.destroyBlock for a guest, its crack cleared for the others
    /// (`destroyBlockProgress(id, pos, -1)`).
    fn guest_destroy(&mut self, host: &mut LanHost, conn: u32, pos: [i32; 3]) {
        self.as_guest(host, conn, |g| {
            let _ = g.break_block(pos);
        });
        if let Some(g) = host.hub.guest(conn) {
            host.hub.broadcast_near(
                [f64::from(pos[0]), f64::from(pos[1]), f64::from(pos[2])],
                64.0,
                &C::BlockDestruction(play::BlockDestruction {
                    id: g.entity_id,
                    pos,
                    progress: 255,
                }),
                Some(conn),
            );
        }
    }

    /// The crack animation other players see.
    fn guest_dig_feedback(&mut self, host: &mut LanHost, conn: u32) {
        let Some(gi) = host.hub.guests.iter().position(|g| g.conn == conn) else {
            return;
        };
        let play = host.play.entry(conn).or_default();
        let Some((pos, _)) = play.digging else {
            return;
        };
        let progress = play
            .mining
            .as_ref()
            .map_or(-1, |m| (m.progress.clamp(0.0, 1.0) * 10.0) as i32 - 1);
        let swinging = play.swing.swinging();
        if progress != play.last_progress && progress >= 0 {
            play.last_progress = progress;
            host.hub.broadcast_near(
                [f64::from(pos[0]), f64::from(pos[1]), f64::from(pos[2])],
                64.0,
                &C::BlockDestruction(play::BlockDestruction {
                    id: host.hub.guests[gi].entity_id,
                    pos,
                    progress: progress as u8,
                }),
                Some(conn),
            );
        }
        let _ = swinging;
    }

    /// Dropped items touching a guest go into their inventory.
    fn guest_pickups(&mut self, host: &mut LanHost, gi: usize) {
        let (conn, mode, position, id) = {
            let g = &host.hub.guests[gi];
            (g.conn, g.game_mode, g.position, g.entity_id)
        };
        if mode == SPECTATOR || self.dropped.is_empty() {
            return;
        }
        let near: Vec<usize> = self
            .dropped
            .iter()
            .enumerate()
            .filter(|(_, d)| {
                d.pickup_delay == 0
                    && (d.position.x - position[0]).abs() < 1.5
                    && (d.position.z - position[2]).abs() < 1.5
                    && d.position.y > position[1] - 0.6
                    && d.position.y < position[1] + 2.4
            })
            .map(|(i, _)| i)
            .collect();
        let mut taken = Vec::new();
        for i in near.into_iter().rev() {
            let (stack, item_id) = (self.dropped[i].stack.clone(), self.dropped[i].id);
            let before = stack.count;
            let rest = self.as_guest(host, conn, |g| g.give(stack)).flatten();
            match rest {
                None => {
                    let item = self.dropped.remove(i);
                    self.guest_pickup(host, id, item);
                    taken.push((item_id, before));
                }
                Some(rest) if rest.count < before => {
                    self.guest_pickup(host, id, self.dropped[i].clone());
                    self.dropped[i].stack = rest;
                    taken.push((item_id, before - self.dropped[i].stack.count));
                }
                Some(_) => {}
            }
        }
        for (item, count) in taken {
            host.hub.take_item(item as i32, id, count as i32, position);
            host.hub
                .sound("minecraft:entity.item.pickup", position, 0.2, 1.4, 7, None);
        }
    }

    /// The host's own `ItemPickupParticle` toward the guest's model.
    fn guest_pickup(
        &mut self,
        host: &LanHost,
        id: i32,
        item: horizon_client::entities::ItemEntity,
    ) {
        use super::pickups::{Collector, Taken, guest_target};
        if let Some(target) = guest_target(host, id as u32) {
            self.add_pickup_toward(
                Taken::Item(Box::new(item)),
                Collector::Guest(id as u32),
                target,
            );
        }
    }

    /// Positional sounds and level events of the tick, for the guests.
    pub(super) fn lan_broadcast_effects(&mut self, host: &mut LanHost) {
        let sounds = self.audio.capture.take().unwrap_or_default();
        self.audio.capture = Some(Vec::new());
        let blocks: Vec<[f64; 3]> = std::mem::take(&mut host.broken);
        for (name, at, volume, pitch) in sounds {
            // Block breaks are heard through the level event.
            if blocks
                .iter()
                .any(|b| (0..3).all(|k| (b[k] + 0.5 - at[k]).abs() < 1e-6))
            {
                continue;
            }
            let source = horizon_client::settings::sound_source(&name) as i32;
            host.hub.sound(&name, at, volume, pitch, source, None);
        }
    }

    /// Guests' arm swings for everyone else.
    pub(super) fn lan_swings(&mut self, host: &mut LanHost) {
        let conns: Vec<u32> = host.play.keys().copied().collect();
        for conn in conns {
            let Some(g) = host.hub.guests.iter().find(|g| g.conn == conn) else {
                continue;
            };
            let (id, at) = (g.entity_id, g.position);
            let play = host.play.get_mut(&conn).expect("play");
            let swinging = play.swing.swinging() && play.swing.ticks == 0;
            if swinging && !play.swung {
                host.hub.swing(id, 0, at, Some(conn));
            }
            play.swung = swinging;
        }
    }
}

/// Entity.checkFallDamage over the moves a guest sent since the server's
/// last tick: whether one of them landed (the fall starts over) and how far
/// the guest has fallen since.
fn pending_fall(g: &horizon_client::lan::guest::Guest) -> (bool, f64) {
    let (mut landed, mut fallen) = (false, 0.0);
    for (moved, on_ground) in &g.moves {
        if *on_ground {
            landed = true;
            fallen = 0.0;
        } else if moved[1] < 0.0 {
            fallen -= moved[1];
        }
    }
    (landed, fallen)
}

/// The rise in one move (blocks) that turns a mid-air fall into a double
/// jump. The mod's lift builds up over ticks, so from a fall the first rising
/// move is small; vanilla has no other way up in mid-air that keeps the fall
/// (climbing, water and landing reset it).
const DOUBLE_JUMP_RISE: f64 = 0.001;
/// Blocks of fall a double jump forgives: vanilla's safe fall height.
const DOUBLE_JUMP_FALL_GRACE: f64 = 3.0;
