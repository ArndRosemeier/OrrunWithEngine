//! Indoor kit: structure from Modular, furniture as post-assembly places.
//!
//! Walls stay generated to the catalog grid. Floors and furniture are Asset Lab
//! meshes (`furn_*`), seated against the plaster — never mated into a wall cell.

use std::path::{Path, PathBuf};

use engine::color::Color;
use engine::mesh::Mesh;
use engine::model::Model;
use engine::place::Place;
use glam::Vec3;
use modular::prelude::*;
use rand::rngs::StdRng;
use rand::SeedableRng;

use super::dwelling::{DwellingBrief, FOOTPRINTS};

const INDOOR_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../Modular/catalogs/indoor.json"
));

pub(crate) const CELL_M: f32 = 4.0;
const STOREY_M: f32 = 2.7;
pub(crate) const WALL_T: f32 = 0.28;
pub(crate) const WALL_CENTER_Z: f32 = -(CELL_M * 0.5 - WALL_T * 0.5);
pub(crate) const DOORWAY_M: f32 = 1.16;
const OPENING_CENTER_Y: f32 = 1.10;

fn plaster_color() -> Color {
    Color::rgb(232, 214, 186)
}
fn plaster_b_color() -> Color {
    Color::rgb(220, 200, 176)
}
fn ceiling_color() -> Color {
    Color::rgb(214, 198, 176)
}
fn wood_color() -> Color {
    Color::rgb(89, 56, 31)
}

/// Piece id → vendored indoor glb. Floor and plinth share the planked cell.
const INDOOR_GLBS: &[(&str, &str)] = &[
    ("floor", "furn_floor.glb"),
    ("plinth", "furn_floor.glb"),
    ("table", "furn_table.glb"),
    ("bed", "furn_bed.glb"),
    ("chest", "furn_chest.glb"),
    ("hearth", "furn_hearth.glb"),
    ("shelf", "furn_shelf.glb"),
    ("cupboard", "furn_cupboard.glb"),
    ("bench", "furn_bench.glb"),
];

/// Footprint half-extents for the shipped furniture meshes (metres).
pub fn furniture_half_xz(piece: &str) -> (f32, f32) {
    match piece {
        "table" => (0.68, 0.41),
        "bed" => (1.025, 0.59),
        "chest" => (0.45, 0.25),
        "hearth" => (0.75, 0.31),
        "shelf" => (0.775, 0.13),
        "cupboard" => (0.54, 0.23),
        "bench" => (0.69, 0.18),
        other => panic!("indoor furniture has no collider for '{other}'"),
    }
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

pub fn catalog() -> Catalog {
    Catalog::from_json(INDOOR_JSON).expect("Modular catalogs/indoor.json")
}

/// One indoor layout: kit pieces plus furniture that is not occupancy.
#[derive(Clone, Debug)]
pub struct InteriorLayout {
    pub pieces: Vec<PlacedMesh>,
    pub furniture: Vec<PlacedMesh>,
    pub storeys: u32,
    pub stair_local: Option<Vec3>,
    /// Portal pose on the authored door wall, in house-local metres.
    pub door_local: Place,
}

/// Assemble the indoor kit and decorate it. Loud if seams stay open.
pub fn assemble(brief: DwellingBrief, seed: u64) -> InteriorLayout {
    brief.validate();
    if !FOOTPRINTS.contains(&(brief.cells_x, brief.cells_z)) {
        panic!(
            "indoor kit has no layout for {}×{}",
            brief.cells_x, brief.cells_z
        );
    }
    let catalog = catalog();
    let mut rng = StdRng::seed_from_u64(seed ^ 0x1D00_1100);
    let (assembly, storeys, stair_cell) = assemble_for(&catalog, brief, &mut rng);
    let open = assembly
        .unmated_seams()
        .unwrap_or_else(|err| panic!("{} indoor seams: {err}", brief.label()));
    if !open.is_empty() {
        panic!("{} indoor kit has free seams: {open:?}", brief.label());
    }
    let origin = footprint_shift(&catalog, &assembly);
    let mut pieces = assembly
        .places()
        .unwrap_or_else(|err| panic!("{} indoor places: {err}", brief.label()));
    shift_xz(&mut pieces, origin);
    let door_local = pieces
        .iter()
        .find(|item| item.piece.as_str() == "door")
        .map(|item| opening_place(&item.place))
        .unwrap_or_else(|| panic!("{} indoor kit has no exterior door", brief.label()));
    if storeys > 1 {
        pieces.extend(cover_upper_floors(brief, origin, &pieces, stair_cell));
    }
    let stair_local = stair_cell.map(|c| {
        Vec3::new(
            (c.x as f32 + 0.5) * CELL_M + origin.x,
            c.y as f32 * STOREY_M,
            (c.z as f32 + 0.5) * CELL_M + origin.z,
        )
    });
    let furniture = decorate(brief, seed, storeys, stair_local);
    let stair_local = stair_local.or_else(|| {
        furniture
            .iter()
            .find(|p| p.piece.as_str() == "stair")
            .map(|p| p.place.position)
    });
    InteriorLayout {
        pieces,
        furniture,
        storeys,
        stair_local,
        door_local,
    }
}

/// Mesh for an indoor piece id. Floors and furniture are Asset Lab glbs.
pub fn piece_mesh(piece: &str) -> Mesh {
    if let Some(file) = indoor_glb(piece) {
        return load_indoor_glb(piece, file);
    }
    match piece {
        "wall" | "wall_b" => slab(
            (0.0, STOREY_M * 0.5, WALL_CENTER_Z),
            (CELL_M, STOREY_M, WALL_T),
            plaster(piece),
        ),
        "partition" => slab(
            (0.0, STOREY_M * 0.5, 0.0),
            (CELL_M, STOREY_M, WALL_T),
            plaster(piece),
        ),
        "door" => door_wall(WALL_CENTER_Z, plaster(piece)),
        "partition_door" => door_wall(0.0, plaster(piece)),
        "corner" => corner_wall(plaster_color()),
        "ceiling" => slab(
            (0.0, STOREY_M - 0.04, 0.0),
            (CELL_M, 0.08, CELL_M),
            ceiling_color(),
        ),
        "stair" => stair_mesh(),
        other => panic!("indoor kit has no mesh for '{other}'"),
    }
}

fn indoor_glb(piece: &str) -> Option<&'static str> {
    INDOOR_GLBS
        .iter()
        .find(|(id, _)| *id == piece)
        .map(|(_, file)| *file)
}

fn indoor_search_paths(file: &str) -> Vec<PathBuf> {
    let mut tried = Vec::new();
    if let Some(dir) = std::env::var_os("ORRUN_ASSETS") {
        tried.push(PathBuf::from(dir).join("kit").join("indoor").join(file));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            tried.push(dir.join("assets").join("kit").join("indoor").join(file));
        }
    }
    tried.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join("kit")
            .join("indoor")
            .join(file),
    );
    tried.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("AssetGenerator")
            .join("assets")
            .join("out")
            .join(file),
    );
    tried
}

fn load_indoor_glb(piece: &str, file: &str) -> Mesh {
    let tried = indoor_search_paths(file);
    let path = tried.iter().find(|p| p.is_file()).cloned().unwrap_or_else(|| {
        panic!(
            "indoor mesh '{piece}' ({file}) not found (tried {}). From C:\\Projekte\\AssetGenerator run: python tools/ag.py generate {} then python tools/sync_props.py",
            tried
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", "),
            Path::new(file)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| file.to_string()),
        )
    });
    let base = path.parent().unwrap_or_else(|| Path::new("."));
    Model::load_with(&path, base, &engine::EngineLimits::default()).unwrap_or_else(|err| {
        panic!(
            "indoor mesh '{}' failed to load from {}: {err}",
            path.display(),
            piece
        )
    })
}

fn plaster(piece: &str) -> Color {
    if piece.ends_with("_b") {
        plaster_b_color()
    } else {
        plaster_color()
    }
}

fn slab(center: (f32, f32, f32), size: (f32, f32, f32), color: Color) -> Mesh {
    Mesh::box_at(center, size, color).expect("indoor box")
}

fn door_wall(z: f32, color: Color) -> Mesh {
    let mut mesh = Mesh::new();
    let side = (CELL_M - DOORWAY_M) * 0.5;
    mesh.add_box(
        (-CELL_M * 0.5 + side * 0.5, STOREY_M * 0.5, z),
        (side, STOREY_M, WALL_T),
        color,
    )
    .expect("door jamb");
    mesh.add_box(
        (CELL_M * 0.5 - side * 0.5, STOREY_M * 0.5, z),
        (side, STOREY_M, WALL_T),
        color,
    )
    .expect("door jamb");
    mesh.add_box(
        (0.0, 2.16 + (STOREY_M - 2.16) * 0.5, z),
        (DOORWAY_M, STOREY_M - 2.16, WALL_T),
        color,
    )
    .expect("door lintel");
    mesh
}

fn corner_wall(color: Color) -> Mesh {
    let mut mesh = Mesh::new();
    mesh.add_box(
        (0.0, STOREY_M * 0.5, WALL_CENTER_Z),
        (CELL_M, STOREY_M, WALL_T),
        color,
    )
    .expect("corner south wall");
    mesh.add_box(
        (WALL_CENTER_Z, STOREY_M * 0.5, 0.0),
        (WALL_T, STOREY_M, CELL_M),
        color,
    )
    .expect("corner west wall");
    mesh
}

fn opening_place(host: &Place) -> Place {
    let (dx, dz) = rotate_xz(0.0, WALL_CENTER_Z, host.yaw_degrees);
    Place::new(
        host.position.x + dx,
        host.position.y + OPENING_CENTER_Y,
        host.position.z + dz,
    )
    .with_yaw_deg(host.yaw_degrees)
}

pub(crate) fn rotate_xz(x: f32, z: f32, yaw_deg: f32) -> (f32, f32) {
    let radians = yaw_deg.to_radians();
    let (sin, cos) = (radians.sin(), radians.cos());
    (x * cos + z * sin, -x * sin + z * cos)
}

fn stair_mesh() -> Mesh {
    let mut mesh = Mesh::new();
    let steps = 8;
    for i in 0..steps {
        let t = (i as f32 + 0.5) / steps as f32;
        let y = t * STOREY_M;
        let z = (t - 0.5) * CELL_M;
        mesh.add_box(
            (0.0, y * 0.5, z),
            (1.4, y.max(0.12), CELL_M / steps as f32),
            wood_color(),
        )
        .expect("stair step");
    }
    mesh
}

fn assemble_for<'a>(
    catalog: &'a Catalog,
    brief: DwellingBrief,
    rng: &mut StdRng,
) -> (Assembly<'a>, u32, Option<Cell>) {
    let mut assembly = Assembly::new(catalog);
    let (ground, roles) = build_indoor_ring(
        &mut assembly,
        catalog,
        rng,
        Cell::new(0, 0, 0),
        brief.cells_x,
        brief.cells_z,
    )
    .expect("indoor ring");
    cap_down(&mut assembly, &ground, "plinth").expect("indoor plinth");
    let storeys = u32::from(brief.storeys);
    let stair_cell = if brief.storeys >= 2 && brief.cells_x >= 3 && brief.cells_z >= 3 {
        Some(Cell::new(1, 0, i32::from(brief.cells_z) - 2))
    } else {
        None
    };
    if let Some(stair_cell) = stair_cell {
        let stair = assembly
            .place(pid("stair"), stair_cell, YawQuarter::Deg0)
            .expect("indoor stair");
        assembly
            .mate(stair, did("down"), pid("plinth"), did("up"))
            .expect("stair plinth");
        fill_indoor_floors(&mut assembly, brief, stair_cell).expect("indoor floors");
        let upper = stack_indoor_ring(&mut assembly, catalog, rng, &ground, &roles)
            .expect("indoor upper");
        cap_up(&mut assembly, &upper, "ceiling").expect("indoor ceiling");
        assembly
            .mate(stair, did("up"), pid("ceiling"), did("down"))
            .expect("stair well cap");
        (assembly, storeys, Some(stair_cell))
    } else if brief.storeys >= 2 {
        let upper = stack_indoor_ring(&mut assembly, catalog, rng, &ground, &roles)
            .expect("indoor upper");
        cap_up(&mut assembly, &upper, "ceiling").expect("indoor ceiling");
        (assembly, storeys, None)
    } else {
        cap_up(&mut assembly, &ground, "ceiling").expect("indoor ceiling");
        (assembly, storeys, None)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RingRole {
    Corner,
    Straight,
    Door,
}

fn next_in_dock(role: RingRole) -> DockId {
    match role {
        RingRole::Corner => did("pos_z"),
        RingRole::Straight | RingRole::Door => did("neg_x"),
    }
}

fn build_indoor_ring(
    assembly: &mut Assembly<'_>,
    catalog: &Catalog,
    rng: &mut StdRng,
    origin: Cell,
    cells_x: u8,
    cells_z: u8,
) -> ModularResult<(Vec<InstanceId>, Vec<RingRole>)> {
    let south_mids = usize::from(cells_x) - 2;
    let east_mids = usize::from(cells_z) - 2;
    let door_slot = south_mids / 2;
    let mut roles = Vec::new();
    roles.push(RingRole::Corner);
    for i in 0..south_mids {
        roles.push(if i == door_slot {
            RingRole::Door
        } else {
            RingRole::Straight
        });
    }
    roles.push(RingRole::Corner);
    for _ in 0..east_mids {
        roles.push(RingRole::Straight);
    }
    roles.push(RingRole::Corner);
    for _ in 0..south_mids {
        roles.push(RingRole::Straight);
    }
    roles.push(RingRole::Corner);
    for _ in 0..east_mids {
        roles.push(RingRole::Straight);
    }

    let piece_for = |role: RingRole, rng: &mut StdRng| -> PieceId {
        match role {
            RingRole::Corner => pick(catalog, "corner", rng),
            RingRole::Straight => pick(catalog, "straight", rng),
            RingRole::Door => pick(catalog, "door", rng),
        }
    };
    let first = piece_for(roles[0], rng);
    let mut ids = Vec::with_capacity(roles.len());
    ids.push(assembly.place(first, origin, YawQuarter::Deg0)?);
    for i in 1..roles.len() {
        let piece = piece_for(roles[i], rng);
        let id = assembly.mate(ids[i - 1], did("pos_x"), piece, next_in_dock(roles[i]))?;
        ids.push(id);
    }
    assembly.join(
        *ids.last().expect("ring"),
        did("pos_x"),
        ids[0],
        did("pos_z"),
    )?;
    Ok((ids, roles))
}

fn stack_indoor_ring(
    assembly: &mut Assembly<'_>,
    catalog: &Catalog,
    rng: &mut StdRng,
    ground: &[InstanceId],
    roles: &[RingRole],
) -> ModularResult<Vec<InstanceId>> {
    let mut upper = Vec::with_capacity(ground.len());
    for (i, &g) in ground.iter().enumerate() {
        let family = match roles[i] {
            RingRole::Corner => "corner",
            RingRole::Straight | RingRole::Door => "straight",
        };
        let piece = pick(catalog, family, rng);
        upper.push(assembly.mate(g, did("up"), piece, did("down"))?);
    }
    let upper_roles: Vec<RingRole> = roles
        .iter()
        .map(|r| match r {
            RingRole::Door => RingRole::Straight,
            other => *other,
        })
        .collect();
    for i in 0..upper.len() {
        let next = (i + 1) % upper.len();
        assembly.join(
            upper[i],
            did("pos_x"),
            upper[next],
            next_in_dock(upper_roles[next]),
        )?;
    }
    Ok(upper)
}

fn fill_indoor_floors(
    assembly: &mut Assembly<'_>,
    brief: DwellingBrief,
    stair_cell: Cell,
) -> ModularResult<()> {
    let max_x = i32::from(brief.cells_x) - 2;
    let max_z = i32::from(brief.cells_z) - 2;
    for x in 1..=max_x {
        for z in 1..=max_z {
            if x == stair_cell.x && z == stair_cell.z {
                continue;
            }
            let ground = assembly.place(pid("floor"), Cell::new(x, 0, z), YawQuarter::Deg0)?;
            assembly.mate(ground, did("down"), pid("plinth"), did("up"))?;
            let upper = assembly.mate(ground, did("up"), pid("floor"), did("down"))?;
            assembly.mate(upper, did("up"), pid("ceiling"), did("down"))?;
        }
    }
    Ok(())
}

fn cap_down(assembly: &mut Assembly<'_>, ring: &[InstanceId], plinth: &str) -> ModularResult<()> {
    for &id in ring {
        assembly.mate(id, did("down"), pid(plinth), did("up"))?;
    }
    Ok(())
}

fn cap_up(assembly: &mut Assembly<'_>, ring: &[InstanceId], ceiling: &str) -> ModularResult<()> {
    for &id in ring {
        assembly.mate(id, did("up"), pid(ceiling), did("down"))?;
    }
    Ok(())
}

fn footprint_shift(catalog: &Catalog, assembly: &Assembly<'_>) -> Vec3 {
    let cells = assembly.occupied_cells();
    let min_x = cells.iter().map(|c| c.x).min().expect("occupancy") as f32;
    let max_x = cells.iter().map(|c| c.x).max().expect("occupancy") as f32;
    let min_z = cells.iter().map(|c| c.z).min().expect("occupancy") as f32;
    let max_z = cells.iter().map(|c| c.z).max().expect("occupancy") as f32;
    let _ = catalog;
    Vec3::new(
        -(min_x + max_x + 1.0) * 0.5 * CELL_M,
        0.0,
        -(min_z + max_z + 1.0) * 0.5 * CELL_M,
    )
}

fn shift_xz(places: &mut [PlacedMesh], origin: Vec3) {
    for item in places {
        item.place.position.x += origin.x;
        item.place.position.z += origin.z;
    }
}

fn cover_upper_floors(
    brief: DwellingBrief,
    origin: Vec3,
    existing: &[PlacedMesh],
    stair_cell: Option<Cell>,
) -> Vec<PlacedMesh> {
    let cells_x = i32::from(brief.cells_x);
    let cells_z = i32::from(brief.cells_z);
    let mut out = Vec::new();
    for x in 0..cells_x {
        for z in 0..cells_z {
            if stair_cell.is_some_and(|c| c.x == x && c.z == z) {
                continue;
            }
            let px = (x as f32 + 0.5) * CELL_M + origin.x;
            let pz = (z as f32 + 0.5) * CELL_M + origin.z;
            if existing.iter().any(|item| {
                item.piece.as_str() == "floor"
                    && (item.place.position.x - px).abs() < 0.2
                    && (item.place.position.z - pz).abs() < 0.2
                    && (item.place.position.y - STOREY_M).abs() < 0.2
            }) {
                continue;
            }
            out.push(PlacedMesh {
                piece: pid("floor"),
                place: Place::new(px, STOREY_M, pz),
            });
        }
    }
    out
}

fn room_cells(brief: DwellingBrief) -> (i32, i32) {
    (i32::from(brief.cells_x), i32::from(brief.cells_z))
}

fn room_inner(brief: DwellingBrief) -> (f32, f32) {
    let (cells_x, cells_z) = room_cells(brief);
    (
        cells_x as f32 * CELL_M * 0.5 - WALL_T,
        cells_z as f32 * CELL_M * 0.5 - WALL_T,
    )
}

#[derive(Clone, Copy)]
enum Wall {
    North,
    South,
    East,
    West,
}

/// Furniture sits on finished faces. Door is local −Z; keep that aisle clear.
fn decorate(
    brief: DwellingBrief,
    seed: u64,
    storeys: u32,
    stair_local: Option<Vec3>,
) -> Vec<PlacedMesh> {
    let _ = seed;
    let (inner_x, inner_z) = room_inner(brief);
    let ground = Room {
        inner_x,
        inner_z,
        y: 0.0,
    };
    let mut out = Vec::new();
    let margin_x = (inner_x - 0.8).max(0.5);
    let margin_z = (inner_z - 0.8).max(0.5);
    out.push(ground.against(Wall::North, (-2.4_f32).max(-margin_x), "bed"));
    out.push(ground.against(Wall::North, 2.6_f32.min(margin_x), "shelf"));
    out.push(ground.against(Wall::East, 1.4_f32.min(margin_z), "hearth"));
    out.push(ground.against(Wall::East, (-2.2_f32).max(-margin_z), "chest"));
    out.push(ground.against(Wall::West, 1.3_f32.min(margin_z), "cupboard"));
    out.push(ground.against(Wall::West, (-2.0_f32).max(-margin_z), "shelf"));
    out.push(ground.open_floor("table", 0.9_f32.min(inner_x - 1.0).max(0.0), 0.2));
    out.push(ground.against(Wall::South, 2.5_f32.min(margin_x), "bench"));
    if brief.cells_z >= 4 {
        out.push(ground.against(Wall::North, 0.0, "bench"));
        out.push(ground.against(Wall::South, (-2.5_f32).max(-margin_x), "bench"));
        out.push(ground.against(Wall::East, 3.8_f32.min(margin_z), "bench"));
        if let Some(stair) = stair_local {
            assert!(
                stair.z > -1.2 || stair.x.abs() > 1.4,
                "stair sits in the door aisle"
            );
        }
    }
    if storeys > 1 {
        let upper = Room {
            inner_x,
            inner_z,
            y: STOREY_M,
        };
        out.push(upper.against(Wall::North, (-2.2_f32).max(-margin_x), "bed"));
        out.push(upper.against(Wall::West, 0.4_f32.min(margin_z), "chest"));
        out.push(upper.against(Wall::East, 0.8_f32.min(margin_z), "shelf"));
        out.push(upper.against(Wall::South, 2.5_f32.min(margin_x), "bench"));
        if stair_local.is_none() {
            out.push(furn_at(
                "stair",
                2.2_f32.min(inner_x - 1.0).max(0.5),
                0.0,
                1.6_f32.min(inner_z - 1.0).max(0.5),
                0.0,
            ));
        }
    }
    for item in &out {
        assert_clear_of_door(item);
    }
    out
}

struct Room {
    inner_x: f32,
    inner_z: f32,
    y: f32,
}

impl Room {
    fn against(&self, wall: Wall, along: f32, piece: &str) -> PlacedMesh {
        let (half_x, half_z) = furniture_half_xz(piece);
        let y = if piece == "shelf" {
            self.y + 0.95
        } else {
            self.y
        };
        let (x, z, yaw) = match wall {
            Wall::North => (along, self.inner_z - half_z, 180.0),
            Wall::South => (along, -self.inner_z + half_z, 0.0),
            Wall::East => (self.inner_x - half_z, along, -90.0),
            Wall::West => (-self.inner_x + half_z, along, 90.0),
        };
        let _ = half_x;
        furn_at(piece, x, y, z, yaw)
    }

    fn open_floor(&self, piece: &str, x: f32, z: f32) -> PlacedMesh {
        furn_at(piece, x, self.y, z, 0.0)
    }
}

fn furn_at(piece: &str, x: f32, y: f32, z: f32, yaw: f32) -> PlacedMesh {
    PlacedMesh {
        piece: pid(piece),
        place: Place::new(x, y, z).with_yaw_deg(yaw),
    }
}

fn assert_clear_of_door(item: &PlacedMesh) {
    if item.piece.as_str() == "stair" {
        return;
    }
    let p = item.place.position;
    let in_aisle = p.x.abs() < 1.35 && p.z < -1.2 && p.y < 1.0;
    assert!(
        !in_aisle,
        "{} at ({:.2}, {:.2}, {:.2}) blocks the door aisle",
        item.piece, p.x, p.y, p.z
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hamlet::HouseTheme;

    fn brief(cells_x: u8, cells_z: u8, storeys: u8) -> DwellingBrief {
        DwellingBrief::new(cells_x, cells_z, storeys, HouseTheme::Any)
    }

    #[test]
    fn indoor_layouts_close_their_seams() {
        for brief in [
            brief(3, 2, 1),
            brief(3, 2, 2),
            brief(3, 4, 2),
            brief(4, 3, 1),
            brief(5, 4, 2),
        ] {
            let layout = assemble(brief, 7);
            assert!(
                !layout.pieces.is_empty(),
                "{} indoor kit placed nothing",
                brief.label()
            );
            assert!(
                layout.furniture.iter().any(|p| p.piece.as_str() == "table"),
                "{} has no table",
                brief.label()
            );
            assert!(
                layout.furniture.iter().any(|p| p.piece.as_str() == "shelf"),
                "{} has no wall shelf",
                brief.label()
            );
            assert!(
                layout
                    .furniture
                    .iter()
                    .any(|p| p.piece.as_str() == "cupboard"),
                "{} has no cupboard",
                brief.label()
            );
            let (inner_x, inner_z) = room_inner(brief);
            for item in &layout.furniture {
                if matches!(item.piece.as_str(), "table" | "stair") {
                    continue;
                }
                let p = item.place.position;
                let on_wall = p.x.abs() > inner_x - 1.1 || p.z.abs() > inner_z - 1.1;
                assert!(
                    on_wall,
                    "{} {} sits in the room middle at {:?}",
                    brief.label(),
                    item.piece,
                    p
                );
            }
        }
    }

    #[test]
    fn planked_floor_replaces_the_stretched_slab() {
        let layout = assemble(brief(3, 2, 1), 7);
        assert!(
            layout
                .pieces
                .iter()
                .any(|p| p.piece.as_str() == "plinth" && p.place.stretch == Vec3::ONE),
            "ground plinths must stay unstretched plank cells"
        );
        assert!(
            layout
                .pieces
                .iter()
                .filter(|p| p.piece.as_str() == "floor")
                .all(|p| p.place.stretch == Vec3::ONE),
            "no stretched floor overlay"
        );
        let floor = piece_mesh("floor");
        assert!(
            floor.point_count() > 24,
            "planked floor should be more than a box, got {} points",
            floor.point_count()
        );
    }

    #[test]
    fn hall_keeps_a_stair_off_the_door() {
        let layout = assemble(brief(3, 4, 2), 3);
        let stair = layout.stair_local.expect("hall stair");
        assert!(
            stair.z > -1.0,
            "hall stair is in the doorway, z={}",
            stair.z
        );
    }

    #[test]
    fn portal_pose_uses_the_authored_door_wall() {
        let layout = assemble(brief(3, 2, 1), 7);
        assert!(layout.door_local.position.x.abs() < 1e-4);
        assert!(
            (layout.door_local.position.z - (-4.0 + WALL_T * 0.5)).abs() < 1e-4,
            "door portal was inset from the wall: z={}",
            layout.door_local.position.z
        );
        assert!((layout.door_local.position.y - OPENING_CENTER_Y).abs() < 1e-4);
        assert!(layout.door_local.yaw_degrees.abs() < 1e-4);
    }
}
