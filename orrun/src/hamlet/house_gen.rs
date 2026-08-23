//! Generative medieval houses from Modular `catalogs/medieval.json`.
//!
//! Builds a closed ring for any supported footprint, stacks a jetty storey when
//! asked, and hangs a door leaf outside Modular occupancy.

use engine::place::Place;
use glam::Vec3;
use modular::prelude::*;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use super::dwelling::{DwellingBrief, HouseTheme, FOOTPRINTS};
use super::kit::KitError;

const WALL_THICKNESS: f32 = 0.28;
const OVERLAP: f32 = 0.02;
const JAMB_PROUD: f32 = 0.02;
const HINGE_INSET: f32 = 0.01;
const THRESHOLD_TOP: f32 = OVERLAP + 0.012 + 0.015 + 0.08;

fn pid(id: &str) -> PieceId {
    PieceId::new(id).unwrap_or_else(|err| panic!("{err}"))
}

fn did(id: &str) -> DockId {
    DockId::new(id).unwrap_or_else(|err| panic!("{err}"))
}

fn fid(id: &str) -> FamilyId {
    FamilyId::new(id).unwrap_or_else(|err| panic!("{err}"))
}

fn pick(catalog: &Catalog, family: &str, theme: HouseTheme, rng: &mut StdRng) -> PieceId {
    let _ = theme; // `Any` — later themes filter family members.
    catalog
        .pick_family(&fid(family), rng)
        .unwrap_or_else(|err| panic!("{err}"))
}

/// Assemble a village dwelling for `brief`. Deterministic for a fixed seed.
pub fn generate(
    catalog: &Catalog,
    brief: DwellingBrief,
    seed: u64,
) -> Result<Vec<PlacedMesh>, KitError> {
    brief.validate();
    if !FOOTPRINTS.contains(&(brief.cells_x, brief.cells_z)) {
        panic!(
            "dwelling footprint {}×{} is not in the village table",
            brief.cells_x, brief.cells_z
        );
    }
    let mut rng = StdRng::seed_from_u64(seed);
    let assembly = assemble(catalog, brief, &mut rng)?;
    let open = assembly.unmated_seams()?;
    if !open.is_empty() {
        panic!(
            "dwelling {}×{}×{} has free seams (generator bug): {open:?}",
            brief.cells_x, brief.cells_z, brief.storeys
        );
    }
    let origin = footprint_origin(catalog, &assembly);
    let mut places = assembly.places()?;
    places.extend(door_leaf_meshes(catalog, &assembly, &mut rng));
    shift_xz(&mut places, origin);
    Ok(places)
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

fn assemble<'a>(
    catalog: &'a Catalog,
    brief: DwellingBrief,
    rng: &mut StdRng,
) -> ModularResult<Assembly<'a>> {
    let mut assembly = Assembly::new(catalog);
    let (ground, roles) = build_ring(
        &mut assembly,
        catalog,
        brief.theme,
        rng,
        Cell::new(0, 0, 0),
        brief.cells_x,
        brief.cells_z,
        false,
    )?;
    let plinth = pick(catalog, "plinth", brief.theme, rng);
    plinths(&mut assembly, &ground, plinth.as_str())?;

    let top = if brief.storeys >= 2 {
        let upper = stack_jetty(&mut assembly, catalog, brief.theme, rng, &ground, &roles)?;
        Some(upper)
    } else {
        None
    };
    let roof_ring = top.as_deref().unwrap_or(&ground);
    let roof = pick(catalog, "roof", brief.theme, rng);
    let chimney = pick(catalog, "chimney", brief.theme, rng);
    let door_index = roles
        .iter()
        .position(|r| *r == RingRole::Door)
        .unwrap_or_else(|| panic!("ring has no door"));
    let chimney_at = if rng.gen_bool(0.65) {
        let mut choices: Vec<usize> = (0..roof_ring.len()).filter(|&i| i != door_index).collect();
        if choices.is_empty() {
            None
        } else {
            Some(choices.swap_remove(rng.gen_range(0..choices.len())))
        }
    } else {
        None
    };
    roofs(
        &mut assembly,
        roof_ring,
        roof.as_str(),
        chimney.as_str(),
        chimney_at,
    )?;

    if brief.cells_x >= 3 && brief.cells_z >= 3 {
        let floor = pick(catalog, "floor", brief.theme, rng);
        fill_interior(
            &mut assembly,
            brief,
            floor.as_str(),
            plinth.as_str(),
            roof.as_str(),
        )?;
    }
    Ok(assembly)
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

fn build_ring(
    assembly: &mut Assembly<'_>,
    catalog: &Catalog,
    theme: HouseTheme,
    rng: &mut StdRng,
    origin: Cell,
    cells_x: u8,
    cells_z: u8,
    jetty: bool,
) -> ModularResult<(Vec<InstanceId>, Vec<RingRole>)> {
    let corner_fam = if jetty { "corner_jetty" } else { "corner" };
    let straight_fam = if jetty { "straight_jetty" } else { "straight" };
    let south_mids = usize::from(cells_x) - 2;
    let east_mids = usize::from(cells_z) - 2;
    let north_mids = south_mids;
    let west_mids = east_mids;
    let door_slot = south_mids / 2;

    let mut roles = Vec::new();
    roles.push(RingRole::Corner);
    for i in 0..south_mids {
        roles.push(if !jetty && i == door_slot {
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
    for _ in 0..north_mids {
        roles.push(RingRole::Straight);
    }
    roles.push(RingRole::Corner);
    for _ in 0..west_mids {
        roles.push(RingRole::Straight);
    }

    let piece_for = |role: RingRole, rng: &mut StdRng| -> PieceId {
        match role {
            RingRole::Corner => pick(catalog, corner_fam, theme, rng),
            RingRole::Straight => pick(catalog, straight_fam, theme, rng),
            RingRole::Door => pick(catalog, "door", theme, rng),
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

fn join_ring(
    assembly: &mut Assembly<'_>,
    ring: &[InstanceId],
    roles: &[RingRole],
) -> ModularResult<()> {
    assert_eq!(ring.len(), roles.len());
    for i in 0..ring.len() {
        let next = (i + 1) % ring.len();
        assembly.join(ring[i], did("pos_x"), ring[next], next_in_dock(roles[next]))?;
    }
    Ok(())
}

fn stack_jetty(
    assembly: &mut Assembly<'_>,
    catalog: &Catalog,
    theme: HouseTheme,
    rng: &mut StdRng,
    ground: &[InstanceId],
    roles: &[RingRole],
) -> ModularResult<Vec<InstanceId>> {
    let mut upper = Vec::with_capacity(ground.len());
    for (i, &g) in ground.iter().enumerate() {
        let family = match roles[i] {
            RingRole::Corner => "corner_jetty",
            RingRole::Straight | RingRole::Door => "straight_jetty",
        };
        let piece = pick(catalog, family, theme, rng);
        upper.push(assembly.mate(g, did("up"), piece, did("down"))?);
    }
    // Upper ring has no door opening — treat former door slots as straight for joins.
    let upper_roles: Vec<RingRole> = roles
        .iter()
        .map(|r| match r {
            RingRole::Door => RingRole::Straight,
            other => *other,
        })
        .collect();
    join_ring(assembly, &upper, &upper_roles)?;
    Ok(upper)
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
    chimney: &str,
    chimney_at: Option<usize>,
) -> ModularResult<()> {
    for (index, &id) in walls.iter().enumerate() {
        let cap = if Some(index) == chimney_at {
            chimney
        } else {
            roof
        };
        assembly.mate(id, did("up"), pid(cap), did("down"))?;
    }
    Ok(())
}

fn fill_interior(
    assembly: &mut Assembly<'_>,
    brief: DwellingBrief,
    floor: &str,
    plinth: &str,
    roof: &str,
) -> ModularResult<()> {
    let max_x = i32::from(brief.cells_x) - 2;
    let max_z = i32::from(brief.cells_z) - 2;
    for x in 1..=max_x {
        for z in 1..=max_z {
            let ground = assembly.place(pid(floor), Cell::new(x, 0, z), YawQuarter::Deg0)?;
            assembly.mate(ground, did("down"), pid(plinth), did("up"))?;
            if brief.storeys >= 2 {
                let upper = assembly.mate(ground, did("up"), pid(floor), did("down"))?;
                assembly.mate(upper, did("up"), pid(roof), did("down"))?;
            } else {
                assembly.mate(ground, did("up"), pid(roof), did("down"))?;
            }
        }
    }
    Ok(())
}

fn door_opening_width(piece: &str) -> f32 {
    match piece {
        "door" => 1.1,
        "door_b" => 1.05,
        other => panic!("door family piece '{other}' has no opening width"),
    }
}

fn rotate_yaw(v: Vec3, yaw: YawQuarter) -> Vec3 {
    match yaw {
        YawQuarter::Deg0 => v,
        YawQuarter::Deg90 => Vec3::new(v.z, v.y, -v.x),
        YawQuarter::Deg180 => Vec3::new(-v.x, v.y, -v.z),
        YawQuarter::Deg270 => Vec3::new(-v.z, v.y, v.x),
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
    rng: &mut StdRng,
) -> Vec<PlacedMesh> {
    let leaf = if rng.gen_bool(0.5) {
        pid("door_plank")
    } else {
        pid("door_sturdy")
    };
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
        panic!("generated dwelling has no door piece to hang a leaf on");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hamlet::kit;

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

    #[test]
    fn every_footprint_and_storey_closes() {
        let catalog = kit::catalog();
        for &(cx, cz) in FOOTPRINTS {
            for storeys in [1_u8, 2] {
                for seed in [0_u64, 3, 11] {
                    let brief = DwellingBrief::new(cx, cz, storeys, HouseTheme::Any);
                    let places = generate(&catalog, brief, seed).unwrap();
                    assert!(
                        places.iter().any(|p| {
                            let s = p.piece.as_str();
                            s == "door_plank" || s == "door_sturdy"
                        }),
                        "{brief:?} seed {seed} missing door leaf"
                    );
                    let mut rng = StdRng::seed_from_u64(seed);
                    let assembly = assemble(&catalog, brief, &mut rng).unwrap();
                    let open = assembly.unmated_seams().unwrap();
                    assert!(open.is_empty(), "{brief:?} free seams: {open:?}");
                    let (sx, sz) = plan_size(&assembly, catalog.pitch());
                    assert!(
                        (sx - brief.size_x()).abs() < 1e-4 && (sz - brief.size_z()).abs() < 1e-4,
                        "{brief:?} occupancy {sx}×{sz}"
                    );
                }
            }
        }
    }

    #[test]
    fn same_seed_is_stable() {
        let catalog = kit::catalog();
        let brief = DwellingBrief::new(4, 3, 2, HouseTheme::Any);
        let a = generate(&catalog, brief, 9).unwrap();
        let b = generate(&catalog, brief, 9).unwrap();
        assert_eq!(a.len(), b.len());
        for (pa, pb) in a.iter().zip(b.iter()) {
            assert_eq!(pa.piece, pb.piece);
            assert!((pa.place.position - pb.place.position).length() < 1e-5);
        }
    }
}
