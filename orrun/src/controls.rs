//! Data-driven combat action bindings. Movement and reserved keys remain engine-owned.

use std::collections::{BTreeMap, HashSet};

use engine::{Input, Key};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::gamedata::{ActionId, GameData};

pub const RESERVED: [&str; 3] = ["Escape", "E", "Q"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReservedKey;

pub fn is_reserved(key: Key) -> bool {
    key == Key::E || key == Key::Q
}
pub fn reserved_names() -> [&'static str; 3] {
    RESERVED
}

/// Action ids pressed this frame. Kept ordered by the player's authored roster.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PressedActions(Vec<ActionId>);

impl PressedActions {
    pub const NONE: Self = Self(Vec::new());
    pub fn from_actions(actions: &[ActionId]) -> Self {
        Self(actions.to_vec())
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn iter(&self) -> impl Iterator<Item = &ActionId> {
        self.0.iter()
    }
}

/// Generic action-id to physical-key map. Unknown ids are rejected during validation.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct KeyBinds(BTreeMap<String, Option<String>>);

impl KeyBinds {
    pub fn for_default_profile(data: &GameData) -> Self {
        let profile_id = data
            .default_player_profile_id()
            .expect("validated default player profile");
        let profile = data
            .profile(&profile_id)
            .expect("validated default player profile exists");
        let mut map = BTreeMap::new();
        for (index, action) in profile.actions().iter().enumerate() {
            let key = default_action_key(index).unwrap_or_else(|| {
                panic!("default profile has more bindable actions than supported keys")
            });
            map.insert(action.as_str().to_owned(), Some(key.as_str().to_owned()));
        }
        Self(map)
    }

    /// Fill a missing settings map from the profile, then validate every id and key loudly.
    pub fn validate_for_default_profile(&mut self, data: &GameData) {
        if self.0.is_empty() {
            *self = Self::for_default_profile(data);
        }
        let profile_id = data
            .default_player_profile_id()
            .expect("validated default player profile");
        let profile = data
            .profile(&profile_id)
            .expect("validated default player profile exists");
        let roster: HashSet<&str> = profile.actions().iter().map(|id| id.as_str()).collect();
        for (id, key) in &self.0 {
            assert!(
                roster.contains(id.as_str()),
                "settings bind unknown/unavailable action {id}"
            );
            if let Some(name) = key {
                let parsed = Key::from_name(name)
                    .unwrap_or_else(|| panic!("settings bind {id} has unknown key {name}"));
                assert!(
                    !is_reserved(parsed),
                    "settings bind {id} uses reserved key {name}"
                );
            }
        }
        for (index, action) in profile.actions().iter().enumerate() {
            self.0
                .entry(action.as_str().to_owned())
                .or_insert_with(|| default_action_key(index).map(|key| key.as_str().to_owned()));
        }
        let mut owners = HashSet::new();
        for key in self.0.values().flatten() {
            assert!(
                owners.insert(key.clone()),
                "settings bind key {key} is assigned more than once"
            );
        }
    }

    pub fn get(&self, action: &ActionId) -> Option<Key> {
        self.0
            .get(action.as_str())
            .and_then(Option::as_deref)
            .and_then(Key::from_name)
            .filter(|key| !is_reserved(*key))
    }
    pub fn display(&self, action: &ActionId) -> String {
        self.0
            .get(action.as_str())
            .and_then(Option::as_deref)
            .filter(|s| !s.is_empty())
            .unwrap_or("unbound")
            .to_owned()
    }
    pub fn action_for(&self, key: Key) -> Option<ActionId> {
        if is_reserved(key) {
            return None;
        }
        self.0
            .iter()
            .find(|(_, bound)| bound.as_deref() == Some(key.as_str()))
            .map(|(id, _)| ActionId::new(id))
    }
    pub fn assign(&mut self, action: &ActionId, key: Key) -> Result<(), ReservedKey> {
        if is_reserved(key) {
            return Err(ReservedKey);
        }
        assert!(
            self.0.contains_key(action.as_str()),
            "cannot bind unavailable action {action}"
        );
        let name = key.as_str().to_owned();
        for bound in self.0.values_mut() {
            if bound.as_deref() == Some(name.as_str()) {
                *bound = None;
            }
        }
        self.0.insert(action.as_str().to_owned(), Some(name));
        Ok(())
    }
    pub fn missing(&self) -> Vec<ActionId> {
        self.0
            .iter()
            .filter(|(_, key)| key.is_none())
            .map(|(id, _)| ActionId::new(id))
            .collect()
    }
    pub fn inspect_map(&self) -> serde_json::Map<String, Value> {
        self.0
            .iter()
            .map(|(id, key)| {
                (
                    id.clone(),
                    key.clone().map(Value::String).unwrap_or(Value::Null),
                )
            })
            .collect()
    }
}

pub fn assign(binds: &mut KeyBinds, action: &ActionId, key: Key) -> Result<(), ReservedKey> {
    binds.assign(action, key)
}
pub fn missing_binds(binds: &KeyBinds) -> Vec<ActionId> {
    binds.missing()
}
pub fn resolve(binds: &KeyBinds, keys: impl IntoIterator<Item = Key>) -> Vec<ActionId> {
    let mut out = Vec::new();
    for key in keys {
        if let Some(action) = binds.action_for(key) {
            if !out.contains(&action) {
                out.push(action);
            }
        }
    }
    out
}
pub fn resolve_pressed(binds: &KeyBinds, roster: &[ActionId], input: &Input) -> PressedActions {
    PressedActions(
        roster
            .iter()
            .filter(|id| binds.get(id).is_some_and(|key| input.pressed(key)))
            .cloned()
            .collect(),
    )
}

fn default_action_key(index: usize) -> Option<Key> {
    [
        Key::Digit1,
        Key::Digit2,
        Key::Digit3,
        Key::Digit4,
        Key::Digit5,
        Key::Digit6,
        Key::Digit7,
        Key::Digit8,
    ]
    .get(index)
    .copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn data() -> GameData {
        GameData::load(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../data/OrrunGameData.xml"),
        )
        .unwrap()
    }
    #[test]
    fn defaults_follow_profile_roster() {
        let data = data();
        let binds = KeyBinds::for_default_profile(&data);
        assert_eq!(binds.get(&ActionId::new("slash")), Some(Key::Digit1));
        assert_eq!(binds.get(&ActionId::new("befriend")), Some(Key::Digit7));
        assert!(resolve(&binds, [Key::Q]).is_empty());
    }
    #[test]
    fn serialization_is_generic() {
        let data = data();
        let binds = KeyBinds::for_default_profile(&data);
        let value = serde_json::to_value(binds).unwrap();
        assert_eq!(value["arrow"], "2");
    }
}
