//! Create kit and bag: 4 equip slots, 8 bag slots, coin as an integer.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use engine::load_rgba8_png;
use serde::{Deserialize, Serialize};

/// Icon family on disk. One PNG each. No runtime atlas.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Family {
    HideWrap,
    ClothScrap,
    Club,
    Blade,
    BonePlate,
    BoneScrap,
    EmberChip,
    Potion,
    Coin,
    SkullJunk,
    Chitin,
    Silk,
    Staff,
    Arrow,
    Badge,
    Rock,
    Food,
    CrateJunk,
}

impl Family {
    pub fn icon_file(self) -> &'static str {
        match self {
            Self::HideWrap => "hide_wrap.png",
            Self::ClothScrap => "cloth_scrap.png",
            Self::Club => "club.png",
            Self::Blade => "blade.png",
            Self::BonePlate => "bone_plate.png",
            Self::BoneScrap => "bone_scrap.png",
            Self::EmberChip => "ember_chip.png",
            Self::Potion => "potion.png",
            Self::Coin => "coin.png",
            Self::SkullJunk => "skull_junk.png",
            Self::Chitin => "chitin.png",
            Self::Silk => "silk.png",
            Self::Staff => "staff.png",
            Self::Arrow => "arrow.png",
            Self::Badge => "badge.png",
            Self::Rock => "rock.png",
            Self::Food => "food.png",
            Self::CrateJunk => "crate_junk.png",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EquipSlot {
    Melee,
    Bow,
    Body,
    Charm,
}

/// loot-v1 names only. Do not grow this into a catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ItemKind {
    WornBlade,
    WornBow,
    WornCloth,
    LesserMend,
    ThinArrows,
    HideWrap,
    CairnBlade,
    HutWrap,
    OrcClub,
    EmberChip,
    BonePlate,
    StaffSplinter,
}

impl ItemKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::WornBlade => "Worn Blade",
            Self::WornBow => "Worn Bow",
            Self::WornCloth => "Worn Cloth",
            Self::LesserMend => "Lesser Mend",
            Self::ThinArrows => "Thin Arrows",
            Self::HideWrap => "Hide Wrap",
            Self::CairnBlade => "Cairn Blade",
            Self::HutWrap => "Hut Wrap",
            Self::OrcClub => "Orc Club",
            Self::EmberChip => "Ember Chip",
            Self::BonePlate => "Bone Plate",
            Self::StaffSplinter => "Staff Splinter",
        }
    }

    pub fn family(self) -> Family {
        match self {
            Self::WornBlade | Self::CairnBlade => Family::Blade,
            Self::WornBow | Self::ThinArrows => Family::Arrow,
            Self::WornCloth | Self::HutWrap => Family::ClothScrap,
            Self::LesserMend => Family::Potion,
            Self::HideWrap => Family::HideWrap,
            Self::OrcClub => Family::Club,
            Self::EmberChip => Family::EmberChip,
            Self::BonePlate => Family::BonePlate,
            Self::StaffSplinter => Family::Staff,
        }
    }

    pub fn equip_slot(self) -> Option<EquipSlot> {
        match self {
            Self::WornBlade | Self::CairnBlade | Self::OrcClub | Self::StaffSplinter => {
                Some(EquipSlot::Melee)
            }
            Self::WornBow => Some(EquipSlot::Bow),
            Self::WornCloth | Self::HutWrap | Self::HideWrap | Self::BonePlate => {
                Some(EquipSlot::Body)
            }
            Self::EmberChip => Some(EquipSlot::Charm),
            Self::LesserMend | Self::ThinArrows => None,
        }
    }

    pub fn stacks(self) -> bool {
        matches!(self, Self::LesserMend | Self::ThinArrows)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub kind: ItemKind,
    pub count: u16,
}

impl Item {
    pub fn one(kind: ItemKind) -> Self {
        Self { kind, count: 1 }
    }

    pub fn name(self) -> &'static str {
        self.kind.name()
    }

    pub fn family(self) -> Family {
        self.kind.family()
    }
}

/// 4 equip + 8 bag. Coin is not a slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inventory {
    pub melee: Option<Item>,
    pub bow: Option<Item>,
    pub body: Option<Item>,
    pub charm: Option<Item>,
    pub bag: [Option<Item>; 8],
    pub coin: i32,
}

impl Inventory {
    pub fn empty() -> Self {
        Self {
            melee: None,
            bow: None,
            body: None,
            charm: None,
            bag: [None; 8],
            coin: 0,
        }
    }

    /// Create kit: Worn Blade, Worn Bow, Worn Cloth, 1 Lesser Mend, 40 arrows, 0 coin.
    pub fn create_kit() -> Self {
        let mut inv = Self::empty();
        inv.melee = Some(Item::one(ItemKind::WornBlade));
        inv.bow = Some(Item::one(ItemKind::WornBow));
        inv.body = Some(Item::one(ItemKind::WornCloth));
        inv.bag[0] = Some(Item::one(ItemKind::LesserMend));
        inv.bag[1] = Some(Item {
            kind: ItemKind::ThinArrows,
            count: 40,
        });
        inv
    }

    pub fn equip(&self, slot: EquipSlot) -> Option<Item> {
        match slot {
            EquipSlot::Melee => self.melee,
            EquipSlot::Bow => self.bow,
            EquipSlot::Body => self.body,
            EquipSlot::Charm => self.charm,
        }
    }

    pub fn equip_mut(&mut self, slot: EquipSlot) -> &mut Option<Item> {
        match slot {
            EquipSlot::Melee => &mut self.melee,
            EquipSlot::Bow => &mut self.bow,
            EquipSlot::Body => &mut self.body,
            EquipSlot::Charm => &mut self.charm,
        }
    }

    pub fn add_coin(&mut self, n: i32) {
        self.coin = self.coin.saturating_add(n).max(0);
    }

    /// Put an item in the bag. Stacks potions/arrows. False if the bag is full.
    pub fn take_item(&mut self, item: Item) -> bool {
        if item.kind.stacks() {
            for slot in &mut self.bag {
                if let Some(have) = slot {
                    if have.kind == item.kind {
                        have.count = have.count.saturating_add(item.count);
                        return true;
                    }
                }
            }
        }
        for slot in &mut self.bag {
            if slot.is_none() {
                *slot = Some(item);
                return true;
            }
        }
        false
    }

    pub fn click_bag(&mut self, i: usize) {
        if i >= self.bag.len() {
            return;
        }
        let Some(item) = self.bag[i] else {
            return;
        };
        let Some(slot) = item.kind.equip_slot() else {
            return;
        };
        let worn = *self.equip_mut(slot);
        *self.equip_mut(slot) = Some(item);
        self.bag[i] = worn;
    }

    pub fn click_equip(&mut self, slot: EquipSlot) {
        let Some(item) = *self.equip_mut(slot) else {
            return;
        };
        for bag in &mut self.bag {
            if bag.is_none() {
                *bag = Some(item);
                *self.equip_mut(slot) = None;
                return;
            }
        }
    }
}

pub fn assets_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("ORRUN_ASSETS") {
        return Some(PathBuf::from(dir));
    }
    let mut tried = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        tried.push(cwd.join("assets"));
        tried.push(cwd.join("orrun").join("assets"));
    }
    tried.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets"));
    tried.into_iter().find(|p| p.is_dir())
}

pub fn icon_path(family: Family) -> Option<PathBuf> {
    Some(assets_dir()?.join("icons").join(family.icon_file()))
}

#[derive(Clone, Debug)]
pub struct IconPixels {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

static ICONS: Mutex<Option<HashMap<Family, IconPixels>>> = Mutex::new(None);

/// Load one family PNG. The game only keeps pixels it actually reads.
pub fn load_icon(family: Family) -> Option<IconPixels> {
    let mut guard = ICONS.lock().ok()?;
    let map = guard.get_or_insert_with(HashMap::new);
    if let Some(hit) = map.get(&family) {
        return Some(hit.clone());
    }
    let path = icon_path(family)?;
    let (width, height, rgba) = load_rgba8_png(&path).ok()?;
    let pix = IconPixels {
        width,
        height,
        rgba,
    };
    map.insert(family, pix.clone());
    Some(pix)
}

pub fn icon_was_loaded(family: Family) -> bool {
    ICONS
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|m| m.contains_key(&family)))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_kit_is_four_equip_eight_bag_zero_coin() {
        let inv = Inventory::create_kit();
        assert_eq!(inv.melee.unwrap().name(), "Worn Blade");
        assert_eq!(inv.bow.unwrap().family(), Family::Arrow);
        assert_eq!(inv.body.unwrap().name(), "Worn Cloth");
        assert!(inv.charm.is_none());
        assert_eq!(inv.bag[0].unwrap().name(), "Lesser Mend");
        assert_eq!(inv.bag[1].unwrap().count, 40);
        assert_eq!(inv.coin, 0);
        assert_eq!(inv.bag.iter().filter(|s| s.is_some()).count(), 2);
        assert_eq!(inv.bag.len(), 8);
    }

    #[test]
    fn worn_bow_uses_arrow_icon_not_a_bow_family() {
        assert_eq!(ItemKind::WornBow.family().icon_file(), "arrow.png");
        assert_eq!(ItemKind::WornBow.equip_slot(), Some(EquipSlot::Bow));
    }

    #[test]
    fn click_bag_swaps_into_matching_equip() {
        let mut inv = Inventory::create_kit();
        inv.bag[2] = Some(Item::one(ItemKind::OrcClub));
        inv.click_bag(2);
        assert_eq!(inv.melee.unwrap().kind, ItemKind::OrcClub);
        assert_eq!(inv.bag[2].unwrap().kind, ItemKind::WornBlade);
    }

    #[test]
    fn icon_files_exist_on_disk() {
        let dir = assets_dir().expect("assets");
        for family in [
            Family::Blade,
            Family::Arrow,
            Family::ClothScrap,
            Family::Potion,
            Family::HideWrap,
            Family::Club,
            Family::EmberChip,
            Family::BonePlate,
            Family::Staff,
            Family::Coin,
        ] {
            let path = dir.join("icons").join(family.icon_file());
            assert!(path.is_file(), "missing {}", path.display());
        }
    }
}
