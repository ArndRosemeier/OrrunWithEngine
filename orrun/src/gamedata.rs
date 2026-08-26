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

use crate::combat::sheets::MobSheet;

const SCHEMA_VERSION: u32 = 1;

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
    /// Identifies an action such as `strike`, `fire_bolt`, or `mend`.
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

/// The finite categories an effect can belong to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectKind {
    Damage,
    Heal,
    Control,
    Movement,
    Defense,
    Utility,
}

impl EffectKind {
    fn from_str(value: &str) -> Result<Self, GameDataError> {
        match value {
            "damage" => Ok(EffectKind::Damage),
            "heal" => Ok(EffectKind::Heal),
            "control" => Ok(EffectKind::Control),
            "movement" => Ok(EffectKind::Movement),
            "defense" => Ok(EffectKind::Defense),
            "utility" => Ok(EffectKind::Utility),
            other => Err(validation(format!("unknown effect kind {other:?}"))),
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
    #[serde(rename = "@cooldown_s", default)]
    cooldown_s: f64,
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
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEffect {
    #[serde(rename = "@id")]
    id: String,
    #[serde(rename = "@name", default)]
    name: String,
    #[serde(rename = "@kind", default)]
    kind: String,
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
    #[serde(rename = "@hp")]
    hp: i32,
    #[serde(rename = "@armor", default)]
    armor: i32,
    #[serde(rename = "@damage")]
    damage: i32,
    #[serde(rename = "@swing_s", default = "default_swing")]
    swing_s: f64,
    #[serde(rename = "@reach_m", default = "default_reach")]
    reach_m: f64,
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
fn default_swing() -> f64 {
    1.0
}
fn default_reach() -> f64 {
    1.8
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
    kind: EffectKind,
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
    pub fn kind(&self) -> EffectKind {
        self.kind
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
}

#[derive(Debug, Clone)]
pub struct MobDefinition {
    id: MobId,
    name: String,
    faction: FactionId,
    mode: MobMode,
    movement_id: MovementId,
    hp: i32,
    armor: i32,
    damage: i32,
    swing_s: f64,
    reach_m: f64,
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
    pub fn hp(&self) -> i32 {
        self.hp
    }
    pub fn armor(&self) -> i32 {
        self.armor
    }
    pub fn damage(&self) -> i32 {
        self.damage
    }
    pub fn swing_s(&self) -> f64 {
        self.swing_s
    }
    pub fn reach_m(&self) -> f64 {
        self.reach_m
    }
    pub fn actions(&self) -> &[ActionId] {
        &self.actions
    }

    /// Temporary adapter: expose base stats in the legacy sheet shape so the
    /// live combat POC keeps running until canonical action resolution lands.
    fn to_mob_sheet(&self, movement: &MovementSpec) -> MobSheet {
        MobSheet {
            id: self.id.as_str().to_string(),
            name: self.name.clone(),
            hp: self.hp,
            armor: self.armor,
            damage: self.damage,
            swing_s: self.swing_s,
            slam_damage: None,
            slam_every_s: None,
            telegraph_s: None,
            reach_m: self.reach_m,
            speed_mps: movement.speed_mps,
            sight_m: 0.0,
            hear_m: 0.0,
            leash_m: 0.0,
            social_m: 0.0,
            xp: 0,
            token_brood: 0,
            specials: Vec::new(),
            scale_hp: None,
            scale_dmg: None,
            scale_xp: None,
        }
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
        validate_ids(
            "profile",
            raw.players.items.iter().map(|p| p.id.as_str()),
        )?;
        validate_ids("mob", raw.mobs.items.iter().map(|m| m.id.as_str()))?;
        validate_ids(
            "movement",
            raw.movement.items.iter().map(|m| m.id.as_str()),
        )?;

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
                    kind: EffectKind::from_str(&e.kind)?,
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
            })
            .collect();
        let mobs: Vec<MobDefinition> = raw
            .mobs
            .items
            .into_iter()
            .map(|m| {
                Ok(MobDefinition {
                    id: MobId::new(m.id),
                    name: m.name,
                    faction: FactionId::new(m.faction),
                    mode: MobMode::from_str(&m.mode)?,
                    movement_id: MovementId::new(m.movement_id),
                    hp: m.hp,
                    armor: m.armor,
                    damage: m.damage,
                    swing_s: m.swing_s,
                    reach_m: m.reach_m,
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
                if assignment.magnitude() <= 0.0 {
                    return Err(validation(format!(
                        "action {} effect {} magnitude must be positive",
                        action.id(),
                        assignment.effect_id()
                    )));
                }
                validate_application(action.id(), assignment)?;
            }
        }

        for mob in &self.mobs {
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
            for action in mob.actions() {
                if !self.actions_by_id.contains_key(action) {
                    return Err(validation(format!(
                        "mob {} references unknown action {}",
                        mob.id(),
                        action
                    )));
                }
            }
            if mob.hp() <= 0 || mob.damage() <= 0 || mob.swing_s() <= 0.0 || mob.reach_m() <= 0.0 {
                return Err(validation(format!(
                    "mob {} has non-positive combat value",
                    mob.id()
                )));
            }
        }

        for movement in &self.movement {
            if movement.speed_mps() <= 0.0 {
                return Err(validation(format!(
                    "movement {} speed_mps must be positive",
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
    pub fn effect(&self, id: &EffectId) -> Option<&Effect> {
        self.effects_by_id.get(id).map(|&i| &self.effects[i])
    }
    pub fn action(&self, id: &ActionId) -> Option<&Action> {
        self.actions_by_id.get(id).map(|&i| &self.actions[i])
    }
    pub fn profile(&self, id: &ProfileId) -> Option<&PlayerProfile> {
        self.profiles_by_id.get(id).map(|&i| &self.player_profiles[i])
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

    /// Legacy adapter for the combat POC: resolve a mob id to a bare sheet.
    pub fn mob_sheet(&self, id: &str) -> Result<MobSheet, GameDataError> {
        let mob = self
            .mobs
            .iter()
            .find(|mob| mob.id().as_str() == id)
            .ok_or_else(|| validation(format!("unknown mob id {id:?}")))?;
        let movement = self
            .movement
            .iter()
            .find(|movement| movement.id() == mob.movement_id())
            .ok_or_else(|| {
                validation(format!("mob {id:?} references missing movement {:?}", mob.movement_id()))
            })?;
        let mut sheet = mob.to_mob_sheet(movement);
        sheet.sight_m = crate::combat::math::SIGHT_AGGRO_M;
        sheet.hear_m = crate::combat::math::HEAR_AGGRO_M;
        sheet.leash_m = crate::combat::math::LEASH_M;
        sheet.social_m = crate::combat::math::SOCIAL_M;
        Ok(sheet)
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

fn validate_ids<'a>(
    label: &str,
    ids: impl Iterator<Item = &'a str>,
) -> Result<(), GameDataError> {
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

fn validate_application(action_id: &ActionId, assignment: &ActionEffect) -> Result<(), GameDataError> {
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

    fn minimal_valid_xml() -> String {
        r#"<OrrunGameData schema_version="1"><skills><skill id="slashing_damage" name="Slashing Damage" level_scale="1"/></skills><factions><faction id="neutral" neutral="true"/><faction id="citizen" neutral="false"/></factions><effects><effect id="slashing_damage" name="Slashing Damage" kind="damage" skill_id="slashing_damage" progression="skill_level"/></effects><actions><action id="strike" name="Strike" target="hostile"><effects><effect effect_id="slashing_damage" magnitude="1" application="single_target" range_m="1.8"/></effects></action></actions><players><profile id="default_player" name="Adventurer" faction="citizen"><skill id="slashing_damage" level="1"/></profile></players><mobs><mob id="crawler_spider_wolf" name="Wolf-spider" faction="citizen" mode="active" hp="70" armor="0" damage="10" movement_id="walk"><action id="strike"/></mob></mobs><movement><spec id="walk" speed_mps="2.5"/></movement><hamlet enabled="true"/><defaults/></OrrunGameData>"#.to_string()
    }

    #[test]
    fn rejects_duplicate_and_invalid_data() {
        let xml = r#"<OrrunGameData schema_version="1"><skills/><factions><faction id="neutral" neutral="true"/><faction id="neutral" neutral="false"/></factions><actions/><players/><mobs/><movement/></OrrunGameData>"#;
        let raw: RawGameData = from_str(xml).unwrap();
        assert!(GameData::from_raw(raw).is_err());
    }

    #[test]
    fn loads_minimal_valid_data() {
        let raw: RawGameData = from_str(&minimal_valid_xml()).unwrap();
        let data = GameData::from_raw(raw).expect("minimal data should load");
        assert_eq!(data.skills().len(), 1);
        assert_eq!(data.actions().len(), 1);
        assert_eq!(data.mobs().len(), 1);
    }

    #[test]
    fn exposes_previously_dropped_fields() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../data/OrrunGameData.xml");
        let data = GameData::load(path).expect("canonical data loads");

        let fire = data.skill(&SkillId::new("fire_damage")).expect("fire skill");
        assert_eq!(fire.level_scale(), 1.0);

        let profile = data
            .profile(&ProfileId::new("default_player"))
            .expect("default player");
        assert!(!profile.skills().is_empty(), "profile skills are loaded");
        for known in profile.skills() {
            assert!(known.level() > 0, "starting level is positive");
        }

        let strike = data.action(&ActionId::new("strike")).expect("strike action");
        assert!(!strike.effects().is_empty(), "strike has effect assignments");

        let wolf = data
            .mob(&MobId::new("crawler_spider_wolf"))
            .expect("wolf mob");
        assert!(wolf.actions().contains(&ActionId::new("strike")));
    }

    #[test]
    fn rejects_zero_radius_aoe() {
        let xml = minimal_valid_xml().replace(
            r#"application="single_target" range_m="1.8""#,
            r#"application="aoe" range_m="5" radius_m="0""#,
        );
        let raw: RawGameData = from_str(&xml).unwrap();
        assert!(GameData::from_raw(raw).is_err());
    }

    #[test]
    fn rejects_unknown_mob_action() {
        let xml = minimal_valid_xml().replace(r#"<action id="strike"/></mob>"#, r#"<action id="missing"/></mob>"#);
        let raw: RawGameData = from_str(&xml).unwrap();
        assert!(GameData::from_raw(raw).is_err());
    }

    #[test]
    fn rejects_unknown_profile_skill() {
        let xml = minimal_valid_xml().replace(
            r#"<skill id="slashing_damage" level="1"/>"#,
            r#"<skill id="missing" level="1"/>"#,
        );
        let raw: RawGameData = from_str(&xml).unwrap();
        assert!(GameData::from_raw(raw).is_err());
    }
}