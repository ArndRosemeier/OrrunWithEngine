//! Orrun v1 combat: bible math, headless sim, live types.
//!
//! One formula set. The `combat_sim` bin is a thin CLI over this crate.

pub mod catalog;
pub mod lock;
pub mod log;
pub mod math;
pub mod sheets;
pub mod sim;
pub mod types;
pub mod verbs;

pub use log::CombatLog;
pub use math::{
    mitigation, skill_rider, Attrs, Discipline, Ranks, BOW_DRAW_S, HEAR_AGGRO_M, LEASH_M,
    MELEE_CONE_DEG, MELEE_REACH_M, MELEE_SWING_S, POTION_CD_S, POTION_HEAL, SIGHT_AGGRO_M,
    SPRINT_MPS, TAB_CONE_DEG, TAB_LOCK_M, TICK, WALK_MPS,
};
pub use sheets::{
    bow_raw, ember_raw, formulas, melee_raw, mend_raw, player_stats, ward_raw, wolf_sheet,
    MobSheet, PlayerStats,
};
pub use sim::{fixture_scenario_1, match_published_rows, run_all, run_scenario, simulate_fight};
pub use types::{
    melee_auto_legal, tab_candidates, ActorId, Aggro, CanonicalHeading, CombatResources,
    HostileState, LivePlayer, Shaken, SpawnSeed, TargetLock, WorldCombat, WorldHostile,
};
pub use verbs::CombatVerb;

/// Published L1 Martial vs 1 wolf fixture used by playtester `combat`.
pub fn fixture_l1_martial_wolf() -> Result<serde_json::Value, String> {
    fixture_scenario_1()
}

use serde::{Deserialize, Serialize};

use crate::combat::math::{SHAKEN_S, START_ARROWS, START_POTIONS};

/// Last shrine = last hatch mouth (dungeon.rs). Persist as GlobalPlace fields.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LastShrine {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub yaw_degrees: f32,
}

/// Persist + live player combat. Sim fight state lives in [`sim`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CombatState {
    pub level: i32,
    pub xp: i32,
    pub discipline: Discipline,
    pub attrs: Attrs,
    pub ranks: Ranks,
    pub hp: f64,
    pub hp_max: f64,
    pub mana: f64,
    pub mana_max: f64,
    pub weapon_skill: i32,
    pub skill_cap: i32,
    pub skill_xp: i32,
    pub potions: i32,
    pub arrows: i32,
    pub lock: Option<u64>,
    pub shaken_until: f64,
    pub last_shrine: Option<LastShrine>,
}

impl CombatState {
    pub fn specialist(level: i32, discipline: Discipline) -> Self {
        let s = player_stats(level, discipline);
        Self {
            level: s.level,
            xp: 0,
            discipline: s.discipline,
            attrs: s.attrs,
            ranks: s.ranks,
            hp: f64::from(s.hp),
            hp_max: f64::from(s.hp),
            mana: f64::from(s.mana),
            mana_max: f64::from(s.mana),
            weapon_skill: s.weapon_skill,
            skill_cap: s.skill_cap,
            skill_xp: s.skill_xp,
            potions: START_POTIONS,
            arrows: START_ARROWS,
            lock: None,
            shaken_until: 0.0,
            last_shrine: None,
        }
    }

    pub fn create(discipline: Discipline) -> Self {
        Self::specialist(1, discipline)
    }

    pub fn is_shaken(&self) -> bool {
        self.shaken_until > 0.0
    }

    pub fn apply_death(&mut self) {
        self.hp = self.hp_max;
        self.mana = self.mana_max;
        self.shaken_until = SHAKEN_S;
        self.lock = None;
    }
}
