//! Contracts of the single world authority.
//!
//! Everything here asks the same question from two directions — overlay versus
//! surface, chunk versus chunk, mesh versus contact — because the whole point
//! of the redesign is that those answers cannot differ.

use std::sync::Arc;
use std::time::{Duration, Instant};

use engine::chunk_stream::ChunkBuilder;
use engine::space::{ChunkCoord, GlobalXZ};
use engine::surface::WATER_CLEARANCE;
use engine::world::World;
use glam::Vec2;

use super::hydro_geom::{coast_signed_full, signed_distance_ring, COAST_QUERY_M};
use super::{
    chunk_span, resolve_spawn, AtlasBounds, AtlasCell, ContinentalSurface, EntryError, Locomotion,
    MapPoint, PropClass, ScatterCatalog, SessionState, TerrainChunkBuilder, WalkInput,
    WorldEntryRequest, WorldSession, WorldStream, CHUNK_SAMPLE_M, CHUNK_SPAN_M, MIN_WATER_DEPTH,
};
use crate::atlas::cell_overlay::AtlasCellOverlay;
use crate::atlas::hydro::HydroSink;
use crate::atlas::{ContinentAtlas, CELL_METRES};

#[test]
fn vendored_props_arrive_with_the_colour_the_generator_authored() {
    // Untextured glTF carries its look in the material factor, and the prop
    // pipeline has nothing else to shade with: a mesh that loads grey is a mesh
    // that will stand in the world as a grey stick.
    let catalog = ScatterCatalog::discover().expect("vendored props");
    assert!(catalog.count_of(PropClass::Grass) >= 3);
    assert!(catalog.count_of(PropClass::Tree) >= 3);
    assert!(catalog.count_of(PropClass::Rock) >= 4);

    let tuft = engine::model::Model::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets/props/grass/grass_tuft_lush.glb"),
    )
    .expect("grass tuft loads")
    .build();
    let green = tuft
        .colors
        .iter()
        .all(|c| c.y > c.x + 0.1 && c.y > c.z + 0.1);
    assert!(green, "grass tuft is not green: {:?}", tuft.colors.first());
}

#[test]
#[ignore = "diagnostic: what a distance ring costs to bake"]
fn what_a_distance_ring_costs() {
    let surface = surface_of(3);
    for tier in super::DISTANT {
        let span = engine::space::ChunkSpan::new(tier.span_m).unwrap();
        let builder =
            TerrainChunkBuilder::distant(Arc::clone(&surface), span, tier.sample_m, tier.sink_m);
        let mid = surface.bounds().metres() / 2.0;
        let centre = ChunkCoord::containing(GlobalXZ::at(mid, mid), span);
        let ring = tier.radius;
        let started = Instant::now();
        let mut chunks = 0;
        for dz in -ring..=ring {
            for dx in -ring..=ring {
                let coord = ChunkCoord::new(centre.x + dx, centre.z + dz);
                if builder.build(coord).unwrap().is_some() {
                    chunks += 1;
                }
            }
        }
        eprintln!(
            "tier {:>4} m: {chunks} chunks in {:.0} ms on one thread",
            tier.sample_m,
            started.elapsed().as_secs_f32() * 1000.0,
        );
    }
}

#[test]
#[ignore = "diagnostic: how far the coarse tiers stand off the walked ground"]
fn how_far_the_tiers_disagree() {
    for seed in [3, 20260809] {
        let (_, surface) = world_of(seed, 64);
        for tier in super::DISTANT {
            let mut over = overshoot_samples(&surface, &tier);
            over.sort_by(|a, b| a.total_cmp(b));
            let at = |q: f64| over[((over.len() - 1) as f64 * q) as usize];
            eprintln!(
                "seed {seed} tier {:>3} m: p50={:+.1} p90={:+.1} p99={:+.1} p99.9={:+.1} max={:+.1}",
                tier.sample_m,
                at(0.50),
                at(0.90),
                at(0.99),
                at(0.999),
                at(1.0),
            );
        }
    }
}

#[test]
fn a_distant_tier_stays_under_the_ground_the_player_walks_on() {
    // The tiers overlap rather than meet, so where the walked tier ends two
    // resolutions of the same hillside are drawn on top of each other and the
    // sink decides which one shows. Ordinary ground has to be covered by it;
    // cliffs and gorges cannot be, because a coarse grid does not know they are
    // there, and no sink deep enough to hide those would leave the tier
    // anywhere near the ground it continues.
    for seed in [3, 20260809] {
        for tier in super::DISTANT {
            let mut over = overshoot_samples(&surface_of(seed), &tier);
            over.sort_by(|a, b| a.total_cmp(b));
            let ordinary = over[(over.len() as f64 * 0.99) as usize];
            assert!(
                ordinary <= 0.0,
                "tier at {} m samples rises {ordinary:.1} m above the walked ground across a \
                 hundredth of seed {seed}; its {} m sink is too shallow",
                tier.sample_m,
                tier.sink_m
            );
            // Sunk deeper than it needs to be, the coarse ground drops away in
            // a terrace along the line where the finer tier stops.
            let typical = over[over.len() / 2];
            assert!(
                typical >= -tier.sink_m * 2.0,
                "tier at {} m samples sits {:.1} m below the walked ground on seed {seed}; \
                 its {} m sink is deeper than it has to be",
                tier.sample_m,
                -typical,
                tier.sink_m
            );
        }
    }
}

#[test]
fn the_continent_grows_forests_as_well_as_deserts() {
    // Rainfall used to be noise around a single mean, and every land cell fell
    // in the same band: seed 1 had no forest on it at all, so the world could
    // not have trees no matter what the scatter asked for.
    for seed in [1, 7, 23] {
        let (atlas, surface) = world_of(seed, 96);
        let mut land = 0usize;
        let mut forest = 0usize;
        for c in &atlas.cells {
            match crate::atlas::pack::biome(*c) {
                crate::atlas::Biome::Ocean | crate::atlas::Biome::Lake => continue,
                crate::atlas::Biome::Forest => forest += 1,
                _ => {}
            }
            land += 1;
        }
        let share = forest as f32 / land as f32;
        assert!(
            (0.05..0.60).contains(&share),
            "seed {seed}: forest share {share:.2} of {land} land cells"
        );

        let canopy = surface
            .fields()
            .canopy01
            .iter()
            .cloned()
            .fold(0.0, f32::max);
        assert!(
            canopy > 0.5,
            "seed {seed}: thickest canopy is only {canopy:.2}"
        );
    }
}

fn surface_of(seed: i32) -> Arc<ContinentalSurface> {
    world_of(seed, 64).1
}

fn world_of(seed: i32, size: usize) -> (ContinentAtlas, Arc<ContinentalSurface>) {
    let atlas = ContinentAtlas::generate(seed, size);
    let surface = Arc::new(ContinentalSurface::new(&atlas).expect("canonical surface"));
    (atlas, surface)
}

/// Height of a tier's drawn surface at `p`, sunk as the builder sinks it.
///
/// A tier only samples on its own lattice, so between samples the mesh is the
/// interpolation of the corners — which is what a viewer sees, and what has to
/// stay below the finer ground.
fn tier_height(surface: &ContinentalSurface, p: GlobalXZ, tier: &super::TerrainTier) -> f32 {
    let s = tier.sample_m;
    let (x0, z0) = ((p.x / s).floor() * s, (p.z / s).floor() * s);
    let (tx, tz) = (((p.x - x0) / s) as f32, ((p.z - z0) / s) as f32);
    let at = |dx: f64, dz: f64| {
        surface
            .column(GlobalXZ::at(x0 + dx * s, z0 + dz * s))
            .ground()
    };
    let top = at(0.0, 0.0) + (at(1.0, 0.0) - at(0.0, 0.0)) * tx;
    let bottom = at(0.0, 1.0) + (at(1.0, 1.0) - at(0.0, 1.0)) * tx;
    top + (bottom - top) * tz - tier.sink_m
}

/// How far a distant tier stands above the ground the player walks on, over a
/// lattice of probes across the continent.
///
/// Positive is the direction that shows: a coarse tier standing above the fine
/// one wins the depth test and covers the detailed ground it is supposed to be
/// hiding behind. Some of that is unavoidable — a grid of hundreds of metres
/// cannot know about a gorge — so what matters is the bulk of the distribution,
/// not its tail.
fn overshoot_samples(surface: &ContinentalSurface, tier: &super::TerrainTier) -> Vec<f32> {
    let span = surface.bounds().metres();
    let probe = 200usize;
    let step = span / probe as f64;
    let mut over = Vec::with_capacity(probe * probe);
    for iz in 0..probe {
        for ix in 0..probe {
            // Off-lattice on purpose: on a shared sample the tiers agree, and
            // the error being measured is the one between them.
            let p = GlobalXZ::at((ix as f64 + 0.37) * step, (iz as f64 + 0.61) * step);
            over.push(tier_height(surface, p, tier) - tier_height(surface, p, &super::NEAR));
        }
    }
    over
}

fn river_reach(atlas: &ContinentAtlas) -> Option<Vec2> {
    atlas
        .hydro
        .rivers
        .iter()
        .find(|r| r.points.len() >= 8)
        .map(|r| r.points[r.points.len() / 2])
}

// ---------------------------------------------------------------- coordinates

#[test]
fn chunks_tile_the_atlas_without_gaps_or_overlap() {
    let bounds = AtlasBounds::new(64);
    let span = chunk_span();
    assert_eq!(CELL_METRES as f64 % CHUNK_SPAN_M, 0.0);
    assert_eq!(CHUNK_SPAN_M % CHUNK_SAMPLE_M, 0.0);
    for k in 0..64 {
        let p = GlobalXZ::at(k as f64 * CHUNK_SPAN_M, 0.0);
        let coord = ChunkCoord::containing(p, span);
        assert_eq!(coord.origin(span).x, p.x);
        assert!(bounds.contains_point(p));
    }
}

// ------------------------------------------------------------------- surface

#[test]
fn surface_samples_are_finite_and_deterministic() {
    let (atlas, surface) = world_of(1, 48);
    let span = atlas.size as f64 * CELL_METRES as f64;
    for iz in 0..40 {
        for ix in 0..40 {
            let p = GlobalXZ::at(
                span * (ix as f64 + 0.5) / 40.0,
                span * (iz as f64 + 0.5) / 40.0,
            );
            let a = surface.column(p);
            let b = surface.column(p);
            assert!(a.ground().is_finite(), "non-finite ground at {p:?}");
            assert!(a.wetness().is_finite(), "non-finite wetness at {p:?}");
            assert_eq!(a.ground(), b.ground(), "surface must be deterministic");
            assert_eq!(a.wetness(), b.wetness());
        }
    }
}

#[test]
fn wet_columns_always_carry_real_depth() {
    let (atlas, surface) = world_of(3, 48);
    let mut wet = 0usize;
    let size = atlas.size as i32;
    for az in 0..size {
        for ax in 0..size {
            let p = GlobalXZ::at(
                (ax as f64 + 0.5) * CELL_METRES as f64,
                (az as f64 + 0.5) * CELL_METRES as f64,
            );
            let column = surface.column(p);
            let sample = column.to_sample();
            if !column.is_wet() {
                assert!(!sample.is_wet());
                continue;
            }
            wet += 1;
            let depth = sample.depth();
            assert!(
                depth >= MIN_WATER_DEPTH.max(WATER_CLEARANCE) - 1e-4,
                "wet column only {depth} m deep at {p:?}"
            );
            assert!(depth < 200.0, "sheet floats {depth} m above the bed");
        }
    }
    assert!(wet > 0, "an atlas with an ocean must have wet columns");
}

#[test]
fn the_zero_contour_of_wetness_is_the_shoreline() {
    let (_atlas, surface) = world_of(11, 48);
    let coast = surface.hydro().coasts.first().expect("coast ring").clone();
    let mut centre = Vec2::ZERO;
    for p in &coast.ring {
        centre += *p;
    }
    centre /= coast.ring.len() as f32;
    let start = coast.ring[0];
    let out = (start - centre).normalize_or_zero();

    let mut wet_side = 0usize;
    let mut dry_side = 0usize;
    for i in -20..=20 {
        let p = start + out * (i as f32 * 30.0);
        let column = surface.column(GlobalXZ::at(p.x as f64, p.y as f64));
        if column.is_wet() {
            wet_side += 1;
            assert!(column.wetness() >= 0.0);
        } else {
            dry_side += 1;
            assert!(column.wetness() < 0.0);
        }
    }
    assert!(
        wet_side > 0 && dry_side > 0,
        "a transect through the coast ring must cross the waterline"
    );
}

#[test]
fn rivers_are_wet_along_the_canonical_centreline() {
    let (atlas, surface) = world_of(5, 64);
    let Some(river) = atlas.hydro.rivers.iter().find(|r| r.points.len() >= 8) else {
        return;
    };
    let i = river.points.len() / 2;
    let mid = river.points[i];
    let column = surface.column(GlobalXZ::at(mid.x as f64, mid.y as f64));
    assert!(column.is_wet(), "the canonical channel must hold water");
    let depth = column.to_sample().depth();
    assert!(depth > 2.0, "expected a carved channel, depth={depth}");

    let hit = surface
        .hydro_index()
        .nearest_river(surface.hydro(), mid)
        .expect("river hit");
    let tangent = (river.points[i + 1] - river.points[i - 1]).normalize_or_zero();
    let perp = Vec2::new(-tangent.y, tangent.x);
    let bank = mid + perp * (hit.half_width * 6.0);
    let bank_column = surface.column(GlobalXZ::at(bank.x as f64, bank.y as f64));
    assert!(
        bank_column.ground() > column.ground() + 2.0,
        "the bank must stand above the thalweg"
    );
}

#[test]
fn lake_interiors_sit_at_the_authored_sheet() {
    let (_atlas, surface) = world_of(5, 64);
    let Some(lake) = surface.hydro().lakes.first().cloned() else {
        return;
    };
    let mut centre = Vec2::ZERO;
    for p in &lake.ring {
        centre += *p;
    }
    centre /= lake.ring.len() as f32;
    if signed_distance_ring(centre, &lake.ring) < 40.0 {
        return;
    }
    let column = surface.column(GlobalXZ::at(centre.x as f64, centre.y as f64));
    assert!(column.is_wet(), "lake centroid must be wet");
    let top = column.water_top().expect("wet");
    assert!(
        (top - lake.surface_z).abs() < 1.0,
        "lake sheet {top} drifted from the authored {}",
        lake.surface_z
    );
}

#[test]
fn broken_hydro_is_rejected_at_construction() {
    let mut atlas = ContinentAtlas::generate(2, 48);
    if atlas.hydro.lakes.is_empty() {
        return;
    }
    let mut hydro = (*atlas.hydro).clone();
    hydro.lakes[0].surface_z = hydro.sea_surface_z - 25.0;
    atlas.hydro = Arc::new(hydro);
    assert!(
        ContinentalSurface::new(&atlas).is_err(),
        "a lake below sea level must be a loud error, not a clamp"
    );
}

// -------------------------------------------------------------- atlas overlay

#[test]
fn the_overlay_raster_agrees_with_the_surface() {
    let (atlas, surface) = world_of(3, 48);
    let bounds = AtlasBounds::of(&atlas);
    let Some(mid) = river_reach(&atlas) else {
        return;
    };
    let cell = AtlasCell::new(
        bounds,
        (mid.x / CELL_METRES).floor() as i32,
        (mid.y / CELL_METRES).floor() as i32,
    )
    .expect("river cell");
    let overlay = AtlasCellOverlay::bake(&atlas, &surface, cell);
    let water = overlay.water.expect("a river cell has water");
    let res = water.res;
    let origin = cell.origin();
    let step = CELL_METRES as f64 / res as f64;
    for iz in 0..res {
        for ix in 0..res {
            let p = GlobalXZ::at(
                origin.x + (ix as f64 + 0.5) * step,
                origin.z + (iz as f64 + 0.5) * step,
            );
            assert_eq!(
                water.wet[iz * res + ix],
                surface.column(p).is_wet(),
                "overlay and 3D disagree about water at {p:?}"
            );
        }
    }
}

// --------------------------------------------------------------- chunk meshes

#[test]
fn adjacent_chunks_share_their_edge_exactly() {
    let (atlas, surface) = world_of(1, 48);
    let builder = TerrainChunkBuilder::new(Arc::clone(&surface));
    let verts = builder.verts_per_axis();
    let mid = (atlas.size as f64 * CELL_METRES as f64 * 0.5 / CHUNK_SPAN_M) as i32;
    let left = ChunkCoord::new(mid, mid);
    let right = ChunkCoord::new(mid + 1, mid);

    let a = builder.build(left).expect("build").expect("content");
    let b = builder.build(right).expect("build").expect("content");
    let (a_land, a_water) = split_layers(&a);
    let (b_land, b_water) = split_layers(&b);

    for iz in 0..verts {
        let ai = iz * verts + (verts - 1);
        let bi = iz * verts;
        assert_eq!(
            a_land.positions[ai].y, b_land.positions[bi].y,
            "edge height differs at row {iz}"
        );
        assert_eq!(
            a_land.normals[ai], b_land.normals[bi],
            "edge normal differs at row {iz} (halo not shared?)"
        );
    }

    // Water vertices that land on the shared plane must coincide too.
    let seam_x = CHUNK_SPAN_M as f32;
    let a_seam = seam_points(a_water, |p| (p.x - seam_x).abs() < 1e-4);
    let b_seam = seam_points(b_water, |p| p.x.abs() < 1e-4);
    assert_eq!(
        a_seam.len(),
        b_seam.len(),
        "water contour vertex count differs across the seam"
    );
    for (pa, pb) in a_seam.iter().zip(b_seam.iter()) {
        assert!(
            (pa.0 - pb.0).abs() < 1e-4 && (pa.1 - pb.1).abs() < 1e-4,
            "water contour tore at the seam: {pa:?} vs {pb:?}"
        );
    }
}

fn split_layers(
    payload: &engine::chunk_stream::ChunkPayload,
) -> (&engine::mesh::BuiltMesh, Option<&engine::mesh::BuiltMesh>) {
    (
        payload
            .layer(engine::space::ChunkLayer::Land)
            .expect("land layer"),
        payload.layer(engine::space::ChunkLayer::Water),
    )
}

/// `(z, y)` of water vertices on a seam plane, sorted for comparison.
fn seam_points(
    mesh: Option<&engine::mesh::BuiltMesh>,
    on_plane: impl Fn(glam::Vec3) -> bool,
) -> Vec<(f32, f32)> {
    let Some(mesh) = mesh else {
        return Vec::new();
    };
    let mut pts: Vec<(f32, f32)> = mesh
        .positions
        .iter()
        .filter(|p| on_plane(**p))
        .map(|p| (p.z, p.y))
        .collect();
    pts.sort_by(|a, b| a.partial_cmp(b).expect("finite"));
    pts.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-6 && (a.1 - b.1).abs() < 1e-6);
    pts
}

#[test]
fn chunk_bakes_do_not_depend_on_build_order() {
    let (_atlas, surface) = world_of(4, 48);
    let builder = TerrainChunkBuilder::new(surface);
    let coord = ChunkCoord::new(60, 60);
    let first = builder.build(coord).expect("build").expect("content");
    let _neighbour = builder.build(coord.offset(1, 1)).expect("build");
    let again = builder.build(coord).expect("build").expect("content");
    let a = split_layers(&first).0;
    let b = split_layers(&again).0;
    assert_eq!(a.positions, b.positions);
    assert_eq!(a.normals, b.normals);
    assert_eq!(a.indices, b.indices);
}

#[test]
fn chunks_outside_the_atlas_have_no_content() {
    let (_atlas, surface) = world_of(4, 48);
    let builder = TerrainChunkBuilder::new(surface);
    assert!(builder
        .build(ChunkCoord::new(-4, 0))
        .expect("build")
        .is_none());
    assert!(builder
        .build(ChunkCoord::new(10_000, 0))
        .expect("build")
        .is_none());
}

#[test]
fn a_chunk_bake_stays_inside_the_frame_budget() {
    let (_atlas, surface) = world_of(1, 48);
    let builder = TerrainChunkBuilder::new(surface);
    let t0 = Instant::now();
    let payload = builder
        .build(ChunkCoord::new(120, 120))
        .expect("build")
        .expect("content");
    let dt = t0.elapsed();
    assert!(payload.layer_count() >= 1);
    assert!(
        dt < Duration::from_millis(600),
        "chunk bake took {dt:?}; streaming would stutter"
    );
}

// ---------------------------------------------------------------------- entry

#[test]
fn entry_resolves_to_dry_ground_near_the_requested_point() {
    let (atlas, surface) = world_of(1, 48);
    let bounds = AtlasBounds::of(&atlas);
    let Some(mid) = river_reach(&atlas) else {
        return;
    };
    let point = MapPoint::from_global(bounds, GlobalXZ::at(mid.x as f64, mid.y as f64))
        .expect("river point");
    let request = WorldEntryRequest::at(point);
    let pose = resolve_spawn(&surface, request).expect("spawn beside the river");

    assert!(!surface.column(pose.ground()).is_wet(), "spawned in water");
    assert!(
        pose.offset_m() <= 480.0,
        "resolver wandered {} m from the request",
        pose.offset_m()
    );
    let again = resolve_spawn(&surface, request).expect("spawn");
    assert_eq!(pose.ground().x, again.ground().x);
    assert_eq!(pose.ground().z, again.ground().z);
    assert_eq!(pose.heading().degrees(), again.heading().degrees());
}

#[test]
fn entry_into_open_ocean_fails_loudly() {
    let (atlas, surface) = world_of(1, 48);
    let bounds = AtlasBounds::of(&atlas);
    let hydro = surface.hydro();
    let size = atlas.size;
    let mut open_water = None;
    for az in 0..size {
        for ax in 0..size {
            let idx = az * size + ax;
            if hydro.cell_coasts[idx].is_empty() && hydro.cell_lakes[idx].is_empty() {
                open_water = Some(GlobalXZ::at(
                    (ax as f64 + 0.5) * CELL_METRES as f64,
                    (az as f64 + 0.5) * CELL_METRES as f64,
                ));
                break;
            }
        }
        if open_water.is_some() {
            break;
        }
    }
    let Some(p) = open_water else {
        return;
    };
    let request = WorldEntryRequest::at_global(bounds, p).expect("in bounds");
    assert!(matches!(
        resolve_spawn(&surface, request),
        Err(EntryError::NoSpawn { .. })
    ));
}

#[test]
fn selecting_outside_the_atlas_is_an_error() {
    let (atlas, _surface) = world_of(1, 48);
    let bounds = AtlasBounds::of(&atlas);
    let outside = GlobalXZ::at(-500.0, 10.0);
    assert!(WorldEntryRequest::at_global(bounds, outside).is_err());
}

// -------------------------------------------------------------------- session

/// Enter at a river reach and run until the player is standing.
fn standing_session(seed: i32, size: usize) -> Option<(World, WorldSession)> {
    let (atlas, surface) = world_of(seed, size);
    let bounds = AtlasBounds::of(&atlas);
    let mid = river_reach(&atlas)?;
    let request = WorldEntryRequest::at_global(bounds, GlobalXZ::at(mid.x as f64, mid.y as f64))
        .expect("river point");

    let mut world = World::new();
    let mut session = WorldSession::new(Arc::clone(&surface));
    session.begin_entry(&mut world, request).expect("entry");
    for _ in 0..600 {
        session.step(&mut world, WalkInput::IDLE).expect("update");
        if session.state() == SessionState::World {
            return Some((world, session));
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!("the entry ring never became resident");
}

#[test]
fn the_mouse_is_taken_only_when_the_player_asks_and_is_given_back_on_the_map() {
    // The world used to grab the pointer the moment it opened, which confines
    // the cursor to the window and follows the player out of the game.
    let Some((mut world, mut session)) = standing_session(1, 48) else {
        return;
    };
    assert!(
        !world.pointer_lock(),
        "standing in the world is not a request for the desktop's mouse"
    );

    session
        .step(
            &mut world,
            WalkInput {
                capture_look: true,
                ..WalkInput::IDLE
            },
        )
        .expect("click");
    assert!(world.pointer_lock(), "a click asks for mouse-look");

    session.return_to_atlas();
    session.step(&mut world, WalkInput::IDLE).expect("map");
    assert!(
        !world.pointer_lock(),
        "a map you have to click needs a cursor"
    );
}

#[test]
fn a_session_loads_its_entry_ring_before_handing_over_control() {
    let (atlas, surface) = world_of(1, 48);
    let bounds = AtlasBounds::of(&atlas);
    let Some(mid) = river_reach(&atlas) else {
        return;
    };
    let request = WorldEntryRequest::at_global(bounds, GlobalXZ::at(mid.x as f64, mid.y as f64))
        .expect("river point");

    let mut world = World::new();
    let mut session = WorldSession::new(Arc::clone(&surface));
    assert_eq!(session.state(), SessionState::Atlas);
    let pose = session.begin_entry(&mut world, request).expect("entry");
    assert_eq!(session.state(), SessionState::Loading);

    for _ in 0..600 {
        session.step(&mut world, WalkInput::IDLE).expect("update");
        if session.state() == SessionState::World {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        session.state(),
        SessionState::World,
        "the entry ring never became resident"
    );

    let feet = session.player_position().expect("player");
    let contact = session
        .contact_height(feet.horizontal())
        .expect("spawn chunk must carry contact");
    assert!(
        (feet.y as f32 - contact).abs() < 0.2,
        "the player must stand on the drawn ground"
    );
    assert!(
        (feet.y - pose.position().y).abs() < 5.0,
        "resolved and resident spawn heights disagree"
    );

    session.return_to_atlas();
    assert_eq!(session.state(), SessionState::Atlas);
    session.resume().expect("resume");
    assert_eq!(session.state(), SessionState::World);
}

#[test]
fn flying_leaves_the_ground_and_walking_lands_back_on_it() {
    let (atlas, surface) = world_of(1, 48);
    let bounds = AtlasBounds::of(&atlas);
    let Some(mid) = river_reach(&atlas) else {
        return;
    };
    let request = WorldEntryRequest::at_global(bounds, GlobalXZ::at(mid.x as f64, mid.y as f64))
        .expect("river point");

    let mut world = World::new();
    let mut session = WorldSession::new(Arc::clone(&surface));
    session.begin_entry(&mut world, request).expect("entry");
    for _ in 0..600 {
        session.step(&mut world, WalkInput::IDLE).expect("update");
        if session.state() == SessionState::World {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(session.state(), SessionState::World);
    assert_eq!(session.locomotion(), Some(Locomotion::Walk));

    let feet = session.player_position().expect("player");
    let eye = session.eye_position().expect("eye");
    assert!(
        (eye.y - feet.y - 1.7).abs() < 1e-6,
        "the camera must sit at eye height above the feet, got {} above {}",
        eye.y,
        feet.y
    );
    assert!(
        (eye.x - feet.x).abs() < 1e-9 && (eye.z - feet.z).abs() < 1e-9,
        "a first-person eye never trails the body"
    );

    let ground = feet.y;
    let climb = WalkInput {
        lift: 1.0,
        step_m: 20.0,
        toggle_fly: true,
        ..WalkInput::IDLE
    };
    session.step(&mut world, climb).expect("start flying");
    assert_eq!(session.locomotion(), Some(Locomotion::Fly));
    let up = session.player_position().expect("player");
    assert!(
        up.y > ground + 19.0,
        "flying must ignore the contact height: {} vs {ground}",
        up.y
    );

    // Keep climbing without touching F, then drop back into walking.
    session
        .step(
            &mut world,
            WalkInput {
                lift: 1.0,
                step_m: 20.0,
                ..WalkInput::IDLE
            },
        )
        .expect("keep flying");
    assert!(session.player_position().expect("player").y > up.y);

    session
        .step(
            &mut world,
            WalkInput {
                toggle_fly: true,
                ..WalkInput::IDLE
            },
        )
        .expect("stop flying");
    assert_eq!(session.locomotion(), Some(Locomotion::Walk));
    let landed = session.player_position().expect("player");
    assert!(
        (landed.y - ground).abs() < 0.2,
        "landing must snap back to the drawn ground: {} vs {ground}",
        landed.y
    );
}

#[test]
fn steering_right_swings_the_view_to_the_right() {
    use engine::camera::Camera;

    use super::session::turn_degrees;

    // One second of full-right steering, whatever the turn rate is set to.
    let quarter = turn_degrees(1.0, 0.0, 1.0);
    let yaw = 20.0;
    let after = Camera::facing_xz(yaw + quarter * (90.0 / quarter.abs()));
    assert!(
        after.dot(Camera::right_xz(yaw)) > 0.999,
        "steering right must rotate the facing onto the screen-right axis"
    );
    assert!(quarter < 0.0, "yaw decreases when turning right");
    assert!(
        turn_degrees(0.0, 10.0, 0.0) < 0.0,
        "moving the mouse right must turn right too"
    );
    assert_eq!(turn_degrees(0.0, 0.0, 0.016), 0.0);
}

#[test]
fn turning_wraps_instead_of_drifting_off_the_compass() {
    let (atlas, surface) = world_of(1, 48);
    let bounds = AtlasBounds::of(&atlas);
    let Some(mid) = river_reach(&atlas) else {
        return;
    };
    let request = WorldEntryRequest::at_global(bounds, GlobalXZ::at(mid.x as f64, mid.y as f64))
        .expect("river point");

    let mut world = World::new();
    let mut session = WorldSession::new(Arc::clone(&surface));
    session.begin_entry(&mut world, request).expect("entry");
    for _ in 0..600 {
        session.step(&mut world, WalkInput::IDLE).expect("update");
        if session.state() == SessionState::World {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(session.state(), SessionState::World);

    let start = session.player_heading().expect("heading").degrees();
    // Ten full turns: Heading rejects anything outside [0, 360).
    for _ in 0..100 {
        session
            .step(
                &mut world,
                WalkInput {
                    yaw_delta_degrees: 36.0,
                    ..WalkInput::IDLE
                },
            )
            .expect("turn");
        session.player_heading().expect("yaw stayed on the compass");
    }
    let end = session.player_heading().expect("heading").degrees();
    assert!(
        (end - start).abs() < 0.01,
        "ten whole turns must come back to the same heading: {start} then {end}"
    );
}

// --------------------------------------------------------------------- stream

#[test]
fn walking_never_leaves_the_ground_missing_underfoot() {
    let (atlas, surface) = world_of(1, 48);
    let bounds = AtlasBounds::of(&atlas);
    let Some(mid) = river_reach(&atlas) else {
        return;
    };
    let request = WorldEntryRequest::at_global(bounds, GlobalXZ::at(mid.x as f64, mid.y as f64))
        .expect("river point");
    let pose = resolve_spawn(&surface, request).expect("spawn");

    let mut world = World::new();
    let mut stream = WorldStream::new(Arc::clone(&surface)).with_visual_ring(2);
    let mut p = pose.ground();
    stream.prepare_entry(&mut world, p).expect("entry ring");

    let dir = Vec2::new(1.0, 0.35).normalize();
    for step in 0..24 {
        p = GlobalXZ::at(p.x + dir.x as f64 * 25.0, p.z + dir.y as f64 * 25.0);
        stream.prepare_entry(&mut world, p).expect("ring");
        stream.sync(&mut world, p, Some(dir)).expect("sync");
        assert!(
            stream.contact_height(p).is_some(),
            "no ground under the walker at step {step}"
        );
    }
    assert!(stream.resident_count() >= 9);
}

#[test]
fn rebasing_keeps_the_world_where_it_was() {
    let (_atlas, surface) = world_of(1, 48);
    let mut world = World::new();
    let mut stream = WorldStream::new(Arc::clone(&surface)).with_visual_ring(1);
    let near = GlobalXZ::at(20_000.0, 20_000.0);
    world
        .set_render_origin(engine::space::RenderOrigin::snapped(near, CHUNK_SPAN_M).unwrap())
        .expect("origin");
    stream.prepare_entry(&mut world, near).expect("ring");
    let ground_before = stream.contact_height(near).expect("contact");
    assert!(!stream.maybe_rebase(&mut world, near).expect("rebase"));

    let far = GlobalXZ::at(24_000.0, 20_000.0);
    assert!(stream.maybe_rebase(&mut world, far).expect("rebase"));
    assert_eq!(
        stream.contact_height(near),
        Some(ground_before),
        "contact is global data and must survive a rebase"
    );
    assert!(world.render_offset_m(far) < CHUNK_SPAN_M);
}

// ----------------------------------------------------------------- hydrology

#[test]
fn hydro_vectors_bake_with_sinks() {
    let atlas = ContinentAtlas::generate(5, 64);
    assert!(!atlas.hydro.coasts.is_empty(), "expected coast rings");
    let mut long_reaches = 0usize;
    for r in &atlas.hydro.rivers {
        assert!(matches!(r.sink, HydroSink::Ocean | HydroSink::Lake { .. }));
        assert!(r.points.len() >= 2);
        assert!(r.half_width_m > 0.0);
        if r.points.len() >= 8 {
            long_reaches += 1;
        }
    }
    assert!(
        long_reaches > 0,
        "expected stitched multi-cell river reaches"
    );
}

#[test]
fn the_coast_index_matches_an_exhaustive_scan() {
    let (atlas, surface) = world_of(11, 48);
    let hydro = &atlas.hydro;
    let size = atlas.size;
    let index = surface.hydro_index();
    for az in 1..size - 1 {
        for ax in 1..size - 1 {
            let mut stamped = false;
            for dz in -1..=1_i32 {
                for dx in -1..=1_i32 {
                    let i = (az as i32 + dz) as usize * size + (ax as i32 + dx) as usize;
                    stamped |= !hydro.cell_coasts[i].is_empty();
                }
            }
            if !stamped {
                continue;
            }
            let p = Vec2::new(
                (ax as f32 + 0.5) * CELL_METRES,
                (az as f32 + 0.5) * CELL_METRES,
            );
            let indexed = index.coast_signed(hydro, p);
            let full = coast_signed_full(hydro, p);
            assert_eq!(
                indexed > 0.0,
                full > 0.0,
                "coast side mismatch at ({ax},{az}): indexed={indexed} exhaustive={full}"
            );
            if full.abs() <= COAST_QUERY_M {
                assert!(
                    (indexed - full).abs() < 1.0,
                    "coast SD mismatch at ({ax},{az}): indexed={indexed} exhaustive={full}"
                );
            } else {
                assert!(
                    indexed.abs() <= COAST_QUERY_M + 1.0,
                    "coast SD must saturate past the query range, got {indexed}"
                );
            }
        }
    }
}

#[test]
fn open_ocean_coast_queries_stay_cheap() {
    let (atlas, surface) = world_of(1, 48);
    let hydro = &atlas.hydro;
    let size = atlas.size;
    let index = surface.hydro_index();
    let mut open = None;
    'outer: for az in 0..size {
        for ax in 0..size {
            if hydro.cell_coasts[az * size + ax].is_empty() {
                open = Some(Vec2::new(
                    (ax as f32 + 0.5) * CELL_METRES,
                    (az as f32 + 0.5) * CELL_METRES,
                ));
                break 'outer;
            }
        }
    }
    let Some(p) = open else {
        return;
    };
    let t0 = Instant::now();
    let mut last = 0.0;
    for _ in 0..20_000 {
        last = index.coast_signed(hydro, p);
    }
    let dt = t0.elapsed();
    assert!(last < 0.0, "open ocean must be negative SD, got {last}");
    assert!(
        dt < Duration::from_millis(200),
        "20k open-ocean coast_signed calls took {dt:?} (full-ring fallback?)"
    );
}
