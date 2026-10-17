//! The workstation menus on the engine: `EnchantmentMenu`, `AnvilMenu`,
//! `GrindstoneMenu`, `SmithingMenu` (the two `ItemCombinerMenu`s),
//! `StonecutterMenu` and `CartographyTableMenu`. Their slots, `slotsChanged`
//! results, `quickMoveStack`, result `onTake` and `clickMenuButton`; the item
//! rules are `crate::stations`'. What the world must do afterwards (levels,
//! the anvil's damage, experience, sounds) is left in `StationState::effects`
//! and `Menu::sounds` for the caller.
use super::{Env, Kind, MAIN, Menu, RESULT, Role, split};
use crate::{items::ItemStack, stations};
use std::sync::Arc;

/// What the workstation menus need of the world (`ContainerLevelAccess`).
pub struct Stations<'a> {
    pub data: &'a stations::StationData,
    /// `ItemStack.getHoverName().getString()` of an item without a custom name.
    pub item_name: &'a dyn Fn(&str) -> String,
    /// An enchantment's network id (`EnchantmentMenu.enchantClue`).
    pub enchantment_id: &'a dyn Fn(&str) -> Option<i32>,
    /// `MapItem.getSavedData`: whether a map is locked, and its scale.
    pub map_state: &'a dyn Fn(i32) -> Option<(bool, u8)>,
}

/// What a take or a button did that the world carries out.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    /// `EnchantmentMenu.clickMenuButton` enchanted the item:
    /// `Player.onEnchantmentPerformed(levels)`, `Stats.ENCHANT_ITEM`, the
    /// enchanted item trigger and the table's sound.
    Enchanted { levels: i32 },
    /// `AnvilMenu.onTake`: `giveExperienceLevels(-levels)` (none in
    /// creative), then the anvil may be damaged (level events 1029 / 1030).
    AnvilUsed { levels: i32 },
    /// `GrindstoneMenu`'s result was taken: the experience of these inputs
    /// (`getExperienceAmount`, level event 1042).
    Ground(Option<ItemStack>, Option<ItemStack>),
    /// `SmithingMenu.onTake` (level event 1044).
    Smithed,
}

/// A workstation's state besides its slots and data slots.
#[derive(Default)]
pub struct StationState {
    /// `Player.experienceLevel`, for the enchanting table's and the anvil's
    /// checks (the caller applies the effects' costs to the player).
    pub levels: i32,
    /// The enchanting table's bookshelves (`EnchantingTableBlock.isValidBookShelf`).
    pub bookshelves: i32,
    /// The player's next `enchantmentSeed` (`onEnchantmentPerformed` draws it).
    pub next_seed: i32,
    /// `AnvilMenu.itemName`.
    pub name: Option<String>,
    repair_item_count_cost: u32,
    only_renaming: bool,
    /// `StonecutterMenu.input`'s item.
    input: Option<Arc<str>>,
    /// `StonecutterMenu.recipesForInput`: indices of the station recipes.
    pub recipes: Vec<usize>,
    pub effects: Vec<Effect>,
}

/// The roles of a workstation's own slots, in menu order (the result follows).
pub(super) fn roles(kind: Kind) -> Vec<Role> {
    match kind {
        Kind::Enchantment => vec![Role::EnchantItem, Role::Lapis],
        Kind::Anvil => vec![Role::Plain, Role::Plain],
        Kind::Grindstone => vec![Role::Grind, Role::Grind],
        Kind::Smithing => (0..3).map(Role::Smithing).collect(),
        Kind::Stonecutter => vec![Role::Plain],
        Kind::Cartography => vec![Role::MapIn, Role::MapExtra],
        _ => Vec::new(),
    }
}

/// The data slots: the enchanting table's costs, seed and clues, the
/// anvil's cost, smithing's recipe error, the stonecutter's selection.
pub(super) fn data_count(kind: Kind) -> usize {
    match kind {
        Kind::Enchantment => 10,
        Kind::Anvil | Kind::Smithing | Kind::Stonecutter => 1,
        _ => 0,
    }
}

/// The cartography table's second slot: paper, an empty map or a glass pane.
pub(super) fn is_map_extra(stack: &ItemStack) -> bool {
    matches!(
        &*stack.item,
        "minecraft:paper" | "minecraft:map" | "minecraft:glass_pane"
    )
}

/// `StringUtil.filterText`: no section signs or control characters.
fn filter_text(text: &str) -> String {
    text.chars()
        .filter(|&c| c != '§' && c >= ' ' && c != '\u{7f}')
        .collect()
}

impl Menu {
    fn station_item(&self, i: usize) -> Option<&ItemStack> {
        self.containers[MAIN][i].as_ref()
    }

    /// The result slot's `mayPickup`: the anvil wants a cost the player can pay.
    pub(super) fn station_may_pickup(&self) -> bool {
        match self.kind {
            Kind::Anvil => {
                (self.creative || self.station.levels >= self.data[0]) && self.data[0] > 0
            }
            _ => true,
        }
    }

    /// `slotsChanged` of the workstation's input container.
    pub(super) fn station_changed(&mut self, env: &Env) {
        let Some(st) = env.stations else { return };
        let e = &st.data.enchantments;
        match self.kind {
            Kind::Enchantment => {
                let offers = stations::enchant_offers(
                    e,
                    env.items,
                    self.data[3],
                    self.station.bookshelves,
                    self.station_item(0),
                );
                for i in 0..3 {
                    self.data[i] = offers.costs[i];
                    let clue = offers.clues[i]
                        .as_ref()
                        .and_then(|(id, level)| Some(((st.enchantment_id)(id)?, *level as i32)));
                    self.data[4 + i] = clue.map_or(-1, |c| c.0);
                    self.data[7 + i] = clue.map_or(-1, |c| c.1);
                }
            }
            Kind::Anvil => {
                // AnvilMenu.createResult.
                let input = self.station_item(0);
                let hover = input
                    .map(|i| stations::hover_name(st.item_name, i))
                    .unwrap_or_default();
                let r = stations::anvil_result(
                    e,
                    env.items,
                    input,
                    self.station_item(1),
                    self.station.name.as_deref(),
                    &hover,
                    self.creative,
                );
                self.containers[RESULT][0] = r.result;
                self.data[0] = r.cost;
                self.station.repair_item_count_cost = r.repair_item_count_cost;
                self.station.only_renaming = r.only_renaming;
            }
            Kind::Grindstone => {
                self.containers[RESULT][0] = stations::grindstone_result(
                    e,
                    env.items,
                    self.station_item(0),
                    self.station_item(1),
                );
            }
            Kind::Smithing => {
                let result = st.data.recipes.smithing_result(
                    e,
                    self.station_item(0),
                    self.station_item(1),
                    self.station_item(2),
                );
                // SmithingMenu.slotsChanged: hasRecipeError.
                let all = (0..3).all(|i| self.station_item(i).is_some());
                self.data[0] = i32::from(all && result.is_none());
                self.containers[RESULT][0] = result;
            }
            Kind::Stonecutter => {
                let input = self.station_item(0).cloned();
                let item = input.as_ref().map(|s| s.item.clone());
                if item != self.station.input {
                    // setupRecipeList.
                    self.station.input = item;
                    self.data[0] = -1;
                    self.containers[RESULT][0] = None;
                    self.station.recipes = match &input {
                        Some(stack) => st
                            .data
                            .recipes
                            .stonecutting
                            .iter()
                            .enumerate()
                            .filter(|(_, r)| r.input.test(stack))
                            .map(|(i, _)| i)
                            .collect(),
                        None => Vec::new(),
                    };
                }
            }
            Kind::Cartography => {
                let both = self.station_item(0).is_some() && self.station_item(1).is_some();
                if self.containers[RESULT][0].is_none() || both {
                    if both {
                        self.cartography_result(st);
                    }
                } else {
                    self.containers[RESULT][0] = None;
                }
            }
            _ => {}
        }
    }

    /// `CartographyTableMenu.setupResultSlot`: a zoomed out or locked copy
    /// (taking it makes the new map, `MAP_POST_PROCESSING`), or two copies.
    fn cartography_result(&mut self, st: &Stations) {
        let (Some(map), Some(extra)) = (self.station_item(0), self.station_item(1)) else {
            return;
        };
        let Some((locked, scale)) = map
            .data()
            .and_then(|d| d.map_id)
            .and_then(|id| (st.map_state)(id))
        else {
            return;
        };
        let copy = |count: u32, post: Option<u8>| {
            let mut out = ItemStack {
                count,
                ..map.clone()
            };
            if post.is_some() {
                out.data_mut().map_post_processing = post;
            }
            out
        };
        let out = match &*extra.item {
            "minecraft:paper" if !locked && scale < 4 => copy(1, Some(1)),
            "minecraft:glass_pane" if !locked => copy(1, Some(0)),
            "minecraft:map" => copy(2, None),
            _ => {
                self.containers[RESULT][0] = None;
                return;
            }
        };
        if self.containers[RESULT][0].as_ref() != Some(&out) {
            self.containers[RESULT][0] = Some(out);
        }
    }

    /// The result slot's `onTake`.
    pub(super) fn station_take(&mut self, env: &Env) {
        match self.kind {
            Kind::Anvil => {
                let cost = self.data[0];
                let levels = if self.creative { 0 } else { cost };
                self.station.levels = (self.station.levels - levels).max(0);
                let repair = self.station.repair_item_count_cost;
                if repair > 0 {
                    match self.station_item(1).cloned() {
                        Some(mut s) if s.count > repair => {
                            s.count -= repair;
                            self.put(env, 1, Some(s));
                        }
                        _ => self.put(env, 1, None),
                    }
                } else if !self.station.only_renaming {
                    self.put(env, 1, None);
                }
                self.data[0] = 0;
                self.put(env, 0, None);
                self.station.effects.push(Effect::AnvilUsed { levels });
            }
            Kind::Grindstone => {
                let inputs = (self.station_item(0).cloned(), self.station_item(1).cloned());
                self.station.effects.push(Effect::Ground(inputs.0, inputs.1));
                self.put(env, 0, None);
                self.put(env, 1, None);
            }
            Kind::Smithing => {
                // shrinkStackInSlot for the template, base and addition.
                for i in 0..3 {
                    if let Some(mut s) = self.station_item(i).cloned() {
                        s.count -= 1;
                        self.put(env, i, Some(s));
                    }
                }
                self.station.effects.push(Effect::Smithed);
            }
            Kind::Stonecutter => {
                let recipe = usize::try_from(self.data[0])
                    .ok()
                    .and_then(|i| self.station.recipes.get(i))
                    .and_then(|&r| env.stations?.data.recipes.stonecutting.get(r));
                if let Some(r) = recipe {
                    self.crafted.push(r.id.clone());
                }
                if self.remove(env, 0, 1).is_some() {
                    self.stonecutter_result(env, self.data[0]);
                }
                self.sounds.push("minecraft:ui.stonecutter.take_result");
            }
            Kind::Cartography => {
                self.remove(env, 0, 1);
                self.remove(env, 1, 1);
                self.sounds.push("minecraft:ui.cartography_table.take_result");
            }
            _ => {}
        }
    }

    /// `StonecutterMenu.setupResultSlot`.
    fn stonecutter_result(&mut self, env: &Env, index: i32) {
        let result = usize::try_from(index)
            .ok()
            .and_then(|i| self.station.recipes.get(i))
            .and_then(|&r| env.stations?.data.recipes.stonecutting.get(r))
            .map(|r| r.result.clone());
        self.containers[RESULT][0] = result;
    }

    /// `StonecutterMenu.clickMenuButton`: selecting the selected recipe again is refused.
    pub(super) fn stonecutter_button(&mut self, env: &Env, id: i32) -> bool {
        if self.data[0] == id {
            return false;
        }
        if usize::try_from(id).is_ok_and(|i| i < self.station.recipes.len()) {
            self.data[0] = id;
            self.stonecutter_result(env, id);
        }
        true
    }

    /// `EnchantmentMenu.clickMenuButton`: enchant with offer `id` for its
    /// lapis and levels; the seed changes and the offers follow.
    pub(super) fn enchant_button(&mut self, env: &Env, id: i32) -> bool {
        if !(0..3).contains(&id) {
            return false;
        }
        let (slot, needed) = (id as usize, id + 1);
        let lapis = self.station_item(1).map_or(0, |l| l.count as i32);
        if lapis < needed && !self.creative {
            return false;
        }
        let cost = self.data[slot];
        let Some(item) = self.station_item(0).cloned() else {
            return false;
        };
        let levels = self.station.levels;
        if cost <= 0 || (levels < needed || levels < cost) && !self.creative {
            return false;
        }
        let Some(st) = env.stations else { return true };
        let seed = self.data[3];
        let Some(enchanted) =
            stations::apply_enchantment(&st.data.enchantments, env.items, seed, slot, cost, &item)
        else {
            return true;
        };
        self.station.levels = (levels - needed).max(0);
        self.put(env, 0, Some(enchanted));
        // ItemStack.consume: creative players keep their lapis.
        if !self.creative && let Some(mut l) = self.station_item(1).cloned() {
            l.count = l.count.saturating_sub(needed as u32);
            if l.count == 0 {
                self.put(env, 1, None);
            } else {
                self.containers[MAIN][1] = Some(l);
            }
        }
        self.station.effects.push(Effect::Enchanted { levels: needed });
        self.data[3] = self.station.next_seed;
        self.station_changed(env);
        true
    }

    /// `AnvilMenu.setItemName` (ServerboundRenameItemPacket): false when the
    /// name is too long or the same.
    pub fn rename(&mut self, env: &Env, name: &str) -> bool {
        if self.kind != Kind::Anvil {
            return false;
        }
        let name = filter_text(name);
        if name.encode_utf16().count() > 50 || self.station.name.as_deref() == Some(&*name) {
            return false;
        }
        self.station.name = Some(name);
        self.station_changed(env);
        true
    }

    /// `AbstractContainerMenu.removed` and `clearContainer` of a
    /// workstation's (or the loom's) own slots: the carried stack and the
    /// inputs go back to the inventory, what doesn't fit is dropped; the
    /// result is discarded.
    pub fn station_close(&mut self, env: &Env) {
        if let Some(mut carried) = self.carried.take() {
            self.place_back(env, &mut carried);
        }
        self.containers[RESULT][0] = None;
        for i in 0..self.containers[MAIN].len() {
            if let Some(mut stack) = self.containers[MAIN][i].take() {
                self.place_back(env, &mut stack);
            }
        }
    }

    /// `ItemCombinerMenu.canMoveIntoInputSlots`: smithing only into an
    /// empty slot whose recipe property set takes the stack.
    fn can_move_into_inputs(&self, env: &Env, stack: &ItemStack) -> bool {
        if self.kind != Kind::Smithing {
            return true;
        }
        let Some(st) = env.stations else { return false };
        (0..3).any(|i| st.data.recipes.smithing_accepts(i, stack) && !self.has_item(i))
    }

    /// The workstations' `quickMoveStack` before the shared ending: false
    /// when nothing could move.
    pub(super) fn station_quick_move(
        &mut self,
        env: &Env,
        index: usize,
        stack: &mut ItemStack,
    ) -> bool {
        match self.kind {
            Kind::Enchantment => {
                if index <= 1 {
                    self.move_to(env, stack, 2, 38, true)
                } else if &*stack.item == "minecraft:lapis_lazuli" {
                    self.move_to(env, stack, 1, 2, true)
                } else if self.has_item(0) || !self.may_place(env, 0, stack) {
                    false
                } else {
                    let one = split(stack, 1);
                    self.put(env, 0, Some(one));
                    true
                }
            }
            Kind::Anvil | Kind::Smithing => {
                // ItemCombinerMenu: the inputs, the result, then the inventory.
                let result = if self.kind == Kind::Anvil { 2 } else { 3 };
                let (start, end, row_end) = (result + 1, result + 28, result + 37);
                if index == result {
                    self.move_to(env, stack, start, row_end, true)
                } else if index < result {
                    self.move_to(env, stack, start, row_end, false)
                } else if self.can_move_into_inputs(env, stack) && index < row_end {
                    self.move_to(env, stack, 0, result, false)
                } else if index < end {
                    self.move_to(env, stack, end, row_end, false)
                } else if index < row_end {
                    self.move_to(env, stack, start, end, false)
                } else {
                    true
                }
            }
            Kind::Grindstone => {
                if index == 2 {
                    self.move_to(env, stack, 3, 39, true)
                } else if index <= 1 {
                    self.move_to(env, stack, 3, 39, false)
                } else if !self.has_item(0) || !self.has_item(1) {
                    self.move_to(env, stack, 0, 2, false)
                } else if index < 30 {
                    self.move_to(env, stack, 30, 39, false)
                } else if index < 39 {
                    self.move_to(env, stack, 3, 30, false)
                } else {
                    true
                }
            }
            Kind::Stonecutter => {
                let accepts = env.stations.is_some_and(|st| {
                    st.data
                        .recipes
                        .stonecutting
                        .iter()
                        .any(|r| r.input.test(stack))
                });
                if index == 1 {
                    self.move_to(env, stack, 2, 38, true)
                } else if index == 0 {
                    self.move_to(env, stack, 2, 38, false)
                } else if accepts {
                    self.move_to(env, stack, 0, 1, false)
                } else if index < 29 {
                    self.move_to(env, stack, 29, 38, false)
                } else if index < 38 {
                    self.move_to(env, stack, 2, 29, false)
                } else {
                    true
                }
            }
            Kind::Cartography => {
                if index == 2 {
                    self.move_to(env, stack, 3, 39, true)
                } else if index <= 1 {
                    self.move_to(env, stack, 3, 39, false)
                } else if stack.data().and_then(|d| d.map_id).is_some() {
                    self.move_to(env, stack, 0, 1, false)
                } else if is_map_extra(stack) {
                    self.move_to(env, stack, 1, 2, false)
                } else if index < 30 {
                    self.move_to(env, stack, 30, 39, false)
                } else if index < 39 {
                    self.move_to(env, stack, 3, 30, false)
                } else {
                    true
                }
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{crafting::Recipes, items::Items, menu::Input, stations::StationData};

    fn stack(item: &str, count: u32) -> Option<ItemStack> {
        Some(ItemStack::new(&format!("minecraft:{item}"), count))
    }

    /// A workstation menu over the game's data, a survival player with no
    /// levels; skipped without the data.
    fn with_station(kind: Kind, test: impl FnOnce(&Env, &mut Menu)) {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/horizon-client/26.3");
        if !root.join("items.json").exists() {
            return;
        }
        let items = Items::catalog(&root).unwrap();
        let recipes = Recipes::load(&root, &items).unwrap();
        let data = StationData::load(&root, &items);
        let names = |id: &str| format!("name of {id}");
        let ids = |_: &str| Some(5);
        // Map 7 is unlocked at scale 0, map 8 locked.
        let maps = |id: i32| match id {
            7 => Some((false, 0)),
            8 => Some((true, 0)),
            _ => None,
        };
        let stations = Stations {
            data: &data,
            item_name: &names,
            enchantment_id: &ids,
            map_state: &maps,
        };
        let env = Env {
            items: &items,
            recipes: &recipes,
            brewing: &data.brewing,
            banners: &data.banners,
            trade: None,
            stations: Some(&stations),
        };
        let mut menu = Menu::new(kind);
        test(&env, &mut menu);
    }

    fn filled_map(id: i32) -> ItemStack {
        let mut map = ItemStack::new("minecraft:filled_map", 1);
        map.data_mut().map_id = Some(id);
        map
    }

    #[test]
    fn the_anvil_repairs_for_levels_and_uses_up_the_material() {
        with_station(Kind::Anvil, |env, menu| {
            let mut sword = ItemStack::new("minecraft:diamond_sword", 1);
            sword.damage = 500;
            menu.set_slot(env, 0, Some(sword));
            menu.set_slot(env, 1, stack("diamond", 3));
            // Two diamonds repair 390 and 110 damage: two levels.
            assert_eq!(menu.slot(2).map(|r| r.damage), Some(0));
            assert_eq!(menu.data[0], 2);
            // mayPickup: a survival player without the levels can't take it.
            menu.click(env, 2, 0, Input::Pickup).unwrap();
            assert_eq!(menu.carried, None);
            menu.station.levels = 5;
            menu.click(env, 2, 0, Input::Pickup).unwrap();
            assert_eq!(menu.carried.as_ref().map(|c| c.damage), Some(0));
            assert_eq!(menu.slot(0), None);
            assert_eq!(menu.slot(1), stack("diamond", 1).as_ref());
            assert_eq!(menu.slot(2), None);
            assert_eq!(menu.data[0], 0);
            assert_eq!(menu.station.levels, 3);
            assert_eq!(menu.station.effects, vec![Effect::AnvilUsed { levels: 2 }]);
        });
    }

    #[test]
    fn renaming_costs_a_level_and_keeps_the_addition() {
        with_station(Kind::Anvil, |env, menu| {
            menu.set_slot(env, 0, stack("diamond_sword", 1));
            menu.set_slot(env, 1, stack("diamond", 3));
            // A diamond sword at full durability: nothing to repair.
            assert_eq!(menu.slot(2), None);
            menu.set_slot(env, 1, None);
            assert!(menu.rename(env, "Sting\u{a7}"));
            assert!(!menu.rename(env, "Sting"));
            assert!(!menu.rename(env, &"x".repeat(51)));
            let result = menu.slot(2).cloned().unwrap();
            assert_eq!(result.custom_name(), Some("Sting"));
            assert_eq!(menu.data[0], 1);
            // The item's own name is no rename.
            assert!(menu.rename(env, "name of minecraft:diamond_sword"));
            assert_eq!((menu.slot(2), menu.data[0]), (None, 0));
            // A second sword that adds nothing: only renaming, so it stays.
            menu.rename(env, "Sting");
            menu.set_slot(env, 1, stack("diamond_sword", 1));
            assert_eq!(menu.data[0], 1);
            menu.station.levels = 1;
            menu.click(env, 2, 0, Input::Pickup).unwrap();
            assert_eq!(menu.carried.as_ref().and_then(|c| c.custom_name()), Some("Sting"));
            assert_eq!(menu.slot(1), stack("diamond_sword", 1).as_ref());
            assert_eq!(menu.station.levels, 0);
        });
    }

    #[test]
    fn the_enchanting_table_offers_and_enchants_a_book() {
        with_station(Kind::Enchantment, |env, menu| {
            menu.station.bookshelves = 15;
            menu.data[3] = 12345;
            menu.set_player_item(9, stack("book", 2));
            menu.set_player_item(10, stack("lapis_lazuli", 3));
            // Quick moves: one book into the item slot, the lapis next to it.
            menu.click(env, 2, 0, Input::QuickMove).unwrap();
            menu.click(env, 3, 0, Input::QuickMove).unwrap();
            assert_eq!(menu.slot(0), stack("book", 1).as_ref());
            assert_eq!(menu.player_item(9), stack("book", 1).as_ref());
            assert_eq!(menu.slot(1), stack("lapis_lazuli", 3).as_ref());
            // Fifteen bookshelves: the third offer costs at least 30 levels.
            assert!(menu.data[0] >= 1 && menu.data[2] >= 30);
            assert_eq!(menu.data[4], 5);
            assert!(menu.data[7] >= 1);
            // Not enough levels: refused.
            assert!(!menu.button(env, 0));
            menu.station.levels = 30;
            menu.station.next_seed = 777;
            assert!(!menu.button(env, 3));
            assert!(menu.button(env, 0));
            let book = menu.slot(0).cloned().unwrap();
            assert_eq!(&*book.item, "minecraft:enchanted_book");
            assert!(!book.stored_enchantments().is_empty());
            assert_eq!(menu.slot(1), stack("lapis_lazuli", 2).as_ref());
            assert_eq!(menu.station.levels, 29);
            assert_eq!(menu.station.effects, vec![Effect::Enchanted { levels: 1 }]);
            // The new seed, and no offers for an enchanted book.
            assert_eq!(menu.data[3], 777);
            assert_eq!(&menu.data[..3], &[0, 0, 0]);
            assert_eq!(&menu.data[4..], &[-1; 6]);
        });
    }

    #[test]
    fn the_stonecutter_selects_a_recipe_and_cuts_the_whole_input() {
        with_station(Kind::Stonecutter, |env, menu| {
            menu.set_slot(env, 0, stack("stone", 3));
            assert_eq!((menu.data[0], menu.slot(1)), (-1, None));
            let cutting = &env.stations.unwrap().data.recipes.stonecutting;
            let slab = menu
                .station
                .recipes
                .iter()
                .position(|&r| cutting[r].id == "minecraft:stone_slab_from_stone_stonecutting")
                .unwrap() as i32;
            assert!(menu.button(env, slab));
            assert!(!menu.button(env, slab));
            assert_eq!(menu.slot(1), stack("stone_slab", 2).as_ref());
            // A shift click takes results while there is input.
            menu.click(env, 1, 0, Input::QuickMove).unwrap();
            assert_eq!(menu.player_item(8), stack("stone_slab", 6).as_ref());
            assert_eq!((menu.slot(0), menu.slot(1)), (None, None));
            assert_eq!(menu.data[0], -1);
            assert!(menu.station.recipes.is_empty());
            assert!(menu.sounds.contains(&"minecraft:ui.stonecutter.take_result"));
        });
    }

    #[test]
    fn the_grindstone_takes_the_enchantments_off() {
        with_station(Kind::Grindstone, |env, menu| {
            let mut sword = ItemStack::new("minecraft:diamond_sword", 1);
            sword.enchant("minecraft:sharpness", 3);
            menu.set_slot(env, 0, Some(sword.clone()));
            assert_eq!(menu.slot(2), stack("diamond_sword", 1).as_ref());
            menu.click(env, 2, 0, Input::Pickup).unwrap();
            assert_eq!(menu.carried, stack("diamond_sword", 1));
            assert_eq!(menu.slot(0), None);
            assert_eq!(menu.station.effects, vec![Effect::Ground(Some(sword), None)]);
        });
    }

    #[test]
    fn smithing_uses_one_of_each_input() {
        with_station(Kind::Smithing, |env, menu| {
            menu.set_slot(env, 0, stack("netherite_upgrade_smithing_template", 2));
            menu.set_slot(env, 1, stack("diamond_sword", 1));
            menu.set_slot(env, 2, stack("netherite_ingot", 3));
            assert_eq!(menu.slot(3), stack("netherite_sword", 1).as_ref());
            assert_eq!(menu.data[0], 0);
            menu.click(env, 3, 0, Input::Pickup).unwrap();
            assert_eq!(menu.carried, stack("netherite_sword", 1));
            assert_eq!(menu.slot(0), stack("netherite_upgrade_smithing_template", 1).as_ref());
            assert_eq!((menu.slot(1), menu.slot(2)), (None, stack("netherite_ingot", 2).as_ref()));
            assert_eq!(menu.station.effects, vec![Effect::Smithed]);
            // Three inputs without a recipe: SmithingMenu.hasRecipeError.
            menu.set_slot(env, 1, stack("stone", 1));
            assert_eq!((menu.slot(3), menu.data[0]), (None, 1));
        });
    }

    #[test]
    fn the_cartography_table_zooms_copies_and_refuses_locked_maps() {
        with_station(Kind::Cartography, |env, menu| {
            menu.set_slot(env, 0, Some(filled_map(7)));
            menu.set_slot(env, 1, stack("paper", 1));
            let zoomed = menu.slot(2).cloned().unwrap();
            assert_eq!(zoomed.data().and_then(|d| d.map_post_processing), Some(1));
            menu.set_slot(env, 1, stack("map", 1));
            assert_eq!(menu.slot(2), Some(&ItemStack { count: 2, ..filled_map(7) }));
            menu.click(env, 2, 0, Input::QuickMove).unwrap();
            assert_eq!(menu.player_item(8), Some(&ItemStack { count: 2, ..filled_map(7) }));
            assert_eq!((menu.slot(0), menu.slot(1), menu.slot(2)), (None, None, None));
            // A locked map can't be locked again.
            menu.set_slot(env, 0, Some(filled_map(8)));
            menu.set_slot(env, 1, stack("glass_pane", 1));
            assert_eq!(menu.slot(2), None);
        });
    }
}
