//! Where the player was standing when they last closed the game.
//!
//! Only the stand is kept — which world, where in it, and which way they were
//! facing. Everything else about the continent is a pure function of the seed,
//! so there is nothing else that could disagree with itself after a reload.
//!
//! The height is deliberately not saved: re-entry resolves the ground under
//! the spot the same way a fresh entry does, so a player cannot come back
//! inside a hill that generated a hand's width differently.

use std::fs;
use std::path::{Path, PathBuf};

use engine::space::GlobalXZ;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::world::Heading;

/// Shape of the file on disk. A file written by another shape is an error, not
/// a guess at what its fields used to mean.
pub const FORMAT: u32 = 1;

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

/// One remembered stand.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedStand {
    pub format: u32,
    pub seed: i32,
    pub size: usize,
    pub x: f64,
    pub z: f64,
    pub yaw_degrees: f32,
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
        }
    }

    pub fn at(&self) -> GlobalXZ {
        GlobalXZ::at(self.x, self.z)
    }

    /// The stand kept for this world, if there is one.
    pub fn read(seed: i32, size: usize) -> Result<Option<Self>, SaveError> {
        let path = path_for(seed, size)?;
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(SaveError::Io {
                    path,
                    doing: "read",
                    source,
                })
            }
        };
        let stand: Self = serde_json::from_str(&text).map_err(|source| SaveError::Unreadable {
            path: path.clone(),
            source,
        })?;
        if stand.format != FORMAT {
            return Err(SaveError::Format {
                path,
                found: stand.format,
            });
        }
        if stand.seed != seed || stand.size != size {
            return Err(SaveError::OtherWorld {
                path,
                seed: stand.seed,
                size: stand.size,
            });
        }
        Ok(Some(stand))
    }

    pub fn write(&self) -> Result<PathBuf, SaveError> {
        let path = path_for(self.seed, self.size)?;
        let dir = path.parent().expect("save path has a directory");
        fs::create_dir_all(dir).map_err(|source| SaveError::Io {
            path: dir.to_path_buf(),
            doing: "created",
            source,
        })?;
        let text = serde_json::to_string_pretty(self).expect("a stand always serialises");
        fs::write(&path, text).map_err(|source| SaveError::Io {
            path: path.clone(),
            doing: "written",
            source,
        })?;
        Ok(path)
    }
}

/// One file per world, so switching seeds does not forget where you were.
pub fn path_for(seed: i32, size: usize) -> Result<PathBuf, SaveError> {
    Ok(save_dir()?.join(format!("stand-{seed}-{size}.json")))
}

fn save_dir() -> Result<PathBuf, SaveError> {
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
    fn each_world_keeps_its_own_stand() {
        // Playing another seed must not overwrite where you were in this one.
        let Ok(a) = path_for(1, 256) else {
            return;
        };
        let b = path_for(2, 256).expect("second path");
        let c = path_for(1, 512).expect("third path");
        assert_ne!(a, b);
        assert_ne!(a, c);
    }
}
