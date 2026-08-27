//! Player preferences that outlive a session.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::atlas::SIZE as MAX_CONTINENT_SIZE;
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
    /// Present uncapped. Off is the default: the game targets headroom and
    /// most desktop compositors already throttle uncapped frames.
    #[serde(default)]
    pub vsync: bool,
    /// Combat verb binds. Missing map (old FORMAT 1 files) gets defaults.
    #[serde(default)]
    pub keys: KeyBinds,
}

pub use crate::controls::KeyBinds;

fn default_continent_size() -> usize {
    DEFAULT_CONTINENT_SIZE
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            format: FORMAT,
            hitch_log: false,
            continent_size: DEFAULT_CONTINENT_SIZE,
            vsync: false,
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
            vsync: true,
            keys: KeyBinds::default(),
        };
        let text = serde_json::to_string(&settings).expect("write");
        let back: Settings = serde_json::from_str(&text).expect("read");
        assert_eq!(settings, back);
    }

    #[test]
    fn vsync_defaults_off_for_old_settings_files() {
        let text = r#"{"format":1,"hitch_log":false}"#;
        let settings: Settings = serde_json::from_str(text).expect("read old settings");
        assert!(!settings.vsync, "vsync must default to off");
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
}
