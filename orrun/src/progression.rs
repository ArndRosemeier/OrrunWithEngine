//! Skill-based actor progression.
//!
//! This is the new progression owner introduced in M1. It is deliberately
//! independent of action execution and live combat: it owns per-proficiency
//! level and exact XP, exposes typed training operations, and emits typed
//! level-up events. It knows nothing about the legacy global level/XP,
//! disciplines, attributes, or ranks.
//!
//! Progression state stores a proficiency's level and its progress toward the
//! next level. Current resource values (current HP, current mana) do not live
//! here; capacity is derived from the proficiency level on demand.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::gamedata::{GameData, MobDefinition, PlayerProfile, Progression, SkillId};

/// Provisional balance parameters for the skill system.
///
/// These values are placeholders so the ownership model can be built and
/// tested now. They are centralized here so a single edit retunes the whole
/// model; M7 replaces them with play-evidence values.
pub mod balance {
    use super::Progression;

    /// XP granted to a skill each time one of its effects executes.
    pub const XP_PER_EFFECT_USE: u64 = 10;

    /// XP granted to the HP proficiency per point of post-mitigation damage
    /// actually received. Amounts are rounded up so any positive event trains.
    pub const XP_PER_HP_DAMAGE: u64 = 1;

    /// XP granted to the mana proficiency per point of mana actually spent.
    /// Amounts are rounded up so any positive event trains.
    pub const XP_PER_MANA_SPENT: u64 = 1;

    /// Base HP capacity at HP level 1.
    pub const HP_BASE: f64 = 100.0;
    /// HP capacity added per HP level above 1.
    pub const HP_PER_LEVEL: f64 = 12.0;

    /// Base mana capacity at mana level 1.
    pub const MANA_BASE: f64 = 50.0;
    /// Mana capacity added per mana level above 1.
    pub const MANA_PER_LEVEL: f64 = 6.0;

    /// XP required to advance a proficiency from `level` to `level + 1`.
    ///
    /// Quadratic in `level` (cumulative cost cubic), so the marginal cost of
    /// pushing one high proficiency grows steeply. This is the only
    /// breadth-vs-depth brake; there is no cap and no anti-grind rule.
    pub fn xp_to_next(level: i32) -> u64 {
        let level = level.max(1) as u64;
        100 * level * level
    }

    /// Max HP capacity at a given HP proficiency level.
    pub fn hp_max(level: i32) -> f64 {
        HP_BASE + HP_PER_LEVEL * (level - 1) as f64
    }

    /// Max mana capacity at a given mana proficiency level.
    pub fn mana_max(level: i32) -> f64 {
        MANA_BASE + MANA_PER_LEVEL * (level - 1) as f64
    }

    /// Effective output of an effect with authored `base` magnitude at a given
    /// skill level and `level_scale`.
    ///
    /// `skill_level` maps to `base * (1 + level_scale * (level - 1))`; `flat`
    /// ignores level entirely and returns `base`.
    pub fn effect_magnitude(
        base: f64,
        level: i32,
        level_scale: f64,
        progression: Progression,
    ) -> f64 {
        match progression {
            Progression::Flat => base,
            Progression::SkillLevel => base * (1.0 + level_scale * (level - 1) as f64),
        }
    }
}

/// A trainable proficiency. Skills are keyed by authored ID; HP and mana are
/// the two built-in resource proficiencies.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Proficiency {
    Skill(SkillId),
    Hp,
    Mana,
}

/// What a level-up changed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LevelUpResult {
    /// A skill leveled; its effects resolve at a higher magnitude.
    Skill,
    /// HP or mana leveled; carry the capacity change.
    Resource { max_before: f64, max_after: f64 },
}

/// An immutable record of a proficiency level-up.
#[derive(Debug, Clone, PartialEq)]
pub struct LevelUpEvent {
    pub proficiency: Proficiency,
    pub old_level: i32,
    pub new_level: i32,
    pub result: LevelUpResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProgressionError {
    #[error("actor does not know skill {0}")]
    UnknownSkill(SkillId),
}

/// Per-proficiency level and progress toward the next level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Track {
    level: i32,
    xp: u64,
}

impl Track {
    fn new(level: i32) -> Self {
        Track {
            level: level.max(1),
            xp: 0,
        }
    }

    /// Grant `amount` XP, advancing levels and pushing one event per level-up.
    fn add_xp(
        &mut self,
        proficiency: &Proficiency,
        amount: u64,
        result_for: impl Fn(i32, i32) -> LevelUpResult,
        events: &mut Vec<LevelUpEvent>,
    ) {
        if amount == 0 {
            return;
        }
        self.xp += amount;
        while self.xp >= balance::xp_to_next(self.level) {
            self.xp -= balance::xp_to_next(self.level);
            let old = self.level;
            self.level += 1;
            events.push(LevelUpEvent {
                proficiency: proficiency.clone(),
                old_level: old,
                new_level: self.level,
                result: result_for(old, self.level),
            });
        }
    }
}

/// One actor's skill, HP, and mana progression. The authoritative owner of all
/// level and XP transitions; callers cannot mutate levels or XP directly.
#[derive(Debug, Clone, PartialEq)]
pub struct ActorProgression {
    skills: BTreeMap<SkillId, Track>,
    hp: Track,
    mana: Track,
}

impl ActorProgression {
    fn from_levels(levels: impl IntoIterator<Item = (SkillId, i32)>) -> Self {
        let mut skills = BTreeMap::new();
        for (id, level) in levels {
            skills.insert(id, Track::new(level));
        }
        ActorProgression {
            skills,
            hp: Track::new(1),
            mana: Track::new(1),
        }
    }

    /// Initialize from an authored player profile: each known skill starts at
    /// its authored level; HP and mana start at level 1.
    pub fn from_profile(profile: &PlayerProfile) -> Self {
        Self::from_levels(
            profile
                .skills()
                .iter()
                .map(|s| (s.id().clone(), s.level())),
        )
    }

    /// Initialize from an authored mob definition: every skill reachable from
    /// the mob's actions starts at level 1 (provisional; the schema carries no
    /// per-mob skill level yet). HP and mana start at level 1.
    pub fn from_mob(mob: &MobDefinition, gamedata: &GameData) -> Self {
        let mut ids = BTreeSet::new();
        for action_id in mob.actions() {
            if let Some(action) = gamedata.action(action_id) {
                for assignment in action.effects() {
                    if let Some(effect) = gamedata.effect(assignment.effect_id()) {
                        ids.insert(effect.skill_id().clone());
                    }
                }
            }
        }
        Self::from_levels(ids.into_iter().map(|id| (id, 1)))
    }

    /// Train a skill because one of its effects executed. Errors loudly if the
    /// actor does not know `skill_id`.
    pub fn record_effect_use(
        &mut self,
        skill_id: &SkillId,
    ) -> Result<Vec<LevelUpEvent>, ProgressionError> {
        let track = self
            .skills
            .get_mut(skill_id)
            .ok_or_else(|| ProgressionError::UnknownSkill(skill_id.clone()))?;
        let mut events = Vec::new();
        track.add_xp(
            &Proficiency::Skill(skill_id.clone()),
            balance::XP_PER_EFFECT_USE,
            |_, _| LevelUpResult::Skill,
            &mut events,
        );
        Ok(events)
    }

    /// Train HP from post-mitigation damage actually received. Zero or
    /// negative amounts do not train.
    pub fn record_damage_taken(&mut self, amount: f64) -> Vec<LevelUpEvent> {
        if amount <= 0.0 {
            return Vec::new();
        }
        let mut events = Vec::new();
        let xp = xp_from_amount(amount, balance::XP_PER_HP_DAMAGE);
        self.hp.add_xp(
            &Proficiency::Hp,
            xp,
            |old, new| LevelUpResult::Resource {
                max_before: balance::hp_max(old),
                max_after: balance::hp_max(new),
            },
            &mut events,
        );
        events
    }

    /// Train mana from mana actually spent. Zero or negative amounts do not
    /// train.
    pub fn record_mana_spent(&mut self, amount: f64) -> Vec<LevelUpEvent> {
        if amount <= 0.0 {
            return Vec::new();
        }
        let mut events = Vec::new();
        let xp = xp_from_amount(amount, balance::XP_PER_MANA_SPENT);
        self.mana.add_xp(
            &Proficiency::Mana,
            xp,
            |old, new| LevelUpResult::Resource {
                max_before: balance::mana_max(old),
                max_after: balance::mana_max(new),
            },
            &mut events,
        );
        events
    }

    /// Known skills and their current levels, in stable authored-ID order.
    pub fn skills(&self) -> impl Iterator<Item = &SkillId> {
        self.skills.keys()
    }
    pub fn skill_level(&self, id: &SkillId) -> Option<i32> {
        self.skills.get(id).map(|t| t.level)
    }
    pub fn skill_xp(&self, id: &SkillId) -> Option<u64> {
        self.skills.get(id).map(|t| t.xp)
    }
    pub fn skill_xp_to_next(&self, id: &SkillId) -> Option<u64> {
        self.skills.get(id).map(|t| balance::xp_to_next(t.level) - t.xp)
    }

    pub fn hp_level(&self) -> i32 {
        self.hp.level
    }
    pub fn hp_xp(&self) -> u64 {
        self.hp.xp
    }
    pub fn hp_xp_to_next(&self) -> u64 {
        balance::xp_to_next(self.hp.level) - self.hp.xp
    }
    pub fn hp_max(&self) -> f64 {
        balance::hp_max(self.hp.level)
    }

    pub fn mana_level(&self) -> i32 {
        self.mana.level
    }
    pub fn mana_xp(&self) -> u64 {
        self.mana.xp
    }
    pub fn mana_xp_to_next(&self) -> u64 {
        balance::xp_to_next(self.mana.level) - self.mana.xp
    }
    pub fn mana_max(&self) -> f64 {
        balance::mana_max(self.mana.level)
    }
}

/// Convert a continuous training amount into whole XP, rounded up so any
/// positive amount trains at least one XP.
fn xp_from_amount(amount: f64, per_unit: u64) -> u64 {
    (amount * per_unit as f64).ceil() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gamedata::{GameData, MobId, ProfileId};

    fn prog(skills: &[(&str, i32)]) -> ActorProgression {
        ActorProgression::from_levels(skills.iter().map(|(id, lvl)| (SkillId::new(*id), *lvl)))
    }

    #[test]
    fn level_cost_is_strictly_increasing() {
        let mut prev = 0;
        for level in 1..30 {
            let cost = balance::xp_to_next(level);
            assert!(cost > prev, "cost at {level} ({cost}) not greater than {prev}");
            prev = cost;
        }
    }

    #[test]
    fn accumulates_deterministically_across_levels() {
        // Costs: 1->2 = 100, 2->3 = 400, 3->4 = 900 (cumulative 1400).
        // 150 uses * 10 XP = 1500 -> reaches level 4 with 100 XP toward 5.
        let mut p = prog(&[("slash", 1)]);
        let mut events = Vec::new();
        for _ in 0..150 {
            events.extend(p.record_effect_use(&SkillId::new("slash")).unwrap());
        }
        assert_eq!(p.skill_level(&SkillId::new("slash")), Some(4));
        assert_eq!(p.skill_xp(&SkillId::new("slash")), Some(100));
        let levels: Vec<i32> = events.iter().map(|e| e.new_level).collect();
        assert_eq!(levels, vec![2, 3, 4]);
        for e in &events {
            assert_eq!(e.result, LevelUpResult::Skill);
        }
    }

    #[test]
    fn rejects_unknown_skill() {
        let mut p = prog(&[]);
        let err = p.record_effect_use(&SkillId::new("nope")).unwrap_err();
        assert_eq!(err, ProgressionError::UnknownSkill(SkillId::new("nope")));
    }

    #[test]
    fn damage_trains_hp_and_levels_up() {
        let mut p = prog(&[]);
        assert_eq!(p.hp_level(), 1);
        assert_eq!(p.hp_max(), 100.0);

        let events = p.record_damage_taken(100.0);
        assert_eq!(p.hp_level(), 2);
        assert_eq!(p.hp_xp(), 0);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].old_level, 1);
        assert_eq!(events[0].new_level, 2);
        assert_eq!(
            events[0].result,
            LevelUpResult::Resource {
                max_before: 100.0,
                max_after: 112.0
            }
        );
        assert_eq!(p.hp_max(), 112.0);
    }

    #[test]
    fn non_positive_damage_does_not_train() {
        let mut p = prog(&[]);
        assert!(p.record_damage_taken(0.0).is_empty());
        assert!(p.record_damage_taken(-5.0).is_empty());
        assert_eq!(p.hp_xp(), 0);
    }

    #[test]
    fn mana_trains_only_from_positive_spend() {
        let mut p = prog(&[]);
        assert!(p.record_mana_spent(0.0).is_empty());
        assert!(p.record_mana_spent(-3.0).is_empty());

        assert!(p.record_mana_spent(50.0).is_empty());
        assert_eq!(p.mana_xp(), 50);
        assert_eq!(p.mana_level(), 1);

        let events = p.record_mana_spent(50.0);
        assert_eq!(p.mana_level(), 2);
        assert_eq!(p.mana_xp(), 0);
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].result,
            LevelUpResult::Resource {
                max_before: 50.0,
                max_after: 56.0
            }
        );
        assert_eq!(p.mana_max(), 56.0);
    }

    #[test]
    fn breadth_advances_faster_than_depth() {
        let mut deep = prog(&[("a", 1)]);
        for _ in 0..300 {
            deep.record_effect_use(&SkillId::new("a")).unwrap();
        }
        let deep_gains: i32 = deep
            .skills()
            .map(|id| deep.skill_level(id).unwrap() - 1)
            .sum();

        let mut broad = prog(&[("a", 1), ("b", 1), ("c", 1)]);
        for _ in 0..100 {
            broad.record_effect_use(&SkillId::new("a")).unwrap();
            broad.record_effect_use(&SkillId::new("b")).unwrap();
            broad.record_effect_use(&SkillId::new("c")).unwrap();
        }
        let broad_gains: i32 = broad
            .skills()
            .map(|id| broad.skill_level(id).unwrap() - 1)
            .sum();

        assert!(
            broad_gains > deep_gains,
            "breadth ({broad_gains}) should beat depth ({deep_gains})"
        );
    }

    #[test]
    fn magnitude_scales_with_skill_level() {
        use crate::gamedata::Progression as P;
        assert_eq!(balance::effect_magnitude(10.0, 1, 1.0, P::SkillLevel), 10.0);
        assert_eq!(balance::effect_magnitude(10.0, 3, 1.0, P::SkillLevel), 30.0);
        assert_eq!(balance::effect_magnitude(10.0, 3, 0.5, P::SkillLevel), 20.0);
        assert_eq!(balance::effect_magnitude(10.0, 99, 1.0, P::Flat), 10.0);
    }

    #[test]
    fn initializes_from_game_data() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../data/OrrunGameData.xml");
        let data = GameData::load(path).expect("canonical data loads");

        let profile = data
            .profile(&ProfileId::new("default_player"))
            .expect("default player");
        let p = ActorProgression::from_profile(profile);
        assert_eq!(p.skill_level(&SkillId::new("slashing_damage")), Some(1));
        assert_eq!(p.skill_level(&SkillId::new("healing")), Some(1));

        let wolf = data
            .mob(&MobId::new("crawler_spider_wolf"))
            .expect("wolf mob");
        let m = ActorProgression::from_mob(wolf, &data);
        assert!(m.skill_level(&SkillId::new("slashing_damage")).is_some());
        assert_eq!(m.hp_level(), 1);
    }
}
