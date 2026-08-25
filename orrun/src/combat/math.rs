//! Locked combat math. Truncate toward zero after the product.
//! Numbers come from combat_sim.py. Do not fork a second formula set.

pub const TICK: f64 = 0.1;
pub const HARD_CAP_S: f64 = 180.0;

pub const WALK_MPS: f64 = 4.5;
pub const SPRINT_MPS: f64 = 7.0;
pub const MELEE_SWING_S: f64 = 1.8;
pub const MELEE_REACH_M: f64 = 2.8;
pub const MELEE_CONE_DEG: f64 = 120.0;
pub const BOW_DRAW_S: f64 = 1.2;
pub const BOW_FULL_M: f64 = 20.0;
pub const BOW_FALLOFF_END_M: f64 = 35.0;
pub const TAB_LOCK_M: f64 = 20.0;
pub const TAB_CONE_DEG: f64 = 90.0;
pub const SIGHT_AGGRO_M: f64 = 12.0;
pub const HEAR_AGGRO_M: f64 = 6.0;
pub const LEASH_M: f64 = 40.0;
pub const SOCIAL_M: f64 = 15.0;

pub const HP_L1: i32 = 100;
pub const HP_PER_LEVEL: i32 = 12;
pub const MANA_L1: i32 = 50;
pub const MANA_PER_LEVEL: i32 = 6;
pub const ATTR_BASE: i32 = 10;
pub const ATTR_PER_LEVEL: i32 = 2;
pub const DISC_PER_LEVEL: i32 = 1;
pub const SKILL_RIDER_PER: f64 = 0.004;
pub const SKILL_UP_COST_MULT: i32 = 8;
pub const SKILL_CAP_PER_LEVEL: i32 = 5;

pub const MELEE_BASE: f64 = 8.0;
pub const MELEE_MIGHT: f64 = 0.4;
pub const BOW_BASE: f64 = 7.0;
pub const BOW_SWIFT: f64 = 0.4;

pub const EMBER_BASE: f64 = 14.0;
pub const EMBER_WILL: f64 = 0.5;
pub const EMBER_MANA: i32 = 3;
pub const EMBER_CAST_S: f64 = 1.2;
pub const EMBER_CD_S: f64 = 0.0;
pub const EMBER_RANGE_M: f64 = 28.0;
pub const EMBER_RANK1: f64 = 1.15;
pub const EMBER_RANK10: f64 = 1.30;

pub const MEND_BASE: f64 = 25.0;
pub const MEND_WILL: f64 = 0.4;
pub const MEND_MANA: i32 = 20;
pub const MEND_CAST_S: f64 = 2.5;
pub const MEND_CD_S: f64 = 0.0;
pub const MEND_RANGE_M: f64 = 28.0;

pub const BIND_MANA: i32 = 16;
pub const BIND_CAST_S: f64 = 1.5;
pub const BIND_CD_S: f64 = 12.0;
pub const BIND_RANGE_M: f64 = 24.0;
pub const BIND_ROOT_S: f64 = 4.0;
pub const BIND_GRACE_S: f64 = 1.0;

pub const WARD_BASE: f64 = 20.0;
pub const WARD_WILL: f64 = 0.3;
pub const WARD_MANA: i32 = 18;
pub const WARD_GCD_S: f64 = 1.0;
pub const WARD_DUR_S: f64 = 8.0;
pub const WARD_CD_S: f64 = 16.0;

pub const STRIKE_MULT: f64 = 1.5;
pub const STRIKE_CD_S: f64 = 6.0;
pub const BASH_ANIM_S: f64 = 0.4;
pub const BASH_STUN_S: f64 = 1.5;
pub const BASH_CD_S: f64 = 8.0;
pub const AIMED_DRAW_S: f64 = 2.0;
pub const AIMED_MULT: f64 = 1.8;
pub const AIMED_CD_S: f64 = 10.0;
pub const PIN_SLOW_PCT: f64 = 0.40;
pub const PIN_DUR_S: f64 = 4.0;
pub const PIN_CD_S: f64 = 12.0;
pub const CLEAVE_PCT: f64 = 0.40;
pub const CLEAVE_RANGE_M: f64 = 2.2;
pub const MARK_MULT: f64 = 1.15;
pub const MARK_DUR_S: f64 = 12.0;
pub const MARK_CD_S: f64 = 20.0;
pub const SECOND_WIND_PCT: f64 = 0.20;
pub const POTION_HEAL: i32 = 40;
pub const POTION_CD_S: f64 = 60.0;
pub const START_ARROWS: i32 = 40;
pub const START_POTIONS: i32 = 1;
pub const MANA_REGEN_COMBAT_PER_S: f64 = 0.4;
pub const MANA_REGEN_OOC_PER_S: f64 = 2.0;
pub const MANA_REGEN_PER_S: f64 = MANA_REGEN_COMBAT_PER_S;
pub const THREAT_DMG: f64 = 1.0;
pub const THREAT_HEAL: f64 = 0.5;
pub const SHAKEN_DMG: f64 = 0.90;
pub const SHAKEN_S: f64 = 300.0;
/// Visible slain beat before shrine teleport.
pub const SLAIN_HOLD_S: f64 = 2.0;
pub const RANK5_WEAPON: f64 = 1.10;
pub const RANK10_SPELL: f64 = 1.15;

pub const WOLF_HP_BASE: i32 = 70;
pub const WOLF_HP_PER: i32 = 18;
pub const WOLF_DMG_BASE: i32 = 10;
pub const WOLF_DMG_PER: i32 = 2;
pub const WOLF_SWING_S: f64 = 2.0;
pub const WOLF_REACH_M: f64 = 1.8;
pub const WOLF_SPEED: f64 = 5.2;
pub const WOLF_ARMOR: i32 = 8;
pub const WOLF_XP_BASE: i32 = 35;
pub const WOLF_XP_PER: i32 = 12;

pub const MOTHER_HP: i32 = 420;
pub const MOTHER_DMG: i32 = 12;
pub const MOTHER_SWING_S: f64 = 2.2;
pub const MOTHER_SLAM: i32 = 24;
pub const MOTHER_SLAM_EVERY_S: f64 = 8.0;
pub const MOTHER_TELE_S: f64 = 1.2;
pub const MOTHER_ARMOR: i32 = 14;
pub const MOTHER_XP: i32 = 280;
pub const MOTHER_TOKEN: i32 = 1;
pub const MOTHER_SPEED: f64 = 3.2;
pub const MOTHER_REACH_M: f64 = 2.2;
pub const MOTHER_ENRAGE_HP: f64 = 0.30;
pub const MOTHER_ENRAGE_DMG: f64 = 1.30;
pub const MOTHER_ENRAGE_SWING: f64 = 0.77;

pub const SCORP_POISON_DPS: f64 = 3.0;
pub const SCORP_POISON_S: f64 = 4.0;

pub const BLOB_HP: i32 = 82;
pub const BLOB_DMG: i32 = 11;
pub const BLOB_SWING_S: f64 = 2.4;
pub const BLOB_ARMOR: i32 = 6;
pub const BLOB_SPEED: f64 = 5.2;
pub const BLOB_REACH_M: f64 = 1.6;
pub const BLOB_XP: i32 = 47;

pub const ORC_HP: i32 = 130;
pub const ORC_DMG: i32 = 15;
pub const ORC_SWING_S: f64 = 2.0;
pub const ORC_ARMOR: i32 = 10;
pub const ORC_SPEED: f64 = 3.5;
pub const ORC_REACH_M: f64 = 2.0;
pub const ORC_XP: i32 = 80;

// L6 first-slice stamp. Wolf ruler HP 70+18*(lvl-1)=160, atk 10+2*(lvl-1)=20, xp 35+12*(lvl-1)=95.
pub const TRIBAL_LEVEL: i32 = 6;
pub const TRIBAL_HP: i32 = 176;
pub const TRIBAL_DMG: i32 = 20;
pub const TRIBAL_SWING_S: f64 = 1.8;
pub const TRIBAL_ARMOR: i32 = 8;
pub const TRIBAL_SPEED: f64 = 5.0;
pub const TRIBAL_REACH_M: f64 = 1.6;
pub const TRIBAL_XP: i32 = 95;

pub const BANDIT_LEVEL: i32 = 5;
pub const BANDIT_HP: i32 = 156;
pub const BANDIT_ARMOR: i32 = 8;
pub const BANDIT_DMG: i32 = 18;
pub const BANDIT_SWING_S: f64 = 1.8;
pub const BANDIT_REACH_M: f64 = 2.2;
pub const BANDIT_SPEED: f64 = 5.0;
pub const BANDIT_XP: i32 = 83;

pub const SKULL_LEVEL: i32 = 6;
pub const SKULL_HP: i32 = 112;
pub const SKULL_DMG: i32 = 20;
pub const SKULL_SWING_S: f64 = 2.0;
pub const SKULL_ARMOR: i32 = 8;
pub const SKULL_SPEED: f64 = 4.8;
pub const SKULL_REACH_M: f64 = 2.0;
pub const SKULL_XP: i32 = 95;
pub const SKULL_BOLT_DMG: i32 = 14;
pub const SKULL_TELE_S: f64 = 1.2;
pub const SKULL_BOLT_RANGE_M: f64 = 24.0;

// L8 Brood bones. Wolf ruler HP 70+18*(8-1)=196, atk 10+2*(8-1)=24, xp 35+12*(8-1)=119.
pub const BONE_LEVEL: i32 = 8;
pub const WARRIOR_HP: i32 = 196;
pub const WARRIOR_DMG: i32 = 24;
pub const WARRIOR_SWING_S: f64 = 2.0;
pub const WARRIOR_REACH_M: f64 = 1.8;
pub const WARRIOR_SPEED: f64 = 5.2;
pub const WARRIOR_ARMOR: i32 = 8;
pub const WARRIOR_XP: i32 = 119;
pub const MINION_HP: i32 = 137;
pub const MAGE_HP: i32 = 137;
pub const MAGE_BOLT_DMG: i32 = 15;
pub const MAGE_TELE_S: f64 = 1.2;
pub const MAGE_BOLT_RANGE_M: f64 = 24.0;

pub const YETI_HP: i32 = 240;
pub const YETI_DMG: i32 = 14;
pub const YETI_SWING_S: f64 = 2.3;
pub const YETI_SLAM: i32 = 26;
pub const YETI_SLAM_EVERY_S: f64 = 8.0;
pub const YETI_TELE_S: f64 = 1.0;
pub const YETI_ARMOR: i32 = 12;
pub const YETI_SPEED: f64 = 3.0;
pub const YETI_REACH_M: f64 = 2.2;
pub const YETI_XP: i32 = 160;

pub const DEMON_HP: i32 = 220;
pub const DEMON_ARMOR: i32 = 12;
pub const DEMON_DMG: i32 = 16;
pub const DEMON_SWING_S: f64 = 2.0;
pub const DEMON_REACH_M: f64 = 2.2;
pub const DEMON_SPEED: f64 = 3.2;
pub const DEMON_XP: i32 = 180;

pub const BLUE_DEMON_HP: i32 = 155;
pub const BLUE_DEMON_ARMOR: i32 = 9;
pub const BLUE_DEMON_DMG: i32 = 12;
pub const BLUE_DEMON_SWING_S: f64 = 2.0;
pub const BLUE_DEMON_REACH_M: f64 = 2.0;
pub const BLUE_DEMON_SPEED: f64 = 3.6;
pub const BLUE_DEMON_XP: i32 = 140;

pub const TRIBAL_VETERAN_HP: i32 = 210;
pub const TRIBAL_VETERAN_ARMOR: i32 = 10;
pub const TRIBAL_VETERAN_DMG: i32 = 22;
pub const TRIBAL_VETERAN_SWING_S: f64 = 1.8;
pub const TRIBAL_VETERAN_REACH_M: f64 = 1.6;
pub const TRIBAL_VETERAN_SPEED: f64 = 5.0;
pub const TRIBAL_VETERAN_XP: i32 = 130;

/// Truncate toward zero (Python `int(x)` on the non-negative combat path).
#[inline]
pub fn trunc(x: f64) -> i32 {
    x as i32
}

pub fn mitigation(incoming: f64, grit_or_armor: i32) -> i32 {
    if incoming <= 0.0 {
        return 0;
    }
    trunc(incoming * 100.0 / (100.0 + f64::from(grit_or_armor)))
}

pub fn skill_rider(skill: i32) -> f64 {
    1.0 + f64::from(skill) * SKILL_RIDER_PER
}

pub fn bow_range_mult(distance: f64) -> f64 {
    if distance <= BOW_FULL_M {
        1.0
    } else if distance >= BOW_FALLOFF_END_M {
        0.0
    } else {
        (BOW_FALLOFF_END_M - distance) / (BOW_FALLOFF_END_M - BOW_FULL_M)
    }
}

pub fn ember_rank_mult(arcane: i32) -> f64 {
    if arcane >= 10 {
        EMBER_RANK10
    } else if arcane >= 1 {
        EMBER_RANK1
    } else {
        1.0
    }
}

pub fn spell_rank_mult(arcane: i32) -> f64 {
    if arcane >= 10 {
        RANK10_SPELL
    } else {
        1.0
    }
}

pub fn melee_hit(might: i32, skill: i32, martial_rank: i32, strike: bool) -> i32 {
    let rider = skill_rider(skill);
    let strike_m = if strike { STRIKE_MULT } else { 1.0 };
    let m5 = if martial_rank >= 5 { RANK5_WEAPON } else { 1.0 };
    trunc((MELEE_BASE + f64::from(might) * MELEE_MIGHT) * rider * strike_m * m5)
}

pub fn bow_hit(swift: i32, skill: i32, hunt_rank: i32, aimed: bool, distance: f64) -> i32 {
    let rider = skill_rider(skill);
    let aimed_m = if aimed { AIMED_MULT } else { 1.0 };
    let h5 = if hunt_rank >= 5 { RANK5_WEAPON } else { 1.0 };
    trunc(
        (BOW_BASE + f64::from(swift) * BOW_SWIFT) * rider * aimed_m * h5 * bow_range_mult(distance),
    )
}

pub fn ember(will: i32, skill: i32, arcane: i32) -> i32 {
    trunc(
        (EMBER_BASE + f64::from(will) * EMBER_WILL)
            * ember_rank_mult(arcane)
            * spell_rank_mult(arcane)
            * skill_rider(skill),
    )
}

pub fn mend(will: i32, arcane: i32) -> i32 {
    trunc((MEND_BASE + f64::from(will) * MEND_WILL) * spell_rank_mult(arcane))
}

pub fn ward(will: i32, arcane: i32) -> i32 {
    trunc((WARD_BASE + f64::from(will) * WARD_WILL) * spell_rank_mult(arcane))
}

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Discipline {
    Martial,
    Hunt,
    Arcane,
}

impl Discipline {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Martial => "Martial",
            Self::Hunt => "Hunt",
            Self::Arcane => "Arcane",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "Martial" | "martial" => Some(Self::Martial),
            "Hunt" | "hunt" => Some(Self::Hunt),
            "Arcane" | "arcane" => Some(Self::Arcane),
            _ => None,
        }
    }
}

impl std::fmt::Display for Discipline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attrs {
    pub might: i32,
    pub swift: i32,
    pub will: i32,
    pub grit: i32,
}

impl Default for Attrs {
    fn default() -> Self {
        Self {
            might: ATTR_BASE,
            swift: ATTR_BASE,
            will: ATTR_BASE,
            grit: ATTR_BASE,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ranks {
    pub martial: i32,
    pub hunt: i32,
    pub arcane: i32,
}
