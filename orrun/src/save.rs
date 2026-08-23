//! Where the player was standing when they last closed the game.
//!
//! FORMAT 3 also keeps combat vitals, bag, and coin. FORMAT 2 also keeps combat vitals and the last hatch-mouth shrine. FORMAT 1
//! (and any older stand that still has seed/size/x/z/yaw) still loads: combat
//! fields take create defaults and the next write is FORMAT 3. The continent
//! itself stays a pure function of the seed.

use std::fs;
use std::path::{Path, PathBuf};

use engine::place::GlobalPlace;
use engine::space::{GlobalPosition, GlobalXZ};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::combat::{Attrs, Ranks};
use crate::inventory::Inventory;
use crate::world::Heading;

/// Shape written on disk. Older stands that still have seed/size/x/z/yaw are
/// migrated in memory; the next write is this format. A newer format, or JSON
/// that is not a stand, is an error.
pub const FORMAT: u32 = 3;

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

    #[error("save {path} is format {found}, this build writes {FORMAT}")]
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

/// One remembered stand plus combat vitals, bag, and coin. FORMAT 3.
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
    pub inventory: Inventory,
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
            inventory: Inventory::create_kit(),
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

fn parse_stand(path: &Path, text: &str) -> Result<SavedStand, SaveError> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|source| SaveError::Unreadable {
            path: path.to_path_buf(),
            source,
        })?;
    let found = value
        .get("format")
        .and_then(serde_json::Value::as_u64)
        .map(|n| n as u32);
    if found == Some(FORMAT) {
        return serde_json::from_value(value).map_err(|source| SaveError::Unreadable {
            path: path.to_path_buf(),
            source,
        });
    }
    if let Some(found) = found {
        if found > FORMAT {
            return Err(SaveError::Format {
                path: path.to_path_buf(),
                found,
            });
        }
    }
    migrate_old_stand(&value).ok_or_else(|| {
        let source = serde_json::from_value::<SavedStand>(value)
            .expect_err("a stand missing seed/size/x/z/yaw cannot be FORMAT 2");
        SaveError::Unreadable {
            path: path.to_path_buf(),
            source,
        }
    })
}

/// FORMAT 1, or any older object that still has the stand pose.
fn migrate_old_stand(value: &serde_json::Value) -> Option<SavedStand> {
    let seed = i32::try_from(value.get("seed")?.as_i64()?).ok()?;
    let size = usize::try_from(value.get("size")?.as_u64()?).ok()?;
    let x = value.get("x")?.as_f64()?;
    let z = value.get("z")?.as_f64()?;
    let yaw_degrees = value.get("yaw_degrees")?.as_f64()? as f32;
    let heading = Heading::from_degrees(yaw_degrees).ok()?;
    let mut stand = SavedStand::new(seed, size, GlobalXZ::at(x, z), heading);
    if let Some(raw) = value.get("last_shrine") {
        if raw.is_null() {
            stand.last_shrine = None;
        } else if let Ok(shrine) = serde_json::from_value(raw.clone()) {
            stand.last_shrine = Some(shrine);
        }
    }
    Some(stand)
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
        assert_eq!(FORMAT, 3);
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
        assert_eq!(back.inventory, stand.inventory);
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

    const FORMAT_1_STAND: &str = r#"{
  "format": 1,
  "seed": 20260809,
  "size": 1000,
  "x": 373721.42083633796,
  "z": 484743.6172915158,
  "yaw_degrees": 61.441334
}"#;

    #[test]
    fn format_1_stand_migrates_to_create_defaults_and_writes_format_2() {
        let dir = isolated_dir();
        let path = dir.join("stand-20260809-1000.json");
        fs::write(&path, FORMAT_1_STAND).expect("write format 1");
        let stand = SavedStand::read_at(&path, 20260809, 1000)
            .expect("format 1 must load")
            .expect("present");
        let parsed: serde_json::Value = serde_json::from_str(FORMAT_1_STAND).expect("fixture");
        assert_eq!(stand.x, parsed["x"].as_f64().expect("x"));
        assert_eq!(stand.z, parsed["z"].as_f64().expect("z"));
        assert_eq!(
            stand.yaw_degrees,
            parsed["yaw_degrees"].as_f64().expect("yaw") as f32
        );
        assert_eq!(stand.seed, 20260809);
        assert_eq!(stand.size, 1000);
        assert_eq!(stand.format, FORMAT);
        assert_eq!(stand.level, 1);
        assert_eq!(stand.xp, 0);
        assert_eq!(stand.hp, 100.0);
        assert_eq!(stand.mana, 50.0);
        assert_eq!(stand.attrs, Attrs::default());
        assert_eq!(
            stand.ranks,
            Ranks {
                martial: 1,
                hunt: 0,
                arcane: 0,
            }
        );
        assert_eq!(stand.shaken_until, 0.0);
        assert_eq!(stand.last_shrine, None);

        let out = dir.join("migrated.json");
        stand.write_at(&out).expect("write migrated");
        let written: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&out).expect("read written"))
                .expect("written json");
        assert_eq!(written["format"], 3);
        assert_eq!(written["x"], parsed["x"]);
        assert_eq!(written["z"], parsed["z"]);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn older_stand_without_format_field_still_migrates() {
        let dir = isolated_dir();
        let path = dir.join("stand-7-64.json");
        fs::write(
            &path,
            r#"{"seed":7,"size":64,"x":1.5,"z":2.25,"yaw_degrees":90.0}"#,
        )
        .expect("write older stand");
        let stand = SavedStand::read_at(&path, 7, 64)
            .expect("older stand must load")
            .expect("present");
        assert_eq!(stand.x, 1.5);
        assert_eq!(stand.z, 2.25);
        assert_eq!(stand.yaw_degrees, 90.0);
        assert_eq!(stand.format, FORMAT);
        assert_eq!(stand.hp, 100.0);
        assert_eq!(stand.mana, 50.0);
        let _ = fs::remove_dir_all(dir);
    }

    const FORMAT_2_STAND: &str = r#"{
  "format": 2,
  "seed": 7,
  "size": 64,
  "x": 1.5,
  "z": 2.25,
  "yaw_degrees": 90.0,
  "level": 1,
  "xp": 0,
  "hp": 100.0,
  "mana": 50.0,
  "attrs": { "might": 10, "grace": 10, "mind": 10, "discipline": 10 },
  "ranks": { "martial": 1, "hunt": 0, "arcane": 0 },
  "shaken_until": 0.0,
  "last_shrine": null
}"#;

    #[test]
    fn format_2_stand_migrates_to_create_kit_bag() {
        let dir = isolated_dir();
        let path = dir.join("stand-7-64.json");
        fs::write(&path, FORMAT_2_STAND).expect("write format 2");
        let stand = SavedStand::read_at(&path, 7, 64)
            .expect("format 2 must load")
            .expect("present");
        assert_eq!(stand.format, FORMAT);
        assert_eq!(stand.inventory, Inventory::create_kit());
        assert_eq!(stand.inventory.coin, 0);
        assert_eq!(stand.inventory.melee.unwrap().name(), "Worn Blade");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn garbage_json_is_still_unreadable() {
        let dir = isolated_dir();
        let path = dir.join("stand-1-1.json");
        fs::write(&path, "{not json").expect("write garbage");
        let err = SavedStand::read_at(&path, 1, 1).expect_err("garbage fails");
        assert!(matches!(err, SaveError::Unreadable { .. }), "{err}");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn remembered_appdata_format_1_stand_keeps_pose() {
        let path = PathBuf::from(r"C:\Users\windo\AppData\Roaming\Orrun\stand-20260809-1000.json");
        if !path.is_file() {
            return;
        }
        let raw: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).expect("read remembered stand"))
                .expect("remembered stand json");
        let stand = SavedStand::read_at(&path, 20260809, 1000)
            .expect("remembered stand must be migratable")
            .expect("present");
        assert_eq!(stand.x, raw["x"].as_f64().expect("x"));
        assert_eq!(stand.z, raw["z"].as_f64().expect("z"));
        assert_eq!(
            stand.yaw_degrees,
            raw["yaw_degrees"].as_f64().expect("yaw") as f32
        );
    }
}
