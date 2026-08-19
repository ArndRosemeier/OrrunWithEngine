//! Where the player was standing when they last closed the game.
//!
//! FORMAT 2 also keeps combat vitals and the last hatch-mouth shrine. The
//! continent itself stays a pure function of the seed.

use std::fs;
use std::path::{Path, PathBuf};

use engine::place::GlobalPlace;
use engine::space::{GlobalPosition, GlobalXZ};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::combat::{Attrs, Ranks};
use crate::world::Heading;

/// Shape of the file on disk. A file written by another shape is an error, not
/// a guess at what its fields used to mean.
pub const FORMAT: u32 = 2;

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

    #[error("save {path} is format {found}, this build writes {FORMAT}; delete it to start over")]
    Format { path: PathBuf, found: u32 },

    #[error("save {path} holds seed {seed} size {size}, which is not the world being played")]
    OtherWorld {
        path: PathBuf,
        seed: i32,
        size: usize,
    },
}

/// Last hatch mouth. Death returns here; no extra shrine mesh.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
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
}

/// One remembered stand plus combat vitals. FORMAT 2.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedStand {
    pub format: u32,
    pub seed: i32,
    pub size: usize,
    pub x: f64,
    pub z: f64,
    pub yaw_degrees: f32,
    pub level: i32,
    pub xp: i32,
    pub hp: f64,
    pub mana: f64,
    pub attrs: Attrs,
    pub ranks: Ranks,
    pub shaken_until: f64,
    pub last_shrine: Option<SavedShrine>,
}

impl SavedStand {
    pub fn new(seed: i32, size: usize, at: GlobalXZ, facing: Heading) -> Self {
        Self {
            format: FORMAT,
            seed,
            size,
            x: at.x,
            z: at.z,
            yaw_degrees: facing.degrees(),
            level: 1,
            xp: 0,
            hp: 100.0,
            mana: 50.0,
            attrs: Attrs::default(),
            ranks: Ranks {
                martial: 1,
                hunt: 0,
                arcane: 0,
            },
            shaken_until: 0.0,
            last_shrine: None,
        }
    }

    pub fn at(&self) -> GlobalXZ {
        GlobalXZ::at(self.x, self.z)
    }

    /// The stand kept for this world, if there is one.
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
        let stand: Self = serde_json::from_str(&text).map_err(|source| SaveError::Unreadable {
            path: path.to_path_buf(),
            source,
        })?;
        if stand.format != FORMAT {
            return Err(SaveError::Format {
                path: path.to_path_buf(),
                found: stand.format,
            });
        }
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
        let dir = path.parent().expect("save path has a directory");
        fs::create_dir_all(dir).map_err(|source| SaveError::Io {
            path: dir.to_path_buf(),
            doing: "created",
            source,
        })?;
        let text = serde_json::to_string_pretty(self).expect("a stand always serialises");
        fs::write(path, text).map_err(|source| SaveError::Io {
            path: path.to_path_buf(),
            doing: "written",
            source,
        })?;
        Ok(())
    }
}

/// One file per world, so switching seeds does not forget where you were.
pub fn path_for(seed: i32, size: usize) -> Result<PathBuf, SaveError> {
    Ok(save_dir()?.join(format!("stand-{seed}-{size}.json")))
}

/// `%APPDATA%/Orrun` on Windows, XDG or `~/.local/share/orrun` elsewhere.
pub fn data_dir() -> Result<PathBuf, SaveError> {
    save_dir()
}

fn save_dir() -> Result<PathBuf, SaveError> {
    if let Some(override_dir) = std::env::var_os("ORRUN_SAVE_DIR") {
        return Ok(PathBuf::from(override_dir));
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        return Ok(Path::new(&appdata).join("Orrun"));
    }
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        return Ok(Path::new(&xdg).join("orrun"));
    }
    if let Some(home) = std::env::var_os("HOME") {
        return Ok(Path::new(&home).join(".local/share/orrun"));
    }
    Err(SaveError::NoHome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::space::GlobalXZ;

    fn isolated_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "orrun-save-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        fs::create_dir_all(&dir).expect("isolated save dir");
        dir
    }

    fn isolate_appdata() -> PathBuf {
        let dir = isolated_dir();
        std::env::set_var("APPDATA", &dir);
        std::env::remove_var("ORRUN_SAVE_DIR");
        dir
    }

    #[test]
    fn format_is_versioned() {
        assert_eq!(FORMAT, 2);
    }

    #[test]
    fn a_stand_survives_the_round_trip() {
        let stand = SavedStand::new(
            20260809,
            256,
            GlobalXZ::at(123456.5, -789.25),
            Heading::from_degrees(271.5).expect("heading"),
        );
        let text = serde_json::to_string(&stand).expect("write");
        let back: SavedStand = serde_json::from_str(&text).expect("read");
        assert_eq!(stand, back);
    }

    #[test]
    fn combat_vitals_and_shrine_round_trip_without_appdata() {
        let dir = isolated_dir();
        let path = dir.join("stand-1-64.json");
        let mut stand = SavedStand::new(
            1,
            64,
            GlobalXZ::at(10.0, 20.0),
            Heading::from_degrees(90.0).expect("heading"),
        );
        stand.level = 3;
        stand.xp = 40;
        stand.hp = 88.0;
        stand.mana = 41.0;
        stand.attrs.might = 14;
        stand.ranks.martial = 3;
        stand.shaken_until = 12.5;
        stand.last_shrine = Some(SavedShrine {
            x: 1.0,
            y: 2.0,
            z: 3.0,
            yaw_degrees: 45.0,
        });
        stand.write_at(&path).expect("write isolated");
        let back = SavedStand::read_at(&path, 1, 64)
            .expect("read isolated")
            .expect("present");
        assert_eq!(back.level, 3);
        assert_eq!(back.xp, 40);
        assert_eq!(back.hp, 88.0);
        assert_eq!(back.mana, 41.0);
        assert_eq!(back.attrs.might, 14);
        assert_eq!(back.ranks.martial, 3);
        assert_eq!(back.shaken_until, 12.5);
        assert_eq!(back.last_shrine.unwrap().x, 1.0);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn write_goes_to_isolated_appdata_not_real() {
        let dir = isolate_appdata();
        let stand = SavedStand::new(
            9,
            32,
            GlobalXZ::at(1.0, 2.0),
            Heading::from_degrees(0.0).expect("heading"),
        );
        let path = stand.write().expect("write via APPDATA");
        assert!(path.starts_with(&dir), "{}", path.display());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn each_world_keeps_its_own_stand() {
        let dir = isolate_appdata();
        std::env::set_var("ORRUN_SAVE_DIR", &dir);
        let a = path_for(1, 256).expect("first path");
        let b = path_for(2, 256).expect("second path");
        let c = path_for(1, 512).expect("third path");
        std::env::remove_var("ORRUN_SAVE_DIR");
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert!(a.starts_with(&dir));
        let _ = fs::remove_dir_all(dir);
    }
}
