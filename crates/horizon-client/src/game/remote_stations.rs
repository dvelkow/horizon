//! A server's workstations on the client: the enchanting table, anvil,
//! grindstone, smithing table, stonecutter, loom and cartography table
//! screens (game/stations.rs, game/loom.rs) over the server's menu. Its slots
//! come in the container packets and its data slots (the enchantment offers,
//! the anvil's cost, the selected recipe or pattern) in
//! ClientboundContainerSetDataPacket; clicks, the buttons
//! (ServerboundContainerButtonClickPacket) and the anvil's name
//! (ServerboundRenameItemPacket) go to the server, which owns the menu.
use super::{
    Game, Screen,
    stations::{INPUT, Kind, RESULT, Station, enchant_button, inside},
};
use horizon_client::{item_data::Levels, items::ItemStack, stations::hover_name};
use horizon_protocol::{
    item::{Component, Stack, component},
    packets::play::{self, Serverbound as S},
    text::plain,
};
use std::sync::Arc;

/// What the client knows of a server's workstation besides its slots.
pub(super) struct ServerStation {
    /// The data slots as the server last sent them.
    data: Vec<i32>,
    /// `AnvilMenu.itemName` as the client last sent it (`setItemName`).
    name: Option<String>,
}
impl ServerStation {
    fn new(kind: Kind) -> Self {
        let data = match kind {
            // EnchantmentMenu: the costs, the seed, then the clues (-1: none).
            Kind::Enchanting => {
                let mut d = vec![0; 10];
                d[4..].fill(-1);
                d
            }
            Kind::Anvil | Kind::Smithing | Kind::Stonecutter | Kind::Loom => vec![0],
            _ => Vec::new(),
        };
        Self { data, name: None }
    }
    /// `EnchantmentMenu.getEnchantmentSeed`: the low 16 bits the packet carries.
    pub(super) fn enchantment_seed(&self) -> i32 {
        self.data.get(3).copied().unwrap_or(0)
    }
}

/// More slot numbers than any menu has (a double chest's 54 and the 36 of
/// the inventory).
pub(super) const MENU_SLOTS: i32 = 128;

/// The station a server's `MenuType` shows.
pub(super) fn station_kind(menu: &str) -> Option<Kind> {
    Some(match menu {
        "minecraft:enchantment" => Kind::Enchanting,
        "minecraft:anvil" => Kind::Anvil,
        "minecraft:grindstone" => Kind::Grindstone,
        "minecraft:smithing" => Kind::Smithing,
        "minecraft:stonecutter" => Kind::Stonecutter,
        "minecraft:loom" => Kind::Loom,
        "minecraft:cartography_table" => Kind::Cartography,
        _ => return None,
    })
}

/// The station's own slots in its menu: the inputs, then the result (the
/// enchanting table has none).
pub(super) fn own_slots(kind: Kind) -> usize {
    kind.inputs() + usize::from(kind.result_pos().is_some())
}

/// A workstation menu's slot as the game's slot id: the inputs, the result,
/// then the 27 + 9 inventory slots.
pub(super) fn station_slot(kind: Kind, slot: usize) -> Option<usize> {
    let own = own_slots(kind);
    match slot {
        _ if slot < kind.inputs() => Some(INPUT + slot),
        _ if slot < own => Some(RESULT),
        _ if slot < own + 27 => Some(9 + slot - own),
        _ if slot < own + 36 => Some(slot - own - 27),
        _ => None,
    }
}

/// A container menu's slot (`generic_3x3`, `hopper`, `brewing_stand`): its
/// `size` slots from 300, then the inventory.
pub(super) fn container_slot(size: usize, slot: usize) -> Option<usize> {
    match slot {
        _ if slot < size => Some(300 + slot),
        _ if slot < size + 27 => Some(9 + slot - size),
        _ if slot < size + 36 => Some(slot - size - 27),
        _ => None,
    }
}

/// What a server's stack carries that the workstation screens show: its
/// enchantments, stored enchantments, custom name and repair cost.
pub(super) fn stack_components(
    stack: &Stack,
    out: &mut ItemStack,
    enchantment: &dyn Fn(i32) -> Option<Arc<str>>,
) {
    let levels = |kind: i32| -> Levels {
        match stack.component(kind) {
            Some(Component::Enchantments(list)) => list
                .iter()
                .filter_map(|&(id, level)| Some((enchantment(id)?, u32::try_from(level).ok()?)))
                .collect(),
            _ => Vec::new(),
        }
    };
    let enchantments = levels(component::ENCHANTMENTS);
    let stored = levels(component::STORED_ENCHANTMENTS);
    if !enchantments.is_empty() {
        out.data_mut().enchantments = enchantments;
    }
    if !stored.is_empty() {
        out.data_mut().stored_enchantments = stored;
    }
    if let Some(Component::Text(name)) = stack.component(component::CUSTOM_NAME) {
        out.data_mut().custom_name = Some(plain(name, &Default::default()));
    }
    if let Some(Component::Int(cost)) = stack.component(component::REPAIR_COST) {
        out.data_mut().repair_cost = u32::try_from(*cost).unwrap_or(0);
    }
    out.tidy();
}

/// `StringUtil.filterText`, then `AnvilMenu.validateName`'s 50 characters.
fn valid_name(text: &str) -> Option<String> {
    let name: String = text
        .chars()
        .filter(|&c| c != '§' && c >= ' ' && c != '\u{7f}')
        .collect();
    (name.encode_utf16().count() <= 50).then_some(name)
}

impl Game<'_> {
    /// OpenScreen of a workstation's menu type: its screen, filled as the
    /// server's packets come.
    pub(super) fn remote_open_station(&mut self, kind: Kind, target: Option<[i32; 3]>) {
        let mut s = Station::new_kind(kind, target.unwrap_or_default());
        s.server = Some(ServerStation::new(kind));
        self.station = Some(s);
        self.container = None;
        self.craft_size = 2;
        self.screen = Screen::Inventory;
        self.capture(false);
        self.right = false;
    }

    /// A data slot of the server's workstation (`AbstractContainerMenu.setData`).
    pub(super) fn remote_station_data(&mut self, id: usize, value: i32) {
        let Some(s) = self.station.as_mut() else {
            return;
        };
        let Some(slot) = s.server.as_mut().and_then(|v| v.data.get_mut(id)) else {
            return;
        };
        *slot = value;
        match s.kind {
            Kind::Anvil => s.anvil.cost = value,
            Kind::Stonecutter => s.selected = usize::try_from(value).ok(),
            Kind::Loom => s.loom.selected = usize::try_from(value).ok(),
            Kind::Enchanting => {
                let data = s.server.as_ref().map(|v| v.data.clone()).unwrap_or_default();
                // The clues: an enchantment's network id and its level.
                let clues: [Option<(Arc<str>, u32)>; 3] = std::array::from_fn(|i| {
                    let level = u32::try_from(data[7 + i]).ok()?;
                    Some((self.remote_enchantment(data[4 + i])?, level))
                });
                if let Some(s) = self.station.as_mut() {
                    s.offers.costs = [data[0], data[1], data[2]];
                    s.offers.clues = clues;
                }
            }
            _ => {}
        }
    }

    /// The server set a slot of its workstation: what the client's menu and
    /// screen work out themselves follows (`slotsChanged`,
    /// `AnvilScreen.slotChanged`, `LoomScreen.containerChanged`).
    pub(super) fn remote_station_slot(&mut self, internal: usize) {
        let Some(kind) = self
            .station
            .as_ref()
            .filter(|s| s.server.is_some())
            .map(|s| s.kind)
        else {
            return;
        };
        match kind {
            Kind::Stonecutter if internal == INPUT => {
                // StonecutterMenu.setupRecipeList for a new input item.
                let Some(s) = self.station.as_mut() else {
                    return;
                };
                let item = s.inputs[0].as_ref().map(|i| i.item.clone());
                if item == s.input_item {
                    return;
                }
                s.input_item = item;
                s.selected = None;
                s.start = 0;
                s.recipes = match &s.inputs[0] {
                    Some(input) => self
                        .res
                        .stations
                        .recipes
                        .stonecutting
                        .iter()
                        .enumerate()
                        .filter(|(_, r)| r.input.test(input))
                        .map(|(i, _)| i)
                        .collect(),
                    None => Vec::new(),
                };
            }
            Kind::Anvil if internal == INPUT => {
                let Some(s) = self.station.as_ref() else {
                    return;
                };
                if s.inputs[0] == s.anvil_last {
                    return;
                }
                // The box shows the new input's name, and onNameChanged follows.
                let input = s.inputs[0].clone();
                let name = input
                    .as_ref()
                    .map(|i| hover_name(&|id| self.pack.names.item(id), i))
                    .unwrap_or_default();
                if let Some(s) = self.station.as_mut() {
                    s.anvil_last = input;
                    s.name = name;
                }
                self.remote_anvil_name();
            }
            Kind::Loom if (INPUT..RESULT).contains(&internal) => {
                // LoomMenu.slotsChanged: the patterns a banner and a dye allow.
                let Some(s) = self.station.as_mut() else {
                    return;
                };
                if s.inputs[0].is_some() && s.inputs[1].is_some() {
                    s.loom.patterns = self.res.stations.banners.selectable(s.inputs[2].as_ref());
                } else {
                    s.loom.patterns.clear();
                    s.loom.selected = None;
                }
                self.loom_screen_changed();
            }
            _ => {}
        }
    }

    /// The screen changed a server's workstation itself: the anvil's name box.
    pub(super) fn remote_station_changed(&mut self) {
        if self.station.as_ref().is_some_and(|s| s.kind == Kind::Anvil) {
            self.remote_anvil_name();
        }
    }

    /// `AnvilScreen.onNameChanged`: the box's text, or nothing when it is the
    /// item's own name, to the server when `setItemName` takes it.
    fn remote_anvil_name(&mut self) {
        let Some(s) = self.station.as_ref() else {
            return;
        };
        let Some(input) = s.inputs[0].as_ref() else {
            return;
        };
        let own = input.custom_name().is_none() && s.name == self.pack.names.item(&input.item);
        let Some(name) = valid_name(if own { "" } else { &s.name }) else {
            return;
        };
        let Some(server) = self.station.as_mut().and_then(|s| s.server.as_mut()) else {
            return;
        };
        if server.name.as_ref() == Some(&name) {
            return;
        }
        server.name = Some(name.clone());
        self.send(S::RenameItem(play::RenameItem { name }));
    }

    /// `handleInventoryButtonClick` on the server's open menu.
    fn remote_button(&mut self, button: i32) {
        if let Some((container, _, _)) = self.remote_menu() {
            self.send(S::ContainerButtonClick(play::ContainerButtonClick {
                container,
                button,
            }));
        }
    }

    /// A left click on a server's workstation screen: an enchantment offer
    /// or a stonecutter recipe (`clickMenuButton` on the client, then the
    /// button packet); the loom's patterns go through `loom_click`. False
    /// when no button was hit, so the slots get the click.
    pub(super) fn remote_station_click(&mut self, local: (f32, f32)) -> bool {
        let Some(s) = self.station.as_ref() else {
            return false;
        };
        match s.kind {
            Kind::Enchanting => {
                let Some(i) = (0..3).find(|&i| {
                    let (x, y, w, h) = enchant_button(i);
                    inside(local, x, y, w, h)
                }) else {
                    return false;
                };
                // EnchantmentMenu.clickMenuButton's checks (the client's menu
                // has no level access, so they are all it does).
                let (needed, cost) = (i as i32 + 1, s.offers.costs[i]);
                let lapis = s.inputs[1].as_ref().map_or(0, |l| l.count as i32);
                let level = self.experience.level;
                let creative = self.creative;
                if (lapis >= needed || creative)
                    && cost > 0
                    && s.inputs[0].is_some()
                    && (level >= needed && level >= cost || creative)
                {
                    self.remote_button(i as i32);
                }
                true
            }
            Kind::Stonecutter if s.inputs[0].is_some() => {
                let Some(index) = (s.start..(s.start + 12).min(s.recipes.len())).find(|index| {
                    let p = index - s.start;
                    let (x, y) = (52.0 + (p % 4) as f32 * 16.0, 14.0 + (p / 4) as f32 * 18.0);
                    inside(local, x, y, 16.0, 18.0)
                }) else {
                    return false;
                };
                // StonecutterMenu.clickMenuButton: the selected recipe again does nothing.
                if s.selected != Some(index) {
                    if let Some(s) = self.station.as_mut() {
                        s.selected = Some(index);
                    }
                    self.audio
                        .play("minecraft:ui.stonecutter.select_recipe", None, 1.0, 1.0);
                    self.remote_button(index as i32);
                }
                true
            }
            Kind::Loom => self.loom_click(local),
            _ => false,
        }
    }

    /// `LoomScreen.mouseClicked` on a server's loom: a shown pattern is
    /// selected (`clickMenuButton`) and the button sent.
    pub(super) fn remote_loom_button(&mut self, index: usize) -> bool {
        let Some(s) = self
            .station
            .as_mut()
            .filter(|s| index < s.loom.patterns.len())
        else {
            return false;
        };
        s.loom.selected = Some(index);
        self.audio
            .play("minecraft:ui.loom.select_pattern", None, 1.0, 1.0);
        self.remote_button(index as i32);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workstation_menu_slots_map_to_distinct_game_slots() {
        for menu in [
            "minecraft:enchantment",
            "minecraft:anvil",
            "minecraft:grindstone",
            "minecraft:smithing",
            "minecraft:stonecutter",
            "minecraft:loom",
            "minecraft:cartography_table",
        ] {
            let kind = station_kind(menu).unwrap();
            let own = own_slots(kind);
            let slots: Vec<usize> = (0..own + 36)
                .map(|s| station_slot(kind, s).unwrap())
                .collect();
            let mut unique = slots.clone();
            unique.sort_unstable();
            unique.dedup();
            assert_eq!(unique.len(), slots.len(), "{menu}");
            assert_eq!(station_slot(kind, own + 36), None);
            // The first inventory slot after the station's, the hotbar last.
            assert_eq!(station_slot(kind, own), Some(9));
            assert_eq!(station_slot(kind, own + 35), Some(8));
        }
        // AnvilMenu: input, addition, result; the enchanting table has no result.
        assert_eq!(station_slot(Kind::Anvil, 2), Some(RESULT));
        assert_eq!(station_slot(Kind::Enchanting, 2), Some(9));
        assert_eq!(station_slot(Kind::Smithing, 3), Some(RESULT));
        assert_eq!(station_slot(Kind::Stonecutter, 1), Some(RESULT));
        // DispenserMenu, HopperMenu, BrewingStandMenu.
        assert_eq!(container_slot(9, 8), Some(308));
        assert_eq!(container_slot(9, 9), Some(9));
        assert_eq!(container_slot(5, 40), Some(8));
        assert_eq!(container_slot(5, 41), None);
    }

    #[test]
    fn anvil_names_are_filtered_and_at_most_fifty_long() {
        assert_eq!(valid_name("a\u{a7}b\u{7}c").as_deref(), Some("abc"));
        assert!(valid_name(&"x".repeat(50)).is_some());
        assert!(valid_name(&"x".repeat(51)).is_none());
    }
}
