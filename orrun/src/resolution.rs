//! Canonical action resolution.
//!
//! M2: the new combat heart as a headless, typed pipeline. One resolver serves
//! players and mobs. It reads action and effect definitions from `GameData`,
//! validates target class and application geometry, spends resources, applies
//! damage and heal through centralized operations, records progression, and
//! emits typed result events. It never branches on a concrete action ID to
//! select game rules.
//!
//! This module is deliberately independent of the legacy `combat` god-object;
//! M3 wires it into the live loop and M6 removes the legacy path. Control,
//! defense, movement, and utility effect kinds are loud errors here until M5.

use thiserror::Error;

use crate::gamedata::{
    ActionId, ActionTarget, Application, EffectId, EffectKind, GameData, SkillId,
};
use crate::progression::{balance, ActorProgression, LevelUpEvent};

/// Why an action request failed. Every failure is loud; there is no silent
/// fallback that hides a targeting, resource, or data error.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum ResolutionError {
    #[error("unknown action {0}")]
    UnknownAction(ActionId),
    #[error("unknown effect {0}")]
    UnknownEffect(EffectId),
    #[error("unsupported effect kind {kind:?} for effect {effect} (not implemented yet)")]
    UnsupportedEffectKind { kind: EffectKind, effect: EffectId },
    #[error("actor {0} does not know skill {1}")]
    UnknownSkill(u32, SkillId),
    #[error("no valid target for action {0}")]
    NoTarget(ActionId),
    #[error("target out of range for action {0}")]
    OutOfRange(ActionId),
    #[error("action {action} needs {need} mana, actor has {have}")]
    InsufficientMana {
        action: ActionId,
        need: f64,
        have: f64,
    },
    #[error("actor index {0} is out of bounds")]
    InvalidActorIndex(usize),
    #[error("actor {0} is dead")]
    DeadActor(u32),
    #[error("single_target action requires an explicit target")]
    MissingExplicitTarget,
}

/// How the caller names a target for the action.
#[derive(Debug, Clone, PartialEq)]
pub enum TargetSelection {
    /// An explicit actor index, used by `single_target` applications.
    Single(usize),
    /// Area/cone selection resolved from the caster's position and facing.
    Area { facing_x: f64, facing_z: f64 },
}

/// A mutable combat participant. Players and mobs use the same shape.
#[derive(Debug, Clone)]
pub struct Actor {
    pub id: u32,
    pub name: String,
    pub faction: crate::gamedata::FactionId,
    pub x: f64,
    pub z: f64,
    pub armor: i32,
    pub hp: f64,
    pub mana: f64,
    pub alive: bool,
    pub progression: ActorProgression,
}

impl Actor {
    pub fn hp_max(&self) -> f64 {
        self.progression.hp_max()
    }

    pub fn mana_max(&self) -> f64 {
        self.progression.mana_max()
    }

    pub fn skill_level(&self, id: &SkillId) -> Option<i32> {
        self.progression.skill_level(id)
    }

    /// Spend mana if available. Zero or negative amounts always succeed.
    pub fn spend_mana(&mut self, amount: f64) -> bool {
        if amount <= 0.0 {
            return true;
        }
        if self.mana < amount {
            return false;
        }
        self.mana -= amount;
        true
    }

    /// Heal and return the amount actually restored. Dead actors do not heal.
    pub fn heal(&mut self, amount: f64) -> f64 {
        if !self.alive || amount <= 0.0 {
            return 0.0;
        }
        let before = self.hp;
        self.hp = (self.hp + amount).min(self.hp_max());
        self.hp - before
    }

    /// Apply post-mitigation damage; returns the HP actually removed. Death is
    /// a centralized transition here, never a caller decision.
    pub fn take_damage(&mut self, amount: f64) -> f64 {
        if !self.alive || amount <= 0.0 {
            return 0.0;
        }
        let before = self.hp;
        self.hp = (self.hp - amount).max(0.0);
        if self.hp <= 0.0 {
            self.alive = false;
        }
        before - self.hp
    }
}

/// One executed effect assignment, with per-target applied amounts.
#[derive(Debug, Clone, PartialEq)]
pub struct AppliedEffect {
    pub effect_id: EffectId,
    pub skill_id: SkillId,
    pub kind: EffectKind,
    pub application: Application,
    /// Pre-mitigation magnitude at the caster's current skill level.
    pub magnitude: f64,
    /// Target actor indices, in application order.
    pub targets: Vec<usize>,
    /// Per-target amount actually applied (damage dealt / HP healed).
    pub applied: Vec<f64>,
}

/// The complete, typed result of resolving one action.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolution {
    pub action_id: ActionId,
    pub caster: usize,
    pub effects: Vec<AppliedEffect>,
    pub mana_spent: f64,
    pub deaths: Vec<usize>,
    pub level_ups: Vec<LevelUpEvent>,
}

struct PlannedEffect {
    effect_id: EffectId,
    skill_id: SkillId,
    kind: EffectKind,
    application: Application,
    magnitude: f64,
    targets: Vec<usize>,
}

/// Centralized mitigation: post-mitigation damage from raw magnitude and armor.
/// This is the authoritative formula for the new pipeline; the legacy
/// `combat::math::mitigation` remains only until M6 removes it.
pub fn mitigate(raw: f64, armor: i32) -> f64 {
    if raw <= 0.0 {
        return 0.0;
    }
    raw * 100.0 / (100.0 + armor.max(0) as f64)
}

/// Resolves actions from GameData against a mutable set of actors.
pub struct Resolver<'a> {
    data: &'a GameData,
}

impl<'a> Resolver<'a> {
    pub fn new(data: &'a GameData) -> Self {
        Self { data }
    }

    /// Validate, spend, apply, and record one action in the canonical order:
    /// resolve actor/action, validate target/range/resources, spend resources,
    /// apply effects, record progression, emit results.
    pub fn execute(
        &self,
        actors: &mut [Actor],
        caster: usize,
        action_id: &ActionId,
        selection: TargetSelection,
    ) -> Result<Resolution, ResolutionError> {
        let action = self
            .data
            .action(action_id)
            .ok_or_else(|| ResolutionError::UnknownAction(action_id.clone()))?;
        if caster >= actors.len() {
            return Err(ResolutionError::InvalidActorIndex(caster));
        }
        if !actors[caster].alive {
            return Err(ResolutionError::DeadActor(actors[caster].id));
        }

        // Phase 1 â€” plan: resolve every assignment to concrete commands without
        // mutating actor state, so a failed action never half-applies.
        let mut planned = Vec::with_capacity(action.effects().len());
        for assignment in action.effects() {
            let effect = self
                .data
                .effect(assignment.effect_id())
                .ok_or_else(|| ResolutionError::UnknownEffect(assignment.effect_id().clone()))?;
            match effect.kind() {
                EffectKind::Damage | EffectKind::Heal => {}
                kind => {
                    return Err(ResolutionError::UnsupportedEffectKind {
                        kind,
                        effect: effect.id().clone(),
                    })
                }
            }
            let skill_id = effect.skill_id();
            let level = actors[caster].skill_level(skill_id).ok_or_else(|| {
                ResolutionError::UnknownSkill(actors[caster].id, skill_id.clone())
            })?;
            let level_scale = self
                .data
                .skill(skill_id)
                .map(|s| s.level_scale())
                .unwrap_or(1.0);
            let magnitude = balance::effect_magnitude(
                assignment.magnitude(),
                level,
                level_scale,
                effect.progression(),
            );
            let targets = select_targets(
                self.data,
                actors,
                caster,
                action_id,
                action.target(),
                assignment.application(),
                assignment.range_m(),
                assignment.radius_m(),
                assignment.angle_deg(),
                selection.clone(),
            )?;
            if targets.is_empty() {
                return Err(ResolutionError::NoTarget(action_id.clone()));
            }
            planned.push(PlannedEffect {
                effect_id: effect.id().clone(),
                skill_id: skill_id.clone(),
                kind: effect.kind(),
                application: assignment.application(),
                magnitude,
                targets,
            });
        }

        // Phase 2 â€” spend resources once per action.
        let mut mana_spent = 0.0;
        if action.mana_cost() > 0.0 {
            if !actors[caster].spend_mana(action.mana_cost()) {
                return Err(ResolutionError::InsufficientMana {
                    action: action_id.clone(),
                    need: action.mana_cost(),
                    have: actors[caster].mana,
                });
            }
            mana_spent = action.mana_cost();
        }

        let mut level_ups = Vec::new();
        if mana_spent > 0.0 {
            level_ups.extend(actors[caster].progression.record_mana_spent(mana_spent));
        }

        // Phase 3 â€” apply and record.
        let mut effects = Vec::with_capacity(planned.len());
        let mut deaths = Vec::new();
        for p in planned {
            level_ups.extend(
                actors[caster]
                    .progression
                    .record_effect_use(&p.skill_id)
                    .map_err(|_| {
                        ResolutionError::UnknownSkill(actors[caster].id, p.skill_id.clone())
                    })?,
            );
            let mut applied = Vec::with_capacity(p.targets.len());
            for &target in &p.targets {
                let amount = match p.kind {
                    EffectKind::Damage => {
                        let raw = mitigate(p.magnitude, actors[target].armor);
                        let dealt = actors[target].take_damage(raw);
                        if dealt > 0.0 {
                            level_ups.extend(actors[target].progression.record_damage_taken(dealt));
                        }
                        if !actors[target].alive && !deaths.contains(&target) {
                            deaths.push(target);
                        }
                        dealt
                    }
                    EffectKind::Heal => actors[target].heal(p.magnitude),
                    _ => unreachable!("effect kind validated during planning"),
                };
                applied.push(amount);
            }
            effects.push(AppliedEffect {
                effect_id: p.effect_id,
                skill_id: p.skill_id,
                kind: p.kind,
                application: p.application,
                magnitude: p.magnitude,
                targets: p.targets,
                applied,
            });
        }

        Ok(Resolution {
            action_id: action_id.clone(),
            caster,
            effects,
            mana_spent,
            deaths,
            level_ups,
        })
    }
}

fn class_matches(
    data: &GameData,
    target_class: ActionTarget,
    caster_faction: &crate::gamedata::FactionId,
    target_faction: &crate::gamedata::FactionId,
    is_self: bool,
) -> bool {
    match target_class {
        ActionTarget::Hostile => data.factions_are_hostile(caster_faction, target_faction),
        ActionTarget::Friendly => !data.factions_are_hostile(caster_faction, target_faction),
        ActionTarget::ActorSelf => is_self,
        ActionTarget::Any => true,
        ActionTarget::None => false,
    }
}

fn dist(ax: f64, az: f64, bx: f64, bz: f64) -> f64 {
    let dx = bx - ax;
    let dz = bz - az;
    (dx * dx + dz * dz).sqrt()
}

fn in_cone(cx: f64, cz: f64, fx: f64, fz: f64, px: f64, pz: f64, half_rad: f64) -> bool {
    let dx = px - cx;
    let dz = pz - cz;
    let d = (dx * dx + dz * dz).sqrt();
    if d <= 1e-9 {
        return true;
    }
    let fl = (fx * fx + fz * fz).sqrt();
    if fl <= 1e-9 {
        return false;
    }
    let dot = ((fx / fl) * (dx / d) + (fz / fl) * (dz / d)).clamp(-1.0, 1.0);
    dot.acos() <= half_rad
}

/// Resolve application geometry into a list of target indices, filtered by the
/// action's target class and living state.
fn select_targets(
    data: &GameData,
    actors: &[Actor],
    caster: usize,
    action_id: &ActionId,
    target_class: ActionTarget,
    application: Application,
    range_m: f64,
    radius_m: f64,
    angle_deg: f64,
    selection: TargetSelection,
) -> Result<Vec<usize>, ResolutionError> {
    let c = &actors[caster];
    let mut out = Vec::new();
    match application {
        Application::SingleTarget => {
            let idx = match selection {
                TargetSelection::Single(idx) => idx,
                TargetSelection::Area { .. } => return Err(ResolutionError::MissingExplicitTarget),
            };
            let Some(t) = actors.get(idx).filter(|t| t.alive) else {
                return Ok(out);
            };
            if !class_matches(data, target_class, &c.faction, &t.faction, idx == caster) {
                return Ok(out);
            }
            if dist(c.x, c.z, t.x, t.z) > range_m {
                return Err(ResolutionError::OutOfRange(action_id.clone()));
            }
            out.push(idx);
        }
        Application::Cone => {
            let (fx, fz) = match &selection {
                TargetSelection::Area { facing_x, facing_z } => (*facing_x, *facing_z),
                TargetSelection::Single(_) => return Err(ResolutionError::MissingExplicitTarget),
            };
            let half = (angle_deg / 2.0).to_radians();
            for (i, t) in actors.iter().enumerate() {
                if !t.alive {
                    continue;
                }
                if dist(c.x, c.z, t.x, t.z) > range_m {
                    continue;
                }
                if !in_cone(c.x, c.z, fx, fz, t.x, t.z, half) {
                    continue;
                }
                if class_matches(data, target_class, &c.faction, &t.faction, i == caster) {
                    out.push(i);
                }
            }
        }
        Application::Aoe => {
            let (cx, cz) = match &selection {
                TargetSelection::Area { facing_x, facing_z } => {
                    (c.x + facing_x * range_m, c.z + facing_z * range_m)
                }
                TargetSelection::Single(_) => return Err(ResolutionError::MissingExplicitTarget),
            };
            for (i, t) in actors.iter().enumerate() {
                if !t.alive {
                    continue;
                }
                if dist(cx, cz, t.x, t.z) > radius_m {
                    continue;
                }
                if class_matches(data, target_class, &c.faction, &t.faction, i == caster) {
                    out.push(i);
                }
            }
        }
        Application::Pbaoe => {
            for (i, t) in actors.iter().enumerate() {
                if !t.alive {
                    continue;
                }
                if dist(c.x, c.z, t.x, t.z) > radius_m {
                    continue;
                }
                if class_matches(data, target_class, &c.faction, &t.faction, i == caster) {
                    out.push(i);
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gamedata::{FactionId, MobId, ProfileId};

    fn data() -> GameData {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../data/OrrunGameData.xml");
        GameData::load(path).expect("canonical data")
    }

    fn player(data: &GameData) -> Actor {
        let profile = data
            .profile(&ProfileId::new("default_player"))
            .expect("default player");
        let progression = ActorProgression::from_profile(profile);
        let hp = progression.hp_max();
        let mana = progression.mana_max();
        Actor {
            id: 0,
            name: "Adventurer".into(),
            faction: FactionId::new("citizen"),
            x: 0.0,
            z: 0.0,
            armor: 0,
            hp,
            mana,
            alive: true,
            progression,
        }
    }

    fn wolf(data: &GameData, x: f64, z: f64) -> Actor {
        let mob = data.mob(&MobId::new("wolf")).expect("wolf mob");
        let progression = ActorProgression::from_mob(mob, data);
        Actor {
            id: 1,
            name: "wolf-spider".into(),
            faction: FactionId::new("wild"),
            x,
            z,
            armor: mob.armor(),
            hp: mob.hp() as f64,
            mana: progression.mana_max(),
            alive: true,
            progression,
        }
    }

    #[test]
    fn strike_deals_mitigated_damage_and_trains_skill() {
        let data = data();
        let mut actors = vec![player(&data), wolf(&data, 1.5, 0.0)];
        let res = Resolver::new(&data)
            .execute(
                &mut actors,
                0,
                &ActionId::new("strike"),
                TargetSelection::Single(1),
            )
            .expect("strike resolves");
        assert_eq!(res.effects.len(), 1);
        assert_eq!(res.effects[0].applied, vec![8.0]);
        assert_eq!(actors[1].hp, 62.0);
        assert_eq!(
            actors[0]
                .progression
                .skill_xp(&SkillId::new("slashing_damage")),
            Some(10)
        );
        assert!(
            res.level_ups.is_empty(),
            "no level-up at skill 1 from one use"
        );
    }

    #[test]
    fn fire_bolt_spends_mana_and_trains_fire_and_mana() {
        let data = data();
        let mut actors = vec![player(&data), wolf(&data, 10.0, 0.0)];
        let mana_before = actors[0].mana;
        let res = Resolver::new(&data)
            .execute(
                &mut actors,
                0,
                &ActionId::new("fire_bolt"),
                TargetSelection::Single(1),
            )
            .expect("fire_bolt resolves");
        assert_eq!(res.mana_spent, 3.0);
        assert_eq!(actors[0].mana, mana_before - 3.0);
        assert_eq!(res.effects[0].applied, vec![14.0]);
        assert_eq!(
            actors[0].progression.skill_xp(&SkillId::new("fire_damage")),
            Some(10)
        );
        assert_eq!(actors[0].progression.mana_xp(), 3);
    }

    #[test]
    fn mend_heals_self_trains_healing_and_spends_mana() {
        let data = data();
        let mut actors = vec![player(&data), wolf(&data, 1.5, 0.0)];
        actors[0].hp = 50.0;
        let res = Resolver::new(&data)
            .execute(
                &mut actors,
                0,
                &ActionId::new("mend"),
                TargetSelection::Single(0),
            )
            .expect("mend resolves");
        assert_eq!(res.effects[0].applied, vec![25.0]);
        assert_eq!(actors[0].hp, 75.0);
        assert_eq!(
            actors[0].progression.skill_xp(&SkillId::new("healing")),
            Some(10)
        );
        assert_eq!(actors[0].progression.mana_xp(), 20);
        assert_eq!(actors[0].mana, actors[0].mana_max() - 20.0);
    }

    #[test]
    fn damage_taken_trains_hp() {
        let data = data();
        // Wolf uses the same resolver and the same strike action.
        let mut actors = vec![player(&data), wolf(&data, 1.5, 0.0)];
        let hp_xp_before = actors[0].progression.hp_xp();
        Resolver::new(&data)
            .execute(
                &mut actors,
                1,
                &ActionId::new("strike"),
                TargetSelection::Single(0),
            )
            .expect("wolf strike resolves");
        assert_eq!(actors[0].hp, actors[0].hp_max() - 8.0);
        assert!(actors[0].progression.hp_xp() > hp_xp_before);
        // The wolf trains its own slashing skill through the same pipeline.
        assert_eq!(
            actors[1]
                .progression
                .skill_xp(&SkillId::new("slashing_damage")),
            Some(10)
        );
    }

    #[test]
    fn multi_effect_action_resolves_every_assignment_and_trains_each_skill() {
        let xml = r#"<OrrunGameData schema_version="2"><skills><skill id="slashing_damage" name="Slashing" level_scale="1"/><skill id="fire_damage" name="Fire" level_scale="1"/></skills><factions><faction id="neutral" neutral="true"/><faction id="citizen" neutral="false"/><faction id="wild" neutral="false"/></factions><effects><effect id="slashing_damage" name="Slashing" kind="damage" skill_id="slashing_damage" progression="skill_level"/><effect id="fire_damage" name="Fire" kind="damage" skill_id="fire_damage" progression="skill_level"/></effects><actions><action id="flame_strike" name="Flame Strike" target="hostile"><effects><effect effect_id="slashing_damage" magnitude="5" application="single_target" range_m="2"/><effect effect_id="fire_damage" magnitude="7" application="single_target" range_m="2"/></effects></action></actions><players><profile id="default_player" name="Adventurer" faction="citizen"><skill id="slashing_damage" level="1"/><skill id="fire_damage" level="1"/></profile></players><mobs><mob id="wolf" name="Wolf" faction="wild" mode="active" hp="70" armor="0" damage="10" movement_id="walk" speed_variance_ratio="0.05" endurance_s="30"><action id="flame_strike"/></mob></mobs><movement><spec id="walk" speed_mps="2.5"/></movement><hamlet enabled="true"/><defaults/></OrrunGameData>"#;
        let data = GameData::from_xml_str(xml).expect("fixture data");
        let profile = data
            .profile(&ProfileId::new("default_player"))
            .expect("profile");
        let progression = ActorProgression::from_profile(profile);
        let mob = data.mob(&MobId::new("wolf")).expect("mob");
        let target_progression = ActorProgression::from_mob(mob, &data);
        let mut actors = vec![
            Actor {
                id: 0,
                name: "Adventurer".into(),
                faction: FactionId::new("citizen"),
                x: 0.0,
                z: 0.0,
                armor: 0,
                hp: progression.hp_max(),
                mana: progression.mana_max(),
                alive: true,
                progression,
            },
            Actor {
                id: 1,
                name: "Wolf".into(),
                faction: FactionId::new("wild"),
                x: 1.5,
                z: 0.0,
                armor: 0,
                hp: 70.0,
                mana: target_progression.mana_max(),
                alive: true,
                progression: target_progression,
            },
        ];
        let res = Resolver::new(&data)
            .execute(
                &mut actors,
                0,
                &ActionId::new("flame_strike"),
                TargetSelection::Single(1),
            )
            .expect("multi-effect action resolves");
        assert_eq!(res.effects.len(), 2);
        assert_eq!(res.effects[0].applied, vec![5.0]);
        assert_eq!(res.effects[1].applied, vec![7.0]);
        assert_eq!(actors[1].hp, 58.0);
        assert_eq!(
            actors[0]
                .progression
                .skill_xp(&SkillId::new("slashing_damage")),
            Some(10)
        );
        assert_eq!(
            actors[0].progression.skill_xp(&SkillId::new("fire_damage")),
            Some(10)
        );
    }

    #[test]
    fn authored_action_magnitude_ab_changes_resolution_without_code_changes() {
        let base = r#"<OrrunGameData schema_version="2"><skills><skill id="slashing_damage" name="Slashing" level_scale="1"/></skills><factions><faction id="neutral" neutral="true"/><faction id="citizen" neutral="false"/><faction id="wild" neutral="false"/></factions><effects><effect id="slashing_damage" name="Slashing" kind="damage" skill_id="slashing_damage" progression="skill_level"/></effects><actions><action id="strike" name="Strike" target="hostile"><effects><effect effect_id="slashing_damage" magnitude="MAGNITUDE" application="single_target" range_m="2"/></effects></action></actions><players><profile id="default_player" name="Adventurer" faction="citizen"><skill id="slashing_damage" level="1"/></profile></players><mobs><mob id="wolf" name="Wolf" faction="wild" mode="active" hp="70" armor="0" damage="10" movement_id="walk" speed_variance_ratio="0.05" endurance_s="30"><action id="strike"/></mob></mobs><movement><spec id="walk" speed_mps="2.5"/></movement><hamlet enabled="true"/><defaults/></OrrunGameData>"#;
        let resolve = |magnitude: &str| {
            let data = GameData::from_xml_str(&base.replace("MAGNITUDE", magnitude))
                .expect("temporary authored variant");
            let mut actors = vec![player(&data), wolf(&data, 1.5, 0.0)];
            Resolver::new(&data)
                .execute(
                    &mut actors,
                    0,
                    &ActionId::new("strike"),
                    TargetSelection::Single(1),
                )
                .expect("variant resolves")
                .effects[0]
                .applied[0]
        };
        assert_eq!(resolve("8"), 8.0);
        assert_eq!(resolve("19"), 19.0);
    }

    #[test]
    fn unsupported_effect_kind_is_a_loud_error() {
        let xml = r#"<OrrunGameData schema_version="2"><skills><skill id="root" name="Root" level_scale="1"/></skills><factions><faction id="neutral" neutral="true"/><faction id="citizen" neutral="false"/><faction id="wild" neutral="false"/></factions><effects><effect id="root" name="Root" kind="control" skill_id="root" progression="skill_level"/></effects><actions><action id="bind" name="Bind" target="hostile"><effects><effect effect_id="root" magnitude="1" application="single_target" range_m="24"/></effects></action></actions><players><profile id="default_player" name="Adventurer" faction="citizen"><skill id="root" level="1"/></profile></players><mobs><mob id="wolf" name="Wolf" faction="wild" mode="active" hp="70" armor="0" damage="10" movement_id="walk" speed_variance_ratio="0.05" endurance_s="30"><action id="bind"/></mob></mobs><movement><spec id="walk" speed_mps="2.5"/></movement><hamlet enabled="true"/><defaults/></OrrunGameData>"#;
        let data = GameData::from_xml_str(xml).expect("fixture data");
        let profile = data
            .profile(&ProfileId::new("default_player"))
            .expect("profile");
        let progression = ActorProgression::from_profile(profile);
        let mut actors = vec![
            Actor {
                id: 0,
                name: "Adventurer".into(),
                faction: FactionId::new("citizen"),
                x: 0.0,
                z: 0.0,
                armor: 0,
                hp: progression.hp_max(),
                mana: progression.mana_max(),
                alive: true,
                progression,
            },
            wolf(&data, 1.5, 0.0),
        ];
        let err = Resolver::new(&data)
            .execute(
                &mut actors,
                0,
                &ActionId::new("bind"),
                TargetSelection::Single(1),
            )
            .unwrap_err();
        assert!(matches!(
            err,
            ResolutionError::UnsupportedEffectKind {
                kind: EffectKind::Control,
                ..
            }
        ));
    }

    #[test]
    fn out_of_range_rejected_without_spending_mana() {
        let data = data();
        let mut actors = vec![player(&data), wolf(&data, 40.0, 0.0)];
        let mana_before = actors[0].mana;
        let err = Resolver::new(&data)
            .execute(
                &mut actors,
                0,
                &ActionId::new("fire_bolt"),
                TargetSelection::Single(1),
            )
            .unwrap_err();
        assert_eq!(err, ResolutionError::OutOfRange(ActionId::new("fire_bolt")));
        assert_eq!(actors[0].mana, mana_before);
    }

    #[test]
    fn insufficient_mana_rejected_before_application() {
        let data = data();
        let mut actors = vec![player(&data), wolf(&data, 1.5, 0.0)];
        actors[0].mana = 1.0;
        let err = Resolver::new(&data)
            .execute(
                &mut actors,
                0,
                &ActionId::new("mend"),
                TargetSelection::Single(0),
            )
            .unwrap_err();
        assert!(matches!(
            err,
            ResolutionError::InsufficientMana { need: 20.0, .. }
        ));
        assert_eq!(actors[0].mana, 1.0);
    }

    #[test]
    fn death_transition_emits_event_and_stops_damage() {
        let data = data();
        let mut actors = vec![player(&data), wolf(&data, 1.5, 0.0)];
        actors[1].hp = 5.0;
        let res = Resolver::new(&data)
            .execute(
                &mut actors,
                0,
                &ActionId::new("strike"),
                TargetSelection::Single(1),
            )
            .expect("strike resolves");
        assert!(!actors[1].alive);
        assert_eq!(res.deaths, vec![1]);
        // A dead actor no longer resolves damage.
        let err = Resolver::new(&data)
            .execute(
                &mut actors,
                0,
                &ActionId::new("strike"),
                TargetSelection::Single(1),
            )
            .unwrap_err();
        assert_eq!(err, ResolutionError::NoTarget(ActionId::new("strike")));
    }

    #[test]
    fn invalid_caster_index_is_a_typed_error() {
        let data = data();
        let mut actors = vec![player(&data)];
        let err = Resolver::new(&data)
            .execute(
                &mut actors,
                99,
                &ActionId::new("strike"),
                TargetSelection::Single(0),
            )
            .unwrap_err();
        assert_eq!(err, ResolutionError::InvalidActorIndex(99));
    }

    #[test]
    fn cone_geometry_selects_facing_targets_only() {
        let data = data();
        let mut actors = vec![player(&data)];
        let mut add = |id, x, z| {
            let mob = data.mob(&MobId::new("wolf")).unwrap();
            let progression = ActorProgression::from_mob(mob, &data);
            actors.push(Actor {
                id,
                name: "wolf".into(),
                faction: FactionId::new("wild"),
                x,
                z,
                armor: 0,
                hp: 70.0,
                mana: progression.mana_max(),
                alive: true,
                progression,
            });
        };
        add(1, 2.0, 0.0); // straight ahead, in cone
        add(2, 0.0, 2.0); // 90Â° off, out of cone
        let got = select_targets(
            &data,
            &actors,
            0,
            &ActionId::new("strike"),
            ActionTarget::Hostile,
            Application::Cone,
            3.0,
            0.0,
            60.0,
            TargetSelection::Area {
                facing_x: 1.0,
                facing_z: 0.0,
            },
        )
        .unwrap();
        assert_eq!(got, vec![1]);
    }

    #[test]
    fn pbaoe_geometry_uses_caster_radius() {
        let data = data();
        let mut actors = vec![player(&data)];
        let mob = data.mob(&MobId::new("wolf")).unwrap();
        let progression = ActorProgression::from_mob(mob, &data);
        for (id, x, z) in [(1, 1.0, 0.0), (2, 5.0, 0.0)] {
            actors.push(Actor {
                id,
                name: "wolf".into(),
                faction: FactionId::new("wild"),
                x,
                z,
                armor: 0,
                hp: 70.0,
                mana: progression.mana_max(),
                alive: true,
                progression: progression.clone(),
            });
        }
        let got = select_targets(
            &data,
            &actors,
            0,
            &ActionId::new("strike"),
            ActionTarget::Hostile,
            Application::Pbaoe,
            0.0,
            2.0,
            0.0,
            TargetSelection::Area {
                facing_x: 1.0,
                facing_z: 0.0,
            },
        )
        .unwrap();
        assert_eq!(got, vec![1]);
    }
}
