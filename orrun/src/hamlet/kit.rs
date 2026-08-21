//! Medieval kit mesh loading and the overland woods hut.
//!
//! Village dwellings are generated in [`super::house_gen`]. Door leaves
//! (`door_plank` / `door_sturdy`) are not catalog pieces: they hang on the
//! opening after the ring closes.

use std::path::{Path, PathBuf};

use engine::error::EngineError;
use engine::mesh::Mesh;
use engine::model::Model;
use engine::place::Place;
use modular::prelude::*;
use thiserror::Error;

const MEDIEVAL_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../Modular/catalogs/medieval.json"
));

/// Piece id → vendored / generated glb file name.
pub const PIECE_GLBS: &[(&str, &str)] = &[
    ("wall", "med_wall.glb"),
    ("wall_b", "med_wall_b.glb"),
    ("window", "med_window.glb"),
    ("window_b", "med_window_b.glb"),
    ("window_c", "med_window_c.glb"),
    ("wall_limewash", "med_wall_limewash.glb"),
    ("wall_blind", "med_wall_blind.glb"),
    ("wall_halftimber", "med_wall_halftimber.glb"),
    ("door", "med_door.glb"),
    ("door_b", "med_door_b.glb"),
    ("corner", "med_corner.glb"),
    ("wall_jetty", "med_wall_jetty.glb"),
    ("wall_b_jetty", "med_wall_b_jetty.glb"),
    ("window_jetty", "med_window_jetty.glb"),
    ("window_b_jetty", "med_window_b_jetty.glb"),
    ("window_c_jetty", "med_window_c_jetty.glb"),
    ("corner_jetty", "med_corner_jetty.glb"),
    ("roof", "med_roof.glb"),
    ("roof_b", "med_roof_b.glb"),
    ("roof_thatch_hip", "med_roof_thatch_hip.glb"),
    ("roof_tile_steep", "med_roof_tile_steep.glb"),
    ("chimney", "med_chimney.glb"),
    ("smoke_gablet", "med_smoke_gablet.glb"),
    ("floor", "med_floor.glb"),
    ("plinth", "med_plinth.glb"),
    ("plinth_b", "med_plinth_b.glb"),
    ("door_plank", "door_plank.glb"),
    ("door_sturdy", "door_sturdy.glb"),
];

/// `med_door` / `med_door_b` opening and `door_plank` / `door_sturdy` leaf.
const WALL_THICKNESS: f32 = 0.28;
const OVERLAP: f32 = 0.02;
const JAMB_PROUD: f32 = 0.02;
const HINGE_INSET: f32 = 0.01;
const THRESHOLD_TOP: f32 = OVERLAP + 0.012 + 0.015 + 0.08;

#[derive(Debug, Error)]
pub enum KitError {
    #[error(transparent)]
    Modular(#[from] ModularError),

    #[error(
        "kit piece {piece} not found (tried {tried}). From C:\\Projekte\\AssetGenerator run: python tools/ag.py generate {stem} then python tools/sync_props.py"
    )]
    MissingPiece {
        piece: String,
        stem: String,
        tried: String,
    },

    #[error("kit piece {path} failed to load: {source}")]
    BadPiece {
        path: PathBuf,
        #[source]
        source: EngineError,
    },
}

fn pid(id: &str) -> PieceId {
    PieceId::new(id).unwrap_or_else(|err| panic!("{err}"))
}

/// Load the medieval kit catalog shipped next to this crate.
pub fn catalog() -> Catalog {
    Catalog::from_json(MEDIEVAL_JSON).expect("Modular catalogs/medieval.json")
}

pub fn glb_name(piece: &PieceId) -> &'static str {
    PIECE_GLBS
        .iter()
        .find(|(id, _)| *id == piece.as_str())
        .map(|(_, file)| *file)
        .unwrap_or_else(|| panic!("no glb mapping for kit piece {piece}"))
}

fn kit_search_paths(file: &str) -> Vec<PathBuf> {
    let mut tried = Vec::new();
    if let Some(dir) = std::env::var_os("ORRUN_ASSETS") {
        tried.push(PathBuf::from(dir).join("kit").join("medieval").join(file));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            tried.push(dir.join("assets").join("kit").join("medieval").join(file));
        }
    }
    tried.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join("kit")
            .join("medieval")
            .join(file),
    );
    tried
}

pub fn load_piece_mesh(piece: &PieceId) -> Result<Mesh, KitError> {
    let file = glb_name(piece);
    let tried = kit_search_paths(file);
    let path = tried.iter().find(|p| p.is_file()).cloned();
    let Some(path) = path else {
        return Err(KitError::MissingPiece {
            piece: piece.to_string(),
            stem: Path::new(file)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| file.to_string()),
            tried: tried
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", "),
        });
    };
    let base = path.parent().unwrap_or_else(|| Path::new("."));
    Model::load_with(&path, base, &engine::EngineLimits::default())
        .map_err(|source| KitError::BadPiece { path, source })
}

/// Rotate a local XZ offset by house yaw (engine Y-up, matching `Place::to_matrix`).
pub fn yaw_xz(x: f32, z: f32, yaw_deg: f32) -> (f32, f32) {
    let rad = yaw_deg.to_radians();
    let (sin, cos) = (rad.sin(), rad.cos());
    (x * cos + z * sin, -x * sin + z * cos)
}

fn footprint_origin(catalog: &Catalog, assembly: &Assembly<'_>) -> glam::Vec3 {
    catalog
        .pitch()
        .mesh_origin(&assembly.occupied_cells())
        .unwrap_or_else(|| panic!("assembly occupancy is empty"))
}

fn shift_xz(places: &mut [PlacedMesh], origin: glam::Vec3) {
    for item in places {
        item.place.position.x -= origin.x;
        item.place.position.z -= origin.z;
    }
}

pub(crate) fn centre_footprint(
    catalog: &Catalog,
    assembly: &Assembly<'_>,
) -> ModularResult<Vec<PlacedMesh>> {
    let origin = footprint_origin(catalog, assembly);
    let mut places = assembly.places()?;
    shift_xz(&mut places, origin);
    Ok(places)
}

fn door_opening_width(piece: &str) -> f32 {
    match piece {
        "door" => 1.1,
        "door_b" => 1.05,
        other => panic!("door family piece '{other}' has no opening width"),
    }
}

/// One-cell woods hut for overland sites only. Not a village dwelling.
pub fn assemble_woods_hut() -> Vec<PlacedMesh> {
    const STOREY: f32 = 2.7;
    const HALF: f32 = 2.0;
    let at = |piece: &str, y: f32, yaw: f32| PlacedMesh {
        piece: pid(piece),
        place: Place::new(0.0, y, 0.0).with_yaw_deg(yaw),
    };
    vec![
        at("door", 0.0, 0.0),
        at("wall_halftimber", 0.0, 90.0),
        at("wall_blind", 0.0, 180.0),
        at("wall_halftimber", 0.0, 270.0),
        at("plinth", -STOREY, 0.0),
        at("plinth", -STOREY, 90.0),
        at("plinth", -STOREY, 180.0),
        at("plinth", -STOREY, 270.0),
        at("roof_thatch_hip", STOREY, 0.0),
        PlacedMesh {
            piece: pid("door_plank"),
            place: Place::new(
                door_opening_width("door") * 0.5 - JAMB_PROUD - HINGE_INSET,
                THRESHOLD_TOP,
                -(HALF - WALL_THICKNESS * 0.5),
            )
            .with_yaw_deg(180.0),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn woods_hut_is_a_small_thatch_cell_not_a_village_box() {
        let places = assemble_woods_hut();
        let names: Vec<_> = places.iter().map(|p| p.piece.as_str()).collect();
        assert!(names.contains(&"roof_thatch_hip"));
        assert!(names.contains(&"wall_halftimber"));
        assert!(names.contains(&"wall_blind"));
        assert!(names.contains(&"door_plank"));
    }

    #[test]
    fn every_piece_glb_is_mapped() {
        let catalog = catalog();
        for (id, _) in PIECE_GLBS {
            if matches!(*id, "door_plank" | "door_sturdy") {
                continue;
            }
            let piece = PieceId::new(*id).unwrap();
            let _ = catalog.piece(&piece).unwrap_or_else(|err| panic!("{err}"));
        }
    }
}
