//! Data-driven live combat and canonical action resolution.

pub mod actions;
pub mod catalog;
pub mod lock;
pub mod log;
pub mod math;
pub mod types;

pub use log::CombatLog;
pub use math::{
    HEAR_AGGRO_M, LEASH_M, SIGHT_AGGRO_M, SPRINT_MPS, TAB_CONE_DEG, TAB_LOCK_M, TICK, WALK_MPS,
};
pub use types::{
    tab_candidates, ActorId, Aggro, CanonicalHeading, CombatHudAction, CombatHudActor,
    CombatHudSnapshot, CombatResources, HostilePresentationSource, HostileState, LivePlayer,
    PlayerSaveError, PlayerSaveSnapshot, Shaken, SpawnSeed, TargetLock, WorldCombat, WorldHostile,
};

use serde::{Deserialize, Serialize};

/// Last shrine = last hatch mouth (dungeon.rs). Persist as GlobalPlace fields.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LastShrine {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub yaw_degrees: f32,
}
