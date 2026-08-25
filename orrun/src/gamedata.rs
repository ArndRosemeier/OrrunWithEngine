//! Canonical authored game data loaded once at runtime.
use crate::combat::sheets::MobSheet;
use quick_xml::de::from_str;
use serde::Deserialize;
use std::{fs, path::Path};
use thiserror::Error;

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

#[derive(Debug, Clone, Deserialize)]
#[serde(rename = "OrrunGameData", deny_unknown_fields)]
struct RawGameData {
    #[serde(rename = "@schema_version")]
    schema_version: u32,
    skills: Skills,
    factions: Factions,
    #[serde(default)]
    effects: EffectsCatalog,
    actions: Actions,
    players: Players,
    mobs: Mobs,
    movement: Movement,
    #[serde(default)]
    hamlet: Hamlet,
    #[serde(rename = "defaults", default)]
    _defaults: serde::de::IgnoredAny,
}
#[derive(Debug, Clone, Deserialize)]
struct Skills {
    #[serde(rename = "skill", default)]
    items: Vec<Skill>,
}
#[derive(Debug, Clone, Deserialize)]
struct Factions {
    #[serde(rename = "faction", default)]
    items: Vec<Faction>,
}
#[derive(Debug, Clone, Deserialize)]
struct Actions {
    #[serde(rename = "action", default)]
    items: Vec<Action>,
}
#[derive(Debug, Clone, Deserialize)]
struct Players {
    #[serde(rename = "profile", default)]
    items: Vec<PlayerProfile>,
}
#[derive(Debug, Clone, Deserialize)]
struct Mobs {
    #[serde(rename = "mob", default)]
    items: Vec<MobDefinition>,
}
#[derive(Debug, Clone, Deserialize)]
struct Movement {
    #[serde(rename = "spec", default)]
    items: Vec<MovementSpec>,
}
#[derive(Debug, Clone, Default, Deserialize)]
struct Hamlet {
    #[serde(rename = "@enabled", default = "default_true")]
    enabled: bool,
}
fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct Skill {
    #[serde(rename = "@id")]
    id: String,
    #[serde(rename = "@name")]
    name: String,
    #[serde(rename = "@description", default)]
    description: String,
}
fn one() -> f64 {
    1.0
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
    "default".into()
}
#[derive(Debug, Clone, Deserialize)]
pub struct Faction {
    #[serde(rename = "@id")]
    id: String,
    #[serde(rename = "@name", default)]
    name: String,
    #[serde(rename = "@neutral", default)]
    neutral: bool,
}
#[derive(Debug, Clone, Deserialize)]
pub struct Action {
    #[serde(rename = "@id")]
    id: String,
    #[serde(rename = "@name", default)]
    name: String,
    #[serde(rename = "@target", default)]
    target: String,
    #[serde(rename = "effects", default)]
    effects: ActionEffects,
}
#[derive(Debug, Clone, Default, Deserialize)]
struct EffectsCatalog {
    #[serde(rename = "effect", default)]
    items: Vec<Effect>,
}
#[derive(Debug, Clone, Default, Deserialize)]
struct ActionEffects {
    #[serde(rename = "effect", default)]
    items: Vec<ActionEffect>,
}
#[derive(Debug, Clone, Deserialize)]
pub struct ActionEffect {
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
fn default_application() -> String {
    "single_target".into()
}
fn default_range() -> f64 {
    1.8
}
#[derive(Debug, Clone, Deserialize)]
pub struct Effect {
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
fn default_progression() -> String {
    "skill_level".into()
}
#[derive(Debug, Clone, Deserialize)]
pub struct PlayerProfile {
    #[serde(rename = "@id")]
    id: String,
    #[serde(rename = "@name", default)]
    name: String,
    #[serde(rename = "@faction")]
    faction: String,
}
#[derive(Debug, Clone, Deserialize)]
pub struct MobDefinition {
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
    #[serde(rename = "@xp", default)]
    xp: i32,
}
#[derive(Debug, Clone, Deserialize)]
pub struct MovementSpec {
    #[serde(rename = "@id")]
    id: String,
    #[serde(rename = "@speed_mps", alias = "@speed")]
    speed_mps: f64,
}

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
    fn from_raw(raw: RawGameData) -> Result<Self, GameDataError> {
        if raw.schema_version != SCHEMA_VERSION {
            return Err(GameDataError::Validation(format!(
                "schema_version must be {SCHEMA_VERSION}"
            )));
        }
        let data = Self {
            skills: raw.skills.items,
            factions: raw.factions.items,
            effects: raw.effects.items,
            actions: raw.actions.items,
            player_profiles: raw.players.items,
            mobs: raw.mobs.items,
            movement: raw.movement.items,
            hamlet: raw.hamlet,
        };
        data.validate()?;
        Ok(data)
    }
    fn validate(&self) -> Result<(), GameDataError> {
        fn ids<T>(label: &str, xs: &[T], id: impl Fn(&T) -> &str) -> Result<(), GameDataError> {
            let mut seen = std::collections::HashSet::new();
            for x in xs {
                let value = id(x);
                if value.is_empty()
                    || !value
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                {
                    return Err(GameDataError::Validation(format!(
                        "{label} id {value:?} is invalid"
                    )));
                }
                if !seen.insert(value) {
                    return Err(GameDataError::Validation(format!(
                        "duplicate {label} id {value:?}"
                    )));
                }
            }
            Ok(())
        }
        ids("skill", &self.skills, |x| &x.id)?;
        ids("faction", &self.factions, |x| &x.id)?;
        ids("effect", &self.effects, |x| &x.id)?;
        ids("action", &self.actions, |x| &x.id)?;
        ids("profile", &self.player_profiles, |x| &x.id)?;
        ids("mob", &self.mobs, |x| &x.id)?;
        ids("movement", &self.movement, |x| &x.id)?;
        let faction_ids: std::collections::HashSet<_> =
            self.factions.iter().map(|x| x.id.as_str()).collect();
        if self.factions.iter().filter(|x| x.neutral).count() != 1 {
            return Err(GameDataError::Validation(
                "exactly one neutral faction is required".into(),
            ));
        }
        let skill_ids: std::collections::HashSet<_> =
            self.skills.iter().map(|x| x.id.as_str()).collect();
        let effect_ids: std::collections::HashSet<_> =
            self.effects.iter().map(|x| x.id.as_str()).collect();
        let action_ids: std::collections::HashSet<_> =
            self.actions.iter().map(|x| x.id.as_str()).collect();
        let move_ids: std::collections::HashSet<_> =
            self.movement.iter().map(|x| x.id.as_str()).collect();
        for p in &self.player_profiles {
            if !faction_ids.contains(p.faction.as_str()) {
                return Err(GameDataError::Validation(format!(
                    "profile {} references unknown faction {}",
                    p.id, p.faction
                )));
            }
        }
        for action in &self.actions {
            if !matches!(
                action.target.as_str(),
                "hostile" | "friendly" | "self" | "any" | "none"
            ) {
                return Err(GameDataError::Validation(format!(
                    "action {} has unknown target {}",
                    action.id, action.target
                )));
            }
        }
        for effect in &self.effects {
            if !matches!(
                effect.kind.as_str(),
                "damage" | "heal" | "control" | "movement" | "defense" | "utility"
            ) {
                return Err(GameDataError::Validation(format!(
                    "effect {} has unknown kind {}",
                    effect.id, effect.kind
                )));
            }
            if !skill_ids.contains(effect.skill_id.as_str()) {
                return Err(GameDataError::Validation(format!(
                    "effect {} references unknown skill_id {}",
                    effect.id, effect.skill_id
                )));
            }
            if !matches!(effect.progression.as_str(), "skill_level" | "flat") {
                return Err(GameDataError::Validation(format!(
                    "effect {} has unknown progression {}",
                    effect.id, effect.progression
                )));
            }
        }
        for action in &self.actions {
            for assignment in &action.effects.items {
                if !effect_ids.contains(assignment.effect_id.as_str()) {
                    return Err(GameDataError::Validation(format!(
                        "action {} references unknown effect {}",
                        action.id, assignment.effect_id
                    )));
                }
                if assignment.magnitude <= 0.0 {
                    return Err(GameDataError::Validation(format!(
                        "action {} effect {} magnitude must be positive",
                        action.id, assignment.effect_id
                    )));
                }
            }
        }
        for m in &self.mobs {
            if !matches!(m.mode.as_str(), "active" | "passive") {
                return Err(GameDataError::Validation(format!(
                    "mob {} has unknown mode {}",
                    m.id, m.mode
                )));
            }
            if !faction_ids.contains(m.faction.as_str()) {
                return Err(GameDataError::Validation(format!(
                    "mob {} references unknown faction {}",
                    m.id, m.faction
                )));
            }
            if !move_ids.contains(m.movement_id.as_str()) {
                return Err(GameDataError::Validation(format!(
                    "mob {} references unknown movement {}",
                    m.id, m.movement_id
                )));
            }
            if m.hp <= 0 || m.damage <= 0 || m.swing_s <= 0.0 || m.reach_m <= 0.0 {
                return Err(GameDataError::Validation(format!(
                    "mob {} has non-positive combat value",
                    m.id
                )));
            }
        }
        for movement in &self.movement {
            if movement.speed_mps <= 0.0 {
                return Err(GameDataError::Validation(format!(
                    "movement {} speed_mps must be positive",
                    movement.id
                )));
            }
        }
        let _ = action_ids;
        Ok(())
    }
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
    pub fn hamlet_enabled(&self) -> bool {
        self.hamlet.enabled
    }
    pub fn mob_sheet(&self, id: &str) -> Result<MobSheet, GameDataError> {
        let mob = self
            .mobs
            .iter()
            .find(|mob| mob.id == id)
            .ok_or_else(|| GameDataError::Validation(format!("unknown mob id {id:?}")))?;
        let movement = self
            .movement
            .iter()
            .find(|movement| movement.id == mob.movement_id)
            .ok_or_else(|| {
                GameDataError::Validation(format!(
                    "mob {id:?} references missing movement {:?}",
                    mob.movement_id
                ))
            })?;
        let mut sheet = mob.to_mob_sheet(movement);
        sheet.sight_m = crate::combat::math::SIGHT_AGGRO_M;
        sheet.hear_m = crate::combat::math::HEAR_AGGRO_M;
        sheet.leash_m = crate::combat::math::LEASH_M;
        sheet.social_m = crate::combat::math::SOCIAL_M;
        Ok(sheet)
    }
}
impl Skill {
    pub fn description(&self) -> &str {
        &self.description
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
}
impl Action {
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn target(&self) -> &str {
        &self.target
    }
    pub fn effects(&self) -> &[ActionEffect] {
        &self.effects.items
    }
}
impl Effect {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn kind(&self) -> &str {
        &self.kind
    }
    pub fn skill_id(&self) -> &str {
        &self.skill_id
    }
    pub fn progression(&self) -> &str {
        &self.progression
    }
}
impl ActionEffect {
    pub fn effect_id(&self) -> &str {
        &self.effect_id
    }
    pub fn magnitude(&self) -> f64 {
        self.magnitude
    }
    pub fn application(&self) -> &str {
        &self.application
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
impl PlayerProfile {
    pub fn name(&self) -> &str {
        &self.name
    }
}
impl Faction {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn is_neutral(&self) -> bool {
        self.neutral
    }
}
impl MobDefinition {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn faction(&self) -> &str {
        &self.faction
    }
    pub fn movement_id(&self) -> &str {
        &self.movement_id
    }
    pub fn to_mob_sheet(&self, movement: &MovementSpec) -> MobSheet {
        MobSheet {
            id: self.id.clone(),
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
            xp: self.xp,
            token_brood: 0,
            specials: Vec::new(),
            scale_hp: None,
            scale_dmg: None,
            scale_xp: None,
        }
    }
}
impl MovementSpec {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn speed_mps(&self) -> f64 {
        self.speed_mps
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_duplicate_and_invalid_data() {
        let xml = r#"<OrrunGameData schema_version="1"><skills/><factions><faction id="neutral" neutral="true"/><faction id="neutral" neutral="false"/></factions><actions/><players/><mobs/><movement/></OrrunGameData>"#;
        let raw: RawGameData = from_str(xml).unwrap();
        assert!(GameData::from_raw(raw).is_err());
    }
}
