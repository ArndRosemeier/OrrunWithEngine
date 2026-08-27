//! Canonical authored game data loaded once at runtime.
//!
//! `OrrunGameData.xml` is the single source of authored gameplay definitions.
//! This module parses it into strongly typed records, validates every
//! reference and finite domain at load time, and exposes read-only accessors
//! plus indexed lookups. Runtime state never lives here.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::Path;

use quick_xml::de::from_str;
use serde::Deserialize;
use thiserror::Error;

const SCHEMA_VERSION: u32 = 3;

#[derive(Debug, Error)]
pub enum GameDataError {
    #[error("read GameData {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("invalid GameData XML: {0}")]
    Xml(#[from] quick_xml::DeError),
    #[error("invalid GameData: {0}")]
    Validation(String),
}

fn validation(message: impl Into<String>) -> GameDataError {
    GameDataError::Validation(message.into())
}

/// A stable, validated identifier. Newtypes keep category mistakes out of
/// lookups and state keys without paying for string interning.
macro_rules! typed_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }
    };
}

typed_id! {
    /// Identifies a trainable proficiency such as `slashing_damage` or `healing`.
    SkillId
}
typed_id! {
    /// Identifies a catalog effect definition.
    EffectId
}
typed_id! {
    /// Identifies an action such as `slash`, `arrow`, or `restore`.
    ActionId
}
typed_id! {
    /// Identifies an actor faction.
    FactionId
}
typed_id! {
    /// Identifies a player profile.
    ProfileId
}
typed_id! {
    /// Identifies a mob definition.
    MobId
}
typed_id! {
    /// Identifies a movement specification.
    MovementId
}
typed_id! {
    /// Identifies an animal species presentation/spawn record.
    SpeciesId
}

/// The finite executable operations an effect performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectOperation {
    DirectDamage,
    Heal,
    Root,
    Hold,
    Snare,
    Charm,
}

impl EffectOperation {
    fn from_str(value: &str) -> Result<Self, GameDataError> {
        match value {
            "direct_damage" => Ok(EffectOperation::DirectDamage),
            "heal" => Ok(EffectOperation::Heal),
            "root" => Ok(EffectOperation::Root),
            "hold" => Ok(EffectOperation::Hold),
            "snare" => Ok(EffectOperation::Snare),
            "charm" => Ok(EffectOperation::Charm),
            other => Err(validation(format!("unknown effect operation {other:?}"))),
        }
    }
}

/// How an effect's output scales.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progression {
    SkillLevel,
    Flat,
}

impl Progression {
    fn from_str(value: &str) -> Result<Self, GameDataError> {
        match value {
            "skill_level" => Ok(Progression::SkillLevel),
            "flat" => Ok(Progression::Flat),
            other => Err(validation(format!("unknown progression {other:?}"))),
        }
    }
}

/// The permitted targeting class of an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionTarget {
    Hostile,
    Friendly,
    ActorSelf,
    Any,
    None,
}

impl ActionTarget {
    fn from_str(value: &str) -> Result<Self, GameDataError> {
        match value {
            "hostile" => Ok(ActionTarget::Hostile),
            "friendly" => Ok(ActionTarget::Friendly),
            "self" => Ok(ActionTarget::ActorSelf),
            "any" => Ok(ActionTarget::Any),
            "none" => Ok(ActionTarget::None),
            other => Err(validation(format!("unknown action target {other:?}"))),
        }
    }
}

/// How an effect assignment applies to space, relative to the selected target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Application {
    SingleTarget,
    Cone,
    Aoe,
    Pbaoe,
}

impl Application {
    fn from_str(value: &str) -> Result<Self, GameDataError> {
        match value {
            "single_target" => Ok(Application::SingleTarget),
            "cone" => Ok(Application::Cone),
            "aoe" => Ok(Application::Aoe),
            "pbaoe" => Ok(Application::Pbaoe),
            other => Err(validation(format!("unknown application {other:?}"))),
        }
    }
}

/// Whether a mob initiates combat or only retaliates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MobMode {
    Active,
    Passive,
}

impl MobMode {
    fn from_str(value: &str) -> Result<Self, GameDataError> {
        match value {
            "active" => Ok(MobMode::Active),
            "passive" => Ok(MobMode::Passive),
            other => Err(validation(format!("unknown mob mode {other:?}"))),
        }
    }
}

// ---------------------------------------------------------------------------
// Raw deserialization DTOs. These hold strings and are converted to the typed
// public model in `GameData::from_raw`. They are never exposed.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename = "OrrunGameData", deny_unknown_fields)]
struct RawGameData {
    #[serde(rename = "@schema_version")]
    schema_version: u32,
    skills: RawSkills,
    factions: RawFactions,
    #[serde(default)]
    effects: RawEffects,
    actions: RawActions,
    players: RawPlayers,
    mobs: RawMobs,
    movement: RawMovement,
    #[serde(default)]
    hamlet: RawHamlet,
    #[serde(rename = "defaults", default)]
    defaults: RawDefaults,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSkills {
    #[serde(rename = "skill", default)]
    items: Vec<RawSkill>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFactions {
    #[serde(rename = "faction", default)]
    items: Vec<RawFaction>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEffects {
    #[serde(rename = "effect", default)]
    items: Vec<RawEffect>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawActions {
    #[serde(rename = "action", default)]
    items: Vec<RawAction>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawActionEffects {
    #[serde(rename = "effect", default)]
    items: Vec<RawActionEffect>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPlayers {
    #[serde(rename = "profile", default)]
    items: Vec<RawPlayerProfile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMobs {
    #[serde(rename = "mob", default)]
    items: Vec<RawMobDefinition>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMovement {
    #[serde(rename = "spec", default)]
    items: Vec<RawMovementSpec>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDefaults {
    #[serde(rename = "value", default)]
    items: Vec<RawDefaultValue>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSkill {
    #[serde(rename = "@id")]
    id: String,
    #[serde(rename = "@name")]
    name: String,
    #[serde(rename = "@description", default)]
    description: String,
    #[serde(rename = "@level_scale", default = "one")]
    level_scale: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFaction {
    #[serde(rename = "@id")]
    id: String,
    #[serde(rename = "@name", default)]
    name: String,
    #[serde(rename = "@neutral", default)]
    neutral: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAction {
    #[serde(rename = "@id")]
    id: String,
    #[serde(rename = "@name", default)]
    name: String,
    #[serde(rename = "@description", default)]
    description: String,
    #[serde(rename = "@target", default = "default_target")]
    target: String,
    #[serde(rename = "@mana_cost", default)]
    mana_cost: f64,
    #[serde(rename = "@cast_s", default)]
    cast_s: f64,
    #[serde(rename = "@cooldown_s")]
    cooldown_s: f64,
    #[serde(rename = "@interruptible")]
    interruptible: bool,
    #[serde(rename = "@reveals")]
    reveals: bool,
    #[serde(rename = "effects", default)]
    effects: RawActionEffects,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawActionEffect {
    #[serde(rename = "@effect_id")]
    effect_id: String,
    #[serde(rename = "@magnitude", default = "one")]
    magnitude: f64,
    #[serde(rename = "@application", default = "default_application")]
    application: String,
    #[serde(rename = "@range_m", default = "default_range")]
    range_m: f64,
    #[serde(rename = "@radius_m", default)]
    radius_m: f64,
    #[serde(rename = "@angle_deg", default)]
    angle_deg: f64,
    #[serde(rename = "@duration_s", default)]
    duration_s: f64,
    #[serde(rename = "@movement_multiplier", default = "one")]
    movement_multiplier: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEffect {
    #[serde(rename = "@id")]
    id: String,
    #[serde(rename = "@name", default)]
    name: String,
    #[serde(rename = "@operation")]
    operation: String,
    #[serde(rename = "@skill_id")]
    skill_id: String,
    #[serde(rename = "@progression", default = "default_progression")]
    progression: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPlayerProfile {
    #[serde(rename = "@id")]
    id: String,
    #[serde(rename = "@name", default)]
    name: String,
    #[serde(rename = "@faction")]
    faction: String,
    #[serde(rename = "skill", default)]
    skills: Vec<RawProfileSkill>,
    #[serde(rename = "action", default)]
    actions: Vec<RawMobActionRef>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProfileSkill {
    #[serde(rename = "@id")]
    id: String,
    #[serde(rename = "@level", default = "default_level")]
    level: i32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMobDefinition {
    #[serde(rename = "@id")]
    id: String,
    #[serde(rename = "@name", default)]
    name: String,
    #[serde(rename = "@faction")]
    faction: String,
    #[serde(rename = "@mode", default = "default_mode")]
    mode: String,
    #[serde(rename = "@movement_id", default = "default_movement")]
    movement_id: String,
    #[serde(rename = "@species_id", default)]
    species_id: String,
    #[serde(rename = "@hp")]
    hp: i32,
    #[serde(rename = "@armor", default)]
    armor: i32,
    #[serde(rename = "@speed_variance_ratio")]
    speed_variance_ratio: f64,
    #[serde(rename = "@endurance_s")]
    endurance_s: f64,
    #[serde(rename = "skill", default)]
    skills: Vec<RawProfileSkill>,
    #[serde(rename = "action", default)]
    actions: Vec<RawMobActionRef>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMobActionRef {
    #[serde(rename = "@id")]
    id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMovementSpec {
    #[serde(rename = "@id")]
    id: String,
    #[serde(rename = "@speed_mps", alias = "@speed")]
    speed_mps: f64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHamlet {
    #[serde(rename = "@enabled", default = "default_true")]
    enabled: bool,
    #[serde(rename = "@width", default = "default_width")]
    width: i32,
    #[serde(rename = "@depth", default = "default_depth")]
    depth: i32,
    #[serde(rename = "@kit_catalog", default = "default_kit")]
    kit_catalog: String,
    #[serde(rename = "layer", default)]
    layers: Vec<RawHamletLayer>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHamletLayer {
    #[serde(rename = "@id")]
    id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDefaultValue {
    #[serde(rename = "@key")]
    key: String,
    #[serde(rename = "@value")]
    value: String,
}

fn one() -> f64 {
    1.0
}
fn default_true() -> bool {
    true
}
fn default_mode() -> String {
    "active".into()
}
fn default_movement() -> String {
    "walk".into()
}
fn default_application() -> String {
    "single_target".into()
}
fn default_range() -> f64 {
    1.8
}
fn default_progression() -> String {
    "skill_level".into()
}
fn default_target() -> String {
    "hostile".into()
}
fn default_level() -> i32 {
    1
}
fn default_width() -> i32 {
    32
}
fn default_depth() -> i32 {
    32
}
fn default_kit() -> String {
    "catalogs/medieval.json".into()
}

// ---------------------------------------------------------------------------
// Typed public model.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Skill {
    id: SkillId,
    name: String,
    description: String,
    level_scale: f64,
}

impl Skill {
    pub fn id(&self) -> &SkillId {
        &self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn description(&self) -> &str {
        &self.description
    }
    pub fn level_scale(&self) -> f64 {
        self.level_scale
    }
}

#[derive(Debug, Clone)]
pub struct Faction {
    id: FactionId,
    name: String,
    neutral: bool,
}

impl Faction {
    pub fn id(&self) -> &FactionId {
        &self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn is_neutral(&self) -> bool {
        self.neutral
    }
}

#[derive(Debug, Clone)]
pub struct Effect {
    id: EffectId,
    name: String,
    operation: EffectOperation,
    skill_id: SkillId,
    progression: Progression,
}

impl Effect {
    pub fn id(&self) -> &EffectId {
        &self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn operation(&self) -> EffectOperation {
        self.operation
    }
    pub fn skill_id(&self) -> &SkillId {
        &self.skill_id
    }
    pub fn progression(&self) -> Progression {
        self.progression
    }
}

#[derive(Debug, Clone)]
pub struct ActionEffect {
    effect_id: EffectId,
    magnitude: f64,
    application: Application,
    range_m: f64,
    radius_m: f64,
    angle_deg: f64,
    duration_s: f64,
    movement_multiplier: f64,
}

impl ActionEffect {
    pub fn effect_id(&self) -> &EffectId {
        &self.effect_id
    }
    pub fn magnitude(&self) -> f64 {
        self.magnitude
    }
    pub fn application(&self) -> Application {
        self.application
    }
    pub fn range_m(&self) -> f64 {
        self.range_m
    }
    pub fn radius_m(&self) -> f64 {
        self.radius_m
    }
    pub fn angle_deg(&self) -> f64 {
        self.angle_deg
    }
    pub fn duration_s(&self) -> f64 {
        self.duration_s
    }
    pub fn movement_multiplier(&self) -> f64 {
        self.movement_multiplier
    }
}

#[derive(Debug, Clone)]
pub struct Action {
    id: ActionId,
    name: String,
    description: String,
    target: ActionTarget,
    mana_cost: f64,
    cast_s: f64,
    cooldown_s: f64,
    interruptible: bool,
    reveals: bool,
    effects: Vec<ActionEffect>,
}

impl Action {
    pub fn id(&self) -> &ActionId {
        &self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn description(&self) -> &str {
        &self.description
    }
    pub fn target(&self) -> ActionTarget {
        self.target
    }
    pub fn mana_cost(&self) -> f64 {
        self.mana_cost
    }
    pub fn cast_s(&self) -> f64 {
        self.cast_s
    }
    pub fn cooldown_s(&self) -> f64 {
        self.cooldown_s
    }
    pub fn interruptible(&self) -> bool {
        self.interruptible
    }
    pub fn reveals(&self) -> bool {
        self.reveals
    }
    pub fn effects(&self) -> &[ActionEffect] {
        &self.effects
    }
}

/// A skill reference with a starting level from a player profile.
#[derive(Debug, Clone)]
pub struct ProfileSkill {
    id: SkillId,
    level: i32,
}

impl ProfileSkill {
    pub fn id(&self) -> &SkillId {
        &self.id
    }
    pub fn level(&self) -> i32 {
        self.level
    }
}

#[derive(Debug, Clone)]
pub struct PlayerProfile {
    id: ProfileId,
    name: String,
    faction: FactionId,
    skills: Vec<ProfileSkill>,
    actions: Vec<ActionId>,
}

impl PlayerProfile {
    pub fn id(&self) -> &ProfileId {
        &self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn faction(&self) -> &FactionId {
        &self.faction
    }
    pub fn skills(&self) -> &[ProfileSkill] {
        &self.skills
    }
    pub fn actions(&self) -> &[ActionId] {
        &self.actions
    }
}

/// Validated, dimensionless randomization ratio applied to a mob's base speed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpeedVarianceRatio(f64);
impl SpeedVarianceRatio {
    fn new(value: f64, mob_id: &MobId) -> Result<Self, GameDataError> {
        if !value.is_finite() || !(0.0..=0.20).contains(&value) {
            return Err(validation(format!(
                "mob {mob_id} speed_variance_ratio must be finite and in 0..=0.20"
            )));
        }
        Ok(Self(value))
    }
    pub fn as_ratio(self) -> f64 {
        self.0
    }
}

/// Validated mob endurance duration in seconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EnduranceSeconds(f64);
impl EnduranceSeconds {
    fn new(value: f64, mob_id: &MobId) -> Result<Self, GameDataError> {
        if !value.is_finite() || value <= 0.0 {
            return Err(validation(format!(
                "mob {mob_id} endurance_s must be finite and positive"
            )));
        }
        Ok(Self(value))
    }
    pub fn seconds(self) -> f64 {
        self.0
    }
}

#[derive(Debug, Clone)]
pub struct MobDefinition {
    id: MobId,
    name: String,
    faction: FactionId,
    mode: MobMode,
    movement_id: MovementId,
    species_id: Option<SpeciesId>,
    hp: i32,
    armor: i32,
    speed_variance_ratio: SpeedVarianceRatio,
    endurance_s: EnduranceSeconds,
    skills: Vec<ProfileSkill>,
    actions: Vec<ActionId>,
}

impl MobDefinition {
    pub fn id(&self) -> &MobId {
        &self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn faction(&self) -> &FactionId {
        &self.faction
    }
    pub fn mode(&self) -> MobMode {
        self.mode
    }
    pub fn movement_id(&self) -> &MovementId {
        &self.movement_id
    }
    pub fn species_id(&self) -> Option<&SpeciesId> {
        self.species_id.as_ref()
    }
    pub fn hp(&self) -> i32 {
        self.hp
    }
    pub fn armor(&self) -> i32 {
        self.armor
    }
    pub fn speed_variance_ratio(&self) -> SpeedVarianceRatio {
        self.speed_variance_ratio
    }
    pub fn endurance_s(&self) -> EnduranceSeconds {
        self.endurance_s
    }
    pub fn skills(&self) -> &[ProfileSkill] {
        &self.skills
    }
    pub fn actions(&self) -> &[ActionId] {
        &self.actions
    }
}

#[derive(Debug, Clone)]
pub struct MovementSpec {
    id: MovementId,
    speed_mps: f64,
}

impl MovementSpec {
    pub fn id(&self) -> &MovementId {
        &self.id
    }
    pub fn speed_mps(&self) -> f64 {
        self.speed_mps
    }
}

#[derive(Debug, Clone)]
pub struct DefaultValue {
    key: String,
    value: String,
}

impl DefaultValue {
    pub fn key(&self) -> &str {
        &self.key
    }
    pub fn value(&self) -> &str {
        &self.value
    }
}

#[derive(Debug, Clone)]
pub struct Hamlet {
    enabled: bool,
    width: i32,
    depth: i32,
    kit_catalog: String,
    layers: Vec<String>,
}

impl Hamlet {
    pub fn enabled(&self) -> bool {
        self.enabled
    }
    pub fn width(&self) -> i32 {
        self.width
    }
    pub fn depth(&self) -> i32 {
        self.depth
    }
    pub fn kit_catalog(&self) -> &str {
        &self.kit_catalog
    }
    pub fn layers(&self) -> &[String] {
        &self.layers
    }
}

// ---------------------------------------------------------------------------
// GameData aggregate.
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct GameData {
    skills: Vec<Skill>,
    factions: Vec<Faction>,
    effects: Vec<Effect>,
    actions: Vec<Action>,
    player_profiles: Vec<PlayerProfile>,
    mobs: Vec<MobDefinition>,
    movement: Vec<MovementSpec>,
    hamlet: Hamlet,
    defaults: Vec<DefaultValue>,

    skills_by_id: HashMap<SkillId, usize>,
    factions_by_id: HashMap<FactionId, usize>,
    effects_by_id: HashMap<EffectId, usize>,
    actions_by_id: HashMap<ActionId, usize>,
    profiles_by_id: HashMap<ProfileId, usize>,
    mobs_by_id: HashMap<MobId, usize>,
    movement_by_id: HashMap<MovementId, usize>,
}

impl GameData {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, GameDataError> {
        let path = path.as_ref();
        let text = fs::read_to_string(path).map_err(|source| GameDataError::Read {
            path: path.display().to_string(),
            source,
        })?;
        let raw: RawGameData = from_str(&text)?;
        Self::from_raw(raw)
    }

    /// Parse and validate GameData from an XML string (authoring tooling and
    /// headless tests).
    pub fn from_xml_str(text: &str) -> Result<Self, GameDataError> {
        let raw: RawGameData = from_str(text)?;
        Self::from_raw(raw)
    }

    fn from_raw(raw: RawGameData) -> Result<Self, GameDataError> {
        if raw.schema_version != SCHEMA_VERSION {
            return Err(validation(format!(
                "schema_version must be {SCHEMA_VERSION}"
            )));
        }
        validate_ids("skill", raw.skills.items.iter().map(|s| s.id.as_str()))?;
        validate_ids("faction", raw.factions.items.iter().map(|f| f.id.as_str()))?;
        validate_ids("effect", raw.effects.items.iter().map(|e| e.id.as_str()))?;
        validate_ids("action", raw.actions.items.iter().map(|a| a.id.as_str()))?;
        validate_ids("profile", raw.players.items.iter().map(|p| p.id.as_str()))?;
        validate_ids("mob", raw.mobs.items.iter().map(|m| m.id.as_str()))?;
        validate_ids("movement", raw.movement.items.iter().map(|m| m.id.as_str()))?;

        let skills: Vec<Skill> = raw
            .skills
            .items
            .into_iter()
            .map(|s| Skill {
                id: SkillId::new(s.id),
                name: s.name,
                description: s.description,
                level_scale: s.level_scale,
            })
            .collect();
        let factions: Vec<Faction> = raw
            .factions
            .items
            .into_iter()
            .map(|f| Faction {
                id: FactionId::new(f.id),
                name: f.name,
                neutral: f.neutral,
            })
            .collect();
        let effects: Vec<Effect> = raw
            .effects
            .items
            .into_iter()
            .map(|e| {
                Ok(Effect {
                    id: EffectId::new(e.id),
                    name: e.name,
                    operation: EffectOperation::from_str(&e.operation)?,
                    skill_id: SkillId::new(e.skill_id),
                    progression: Progression::from_str(&e.progression)?,
                })
            })
            .collect::<Result<_, GameDataError>>()?;
        let actions: Vec<Action> = raw
            .actions
            .items
            .into_iter()
            .map(|a| {
                Ok(Action {
                    id: ActionId::new(a.id),
                    name: a.name,
                    description: a.description,
                    target: ActionTarget::from_str(&a.target)?,
                    mana_cost: a.mana_cost,
                    cast_s: a.cast_s,
                    cooldown_s: a.cooldown_s,
                    interruptible: a.interruptible,
                    reveals: a.reveals,
                    effects: a
                        .effects
                        .items
                        .into_iter()
                        .map(|assignment| {
                            Ok(ActionEffect {
                                effect_id: EffectId::new(assignment.effect_id),
                                magnitude: assignment.magnitude,
                                application: Application::from_str(&assignment.application)?,
                                range_m: assignment.range_m,
                                radius_m: assignment.radius_m,
                                angle_deg: assignment.angle_deg,
                                duration_s: assignment.duration_s,
                                movement_multiplier: assignment.movement_multiplier,
                            })
                        })
                        .collect::<Result<Vec<_>, GameDataError>>()?,
                })
            })
            .collect::<Result<_, GameDataError>>()?;
        let player_profiles: Vec<PlayerProfile> = raw
            .players
            .items
            .into_iter()
            .map(|p| PlayerProfile {
                id: ProfileId::new(p.id),
                name: p.name,
                faction: FactionId::new(p.faction),
                skills: p
                    .skills
                    .into_iter()
                    .map(|s| ProfileSkill {
                        id: SkillId::new(s.id),
                        level: s.level,
                    })
                    .collect(),
                actions: p.actions.into_iter().map(|a| ActionId::new(a.id)).collect(),
            })
            .collect();
        let mobs: Vec<MobDefinition> = raw
            .mobs
            .items
            .into_iter()
            .map(|m| {
                let id = MobId::new(m.id);
                let speed_variance_ratio = SpeedVarianceRatio::new(m.speed_variance_ratio, &id)?;
                let endurance_s = EnduranceSeconds::new(m.endurance_s, &id)?;
                Ok(MobDefinition {
                    id,
                    name: m.name,
                    faction: FactionId::new(m.faction),
                    mode: MobMode::from_str(&m.mode)?,
                    movement_id: MovementId::new(m.movement_id),
                    species_id: (!m.species_id.is_empty()).then(|| SpeciesId::new(m.species_id)),
                    hp: m.hp,
                    armor: m.armor,
                    speed_variance_ratio,
                    endurance_s,
                    skills: m
                        .skills
                        .into_iter()
                        .map(|s| ProfileSkill {
                            id: SkillId::new(s.id),
                            level: s.level,
                        })
                        .collect(),
                    actions: m.actions.into_iter().map(|a| ActionId::new(a.id)).collect(),
                })
            })
            .collect::<Result<_, GameDataError>>()?;
        let movement: Vec<MovementSpec> = raw
            .movement
            .items
            .into_iter()
            .map(|m| MovementSpec {
                id: MovementId::new(m.id),
                speed_mps: m.speed_mps,
            })
            .collect();
        let hamlet = Hamlet {
            enabled: raw.hamlet.enabled,
            width: raw.hamlet.width,
            depth: raw.hamlet.depth,
            kit_catalog: raw.hamlet.kit_catalog,
            layers: raw.hamlet.layers.into_iter().map(|l| l.id).collect(),
        };
        let defaults: Vec<DefaultValue> = raw
            .defaults
            .items
            .into_iter()
            .map(|d| DefaultValue {
                key: d.key,
                value: d.value,
            })
            .collect();

        let data = GameData {
            skills_by_id: index(&skills, Skill::id),
            factions_by_id: index(&factions, Faction::id),
            effects_by_id: index(&effects, Effect::id),
            actions_by_id: index(&actions, Action::id),
            profiles_by_id: index(&player_profiles, PlayerProfile::id),
            mobs_by_id: index(&mobs, MobDefinition::id),
            movement_by_id: index(&movement, MovementSpec::id),
            skills,
            factions,
            effects,
            actions,
            player_profiles,
            mobs,
            movement,
            hamlet,
            defaults,
        };
        data.validate()?;
        Ok(data)
    }

    fn validate(&self) -> Result<(), GameDataError> {
        let neutral_count = self.factions.iter().filter(|f| f.neutral).count();
        if neutral_count != 1 {
            return Err(validation("exactly one neutral faction is required"));
        }

        for profile in &self.player_profiles {
            if !self.factions_by_id.contains_key(profile.faction()) {
                return Err(validation(format!(
                    "profile {} references unknown faction {}",
                    profile.id(),
                    profile.faction()
                )));
            }
            if profile.actions().is_empty() {
                return Err(validation(format!(
                    "profile {} action roster must not be empty",
                    profile.id()
                )));
            }
            validate_actor_roster(
                "profile",
                profile.id().as_str(),
                profile.skills(),
                profile.actions(),
                &self.skills_by_id,
                &self.actions_by_id,
                &self.actions,
                &self.effects_by_id,
                &self.effects,
            )?;
            for known in profile.skills() {
                if !self.skills_by_id.contains_key(known.id()) {
                    return Err(validation(format!(
                        "profile {} references unknown skill {}",
                        profile.id(),
                        known.id()
                    )));
                }
                if known.level() <= 0 {
                    return Err(validation(format!(
                        "profile {} skill {} has non-positive level {}",
                        profile.id(),
                        known.id(),
                        known.level()
                    )));
                }
            }
        }

        for effect in &self.effects {
            if !self.skills_by_id.contains_key(effect.skill_id()) {
                return Err(validation(format!(
                    "effect {} references unknown skill_id {}",
                    effect.id(),
                    effect.skill_id()
                )));
            }
        }

        for action in &self.actions {
            if !action.mana_cost().is_finite()
                || !action.cast_s().is_finite()
                || !action.cooldown_s().is_finite()
            {
                return Err(validation(format!(
                    "action {} timing and mana values must be finite",
                    action.id()
                )));
            }
            if action.mana_cost() < 0.0 {
                return Err(validation(format!(
                    "action {} mana_cost must be non-negative",
                    action.id()
                )));
            }
            if action.cast_s() < 0.0 || action.cooldown_s() < 0.0 {
                return Err(validation(format!(
                    "action {} cast_s and cooldown_s must be non-negative",
                    action.id()
                )));
            }
            for assignment in action.effects() {
                if !self.effects_by_id.contains_key(assignment.effect_id()) {
                    return Err(validation(format!(
                        "action {} references unknown effect {}",
                        action.id(),
                        assignment.effect_id()
                    )));
                }
                let effect = self
                    .effect(assignment.effect_id())
                    .expect("validated effect reference");
                validate_finite_action_numbers(action, assignment)?;
                match effect.operation() {
                    EffectOperation::DirectDamage | EffectOperation::Heal => {
                        if assignment.magnitude() <= 0.0
                            || assignment.duration_s() != 0.0
                            || assignment.movement_multiplier() != 1.0
                        {
                            return Err(validation(format!("action {} effect {} direct operations require positive magnitude, zero duration_s, and movement_multiplier 1", action.id(), assignment.effect_id())));
                        }
                    }
                    EffectOperation::Root | EffectOperation::Hold | EffectOperation::Charm => {
                        if assignment.magnitude() != 1.0
                            || assignment.duration_s() <= 0.0
                            || assignment.movement_multiplier() != 1.0
                        {
                            return Err(validation(format!("action {} effect {} control operation requires magnitude 1, positive duration_s, and movement_multiplier 1", action.id(), assignment.effect_id())));
                        }
                    }
                    EffectOperation::Snare => {
                        if assignment.magnitude() != 1.0
                            || assignment.duration_s() <= 0.0
                            || !(0.0 < assignment.movement_multiplier()
                                && assignment.movement_multiplier() < 1.0)
                        {
                            return Err(validation(format!("action {} effect {} snare requires magnitude 1, positive duration_s, and movement_multiplier in 0..1", action.id(), assignment.effect_id())));
                        }
                    }
                }
                validate_application(action.id(), assignment)?;
            }
        }

        let mut species_links = std::collections::BTreeSet::new();
        for mob in &self.mobs {
            if let Some(species_id) = mob.species_id() {
                if !species_links.insert(species_id.clone()) {
                    return Err(validation(format!(
                        "species {} is linked by more than one mob",
                        species_id
                    )));
                }
            }
            if !self.factions_by_id.contains_key(mob.faction()) {
                return Err(validation(format!(
                    "mob {} references unknown faction {}",
                    mob.id(),
                    mob.faction()
                )));
            }
            if !self.movement_by_id.contains_key(mob.movement_id()) {
                return Err(validation(format!(
                    "mob {} references unknown movement {}",
                    mob.id(),
                    mob.movement_id()
                )));
            }
            if mob.actions().is_empty() {
                return Err(validation(format!(
                    "mob {} action roster must not be empty",
                    mob.id()
                )));
            }
            validate_actor_roster(
                "mob",
                mob.id().as_str(),
                mob.skills(),
                mob.actions(),
                &self.skills_by_id,
                &self.actions_by_id,
                &self.actions,
                &self.effects_by_id,
                &self.effects,
            )?;
            for action in mob.actions() {
                if !self.actions_by_id.contains_key(action) {
                    return Err(validation(format!(
                        "mob {} references unknown action {}",
                        mob.id(),
                        action
                    )));
                }
            }
            if mob.hp() <= 0 {
                return Err(validation(format!(
                    "mob {} has non-positive combat value",
                    mob.id()
                )));
            }
        }

        for movement in &self.movement {
            if !movement.speed_mps().is_finite() || movement.speed_mps() <= 0.0 {
                return Err(validation(format!(
                    "movement {} speed_mps must be finite and positive",
                    movement.id()
                )));
            }
        }

        Ok(())
    }

    // Slices, in authored order.
    pub fn skills(&self) -> &[Skill] {
        &self.skills
    }
    pub fn factions(&self) -> &[Faction] {
        &self.factions
    }
    pub fn effects(&self) -> &[Effect] {
        &self.effects
    }
    pub fn actions(&self) -> &[Action] {
        &self.actions
    }
    pub fn player_profiles(&self) -> &[PlayerProfile] {
        &self.player_profiles
    }
    pub fn mobs(&self) -> &[MobDefinition] {
        &self.mobs
    }
    pub fn movement(&self) -> &[MovementSpec] {
        &self.movement
    }
    pub fn hamlet(&self) -> &Hamlet {
        &self.hamlet
    }
    pub fn defaults(&self) -> &[DefaultValue] {
        &self.defaults
    }

    // Indexed lookups.
    pub fn skill(&self, id: &SkillId) -> Option<&Skill> {
        self.skills_by_id.get(id).map(|&i| &self.skills[i])
    }
    pub fn faction(&self, id: &FactionId) -> Option<&Faction> {
        self.factions_by_id.get(id).map(|&i| &self.factions[i])
    }
    pub fn factions_are_hostile(&self, first: &FactionId, second: &FactionId) -> bool {
        assert!(
            self.factions_by_id.contains_key(first),
            "unknown faction {first}"
        );
        assert!(
            self.factions_by_id.contains_key(second),
            "unknown faction {second}"
        );
        first != second
    }
    pub fn effect(&self, id: &EffectId) -> Option<&Effect> {
        self.effects_by_id.get(id).map(|&i| &self.effects[i])
    }
    pub fn action(&self, id: &ActionId) -> Option<&Action> {
        self.actions_by_id.get(id).map(|&i| &self.actions[i])
    }
    pub fn profile(&self, id: &ProfileId) -> Option<&PlayerProfile> {
        self.profiles_by_id
            .get(id)
            .map(|&i| &self.player_profiles[i])
    }
    pub fn mob(&self, id: &MobId) -> Option<&MobDefinition> {
        self.mobs_by_id.get(id).map(|&i| &self.mobs[i])
    }
    pub fn movement_by_id(&self, id: &MovementId) -> Option<&MovementSpec> {
        self.movement_by_id.get(id).map(|&i| &self.movement[i])
    }

    pub fn hamlet_enabled(&self) -> bool {
        self.hamlet.enabled
    }

    /// Look up a `<defaults>` value by key.
    pub fn default_value(&self, key: &str) -> Option<&str> {
        self.defaults
            .iter()
            .find(|d| d.key() == key)
            .map(|d| d.value())
    }

    /// The authored default player profile, from the `default_player_profile`
    /// defaults entry. Loud if the entry is missing or names an unknown profile.
    pub fn default_player_profile_id(&self) -> Result<ProfileId, GameDataError> {
        let key = self
            .default_value("default_player_profile")
            .ok_or_else(|| validation("missing defaults entry `default_player_profile`"))?;
        let id = ProfileId::new(key);
        if !self.profiles_by_id.contains_key(&id) {
            return Err(validation(format!(
                "default_player_profile `{key}` is not a known profile"
            )));
        }
        Ok(id)
    }
}
fn index<K, T>(items: &[T], id: impl Fn(&T) -> &K) -> HashMap<K, usize>
where
    K: Clone + std::hash::Hash + Eq,
{
    items
        .iter()
        .enumerate()
        .map(|(i, item)| (id(item).clone(), i))
        .collect()
}

fn validate_ids<'a>(label: &str, ids: impl Iterator<Item = &'a str>) -> Result<(), GameDataError> {
    let mut seen = std::collections::HashSet::new();
    for value in ids {
        if value.is_empty()
            || !value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(validation(format!("{label} id {value:?} is invalid")));
        }
        if !seen.insert(value) {
            return Err(validation(format!("duplicate {label} id {value:?}")));
        }
    }
    Ok(())
}

fn validate_finite_action_numbers(
    action: &Action,
    assignment: &ActionEffect,
) -> Result<(), GameDataError> {
    for (name, value) in [
        ("magnitude", assignment.magnitude()),
        ("range_m", assignment.range_m()),
        ("radius_m", assignment.radius_m()),
        ("angle_deg", assignment.angle_deg()),
        ("duration_s", assignment.duration_s()),
        ("movement_multiplier", assignment.movement_multiplier()),
    ] {
        if !value.is_finite() {
            return Err(validation(format!(
                "action {} effect {} {name} must be finite",
                action.id(),
                assignment.effect_id()
            )));
        }
    }
    Ok(())
}

fn validate_actor_roster(
    label: &str,
    id: &str,
    skills: &[ProfileSkill],
    action_ids: &[ActionId],
    skills_by_id: &HashMap<SkillId, usize>,
    actions_by_id: &HashMap<ActionId, usize>,
    actions: &[Action],
    effects_by_id: &HashMap<EffectId, usize>,
    effects: &[Effect],
) -> Result<(), GameDataError> {
    let mut seen_skills = std::collections::HashSet::new();
    for skill in skills {
        if !seen_skills.insert(skill.id()) {
            return Err(validation(format!(
                "{label} {id} has duplicate skill {}",
                skill.id()
            )));
        }
        if !skills_by_id.contains_key(skill.id()) || skill.level() <= 0 {
            return Err(validation(format!(
                "{label} {id} has invalid skill {} level {}",
                skill.id(),
                skill.level()
            )));
        }
    }
    let mut seen_actions = std::collections::HashSet::new();
    for action_id in action_ids {
        if !seen_actions.insert(action_id) {
            return Err(validation(format!(
                "{label} {id} has duplicate action {action_id}"
            )));
        }
        let action = actions_by_id
            .get(action_id)
            .map(|&i| &actions[i])
            .ok_or_else(|| {
                validation(format!(
                    "{label} {id} references unknown action {action_id}"
                ))
            })?;
        for assignment in action.effects() {
            let effect = effects_by_id
                .get(assignment.effect_id())
                .map(|&i| &effects[i])
                .expect("action effects validated");
            if !seen_skills.contains(effect.skill_id()) {
                return Err(validation(format!(
                    "{label} {id} action {action_id} requires unassigned skill {}",
                    effect.skill_id()
                )));
            }
        }
    }
    Ok(())
}

fn validate_application(
    action_id: &ActionId,
    assignment: &ActionEffect,
) -> Result<(), GameDataError> {
    if assignment.range_m() < 0.0 || assignment.radius_m() < 0.0 {
        return Err(validation(format!(
            "action {} effect {} range and radius must be non-negative",
            action_id,
            assignment.effect_id()
        )));
    }
    match assignment.application() {
        Application::SingleTarget => {
            if assignment.range_m() <= 0.0 {
                return Err(validation(format!(
                    "action {} single_target effect {} requires positive range",
                    action_id,
                    assignment.effect_id()
                )));
            }
        }
        Application::Cone => {
            if assignment.range_m() <= 0.0 {
                return Err(validation(format!(
                    "action {} cone effect {} requires positive range",
                    action_id,
                    assignment.effect_id()
                )));
            }
            if !(0.0 < assignment.angle_deg() && assignment.angle_deg() <= 360.0) {
                return Err(validation(format!(
                    "action {} cone effect {} angle must be between 0 and 360 degrees",
                    action_id,
                    assignment.effect_id()
                )));
            }
        }
        Application::Aoe => {
            if assignment.range_m() <= 0.0 {
                return Err(validation(format!(
                    "action {} aoe effect {} requires positive range",
                    action_id,
                    assignment.effect_id()
                )));
            }
            if assignment.radius_m() <= 0.0 {
                return Err(validation(format!(
                    "action {} aoe effect {} requires positive radius",
                    action_id,
                    assignment.effect_id()
                )));
            }
        }
        Application::Pbaoe => {
            if assignment.range_m() != 0.0 {
                return Err(validation(format!(
                    "action {} pbaoe effect {} range must be zero",
                    action_id,
                    assignment.effect_id()
                )));
            }
            if assignment.radius_m() <= 0.0 {
                return Err(validation(format!(
                    "action {} pbaoe effect {} requires positive radius",
                    action_id,
                    assignment.effect_id()
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canonical_xml() -> String {
        std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../data/OrrunGameData.xml"),
        )
        .unwrap()
    }

    #[test]
    fn canonical_schema_three_loads_typed_contract() {
        let data = GameData::from_xml_str(&canonical_xml()).expect("canonical data");
        let profile = data.profile(&ProfileId::new("default_player")).unwrap();
        assert_eq!(profile.actions().len(), 7);
        let snare = data
            .action(&ActionId::new("hobble"))
            .unwrap()
            .effects()
            .first()
            .unwrap();
        assert_eq!(snare.duration_s(), 6.0);
        assert_eq!(snare.movement_multiplier(), 0.5);
        let heal = data.action(&ActionId::new("restore")).unwrap();
        assert!(heal.interruptible());
        assert!(!heal.reveals());
        let hexer = data.mob(&MobId::new("hexer")).unwrap();
        assert!(hexer
            .skills()
            .iter()
            .any(|s| s.id() == &SkillId::new("charm") && s.level() == 2));
    }

    #[test]
    fn rejects_unknown_attributes() {
        let xml = canonical_xml().replacen(r#"id="slash""#, r#"id="slash" surprise="x""#, 1);
        assert!(GameData::from_xml_str(&xml)
            .unwrap_err()
            .to_string()
            .contains("unknown field"));
    }

    #[test]
    fn rejects_non_finite_numeric_values() {
        for (old, new) in [
            (r#"mana_cost="0""#, r#"mana_cost="NaN""#),
            (r#"duration_s="4""#, r#"duration_s="NaN""#),
            (
                r#"movement_multiplier="0.5""#,
                r#"movement_multiplier="NaN""#,
            ),
            (r#"speed_mps="2.5""#, r#"speed_mps="NaN""#),
        ] {
            let xml = canonical_xml().replacen(old, new, 1);
            let error = GameData::from_xml_str(&xml).expect_err("non-finite must fail");
            assert!(error.to_string().contains("finite"), "{error}");
        }
    }

    #[test]
    fn enforces_operation_specific_assignment_domains() {
        let direct = canonical_xml().replacen(
            r#"duration_s="0" movement_multiplier="1""#,
            r#"duration_s="2" movement_multiplier="1""#,
            1,
        );
        assert!(GameData::from_xml_str(&direct)
            .unwrap_err()
            .to_string()
            .contains("zero duration_s"));
        let snare = canonical_xml().replacen(
            r#"duration_s="6" movement_multiplier="0.5""#,
            r#"duration_s="6" movement_multiplier="1""#,
            1,
        );
        assert!(GameData::from_xml_str(&snare)
            .unwrap_err()
            .to_string()
            .contains("0..1"));
    }

    #[test]
    fn rejects_incoherent_actor_rosters() {
        let xml = canonical_xml().replacen(r#"<skill id="melee" level="1" />"#, "", 1);
        assert!(GameData::from_xml_str(&xml)
            .unwrap_err()
            .to_string()
            .contains("requires unassigned skill"));
        let xml =
            canonical_xml().replacen(r#"<action id="slash" />"#, r#"<action id="missing" />"#, 1);
        assert!(GameData::from_xml_str(&xml)
            .unwrap_err()
            .to_string()
            .contains("unknown action"));
    }
}
