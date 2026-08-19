//! Press-verb binds. Last assign wins. Esc/E reserved. Q stays strafe.

use engine::{Input, Key};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Bindable press-verb. Locomotion (Q/E/Tab/Shift/F/Space) is not here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Strike,
    Bash,
    AimedShot,
    Pin,
    Ember,
    Bind,
    Mend,
    Ward,
    Potion,
    Mark,
    SecondWind,
}

impl Action {
    pub const ALL: [Self; 11] = [
        Self::Strike,
        Self::Bash,
        Self::AimedShot,
        Self::Pin,
        Self::Ember,
        Self::Bind,
        Self::Mend,
        Self::Ward,
        Self::Potion,
        Self::Mark,
        Self::SecondWind,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::Strike => "strike",
            Self::Bash => "bash",
            Self::AimedShot => "aimed",
            Self::Pin => "pin",
            Self::Ember => "ember",
            Self::Bind => "bind",
            Self::Mend => "mend",
            Self::Ward => "ward",
            Self::Potion => "potion",
            Self::Mark => "mark",
            Self::SecondWind => "second_wind",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Strike => "Strike",
            Self::Bash => "Bash",
            Self::AimedShot => "Aimed Shot",
            Self::Pin => "Pin",
            Self::Ember => "Ember",
            Self::Bind => "Bind",
            Self::Mend => "Mend",
            Self::Ward => "Ward",
            Self::Potion => "Potion",
            Self::Mark => "Mark",
            Self::SecondWind => "Second Wind",
        }
    }

    pub fn default_key(self) -> Key {
        match self {
            Self::Strike => Key::Digit1,
            Self::Bash => Key::Digit2,
            Self::AimedShot => Key::Digit3,
            Self::Pin => Key::Digit4,
            Self::Ember => Key::Digit5,
            Self::Bind => Key::Digit6,
            Self::Mend => Key::Digit7,
            Self::Ward => Key::Digit8,
            Self::Potion => Key::R,
            Self::Mark => Key::T,
            Self::SecondWind => Key::G,
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Some(match id {
            "strike" => Self::Strike,
            "bash" => Self::Bash,
            "aimed" | "aimed_shot" => Self::AimedShot,
            "pin" => Self::Pin,
            "ember" => Self::Ember,
            "bind" => Self::Bind,
            "mend" => Self::Mend,
            "ward" => Self::Ward,
            "potion" => Self::Potion,
            "mark" => Self::Mark,
            "second_wind" => Self::SecondWind,
            _ => return None,
        })
    }

    /// Rank-gate: the key is a no-op until this rank is known.
    pub fn rank_ok(self, martial: i32, hunt: i32, arcane: i32) -> bool {
        match self {
            Self::Strike => martial >= 1,
            Self::Bash => martial >= 3,
            Self::AimedShot => hunt >= 1,
            Self::Pin => hunt >= 3,
            Self::Ember => true,
            Self::Bind => arcane >= 5,
            Self::Mend => arcane >= 3,
            Self::Ward => arcane >= 7,
            Self::Potion => true,
            Self::Mark => hunt >= 10,
            Self::SecondWind => martial >= 10,
        }
    }

    fn bit(self) -> u16 {
        1 << (self as u16)
    }
}

impl std::fmt::Display for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Rank miss recorded for inspect. The press itself is a silent no-op.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RankGate {
    pub action: Action,
    pub blocked: bool,
}

/// Copy-sized set of actions that went down this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PressedActions {
    bits: u16,
}

impl PressedActions {
    pub const NONE: Self = Self { bits: 0 };

    pub fn from_actions(actions: &[Action]) -> Self {
        let mut bits = 0u16;
        for action in actions {
            bits |= action.bit();
        }
        Self { bits }
    }

    pub fn insert(&mut self, action: Action) {
        self.bits |= action.bit();
    }

    pub fn is_empty(self) -> bool {
        self.bits == 0
    }

    pub fn iter(self) -> impl Iterator<Item = Action> {
        Action::ALL.into_iter().filter(move |action| self.bits & action.bit() != 0)
    }
}

/// Escape is not an engine gameplay [Key]; E is door interact. Neither is rebindable.
pub const RESERVED: [&str; 2] = ["Escape", "E"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReservedKey;

pub fn is_reserved(key: Key) -> bool {
    key == Key::E
}

pub fn reserved_names() -> [&'static str; 2] {
    RESERVED
}

pub fn missing_binds(binds: &KeyBinds) -> Vec<Action> {
    binds.missing()
}

/// Resolve physical keys against the current binds. Reserved keys never resolve.
pub fn resolve(binds: &KeyBinds, keys: impl IntoIterator<Item = Key>) -> Vec<Action> {
    let mut out = Vec::new();
    for key in keys {
        if is_reserved(key) {
            continue;
        }
        if let Some(action) = binds.action_for(key) {
            if !out.contains(&action) {
                out.push(action);
            }
        }
    }
    out
}

/// Edge-triggered: every matching pressed bind this frame.
pub fn resolve_pressed(binds: &KeyBinds, input: &Input) -> PressedActions {
    let mut found = PressedActions::NONE;
    for action in Action::ALL {
        if let Some(key) = binds.get(action) {
            if is_reserved(key) {
                continue;
            }
            if input.pressed(key) {
                found.insert(action);
            }
        }
    }
    found
}

/// Last assign wins. Reserved keys are rejected.
pub fn assign(binds: &mut KeyBinds, action: Action, key: Key) -> Result<(), ReservedKey> {
    if is_reserved(key) {
        return Err(ReservedKey);
    }
    binds.apply_assign(action, key);
    Ok(())
}

/// Action to key. None is unbound. Last assign wins on conflict.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct KeyBinds {
    #[serde(default = "default_strike")]
    pub strike: Option<String>,
    #[serde(default = "default_bash")]
    pub bash: Option<String>,
    #[serde(default = "default_aimed")]
    pub aimed: Option<String>,
    #[serde(default = "default_pin")]
    pub pin: Option<String>,
    #[serde(default = "default_ember")]
    pub ember: Option<String>,
    #[serde(default = "default_bind")]
    pub bind: Option<String>,
    #[serde(default = "default_mend")]
    pub mend: Option<String>,
    #[serde(default = "default_ward")]
    pub ward: Option<String>,
    /// Potion. Default is R: Q already sidesteps.
    #[serde(default = "default_potion")]
    pub potion: Option<String>,
    #[serde(default = "default_mark")]
    pub mark: Option<String>,
    #[serde(default = "default_second_wind")]
    pub second_wind: Option<String>,
}

fn default_strike() -> Option<String> {
    Some("1".into())
}
fn default_bash() -> Option<String> {
    Some("2".into())
}
fn default_aimed() -> Option<String> {
    Some("3".into())
}
fn default_pin() -> Option<String> {
    Some("4".into())
}
fn default_ember() -> Option<String> {
    Some("5".into())
}
fn default_bind() -> Option<String> {
    Some("6".into())
}
fn default_mend() -> Option<String> {
    Some("7".into())
}
fn default_ward() -> Option<String> {
    Some("8".into())
}
fn default_potion() -> Option<String> {
    Some("R".into())
}
fn default_mark() -> Option<String> {
    Some("T".into())
}
fn default_second_wind() -> Option<String> {
    Some("G".into())
}

impl Default for KeyBinds {
    fn default() -> Self {
        Self {
            strike: default_strike(),
            bash: default_bash(),
            aimed: default_aimed(),
            pin: default_pin(),
            ember: default_ember(),
            bind: default_bind(),
            mend: default_mend(),
            ward: default_ward(),
            potion: default_potion(),
            mark: default_mark(),
            second_wind: default_second_wind(),
        }
    }
}

impl KeyBinds {
    fn slot(&self, action: Action) -> Option<&str> {
        match action {
            Action::Strike => self.strike.as_deref(),
            Action::Bash => self.bash.as_deref(),
            Action::AimedShot => self.aimed.as_deref(),
            Action::Pin => self.pin.as_deref(),
            Action::Ember => self.ember.as_deref(),
            Action::Bind => self.bind.as_deref(),
            Action::Mend => self.mend.as_deref(),
            Action::Ward => self.ward.as_deref(),
            Action::Potion => self.potion.as_deref(),
            Action::Mark => self.mark.as_deref(),
            Action::SecondWind => self.second_wind.as_deref(),
        }
    }

    fn slot_mut(&mut self, action: Action) -> &mut Option<String> {
        match action {
            Action::Strike => &mut self.strike,
            Action::Bash => &mut self.bash,
            Action::AimedShot => &mut self.aimed,
            Action::Pin => &mut self.pin,
            Action::Ember => &mut self.ember,
            Action::Bind => &mut self.bind,
            Action::Mend => &mut self.mend,
            Action::Ward => &mut self.ward,
            Action::Potion => &mut self.potion,
            Action::Mark => &mut self.mark,
            Action::SecondWind => &mut self.second_wind,
        }
    }

    pub fn get(&self, action: Action) -> Option<Key> {
        self.slot(action)
            .and_then(Key::from_name)
            .filter(|key| !is_reserved(*key))
    }

    pub fn display(&self, action: Action) -> String {
        match self.slot(action) {
            Some(name) if !name.is_empty() => name.to_string(),
            _ => "unbound".into(),
        }
    }

    pub fn action_for(&self, key: Key) -> Option<Action> {
        if is_reserved(key) {
            return None;
        }
        let name = key.as_str();
        for action in Action::ALL {
            if self.slot(action) == Some(name) {
                return Some(action);
            }
        }
        None
    }

    pub fn verb_for(&self, key: Key) -> Option<Action> {
        resolve(self, [key]).into_iter().next()
    }

    fn apply_assign(&mut self, action: Action, key: Key) {
        let name = key.as_str().to_string();
        for other in Action::ALL {
            if other != action && self.slot(other) == Some(name.as_str()) {
                *self.slot_mut(other) = None;
            }
        }
        *self.slot_mut(action) = Some(name);
    }

    /// Last assign wins: the previous owner of key becomes unbound.
    /// Reserved keys are rejected and the map is left unchanged.
    pub fn assign(&mut self, action: Action, key: Key) {
        let _ = crate::controls::assign(self, action, key);
    }

    pub fn missing(&self) -> Vec<Action> {
        Action::ALL
            .into_iter()
            .filter(|a| self.get(*a).is_none())
            .collect()
    }

    pub fn inspect_map(&self) -> serde_json::Map<String, Value> {
        let mut m = serde_json::Map::new();
        for action in Action::ALL {
            let v = match self.slot(action) {
                Some(name) => Value::String(name.to_string()),
                None => Value::Null,
            };
            m.insert(action.id().into(), v);
        }
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_cover_every_press_verb() {
        let keys = KeyBinds::default();
        assert!(keys.missing().is_empty());
        assert_eq!(keys.get(Action::Strike), Some(Key::Digit1));
        assert_eq!(keys.get(Action::Bash), Some(Key::Digit2));
        assert_eq!(keys.get(Action::Potion), Some(Key::R));
        assert_eq!(keys.get(Action::Mark), Some(Key::T));
        assert_eq!(keys.get(Action::SecondWind), Some(Key::G));
        assert_eq!(keys.verb_for(Key::Q), None);
        assert_eq!(keys.verb_for(Key::E), None);
        assert_eq!(resolve(&keys, [Key::Digit1]), vec![Action::Strike]);
        assert!(resolve(&keys, [Key::Q]).is_empty());
    }

    #[test]
    fn last_assign_unbinds_the_previous_owner() {
        let mut keys = KeyBinds::default();
        assert!(assign(&mut keys, Action::Strike, Key::Digit5).is_ok());
        assert_eq!(keys.get(Action::Strike), Some(Key::Digit5));
        assert_eq!(keys.get(Action::Ember), None);
        assert_eq!(keys.display(Action::Ember), "unbound");
    }

    #[test]
    fn reserved_e_is_not_rebindable() {
        let mut keys = KeyBinds::default();
        assert!(assign(&mut keys, Action::Strike, Key::E).is_err());
        assert_eq!(keys.get(Action::Strike), Some(Key::Digit1));
        assert!(is_reserved(Key::E));
        assert!(resolve(&keys, [Key::E]).is_empty());
        assert_eq!(RESERVED, ["Escape", "E"]);
    }

    #[test]
    fn q_is_strafe_not_a_default_bind() {
        let keys = KeyBinds::default();
        assert!(!is_reserved(Key::Q));
        assert_eq!(keys.action_for(Key::Q), None);
        assert_eq!(keys.get(Action::Potion), Some(Key::R));
    }

    #[test]
    fn bash_is_rank_gated_on_l1_martial() {
        assert!(Action::Strike.rank_ok(1, 0, 0));
        assert!(!Action::Bash.rank_ok(1, 0, 0));
        assert!(Action::Ember.rank_ok(1, 0, 0));
        assert!(!Action::Mark.rank_ok(1, 0, 0));
        assert!(!Action::SecondWind.rank_ok(1, 0, 0));
    }

    #[test]
    fn missing_mark_migrates() {
        let text = r#"{"strike":"1","bash":"2","aimed":"3","pin":"4","ember":"5","bind":"6","mend":"7","ward":"8","potion":"R"}"#;
        let keys: KeyBinds = serde_json::from_str(text).expect("old keys");
        assert_eq!(keys.get(Action::Mark), Some(Key::T));
        assert_eq!(keys.get(Action::SecondWind), Some(Key::G));
        assert!(missing_binds(&keys).is_empty());
    }
}
