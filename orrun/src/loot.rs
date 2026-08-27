//! loot-v1 corpse piles. Families on disk only. No 726-name catalog.

use crate::combat::ActorId;
use crate::inventory::{Inventory, InventoryError, Item, ItemKind, TakeItemOutcome};

/// Cairn vs hut, mapped by the session from overland sites.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LootSite {
    Cairn,
    Hut,
}

/// One unlooted (or partly looted) corpse pile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroundPile {
    pub actor_id: ActorId,
    pub mob_id: String,
    pub items: Vec<Item>,
    pub coin: i32,
}

impl GroundPile {
    pub fn empty(&self) -> bool {
        self.items.is_empty() && self.coin <= 0
    }

    pub fn has_visible_family(&self) -> bool {
        !self.items.is_empty()
    }
}

/// Deterministic 2..=8 coin. Always present on a lootable corpse.
pub fn roll_coin(mob_id: &str, idx: i32) -> i32 {
    let mut h = 2166136261u32;
    for b in mob_id.as_bytes() {
        h = h.wrapping_mul(16777619) ^ u32::from(*b);
    }
    h = h.wrapping_mul(16777619) ^ idx as u32;
    2 + (h % 7) as i32
}

/// Site-dependent bandit drop. Unknown site is deterministic per idx.
pub fn bandit_drop(site: Option<LootSite>, idx: i32) -> ItemKind {
    match site {
        Some(LootSite::Cairn) => ItemKind::CairnBlade,
        Some(LootSite::Hut) => ItemKind::HutWrap,
        None if idx.rem_euclid(2) == 0 => ItemKind::CairnBlade,
        None => ItemKind::HutWrap,
    }
}

/// Family item for a catalog id. Skip ids with no family (coin only).
pub fn family_drop(mob_id: &str, site: Option<LootSite>, idx: i32) -> Option<ItemKind> {
    match mob_id {
        "wolf" => Some(ItemKind::HideWrap),
        "bandit" | "male_bandit" => Some(bandit_drop(site, idx)),
        "orc" => Some(ItemKind::OrcClub),
        "orc_skull" => Some(ItemKind::EmberChip),
        "skeleton_warrior" => Some(ItemKind::BonePlate),
        "skeleton_mage" => Some(ItemKind::StaffSplinter),
        "skeleton_minion" => Some(ItemKind::LesserMend),
        // tribal: no weapon. yeti / demon / blue_demon / tribal_veteran: no extra names.
        _ => None,
    }
}

pub fn roll_pile(mob_id: &str, actor_id: ActorId, site: Option<LootSite>) -> GroundPile {
    let idx = actor_id
        .runtime_index()
        .expect("loot cannot belong to player");
    let mut items = Vec::new();
    if let Some(kind) = family_drop(mob_id, site, idx) {
        items.push(Item::one(kind));
    }
    GroundPile {
        actor_id,
        mob_id: mob_id.to_string(),
        items,
        coin: roll_coin(mob_id, idx),
    }
}

/// Playtester: guaranteed visible family + sparkle. Orc Club is the orc family.
pub fn force_visible_pile(mob_id: &str, actor_id: ActorId, site: Option<LootSite>) -> GroundPile {
    let idx = actor_id
        .runtime_index()
        .expect("loot cannot belong to player");
    let mut pile = roll_pile(mob_id, actor_id, site);
    if pile.items.is_empty() {
        let kind = family_drop(mob_id, site, idx).unwrap_or(ItemKind::OrcClub);
        pile.items.push(Item::one(kind));
    }
    pile
}

pub fn take_one(
    inv: &mut Inventory,
    pile: &mut GroundPile,
    item_i: usize,
) -> Result<bool, InventoryError> {
    if item_i >= pile.items.len() {
        return Ok(false);
    }
    let item = pile.items[item_i];
    if inv.take_item(item)? == TakeItemOutcome::BagFull {
        return Ok(false);
    }
    pile.items.remove(item_i);
    Ok(true)
}

pub fn take_coin(inv: &mut Inventory, pile: &mut GroundPile) -> Result<(), InventoryError> {
    if pile.coin == 0 {
        return Ok(());
    }
    inv.add_coin(pile.coin)?;
    pile.coin = 0;
    Ok(())
}

pub fn take_all(inv: &mut Inventory, pile: &mut GroundPile) -> Result<(), InventoryError> {
    take_coin(inv, pile)?;
    let mut i = 0;
    while i < pile.items.len() {
        if take_one(inv, pile, i)? {
            continue;
        }
        i += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taking_coin_propagates_invalid_credit_without_mutating_pile() {
        let mut inv = Inventory::empty();
        let mut pile = GroundPile {
            actor_id: ActorId::from_runtime_index(1),
            mob_id: "test".to_string(),
            items: Vec::new(),
            coin: -1,
        };

        assert_eq!(
            take_coin(&mut inv, &mut pile),
            Err(InventoryError::NegativeCoinCredit { amount: -1 })
        );
        assert_eq!(inv.coin, 0);
        assert_eq!(pile.coin, -1);
    }

    #[test]
    fn taking_coin_propagates_overflow_without_mutating_pile() {
        let mut inv = Inventory::empty();
        inv.coin = i32::MAX;
        let mut pile = GroundPile {
            actor_id: ActorId::from_runtime_index(1),
            mob_id: "test".to_string(),
            items: Vec::new(),
            coin: 1,
        };

        assert_eq!(
            take_coin(&mut inv, &mut pile),
            Err(InventoryError::CoinCreditOverflow {
                balance: i32::MAX,
                amount: 1,
            })
        );
        assert_eq!(inv.coin, i32::MAX);
        assert_eq!(pile.coin, 1);
    }

    #[test]
    fn coin_is_always_two_to_eight() {
        for idx in 0..32 {
            let c = roll_coin("orc", idx);
            assert!((2..=8).contains(&c), "coin {c}");
        }
    }

    #[test]
    fn drop_table_uses_loot_v1_names() {
        assert_eq!(family_drop("wolf", None, 0), Some(ItemKind::HideWrap));
        assert_eq!(
            family_drop("bandit", Some(LootSite::Cairn), 1),
            Some(ItemKind::CairnBlade)
        );
        assert_eq!(
            family_drop("bandit", Some(LootSite::Hut), 1),
            Some(ItemKind::HutWrap)
        );
        assert_eq!(family_drop("orc", None, 0), Some(ItemKind::OrcClub));
        assert_eq!(family_drop("orc_skull", None, 0), Some(ItemKind::EmberChip));
        assert_eq!(
            family_drop("skeleton_warrior", None, 0),
            Some(ItemKind::BonePlate)
        );
        assert_eq!(
            family_drop("skeleton_mage", None, 0),
            Some(ItemKind::StaffSplinter)
        );
        assert_eq!(
            family_drop("skeleton_minion", None, 0),
            Some(ItemKind::LesserMend)
        );
        assert_eq!(family_drop("tribal", None, 0), None);
        assert_eq!(family_drop("tribal_veteran", None, 0), None);
        assert_eq!(family_drop("yeti", None, 0), None);
    }

    #[test]
    fn force_visible_always_has_a_family_icon() {
        let pile = force_visible_pile("orc", ActorId::from_runtime_index(1), None);
        assert!(pile.has_visible_family());
        assert_eq!(pile.items[0].name(), "Orc Club");
        assert!((2..=8).contains(&pile.coin));
    }
}
