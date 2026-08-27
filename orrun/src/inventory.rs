//! Create kit and bag: 4 equip slots, 8 bag slots, coin as an integer.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use engine::load_rgba8_png;
use serde::{Deserialize, Serialize};
use thiserror::Error;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TakeItemOutcome {
    Added,
    BagFull,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum InventoryError {
    #[error("coin credit must not be negative: {amount}")]
    NegativeCoinCredit { amount: i32 },
    #[error("coin debit must not be negative: {amount}")]
    NegativeCoinDebit { amount: i32 },
    #[error("coin credit overflow: balance {balance}, credit {amount}")]
    CoinCreditOverflow { balance: i32, amount: i32 },
    #[error("insufficient coin: balance {balance}, debit {amount}")]
    InsufficientCoin { balance: i32, amount: i32 },
    #[error("item stack overflow for {kind:?}: current {current}, additional {additional}")]
    StackOverflow {
        kind: ItemKind,
        current: u16,
        additional: u16,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IconAsset {
    Item(Family),
    Shaken,
}

impl IconAsset {
    pub fn relative_path(self) -> PathBuf {
        match self {
            Self::Item(family) => PathBuf::from("icons").join(family.icon_file()),
            Self::Shaken => PathBuf::from("icons").join("status").join("shaken.png"),
        }
    }
}

#[derive(Debug, Error)]
pub enum IconError {
    #[error("no Orrun asset directory found; tried {candidates:?}")]
    NoAssets { candidates: Vec<PathBuf> },
    #[error("icon asset {asset:?} is missing at {}", .path.display())]
    Missing { asset: IconAsset, path: PathBuf },
    #[error("icon asset {asset:?} at {} failed to decode: {source}", .path.display())]
    Decode {
        asset: IconAsset,
        path: PathBuf,
        #[source]
        source: engine::error::EngineError,
    },
    #[error("icon cache mutex was poisoned")]
    CachePoisoned,
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

    pub fn add_coin(&mut self, amount: i32) -> Result<(), InventoryError> {
        if amount < 0 {
            return Err(InventoryError::NegativeCoinCredit { amount });
        }
        self.coin = self
            .coin
            .checked_add(amount)
            .ok_or(InventoryError::CoinCreditOverflow {
                balance: self.coin,
                amount,
            })?;
        Ok(())
    }

    pub fn debit_coin(&mut self, amount: i32) -> Result<(), InventoryError> {
        if amount < 0 {
            return Err(InventoryError::NegativeCoinDebit { amount });
        }
        self.coin = self
            .coin
            .checked_sub(amount)
            .filter(|balance| *balance >= 0)
            .ok_or(InventoryError::InsufficientCoin {
                balance: self.coin,
                amount,
            })?;
        Ok(())
    }

    /// Put an item in the bag. Stacks potions/arrows.
    pub fn take_item(&mut self, item: Item) -> Result<TakeItemOutcome, InventoryError> {
        if item.kind.stacks() {
            for have in self.bag.iter_mut().flatten() {
                if have.kind == item.kind {
                    have.count = have.count.checked_add(item.count).ok_or(
                        InventoryError::StackOverflow {
                            kind: item.kind,
                            current: have.count,
                            additional: item.count,
                        },
                    )?;
                    return Ok(TakeItemOutcome::Added);
                }
            }
        }
        for slot in &mut self.bag {
            if slot.is_none() {
                *slot = Some(item);
                return Ok(TakeItemOutcome::Added);
            }
        }
        Ok(TakeItemOutcome::BagFull)
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

pub fn asset_candidates() -> Vec<PathBuf> {
    let mut tried = Vec::new();
    if let Some(dir) = std::env::var_os("ORRUN_ASSETS") {
        tried.push(PathBuf::from(dir));
    }
    if let Ok(cwd) = std::env::current_dir() {
        tried.push(cwd.join("assets"));
        tried.push(cwd.join("orrun").join("assets"));
    }
    tried.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets"));
    tried
}

pub fn assets_dir() -> Result<PathBuf, IconError> {
    let candidates = asset_candidates();
    candidates
        .iter()
        .find(|path| path.is_dir())
        .cloned()
        .ok_or(IconError::NoAssets { candidates })
}

pub fn icon_path(asset: IconAsset) -> Result<PathBuf, IconError> {
    let path = assets_dir()?.join(asset.relative_path());
    if !path.is_file() {
        return Err(IconError::Missing { asset, path });
    }
    Ok(path)
}

#[derive(Clone, Debug)]
pub struct IconPixels {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

static ICONS: Mutex<Option<HashMap<IconAsset, IconPixels>>> = Mutex::new(None);

pub fn load_icon(asset: IconAsset) -> Result<IconPixels, IconError> {
    let mut guard = ICONS.lock().map_err(|_| IconError::CachePoisoned)?;
    let map = guard.get_or_insert_with(HashMap::new);
    if let Some(hit) = map.get(&asset) {
        return Ok(hit.clone());
    }
    let path = icon_path(asset)?;
    let (width, height, rgba) = load_rgba8_png(&path).map_err(|source| IconError::Decode {
        asset,
        path: path.clone(),
        source,
    })?;
    let pixels = IconPixels {
        width,
        height,
        rgba,
    };
    map.insert(asset, pixels.clone());
    Ok(pixels)
}

pub fn icon_was_loaded(asset: IconAsset) -> Result<bool, IconError> {
    let guard = ICONS.lock().map_err(|_| IconError::CachePoisoned)?;
    Ok(guard
        .as_ref()
        .is_some_and(|icons| icons.contains_key(&asset)))
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
    fn coin_credit_rejects_negative_amount_without_mutation() {
        let mut inv = Inventory::empty();
        inv.coin = 7;

        assert_eq!(
            inv.add_coin(-1),
            Err(InventoryError::NegativeCoinCredit { amount: -1 })
        );
        assert_eq!(inv.coin, 7);
    }

    #[test]
    fn coin_credit_reports_overflow_without_mutation() {
        let mut inv = Inventory::empty();
        inv.coin = i32::MAX;

        assert_eq!(
            inv.add_coin(1),
            Err(InventoryError::CoinCreditOverflow {
                balance: i32::MAX,
                amount: 1,
            })
        );
        assert_eq!(inv.coin, i32::MAX);
    }

    #[test]
    fn coin_credit_and_debit_update_balance() {
        let mut inv = Inventory::empty();

        assert_eq!(inv.add_coin(12), Ok(()));
        assert_eq!(inv.debit_coin(5), Ok(()));
        assert_eq!(inv.coin, 7);
    }

    #[test]
    fn coin_debit_rejects_negative_and_insufficient_amounts() {
        let mut inv = Inventory::empty();
        inv.coin = 7;

        assert_eq!(
            inv.debit_coin(-1),
            Err(InventoryError::NegativeCoinDebit { amount: -1 })
        );
        assert_eq!(
            inv.debit_coin(8),
            Err(InventoryError::InsufficientCoin {
                balance: 7,
                amount: 8,
            })
        );
        assert_eq!(inv.coin, 7);
    }

    #[test]
    fn stack_addition_reports_overflow_without_mutation() {
        let mut inv = Inventory::empty();
        inv.bag[0] = Some(Item {
            kind: ItemKind::ThinArrows,
            count: u16::MAX,
        });

        assert_eq!(
            inv.take_item(Item::one(ItemKind::ThinArrows)),
            Err(InventoryError::StackOverflow {
                kind: ItemKind::ThinArrows,
                current: u16::MAX,
                additional: 1,
            })
        );
        assert_eq!(inv.bag[0].expect("arrow stack").count, u16::MAX);
    }

    #[test]
    fn stack_addition_and_full_bag_have_typed_outcomes() {
        let mut inv = Inventory::empty();
        inv.bag[0] = Some(Item {
            kind: ItemKind::ThinArrows,
            count: 40,
        });

        assert_eq!(
            inv.take_item(Item {
                kind: ItemKind::ThinArrows,
                count: 2,
            }),
            Ok(TakeItemOutcome::Added)
        );
        assert_eq!(inv.bag[0].expect("arrow stack").count, 42);

        inv.bag = [Some(Item::one(ItemKind::OrcClub)); 8];
        assert_eq!(
            inv.take_item(Item::one(ItemKind::CairnBlade)),
            Ok(TakeItemOutcome::BagFull)
        );
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
