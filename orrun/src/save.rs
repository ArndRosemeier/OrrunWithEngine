//! Canonical persisted player state. Format changes are intentionally incompatible.

use std::fs;
use std::path::{Path, PathBuf};

use engine::place::GlobalPlace;
use engine::space::{GlobalPosition, GlobalXZ};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::combat::PlayerSaveSnapshot;
use crate::inventory::Inventory;
use crate::world::Heading;

/// M5 skill-progression semantics. Formats 1-4 are incompatible and are not migrated.
pub const FORMAT: u32 = 5;

#[derive(Debug, Error)]
pub enum SaveError {
    #[error("no home directory to keep saves in (set APPDATA, XDG_DATA_HOME or HOME)")]
    NoHome,
    #[error("save {path} could not be {doing}")]
    Io {
        path: PathBuf,
        doing: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("save {path} is not readable Orrun state")]
    Unreadable {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("save {path} uses incompatible format {found}; this build requires format {FORMAT}")]
    IncompatibleFormat { path: PathBuf, found: String },
    #[error("save {path} holds seed {seed} size {size}, which is not the world being played")]
    OtherWorld {
        path: PathBuf,
        seed: i32,
        size: usize,
    },
    #[error("save {path} contains non-finite {field}")]
    NonFinite { path: PathBuf, field: &'static str },
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedShrine {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub yaw_degrees: f32,
}

impl SavedShrine {
    pub fn from_place(place: GlobalPlace) -> Self {
        Self {
            x: place.position.x,
            y: place.position.y,
            z: place.position.z,
            yaw_degrees: place.yaw_degrees,
        }
    }
    pub fn to_place(self) -> GlobalPlace {
        GlobalPlace::at(GlobalPosition::at(self.x, self.y, self.z)).with_yaw_deg(self.yaw_degrees)
    }
    fn validate(&self, path: &Path) -> Result<(), SaveError> {
        finite(path, "last_shrine.x", self.x)?;
        finite(path, "last_shrine.y", self.y)?;
        finite(path, "last_shrine.z", self.z)?;
        finite(path, "last_shrine.yaw_degrees", f64::from(self.yaw_degrees))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedStand {
    pub format: u32,
    pub seed: i32,
    pub size: usize,
    pub x: f64,
    pub z: f64,
    pub yaw_degrees: f32,
    pub player: PlayerSaveSnapshot,
    pub last_shrine: Option<SavedShrine>,
    pub inventory: Inventory,
}

impl SavedStand {
    pub fn new(
        seed: i32,
        size: usize,
        at: GlobalXZ,
        facing: Heading,
        player: PlayerSaveSnapshot,
        last_shrine: Option<SavedShrine>,
        inventory: Inventory,
    ) -> Self {
        Self {
            format: FORMAT,
            seed,
            size,
            x: at.x,
            z: at.z,
            yaw_degrees: facing.degrees(),
            player,
            last_shrine,
            inventory,
        }
    }
    pub fn at(&self) -> GlobalXZ {
        GlobalXZ::at(self.x, self.z)
    }
    pub fn read(seed: i32, size: usize) -> Result<Option<Self>, SaveError> {
        let path = path_for(seed, size)?;
        Self::read_at(&path, seed, size)
    }
    pub fn read_at(path: &Path, seed: i32, size: usize) -> Result<Option<Self>, SaveError> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(SaveError::Io {
                    path: path.to_path_buf(),
                    doing: "read",
                    source,
                })
            }
        };
        let stand = parse_stand(path, &text)?;
        if stand.seed != seed || stand.size != size {
            return Err(SaveError::OtherWorld {
                path: path.to_path_buf(),
                seed: stand.seed,
                size: stand.size,
            });
        }
        Ok(Some(stand))
    }
    pub fn write(&self) -> Result<PathBuf, SaveError> {
        let path = path_for(self.seed, self.size)?;
        self.write_at(&path)?;
        Ok(path)
    }
    pub fn write_at(&self, path: &Path) -> Result<(), SaveError> {
        self.validate(path)?;
        let dir = path.parent().expect("save path has a directory");
        fs::create_dir_all(dir).map_err(|source| SaveError::Io {
            path: dir.to_path_buf(),
            doing: "created",
            source,
        })?;
        let text = serde_json::to_string_pretty(self).expect("a validated stand always serializes");
        fs::write(path, text).map_err(|source| SaveError::Io {
            path: path.to_path_buf(),
            doing: "written",
            source,
        })
    }
    fn validate(&self, path: &Path) -> Result<(), SaveError> {
        finite(path, "x", self.x)?;
        finite(path, "z", self.z)?;
        finite(path, "yaw_degrees", f64::from(self.yaw_degrees))?;
        if let Some(shrine) = &self.last_shrine {
            shrine.validate(path)?;
        }
        Ok(())
    }
}

fn parse_stand(path: &Path, text: &str) -> Result<SavedStand, SaveError> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|source| SaveError::Unreadable {
            path: path.to_path_buf(),
            source,
        })?;
    let found = value.get("format").and_then(serde_json::Value::as_u64);
    if found != Some(u64::from(FORMAT)) {
        let found = found.map_or_else(|| "missing".to_owned(), |n| n.to_string());
        return Err(SaveError::IncompatibleFormat {
            path: path.to_path_buf(),
            found,
        });
    }
    let stand: SavedStand =
        serde_json::from_value(value).map_err(|source| SaveError::Unreadable {
            path: path.to_path_buf(),
            source,
        })?;
    stand.validate(path)?;
    Ok(stand)
}

fn finite(path: &Path, field: &'static str, value: f64) -> Result<(), SaveError> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(SaveError::NonFinite {
            path: path.to_path_buf(),
            field,
        })
    }
}

pub fn path_for(seed: i32, size: usize) -> Result<PathBuf, SaveError> {
    Ok(save_dir()?.join(format!("stand-{seed}-{size}.json")))
}
pub fn data_dir() -> Result<PathBuf, SaveError> {
    save_dir()
}
fn save_dir() -> Result<PathBuf, SaveError> {
    if let Some(dir) = std::env::var_os("ORRUN_SAVE_DIR") {
        return Ok(PathBuf::from(dir));
    }
    if let Some(dir) = std::env::var_os("APPDATA") {
        return Ok(Path::new(&dir).join("Orrun"));
    }
    if let Some(dir) = std::env::var_os("XDG_DATA_HOME") {
        return Ok(Path::new(&dir).join("orrun"));
    }
    if let Some(dir) = std::env::var_os("HOME") {
        return Ok(Path::new(&dir).join(".local/share/orrun"));
    }
    Err(SaveError::NoHome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::progression::{
        ActorProgressionSnapshot, ProgressionTrackSnapshot, SkillProgressionSnapshot,
    };

    fn player() -> PlayerSaveSnapshot {
        PlayerSaveSnapshot::new(
            ActorProgressionSnapshot::new(
                vec![SkillProgressionSnapshot::new(
                    "slash",
                    ProgressionTrackSnapshot::new(3, 17),
                )],
                ProgressionTrackSnapshot::new(2, 9),
                ProgressionTrackSnapshot::new(4, 22),
            ),
            77.5,
            23.25,
        )
    }
    fn stand() -> SavedStand {
        SavedStand::new(
            7,
            64,
            GlobalXZ::at(1.5, -2.25),
            Heading::from_degrees(91.0).unwrap(),
            player(),
            Some(SavedShrine {
                x: 4.0,
                y: 5.0,
                z: 6.0,
                yaw_degrees: 12.0,
            }),
            Inventory::create_kit(),
        )
    }
    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("orrun-save-{}-{name}.json", std::process::id()))
    }

    #[test]
    fn canonical_round_trip_is_exact() {
        let path = temp("roundtrip");
        let expected = stand();
        expected.write_at(&path).unwrap();
        let actual = SavedStand::read_at(&path, 7, 64).unwrap().unwrap();
        assert_eq!(actual, expected);
        let _ = fs::remove_file(path);
    }
    #[test]
    fn rejects_legacy_missing_and_future_formats() {
        for (name, json) in [
            ("one", r#"{"format":1}"#),
            ("two", r#"{"format":2}"#),
            ("three", r#"{"format":3}"#),
            ("four", r#"{"format":4}"#),
            ("missing", r#"{}"#),
            ("future", r#"{"format":6}"#),
        ] {
            let path = temp(name);
            fs::write(&path, json).unwrap();
            assert!(matches!(
                SavedStand::read_at(&path, 7, 64),
                Err(SaveError::IncompatibleFormat { .. })
            ));
            let _ = fs::remove_file(path);
        }
    }
    #[test]
    fn rejects_structurally_readable_format_four_before_progression_restore() {
        let mut value = serde_json::to_value(stand()).unwrap();
        value["format"] = 4.into();
        value["player"]["progression"]["skills"][0]["skill_id"] = "fire_damage".into();
        let path = temp("format-four-m5-incompatible");
        fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();

        assert!(matches!(
            SavedStand::read_at(&path, 7, 64),
            Err(SaveError::IncompatibleFormat { found, .. }) if found == "4"
        ));
        let _ = fs::remove_file(path);
    }
    #[test]
    fn rejects_unknown_fields_and_non_finite_pose() {
        let mut value = serde_json::to_value(stand()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("level".into(), 1.into());
        let path = temp("unknown");
        fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();
        assert!(matches!(
            SavedStand::read_at(&path, 7, 64),
            Err(SaveError::Unreadable { .. })
        ));
        let mut bad = stand();
        bad.x = f64::NAN;
        assert!(matches!(
            bad.write_at(&path),
            Err(SaveError::NonFinite { .. })
        ));
        let _ = fs::remove_file(path);
    }
    #[test]
    fn canonical_json_has_no_legacy_progression_fields() {
        let value = serde_json::to_value(stand()).unwrap();
        for field in [
            "level",
            "xp",
            "attrs",
            "ranks",
            "shaken_until",
            "hp",
            "mana",
        ] {
            assert!(value.get(field).is_none(), "legacy field {field}");
        }
        assert_eq!(value["inventory"]["coin"], 0);
    }
}
