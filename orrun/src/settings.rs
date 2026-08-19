//! Player preferences that outlive a session.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use engine::Key;
use serde_json::Value;

use crate::atlas::SIZE as MAX_CONTINENT_SIZE;
use crate::combat::CombatVerb;
use crate::save::{self, SaveError};

pub const FORMAT: u32 = 1;
pub const DEFAULT_CONTINENT_SIZE: usize = 256;
pub const MIN_CONTINENT_SIZE: usize = 32;

/// Clamp a requested atlas edge length to supported bounds.
pub fn clamp_continent_size(size: usize) -> usize {
    size.clamp(MIN_CONTINENT_SIZE, MAX_CONTINENT_SIZE)
}

#[derive(Debug, Error)]
pub enum SettingsError {
    #[error(transparent)]
    Save(#[from] SaveError),

    #[error("settings {path} could not be {doing}")]
    Io {
        path: PathBuf,
        doing: &'static str,
        #[source]
        source: std::io::Error,
    },

    #[error("settings {path} is not readable Orrun state")]
    Unreadable {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error(
        "settings {path} is format {found}, this build writes {FORMAT}; delete it to start over"
    )]
    Format { path: PathBuf, found: u32 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub format: u32,
    pub hitch_log: bool,
    #[serde(default = "default_continent_size")]
    pub continent_size: usize,
    /// Combat verb binds. Missing map (old FORMAT 1 files) gets defaults.
    #[serde(default)]
    pub keys: KeyBinds,
}

/// Action → key. `null` is unbound. Last assign wins on conflict.
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
        }
    }
}

impl KeyBinds {
    fn slot(&self, verb: CombatVerb) -> Option<&str> {
        match verb {
            CombatVerb::Strike => self.strike.as_deref(),
            CombatVerb::Bash => self.bash.as_deref(),
            CombatVerb::AimedShot => self.aimed.as_deref(),
            CombatVerb::Pin => self.pin.as_deref(),
            CombatVerb::Ember => self.ember.as_deref(),
            CombatVerb::Bind => self.bind.as_deref(),
            CombatVerb::Mend => self.mend.as_deref(),
            CombatVerb::Ward => self.ward.as_deref(),
            CombatVerb::Potion => self.potion.as_deref(),
        }
    }

    fn slot_mut(&mut self, verb: CombatVerb) -> &mut Option<String> {
        match verb {
            CombatVerb::Strike => &mut self.strike,
            CombatVerb::Bash => &mut self.bash,
            CombatVerb::AimedShot => &mut self.aimed,
            CombatVerb::Pin => &mut self.pin,
            CombatVerb::Ember => &mut self.ember,
            CombatVerb::Bind => &mut self.bind,
            CombatVerb::Mend => &mut self.mend,
            CombatVerb::Ward => &mut self.ward,
            CombatVerb::Potion => &mut self.potion,
        }
    }

    pub fn get(&self, verb: CombatVerb) -> Option<Key> {
        self.slot(verb).and_then(Key::from_name)
    }

    pub fn display(&self, verb: CombatVerb) -> String {
        match self.slot(verb) {
            Some(name) if !name.is_empty() => name.to_string(),
            _ => "unbound".into(),
        }
    }

    pub fn verb_for(&self, key: Key) -> Option<CombatVerb> {
        let name = key.as_str();
        for verb in CombatVerb::ALL {
            if self.slot(verb) == Some(name) {
                return Some(verb);
            }
        }
        None
    }

    pub fn verb_pressed(&self, input: &engine::Input) -> Option<CombatVerb> {
        let mut found = None;
        for verb in CombatVerb::ALL {
            if let Some(key) = self.get(verb) {
                if input.pressed(key) {
                    found = Some(verb);
                }
            }
        }
        found
    }

    /// Last assign wins: the previous owner of `key` becomes unbound.
    pub fn assign(&mut self, verb: CombatVerb, key: Key) {
        let name = key.as_str().to_string();
        for other in CombatVerb::ALL {
            if other != verb && self.slot(other) == Some(name.as_str()) {
                *self.slot_mut(other) = None;
            }
        }
        *self.slot_mut(verb) = Some(name);
    }

    pub fn missing(&self) -> Vec<CombatVerb> {
        CombatVerb::ALL
            .into_iter()
            .filter(|v| self.get(*v).is_none())
            .collect()
    }

    pub fn inspect_map(&self) -> serde_json::Map<String, Value> {
        let mut m = serde_json::Map::new();
        for verb in CombatVerb::ALL {
            let v = match self.slot(verb) {
                Some(name) => Value::String(name.to_string()),
                None => Value::Null,
            };
            m.insert(verb.id().into(), v);
        }
        m
    }
}

fn default_continent_size() -> usize {
    DEFAULT_CONTINENT_SIZE
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            format: FORMAT,
            hitch_log: false,
            continent_size: DEFAULT_CONTINENT_SIZE,
            keys: KeyBinds::default(),
        }
    }
}

impl Settings {
    pub fn continent_size(&self) -> usize {
        clamp_continent_size(self.continent_size)
    }

    pub fn load() -> Result<Self, SettingsError> {
        let path = settings_path()?;
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(source) => {
                return Err(SettingsError::Io {
                    path,
                    doing: "read",
                    source,
                });
            }
        };
        let settings: Self =
            serde_json::from_str(&text).map_err(|source| SettingsError::Unreadable {
                path: path.clone(),
                source,
            })?;
        if settings.format != FORMAT {
            return Err(SettingsError::Format {
                path,
                found: settings.format,
            });
        }
        Ok(settings)
    }

    pub fn write(&self) -> Result<PathBuf, SettingsError> {
        let path = settings_path()?;
        let dir = path.parent().expect("settings path has a directory");
        fs::create_dir_all(dir).map_err(|source| SettingsError::Io {
            path: dir.to_path_buf(),
            doing: "created",
            source,
        })?;
        let text = serde_json::to_string_pretty(self).expect("settings always serialise");
        fs::write(&path, text).map_err(|source| SettingsError::Io {
            path: path.clone(),
            doing: "written",
            source,
        })?;
        Ok(path)
    }
}

pub fn settings_path() -> Result<PathBuf, SettingsError> {
    Ok(save::data_dir()?.join("settings.json"))
}

pub fn hitch_log_path() -> Result<PathBuf, SettingsError> {
    Ok(save::data_dir()?.join("hitch.log"))
}

/// Delete any previous hitch log so the next session starts a clean file.
pub fn begin_hitch_log(path: &Path) -> Result<(), SettingsError> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|source| SettingsError::Io {
            path: dir.to_path_buf(),
            doing: "created",
            source,
        })?;
    }
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(SettingsError::Io {
            path: path.to_path_buf(),
            doing: "removed",
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turning_the_hitch_log_on_replaces_the_old_file() {
        let dir = std::env::temp_dir().join(format!("orrun-hitch-test-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("hitch.log");
        fs::write(&path, "old hitch").expect("seed");
        begin_hitch_log(&path).expect("replace");
        assert!(
            !path.exists(),
            "the previous hitch log must be gone before the first write"
        );
        fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn settings_round_trip() {
        let settings = Settings {
            format: FORMAT,
            hitch_log: true,
            continent_size: 512,
            keys: KeyBinds::default(),
        };
        let text = serde_json::to_string(&settings).expect("write");
        let back: Settings = serde_json::from_str(&text).expect("read");
        assert_eq!(settings, back);
    }

    #[test]
    fn removed_instance_submit_preference_is_ignored() {
        let text = r#"{"format":1,"hitch_log":false,"instance_submit":"cpu_indexed"}"#;
        let settings: Settings = serde_json::from_str(text).expect("read old settings");
        assert_eq!(settings, Settings::default());
    }

    #[test]
    fn missing_continent_size_defaults_to_256() {
        let text = r#"{"format":1,"hitch_log":false}"#;
        let settings: Settings = serde_json::from_str(text).expect("read old settings");
        assert_eq!(settings.continent_size(), DEFAULT_CONTINENT_SIZE);
    }

    #[test]
    fn continent_size_clamps_to_atlas_max() {
        assert_eq!(clamp_continent_size(10_000), MAX_CONTINENT_SIZE);
    }

    #[test]
    fn missing_keys_map_migrates_to_defaults() {
        let text = r#"{"format":1,"hitch_log":false,"continent_size":256}"#;
        let settings: Settings = serde_json::from_str(text).expect("read old settings");
        assert_eq!(settings.keys, KeyBinds::default());
        assert_eq!(settings.keys.get(CombatVerb::Strike), Some(Key::Digit1));
        assert_eq!(settings.keys.get(CombatVerb::Ember), Some(Key::Digit5));
        assert_eq!(settings.keys.get(CombatVerb::Potion), Some(Key::R));
        assert!(settings.keys.missing().is_empty());
    }

    #[test]
    fn last_assign_unbinds_the_previous_owner() {
        let mut keys = KeyBinds::default();
        keys.assign(CombatVerb::Strike, Key::Digit5);
        assert_eq!(keys.get(CombatVerb::Strike), Some(Key::Digit5));
        assert_eq!(keys.get(CombatVerb::Ember), None);
        assert_eq!(keys.display(CombatVerb::Ember), "unbound");
    }
}
