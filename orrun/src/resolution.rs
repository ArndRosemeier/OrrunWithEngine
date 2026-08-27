//! Canonical, data-driven action resolution for players and mobs.
//!
//! The resolver validates authored actions and targets, spends resources, applies
//! centralized effects, records progression, and emits typed result events.

use std::collections::BTreeMap;

use thiserror::Error;

use crate::gamedata::{
    ActionId, ActionTarget, Application, EffectId, EffectOperation, GameData, SkillId,
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
    #[error("unsupported effect kind {operation:?} for effect {effect} (not implemented yet)")]
    UnsupportedEffectOperation {
        operation: EffectOperation,
        effect: EffectId,
    },
    #[error("actor {0} does not know skill {1}")]
    UnknownSkill(u32, SkillId),
    #[error("actor {actor} knows skill {skill}, but its canonical definition is missing")]
    MissingCanonicalSkill { actor: u32, skill: SkillId },
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

/// Stable canonical identity for an actor referenced by resolution events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct ResolutionActorId {
    canonical: u32,
}

impl ResolutionActorId {
    pub const fn new(canonical: u32) -> Self {
        Self { canonical }
    }
    pub const fn get(self) -> u32 {
        self.canonical
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub enum TimedStatusKind {
    Root,
    Hold,
    Snare,
    Charm,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TimedStatus {
    kind: TimedStatusKind,
    remaining_s: f64,
    movement_multiplier: f64,
    source: ResolutionActorId,
    charm_faction: Option<crate::gamedata::FactionId>,
}
impl TimedStatus {
    pub const fn kind(&self) -> TimedStatusKind {
        self.kind
    }
    pub fn remaining_s(&self) -> f64 {
        self.remaining_s
    }
    pub fn movement_multiplier(&self) -> f64 {
        self.movement_multiplier
    }
    pub const fn source(&self) -> ResolutionActorId {
        self.source
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActorCast {
    action_id: ActionId,
    target: Option<ResolutionActorId>,
    remaining_s: f64,
    total_s: f64,
}
impl ActorCast {
    pub fn action_id(&self) -> &ActionId {
        &self.action_id
    }
    pub fn target(&self) -> Option<ResolutionActorId> {
        self.target
    }
    pub fn remaining_s(&self) -> f64 {
        self.remaining_s
    }
    pub fn total_s(&self) -> f64 {
        self.total_s
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldCastOutcome {
    NoCast,
    Interrupted,
    UninterruptibleContinues,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActorActionState {
    roster: Vec<ActionId>,
    cooldowns: BTreeMap<ActionId, f64>,
    cast: Option<ActorCast>,
}
impl ActorActionState {
    pub fn new(roster: Vec<ActionId>) -> Self {
        Self {
            roster,
            cooldowns: BTreeMap::new(),
            cast: None,
        }
    }
    pub fn roster(&self) -> &[ActionId] {
        &self.roster
    }
    pub fn knows(&self, id: &ActionId) -> bool {
        self.roster.contains(id)
    }
    pub fn cooldown_s(&self, id: &ActionId) -> f64 {
        self.cooldowns.get(id).copied().unwrap_or(0.0)
    }
    pub fn cast(&self) -> Option<&ActorCast> {
        self.cast.as_ref()
    }
    pub fn start_cast(&mut self, id: ActionId, target: Option<ResolutionActorId>, seconds: f64) {
        assert!(self.cast.is_none(), "actor already casting");
        self.cast = Some(ActorCast {
            action_id: id,
            target,
            remaining_s: seconds,
            total_s: seconds,
        });
    }
    pub fn cancel_cast(&mut self) -> Option<ActorCast> {
        self.cast.take()
    }
    pub fn take_completed_cast(&mut self) -> Option<ActorCast> {
        if self
            .cast
            .as_ref()
            .is_some_and(|cast| cast.remaining_s <= 0.0)
        {
            self.cast.take()
        } else {
            None
        }
    }
    pub fn start_cooldown(&mut self, id: ActionId, seconds: f64) {
        if seconds > 0.0 {
            self.cooldowns.insert(id, seconds);
        }
    }
    pub fn tick(&mut self, dt: f64, advance_cast: bool) {
        if advance_cast {
            if let Some(cast) = &mut self.cast {
                cast.remaining_s = cast.remaining_s - dt;
                if cast.remaining_s <= f64::EPSILON * cast.total_s.max(1.0) * 16.0 {
                    cast.remaining_s = 0.0;
                }
            }
        }
        self.cooldowns.retain(|_, remaining| {
            *remaining = (*remaining - dt).max(0.0);
            *remaining > 0.0
        });
    }
}

/// A mutable combat participant. Players and mobs use the same authoritative shape.
#[derive(Debug, Clone)]
pub struct Actor {
    pub(crate) id: ResolutionActorId,
    pub(crate) name: String,
    pub(crate) base_faction: crate::gamedata::FactionId,
    pub(crate) x: f64,
    z: f64,
    armor: i32,
    hp: f64,
    mana: f64,
    alive: bool,
    pub(crate) progression: ActorProgression,
    pub(crate) actions: ActorActionState,
    pub(crate) statuses: BTreeMap<TimedStatusKind, TimedStatus>,
}

impl Actor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: ResolutionActorId,
        name: String,
        faction: crate::gamedata::FactionId,
        x: f64,
        z: f64,
        armor: i32,
        hp: f64,
        mana: f64,
        alive: bool,
        progression: ActorProgression,
        actions: Vec<ActionId>,
    ) -> Self {
        Self {
            id,
            name,
            base_faction: faction,
            x,
            z,
            armor,
            hp,
            mana,
            alive,
            progression,
            actions: ActorActionState::new(actions),
            statuses: BTreeMap::new(),
        }
    }
    pub const fn id(&self) -> ResolutionActorId {
        self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn base_faction(&self) -> &crate::gamedata::FactionId {
        &self.base_faction
    }
    pub fn effective_faction(&self) -> &crate::gamedata::FactionId {
        self.statuses
            .get(&TimedStatusKind::Charm)
            .and_then(|s| s.charm_faction.as_ref())
            .unwrap_or(&self.base_faction)
    }
    pub fn position(&self) -> (f64, f64) {
        (self.x, self.z)
    }
    pub fn set_position(&mut self, x: f64, z: f64) {
        self.x = x;
        self.z = z;
    }
    pub fn armor(&self) -> i32 {
        self.armor
    }
    pub fn hp(&self) -> f64 {
        self.hp
    }
    pub fn set_hp(&mut self, hp: f64) {
        self.hp = hp;
        self.alive = hp > 0.0;
    }
    pub fn mana(&self) -> f64 {
        self.mana
    }
    pub fn set_mana(&mut self, mana: f64) {
        self.mana = mana;
    }
    pub fn is_alive(&self) -> bool {
        self.alive
    }
    pub fn progression(&self) -> &ActorProgression {
        &self.progression
    }
    pub fn progression_mut(&mut self) -> &mut ActorProgression {
        &mut self.progression
    }
    pub fn actions(&self) -> &ActorActionState {
        &self.actions
    }
    pub(crate) fn actions_mut(&mut self) -> &mut ActorActionState {
        &mut self.actions
    }
    pub fn statuses(&self) -> impl Iterator<Item = &TimedStatus> {
        self.statuses.values()
    }
    pub fn can_move(&self) -> bool {
        self.alive
            && !self.statuses.contains_key(&TimedStatusKind::Root)
            && !self.statuses.contains_key(&TimedStatusKind::Hold)
    }
    pub fn can_act(&self) -> bool {
        self.alive && !self.statuses.contains_key(&TimedStatusKind::Hold)
    }
    pub fn movement_multiplier(&self) -> f64 {
        if !self.can_move() {
            0.0
        } else {
            self.statuses
                .get(&TimedStatusKind::Snare)
                .map_or(1.0, |s| s.movement_multiplier)
        }
    }
    pub fn tick_runtime(&mut self, dt: f64) {
        assert!(
            dt.is_finite() && dt >= 0.0,
            "actor runtime dt must be finite and non-negative"
        );
        let held = self.statuses.contains_key(&TimedStatusKind::Hold);
        self.actions.tick(dt, !held);
        self.statuses.retain(|_, status| {
            status.remaining_s = (status.remaining_s - dt).max(0.0);
            status.remaining_s > 0.0
        });
    }
    pub fn apply_hold_cast_rule(&mut self, data: &GameData) -> HoldCastOutcome {
        let Some(cast) = self.actions.cast() else {
            return HoldCastOutcome::NoCast;
        };
        let action = data
            .action(cast.action_id())
            .unwrap_or_else(|| panic!("live cast references unknown action {}", cast.action_id()));
        if action.interruptible() {
            let cancelled = self
                .actions
                .cancel_cast()
                .expect("interruptible live cast disappeared during Hold application");
            assert_eq!(cancelled.action_id(), action.id());
            HoldCastOutcome::Interrupted
        } else {
            HoldCastOutcome::UninterruptibleContinues
        }
    }
    fn apply_status(
        &mut self,
        kind: TimedStatusKind,
        duration_s: f64,
        movement_multiplier: f64,
        source: ResolutionActorId,
        charm_faction: Option<crate::gamedata::FactionId>,
    ) -> bool {
        let replacement = TimedStatus {
            kind,
            remaining_s: duration_s,
            movement_multiplier,
            source,
            charm_faction,
        };
        match self.statuses.get_mut(&kind) {
            Some(existing) if existing.remaining_s >= duration_s => false,
            Some(existing) => {
                *existing = replacement;
                true
            }
            None => {
                self.statuses.insert(kind, replacement);
                true
            }
        }
    }
    pub fn hp_max(&self) -> f64 {
        self.progression.hp_max()
    }
    pub fn mana_max(&self) -> f64 {
        self.progression.mana_max()
    }
    pub fn skill_level(&self, id: &SkillId) -> Option<i32> {
        self.progression.skill_level(id)
    }
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
    pub fn heal(&mut self, amount: f64) -> f64 {
        if !self.alive || amount <= 0.0 {
            return 0.0;
        }
        let before = self.hp;
        self.hp = (self.hp + amount).min(self.hp_max());
        self.hp - before
    }
    pub fn take_damage(&mut self, amount: f64) -> f64 {
        if !self.alive || amount <= 0.0 {
            return 0.0;
        }
        let before = self.hp;
        self.hp = (self.hp - amount).max(0.0);
        self.alive = self.hp > 0.0;
        before - self.hp
    }
}

/// One executed effect assignment, with per-target applied amounts.
#[derive(Debug, Clone, PartialEq)]
pub struct AppliedEffect {
    pub effect_id: EffectId,
    pub skill_id: SkillId,
    pub operation: EffectOperation,
    pub application: Application,
    /// Pre-mitigation magnitude at the caster's current skill level.
    pub magnitude: f64,
    /// Stable target actor identities, in application order.
    pub targets: Vec<ResolutionActorId>,
    pub duration_s: f64,
    pub movement_multiplier: f64,
    /// Per-target amount actually applied (damage dealt / HP healed).
    pub applied: Vec<f64>,
}

/// A progression event attributed to the actor whose progression changed.
#[derive(Debug, Clone, PartialEq)]
pub struct AttributedLevelUpEvent {
    actor: ResolutionActorId,
    event: LevelUpEvent,
}

impl AttributedLevelUpEvent {
    pub const fn actor(&self) -> ResolutionActorId {
        self.actor
    }

    pub const fn event(&self) -> &LevelUpEvent {
        &self.event
    }
}

/// The complete, typed result of resolving one action.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolution {
    pub action_id: ActionId,
    pub caster: ResolutionActorId,
    pub effects: Vec<AppliedEffect>,
    pub mana_spent: f64,
    pub deaths: Vec<ResolutionActorId>,
    pub level_ups: Vec<AttributedLevelUpEvent>,
}

struct PlannedEffect {
    effect_id: EffectId,
    skill_id: SkillId,
    operation: EffectOperation,
    application: Application,
    magnitude: f64,
    duration_s: f64,
    movement_multiplier: f64,
    targets: Vec<usize>,
}

/// Centralized mitigation: post-mitigation damage from raw magnitude and armor.
/// This is the authoritative damage formula.
pub fn mitigate(raw: f64, armor: i32) -> f64 {
    if raw <= 0.0 {
        return 0.0;
    }
    raw * 100.0 / (100.0 + armor.max(0) as f64)
}

fn canonical_skill_level_scale(
    data: &GameData,
    actor: ResolutionActorId,
    skill_id: &SkillId,
) -> Result<f64, ResolutionError> {
    data.skill(skill_id)
        .map(|skill| skill.level_scale())
        .ok_or_else(|| ResolutionError::MissingCanonicalSkill {
            actor: actor.get(),
            skill: skill_id.clone(),
        })
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
        if !actors[caster].is_alive() {
            return Err(ResolutionError::DeadActor(actors[caster].id().get()));
        }
        if !actors[caster].can_act() {
            return Err(ResolutionError::DeadActor(actors[caster].id().get()));
        }
        if !actors[caster].actions().knows(action_id) {
            return Err(ResolutionError::UnknownAction(action_id.clone()));
        }

        // Phase 1 â€” plan: resolve every assignment to concrete commands without
        // mutating actor state, so a failed action never half-applies.
        let mut planned = Vec::with_capacity(action.effects().len());
        for assignment in action.effects() {
            let effect = self
                .data
                .effect(assignment.effect_id())
                .ok_or_else(|| ResolutionError::UnknownEffect(assignment.effect_id().clone()))?;

            let skill_id = effect.skill_id();
            let level = actors[caster].skill_level(skill_id).ok_or_else(|| {
                ResolutionError::UnknownSkill(actors[caster].id().get(), skill_id.clone())
            })?;
            let level_scale =
                canonical_skill_level_scale(self.data, actors[caster].id(), skill_id)?;
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
                operation: effect.operation(),
                application: assignment.application(),
                magnitude,
                duration_s: assignment.duration_s(),
                movement_multiplier: assignment.movement_multiplier(),
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
                    have: actors[caster].mana(),
                });
            }
            mana_spent = action.mana_cost();
        }

        let mut level_ups = Vec::new();
        let caster_id = actors[caster].id();
        if mana_spent > 0.0 {
            append_level_ups(
                &mut level_ups,
                caster_id,
                actors[caster]
                    .progression_mut()
                    .record_mana_spent(mana_spent),
            );
        }

        // Phase 3 â€” apply and record.
        let mut effects = Vec::with_capacity(planned.len());
        let mut deaths = Vec::new();
        for p in planned {
            let skill_events = actors[caster]
                .progression_mut()
                .record_effect_use(&p.skill_id)
                .map_err(|_| {
                    ResolutionError::UnknownSkill(actors[caster].id().get(), p.skill_id.clone())
                })?;
            append_level_ups(&mut level_ups, caster_id, skill_events);
            let mut applied = Vec::with_capacity(p.targets.len());
            for &target in &p.targets {
                let amount = match p.operation {
                    EffectOperation::DirectDamage => {
                        let raw = mitigate(p.magnitude, actors[target].armor());
                        let dealt = actors[target].take_damage(raw);
                        if dealt > 0.0 {
                            let target_id = actors[target].id();
                            let hp_events =
                                actors[target].progression_mut().record_damage_taken(dealt);
                            append_level_ups(&mut level_ups, target_id, hp_events);
                        }
                        if !actors[target].is_alive() && !deaths.contains(&actors[target].id()) {
                            deaths.push(actors[target].id());
                        }
                        dealt
                    }
                    EffectOperation::Heal => actors[target].heal(p.magnitude),
                    EffectOperation::Root => {
                        if actors[target].apply_status(
                            TimedStatusKind::Root,
                            p.duration_s,
                            1.0,
                            caster_id,
                            None,
                        ) {
                            p.duration_s
                        } else {
                            0.0
                        }
                    }
                    EffectOperation::Hold => {
                        let applied = actors[target].apply_status(
                            TimedStatusKind::Hold,
                            p.duration_s,
                            1.0,
                            caster_id,
                            None,
                        );
                        if applied {
                            actors[target].apply_hold_cast_rule(self.data);
                            p.duration_s
                        } else {
                            0.0
                        }
                    }
                    EffectOperation::Snare => {
                        if actors[target].apply_status(
                            TimedStatusKind::Snare,
                            p.duration_s,
                            p.movement_multiplier,
                            caster_id,
                            None,
                        ) {
                            p.movement_multiplier
                        } else {
                            0.0
                        }
                    }
                    EffectOperation::Charm => {
                        let faction = actors[caster].effective_faction().clone();
                        if actors[target].apply_status(
                            TimedStatusKind::Charm,
                            p.duration_s,
                            1.0,
                            caster_id,
                            Some(faction),
                        ) {
                            p.duration_s
                        } else {
                            0.0
                        }
                    }
                };
                applied.push(amount);
            }
            effects.push(AppliedEffect {
                effect_id: p.effect_id,
                skill_id: p.skill_id,
                operation: p.operation,
                application: p.application,
                magnitude: p.magnitude,
                targets: p.targets.iter().map(|&index| actors[index].id()).collect(),
                duration_s: p.duration_s,
                movement_multiplier: p.movement_multiplier,
                applied,
            });
        }

        Ok(Resolution {
            action_id: action_id.clone(),
            caster: caster_id,
            effects,
            mana_spent,
            deaths,
            level_ups,
        })
    }
}

fn append_level_ups(
    attributed: &mut Vec<AttributedLevelUpEvent>,
    actor: ResolutionActorId,
    events: Vec<LevelUpEvent>,
) {
    attributed.extend(
        events
            .into_iter()
            .map(|event| AttributedLevelUpEvent { actor, event }),
    );
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
            let Some(t) = actors.get(idx).filter(|t| t.is_alive()) else {
                return Ok(out);
            };
            if !class_matches(
                data,
                target_class,
                c.effective_faction(),
                t.effective_faction(),
                idx == caster,
            ) {
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
                if !t.is_alive() {
                    continue;
                }
                if dist(c.x, c.z, t.x, t.z) > range_m {
                    continue;
                }
                if !in_cone(c.x, c.z, fx, fz, t.x, t.z, half) {
                    continue;
                }
                if class_matches(
                    data,
                    target_class,
                    c.effective_faction(),
                    t.effective_faction(),
                    i == caster,
                ) {
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
                if !t.is_alive() {
                    continue;
                }
                if dist(cx, cz, t.x, t.z) > radius_m {
                    continue;
                }
                if class_matches(
                    data,
                    target_class,
                    c.effective_faction(),
                    t.effective_faction(),
                    i == caster,
                ) {
                    out.push(i);
                }
            }
        }
        Application::Pbaoe => {
            for (i, t) in actors.iter().enumerate() {
                if !t.is_alive() {
                    continue;
                }
                if dist(c.x, c.z, t.x, t.z) > radius_m {
                    continue;
                }
                if class_matches(
                    data,
                    target_class,
                    c.effective_faction(),
                    t.effective_faction(),
                    i == caster,
                ) {
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
    use crate::progression::Proficiency;

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
            id: ResolutionActorId::new(0),
            name: "Adventurer".into(),
            base_faction: FactionId::new("citizen"),
            x: 0.0,
            z: 0.0,
            armor: 0,
            hp,
            mana,
            alive: true,
            progression,
            actions: ActorActionState::new(
                data.profile(&ProfileId::new("default_player"))
                    .unwrap()
                    .actions()
                    .to_vec(),
            ),
            statuses: BTreeMap::new(),
        }
    }

    fn wolf(data: &GameData, x: f64, z: f64) -> Actor {
        let mob = data.mob(&MobId::new("wolf")).expect("wolf mob");
        let progression = ActorProgression::from_mob(mob, data);
        Actor {
            id: ResolutionActorId::new(1),
            name: "wolf-spider".into(),
            base_faction: FactionId::new("wild"),
            x,
            z,
            armor: mob.armor(),
            hp: mob.hp() as f64,
            mana: progression.mana_max(),
            alive: true,
            progression,
            actions: ActorActionState::new(mob.actions().to_vec()),
            statuses: BTreeMap::new(),
        }
    }

    #[test]
    fn missing_canonical_skill_definition_is_a_typed_error() {
        let data = data();
        let actor = ResolutionActorId::new(41);
        let missing = SkillId::new("missing_canonical_skill");
        assert_eq!(
            canonical_skill_level_scale(&data, actor, &missing),
            Err(ResolutionError::MissingCanonicalSkill {
                actor: 41,
                skill: missing,
            })
        );
    }

    #[test]
    fn strike_deals_mitigated_damage_and_trains_skill() {
        let data = data();
        let mut actors = vec![player(&data), wolf(&data, 1.5, 0.0)];
        let res = Resolver::new(&data)
            .execute(
                &mut actors,
                0,
                &ActionId::new("slash"),
                TargetSelection::Single(1),
            )
            .expect("slash resolves");
        assert_eq!(res.effects.len(), 1);
        assert_eq!(res.effects[0].applied, vec![10.0]);
        assert_eq!(actors[1].hp, 60.0);
        assert_eq!(
            actors[0].progression.skill_xp(&SkillId::new("melee")),
            Some(10)
        );
        assert!(
            res.level_ups.is_empty(),
            "no level-up at skill 1 from one use"
        );
    }

    #[test]
    fn arrow_executes_ranged_damage_and_trains_ranged() {
        let data = data();
        let mut actors = vec![player(&data), wolf(&data, 10.0, 0.0)];
        let mana_before = actors[0].mana;
        let res = Resolver::new(&data)
            .execute(
                &mut actors,
                0,
                &ActionId::new("arrow"),
                TargetSelection::Single(1),
            )
            .expect("arrow resolves");
        assert_eq!(res.mana_spent, 0.0);
        assert_eq!(actors[0].mana, mana_before);
        assert_eq!(res.effects[0].applied, vec![8.0]);
        assert_eq!(
            actors[0].progression.skill_xp(&SkillId::new("ranged")),
            Some(10)
        );
        assert_eq!(actors[0].progression.mana_xp(), 0);
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
                &ActionId::new("restore"),
                TargetSelection::Single(0),
            )
            .expect("restore resolves");
        assert_eq!(res.effects[0].applied, vec![18.0]);
        assert_eq!(actors[0].hp, 68.0);
        assert_eq!(
            actors[0].progression.skill_xp(&SkillId::new("healing")),
            Some(10)
        );
        assert_eq!(actors[0].progression.mana_xp(), 8);
        assert_eq!(actors[0].mana, actors[0].mana_max() - 8.0);
    }

    #[test]
    fn damage_taken_trains_hp() {
        let data = data();
        // Wolf uses the same resolver and the same slash action.
        let mut actors = vec![player(&data), wolf(&data, 1.5, 0.0)];
        let hp_xp_before = actors[0].progression.hp_xp();
        Resolver::new(&data)
            .execute(
                &mut actors,
                1,
                &ActionId::new("slash"),
                TargetSelection::Single(0),
            )
            .expect("wolf slash resolves");
        assert_eq!(actors[0].hp, actors[0].hp_max() - 10.0);
        assert!(actors[0].progression.hp_xp() > hp_xp_before);
        // The wolf trains its own slashing skill through the same pipeline.
        assert_eq!(
            actors[1].progression.skill_xp(&SkillId::new("melee")),
            Some(10)
        );
    }

    #[test]
    fn player_mana_skill_and_target_hp_level_ups_have_canonical_owners() {
        let data = data();
        let mut actors = vec![player(&data), wolf(&data, 10.0, 0.0)];
        actors[0].id = ResolutionActorId::new(700);
        actors[1].id = ResolutionActorId::new(42);
        for _ in 0..9 {
            actors[0]
                .progression
                .record_effect_use(&SkillId::new("ranged"))
                .expect("player knows fire damage");
        }
        actors[0].progression.record_mana_spent(100.0);
        actors[1].progression.record_damage_taken(92.0);

        let resolution = Resolver::new(&data)
            .execute(
                &mut actors,
                0,
                &ActionId::new("arrow"),
                TargetSelection::Single(1),
            )
            .expect("fire bolt resolves");

        let ownership: Vec<(u32, Proficiency)> = resolution
            .level_ups
            .iter()
            .map(|attributed| {
                (
                    attributed.actor().get(),
                    attributed.event().proficiency.clone(),
                )
            })
            .collect();
        assert_eq!(
            ownership,
            vec![
                (700, Proficiency::Skill(SkillId::new("ranged"))),
                (42, Proficiency::Hp),
            ]
        );
    }

    #[test]
    fn hostile_action_events_for_both_actors_are_unambiguous() {
        let data = data();
        let mut actors = vec![player(&data), wolf(&data, 1.5, 0.0)];
        actors[0].id = ResolutionActorId::new(9001);
        actors[1].id = ResolutionActorId::new(73);
        for _ in 0..9 {
            actors[1]
                .progression
                .record_effect_use(&SkillId::new("melee"))
                .expect("wolf knows slashing damage");
        }
        actors[0].progression.record_damage_taken(92.0);

        let resolution = Resolver::new(&data)
            .execute(
                &mut actors,
                1,
                &ActionId::new("slash"),
                TargetSelection::Single(0),
            )
            .expect("wolf slash resolves");

        assert_eq!(resolution.level_ups.len(), 2);
        assert_eq!(resolution.level_ups[0].actor().get(), 73);
        assert_eq!(
            resolution.level_ups[0].event().proficiency,
            Proficiency::Skill(SkillId::new("melee"))
        );
        assert_eq!(resolution.level_ups[1].actor().get(), 9001);
        assert_eq!(resolution.level_ups[1].event().proficiency, Proficiency::Hp);
    }

    #[test]
    fn failed_request_records_no_progression_events() {
        let data = data();
        let mut actors = vec![player(&data), wolf(&data, 40.0, 0.0)];
        let player_progression_before = actors[0].progression.clone();
        let target_progression_before = actors[1].progression.clone();

        let error = Resolver::new(&data)
            .execute(
                &mut actors,
                0,
                &ActionId::new("arrow"),
                TargetSelection::Single(1),
            )
            .expect_err("out-of-range request must fail");

        assert_eq!(error, ResolutionError::OutOfRange(ActionId::new("arrow")));
        assert_eq!(actors[0].progression, player_progression_before);
        assert_eq!(actors[1].progression, target_progression_before);
    }

    #[test]
    fn multi_effect_action_resolves_every_assignment_and_trains_each_skill() {
        let xml = r#"<OrrunGameData schema_version="3"><skills><skill id="slashing_damage" name="Slashing" level_scale="1"/><skill id="fire_damage" name="Fire" level_scale="1"/></skills><factions><faction id="neutral" neutral="true"/><faction id="citizen" neutral="false"/><faction id="wild" neutral="false"/></factions><effects><effect id="slashing_damage" name="Slashing" operation="direct_damage" skill_id="slashing_damage" progression="skill_level"/><effect id="fire_damage" name="Fire" operation="direct_damage" skill_id="fire_damage" progression="skill_level"/></effects><actions><action id="flame_strike" name="Flame Strike" target="hostile" mana_cost="0" cast_s="0" cooldown_s="0" interruptible="false" reveals="true"><effects><effect effect_id="slashing_damage" magnitude="5" application="single_target" range_m="2" duration_s="0" movement_multiplier="1"/><effect effect_id="fire_damage" magnitude="7" application="single_target" range_m="2" duration_s="0" movement_multiplier="1"/></effects></action></actions><players><profile id="default_player" name="Adventurer" faction="citizen"><skill id="slashing_damage" level="1"/><skill id="fire_damage" level="1"/><action id="flame_strike"/></profile></players><mobs><mob id="wolf" name="Wolf" faction="wild" mode="active" hp="70" armor="0" movement_id="walk" speed_variance_ratio="0.05" endurance_s="30"><skill id="slashing_damage" level="1"/><skill id="fire_damage" level="1"/><action id="flame_strike"/></mob></mobs><movement><spec id="walk" speed_mps="2.5"/></movement><hamlet enabled="true"/><defaults/></OrrunGameData>"#;
        let data = GameData::from_xml_str(xml).expect("fixture data");
        let profile = data
            .profile(&ProfileId::new("default_player"))
            .expect("profile");
        let progression = ActorProgression::from_profile(profile);
        let mob = data.mob(&MobId::new("wolf")).expect("mob");
        let target_progression = ActorProgression::from_mob(mob, &data);
        let mut actors = vec![
            Actor {
                id: ResolutionActorId::new(0),
                name: "Adventurer".into(),
                base_faction: FactionId::new("citizen"),
                x: 0.0,
                z: 0.0,
                armor: 0,
                hp: progression.hp_max(),
                mana: progression.mana_max(),
                alive: true,
                progression,
                actions: ActorActionState::new(
                    data.profile(&ProfileId::new("default_player"))
                        .unwrap()
                        .actions()
                        .to_vec(),
                ),
                statuses: BTreeMap::new(),
            },
            Actor {
                id: ResolutionActorId::new(1),
                name: "Wolf".into(),
                base_faction: FactionId::new("wild"),
                x: 1.5,
                z: 0.0,
                armor: 0,
                hp: 70.0,
                mana: target_progression.mana_max(),
                alive: true,
                progression: target_progression,
                actions: ActorActionState::new(mob.actions().to_vec()),
                statuses: BTreeMap::new(),
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
        let base = r#"<OrrunGameData schema_version="3"><skills><skill id="slashing_damage" name="Slashing" level_scale="1"/></skills><factions><faction id="neutral" neutral="true"/><faction id="citizen" neutral="false"/><faction id="wild" neutral="false"/></factions><effects><effect id="slashing_damage" name="Slashing" operation="direct_damage" skill_id="slashing_damage" progression="skill_level"/></effects><actions><action id="slash" name="Slash" target="hostile" mana_cost="0" cast_s="0" cooldown_s="0" interruptible="false" reveals="true"><effects><effect effect_id="slashing_damage" magnitude="MAGNITUDE" application="single_target" range_m="2" duration_s="0" movement_multiplier="1"/></effects></action></actions><players><profile id="default_player" name="Adventurer" faction="citizen"><skill id="slashing_damage" level="1"/><action id="slash"/></profile></players><mobs><mob id="wolf" name="Wolf" faction="wild" mode="active" hp="70" armor="0" movement_id="walk" speed_variance_ratio="0.05" endurance_s="30"><skill id="slashing_damage" level="1"/><action id="slash"/></mob></mobs><movement><spec id="walk" speed_mps="2.5"/></movement><hamlet enabled="true"/><defaults/></OrrunGameData>"#;
        let resolve = |magnitude: &str| {
            let data = GameData::from_xml_str(&base.replace("MAGNITUDE", magnitude))
                .expect("temporary authored variant");
            let mut actors = vec![player(&data), wolf(&data, 1.5, 0.0)];
            Resolver::new(&data)
                .execute(
                    &mut actors,
                    0,
                    &ActionId::new("slash"),
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
    fn root_effect_applies_typed_status() {
        let xml = r#"<OrrunGameData schema_version="3"><skills><skill id="root" name="Root" level_scale="1"/></skills><factions><faction id="neutral" neutral="true"/><faction id="citizen" neutral="false"/><faction id="wild" neutral="false"/></factions><effects><effect id="root" name="Root" operation="root" skill_id="root" progression="skill_level"/></effects><actions><action id="bind" name="Bind" target="hostile" mana_cost="0" cast_s="0" cooldown_s="0" interruptible="false" reveals="true"><effects><effect effect_id="root" magnitude="1" application="single_target" range_m="24" duration_s="1" movement_multiplier="1"/></effects></action></actions><players><profile id="default_player" name="Adventurer" faction="citizen"><skill id="root" level="1"/><action id="bind"/></profile></players><mobs><mob id="wolf" name="Wolf" faction="wild" mode="active" hp="70" armor="0" movement_id="walk" speed_variance_ratio="0.05" endurance_s="30"><skill id="root" level="1"/><action id="bind"/></mob></mobs><movement><spec id="walk" speed_mps="2.5"/></movement><hamlet enabled="true"/><defaults/></OrrunGameData>"#;
        let data = GameData::from_xml_str(xml).expect("fixture data");
        let profile = data
            .profile(&ProfileId::new("default_player"))
            .expect("profile");
        let progression = ActorProgression::from_profile(profile);
        let mut actors = vec![
            Actor {
                id: ResolutionActorId::new(0),
                name: "Adventurer".into(),
                base_faction: FactionId::new("citizen"),
                x: 0.0,
                z: 0.0,
                armor: 0,
                hp: progression.hp_max(),
                mana: progression.mana_max(),
                alive: true,
                progression,
                actions: ActorActionState::new(
                    data.profile(&ProfileId::new("default_player"))
                        .unwrap()
                        .actions()
                        .to_vec(),
                ),
                statuses: BTreeMap::new(),
            },
            wolf(&data, 1.5, 0.0),
        ];
        Resolver::new(&data)
            .execute(
                &mut actors,
                0,
                &ActionId::new("bind"),
                TargetSelection::Single(1),
            )
            .expect("root resolves");
        assert!(!actors[1].can_move());
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
                &ActionId::new("arrow"),
                TargetSelection::Single(1),
            )
            .unwrap_err();
        assert_eq!(err, ResolutionError::OutOfRange(ActionId::new("arrow")));
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
                &ActionId::new("restore"),
                TargetSelection::Single(0),
            )
            .unwrap_err();
        assert!(matches!(
            err,
            ResolutionError::InsufficientMana { need: 8.0, .. }
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
                &ActionId::new("slash"),
                TargetSelection::Single(1),
            )
            .expect("slash resolves");
        assert!(!actors[1].alive);
        assert_eq!(res.deaths, vec![ResolutionActorId::new(1)]);
        // A dead actor no longer resolves damage.
        let err = Resolver::new(&data)
            .execute(
                &mut actors,
                0,
                &ActionId::new("slash"),
                TargetSelection::Single(1),
            )
            .unwrap_err();
        assert_eq!(err, ResolutionError::NoTarget(ActionId::new("slash")));
    }

    #[test]
    fn invalid_caster_index_is_a_typed_error() {
        let data = data();
        let mut actors = vec![player(&data)];
        let err = Resolver::new(&data)
            .execute(
                &mut actors,
                99,
                &ActionId::new("slash"),
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
                id: ResolutionActorId::new(id),
                name: "wolf".into(),
                base_faction: FactionId::new("wild"),
                x,
                z,
                armor: 0,
                hp: 70.0,
                mana: progression.mana_max(),
                alive: true,
                progression,
                actions: ActorActionState::new(vec![ActionId::new("bind")]),
                statuses: BTreeMap::new(),
            });
        };
        add(1, 2.0, 0.0); // straight ahead, in cone
        add(2, 0.0, 2.0); // 90Â° off, out of cone
        let got = select_targets(
            &data,
            &actors,
            0,
            &ActionId::new("slash"),
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
                id: ResolutionActorId::new(id),
                name: "wolf".into(),
                base_faction: FactionId::new("wild"),
                x,
                z,
                armor: 0,
                hp: 70.0,
                mana: progression.mana_max(),
                alive: true,
                progression: progression.clone(),
                actions: ActorActionState::new(vec![ActionId::new("slash")]),
                statuses: BTreeMap::new(),
            });
        }
        let got = select_targets(
            &data,
            &actors,
            0,
            &ActionId::new("slash"),
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

    #[test]
    fn actor_status_expiry_removes_only_elapsed_status() {
        let data = data();
        let mut actor = player(&data);
        actor.apply_status(
            TimedStatusKind::Root,
            0.5,
            1.0,
            ResolutionActorId::new(1),
            None,
        );
        actor.apply_status(
            TimedStatusKind::Snare,
            2.0,
            0.5,
            ResolutionActorId::new(1),
            None,
        );
        actor.tick_runtime(0.5);
        assert!(!actor.statuses.contains_key(&TimedStatusKind::Root));
        assert!(actor.statuses.contains_key(&TimedStatusKind::Snare));
    }

    #[test]
    fn hold_cancels_interruptible_cast_but_not_uninterruptible_cast() {
        let data = data();
        let mut actor = player(&data);
        actor
            .actions_mut()
            .start_cast(ActionId::new("restore"), None, 1.0);
        assert_eq!(
            actor.apply_hold_cast_rule(&data),
            HoldCastOutcome::Interrupted
        );
        actor
            .actions_mut()
            .start_cast(ActionId::new("slash"), None, 1.0);
        assert_eq!(
            actor.apply_hold_cast_rule(&data),
            HoldCastOutcome::UninterruptibleContinues
        );
        actor.apply_status(
            TimedStatusKind::Hold,
            0.5,
            1.0,
            ResolutionActorId::new(1),
            None,
        );
        actor.tick_runtime(0.5);
        let cast = actor
            .actions()
            .cast()
            .expect("uninterruptible cast remains");
        assert_eq!(
            cast.remaining_s(),
            1.0,
            "Hold pauses an uninterruptible cast"
        );
    }
}
