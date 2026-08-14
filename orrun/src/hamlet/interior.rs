//! Indoor kit: structure from Modular, furniture as post-assembly places.
//!
//! Meshes are generated to the catalog grid (Asset Lab can replace them later).
//! Furniture never `mate`s into a wall cell.

use engine::color::Color;
use engine::mesh::Mesh;
use engine::place::Place;
use glam::Vec3;
use modular::prelude::*;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

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
fn floor_color() -> Color {
    Color::rgb(168, 112, 72)
}
fn ceiling_color() -> Color {
    Color::rgb(214, 198, 176)
}
fn wood_color() -> Color {
    Color::rgb(89, 56, 31)
}
fn linen_color() -> Color {
    Color::rgb(196, 178, 142)
}
fn hearth_color() -> Color {
    Color::rgb(70, 64, 58)
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
pub fn assemble(catalog_id: &str, seed: u64) -> InteriorLayout {
    let catalog = catalog();
    let mut rng = StdRng::seed_from_u64(seed ^ 0x1D00_1100);
    let (assembly, storeys, stair_cell) = match catalog_id {
        "house_hut_thatch" => (assemble_hut(&catalog, &mut rng), 1, None),
        "house_cabin_timber" => (assemble_cabin(&catalog, &mut rng), 1, None),
        "house_cottage_stone" => assemble_cottage(&catalog, &mut rng),
        "house_hall_large" => assemble_hall(&catalog, &mut rng),
        other => panic!("'{other}' has no indoor recipe"),
    };
    let open = assembly
        .unmated_seams()
        .unwrap_or_else(|err| panic!("{catalog_id} indoor seams: {err}"));
    if !open.is_empty() {
        panic!("{catalog_id} indoor kit has free seams: {open:?}");
    }
    let origin = footprint_shift(&catalog, &assembly);
    let mut pieces = assembly
        .places()
        .unwrap_or_else(|err| panic!("{catalog_id} indoor places: {err}"));
    shift_xz(&mut pieces, origin);
    let door_local = pieces
        .iter()
        .find(|item| item.piece.as_str() == "door")
        .map(|item| opening_place(&item.place))
        .unwrap_or_else(|| panic!("{catalog_id} indoor kit has no exterior door"));
    let (sx, sz) = if catalog_id == "house_hall_large" {
        (2.8, 3.6)
    } else {
        (2.8, 1.8)
    };
    pieces.push(PlacedMesh {
        piece: pid("floor"),
        place: Place::new(0.0, 0.02, 0.0).with_stretch(Vec3::new(sx, 1.0, sz)),
    });
    if storeys > 1 {
        pieces.push(PlacedMesh {
            piece: pid("floor"),
            place: Place::new(0.0, STOREY_M, 0.0).with_stretch(Vec3::new(sx, 1.0, sz)),
        });
    }
    let stair_local = stair_cell.map(|c| {
        Vec3::new(
            (c.x as f32 + 0.5) * CELL_M + origin.x,
            c.y as f32 * STOREY_M,
            (c.z as f32 + 0.5) * CELL_M + origin.z,
        )
    });
    let furniture = decorate(catalog_id, seed, storeys, stair_local);
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

/// Generated mesh for an indoor piece id. Matches the 4 × 2.7 m cell.
pub fn piece_mesh(piece: &str) -> Mesh {
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
        "floor" => slab((0.0, 0.04, 0.0), (CELL_M, 0.08, CELL_M), floor_color()),
        "ceiling" => slab(
            (0.0, STOREY_M - 0.04, 0.0),
            (CELL_M, 0.08, CELL_M),
            ceiling_color(),
        ),
        "plinth" => slab((0.0, -0.08, 0.0), (CELL_M, 0.16, CELL_M), wood_color()),
        "stair" => stair_mesh(),
        "table" => slab((0.0, 0.38, 0.0), (1.2, 0.76, 0.8), wood_color()),
        "bed" => slab((0.0, 0.28, 0.0), (2.0, 0.56, 1.1), linen_color()),
        "chest" => slab((0.0, 0.28, 0.0), (0.7, 0.56, 0.45), wood_color()),
        "hearth" => slab((0.0, 0.45, 0.0), (1.1, 0.9, 0.55), hearth_color()),
        other => panic!("indoor kit has no mesh for '{other}'"),
    }
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

fn assemble_hut<'a>(catalog: &'a Catalog, rng: &mut StdRng) -> Assembly<'a> {
    let mut assembly = Assembly::new(catalog);
    let ring = ring_3x2(&mut assembly, catalog, rng, Cell::new(0, 0, 0)).expect("hut ring");
    cap_ring(&mut assembly, &ring, "plinth", "ceiling").expect("hut cap");
    assembly
}

fn assemble_cabin<'a>(catalog: &'a Catalog, rng: &mut StdRng) -> Assembly<'a> {
    // 3×2 has no free interior cell for a mated partition. Rooms are furniture.
    assemble_hut(catalog, rng)
}

fn assemble_cottage<'a>(
    catalog: &'a Catalog,
    rng: &mut StdRng,
) -> (Assembly<'a>, u32, Option<Cell>) {
    let mut assembly = Assembly::new(catalog);
    let ground = ring_3x2(&mut assembly, catalog, rng, Cell::new(0, 0, 0)).expect("cottage ground");
    cap_down(&mut assembly, &ground, "plinth").expect("cottage plinth");
    let upper = stack_ring_3x2(&mut assembly, catalog, rng, &ground).expect("cottage upper");
    cap_up(&mut assembly, &upper, "ceiling").expect("cottage ceiling");
    (assembly, 2, None)
}

fn assemble_hall<'a>(catalog: &'a Catalog, rng: &mut StdRng) -> (Assembly<'a>, u32, Option<Cell>) {
    let mut assembly = Assembly::new(catalog);
    let ground = ring_3x4(&mut assembly, catalog, rng, Cell::new(0, 0, 0)).expect("hall ground");
    cap_down(&mut assembly, &ground, "plinth").expect("hall plinth");
    let stair_cell = Cell::new(1, 0, 2);
    let stair = assembly
        .place(pid("stair"), stair_cell, YawQuarter::Deg0)
        .expect("hall stair");
    assembly
        .mate(stair, did("down"), pid("plinth"), did("up"))
        .expect("stair plinth");
    let floor = assembly
        .place(pid("floor"), Cell::new(1, 0, 1), YawQuarter::Deg0)
        .expect("hall floor");
    assembly
        .mate(floor, did("down"), pid("plinth"), did("up"))
        .expect("floor plinth");
    let upper = stack_ring_3x4(&mut assembly, catalog, rng, &ground).expect("hall upper");
    cap_up(&mut assembly, &upper, "ceiling").expect("hall ceiling");
    assembly
        .mate(stair, did("up"), pid("ceiling"), did("down"))
        .expect("stair well cap");
    let upper_floor = assembly
        .mate(floor, did("up"), pid("floor"), did("down"))
        .expect("upper hall floor");
    assembly
        .mate(upper_floor, did("up"), pid("ceiling"), did("down"))
        .expect("upper floor ceiling");
    (assembly, 2, Some(stair_cell))
}

fn ring_3x2(
    assembly: &mut Assembly<'_>,
    catalog: &Catalog,
    rng: &mut StdRng,
    origin: Cell,
) -> ModularResult<[InstanceId; 6]> {
    let corner = pick(catalog, "corner", rng);
    let door = pick(catalog, "door", rng);
    let north = pick(catalog, "straight", rng);
    let sw = assembly.place(corner.clone(), origin, YawQuarter::Deg0)?;
    let south = assembly.mate(sw, did("pos_x"), door, did("neg_x"))?;
    let se = assembly.mate(south, did("pos_x"), corner.clone(), did("pos_z"))?;
    let ne = assembly.mate(se, did("pos_x"), pick(catalog, "corner", rng), did("pos_z"))?;
    let north_id = assembly.mate(ne, did("pos_x"), north, did("neg_x"))?;
    let nw = assembly.mate(
        north_id,
        did("pos_x"),
        pick(catalog, "corner", rng),
        did("pos_z"),
    )?;
    assembly.join(nw, did("pos_x"), sw, did("pos_z"))?;
    Ok([sw, south, se, ne, north_id, nw])
}

fn ring_3x4(
    assembly: &mut Assembly<'_>,
    catalog: &Catalog,
    rng: &mut StdRng,
    origin: Cell,
) -> ModularResult<[InstanceId; 10]> {
    let corner = pick(catalog, "corner", rng);
    let door = pick(catalog, "door", rng);
    let sw = assembly.place(corner.clone(), origin, YawQuarter::Deg0)?;
    let south = assembly.mate(sw, did("pos_x"), door, did("neg_x"))?;
    let se = assembly.mate(south, did("pos_x"), corner.clone(), did("pos_z"))?;
    let e1 = assembly.mate(
        se,
        did("pos_x"),
        pick(catalog, "straight", rng),
        did("neg_x"),
    )?;
    let e2 = assembly.mate(
        e1,
        did("pos_x"),
        pick(catalog, "straight", rng),
        did("neg_x"),
    )?;
    let ne = assembly.mate(e2, did("pos_x"), pick(catalog, "corner", rng), did("pos_z"))?;
    let north = assembly.mate(
        ne,
        did("pos_x"),
        pick(catalog, "straight", rng),
        did("neg_x"),
    )?;
    let nw = assembly.mate(
        north,
        did("pos_x"),
        pick(catalog, "corner", rng),
        did("pos_z"),
    )?;
    let w1 = assembly.mate(
        nw,
        did("pos_x"),
        pick(catalog, "straight", rng),
        did("neg_x"),
    )?;
    let w2 = assembly.mate(
        w1,
        did("pos_x"),
        pick(catalog, "straight", rng),
        did("neg_x"),
    )?;
    assembly.join(w2, did("pos_x"), sw, did("pos_z"))?;
    Ok([sw, south, se, e1, e2, ne, north, nw, w1, w2])
}

fn stack_ring_3x2(
    assembly: &mut Assembly<'_>,
    catalog: &Catalog,
    rng: &mut StdRng,
    ground: &[InstanceId; 6],
) -> ModularResult<[InstanceId; 6]> {
    let mut upper = [ground[0]; 6];
    for (i, &g) in ground.iter().enumerate() {
        let piece = if i == 1 {
            pick(catalog, "straight", rng)
        } else if i == 0 || i == 2 || i == 3 || i == 5 {
            pick(catalog, "corner", rng)
        } else {
            pick(catalog, "straight", rng)
        };
        upper[i] = assembly.mate(g, did("up"), piece, did("down"))?;
    }
    assembly.join(upper[5], did("pos_x"), upper[0], did("pos_z"))?;
    assembly.join(upper[0], did("pos_x"), upper[1], did("neg_x"))?;
    assembly.join(upper[1], did("pos_x"), upper[2], did("pos_z"))?;
    assembly.join(upper[2], did("pos_x"), upper[3], did("pos_z"))?;
    assembly.join(upper[3], did("pos_x"), upper[4], did("neg_x"))?;
    assembly.join(upper[4], did("pos_x"), upper[5], did("pos_z"))?;
    Ok(upper)
}

fn stack_ring_3x4(
    assembly: &mut Assembly<'_>,
    catalog: &Catalog,
    rng: &mut StdRng,
    ground: &[InstanceId; 10],
) -> ModularResult<[InstanceId; 10]> {
    let mut upper = [ground[0]; 10];
    for (i, &g) in ground.iter().enumerate() {
        let piece = if i == 1 {
            pick(catalog, "straight", rng)
        } else if matches!(i, 0 | 2 | 5 | 7) {
            pick(catalog, "corner", rng)
        } else {
            pick(catalog, "straight", rng)
        };
        upper[i] = assembly.mate(g, did("up"), piece, did("down"))?;
    }
    assembly.join(upper[9], did("pos_x"), upper[0], did("pos_z"))?;
    assembly.join(upper[0], did("pos_x"), upper[1], did("neg_x"))?;
    assembly.join(upper[1], did("pos_x"), upper[2], did("pos_z"))?;
    assembly.join(upper[2], did("pos_x"), upper[3], did("neg_x"))?;
    assembly.join(upper[3], did("pos_x"), upper[4], did("neg_x"))?;
    assembly.join(upper[4], did("pos_x"), upper[5], did("pos_z"))?;
    assembly.join(upper[5], did("pos_x"), upper[6], did("neg_x"))?;
    assembly.join(upper[6], did("pos_x"), upper[7], did("pos_z"))?;
    assembly.join(upper[7], did("pos_x"), upper[8], did("neg_x"))?;
    assembly.join(upper[8], did("pos_x"), upper[9], did("neg_x"))?;
    Ok(upper)
}

fn cap_ring(
    assembly: &mut Assembly<'_>,
    ring: &[InstanceId],
    plinth: &str,
    ceiling: &str,
) -> ModularResult<()> {
    cap_down(assembly, ring, plinth)?;
    cap_up(assembly, ring, ceiling)
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

/// Furniture sits on finished faces. Door is local −Z; keep that aisle clear.
fn decorate(
    catalog_id: &str,
    seed: u64,
    storeys: u32,
    stair_local: Option<Vec3>,
) -> Vec<PlacedMesh> {
    let mut rng = StdRng::seed_from_u64(seed ^ 0xF01D_E001);
    let depth = if catalog_id == "house_hall_large" {
        6.0
    } else {
        3.2
    };
    let mut out = Vec::new();
    out.push(furn("table", 0.4 + rng.gen_range(-0.3..0.3), 0.0, 0.6));
    out.push(furn("bed", rng.gen_range(-2.2..-1.4), 0.0, depth - 0.8));
    out.push(furn("chest", 2.0, 0.0, -depth + 1.4));
    out.push(furn(
        "hearth",
        rng.gen_range(1.6..2.3),
        0.0,
        rng.gen_range(0.2..1.1),
    ));
    if storeys > 1 {
        if let Some(stair) = stair_local {
            // Stay off the stair footprint and the door aisle (z < -1).
            assert!(
                stair.z > -1.2 || stair.x.abs() > 1.4,
                "stair sits in the door aisle"
            );
        } else {
            out.push(furn("stair", 2.2, 0.0, 1.6));
        }
    }
    out
}

fn furn(piece: &str, x: f32, y: f32, z: f32) -> PlacedMesh {
    PlacedMesh {
        piece: pid(piece),
        place: Place::new(x, y, z),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indoor_recipes_close_their_seams() {
        for id in [
            "house_hut_thatch",
            "house_cabin_timber",
            "house_cottage_stone",
            "house_hall_large",
        ] {
            let layout = assemble(id, 7);
            assert!(!layout.pieces.is_empty(), "{id} indoor kit placed nothing");
            assert!(
                layout.furniture.iter().any(|p| p.piece.as_str() == "table"),
                "{id} has no table"
            );
            let door_aisle = layout
                .furniture
                .iter()
                .filter(|p| p.piece.as_str() != "chest")
                .all(|p| p.place.position.z > -2.4 || p.place.position.x.abs() > 1.2);
            assert!(door_aisle, "{id} furniture blocks the door aisle");
        }
    }

    #[test]
    fn hall_keeps_a_stair_off_the_door() {
        let layout = assemble("house_hall_large", 3);
        let stair = layout.stair_local.expect("hall stair");
        assert!(
            stair.z > -1.0,
            "hall stair is in the doorway, z={}",
            stair.z
        );
    }

    #[test]
    fn portal_pose_uses_the_authored_door_wall() {
        let layout = assemble("house_hut_thatch", 7);
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
