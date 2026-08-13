//! Castle kit recipes for hamlets. Vocabulary stays in `catalogs/castle.json`.
//!
//! Pieces come from Asset Lab `castle_*` products. Keep-and-curtain rings match
//! [`super::castle`] cell counts. Gate on the south (−Z) wall, yaw 0.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use engine::mesh::Mesh;
use engine::model::Model;
use modular::prelude::*;

use super::castle::{self, CastleLayout};
use super::kit::{self, KitError};

const CASTLE_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../Modular/catalogs/castle.json"
));

pub const PIECE_GLBS: &[(&str, &str)] = &[
    ("curtain", "castle_curtain.glb"),
    ("loop", "castle_loop.glb"),
    ("gate", "castle_gate.glb"),
    ("tower", "castle_tower.glb"),
    ("turret", "castle_turret.glb"),
    ("battlement", "castle_battlement.glb"),
    ("plinth", "castle_plinth.glb"),
];

const RECIPE_IDS: &[&str] = &["castle_keep_8x6", "castle_keep_12x10", "castle_keep_16x14"];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Side {
    South,
    East,
    North,
    West,
}

struct Ring {
    ids: Vec<InstanceId>,
    is_corner: Vec<bool>,
}

impl Ring {
    fn corner_indices(&self) -> [usize; 4] {
        let mut out = [0; 4];
        let mut n = 0;
        for (i, &corner) in self.is_corner.iter().enumerate() {
            if corner {
                out[n] = i;
                n += 1;
            }
        }
        assert_eq!(n, 4, "ring must have four towers");
        out
    }
}

fn pid(id: &str) -> PieceId {
    PieceId::new(id).unwrap_or_else(|err| panic!("{err}"))
}

fn did(id: &str) -> DockId {
    DockId::new(id).unwrap_or_else(|err| panic!("{err}"))
}

pub fn catalog() -> Catalog {
    Catalog::from_json(CASTLE_JSON).expect("Modular catalogs/castle.json")
}

pub fn glb_name(piece: &PieceId) -> &'static str {
    PIECE_GLBS
        .iter()
        .find(|(id, _)| *id == piece.as_str())
        .map(|(_, file)| *file)
        .unwrap_or_else(|| panic!("castle kit has no glb mapping for piece {piece}"))
}

fn projekte_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn kit_search_paths(file: &str) -> Vec<PathBuf> {
    let mut tried = Vec::new();
    if let Some(dir) = std::env::var_os("ORRUN_ASSETS") {
        tried.push(PathBuf::from(dir).join("kit").join("castle").join(file));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            tried.push(dir.join("assets").join("kit").join("castle").join(file));
        }
    }
    tried.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join("kit")
            .join("castle")
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

/// Closed, footprint-centred places for every castle catalog id.
pub fn castle_recipes(catalog: &Catalog) -> Result<HashMap<String, Vec<PlacedMesh>>, KitError> {
    let mut out = HashMap::new();
    for id in RECIPE_IDS {
        out.insert((*id).to_string(), assemble_castle(catalog, id)?);
    }
    Ok(out)
}

pub fn assemble_castle(catalog: &Catalog, catalog_id: &str) -> Result<Vec<PlacedMesh>, KitError> {
    let layout = castle::layout_for(catalog_id)
        .unwrap_or_else(|| panic!("'{catalog_id}' is not a castle recipe"));
    let assembly = assemble_keep_and_curtain(catalog, layout)?;
    let open = assembly.unmated_seams()?;
    if !open.is_empty() {
        panic!("{catalog_id} has free seams (kit bug): {open:?}");
    }
    Ok(kit::centre_footprint(catalog, &assembly)?)
}

fn assemble_keep_and_curtain<'a>(
    catalog: &'a Catalog,
    layout: CastleLayout,
) -> ModularResult<Assembly<'a>> {
    let mut assembly = Assembly::new(catalog);
    enclosed_ward(
        &mut assembly,
        Cell::new(0, 0, 0),
        layout.cells_x,
        layout.cells_z,
        layout.bailey_storeys,
        layout.tower_extra,
        &[Side::South],
        true,
    )?;
    enclosed_ward(
        &mut assembly,
        Cell::new(layout.keep_origin_x, 0, layout.keep_origin_z),
        layout.keep_cells_x,
        layout.keep_cells_z,
        layout.keep_storeys,
        0,
        &[Side::South],
        true,
    )?;
    Ok(assembly)
}

fn edge_piece(side: Side, index: i32, len: i32, gates: &[Side], loops: bool) -> &'static str {
    if gates.contains(&side) && len > 0 && index == len / 2 {
        return "gate";
    }
    if loops && index % 2 == 1 {
        "loop"
    } else {
        "curtain"
    }
}

fn perimeter(width: i32, depth: i32, gates: &[Side], loops: bool) -> Vec<(&'static str, bool)> {
    assert!(width >= 2 && depth >= 2, "ring needs width>=2 and depth>=2");
    for side in gates {
        let n = match side {
            Side::South | Side::North => width - 2,
            Side::East | Side::West => depth - 2,
        };
        assert!(n > 0, "gate side needs at least one straight cell");
    }
    let mut out = Vec::new();
    out.push(("tower", true));
    let south_n = width - 2;
    for i in 0..south_n {
        out.push((edge_piece(Side::South, i, south_n, gates, loops), false));
    }
    out.push(("tower", true));
    let east_n = depth - 2;
    for i in 0..east_n {
        out.push((edge_piece(Side::East, i, east_n, gates, loops), false));
    }
    out.push(("tower", true));
    let north_n = width - 2;
    for i in 0..north_n {
        out.push((edge_piece(Side::North, i, north_n, gates, loops), false));
    }
    out.push(("tower", true));
    let west_n = depth - 2;
    for i in 0..west_n {
        out.push((edge_piece(Side::West, i, west_n, gates, loops), false));
    }
    out
}

fn onto_dock(is_corner: bool) -> DockId {
    if is_corner {
        did("pos_z")
    } else {
        did("neg_x")
    }
}

fn place_ring(
    assembly: &mut Assembly<'_>,
    origin: Cell,
    steps: &[(&'static str, bool)],
) -> ModularResult<Ring> {
    let first = assembly.place(pid(steps[0].0), origin, YawQuarter::Deg0)?;
    let mut ids = vec![first];
    let mut is_corner = vec![steps[0].1];
    let mut prev = first;
    for &(piece, corner) in &steps[1..] {
        let next = assembly.mate(prev, did("pos_x"), pid(piece), onto_dock(corner))?;
        ids.push(next);
        is_corner.push(corner);
        prev = next;
    }
    assembly.join(prev, did("pos_x"), first, did("pos_z"))?;
    Ok(Ring { ids, is_corner })
}

fn join_ring(assembly: &mut Assembly<'_>, ring: &Ring) -> ModularResult<()> {
    let n = ring.ids.len();
    for i in 0..n {
        let a = ring.ids[i];
        let b = ring.ids[(i + 1) % n];
        let b_dock = onto_dock(ring.is_corner[(i + 1) % n]);
        assembly.join(a, did("pos_x"), b, b_dock)?;
    }
    Ok(())
}

fn stack_ring(
    assembly: &mut Assembly<'_>,
    below: &Ring,
    steps: &[(&'static str, bool)],
) -> ModularResult<Ring> {
    assert_eq!(below.ids.len(), steps.len());
    let mut ids = Vec::with_capacity(steps.len());
    let mut is_corner = Vec::with_capacity(steps.len());
    for (i, &id) in below.ids.iter().enumerate() {
        let next = assembly.mate(id, did("up"), pid(steps[i].0), did("down"))?;
        ids.push(next);
        is_corner.push(steps[i].1);
    }
    let ring = Ring { ids, is_corner };
    join_ring(assembly, &ring)?;
    Ok(ring)
}

fn plinths(assembly: &mut Assembly<'_>, ground: &Ring) -> ModularResult<()> {
    for &id in &ground.ids {
        assembly.mate(id, did("down"), pid("plinth"), did("up"))?;
    }
    Ok(())
}

fn cap_with_towers(assembly: &mut Assembly<'_>, top: &Ring, tower_extra: u32) -> ModularResult<()> {
    if tower_extra == 0 {
        for &id in &top.ids {
            assembly.mate(id, did("up"), pid("battlement"), did("down"))?;
        }
        return Ok(());
    }
    let corners = top.corner_indices();
    for (i, &id) in top.ids.iter().enumerate() {
        if corners.contains(&i) {
            continue;
        }
        assembly.mate(id, did("up"), pid("battlement"), did("down"))?;
    }
    let mut heads = corners.map(|i| top.ids[i]);
    for _ in 0..tower_extra {
        for head in &mut heads {
            *head = assembly.mate(*head, did("up"), pid("turret"), did("down"))?;
        }
    }
    for head in heads {
        assembly.mate(head, did("up"), pid("battlement"), did("down"))?;
    }
    Ok(())
}

fn enclosed_ward(
    assembly: &mut Assembly<'_>,
    origin: Cell,
    width: i32,
    depth: i32,
    storeys: u32,
    tower_extra: u32,
    gates: &[Side],
    loops: bool,
) -> ModularResult<()> {
    assert!(storeys >= 1, "ward needs at least one storey");
    let ground_steps = perimeter(width, depth, gates, loops);
    let upper_steps = perimeter(width, depth, &[], loops);
    let ground = place_ring(assembly, origin, &ground_steps)?;
    plinths(assembly, &ground)?;
    let mut top = ground;
    for _ in 1..storeys {
        top = stack_ring(assembly, &top, &upper_steps)?;
    }
    cap_with_towers(assembly, &top, tower_extra)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_castle_recipe_closes_and_matches_the_catalog() {
        let catalog = catalog();
        for id in RECIPE_IDS {
            let spec = super::super::catalog::spec_for(id).expect("catalog");
            assert!(spec.is_castle());
            let layout = castle::layout_for(id).expect("layout");
            let assembly = assemble_keep_and_curtain(&catalog, layout).unwrap();
            let open = assembly.unmated_seams().unwrap();
            assert!(open.is_empty(), "{id} free seams: {open:?}");
            let cells = assembly.occupied_cells();
            let min_x = cells.iter().map(|c| c.x).min().expect("occupancy");
            let max_x = cells.iter().map(|c| c.x).max().expect("occupancy");
            let min_z = cells.iter().map(|c| c.z).min().expect("occupancy");
            let max_z = cells.iter().map(|c| c.z).max().expect("occupancy");
            let size_x = (max_x - min_x + 1) as f32 * catalog.pitch().xz;
            let size_z = (max_z - min_z + 1) as f32 * catalog.pitch().xz;
            assert!(
                (size_x - spec.size_x).abs() < 1e-4 && (size_z - spec.size_z).abs() < 1e-4,
                "{id} occupancy {size_x}×{size_z} vs catalog {}×{}",
                spec.size_x,
                spec.size_z
            );
        }
    }

    #[test]
    fn village_keep_opens_at_grade() {
        let catalog = catalog();
        let layout = castle::layout_for("castle_keep_8x6").expect("layout");
        let assembly = assemble_keep_and_curtain(&catalog, layout).unwrap();
        let gates: Vec<_> = assembly
            .places()
            .unwrap()
            .into_iter()
            .filter(|item| item.piece.as_str() == "gate")
            .collect();
        assert_eq!(gates.len(), 2, "bailey and keep each need a ground gate");
        for gate in &gates {
            assert!(
                gate.place.position.y.abs() < 0.01,
                "gate must sit at grade, not on an upper storey: {:?}",
                gate.place.position
            );
        }
    }
}
