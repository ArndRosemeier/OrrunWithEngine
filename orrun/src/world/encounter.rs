//! Typed encounter composition and lifecycle authority.

use crate::combat::WorldCombat;
use crate::gamedata::MobId;

#[derive(Clone, Debug)]
pub struct HeldMobFixture {
    mob_id: MobId,
    forward_m: f64,
    right_m: f64,
}
impl HeldMobFixture {
    pub fn new(mob_id: MobId, forward_m: f64, right_m: f64) -> Self {
        assert!(
            forward_m.is_finite() && right_m.is_finite(),
            "held mob fixture offsets must be finite"
        );
        Self {
            mob_id,
            forward_m,
            right_m,
        }
    }
}
#[derive(Clone, Debug)]
pub enum EncounterPlan {
    NormalWorld,
    WolfLine,
    Held(Vec<HeldMobFixture>),
    Single { mob_id: MobId, held: bool },
    Bones,
    Mage,
    Orc,
    Yeti,
    Demon,
    BlueDemon,
    TribalVeteran,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EncounterTransition {
    Planned,
    Prepared,
    SeatedHeld,
    Active,
}
#[derive(Clone, Debug)]
pub struct EncounterDirector {
    plan: EncounterPlan,
    transition: EncounterTransition,
    seated: bool,
    skip_roster_pins: bool,
}
impl Default for EncounterDirector {
    fn default() -> Self {
        Self {
            plan: EncounterPlan::NormalWorld,
            transition: EncounterTransition::Active,
            seated: false,
            skip_roster_pins: false,
        }
    }
}
impl EncounterDirector {
    pub fn plan(&mut self, plan: EncounterPlan) {
        if matches!(plan, EncounterPlan::Held(ref mobs) if mobs.is_empty()) {
            panic!("held encounter requires at least one actor");
        }
        self.plan = plan;
        self.transition = EncounterTransition::Planned;
        self.seated = false;
    }
    pub fn transition(&self) -> EncounterTransition {
        self.transition
    }
    pub fn prepare(&mut self) {
        assert!(
            self.transition == EncounterTransition::Planned,
            "only a planned encounter can be prepared"
        );
        self.transition = EncounterTransition::Prepared;
    }
    pub(crate) fn mob_ids(&self) -> Vec<MobId> {
        match &self.plan {
            EncounterPlan::NormalWorld => Vec::new(),
            EncounterPlan::WolfLine => vec![MobId::new("wolf")],
            EncounterPlan::Held(mobs) => mobs.iter().map(|mob| mob.mob_id.clone()).collect(),
            EncounterPlan::Single { mob_id, .. } => vec![mob_id.clone()],
            EncounterPlan::Bones => vec![
                MobId::new("skeleton_warrior"),
                MobId::new("skeleton_minion"),
            ],
            EncounterPlan::Mage => vec![MobId::new("skeleton_mage")],
            EncounterPlan::Orc => vec![MobId::new("orc")],
            EncounterPlan::Yeti => vec![MobId::new("yeti")],
            EncounterPlan::Demon => vec![MobId::new("demon")],
            EncounterPlan::BlueDemon => vec![MobId::new("blue_demon")],
            EncounterPlan::TribalVeteran => vec![MobId::new("tribal_veteran")],
        }
    }
    pub fn is_seated(&self) -> bool {
        self.seated
    }
    pub fn is_held(&self) -> bool {
        self.transition == EncounterTransition::SeatedHeld
    }
    pub fn is_normal_world(&self) -> bool {
        matches!(self.plan, EncounterPlan::NormalWorld)
    }
    pub fn skip_roster_pins(&self) -> bool {
        self.skip_roster_pins
    }
    pub fn set_skip_roster_pins(&mut self, skip: bool) {
        self.skip_roster_pins = skip;
    }
    pub fn activate(&mut self, combat: &mut WorldCombat) {
        assert!(
            self.transition == EncounterTransition::SeatedHeld,
            "only a seated held encounter can activate"
        );
        combat.reset_for_fixture_start();
        self.transition = EncounterTransition::Active;
    }
    pub fn seat(
        &mut self,
        combat: &mut WorldCombat,
        player_x: f64,
        player_z: f64,
        facing_x: f64,
        facing_z: f64,
    ) {
        assert!(
            self.transition == EncounterTransition::Prepared,
            "encounter must be prepared before seating"
        );
        let length = facing_x.hypot(facing_z);
        assert!(
            length.is_finite() && length > 1e-9,
            "encounter requires finite non-zero facing"
        );
        let (fx, fz) = (facing_x / length, facing_z / length);
        let (rx, rz) = (-fz, fx);
        let specs: Vec<(MobId, f64, f64)> = match &self.plan {
            EncounterPlan::NormalWorld => Vec::new(),
            EncounterPlan::Single { mob_id, .. } => vec![(mob_id.clone(), 1.5, 0.0)],
            EncounterPlan::WolfLine => vec![
                (MobId::new("wolf"), 1.5, 0.0),
                (MobId::new("wolf"), 1.5, -1.8),
                (MobId::new("wolf"), 1.5, 1.8),
            ],
            EncounterPlan::Held(mobs) => mobs
                .iter()
                .map(|m| (m.mob_id.clone(), m.forward_m, m.right_m))
                .collect(),
            EncounterPlan::Bones => vec![
                (MobId::new("skeleton_warrior"), 1.5, 0.0),
                (MobId::new("skeleton_minion"), 1.5, -1.8),
                (MobId::new("skeleton_minion"), 1.5, 1.8),
            ],
            EncounterPlan::Mage => vec![(MobId::new("skeleton_mage"), 1.5, 0.0)],
            EncounterPlan::Orc => vec![(MobId::new("orc"), 1.5, 0.0)],
            EncounterPlan::Yeti => vec![(MobId::new("yeti"), 1.5, 0.0)],
            EncounterPlan::Demon => vec![(MobId::new("demon"), 1.5, 0.0)],
            EncounterPlan::BlueDemon => vec![(MobId::new("blue_demon"), 1.5, 0.0)],
            EncounterPlan::TribalVeteran => vec![(MobId::new("tribal_veteran"), 1.5, 0.0)],
        };
        for (mob, _, _) in &specs {
            assert!(
                combat.game_data().mob(mob).is_some(),
                "encounter references unknown mob {mob}"
            );
        }
        combat.clear_hostiles();
        combat.reset_encounter_state();
        for (index, (mob, forward, right)) in specs.into_iter().enumerate() {
            let x = player_x + fx * forward + rx * right;
            let z = player_z + fz * forward + rz * right;
            combat.add_arena_mob(
                &mob,
                i32::try_from(index).expect("encounter actor count"),
                x,
                z,
                x,
                z,
            );
        }
        self.seated = true;
        self.transition = if matches!(
            self.plan,
            EncounterPlan::Held(_)
                | EncounterPlan::WolfLine
                | EncounterPlan::Single { held: true, .. }
        ) {
            EncounterTransition::SeatedHeld
        } else {
            EncounterTransition::Active
        };
    }
}

// Dungeon encounter roster composition belongs with encounter authority.
const STRAFE_M: f64 = 1.8;
fn is_bone_id(id: &str) -> bool {
    matches!(id, "skeleton_warrior" | "skeleton_minion" | "skeleton_mage")
}
pub fn seat_dungeon_skulls(combat: &mut WorldCombat, spots: &[engine::space::GlobalXZ]) {
    let next = combat.hostiles().iter().map(|h| h.idx).max().unwrap_or(-1) + 1;
    for (idx, p) in (next..).zip(spots.iter()) {
        combat.add_hostile(combat.hostile_metadata(
            &crate::gamedata::MobId::new("orc_skull"),
            idx,
            p.x,
            p.z,
            p.x,
            p.z,
        ));
    }
}

pub fn clear_dungeon_skulls(combat: &mut WorldCombat) {
    combat.retain_hostiles(|h| h.mob_id != "orc_skull");
    if let Some(lock) = combat.lock_id() {
        if !combat.hostiles().iter().any(|h| h.idx == lock) {
            combat.set_lock(None);
        }
    }
}

pub fn seat_dungeon_bones(
    combat: &mut WorldCombat,
    spots: &[engine::space::GlobalXZ],
    heart: Option<engine::space::GlobalXZ>,
) {
    let mut idx = combat.hostiles().iter().map(|h| h.idx).max().unwrap_or(-1) + 1;

    let (fx, fz) = (1.0, 0.0);
    let (sx, sz) = (-fz, fx);
    for p in spots {
        combat.add_hostile(combat.hostile_metadata(
            &crate::gamedata::MobId::new("skeleton_warrior"),
            idx,
            p.x,
            p.z,
            p.x,
            p.z,
        ));
        idx += 1;
        combat.add_hostile(combat.hostile_metadata(
            &crate::gamedata::MobId::new("skeleton_minion"),
            idx,
            p.x + sx * -STRAFE_M,
            p.z + sz * -STRAFE_M,
            p.x + sx * -STRAFE_M,
            p.z + sz * -STRAFE_M,
        ));
        idx += 1;
        combat.add_hostile(combat.hostile_metadata(
            &crate::gamedata::MobId::new("skeleton_minion"),
            idx,
            p.x + sx * STRAFE_M,
            p.z + sz * STRAFE_M,
            p.x + sx * STRAFE_M,
            p.z + sz * STRAFE_M,
        ));
        idx += 1;
    }
    let mage_at = spots.iter().find(|p| {
        heart
            .map(|h| (p.x - h.x).hypot(p.z - h.z) > 0.05)
            .unwrap_or(spots.len() > 1)
    });
    if let Some(p) = mage_at {
        combat.add_hostile(combat.hostile_metadata(
            &crate::gamedata::MobId::new("skeleton_mage"),
            idx,
            p.x + fx * STRAFE_M,
            p.z + fz * STRAFE_M,
            p.x + fx * STRAFE_M,
            p.z + fz * STRAFE_M,
        ));
    }
}

pub fn clear_dungeon_bones(combat: &mut WorldCombat) {
    combat.retain_hostiles(|h| !is_bone_id(&h.mob_id));
    if let Some(lock) = combat.lock_id() {
        if !combat.hostiles().iter().any(|h| h.idx == lock) {
            combat.set_lock(None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    fn combat() -> WorldCombat {
        WorldCombat::with_game_data(Arc::new(
            crate::gamedata::GameData::load(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../data/OrrunGameData.xml"),
            )
            .unwrap(),
        ))
    }
    #[test]
    fn invalid_plan_is_rejected_before_roster_mutation() {
        let mut c = combat();
        c.add_arena_mob(&MobId::new("wolf"), 0, 0.0, 0.0, 0.0, 0.0);
        let before = c.hostiles()[0].actor_id();
        let mut d = EncounterDirector::default();
        d.plan(EncounterPlan::Single {
            mob_id: MobId::new("missing"),
            held: false,
        });
        d.prepare();
        let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            d.seat(&mut c, 0.0, 0.0, 1.0, 0.0)
        }));
        assert!(failed.is_err());
        assert_eq!(c.hostiles()[0].actor_id(), before);
    }
    #[test]
    fn held_transition_is_typed() {
        let mut c = combat();
        let mut d = EncounterDirector::default();
        d.plan(EncounterPlan::WolfLine);
        d.prepare();
        d.seat(&mut c, 0.0, 0.0, 1.0, 0.0);
        assert_eq!(d.transition(), EncounterTransition::SeatedHeld);
        d.activate(&mut c);
        assert_eq!(d.transition(), EncounterTransition::Active);
    }
}
