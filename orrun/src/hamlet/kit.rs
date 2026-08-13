//! Medieval house recipes for hamlets. Kit vocabulary stays out of `modular`.
//!
//! Pieces come from Asset Lab `med_*` products. Catalog JSON is the Modular
//! medieval kit. Door wall is the south (−Z) long side, matching planner yaw 0.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use engine::error::EngineError;
use engine::mesh::Mesh;
use engine::model::Model;
#[cfg(test)]
use engine::place::Place;
#[cfg(test)]
use glam::Vec3;
use modular::prelude::*;
use thiserror::Error;

const MEDIEVAL_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../Modular/catalogs/medieval.json"
));

/// Piece id → vendored / generated glb file name.
pub const PIECE_GLBS: &[(&str, &str)] = &[
    ("wall", "med_wall.glb"),
    ("window", "med_window.glb"),
    ("window_b", "med_window_b.glb"),
    ("door", "med_door.glb"),
    ("corner", "med_corner.glb"),
    ("wall_jetty", "med_wall_jetty.glb"),
    ("window_jetty", "med_window_jetty.glb"),
    ("window_b_jetty", "med_window_b_jetty.glb"),
    ("corner_jetty", "med_corner_jetty.glb"),
    ("roof", "med_roof.glb"),
    ("chimney", "med_chimney.glb"),
    ("plinth", "med_plinth.glb"),
];

const DWELLING_IDS: &[&str] = &[
    "house_hut_thatch",
    "house_cabin_timber",
    "house_cottage_stone",
    "house_hall_large",
];

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

fn did(id: &str) -> DockId {
    DockId::new(id).unwrap_or_else(|err| panic!("{err}"))
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
        .unwrap_or_else(|| panic!("medieval kit has no glb mapping for piece {piece}"))
}

fn projekte_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
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
    tried.push(
        projekte_root()
            .join("AssetGenerator")
            .join("assets")
            .join("out")
            .join(file),
    );
    tried
}

/// Load one kit piece mesh. Vendored copy wins; Asset Lab output is the fallback.
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

/// Closed, footprint-centred places for every dwelling catalog id.
pub fn dwelling_recipes(catalog: &Catalog) -> Result<HashMap<String, Vec<PlacedMesh>>, KitError> {
    let mut out = HashMap::new();
    for id in DWELLING_IDS {
        out.insert((*id).to_string(), assemble_dwelling(catalog, id)?);
    }
    Ok(out)
}

pub fn assemble_dwelling(catalog: &Catalog, catalog_id: &str) -> Result<Vec<PlacedMesh>, KitError> {
    let assembly = match catalog_id {
        "house_hut_thatch" => assemble_hut(catalog)?,
        "house_cabin_timber" => assemble_cabin(catalog)?,
        "house_cottage_stone" => assemble_townhouse(catalog)?,
        "house_hall_large" => assemble_hall(catalog)?,
        other => panic!("'{other}' is not a modular dwelling"),
    };
    let open = assembly.unmated_seams()?;
    if !open.is_empty() {
        panic!("{catalog_id} has free seams (kit bug): {open:?}");
    }
    Ok(centre_footprint(catalog, &assembly)?)
}

/// Rotate a local XZ offset by house yaw (engine Y-up, matching `Place::to_matrix`).
pub fn yaw_xz(x: f32, z: f32, yaw_deg: f32) -> (f32, f32) {
    let rad = yaw_deg.to_radians();
    let (sin, cos) = (rad.sin(), rad.cos());
    (x * cos + z * sin, -x * sin + z * cos)
}

/// Rotate `local` (footprint centre at xz origin, floor at y=0) into the house pose.
#[cfg(test)]
pub fn world_place(local: Place, house_at: Vec3, house_yaw_deg: f32) -> Place {
    let (dx, dz) = yaw_xz(local.position.x, local.position.z, house_yaw_deg);
    Place::new(
        house_at.x + dx,
        house_at.y + local.position.y,
        house_at.z + dz,
    )
    .with_yaw_deg(house_yaw_deg + local.yaw_degrees)
}

pub(crate) fn centre_footprint(catalog: &Catalog, assembly: &Assembly<'_>) -> ModularResult<Vec<PlacedMesh>> {
    let cells = assembly.occupied_cells();
    let origin = catalog
        .pitch()
        .mesh_origin(&cells)
        .unwrap_or_else(|| panic!("assembly occupancy is empty"));
    let mut places = assembly.places()?;
    for item in &mut places {
        item.place.position.x -= origin.x;
        item.place.position.z -= origin.z;
    }
    Ok(places)
}

#[cfg(test)]
fn plan_size(assembly: &Assembly<'_>, pitch: GridPitch) -> (f32, f32) {
    let cells = assembly.occupied_cells();
    let min_x = cells.iter().map(|c| c.x).min().expect("occupancy");
    let max_x = cells.iter().map(|c| c.x).max().expect("occupancy");
    let min_z = cells.iter().map(|c| c.z).min().expect("occupancy");
    let max_z = cells.iter().map(|c| c.z).max().expect("occupancy");
    (
        (max_x - min_x + 1) as f32 * pitch.xz,
        (max_z - min_z + 1) as f32 * pitch.xz,
    )
}

fn ring_3x2(
    assembly: &mut Assembly<'_>,
    origin: Cell,
    sw: &str,
    south: &str,
    se: &str,
    ne: &str,
    north: &str,
    nw: &str,
) -> ModularResult<[InstanceId; 6]> {
    let sw_id = assembly.place(pid(sw), origin, YawQuarter::Deg0)?;
    let south_id = assembly.mate(sw_id, did("pos_x"), pid(south), did("neg_x"))?;
    let se_id = assembly.mate(south_id, did("pos_x"), pid(se), did("pos_z"))?;
    let ne_id = assembly.mate(se_id, did("pos_x"), pid(ne), did("pos_z"))?;
    let north_id = assembly.mate(ne_id, did("pos_x"), pid(north), did("neg_x"))?;
    let nw_id = assembly.mate(north_id, did("pos_x"), pid(nw), did("pos_z"))?;
    assembly.join(nw_id, did("pos_x"), sw_id, did("pos_z"))?;
    Ok([sw_id, south_id, se_id, ne_id, north_id, nw_id])
}

fn join_ring_3x2(assembly: &mut Assembly<'_>, ring: &[InstanceId; 6]) -> ModularResult<()> {
    assembly.join(ring[0], did("pos_x"), ring[1], did("neg_x"))?;
    assembly.join(ring[1], did("pos_x"), ring[2], did("pos_z"))?;
    assembly.join(ring[2], did("pos_x"), ring[3], did("pos_z"))?;
    assembly.join(ring[3], did("pos_x"), ring[4], did("neg_x"))?;
    assembly.join(ring[4], did("pos_x"), ring[5], did("pos_z"))?;
    assembly.join(ring[5], did("pos_x"), ring[0], did("pos_z"))?;
    Ok(())
}

fn ring_3x4(
    assembly: &mut Assembly<'_>,
    origin: Cell,
    sw: &str,
    south: &str,
    se: &str,
    e1: &str,
    e2: &str,
    ne: &str,
    north: &str,
    nw: &str,
    w1: &str,
    w2: &str,
) -> ModularResult<[InstanceId; 10]> {
    let sw_id = assembly.place(pid(sw), origin, YawQuarter::Deg0)?;
    let south_id = assembly.mate(sw_id, did("pos_x"), pid(south), did("neg_x"))?;
    let se_id = assembly.mate(south_id, did("pos_x"), pid(se), did("pos_z"))?;
    let e1_id = assembly.mate(se_id, did("pos_x"), pid(e1), did("neg_x"))?;
    let e2_id = assembly.mate(e1_id, did("pos_x"), pid(e2), did("neg_x"))?;
    let ne_id = assembly.mate(e2_id, did("pos_x"), pid(ne), did("pos_z"))?;
    let north_id = assembly.mate(ne_id, did("pos_x"), pid(north), did("neg_x"))?;
    let nw_id = assembly.mate(north_id, did("pos_x"), pid(nw), did("pos_z"))?;
    let w1_id = assembly.mate(nw_id, did("pos_x"), pid(w1), did("neg_x"))?;
    let w2_id = assembly.mate(w1_id, did("pos_x"), pid(w2), did("neg_x"))?;
    assembly.join(w2_id, did("pos_x"), sw_id, did("pos_z"))?;
    Ok([
        sw_id, south_id, se_id, e1_id, e2_id, ne_id, north_id, nw_id, w1_id, w2_id,
    ])
}

fn join_ring_3x4(assembly: &mut Assembly<'_>, ring: &[InstanceId; 10]) -> ModularResult<()> {
    assembly.join(ring[0], did("pos_x"), ring[1], did("neg_x"))?;
    assembly.join(ring[1], did("pos_x"), ring[2], did("pos_z"))?;
    assembly.join(ring[2], did("pos_x"), ring[3], did("neg_x"))?;
    assembly.join(ring[3], did("pos_x"), ring[4], did("neg_x"))?;
    assembly.join(ring[4], did("pos_x"), ring[5], did("pos_z"))?;
    assembly.join(ring[5], did("pos_x"), ring[6], did("neg_x"))?;
    assembly.join(ring[6], did("pos_x"), ring[7], did("pos_z"))?;
    assembly.join(ring[7], did("pos_x"), ring[8], did("neg_x"))?;
    assembly.join(ring[8], did("pos_x"), ring[9], did("neg_x"))?;
    assembly.join(ring[9], did("pos_x"), ring[0], did("pos_z"))?;
    Ok(())
}

fn plinths(assembly: &mut Assembly<'_>, ground: &[InstanceId]) -> ModularResult<()> {
    for &id in ground {
        assembly.mate(id, did("down"), pid("plinth"), did("up"))?;
    }
    Ok(())
}

fn roofs(
    assembly: &mut Assembly<'_>,
    walls: &[InstanceId],
    chimney_at: Option<usize>,
) -> ModularResult<()> {
    for (index, &id) in walls.iter().enumerate() {
        let cap = if Some(index) == chimney_at {
            "chimney"
        } else {
            "roof"
        };
        assembly.mate(id, did("up"), pid(cap), did("down"))?;
    }
    Ok(())
}

fn assemble_hut(catalog: &Catalog) -> ModularResult<Assembly<'_>> {
    let mut assembly = Assembly::new(catalog);
    let ring = ring_3x2(
        &mut assembly,
        Cell::new(0, 0, 0),
        "corner",
        "door",
        "corner",
        "corner",
        "wall",
        "corner",
    )?;
    plinths(&mut assembly, &ring)?;
    roofs(&mut assembly, &ring, None)?;
    Ok(assembly)
}

fn assemble_cabin(catalog: &Catalog) -> ModularResult<Assembly<'_>> {
    let mut assembly = Assembly::new(catalog);
    let ring = ring_3x2(
        &mut assembly,
        Cell::new(0, 0, 0),
        "corner",
        "door",
        "corner",
        "corner",
        "window_b",
        "corner",
    )?;
    plinths(&mut assembly, &ring)?;
    roofs(&mut assembly, &ring, Some(4))?;
    Ok(assembly)
}

fn assemble_townhouse(catalog: &Catalog) -> ModularResult<Assembly<'_>> {
    let mut assembly = Assembly::new(catalog);
    let ground = ring_3x2(
        &mut assembly,
        Cell::new(0, 0, 0),
        "corner",
        "door",
        "corner",
        "corner",
        "window",
        "corner",
    )?;
    plinths(&mut assembly, &ground)?;
    let upper_ids = [
        "corner_jetty",
        "window_jetty",
        "corner_jetty",
        "corner_jetty",
        "window_b_jetty",
        "corner_jetty",
    ];
    let upper = [
        assembly.mate(ground[0], did("up"), pid(upper_ids[0]), did("down"))?,
        assembly.mate(ground[1], did("up"), pid(upper_ids[1]), did("down"))?,
        assembly.mate(ground[2], did("up"), pid(upper_ids[2]), did("down"))?,
        assembly.mate(ground[3], did("up"), pid(upper_ids[3]), did("down"))?,
        assembly.mate(ground[4], did("up"), pid(upper_ids[4]), did("down"))?,
        assembly.mate(ground[5], did("up"), pid(upper_ids[5]), did("down"))?,
    ];
    join_ring_3x2(&mut assembly, &upper)?;
    roofs(&mut assembly, &upper, Some(4))?;
    Ok(assembly)
}

fn assemble_hall(catalog: &Catalog) -> ModularResult<Assembly<'_>> {
    let mut assembly = Assembly::new(catalog);
    let ground = ring_3x4(
        &mut assembly,
        Cell::new(0, 0, 0),
        "corner",
        "door",
        "corner",
        "window",
        "window_b",
        "corner",
        "window_b",
        "corner",
        "window",
        "wall",
    )?;
    plinths(&mut assembly, &ground)?;
    let upper_ids = [
        "corner_jetty",
        "window_jetty",
        "corner_jetty",
        "window_jetty",
        "window_b_jetty",
        "corner_jetty",
        "window_b_jetty",
        "corner_jetty",
        "window_jetty",
        "wall_jetty",
    ];
    let upper = [
        assembly.mate(ground[0], did("up"), pid(upper_ids[0]), did("down"))?,
        assembly.mate(ground[1], did("up"), pid(upper_ids[1]), did("down"))?,
        assembly.mate(ground[2], did("up"), pid(upper_ids[2]), did("down"))?,
        assembly.mate(ground[3], did("up"), pid(upper_ids[3]), did("down"))?,
        assembly.mate(ground[4], did("up"), pid(upper_ids[4]), did("down"))?,
        assembly.mate(ground[5], did("up"), pid(upper_ids[5]), did("down"))?,
        assembly.mate(ground[6], did("up"), pid(upper_ids[6]), did("down"))?,
        assembly.mate(ground[7], did("up"), pid(upper_ids[7]), did("down"))?,
        assembly.mate(ground[8], did("up"), pid(upper_ids[8]), did("down"))?,
        assembly.mate(ground[9], did("up"), pid(upper_ids[9]), did("down"))?,
    ];
    join_ring_3x4(&mut assembly, &upper)?;
    roofs(&mut assembly, &upper, Some(6))?;
    Ok(assembly)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> Catalog {
        super::catalog()
    }

    #[test]
    fn every_dwelling_recipe_closes_and_matches_the_village_catalog() {
        let catalog = catalog();
        for id in DWELLING_IDS {
            let spec = super::super::catalog::spec_for(id).expect("catalog");
            assert!(spec.is_dwelling());
            let assembly = match *id {
                "house_hut_thatch" => assemble_hut(&catalog).unwrap(),
                "house_cabin_timber" => assemble_cabin(&catalog).unwrap(),
                "house_cottage_stone" => assemble_townhouse(&catalog).unwrap(),
                "house_hall_large" => assemble_hall(&catalog).unwrap(),
                other => panic!("{other}"),
            };
            let open = assembly.unmated_seams().unwrap();
            assert!(open.is_empty(), "{id} free seams: {open:?}");
            let (size_x, size_z) = plan_size(&assembly, catalog.pitch());
            assert!(
                (size_x - spec.size_x).abs() < 1e-4 && (size_z - spec.size_z).abs() < 1e-4,
                "{id} occupancy {size_x}×{size_z} vs catalog {}×{}",
                spec.size_x,
                spec.size_z
            );
        }
    }

    #[test]
    fn cabin_puts_the_door_on_the_south_wall() {
        let catalog = catalog();
        let assembly = assemble_cabin(&catalog).unwrap();
        let door = assembly
            .instances()
            .into_iter()
            .find(|p| p.piece == pid("door"))
            .expect("door");
        let min_z = assembly
            .occupied_cells()
            .into_iter()
            .filter(|c| c.y == 0)
            .map(|c| c.z)
            .min()
            .unwrap();
        assert_eq!(door.cell.z, min_z);
        assert_eq!(door.cell.y, 0);
    }

    #[test]
    fn townhouse_upper_ring_is_jettied() {
        let catalog = catalog();
        let assembly = assemble_townhouse(&catalog).unwrap();
        let jetty = assembly
            .instances()
            .into_iter()
            .filter(|p| p.cell.y == 1)
            .count();
        assert_eq!(jetty, 6);
    }

    #[test]
    fn world_place_rotates_the_door_with_the_house() {
        let local = Place::new(0.0, 0.0, -4.0);
        let moved = world_place(local, Vec3::new(10.0, 5.0, 20.0), 90.0);
        assert!((moved.position.x - 6.0).abs() < 1e-4, "{}", moved.position);
        assert!((moved.position.y - 5.0).abs() < 1e-4);
        assert!((moved.position.z - 20.0).abs() < 1e-4);
        assert!((moved.yaw_degrees - 90.0).abs() < 1e-4);
    }
}
