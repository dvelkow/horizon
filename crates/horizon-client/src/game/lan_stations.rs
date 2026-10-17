//! Workstations for LAN guests: the enchanting table, anvil, grindstone,
//! smithing table, stonecutter, loom and cartography table a guest opens run
//! the host's menu engine (`horizon_client::menu`, `menu::station`) over the
//! guest's inventory, with vanilla's packets: OpenScreen, container clicks,
//! the button click (`clickMenuButton`) and rename packets, the data slots
//! (ContainerSetData) and the inputs back into the inventory when the menu
//! closes (`clearContainer`). The brewing stand's clicks run on the engine's
//! `BrewingStandMenu` over its block entity, like the furnace's.
use super::{
    Game,
    lan::LanHost,
    lan_crafting::{input, load_player, store_player},
    stations::Kind as StationKind,
};
use horizon_client::{
    containers::BlockEntity,
    items::ItemStack,
    lan::{guest::Guest, menu::Slots},
    menu::{Effect, Env, Kind, Menu, Stations},
    stations,
};
use horizon_core::random::{LegacyRandom, RandomSource};
use horizon_protocol::packets::play::{self, Clientbound as C};

/// A guest's open workstation.
pub(super) struct GuestStation {
    window: i32,
    kind: StationKind,
    pos: [i32; 3],
    state: i32,
    /// Kept between clicks, so a drag (quick craft) spans them.
    menu: Box<Menu>,
    /// The data slots the client was last sent (`remoteDataSlots`).
    sent_data: Vec<i32>,
    /// The tick of the last take sound (`lastSoundTime`).
    last_sound: u64,
}

/// The guest's workstation and brewing stand menus.
#[derive(Default)]
pub(super) struct GuestStations {
    open: Option<GuestStation>,
    /// The open brewing stand's window and menu; its slots live in the block
    /// entity and the hub's container menu keeps the client in sync.
    brewing: Option<(i32, Box<Menu>)>,
}

/// The station's menu, `MenuType` and title; None for those guests don't get.
fn menu_type(kind: StationKind) -> Option<(Kind, &'static str, &'static str)> {
    Some(match kind {
        StationKind::Enchanting => (Kind::Enchantment, "minecraft:enchantment", "container.enchant"),
        StationKind::Anvil => (Kind::Anvil, "minecraft:anvil", "container.repair"),
        StationKind::Grindstone => (
            Kind::Grindstone,
            "minecraft:grindstone",
            "container.grindstone_title",
        ),
        StationKind::Smithing => (Kind::Smithing, "minecraft:smithing", "container.upgrade"),
        StationKind::Stonecutter => (
            Kind::Stonecutter,
            "minecraft:stonecutter",
            "container.stonecutter",
        ),
        StationKind::Loom => (Kind::Loom, "minecraft:loom", "container.loom"),
        StationKind::Cartography => (
            Kind::Cartography,
            "minecraft:cartography_table",
            "container.cartography_table",
        ),
        StationKind::Merchant | StationKind::Beacon => return None,
    })
}

/// `Player.giveExperienceLevels` for a guest.
fn give_levels(g: &mut Guest, levels: i32) {
    g.xp_level = g.xp_level.saturating_add(levels);
    if g.xp_level < 0 {
        g.xp_level = 0;
        g.xp_progress = 0.0;
        g.xp_total = 0;
    }
}

/// `Player.canInteractWithBlock(pos, 4.0)`: the block's box within the
/// reach plus four from the eyes.
fn within_reach(g: &Guest, pos: [i32; 3]) -> bool {
    let eye = [
        g.position[0],
        g.position[1] + g.eye_height(),
        g.position[2],
    ];
    let reach = if g.creative() { 5.0 } else { 4.5 } + 4.0;
    let d2: f64 = (0..3)
        .map(|k| {
            let low = f64::from(pos[k]);
            (eye[k] - eye[k].clamp(low, low + 1.0)).powi(2)
        })
        .sum();
    d2 < reach * reach
}

fn centre(pos: [i32; 3]) -> [f64; 3] {
    [
        f64::from(pos[0]) + 0.5,
        f64::from(pos[1]) + 0.5,
        f64::from(pos[2]) + 0.5,
    ]
}

impl Game<'_> {
    /// The guest opened a workstation (`getMenuProvider`, `ServerPlayer.openMenu`).
    pub(super) fn lan_open_station(
        &mut self,
        host: &mut LanHost,
        conn: u32,
        kind: StationKind,
        pos: [i32; 3],
    ) {
        let Some((menu_kind, menu_type, title)) = menu_type(kind) else {
            return;
        };
        let Some(gi) = host.hub.guests.iter().position(|g| g.conn == conn) else {
            return;
        };
        // ServerPlayer.openMenu closes the menu that was open.
        self.lan_crafting_closed(host, conn, None);
        self.lan_merchant_closed(host, conn, None);
        self.lan_station_closed(host, conn, None);
        self.guest_container_closed(host, conn);
        // A named enchanting table shows its name (Nameable.getDisplayName).
        let title = match self.world.block_names.get(&(pos[0], pos[1], pos[2])) {
            Some(name) => horizon_protocol::text::literal(name),
            None => horizon_protocol::text::translatable(title, &[]),
        };
        let Some(window) = host.hub.open_window(conn, menu_type, title) else {
            return;
        };
        let mut menu = Box::new(Menu::new(menu_kind));
        if menu_kind == Kind::Enchantment {
            // Player.enchantmentSeed: a player new here draws theirs.
            let g = &mut host.hub.guests[gi];
            if g.xp_seed == 0 {
                g.xp_seed = self.world.random.next_int();
            }
            menu.data[3] = g.xp_seed;
        }
        let mut station = GuestStation {
            window,
            kind,
            pos,
            state: 0,
            menu,
            sent_data: Vec::new(),
            last_sound: 0,
        };
        self.lan_station_send(host, conn, &mut station);
        host.play.entry(conn).or_default().stations.open = Some(station);
    }

    /// The guest's open workstation, taken out (window `window`, any when None).
    fn take_station(
        host: &mut LanHost,
        conn: u32,
        window: Option<i32>,
    ) -> Option<GuestStation> {
        host.play
            .get_mut(&conn)?
            .stations
            .open
            .take_if(|s| window.is_none_or(|w| s.window == w))
    }

    /// Runs `op` on the station with the guest's inventory, levels and seed
    /// copied in and back, then carries out what it did (levels, sounds, the
    /// anvil's wear, experience, new maps) and drops what it threw out.
    fn station_run<R>(
        &mut self,
        host: &mut LanHost,
        conn: u32,
        st: &mut GuestStation,
        op: impl FnOnce(&mut Menu, &Env) -> R,
    ) -> Option<R> {
        let gi = host.hub.guests.iter().position(|g| g.conn == conn)?;
        if st.kind == StationKind::Cartography {
            self.load_guest_maps(&host.hub.guests[gi], &st.menu);
        }
        let bookshelves = match st.kind {
            StationKind::Enchanting => self.count_bookshelves(st.pos),
            _ => 0,
        };
        // Player.onEnchantmentPerformed's draw, should an enchantment happen.
        let next_seed = match st.kind {
            StationKind::Enchanting => self.world.random.next_int(),
            _ => 0,
        };
        let result = {
            let hub = &host.hub;
            let names = |item: &str| self.pack.names.item(item);
            let ids = |e: &str| hub.registry_id("minecraft:enchantment", e);
            let maps = |id: i32| self.maps.data.get(&id).map(|m| (m.locked, m.scale));
            let stations = Stations {
                data: &self.res.stations,
                item_name: &names,
                enchantment_id: &ids,
                map_state: &maps,
            };
            let env = Env {
                items: &self.res.items,
                recipes: &self.res.recipes,
                brewing: &self.res.stations.brewing,
                banners: &self.res.stations.banners,
                trade: None,
                stations: Some(&stations),
            };
            let g = &hub.guests[gi];
            let menu = &mut st.menu;
            load_player(menu, g);
            menu.station.levels = g.xp_level;
            menu.station.bookshelves = bookshelves;
            menu.station.next_seed = next_seed;
            op(menu, &env)
        };
        let menu = &mut st.menu;
        let mut dropped = std::mem::take(&mut menu.dropped);
        let effects = std::mem::take(&mut menu.station.effects);
        let sounds = std::mem::take(&mut menu.sounds);
        menu.drops.clear();
        menu.crafted.clear();
        let g = &mut host.hub.guests[gi];
        store_player(menu, g);
        if st.kind == StationKind::Enchanting {
            g.xp_seed = menu.data[3];
        }
        // MapItem.onCraftedPostProcess for the maps the cartography table made.
        if st.kind == StationKind::Cartography {
            let g = &mut host.hub.guests[gi];
            let held = g.inventory.iter_mut().chain([&mut g.carried, &mut g.offhand]);
            for stack in held.flatten().chain(dropped.iter_mut()) {
                self.post_process_map(stack);
            }
        }
        self.drop_guest_items(host, gi, dropped);
        let at = centre(st.pos);
        if !sounds.is_empty() && st.last_sound != self.ticks {
            st.last_sound = self.ticks;
            for sound in sounds {
                self.audio.play(sound, Some(at), 1.0, 1.0);
            }
        }
        for effect in effects {
            self.station_effect(host, conn, st, effect);
        }
        Some(result)
    }

    /// What a take or button did to the world besides the menu.
    fn station_effect(&mut self, host: &mut LanHost, conn: u32, st: &GuestStation, effect: Effect) {
        let Some(gi) = host.hub.guests.iter().position(|g| g.conn == conn) else {
            return;
        };
        let (pos, at) = (st.pos, centre(st.pos));
        let pitch = |game: &mut Self| game.world.random.next_float() * 0.1 + 0.9;
        match effect {
            Effect::Enchanted { levels } => {
                // Player.onEnchantmentPerformed (the seed came back with the menu).
                give_levels(&mut host.hub.guests[gi], -levels);
                let item = st.menu.slot(0).cloned();
                self.as_guest(host, conn, |g| {
                    g.stats.custom("enchant_item", 1);
                    if let Some(item) = &item {
                        g.trigger_item("minecraft:enchanted_item", item);
                    }
                });
                let pitch = pitch(self);
                self.audio
                    .play("minecraft:block.enchantment_table.use", Some(at), 1.0, pitch);
            }
            Effect::AnvilUsed { levels } => {
                give_levels(&mut host.hub.guests[gi], -levels);
                let creative = host.hub.guests[gi].creative();
                let pitch = pitch(self);
                if creative || self.world.random.next_float() >= 0.12 {
                    self.audio.play("minecraft:block.anvil.use", Some(at), 1.0, pitch);
                    return;
                }
                // AnvilBlock.damage: chipped, damaged, then gone.
                let state = self.world.block(pos[0], pos[1], pos[2]);
                let next = match &*self.res.facts.get(state).name {
                    "minecraft:anvil" => Some("minecraft:chipped_anvil"),
                    "minecraft:chipped_anvil" => Some("minecraft:damaged_anvil"),
                    _ => None,
                };
                match next {
                    Some(next) => {
                        let _ = self.set_state_named(pos, next, &["facing"]);
                        self.audio.play("minecraft:block.anvil.use", Some(at), 1.0, pitch);
                    }
                    None => {
                        // The menu closes with it (lan_stations_tick).
                        let _ = self.world.set(pos[0], pos[1], pos[2], self.world.air);
                        self.world.modified.insert((pos[0] >> 4, pos[2] >> 4));
                        self.audio
                            .play("minecraft:block.anvil.destroy", Some(at), 1.0, pitch);
                    }
                }
            }
            Effect::Ground(a, b) => {
                let mut random = LegacyRandom::new(self.world.random.next_long());
                let xp = stations::grindstone_experience(
                    &self.res.stations.enchantments,
                    &mut random,
                    a.as_ref(),
                    b.as_ref(),
                );
                if xp > 0 {
                    self.spawn_orbs(at, xp);
                }
                let pitch = pitch(self);
                self.audio
                    .play("minecraft:block.grindstone.use", Some(at), 1.0, pitch);
            }
            Effect::Smithed => {
                let pitch = pitch(self);
                self.audio
                    .play("minecraft:block.smithing_table.use", Some(at), 1.0, pitch);
            }
        }
    }

    /// The maps a cartography click may look at, loaded (`MapItem.getSavedData`).
    fn load_guest_maps(&mut self, g: &Guest, menu: &Menu) {
        let ids: Vec<i32> = g
            .inventory
            .iter()
            .chain([&g.carried, &g.offhand])
            .flatten()
            .chain(menu.slot(0))
            .filter_map(|s| s.data().and_then(|d| d.map_id))
            .collect();
        for id in ids {
            let _ = self.map(id);
        }
    }

    /// `MapItem.onCraftedPostProcess`: a zoomed out (or locked) copy of the
    /// map under a new id.
    fn post_process_map(&mut self, stack: &mut ItemStack) {
        let Some(post) = stack
            .extra
            .as_mut()
            .and_then(|d| d.map_post_processing.take())
        else {
            return;
        };
        stack.tidy();
        let Some(id) = stack.data().and_then(|d| d.map_id) else {
            return;
        };
        let made = self
            .map(id)
            .map(|m| if post == 0 { m.locked_copy() } else { m.scaled() });
        if let Some(data) = made {
            let new = self.new_map_id(data);
            stack.data_mut().map_id = Some(new);
        }
    }

    /// The whole station menu for the client (`sendAllDataToRemote`): the
    /// slots and carried stack, then the data slots that changed.
    fn lan_station_send(&mut self, host: &mut LanHost, conn: u32, st: &mut GuestStation) {
        let Some(gi) = host.hub.guests.iter().position(|g| g.conn == conn) else {
            return;
        };
        st.state = (st.state + 1) & 0x7FFF;
        let own = st.menu.slot_count() - 36;
        let mut items: Vec<Option<ItemStack>> = (0..own).map(|s| st.menu.slot(s).cloned()).collect();
        let g = &host.hub.guests[gi];
        items.extend((9..36).chain(0..9).map(|i| g.inventory[i].clone()));
        let carried = g.carried.clone();
        let items = items.iter().map(|s| host.hub.stack(s)).collect();
        let carried = host.hub.stack(&carried);
        host.hub.send_to(
            conn,
            &C::ContainerSetContent(play::ContainerSetContent {
                container: st.window,
                state: st.state,
                items,
                carried,
            }),
        );
        st.sent_data.resize(st.menu.data.len(), i32::MIN);
        for (id, &value) in st.menu.data.iter().enumerate() {
            if st.sent_data[id] != value {
                st.sent_data[id] = value;
                host.hub.send_to(
                    conn,
                    &C::ContainerSetData(play::ContainerSetData {
                        container: st.window,
                        id: id as i16,
                        value: value as i16,
                    }),
                );
            }
        }
    }

    /// Runs `op` on the guest's station `window`, then syncs the client.
    /// False when no such window is open.
    fn lan_station_op(
        &mut self,
        host: &mut LanHost,
        conn: u32,
        window: Option<i32>,
        op: impl FnOnce(&mut Menu, &Env),
    ) -> bool {
        let Some(mut st) = Self::take_station(host, conn, window) else {
            return false;
        };
        self.station_run(host, conn, &mut st, op);
        self.lan_station_send(host, conn, &mut st);
        host.play.entry(conn).or_default().stations.open = Some(st);
        true
    }

    /// A container click on the guest's workstation; false for other windows.
    pub(super) fn lan_station_click(
        &mut self,
        host: &mut LanHost,
        conn: u32,
        p: &play::ContainerClick,
    ) -> bool {
        if p.container == 0 {
            return false;
        }
        // A spectator's click changes nothing; the client is resynchronised.
        let spectator = host.hub.guest(conn).is_none_or(|g| g.spectator());
        let (slot, button) = (i32::from(p.slot), i32::from(p.button));
        self.lan_station_op(host, conn, Some(p.container), |menu, env| {
            if !spectator {
                let _ = menu.click(env, slot, button, input(p.input));
            }
        })
    }

    /// ServerboundContainerButtonClickPacket (`clickMenuButton`): an
    /// enchantment, a stonecutter recipe, a loom pattern.
    pub(super) fn lan_station_button(
        &mut self,
        host: &mut LanHost,
        conn: u32,
        window: i32,
        button: i32,
    ) {
        if host.hub.guest(conn).is_none_or(|g| g.spectator()) {
            return;
        }
        self.lan_station_op(host, conn, Some(window), |menu, env| {
            menu.button(env, button);
        });
    }

    /// ServerboundRenameItemPacket: the open anvil's `setItemName`.
    pub(super) fn lan_station_rename(&mut self, host: &mut LanHost, conn: u32, name: &str) {
        let anvil = host
            .play
            .get(&conn)
            .and_then(|p| p.stations.open.as_ref())
            .is_some_and(|s| s.kind == StationKind::Anvil);
        if anvil {
            self.lan_station_op(host, conn, None, |menu, env| {
                menu.rename(env, name);
            });
        }
    }

    /// The guest's workstation closes (`AbstractContainerMenu.removed`,
    /// `clearContainer`): the carried stack and the inputs go back to the
    /// inventory, what doesn't fit is dropped. False when no such window is open.
    pub(super) fn lan_station_closed(
        &mut self,
        host: &mut LanHost,
        conn: u32,
        window: Option<i32>,
    ) -> bool {
        let Some(mut st) = Self::take_station(host, conn, window) else {
            return false;
        };
        self.station_run(host, conn, &mut st, |menu, env| menu.station_close(env));
        true
    }

    /// `ServerPlayer.tick`'s `stillValid` for the guests' workstations: the
    /// block must still be there and within reach, else the menu closes.
    pub(super) fn lan_stations_tick(&mut self, host: &mut LanHost) {
        let open: Vec<(u32, StationKind, [i32; 3], i32)> = host
            .play
            .iter()
            .filter_map(|(conn, p)| {
                let s = p.stations.open.as_ref()?;
                Some((*conn, s.kind, s.pos, s.window))
            })
            .collect();
        for (conn, kind, pos, window) in open {
            let Some(g) = host.hub.guest(conn) else {
                continue;
            };
            let state = self.world.block(pos[0], pos[1], pos[2]);
            let name = &self.res.facts.get(state).name;
            if StationKind::from_block(name) == Some(kind) && within_reach(g, pos) {
                continue;
            }
            // ServerPlayer.closeContainer.
            host.hub.send_to(
                conn,
                &C::ContainerClose(play::ContainerClose { container: window }),
            );
            self.lan_station_closed(host, conn, Some(window));
        }
    }

    /// A guest left: what their workstation held goes back into their
    /// inventory before it is saved (dropped where they stood if full).
    pub(super) fn lan_stations_left(&mut self, host: &mut LanHost, g: &mut Guest) {
        let Some(mut st) = host
            .play
            .get_mut(&g.conn)
            .and_then(|p| p.stations.open.take())
        else {
            return;
        };
        let env = Env {
            items: &self.res.items,
            recipes: &self.res.recipes,
            brewing: &self.res.stations.brewing,
            banners: &self.res.stations.banners,
            trade: None,
            stations: None,
        };
        let menu = &mut st.menu;
        load_player(menu, g);
        menu.station_close(&env);
        let dropped = std::mem::take(&mut menu.dropped);
        store_player(menu, g);
        for stack in dropped {
            self.spawn_item(stack, g.position, [0.0, 0.1, 0.0], 40);
        }
    }

    /// A click on the guest's open brewing stand: the host's
    /// `BrewingStandMenu` (one bottle per slot, ingredients, blaze powder,
    /// quick moves by kind) over the block entity's slots. False for other windows.
    pub(super) fn lan_brewing_click(
        &mut self,
        host: &mut LanHost,
        conn: u32,
        p: &play::ContainerClick,
    ) -> bool {
        let Some(gi) = host.hub.guests.iter().position(|g| g.conn == conn) else {
            return false;
        };
        let Some(pos) = host.hub.guests[gi]
            .menu
            .as_ref()
            .filter(|m| p.container != 0 && m.window == p.container)
            .and_then(|m| match m.slots {
                Slots::Block(pos) => Some(pos),
                _ => None,
            })
        else {
            return false;
        };
        let Some(BlockEntity::Brewing(b)) = self.world.block_entities.get(&pos) else {
            return false;
        };
        let items = b.items.clone();
        if host.hub.guests[gi].spectator() {
            return true;
        }
        let mut menu = host
            .play
            .entry(conn)
            .or_default()
            .stations
            .brewing
            .take()
            .filter(|(window, _)| *window == p.container)
            .map_or_else(|| Box::new(Menu::new(Kind::Brewing)), |(_, m)| m);
        let changed = self.guest_menu_op(host, gi, &mut menu, |menu, env| {
            for (k, s) in items.iter().enumerate() {
                menu.set_slot(env, k, s.clone());
            }
            let before: Vec<_> = (0..menu.slot_count())
                .map(|s| menu.slot(s).cloned())
                .collect();
            let _ = menu.click(env, i32::from(p.slot), i32::from(p.button), input(p.input));
            (0..menu.slot_count())
                .filter(|&s| menu.slot(s) != before[s].as_ref())
                .collect::<Vec<_>>()
        });
        if let Some(BlockEntity::Brewing(b)) = self.world.block_entities.get_mut(&pos) {
            for (k, slot) in b.items.iter_mut().enumerate() {
                *slot = menu.slot(k).cloned();
            }
            self.world.modified.insert((pos.0 >> 4, pos.2 >> 4));
        }
        host.play.entry(conn).or_default().stations.brewing = Some((p.container, menu));
        host.hub.inventory_clicked(conn, p, &self.world, &changed);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_guest_station_has_a_vanilla_menu_type() {
        for (kind, menu) in [
            (StationKind::Enchanting, "minecraft:enchantment"),
            (StationKind::Anvil, "minecraft:anvil"),
            (StationKind::Grindstone, "minecraft:grindstone"),
            (StationKind::Smithing, "minecraft:smithing"),
            (StationKind::Stonecutter, "minecraft:stonecutter"),
            (StationKind::Loom, "minecraft:loom"),
            (StationKind::Cartography, "minecraft:cartography_table"),
        ] {
            let (engine, name, _) = menu_type(kind).unwrap();
            assert_eq!(name, menu);
            // The engine's menu has the station's slots before the 36 of the inventory.
            let own = Menu::new(engine).slot_count() - 36;
            assert_eq!(own, super::super::remote_stations::own_slots(kind));
        }
        assert!(menu_type(StationKind::Beacon).is_none());
    }
}
