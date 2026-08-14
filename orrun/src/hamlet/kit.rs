//! Medieval house recipes for hamlets. Kit vocabulary stays out of `modular`.
//!
//! Pieces come from Asset Lab `med_*` products. Catalog JSON is the Modular
//! medieval kit. Door wall is the south (−Z) long side, matching planner yaw 0.
//! Footprint and storey layout stay a recipe; each slot picks a seeded member
//! of the catalog family that shares its seams. Door leaves (`door_plank` /
//! `door_sturdy`) are not catalog pieces: they hang on the opening after the
//! ring closes, matching Modular's `medieval_house` example.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use engine::error::EngineError;
use engine::mesh::Mesh;
use engine::model::Model;
use engine::place::Place;
use glam::Vec3;
use modular::prelude::*;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
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
    ("chimney", "med_chimney.glb"),
    ("floor", "med_floor.glb"),
    ("plinth", "med_plinth.glb"),
    ("plinth_b", "med_plinth_b.glb"),
    ("door_plank", "door_plank.glb"),
    ("door_sturdy", "door_sturdy.glb"),
];

/// `med_door` / `med_door_b` opening and `door_plank` / `door_sturdy` leaf.
/// Jamb proud is `hard_surface.kit_cell` `_opening_trim`; threshold is the
/// door floor slab. Leaf pivot is the +X jamb; yaw 180° so hardware (authored
/// −Y) faces the street.
const WALL_THICKNESS: f32 = 0.28;
const OVERLAP: f32 = 0.02;
const JAMB_PROUD: f32 = 0.02;
const HINGE_INSET: f32 = 0.01;
const THRESHOLD_TOP: f32 = OVERLAP + 0.012 + 0.015 + 0.08;

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

fn fid(id: &str) -> FamilyId {
    FamilyId::new(id).unwrap_or_else(|err| panic!("{err}"))
}

fn pick(catalog: &Catalog, family: &str, rng: &mut StdRng) -> PieceId {
    catalog
        .pick_family(&fid(family), rng)
        .unwrap_or_else(|err| panic!("{err}"))
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

pub fn assemble_dwelling(
    catalog: &Catalog,
    catalog_id: &str,
    seed: u64,
) -> Result<Vec<PlacedMesh>, KitError> {
    let mut rng = StdRng::seed_from_u64(seed);
    let assembly = match catalog_id {
        "house_hut_thatch" => assemble_hut(catalog, &mut rng)?,
        "house_cabin_timber" => assemble_cabin(catalog, &mut rng)?,
        "house_cottage_stone" => assemble_townhouse(catalog, &mut rng)?,
        "house_hall_large" => assemble_hall(catalog, &mut rng)?,
        other => panic!("'{other}' is not a modular dwelling"),
    };
    let open = assembly.unmated_seams()?;
    if !open.is_empty() {
        panic!("{catalog_id} has free seams (kit bug): {open:?}");
    }
    let origin = footprint_origin(catalog, &assembly);
    let mut places = assembly.places()?;
    places.extend(door_leaf_meshes(catalog, &assembly, catalog_id));
    shift_xz(&mut places, origin);
    Ok(places)
}

/// Seeded layouts kept per dwelling catalog id.
///
/// Family picks live in the recipe, not in Modular. Ports stamp hundreds of
/// houses; seating looks these up instead of assembling each plot.
pub const VARIATIONS_PER_DWELLING: u32 = 16;

#[derive(Clone, Debug)]
pub struct DwellingRecipes {
    by_id: HashMap<&'static str, Vec<Vec<PlacedMesh>>>,
}

impl DwellingRecipes {
    pub fn roll(catalog: &Catalog) -> Result<Self, KitError> {
        let mut by_id = HashMap::new();
        for id in DWELLING_IDS {
            let mut variations = Vec::with_capacity(VARIATIONS_PER_DWELLING as usize);
            for seed in 0..u64::from(VARIATIONS_PER_DWELLING) {
                variations.push(assemble_dwelling(catalog, id, seed)?);
            }
            by_id.insert(*id, variations);
        }
        Ok(Self { by_id })
    }

    pub fn get(&self, catalog_id: &str, seed: u64) -> &[PlacedMesh] {
        let variations = self
            .by_id
            .get(catalog_id)
            .unwrap_or_else(|| panic!("'{catalog_id}' is not a modular dwelling"));
        &variations[(seed % variations.len() as u64) as usize]
    }
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

fn footprint_origin(catalog: &Catalog, assembly: &Assembly<'_>) -> Vec3 {
    catalog
        .pitch()
        .mesh_origin(&assembly.occupied_cells())
        .unwrap_or_else(|| panic!("assembly occupancy is empty"))
}

fn shift_xz(places: &mut [PlacedMesh], origin: Vec3) {
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

fn rotate_yaw(v: Vec3, yaw: YawQuarter) -> Vec3 {
    match yaw {
        YawQuarter::Deg0 => v,
        YawQuarter::Deg90 => Vec3::new(v.z, v.y, -v.x),
        YawQuarter::Deg180 => Vec3::new(-v.x, v.y, -v.z),
        YawQuarter::Deg270 => Vec3::new(-v.z, v.y, v.x),
    }
}

fn door_opening_width(piece: &str) -> f32 {
    match piece {
        "door" => 1.1,
        "door_b" => 1.05,
        other => panic!("door family piece '{other}' has no opening width"),
    }
}

fn door_leaf_id(catalog_id: &str) -> &'static str {
    match catalog_id {
        "house_hut_thatch" | "house_cabin_timber" => "door_plank",
        "house_cottage_stone" | "house_hall_large" => "door_sturdy",
        other => panic!("'{other}' is not a modular dwelling"),
    }
}

fn door_leaf_place(catalog: &Catalog, host: &Placed) -> Place {
    let piece = catalog
        .piece(&host.piece)
        .unwrap_or_else(|err| panic!("{err}"));
    let cells = piece.world_occupancy(host.cell, host.yaw);
    let origin = catalog
        .pitch()
        .mesh_origin(&cells)
        .unwrap_or_else(|| panic!("{} has empty occupancy", host.piece));
    let local = Vec3::new(
        door_opening_width(host.piece.as_str()) * 0.5 - JAMB_PROUD - HINGE_INSET,
        THRESHOLD_TOP,
        -(catalog.pitch().xz * 0.5 - WALL_THICKNESS * 0.5),
    );
    let offset = rotate_yaw(local, host.yaw);
    Place::new(
        origin.x + offset.x,
        origin.y + offset.y,
        origin.z + offset.z,
    )
    .with_yaw_deg(host.yaw.degrees() + 180.0)
}

fn door_leaf_meshes(
    catalog: &Catalog,
    assembly: &Assembly<'_>,
    catalog_id: &str,
) -> Vec<PlacedMesh> {
    let leaf = pid(door_leaf_id(catalog_id));
    let mut out = Vec::new();
    for placed in assembly.instances() {
        let family = catalog
            .piece(&placed.piece)
            .unwrap_or_else(|err| panic!("{err}"))
            .family();
        if family.as_str() != "door" {
            continue;
        }
        out.push(PlacedMesh {
            piece: leaf.clone(),
            place: door_leaf_place(catalog, placed),
        });
    }
    if out.is_empty() {
        panic!("{catalog_id} has no door piece to hang a leaf on");
    }
    out
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

fn plinths(assembly: &mut Assembly<'_>, ground: &[InstanceId], plinth: &str) -> ModularResult<()> {
    for &id in ground {
        assembly.mate(id, did("down"), pid(plinth), did("up"))?;
    }
    Ok(())
}

fn roofs(
    assembly: &mut Assembly<'_>,
    walls: &[InstanceId],
    roof: &str,
    chimney_at: Option<usize>,
) -> ModularResult<()> {
    for (index, &id) in walls.iter().enumerate() {
        let cap = if Some(index) == chimney_at {
            "chimney"
        } else {
            roof
        };
        assembly.mate(id, did("up"), pid(cap), did("down"))?;
    }
    Ok(())
}

fn fill_interior_3x4(
    assembly: &mut Assembly<'_>,
    floor: &str,
    plinth: &str,
    roof: &str,
) -> ModularResult<()> {
    // A 3×4 ring leaves (1, *, 1) and (1, *, 2) empty. Wall floors only cover
    // the ring; these stacks close the room on both storeys.
    for z in 1..=2 {
        let ground = assembly.place(pid(floor), Cell::new(1, 0, z), YawQuarter::Deg0)?;
        assembly.mate(ground, did("down"), pid(plinth), did("up"))?;
        let upper = assembly.mate(ground, did("up"), pid(floor), did("down"))?;
        assembly.mate(upper, did("up"), pid(roof), did("down"))?;
    }
    Ok(())
}

fn assemble_hut<'a>(catalog: &'a Catalog, rng: &mut StdRng) -> ModularResult<Assembly<'a>> {
    let mut assembly = Assembly::new(catalog);
    let corner = pick(catalog, "corner", rng);
    let door = pick(catalog, "door", rng);
    let north = pick(catalog, "straight", rng);
    let ring = ring_3x2(
        &mut assembly,
        Cell::new(0, 0, 0),
        corner.as_str(),
        door.as_str(),
        corner.as_str(),
        corner.as_str(),
        north.as_str(),
        corner.as_str(),
    )?;
    let plinth = pick(catalog, "plinth", rng);
    let roof = pick(catalog, "roof", rng);
    plinths(&mut assembly, &ring, plinth.as_str())?;
    roofs(&mut assembly, &ring, roof.as_str(), None)?;
    Ok(assembly)
}

fn assemble_cabin<'a>(catalog: &'a Catalog, rng: &mut StdRng) -> ModularResult<Assembly<'a>> {
    let mut assembly = Assembly::new(catalog);
    let corner = pick(catalog, "corner", rng);
    let door = pick(catalog, "door", rng);
    let north = pick(catalog, "straight", rng);
    let ring = ring_3x2(
        &mut assembly,
        Cell::new(0, 0, 0),
        corner.as_str(),
        door.as_str(),
        corner.as_str(),
        corner.as_str(),
        north.as_str(),
        corner.as_str(),
    )?;
    let plinth = pick(catalog, "plinth", rng);
    let roof = pick(catalog, "roof", rng);
    plinths(&mut assembly, &ring, plinth.as_str())?;
    roofs(
        &mut assembly,
        &ring,
        roof.as_str(),
        Some(rng.gen_range(0..ring.len())),
    )?;
    Ok(assembly)
}

fn stack_upper_3x2(
    assembly: &mut Assembly<'_>,
    catalog: &Catalog,
    ground: &[InstanceId; 6],
    rng: &mut StdRng,
) -> ModularResult<[InstanceId; 6]> {
    let corner = pick(catalog, "corner_jetty", rng);
    let ids = [
        corner.clone(),
        pick(catalog, "straight_jetty", rng),
        corner.clone(),
        corner.clone(),
        pick(catalog, "straight_jetty", rng),
        corner,
    ];
    let upper = [
        assembly.mate(ground[0], did("up"), ids[0].clone(), did("down"))?,
        assembly.mate(ground[1], did("up"), ids[1].clone(), did("down"))?,
        assembly.mate(ground[2], did("up"), ids[2].clone(), did("down"))?,
        assembly.mate(ground[3], did("up"), ids[3].clone(), did("down"))?,
        assembly.mate(ground[4], did("up"), ids[4].clone(), did("down"))?,
        assembly.mate(ground[5], did("up"), ids[5].clone(), did("down"))?,
    ];
    join_ring_3x2(assembly, &upper)?;
    Ok(upper)
}

fn assemble_townhouse<'a>(catalog: &'a Catalog, rng: &mut StdRng) -> ModularResult<Assembly<'a>> {
    let mut assembly = Assembly::new(catalog);
    let corner = pick(catalog, "corner", rng);
    let door = pick(catalog, "door", rng);
    let north = pick(catalog, "straight", rng);
    let ground = ring_3x2(
        &mut assembly,
        Cell::new(0, 0, 0),
        corner.as_str(),
        door.as_str(),
        corner.as_str(),
        corner.as_str(),
        north.as_str(),
        corner.as_str(),
    )?;
    let plinth = pick(catalog, "plinth", rng);
    plinths(&mut assembly, &ground, plinth.as_str())?;
    let upper = stack_upper_3x2(&mut assembly, catalog, &ground, rng)?;
    let roof = pick(catalog, "roof", rng);
    roofs(
        &mut assembly,
        &upper,
        roof.as_str(),
        Some(rng.gen_range(0..upper.len())),
    )?;
    Ok(assembly)
}

fn assemble_hall<'a>(catalog: &'a Catalog, rng: &mut StdRng) -> ModularResult<Assembly<'a>> {
    let mut assembly = Assembly::new(catalog);
    let corner = pick(catalog, "corner", rng);
    let door = pick(catalog, "door", rng);
    let e1 = pick(catalog, "straight", rng);
    let e2 = pick(catalog, "straight", rng);
    let north = pick(catalog, "straight", rng);
    let w1 = pick(catalog, "straight", rng);
    let w2 = pick(catalog, "straight", rng);
    let ground = ring_3x4(
        &mut assembly,
        Cell::new(0, 0, 0),
        corner.as_str(),
        door.as_str(),
        corner.as_str(),
        e1.as_str(),
        e2.as_str(),
        corner.as_str(),
        north.as_str(),
        corner.as_str(),
        w1.as_str(),
        w2.as_str(),
    )?;
    let plinth = pick(catalog, "plinth", rng);
    plinths(&mut assembly, &ground, plinth.as_str())?;
    let corner_j = pick(catalog, "corner_jetty", rng);
    let upper_ids = [
        corner_j.clone(),
        pick(catalog, "straight_jetty", rng),
        corner_j.clone(),
        pick(catalog, "straight_jetty", rng),
        pick(catalog, "straight_jetty", rng),
        corner_j.clone(),
        pick(catalog, "straight_jetty", rng),
        corner_j.clone(),
        pick(catalog, "straight_jetty", rng),
        pick(catalog, "straight_jetty", rng),
    ];
    let upper = [
        assembly.mate(ground[0], did("up"), upper_ids[0].clone(), did("down"))?,
        assembly.mate(ground[1], did("up"), upper_ids[1].clone(), did("down"))?,
        assembly.mate(ground[2], did("up"), upper_ids[2].clone(), did("down"))?,
        assembly.mate(ground[3], did("up"), upper_ids[3].clone(), did("down"))?,
        assembly.mate(ground[4], did("up"), upper_ids[4].clone(), did("down"))?,
        assembly.mate(ground[5], did("up"), upper_ids[5].clone(), did("down"))?,
        assembly.mate(ground[6], did("up"), upper_ids[6].clone(), did("down"))?,
        assembly.mate(ground[7], did("up"), upper_ids[7].clone(), did("down"))?,
        assembly.mate(ground[8], did("up"), upper_ids[8].clone(), did("down"))?,
        assembly.mate(ground[9], did("up"), upper_ids[9].clone(), did("down"))?,
    ];
    join_ring_3x4(&mut assembly, &upper)?;
    let roof = pick(catalog, "roof", rng);
    roofs(
        &mut assembly,
        &upper,
        roof.as_str(),
        Some(rng.gen_range(0..upper.len())),
    )?;
    let floor = pick(catalog, "floor", rng);
    fill_interior_3x4(
        &mut assembly,
        floor.as_str(),
        plinth.as_str(),
        roof.as_str(),
    )?;
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
            let mut rng = StdRng::seed_from_u64(1);
            let assembly = match *id {
                "house_hut_thatch" => assemble_hut(&catalog, &mut rng).unwrap(),
                "house_cabin_timber" => assemble_cabin(&catalog, &mut rng).unwrap(),
                "house_cottage_stone" => assemble_townhouse(&catalog, &mut rng).unwrap(),
                "house_hall_large" => assemble_hall(&catalog, &mut rng).unwrap(),
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
        let mut rng = StdRng::seed_from_u64(1);
        let assembly = assemble_cabin(&catalog, &mut rng).unwrap();
        let door = assembly
            .instances()
            .into_iter()
            .find(|p| catalog.piece(&p.piece).unwrap().family().as_str() == "door")
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
        let mut rng = StdRng::seed_from_u64(1);
        let assembly = assemble_townhouse(&catalog, &mut rng).unwrap();
        let jetty = assembly
            .instances()
            .into_iter()
            .filter(|p| p.cell.y == 1)
            .count();
        assert_eq!(jetty, 6);
    }

    #[test]
    fn hall_fills_the_interior_cells() {
        let catalog = catalog();
        let mut rng = StdRng::seed_from_u64(1);
        let assembly = assemble_hall(&catalog, &mut rng).unwrap();
        let ground: std::collections::HashSet<_> = assembly
            .occupied_cells()
            .into_iter()
            .filter(|cell| cell.y == 0)
            .collect();
        assert_eq!(ground.len(), 12);
        assert!(ground.contains(&Cell::new(1, 0, 1)));
        assert!(ground.contains(&Cell::new(1, 0, 2)));
        let floor_cells = assembly
            .instances()
            .into_iter()
            .filter(|placed| placed.piece.as_str() == "floor")
            .count();
        assert_eq!(floor_cells, 4);
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

    #[test]
    fn same_seed_reproduces_pieces_and_different_seeds_can_vary() {
        let catalog = catalog();
        let a = assemble_dwelling(&catalog, "house_hall_large", 3).unwrap();
        let b = assemble_dwelling(&catalog, "house_hall_large", 3).unwrap();
        let c = assemble_dwelling(&catalog, "house_hall_large", 11).unwrap();
        let names = |places: &[PlacedMesh]| {
            places
                .iter()
                .map(|item| item.piece.as_str().to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&a), names(&b));
        assert_ne!(names(&a), names(&c));
    }

    fn leaf_of(places: &[PlacedMesh]) -> &PlacedMesh {
        places
            .iter()
            .find(|item| {
                item.piece.as_str() == "door_plank" || item.piece.as_str() == "door_sturdy"
            })
            .expect("door leaf")
    }

    fn door_of<'a>(catalog: &Catalog, assembly: &'a Assembly<'_>) -> &'a Placed {
        assembly
            .instances()
            .into_iter()
            .find(|placed| catalog.piece(&placed.piece).unwrap().family().as_str() == "door")
            .expect("door")
    }

    #[test]
    fn every_dwelling_hangs_one_leaf_on_the_south_door() {
        let catalog = catalog();
        let expected = [
            ("house_hut_thatch", "door_plank"),
            ("house_cabin_timber", "door_plank"),
            ("house_cottage_stone", "door_sturdy"),
            ("house_hall_large", "door_sturdy"),
        ];
        for (id, leaf_id) in expected {
            let places = assemble_dwelling(&catalog, id, 1).unwrap();
            let leaves: Vec<_> = places
                .iter()
                .filter(|item| {
                    item.piece.as_str() == "door_plank" || item.piece.as_str() == "door_sturdy"
                })
                .collect();
            assert_eq!(leaves.len(), 1, "{id}");
            assert_eq!(leaves[0].piece.as_str(), leaf_id, "{id}");
        }
    }

    #[test]
    fn door_leaf_sits_on_the_plus_x_jamb_facing_the_street() {
        let catalog = catalog();
        let mut rng = StdRng::seed_from_u64(1);
        let assembly = assemble_cabin(&catalog, &mut rng).unwrap();
        let host = door_of(&catalog, &assembly);
        let places = assemble_dwelling(&catalog, "house_cabin_timber", 1).unwrap();
        let leaf = leaf_of(&places);
        let origin = footprint_origin(&catalog, &assembly);
        let expected = door_leaf_place(&catalog, host);
        assert!((leaf.place.position.x - (expected.position.x - origin.x)).abs() < 1e-4);
        assert!((leaf.place.position.y - expected.position.y).abs() < 1e-4);
        assert!((leaf.place.position.z - (expected.position.z - origin.z)).abs() < 1e-4);
        assert!((leaf.place.yaw_degrees - (host.yaw.degrees() + 180.0)).abs() < 1e-4);
        assert_eq!(host.cell.z, 0);
        assert_eq!(host.yaw, YawQuarter::Deg0);
    }

    #[test]
    fn door_b_also_gets_a_leaf() {
        let catalog = catalog();
        let seed = (0..u64::from(VARIATIONS_PER_DWELLING)).find(|&seed| {
            let mut rng = StdRng::seed_from_u64(seed);
            let assembly = assemble_hut(&catalog, &mut rng).unwrap();
            door_of(&catalog, &assembly).piece.as_str() == "door_b"
        });
        let Some(seed) = seed else {
            panic!("no hut variation picked door_b");
        };
        let places = assemble_dwelling(&catalog, "house_hut_thatch", seed).unwrap();
        assert_eq!(leaf_of(&places).piece.as_str(), "door_plank");
    }

    #[test]
    fn rolled_variations_stay_in_memory_for_the_same_slot() {
        let catalog = catalog();
        let recipes = DwellingRecipes::roll(&catalog).unwrap();
        let a = recipes.get("house_hall_large", 3);
        let b = recipes.get("house_hall_large", 3 + u64::from(VARIATIONS_PER_DWELLING));
        let assembled = assemble_dwelling(&catalog, "house_hall_large", 3).unwrap();
        let names = |places: &[PlacedMesh]| {
            places
                .iter()
                .map(|item| item.piece.as_str().to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(a), names(&assembled));
        assert_eq!(names(a), names(b));
        assert!(std::ptr::eq(a, b));
        for id in DWELLING_IDS {
            assert_eq!(
                recipes.get(id, 0).len(),
                assemble_dwelling(&catalog, id, 0).unwrap().len()
            );
        }
    }
}
