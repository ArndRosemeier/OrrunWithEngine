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
use glam::{Vec2, Vec3};

use super::hydro_geom::{coast_signed_full, signed_distance_ring, COAST_QUERY_M};
use super::{
    best_settlement_entry, chunk_span, classify_settlement, resolve_spawn, AtlasBounds, AtlasCell,
    ContinentalSurface, EntryError, HamletStand, Locomotion, MapPoint, PondField, PondWindow,
    PropClass, ScatterCatalog, SessionError, SessionState, SettlementLayer, TerrainChunkBuilder,
    TravelPhase, TravelTimings, WalkInput, WaterBody, WorldEntryRequest, WorldSession, WorldStream,
    CHUNK_SAMPLE_M, CHUNK_SPAN_M, MEDIUM, MIN_WATER_DEPTH,
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
    assert!(catalog.count_of(PropClass::Tree) >= 8);
    assert!(catalog.count_of(PropClass::Rock) >= 4);
    assert!(catalog.count_of(PropClass::Bush) >= 6);
    assert!(catalog.count_of(PropClass::Snag) >= 2);
    assert!(catalog.count_of(PropClass::Mushroom) >= 2);
    assert!(catalog.count_of(PropClass::Berry) >= 2);

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
fn vendored_kit_pieces_keep_their_baked_albedo() {
    // Kit cells bake the look into a baseColor map. Scatter rocks do the same;
    // stripping those maps leaves a white 1×1 albedo.
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/kit/medieval/med_wall.glb");
    let blob = std::fs::read(&path).expect("med_wall.glb");
    assert!(
        blob.windows(b"baseColorTexture".len())
            .any(|w| w == b"baseColorTexture"),
        "{} lost its albedo map",
        path.display()
    );
    let mesh = engine::model::Model::load(&path)
        .expect("medieval wall loads")
        .build();
    assert!(
        mesh.uvs.iter().any(|uv| uv[0] > 0.01 || uv[1] > 0.01),
        "wall UVs are missing; albedo would sample a single texel"
    );
}

#[test]
fn vendored_indoor_pieces_keep_their_baked_albedo() {
    for file in ["furn_floor.glb", "furn_bed.glb", "furn_hearth.glb"] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets/kit/indoor")
            .join(file);
        let blob = std::fs::read(&path)
            .unwrap_or_else(|err| panic!("read textured indoor piece {}: {err}", path.display()));
        assert!(
            blob.windows(b"baseColorTexture".len())
                .any(|window| window == b"baseColorTexture"),
            "{} lost its baked albedo map",
            path.display()
        );
        let mesh = engine::model::Model::load(&path)
            .unwrap_or_else(|err| panic!("load textured indoor piece {}: {err}", path.display()));
        assert!(
            mesh.albedo().is_some(),
            "{} loaded without an albedo map",
            path.display()
        );
    }
}

#[test]
fn vendored_castle_pieces_keep_their_baked_albedo() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("assets/kit/castle/castle_curtain.glb");
    let blob = std::fs::read(&path).expect("castle_curtain.glb");
    assert!(
        blob.windows(b"baseColorTexture".len())
            .any(|w| w == b"baseColorTexture"),
        "{} lost its albedo map",
        path.display()
    );
    let mesh = engine::model::Model::load(&path)
        .expect("castle curtain loads")
        .build();
    assert!(
        mesh.uvs.iter().any(|uv| uv[0] > 0.01 || uv[1] > 0.01),
        "curtain UVs are missing; albedo would sample a single texel"
    );
}

#[test]
fn vendored_rocks_keep_their_baked_albedo() {
    // Rocks bake veins into a baseColor map. The old sync dropped that map and
    // the game painted a flat grey, which is the untextured stone in the world.
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/props/rocks");
    let mut found = 0;
    for entry in std::fs::read_dir(&dir).expect("rocks dir") {
        let path = entry.expect("entry").path();
        if !path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("glb"))
        {
            continue;
        }
        found += 1;
        let blob = std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        assert!(
            blob.windows(b"baseColorTexture".len())
                .any(|w| w == b"baseColorTexture"),
            "{} lost its albedo map",
            path.display()
        );
        let mesh = engine::model::Model::load(&path)
            .unwrap_or_else(|e| panic!("{} loads: {e}", path.display()));
        assert!(
            mesh.albedo().is_some(),
            "{} loaded without an albedo map",
            path.display()
        );
    }
    assert!(found >= 4, "expected several rock meshes, found {found}");
}

#[test]
fn vendored_kit_plinth_sits_on_the_origin() {
    let plinth = engine::model::Model::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/kit/medieval/med_plinth.glb"),
    )
    .expect("medieval plinth loads")
    .build();
    let min_y = plinth
        .positions
        .iter()
        .map(|p| p.y)
        .fold(f32::INFINITY, f32::min);
    let max_y = plinth
        .positions
        .iter()
        .map(|p| p.y)
        .fold(f32::NEG_INFINITY, f32::max);
    assert!(
        min_y.abs() < 0.05,
        "plinth base should sit on y=0 after Y-up export, got {min_y}"
    );
    assert!(
        max_y > 2.0,
        "plinth should fill the storey cell; max y is {max_y}"
    );
}

#[test]
fn inland_low_pop_is_a_hamlet() {
    assert_eq!(classify_settlement(7, -1), 0);
    assert_eq!(classify_settlement(9, -1), 1);
    assert_eq!(classify_settlement(12, -1), 2);
    assert_eq!(classify_settlement(11, 0), 3);
}

#[test]
fn surface_carries_every_atlas_settlement_pin() {
    let (atlas, surface) = world_of(20260809, 64);
    let nodes = atlas
        .nodes
        .iter()
        .filter(|n| n.kind == crate::atlas::NodeKind::Settlement)
        .count();
    assert_eq!(surface.settlements().len(), nodes);
}

#[test]
fn nearest_dungeon_is_the_closest_mouth() {
    let (_, surface) = world_of(20260816, 64);
    let pins = surface.dungeon_pins();
    assert!(
        !pins.is_empty(),
        "seed 20260816 size 64 must plant dungeon mouths"
    );
    for pin in pins {
        let found = surface
            .nearest_dungeon(pin.at)
            .expect("a pin is nearest to itself");
        assert_eq!(
            found.id, pin.id,
            "nearest to dungeon {} was {}",
            pin.id, found.id
        );
    }
}

#[test]
fn largest_settlement_is_the_highest_tier_then_pop() {
    let (_, surface) = world_of(20260809, 64);
    let Some(best) = surface.largest_settlement() else {
        return;
    };
    for pin in surface.settlements() {
        assert!(
            (best.tier, best.population, best.id) >= (pin.tier, pin.population, pin.id),
            "pin {} tier {} pop {} outranks the reported largest",
            pin.id,
            pin.tier,
            pin.population
        );
    }
}

#[test]
fn inland_atlas_land_is_not_carved_as_open_ocean() {
    let (atlas, surface) = world_of(20260809, 1000);
    let p = GlobalXZ::at(457_327.0, 431_562.0);
    let biome = crate::atlas::pack::biome(atlas.cell_at(457, 431));
    assert!(
        crate::atlas::biomes::is_land(biome),
        "the reported click must stay atlas land, got {biome:?}"
    );
    let column = surface.column(p);
    assert!(
        !matches!(column.body(), Some(WaterBody::Ocean)),
        "plains at ({:.0}, {:.0}) was carved as ocean (wetness={:.1} ground={:.1})",
        p.x,
        p.z,
        column.wetness(),
        column.ground()
    );
    let ponds = PondField::empty(p);
    let request = WorldEntryRequest::at_global(AtlasBounds::of(&atlas), p).expect("in bounds");
    resolve_spawn(&surface, &ponds, request)
        .expect("a green inland click must have walkable ground nearby");
    let coast = surface.coast_signed(p);
    assert!(
        coast > 90.0,
        "inland plains must sit well inside the land side, coast_sd={coast}"
    );
    assert_eq!(
        super::ambience::ocean_presence(coast),
        0.0,
        "ocean ambience must be silent on dry inland ground"
    );
}

#[test]
fn full_size_has_a_walkable_start_settlement() {
    let (_, surface) = world_of(20260809, 1000);
    let ponds = PondField::empty(GlobalXZ::at(0.0, 0.0));
    let (pin, request) =
        best_settlement_entry(&surface, &ponds).expect("at least one settlement must be walkable");
    resolve_spawn(&surface, &ponds, request).expect("chosen settlement resolves");
    eprintln!(
        "full-size start: {} pop {} at ({:.0}, {:.0})",
        pin.tier, pin.population, pin.at.x, pin.at.z
    );
}

#[test]
fn a_continent_has_a_handful_of_ports() {
    for seed in [20260809, 7, 99] {
        let (_, surface) = world_of(seed, 128);
        let n = surface.settlements().len();
        assert!(n >= 2, "seed {seed} has no settlements");
        let mut counts = [0usize; 4];
        for pin in surface.settlements() {
            counts[pin.tier as usize] += 1;
        }
        let ports = counts[3];
        assert!(
            (2..=4).contains(&ports),
            "seed {seed}: {n} settlements, {ports} ports (want 2..=4); counts={counts:?}"
        );
        let best = surface.largest_settlement().expect("settlement");
        assert_eq!(best.tier, 3, "seed {seed}: largest is tier {}", best.tier);
        if n >= 4 {
            assert!(
                counts[2] >= 1,
                "seed {seed}: expected at least one town under the ports"
            );
        }
    }
}

#[test]
fn surface_bakes_atlas_roads_into_world_metres() {
    let (atlas, surface) = world_of(20260809, 64);
    assert!(
        !atlas.road_links.is_empty(),
        "this seed is supposed to have roads"
    );
    assert!(
        !surface.roads().is_empty(),
        "atlas road links must become world-metre polylines"
    );
    let extent = atlas.size as f32 * CELL_METRES;
    for road in surface.roads() {
        assert!(
            road.points.len() >= 2,
            "road {} is a path, not a point",
            road.id
        );
        for p in &road.points {
            assert!(
                p.x >= -1.0 && p.y >= -1.0 && p.x <= extent + 1.0 && p.y <= extent + 1.0,
                "road {} left the continent at {p:?}",
                road.id
            );
        }
    }
}

#[test]
fn a_hamlet_split_across_a_river_gets_a_footbridge() {
    let (_, surface) = world_of(20260809, 64);
    let ponds = PondField::empty(GlobalXZ::at(0.0, 0.0));
    let mut span = None;
    for pin in surface.settlements() {
        let plaza = Vec2::new(pin.at.x as f32, pin.at.z as f32);
        let Some(river) = surface.hydro_index().nearest_river(surface.hydro(), plaza) else {
            continue;
        };
        if river.dist > 180.0 || river.tangent.length_squared() < 1e-6 {
            continue;
        }
        let perp = Vec2::new(-river.tangent.y, river.tangent.x).normalize_or_zero();
        let offset = (river.half_width + 25.0).max(30.0);
        let left = river.at + perp * offset;
        let right = river.at - perp * offset;
        let hamlet = HamletStand {
            at: pin.at,
            radius: 40.0,
            houses: vec![
                GlobalXZ::at(f64::from(left.x), f64::from(left.y)),
                GlobalXZ::at(f64::from(right.x), f64::from(right.y)),
            ],
            cut: Vec::new(),
        };
        if let Some(found) = super::paths::hamlet_span(&surface, &ponds, &hamlet) {
            span = Some(found);
            break;
        }
    }
    let span = span.expect("houses on both banks of a nearby river must get a footbridge");
    assert_eq!(span.kind, super::paths::SpanKind::Bridge);
    assert!(
        span.a.distance(span.b) >= 4.0,
        "footbridge too short: {}",
        span.a.distance(span.b)
    );
}

#[test]
fn following_the_pond_window_does_not_scan_on_the_game_thread() {
    let surface = surface_of(20260809);
    let mut window = PondWindow::new(Arc::clone(&surface));
    let focus = GlobalXZ::at(20_000.0, 20_000.0);
    let started = Instant::now();
    window.follow(focus);
    let took = started.elapsed();
    assert!(
        took < Duration::from_millis(200),
        "follow blocked for {took:?}; it must only start a thread"
    );
    assert!(
        !window.field().covers(focus, MEDIUM.reach_m()),
        "the placeholder field must stay in hand until the thread finishes"
    );
}

#[test]
fn following_settlements_does_not_pack_on_the_game_thread() {
    let surface = surface_of(20260809);
    let Some(pin) = surface.settlements().first().copied() else {
        return;
    };
    let ponds = PondWindow::new(Arc::clone(&surface));
    let stream = WorldStream::new(Arc::clone(&surface), ponds.shared());
    let mut world = World::new();
    let mut layer = SettlementLayer::install(&mut world, surface.world_seed()).expect("kit");
    let field = ponds.field();
    let started = Instant::now();
    layer
        .follow(&mut world, &stream, &surface, &field, pin.at, false)
        .expect("follow");
    let took = started.elapsed();
    assert!(
        took < Duration::from_millis(200),
        "follow blocked for {took:?}; it must only start a thread"
    );
    assert!(
        layer.busy(),
        "packing must be in flight rather than finished on this thread"
    );
    assert!(
        layer.hamlets().is_empty(),
        "nothing seated until the thread lands"
    );
}

#[test]
#[ignore = "diagnostic: what a window of ponds costs and what it holds"]
fn what_a_pond_window_costs() {
    for seed in [3, 20260809] {
        let surface = surface_of(seed);
        let mid = surface.bounds().metres() / 2.0;
        let started = Instant::now();
        let field = PondField::build(&surface, GlobalXZ::at(mid, mid));
        let took = started.elapsed().as_secs_f32() * 1000.0;
        let reach: f32 = field.ponds().iter().map(|p| p.reach_m()).sum();
        println!(
            "seed {seed}: {} ponds ({:.1} km of reach) in {took:.0} ms",
            field.ponds().len(),
            reach / 1000.0,
        );
    }
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
        println!(
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
            println!(
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
fn coarse_tiers_do_not_bridge_the_reported_river_canyon() {
    let (_, surface) = world_of(20260809, 256);
    // Fixture refreshed after Quilez lowland relief moved the carve ~80 m from
    // the original camera-ahead centreline at (135335, 88563). The point must
    // stay on a deep wet bed so medium/far quads cannot bridge the banks.
    let p = GlobalXZ::at(135_385.0, 88_498.0);
    let column = surface.column(p);
    let walked = tier_height(&surface, p, &super::NEAR);
    let medium = tier_height(&surface, p, &super::MEDIUM);
    let far = tier_height(&surface, p, &super::FAR);
    assert!(column.is_wet(), "reported point is no longer in the river");
    assert!(
        surface.base_ground(p) - column.ground() > 70.0,
        "the invading hill no longer forms a canyon"
    );
    assert!(
        medium <= walked + 0.25 && far <= walked + 0.25,
        "coarse ground bridges the canyon: walked={walked:.1}, medium={medium:.1}, far={far:.1}"
    );
    assert!(
        far <= column.sheet_hint() - 5.0,
        "far ground {far:.1} is not clearly below river sheet {:.1}",
        column.sheet_hint()
    );
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
fn high_relief_reads_as_ranges_not_high_plains() {
    // Atlas loft is a kilometre-smooth field. Without contrasting it against
    // its neighbourhood, Peak height is a high plane with 50 m of wrinkle.
    // Orogen flanks have to come out steeper than the lowland, over a span
    // that is the mountain, not the hill noise.
    let surface = surface_of(20260809);
    let fields = surface.fields();
    let span = surface.bounds().metres();
    let step = 80.0;
    let mut alpine = Vec::new();
    let mut plains = Vec::new();
    let probe = 120usize;
    let lattice = span / probe as f64;
    for iz in 2..probe - 2 {
        for ix in 2..probe - 2 {
            let x = (ix as f64 + 0.5) * lattice;
            let z = (iz as f64 + 0.5) * lattice;
            let elev = fields.sample_smooth(&fields.elevation_m, x as f32, z as f32);
            let relief = fields.sample_smooth(&fields.relief01, x as f32, z as f32);
            if elev < 40.0 {
                continue;
            }
            let slope = local_slope(&surface, x, z, step);
            if relief > 0.50 && elev > 900.0 {
                alpine.push(slope);
            } else if relief < 0.22 && (50.0..350.0).contains(&elev) {
                plains.push(slope);
            }
        }
    }
    assert!(
        alpine.len() > 80,
        "expected an orogen on seed 20260809, got {} alpine probes",
        alpine.len()
    );
    assert!(
        plains.len() > 80,
        "expected lowland on seed 20260809, got {} plains probes",
        plains.len()
    );
    alpine.sort_by(|a, b| a.total_cmp(b));
    plains.sort_by(|a, b| a.total_cmp(b));
    let med = |v: &[f32]| v[v.len() / 2];
    let alpine_med = med(&alpine);
    let plains_med = med(&plains);
    assert!(
        alpine_med > plains_med * 2.0,
        "alpine median slope {alpine_med:.3} is not a range next to plains {plains_med:.3}"
    );
    assert!(
        alpine_med > 0.12,
        "alpine median slope {alpine_med:.3} still reads as a high plane"
    );
}

#[test]
fn alpine_crests_are_not_a_regular_wave() {
    // A 1.4 km Laplacian of a bicubic loft is a sine: crests equally spaced.
    // Ridged mountain noise was the other regular wave — a comb of isolines.
    // Walk alpine transects and refuse a profile whose peaks keep one wavelength.
    let surface = surface_of(20260809);
    let fields = surface.fields();
    let span = surface.bounds().metres();
    let mut starts = Vec::new();
    let probe = 80usize;
    let lattice = span / probe as f64;
    for iz in 8..probe - 8 {
        for ix in 8..probe - 8 {
            let x = (ix as f64 + 0.5) * lattice;
            let z = (iz as f64 + 0.5) * lattice;
            let elev = fields.sample_smooth(&fields.elevation_m, x as f32, z as f32);
            let relief = fields.sample_smooth(&fields.relief01, x as f32, z as f32);
            if relief > 0.55 && elev > 1_000.0 {
                starts.push((x, z));
            }
        }
    }
    assert!(
        starts.len() > 20,
        "expected alpine ground on seed 20260809, got {}",
        starts.len()
    );

    let step = 40.0;
    let samples = 160usize;
    let mut spacings = Vec::new();
    for &(x0, z0) in starts.iter().step_by((starts.len() / 8).max(1)) {
        let mut h = Vec::with_capacity(samples);
        for i in 0..samples {
            let x = x0 + i as f64 * step;
            h.push(surface.base_ground(GlobalXZ::at(x, z0)));
        }
        let mut peaks = Vec::new();
        for i in 6..samples - 6 {
            // Broad crests: ridged isolines were 40 m knives, the loft sine
            // is a kilometre wave. Either is a peak against its shoulders.
            let left = h[i - 6..i].iter().copied().fold(f32::MAX, f32::min);
            let right = h[i + 1..i + 7].iter().copied().fold(f32::MAX, f32::min);
            if h[i] >= h[i - 1] && h[i] >= h[i + 1] && h[i] - left.min(right) > 8.0 {
                peaks.push(i);
            }
        }
        for w in peaks.windows(2) {
            spacings.push((w[1] - w[0]) as f32 * step as f32);
        }
    }
    assert!(
        spacings.len() > 12,
        "too few alpine crests to judge a wave, got {}",
        spacings.len()
    );
    let mean = spacings.iter().sum::<f32>() / spacings.len() as f32;
    let var = spacings
        .iter()
        .map(|s| {
            let d = *s - mean;
            d * d
        })
        .sum::<f32>()
        / spacings.len() as f32;
    let cv = var.sqrt() / mean.max(1.0);
    assert!(
        cv > 0.28,
        "alpine crest spacing is too regular (mean {mean:.0} m, cv {cv:.2}); the range is still a sine"
    );
}

#[test]
fn dominant_massif_summits_survive_the_far_grid() {
    let (atlas, surface) = world_of(20260809, 128);
    assert!(
        !atlas.alpine_massifs.is_empty(),
        "fixed alpine seed has no massif sites"
    );
    let sample_m = super::FAR.sample_m as f32;
    for (i, site) in atlas.alpine_massifs.iter().take(16).enumerate() {
        let across_x = -site.crest_axis_z;
        let across_z = site.crest_axis_x;
        let summit_x = site.centre_x_m
            + site.crest_axis_x * site.summit_along_offset_m
            + across_x * site.summit_across_offset_m;
        let summit_z = site.centre_z_m
            + site.crest_axis_z * site.summit_along_offset_m
            + across_z * site.summit_across_offset_m;
        let summit = GlobalXZ::at(f64::from(summit_x), f64::from(summit_z));
        let summit_ax = (summit_x / CELL_METRES).floor() as i32;
        let summit_az = (summit_z / CELL_METRES).floor() as i32;
        assert!(
            !surface.column(summit).is_wet(),
            "massif {i} summit ({summit_x:.0}, {summit_z:.0}) cell ({summit_ax},{summit_az}) is {:?} in atlas and {:?} on surface",
            crate::atlas::pack::biome(atlas.cell_at(summit_ax, summit_az)),
            surface.column(summit).body(),
        );
        let grid_x = (summit_x / sample_m).round() * sample_m;
        let grid_z = (summit_z / sample_m).round() * sample_m;
        let mut retained_m = 0.0f32;
        for dz in -1..=1 {
            for dx in -1..=1 {
                retained_m = retained_m.max(
                    surface
                        .debug_layers(grid_x + dx as f32 * sample_m, grid_z + dz as f32 * sample_m)
                        .7,
                );
            }
        }
        assert!(
            retained_m >= site.prominence_m * 0.82,
            "massif {i} loses its summit on the 125 m grid: retained {retained_m:.0} m of {:.0} m",
            site.prominence_m
        );
    }
}

#[test]
fn alpine_silhouette_is_quilez_not_a_sparse_massif() {
    // Ridge massifs are rare landmarks. A snowy column is almost always loft
    // plus Quilez IQ — if |iq| is tiny, the view is still the 1 km sine.
    let surface = surface_of(20260809);
    let fields = surface.fields();
    let span = surface.bounds().metres();
    let probe = 80usize;
    let lattice = span / probe as f64;
    let mut iq = Vec::new();
    let mut massif_hits = 0usize;
    for iz in 6..probe - 6 {
        for ix in 6..probe - 6 {
            let x = (ix as f64 + 0.5) * lattice;
            let z = (iz as f64 + 0.5) * lattice;
            let elev = fields.sample_smooth(&fields.elevation_m, x as f32, z as f32);
            let relief = fields.sample_smooth(&fields.relief01, x as f32, z as f32);
            if relief <= 0.50 || elev <= 900.0 {
                continue;
            }
            let layers = surface.terrain_layers(GlobalXZ::at(x, z));
            iq.push(layers.iq_m.abs());
            if layers.massif_m > 50.0 {
                massif_hits += 1;
            }
        }
    }
    assert!(
        iq.len() > 40,
        "expected alpine ground on seed 20260809, got {}",
        iq.len()
    );
    iq.sort_by(|a, b| a.total_cmp(b));
    let med = iq[iq.len() / 2];
    assert!(
        med > 80.0,
        "alpine median |iq| is {med:.1} m; the range is still the loft sine"
    );
    let massif_frac = massif_hits as f32 / iq.len() as f32;
    assert!(
        massif_frac < 0.20,
        "massifs cover {massif_frac:.2} of alpine probes; a random summit is not a ridge site"
    );
}

#[test]
fn lowland_has_a_quiet_floor_and_occasional_hills() {
    // The old look was 20 m of FBM on every plains sample: a complicated sine.
    // The floor has to stay gentle, and a minority of 16 m probes has to be a
    // knoll face or a ravine wall — otherwise the landmarks never arrived.
    let surface = surface_of(20260809);
    let fields = surface.fields();
    let span = surface.bounds().metres();
    let step = 24.0;
    let probe = 90usize;
    let lattice = span / probe as f64;
    let mut slopes = Vec::new();
    let mut grain = Vec::new();
    for iz in 4..probe - 4 {
        for ix in 4..probe - 4 {
            let x = (ix as f64 + 0.5) * lattice;
            let z = (iz as f64 + 0.5) * lattice;
            let elev = fields.sample_smooth(&fields.elevation_m, x as f32, z as f32);
            let relief = fields.sample_smooth(&fields.relief01, x as f32, z as f32);
            if relief >= 0.18 || !(40.0..250.0).contains(&elev) {
                continue;
            }
            slopes.push(local_slope(&surface, x, z, step));
            grain.push(surface.terrain_layers(GlobalXZ::at(x, z)).grain_m.abs());
        }
    }
    assert!(
        slopes.len() > 80,
        "expected lowland on seed 20260809, got {} plains probes",
        slopes.len()
    );
    slopes.sort_by(|a, b| a.total_cmp(b));
    let med = slopes[slopes.len() / 2];
    let p95 = slopes[(slopes.len() as f64 * 0.95) as usize];
    let steep = slopes.iter().filter(|s| **s > 0.38).count();
    assert!(
        med < 0.12,
        "plains median slope {med:.3} is not a quiet floor (p95 {p95:.3}, steep {steep}/{})",
        slopes.len()
    );
    assert!(
        p95 > med * 2.0,
        "plains 95th slope {p95:.3} is not a landmark next to the floor {med:.3}"
    );
    assert!(
        steep >= 2,
        "no knoll or ravine faces on the plains ({steep} steep of {}, med {med:.3} p95 {p95:.3})",
        slopes.len()
    );
    assert!(
        steep < slopes.len() / 4,
        "plains are cliffs everywhere ({steep} of {}), not occasional",
        slopes.len()
    );
    grain.sort_by(|a, b| a.total_cmp(b));
    let grain_med = grain[grain.len() / 2];
    assert!(
        grain_med > 6.0,
        "plains median |grain| is {grain_med:.1} m; lowland Quilez hills never arrived"
    );
}

#[test]
fn every_large_default_forest_has_expansion_hills() {
    let (_, surface) = world_of(20260809, 256);
    let fields = surface.fields();
    let span_m = surface.bounds().metres();
    let step_m = 500.0_f64;
    let side = (span_m / step_m) as usize;
    let mut forest = vec![false; side * side];
    let mut hill = vec![false; side * side];
    let mut forest_samples = 0usize;
    let mut forest_hills_10 = 0usize;
    let mut forest_hills_30 = 0usize;

    for iz in 0..side {
        for ix in 0..side {
            let x = (ix as f64 + 0.5) * step_m;
            let z = (iz as f64 + 0.5) * step_m;
            let canopy = fields.sample_smooth(&fields.canopy01, x as f32, z as f32);
            if canopy < 0.55 {
                continue;
            }
            let knoll = surface.debug_layers(x as f32, z as f32).5;
            let at = iz * side + ix;
            forest[at] = true;
            hill[at] = knoll > 10.0;
            forest_samples += 1;
            forest_hills_10 += usize::from(knoll > 10.0);
            forest_hills_30 += usize::from(knoll > 30.0);
        }
    }

    const MACRO_SIDE: usize = 20;
    let mut forest_macrotiles = 0usize;
    let mut empty_macrotiles = 0usize;
    let mut fewest_hills_in_macrotile = usize::MAX;
    for macro_z in 0..side.div_ceil(MACRO_SIDE) {
        for macro_x in 0..side.div_ceil(MACRO_SIDE) {
            let mut forest_in_tile = 0usize;
            let mut hills_in_tile = 0usize;
            for iz in macro_z * MACRO_SIDE..((macro_z + 1) * MACRO_SIDE).min(side) {
                for ix in macro_x * MACRO_SIDE..((macro_x + 1) * MACRO_SIDE).min(side) {
                    let at = iz * side + ix;
                    forest_in_tile += usize::from(forest[at]);
                    hills_in_tile += usize::from(hill[at]);
                }
            }
            if forest_in_tile >= 200 {
                forest_macrotiles += 1;
                empty_macrotiles += usize::from(hills_in_tile == 0);
                fewest_hills_in_macrotile = fewest_hills_in_macrotile.min(hills_in_tile);
            }
        }
    }

    println!(
        "forest: {forest_samples} samples, knoll>10m {forest_hills_10} ({:.2}%), \
         knoll>30m {forest_hills_30} ({:.2}%), 10km forest tiles without a \
         sampled hill {empty_macrotiles}/{forest_macrotiles}, fewest hills \
         in one tile {fewest_hills_in_macrotile}",
        100.0 * forest_hills_10 as f32 / forest_samples as f32,
        100.0 * forest_hills_30 as f32 / forest_samples as f32
    );
    assert!(
        forest_macrotiles >= 60,
        "default seed has only {forest_macrotiles} dense forest regions to check"
    );
    assert_eq!(
        empty_macrotiles, 0,
        "{empty_macrotiles}/{forest_macrotiles} dense forest regions have no expansion hill"
    );
    assert!(
        fewest_hills_in_macrotile >= 10,
        "a 10 km dense forest region has only {fewest_hills_in_macrotile} sampled hills"
    );
}

#[test]
fn cliffs_and_ravines_are_sparse_bounded_landforms() {
    let surface = surface_of(20260809);
    let span_m = surface.bounds().metres();
    let sample_m = 120.0_f64;
    let side = (span_m / sample_m) as usize;
    let mut hill_samples = 0usize;
    let mut cliff_samples = 0usize;
    let mut ravine_samples = 0usize;
    let mut max_cliff_grade = 0.0_f32;
    let mut cliff_at = GlobalXZ::at(0.0, 0.0);
    let mut deepest_ravine_m = 0.0_f32;
    let mut ravine_at = GlobalXZ::at(0.0, 0.0);

    for iz in 0..side {
        for ix in 0..side {
            let x = (ix as f64 + 0.5) * sample_m;
            let z = (iz as f64 + 0.5) * sample_m;
            let layers = surface.debug_layers(x as f32, z as f32);
            let knoll_m = layers.5;
            let ravine_m = layers.6;
            hill_samples += usize::from(knoll_m > 10.0);
            ravine_samples += usize::from(ravine_m.abs() > 2.0);
            if ravine_m < deepest_ravine_m {
                deepest_ravine_m = ravine_m;
                ravine_at = GlobalXZ::at(x, z);
            }

            let step_m = 8.0_f32;
            let knoll_x = surface.debug_layers(x as f32 + step_m, z as f32).5;
            let knoll_z = surface.debug_layers(x as f32, z as f32 + step_m).5;
            let gx = (knoll_x - knoll_m) / step_m;
            let gz = (knoll_z - knoll_m) / step_m;
            let grade = (gx * gx + gz * gz).sqrt();
            if grade > 0.55 && knoll_m.max(knoll_x).max(knoll_z) > 10.0 {
                cliff_samples += 1;
            }
            if grade > max_cliff_grade {
                max_cliff_grade = grade;
                cliff_at = GlobalXZ::at(x, z);
            }
        }
    }

    assert!(
        max_cliff_grade > 0.70 && cliff_samples >= 8,
        "no real knoll scarps: max grade {max_cliff_grade:.2}, samples {cliff_samples}"
    );
    assert!(
        cliff_samples * 20 < hill_samples,
        "cliffs cover {cliff_samples}/{hill_samples} hill samples; they are not occasional"
    );
    let mut broad_hill_samples = 0usize;
    for dz in -6_i32..=6_i32 {
        for dx in -6_i32..=6_i32 {
            let x = cliff_at.x + f64::from(dx) * 100.0;
            let z = cliff_at.z + f64::from(dz) * 100.0;
            broad_hill_samples += usize::from(surface.debug_layers(x as f32, z as f32).5 > 10.0);
        }
    }
    assert!(
        broad_hill_samples >= 16,
        "scarp at ({:.0},{:.0}) is not attached to a broad hill ({broad_hill_samples} samples)",
        cliff_at.x,
        cliff_at.z
    );

    assert!(
        deepest_ravine_m < -12.0 && ravine_samples >= 8,
        "no real ravines: deepest {deepest_ravine_m:.1} m, samples {ravine_samples}"
    );
    assert!(
        ravine_samples * 100 < side * side,
        "ravines cover {ravine_samples}/{} samples; they are not rare",
        side * side
    );
    let mut paired_shoulders = false;
    for heading in 0..24 {
        let angle = heading as f64 * std::f64::consts::TAU / 24.0;
        let (sin_a, cos_a) = angle.sin_cos();
        let mut positive_bank_m = 0.0_f32;
        let mut negative_bank_m = 0.0_f32;
        for distance_m in (60..=360).step_by(20) {
            let d = f64::from(distance_m);
            positive_bank_m = positive_bank_m.max(
                surface
                    .debug_layers(
                        (ravine_at.x + cos_a * d) as f32,
                        (ravine_at.z + sin_a * d) as f32,
                    )
                    .6,
            );
            negative_bank_m = negative_bank_m.max(
                surface
                    .debug_layers(
                        (ravine_at.x - cos_a * d) as f32,
                        (ravine_at.z - sin_a * d) as f32,
                    )
                    .6,
            );
        }
        paired_shoulders |= positive_bank_m > 2.0 && negative_bank_m > 2.0;
    }
    assert!(
        paired_shoulders,
        "ravine at ({:.0},{:.0}) has no raised shoulder on both sides",
        ravine_at.x, ravine_at.z
    );

    assert_eq!(
        surface.debug_layers(cliff_at.x as f32, cliff_at.z as f32),
        surface.debug_layers(cliff_at.x as f32, cliff_at.z as f32),
        "cliff sampling is not deterministic"
    );
    assert_eq!(
        surface.debug_layers(ravine_at.x as f32, ravine_at.z as f32),
        surface.debug_layers(ravine_at.x as f32, ravine_at.z as f32),
        "ravine sampling is not deterministic"
    );
}

#[test]
fn alpine_slopes_have_a_cliff_tail() {
    // Median steepness made the ranges taller sines. A cliff is the tail: a
    // 16 m probe that is a face, not another octave of ridge.
    let surface = surface_of(20260809);
    let fields = surface.fields();
    let span = surface.bounds().metres();
    let step = 16.0;
    let probe = 90usize;
    let lattice = span / probe as f64;
    let mut slopes = Vec::new();
    for iz in 4..probe - 4 {
        for ix in 4..probe - 4 {
            let x = (ix as f64 + 0.5) * lattice;
            let z = (iz as f64 + 0.5) * lattice;
            let elev = fields.sample_smooth(&fields.elevation_m, x as f32, z as f32);
            let relief = fields.sample_smooth(&fields.relief01, x as f32, z as f32);
            if relief > 0.50 && elev > 900.0 {
                slopes.push(local_slope(&surface, x, z, step));
            }
        }
    }
    assert!(
        slopes.len() > 80,
        "expected an orogen on seed 20260809, got {} alpine probes",
        slopes.len()
    );
    slopes.sort_by(|a, b| a.total_cmp(b));
    let med = slopes[slopes.len() / 2];
    let p95 = slopes[(slopes.len() as f64 * 0.95) as usize];
    let cliffs = slopes.iter().filter(|s| **s > 0.55).count();
    assert!(
        p95 > med * 1.7,
        "alpine 95th slope {p95:.3} is not a tail on the median {med:.3}"
    );
    assert!(
        cliffs >= 4,
        "alpine has no cliff faces ({cliffs} of {} above 0.55)",
        slopes.len()
    );
}

#[test]
fn the_reported_foothills_are_not_a_ridge_comb() {
    // The view at (119665, 95758) looking yaw 204° was a comb of 40 m knoll
    // fins. An east transect missed them. This walks the look, the 25 m
    // medium grid the flyer actually sees, and checks that a knoll in the
    // neighbourhood is a hill hundreds of metres across, not a knife.
    let (_, surface) = world_of(20260809, 256);
    let x0 = 119_665.0;
    let z0 = 95_758.0;
    let yaw = 204.0_f32.to_radians();
    let (fx, fz) = (yaw.sin() as f64, yaw.cos() as f64);

    let mut h4 = Vec::with_capacity(100);
    for i in 0..100 {
        h4.push(surface.base_ground(GlobalXZ::at(
            x0 + i as f64 * 4.0 * fx,
            z0 + i as f64 * 4.0 * fz,
        )));
    }
    let cliff4 = (1..h4.len())
        .filter(|&i| (h4[i] - h4[i - 1]).abs() > 12.0)
        .count();
    let peaks4 = (2..h4.len() - 2)
        .filter(|&i| {
            let drop = h4[i] - h4[i - 1].min(h4[i + 1]);
            h4[i] > h4[i - 1] && h4[i] > h4[i + 1] && drop > 12.0
        })
        .count();
    assert!(
        cliff4 <= 3,
        "look transect has {cliff4} walls (>12 m in 4 m); knolls are still fins"
    );
    assert!(
        peaks4 <= 2,
        "look transect has {peaks4} sharp crests in 400 m; still a comb"
    );

    let mut med_peaks = 0usize;
    let mut prev = 0.0_f32;
    let mut prev2 = 0.0_f32;
    for i in 0..80 {
        let h = surface.base_ground(GlobalXZ::at(
            x0 + i as f64 * 25.0 * fx,
            z0 + i as f64 * 25.0 * fz,
        ));
        if i >= 2 && prev > prev2 && prev > h && prev - h.min(prev2) > 10.0 {
            med_peaks += 1;
        }
        prev2 = prev;
        prev = h;
    }
    assert!(
        med_peaks <= 2,
        "medium-grid look has {med_peaks} crests in 2 km; the flyer still sees a comb"
    );

    let mut steep = 0usize;
    let mut n = 0usize;
    for iz in -25..=25 {
        for ix in -25..=25 {
            let s = local_slope(&surface, x0 + ix as f64 * 8.0, z0 + iz as f64 * 8.0, 4.0);
            if s > 0.8 {
                steep += 1;
            }
            n += 1;
        }
    }
    assert!(
        steep * 20 < n,
        "foothill box is {steep}/{n} cliff samples; still a comb"
    );

    let mut max_knoll = 0.0_f32;
    let mut knoll_at = (x0 as f32, z0 as f32);
    for iz in -50..=50 {
        for ix in -50..=50 {
            let x = x0 as f32 + ix as f32 * 80.0;
            let z = z0 as f32 + iz as f32 * 80.0;
            let k = surface.debug_layers(x, z).5;
            if k > max_knoll {
                max_knoll = k;
                knoll_at = (x, z);
            }
        }
    }
    assert!(
        max_knoll > 40.0,
        "no bulky knoll in 8 km of the reported foothills (max {max_knoll:.1})"
    );
    let (kx, kz) = knoll_at;
    let wide = (-12..=12)
        .filter(|&i| surface.debug_layers(kx + i as f32 * 40.0, kz).5 > 12.0)
        .count();
    assert!(
        wide >= 8,
        "knoll at ({kx:.0},{kz:.0}) is {max_knoll:.1} m but only {wide} samples stay above 12 m; still a fin"
    );
}

fn local_slope(surface: &ContinentalSurface, x: f64, z: f64, step: f64) -> f32 {
    let h = surface.base_ground(GlobalXZ::at(x, z));
    let hx = surface.base_ground(GlobalXZ::at(x + step, z));
    let hz = surface.base_ground(GlobalXZ::at(x, z + step));
    let gx = (hx - h) / step as f32;
    let gz = (hz - h) / step as f32;
    (gx * gx + gz * gz).sqrt()
}

#[test]
fn a_pond_is_the_same_pond_whichever_window_found_it() {
    // The whole basis of a moving window: a chunk baked before a rebuild and
    // its neighbour baked after must be looking at the same water. Two windows
    // a kilometre and a half apart share a lot of ground, and every pond on
    // that ground has to come out identical in both.
    let surface = surface_of(20260809);
    let mid = surface.bounds().metres() / 2.0;
    let here = GlobalXZ::at(mid, mid);
    let there = GlobalXZ::at(mid + 1_500.0, mid - 900.0);
    let a = PondField::build(&surface, here);
    let b = PondField::build(&surface, there);

    let shared = Vec2::new(mid as f32 + 700.0, mid as f32 - 400.0);
    let near = |field: &PondField| -> Vec<[f32; 4]> {
        let mut found: Vec<[f32; 4]> = field
            .ponds()
            .iter()
            .filter(|pond| pond.centre().distance(shared) < 2_200.0)
            .map(|pond| {
                [
                    pond.centre().x,
                    pond.centre().y,
                    pond.sheet_z(),
                    pond.reach_m(),
                ]
            })
            .collect();
        found.sort_by(|l, r| l[0].partial_cmp(&r[0]).expect("finite ponds"));
        found
    };
    let (from_here, from_there) = (near(&a), near(&b));
    assert!(
        from_here.len() >= 2,
        "expected a sample of ponds, got {}",
        from_here.len()
    );
    assert_eq!(
        from_here, from_there,
        "the two windows disagree about the ponds on the ground they share"
    );
}

#[test]
fn a_pond_holds_water_over_its_whole_floor() {
    let surface = surface_of(20260809);
    let mid = surface.bounds().metres() / 2.0;
    let field = PondField::build(&surface, GlobalXZ::at(mid, mid));
    assert!(field.ponds().len() > 20);

    let mut floor = 0;
    for pond in field.ponds() {
        for iz in -8..=8 {
            for ix in -8..=8 {
                let p = pond.centre() + Vec2::new(ix as f32 * 6.0, iz as f32 * 6.0);
                if !pond.contains(p) {
                    continue;
                }
                let mut column = surface.column(GlobalXZ::at(p.x as f64, p.y as f64));
                if column.is_wet() {
                    // Atlas water was already here and wins outright.
                    continue;
                }
                field.carve(GlobalXZ::at(p.x as f64, p.y as f64), &mut column);
                assert!(
                    column.ground() <= pond.sheet_z() - MIN_WATER_DEPTH,
                    "a pond floor stands {:.2} m above its own surface",
                    column.ground() - pond.sheet_z()
                );
                assert_eq!(column.water_top(), Some(pond.sheet_z()));
                floor += 1;
            }
        }
    }
    assert!(floor > 200, "only {floor} points fell inside a pond");
}

#[test]
fn a_pond_does_not_hang_a_sheet_over_a_hillside() {
    // The old rim was the highest ground on an 80 m ray. In the mountains that
    // is a ridge, the flood fill then takes every cell under that height, and
    // the contour draws a pane in the air. A pond only cuts, so its sheet has
    // to sit in the hollow — a few metres above the uncarved floor — even on
    // high ground.
    let surface = surface_of(20260809);
    let mut foci = vec![GlobalXZ::at(
        surface.bounds().metres() / 2.0,
        surface.bounds().metres() / 2.0,
    )];
    let fields = surface.fields();
    let span = surface.bounds().metres();
    let mut alpine = 0;
    for iz in 10..50 {
        for ix in 10..50 {
            let x = span * (ix as f64 + 0.5) / 64.0;
            let z = span * (iz as f64 + 0.5) / 64.0;
            let elev = fields.sample_smooth(&fields.elevation_m, x as f32, z as f32);
            let relief = fields.sample_smooth(&fields.relief01, x as f32, z as f32);
            if relief > 0.5 && elev > 700.0 {
                foci.push(GlobalXZ::at(x, z));
                alpine += 1;
                if alpine >= 4 {
                    break;
                }
            }
        }
        if alpine >= 4 {
            break;
        }
    }

    let mut seen = 0;
    for focus in foci {
        let field = PondField::build(&surface, focus);
        for pond in field.ponds() {
            for iz in -10..=10 {
                for ix in -10..=10 {
                    let p = pond.centre() + Vec2::new(ix as f32 * 6.0, iz as f32 * 6.0);
                    if !pond.contains(p) {
                        continue;
                    }
                    let ground = surface.base_ground(GlobalXZ::at(p.x as f64, p.y as f64));
                    let drop = pond.sheet_z() - ground;
                    assert!(
                        drop < 14.0,
                        "a pond at ({:.0}, {:.0}) hangs its sheet {drop:.1} m over the ground",
                        p.x,
                        p.y
                    );
                    seen += 1;
                }
            }
        }
    }
    assert!(seen > 50, "only {seen} pond-floor samples");
}

#[test]
fn a_pond_does_not_leave_dry_islands_under_its_sheet() {
    // Skipping cells that sat too far below the sheet left holes in the water,
    // and the shore drape pulled the mesh down around them. If the ground is
    // under the sheet and the cells around it are wet, that cell is bed.
    let surface = surface_of(20260809);
    let mid = surface.bounds().metres() / 2.0;
    let field = PondField::build(&surface, GlobalXZ::at(mid, mid));
    let step = CHUNK_SAMPLE_M as f32;
    for pond in field.ponds() {
        let origin = Vec2::new(
            (pond.centre().x / step).floor() * step + step * 0.5,
            (pond.centre().y / step).floor() * step + step * 0.5,
        );
        for iz in -20..=20 {
            for ix in -20..=20 {
                let p = origin + Vec2::new(ix as f32 * step, iz as f32 * step);
                if pond.centre().distance(p) > pond.reach_m() {
                    continue;
                }
                if pond.contains(p) {
                    continue;
                }
                let ground = surface.base_ground(GlobalXZ::at(p.x as f64, p.y as f64));
                if pond.sheet_z() - ground <= 0.0 {
                    continue;
                }
                let wet_n = [(-step, 0.0), (step, 0.0), (0.0, -step), (0.0, step)]
                    .iter()
                    .filter(|(dx, dz)| pond.contains(p + Vec2::new(*dx, *dz)))
                    .count();
                assert!(
                    wet_n < 4,
                    "a dry island under the sheet at ({:.0}, {:.0})",
                    p.x,
                    p.y
                );
            }
        }
    }
    assert!(field.ponds().len() > 5, "no ponds to check for islands");
}

#[test]
fn a_window_rebuild_leaves_the_ground_and_the_water_where_they_were() {
    // The seam contract, from the one direction the sub-atlas layer can break
    // it: a chunk baked while the window sat one place and its neighbour baked
    // after the window moved. Both windows cover this ground, so both have to
    // cut it identically or there is a step in the pond at the seam.
    let (_atlas, surface) = world_of(20260809, 48);
    let mid = surface.bounds().metres() / 2.0;
    let here = GlobalXZ::at(mid, mid);
    let shared: super::SharedPonds = Arc::new(std::sync::RwLock::new(Arc::new(PondField::build(
        &surface, here,
    ))));
    let builder = TerrainChunkBuilder::new(Arc::clone(&surface)).with_ponds(Arc::clone(&shared));

    let coord = busiest_chunk(&shared.read().expect("field"), here);
    let before = builder.build(coord).expect("build").expect("content");

    // Move the window as far as it can go while still speaking for this chunk.
    let there = GlobalXZ::at(mid + 2_000.0, mid + 1_200.0);
    *shared.write().expect("field") = Arc::new(PondField::build(&surface, there));
    let after = builder.build(coord).expect("build").expect("content");

    let (land_a, water_a) = split_layers(&before);
    let (land_b, water_b) = split_layers(&after);
    heights_agree(&land_a.positions, &land_b.positions, "the ground");
    let (water_a, water_b) = (
        water_a.expect("ponds in the busiest chunk"),
        water_b.expect("ponds in the busiest chunk"),
    );
    heights_agree(&water_a.positions, &water_b.positions, "the water");
}

/// A millimetre of height on a four-metre grid is not a seam; a step is.
fn heights_agree(a: &[Vec3], b: &[Vec3], what: &str) {
    assert_eq!(a.len(), b.len(), "{what} vertex count changed");
    let mut max_dy = 0.0f32;
    for (pa, pb) in a.iter().zip(b) {
        assert!(
            (pa.x - pb.x).abs() < 1e-4 && (pa.z - pb.z).abs() < 1e-4,
            "{what} vertex slid in plan"
        );
        max_dy = max_dy.max((pa.y - pb.y).abs());
    }
    assert!(
        max_dy < 0.02,
        "{what} moved {max_dy:.3} m between windows that both cover this chunk"
    );
}

/// The chunk holding the pond nearest `focus`.
///
/// A seam test over ground with no pond on it proves nothing, so the test
/// picks its own subject rather than hoping the window centre is wet.
fn busiest_chunk(field: &PondField, focus: GlobalXZ) -> ChunkCoord {
    let span = chunk_span();
    let focus2 = Vec2::new(focus.x as f32, focus.z as f32);
    let pond = field
        .ponds()
        .iter()
        .min_by(|a, b| {
            a.centre()
                .distance(focus2)
                .total_cmp(&b.centre().distance(focus2))
        })
        .expect("the window holds a pond");
    ChunkCoord::containing(
        GlobalXZ::at(f64::from(pond.centre().x), f64::from(pond.centre().y)),
        span,
    )
}

#[test]
fn a_pond_is_contoured_with_the_rest_of_the_water() {
    // A pond is seated on the same 4 m columns as the land, so the marching
    // squares that draw every other body have to see it as wet. Hiding it from
    // them was what forced a second mesh, and that mesh is what floated.
    let (_atlas, surface) = world_of(20260809, 48);
    let mid = surface.bounds().metres() / 2.0;
    let here = GlobalXZ::at(mid, mid);
    let field = Arc::new(PondField::build(&surface, here));

    let mut carved = 0;
    for pond in field.ponds().iter().take(40) {
        for iz in -6..=6 {
            for ix in -6..=6 {
                let point = pond.centre() + Vec2::new(ix as f32 * 4.0, iz as f32 * 4.0);
                if !pond.contains(point) {
                    continue;
                }
                let p = GlobalXZ::at(point.x as f64, point.y as f64);
                let mut column = surface.column(p);
                if column.is_wet() {
                    continue;
                }
                field.carve(p, &mut column);
                let Some(super::WaterBody::Pond) = column.body() else {
                    continue;
                };
                assert!(column.wetness() >= 0.0, "a pond is not wet");
                assert!(
                    column.contour_wetness() >= 0.0,
                    "a pond would be hidden from the contour that draws it"
                );
                let top = column.water_top().expect("a wet pond has a sheet");
                assert!(
                    column.ground() < top,
                    "a pond's sheet sits in the ground, not over it"
                );
                carved += 1;
            }
        }
    }
    assert!(carved > 40, "only {carved} points landed in a pond");

    let shared: super::SharedPonds = Arc::new(std::sync::RwLock::new(Arc::clone(&field)));
    let builder = TerrainChunkBuilder::new(Arc::clone(&surface)).with_ponds(shared);
    let coord = busiest_chunk(&field, here);
    let payload = builder.build(coord).expect("build").expect("content");
    let water = split_layers(&payload).1.expect("a drawn pond");
    assert!(
        water.indices.len() >= 6,
        "the busiest chunk on the map has no water triangles"
    );
}

#[test]
fn only_reeds_stand_in_the_shallows_and_no_tree_stands_in_a_pond() {
    assert!(
        PropClass::Reed.stands_in(0.5),
        "a reed belongs in the water"
    );
    assert!(!PropClass::Reed.stands_in(1.6), "a reed cannot swim");
    assert!(
        !PropClass::Reed.stands_in(-6.0),
        "a reed does not grow up the bank"
    );
    for class in super::PROP_CLASSES {
        if class == PropClass::Reed {
            continue;
        }
        assert!(
            !class.stands_in(0.3),
            "{class:?} would stand in standing water"
        );
    }
    // A pond's own margin is a metre or two, so keeping trees out of the
    // water is exactly the tree margin.
    assert!(!PropClass::Tree.stands_in(-1.0));
    assert!(PropClass::Tree.stands_in(-8.0));
}

#[test]
fn bank_cover_needs_a_bank() {
    let (_atlas, surface) = world_of(20260809, 48);
    let p = dry_inland(&surface);
    let ground = surface.column(p).ground();
    let seed = surface.world_seed() as u32 as u64;
    let dry = super::GroundCover::sample(seed, &surface, p, ground, super::Fall::default(), 0.0)
        .with_water(-500.0);
    let wet = super::GroundCover::sample(seed, &surface, p, ground, super::Fall::default(), 0.0)
        .with_water(0.0);
    assert_eq!(dry.reed, 0.0, "reeds grew half a kilometre from any water");
    assert_eq!(dry.bank, 0.0);
    assert!(wet.reed > 0.3, "no reeds at the waterline");
    assert!(wet.bush > dry.bush, "scrub is no thicker along a bank");
    assert!(wet.tree > dry.tree, "no gallery woodland along the water");
}

/// The scatter settles most of its lattice on `water_reach` alone, and skips
/// the column entirely when it reads dry. If the bound could ever come in under
/// the real figure, a tree would stand in a river and nothing would catch it.
#[test]
fn the_cheap_water_bound_never_claims_ground_is_drier_than_it_is() {
    let (_atlas, surface) = world_of(20260809, 48);
    let field = PondField::build(&surface, dry_inland(&surface));
    let probe = 220usize;
    let step = surface.bounds().metres() / probe as f64;
    let mut wet = 0usize;
    for iz in 0..probe {
        for ix in 0..probe {
            let p = GlobalXZ::at((ix as f64 + 0.5) * step, (iz as f64 + 0.5) * step);
            let mut column = surface.column(p);
            field.carve(p, &mut column);
            let bound = surface
                .water_reach(p)
                .max(field.water_reach(Vec2::new(p.x as f32, p.z as f32)));
            assert!(
                bound >= column.wetness() - 1e-3,
                "at ({:.0}, {:.0}) the bound reads {bound:.2} m but the column is {:.2} m",
                p.x,
                p.z,
                column.wetness()
            );
            wet += usize::from(column.is_wet());
        }
    }
    assert!(wet > 100, "only {wet} wet samples: nothing was proved");
}

/// Ordinary dry ground well above the beach, for judging cover on.
fn dry_inland(surface: &ContinentalSurface) -> GlobalXZ {
    let probe = 96usize;
    let step = surface.bounds().metres() / probe as f64;
    let sea = surface.sea_surface_z();
    for iz in 0..probe {
        for ix in 0..probe {
            let p = GlobalXZ::at((ix as f64 + 0.5) * step, (iz as f64 + 0.5) * step);
            let column = surface.column(p);
            if !column.is_wet() && column.ground() > sea + 40.0 && column.ground() < 400.0 {
                return p;
            }
        }
    }
    panic!("the continent has no ordinary dry ground on it");
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
            .column_for_grid(
                GlobalXZ::at(x0 + dx * s, z0 + dz * s),
                tier.sample_m as f32,
                tier.sink_m,
            )
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
        assert_eq!(
            a_land.uvs[ai], b_land.uvs[bi],
            "edge soil splat differs at row {iz}"
        );
    }
    assert_eq!(a_land.uvs.len(), a_land.positions.len());
    for uv in &a_land.uvs {
        assert!(
            (0.0..=1.0).contains(&uv[0]) && (0.0..=1.0).contains(&uv[1]),
            "soil splat {uv:?} is not a weight"
        );
        assert!(
            uv[0] + uv[1] <= 1.0 + 1e-5,
            "soil splat {uv:?} sums past lush"
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

#[test]
fn inland_grass_is_not_one_soil() {
    // Dry sward and peat have to actually win somewhere, or the extra albedos
    // are dead bindings and the continent is one green tile with a tint. The
    // geographic centre is often alpine, so sample a diagonal band (and its
    // neighbours) that crosses low woods and sunny flanks.
    let (atlas, surface) = world_of(1, 48);
    let builder = TerrainChunkBuilder::new(Arc::clone(&surface));
    let span = atlas.size as f64 * CELL_METRES as f64 / CHUNK_SPAN_M;
    let mut dry = 0u32;
    let mut moor = 0u32;
    for t in [0.22, 0.28, 0.34, 0.40, 0.46, 0.52, 0.58, 0.64] {
        let c = (span * t) as i32;
        for (dx, dz) in [(0, 0), (1, 0), (0, 1), (-1, 0), (0, -1)] {
            let Some(payload) = builder
                .build(ChunkCoord::new(c + dx, c + dz))
                .expect("build")
            else {
                continue;
            };
            let land = payload
                .layer(engine::space::ChunkLayer::Land)
                .expect("land");
            for uv in &land.uvs {
                if uv[0] > 0.35 {
                    dry += 1;
                }
                if uv[1] > 0.35 {
                    moor += 1;
                }
            }
        }
    }
    assert!(
        dry > 80,
        "sunny/arid ground never took the straw albedo ({dry} verts)"
    );
    assert!(
        moor > 80,
        "banks and woods never took the peat albedo ({moor} verts)"
    );
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
    let ponds = PondField::build(&surface, request.requested());
    let pose = resolve_spawn(&surface, &ponds, request).expect("spawn beside the river");

    let mut standing = surface.column(pose.ground());
    ponds.carve(pose.ground(), &mut standing);
    assert!(!standing.is_wet(), "spawned in water");
    assert!(
        pose.offset_m() <= 480.0,
        "resolver wandered {} m from the request",
        pose.offset_m()
    );
    let again = resolve_spawn(&surface, &ponds, request).expect("spawn");
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
    // No pond has ever reached open ocean, so an empty window is the truth
    // here and saves scanning a continent to prove it.
    assert!(matches!(
        resolve_spawn(&surface, &PondField::empty(p), request),
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
    let mut session = WorldSession::new(Arc::clone(&surface)).with_instant_travel();
    session.begin_entry(&mut world, request).expect("entry");
    wait_until_world(&mut session, &mut world);
    Some((world, session))
}

/// Pond scanning and the entry ring take a few seconds in debug; the loading
/// screen covers that. Tests wait for the real work, not a short wall clock.
fn wait_until_world(session: &mut WorldSession, world: &mut World) {
    session.wait_until_world(world);
}

fn wait_until_world_for(session: &mut WorldSession, world: &mut World, budget: Duration) {
    session.wait_until_world_for(world, budget);
}

#[test]
fn the_saved_stand_enters_the_world() {
    // The live save is where Continue stalls at "streaming ground 0%".
    let Some(stand) = crate::save::SavedStand::read(20260809, 256).ok().flatten() else {
        return;
    };
    println!(
        "saved stand ({:.0}, {:.0}) seed={} size={}",
        stand.x, stand.z, stand.seed, stand.size
    );
    let (atlas, surface) = world_of(stand.seed, stand.size);
    let bounds = AtlasBounds::of(&atlas);
    let request = WorldEntryRequest::at_global(bounds, stand.at()).expect("saved point");
    let mut world = World::new();
    let mut session = WorldSession::new(Arc::clone(&surface)).with_instant_travel();
    match session.begin_entry(&mut world, request) {
        Ok(()) => {}
        Err(SessionError::Entry(EntryError::NoSpawn { .. })) => {
            // Old builds could save dungeon-local coordinates as a continent
            // stand. Startup reports this and falls back to the largest town.
            return;
        }
        Err(err) => panic!("saved entry failed unexpectedly: {err}"),
    }
    wait_until_world_for(&mut session, &mut world, Duration::from_secs(180));
    assert_eq!(session.state(), SessionState::World);
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
    let mut session = WorldSession::new(Arc::clone(&surface)).with_instant_travel();
    assert_eq!(session.state(), SessionState::Atlas);
    session.begin_entry(&mut world, request).expect("entry");
    assert_eq!(session.state(), SessionState::Travel);
    // The spawn is not known at the moment of the request: the water under it has
    // to be traced first, and that happens behind the loading screen.
    assert!(session.spawn().is_none());

    wait_until_world(&mut session, &mut world);

    let feet = session.player_position().expect("player");
    let contact = session
        .contact_height(feet.horizontal())
        .expect("spawn chunk must carry contact");
    assert!(
        (feet.y as f32 - contact).abs() < 0.2,
        "the player must stand on the drawn ground"
    );
    let pose = session
        .spawn()
        .expect("a spawn, once the world is standing");
    assert!(
        (feet.y - pose.position().y).abs() < 5.0,
        "resolved and resident spawn heights disagree"
    );

    session.return_to_atlas();
    assert_eq!(session.state(), SessionState::Atlas);
    session.resume().expect("resume");
    assert_eq!(session.state(), SessionState::World);
}

fn land_request(seed: i32, size: usize) -> Option<(Arc<ContinentalSurface>, WorldEntryRequest)> {
    let (atlas, surface) = world_of(seed, size);
    let bounds = AtlasBounds::of(&atlas);
    let mid = river_reach(&atlas)?;
    let request = WorldEntryRequest::at_global(bounds, GlobalXZ::at(mid.x as f64, mid.y as f64))
        .expect("river point");
    Some((surface, request))
}

fn ocean_request(seed: i32, size: usize) -> Option<(Arc<ContinentalSurface>, WorldEntryRequest)> {
    let (atlas, surface) = world_of(seed, size);
    let bounds = AtlasBounds::of(&atlas);
    let hydro = surface.hydro();
    for az in 0..atlas.size {
        for ax in 0..atlas.size {
            let idx = az * atlas.size + ax;
            if hydro.cell_coasts[idx].is_empty() && hydro.cell_lakes[idx].is_empty() {
                let p = GlobalXZ::at(
                    (ax as f64 + 0.5) * CELL_METRES as f64,
                    (az as f64 + 0.5) * CELL_METRES as f64,
                );
                let request = WorldEntryRequest::at_global(bounds, p).expect("in bounds");
                return Some((surface, request));
            }
        }
    }
    None
}

#[test]
fn a_bad_travel_target_leaves_the_source_world_alone() {
    let Some((surface, land)) = land_request(1, 48) else {
        return;
    };
    let Some((_, ocean)) = ocean_request(1, 48) else {
        return;
    };
    let mut world = World::new();
    let mut session = WorldSession::new(Arc::clone(&surface)).with_instant_travel();
    session.begin_entry(&mut world, land).expect("land");
    wait_until_world(&mut session, &mut world);
    let origin = world.render_origin();
    let chunks = session.stream().resident_count();
    let feet = session.player_position().expect("player");

    let err = session.begin_entry(&mut world, ocean).expect_err("ocean");
    assert!(matches!(err, super::SessionError::Entry(_)));
    assert_eq!(session.state(), SessionState::World);
    assert_eq!(session.stream().resident_count(), chunks);
    assert_eq!(world.render_origin(), origin);
    let still = session.player_position().expect("player");
    assert_eq!(still.x, feet.x);
    assert_eq!(still.z, feet.z);
}

#[test]
fn travel_resets_the_destination_stream_once() {
    let Some((surface, request)) = land_request(1, 48) else {
        return;
    };
    let mut world = World::new();
    let mut session = WorldSession::new(surface).with_instant_travel();
    session.begin_entry(&mut world, request).expect("entry");
    assert_eq!(session.state(), SessionState::Travel);
    assert_eq!(session.travel_handoffs(), 1);
    wait_until_world(&mut session, &mut world);
    assert_eq!(session.state(), SessionState::World);
    assert_eq!(session.travel_handoffs(), 0);
}

#[test]
fn travel_keeps_the_pointer_unlocked() {
    let Some((surface, request)) = land_request(1, 48) else {
        return;
    };
    let mut world = World::new();
    let mut session = WorldSession::new(surface).with_instant_travel();
    session.begin_entry(&mut world, request).expect("entry");
    session
        .step(
            &mut world,
            WalkInput {
                capture_look: true,
                skip_travel: true,
                ..WalkInput::IDLE
            },
        )
        .expect("travel");
    if session.state() == SessionState::Travel {
        assert!(!world.pointer_lock(), "travel must not capture the mouse");
    }
}

#[test]
fn skip_cannot_land_before_the_destination_is_ready() {
    let Some((surface, request)) = land_request(1, 48) else {
        return;
    };
    let mut world = World::new();
    let mut session = WorldSession::new(surface);
    session.set_travel_timings(TravelTimings::cinematic());
    session.begin_entry(&mut world, request).expect("entry");
    assert_eq!(session.travel_phase(), Some(TravelPhase::Transfer));
    session
        .step(
            &mut world,
            WalkInput {
                skip_travel: true,
                dt: 1.0 / 60.0,
                ..WalkInput::IDLE
            },
        )
        .expect("skip");
    assert_eq!(session.state(), SessionState::Travel);
    assert!(!session.destination_ready());
    assert_ne!(session.travel_phase(), Some(TravelPhase::Descent));
}

#[test]
fn travel_holds_until_the_ring_is_ready_then_lands_on_contact() {
    let Some((surface, request)) = land_request(1, 48) else {
        return;
    };
    let mut world = World::new();
    let mut session = WorldSession::new(surface).with_instant_travel();
    session.begin_entry(&mut world, request).expect("entry");
    let mut saw_hold = false;
    let deadline = Instant::now() + Duration::from_secs(90);
    while Instant::now() < deadline {
        session.step(&mut world, WalkInput::IDLE).expect("update");
        if session.travel_phase() == Some(TravelPhase::Hold) {
            saw_hold = true;
        }
        if session.state() == SessionState::World {
            break;
        }
    }
    assert_eq!(session.state(), SessionState::World);
    assert!(
        saw_hold
            || session
                .stream()
                .required_ready(session.spawn().expect("spawn").ground()),
        "a slow ring must hold; an instant ring may skip the visible hold"
    );
    let feet = session.player_position().expect("player");
    let contact = session.contact_height(feet.horizontal()).expect("contact");
    assert!(
        (feet.y as f32 - contact).abs() < 0.2,
        "travel must land on the drawn ground"
    );
}

#[test]
fn mid_walk_travel_ascends_before_it_resets() {
    let Some((mut world, mut session)) = standing_session(1, 48) else {
        return;
    };
    let Some((surface, next)) = land_request(3, 48) else {
        return;
    };
    // Same continent: reuse the standing session's surface by picking another
    // point on this atlas, not a different seed.
    let _ = surface;
    let Some(pin) = session.surface().largest_settlement() else {
        return;
    };
    let bounds = session.surface().bounds();
    let next = WorldEntryRequest::at_global(bounds, pin.at).unwrap_or(next);
    session.set_travel_timings(TravelTimings::cinematic());
    let chunks_before = session.stream().resident_count();
    session.begin_entry(&mut world, next).expect("re-entry");
    assert_eq!(session.travel_phase(), Some(TravelPhase::Ascent));
    assert_eq!(session.travel_handoffs(), 0);
    assert_eq!(session.stream().resident_count(), chunks_before);
    session
        .step(
            &mut world,
            WalkInput {
                skip_travel: true,
                dt: 1.0 / 60.0,
                ..WalkInput::IDLE
            },
        )
        .expect("skip ascent");
    assert_eq!(session.travel_handoffs(), 1);
    assert_eq!(session.state(), SessionState::Travel);
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
    let mut session = WorldSession::new(Arc::clone(&surface)).with_instant_travel();
    session.begin_entry(&mut world, request).expect("entry");
    wait_until_world(&mut session, &mut world);
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
        direction: Vec3::Y,
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

    let along = Vec3::new(0.6, 0.8, 0.0).normalize();
    session
        .step(
            &mut world,
            WalkInput {
                direction: along,
                step_m: 10.0,
                ..WalkInput::IDLE
            },
        )
        .expect("fly along the look");
    let along_pos = session.player_position().expect("player");
    assert!(
        along_pos.y > up.y + 7.0,
        "a look-up flight must climb: {} vs {}",
        along_pos.y,
        up.y
    );
    assert!(
        (along_pos.x - up.x).abs() > 5.0,
        "a look-up flight must also travel forward: {} vs {}",
        along_pos.x,
        up.x
    );

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
    let floor = session
        .contact_height(landed.horizontal())
        .expect("contact underfoot") as f64
        + 0.05;
    assert!(
        (landed.y - floor).abs() < 0.2,
        "landing must snap back to the drawn ground: {} vs {floor}",
        landed.y
    );

    session
        .step(
            &mut world,
            WalkInput {
                jump: true,
                dt: 1.0 / 60.0,
                ..WalkInput::IDLE
            },
        )
        .expect("jump");
    let sprung = session.player_position().expect("player");
    assert!(
        sprung.y > landed.y + 0.05,
        "space must leave the ground: {} vs {}",
        sprung.y,
        landed.y
    );
    for _ in 0..90 {
        session
            .step(
                &mut world,
                WalkInput {
                    dt: 1.0 / 60.0,
                    ..WalkInput::IDLE
                },
            )
            .expect("fall");
    }
    let after_jump = session.player_position().expect("player");
    assert!(
        (after_jump.y - floor).abs() < 0.2,
        "a jump must land back on the drawn ground: {} vs {floor}",
        after_jump.y
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
    let mut session = WorldSession::new(Arc::clone(&surface)).with_instant_travel();
    session.begin_entry(&mut world, request).expect("entry");
    wait_until_world(&mut session, &mut world);

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
    let ponds = PondField::build(&surface, GlobalXZ::at(mid.x as f64, mid.y as f64));
    let pose = resolve_spawn(&surface, &ponds, request).expect("spawn");

    let mut world = World::new();
    let mut stream = WorldStream::new(
        Arc::clone(&surface),
        PondWindow::new(Arc::clone(&surface)).shared(),
    )
    .with_visual_ring(2);
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
    let mut stream = WorldStream::new(
        Arc::clone(&surface),
        PondWindow::new(Arc::clone(&surface)).shared(),
    )
    .with_visual_ring(1);
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
fn an_ocean_bound_river_reaches_the_sea() {
    // The polyline used to end at the last land-cell centre. The coast ring
    // meanders on, and the mouth was a capsule on the beach. It has to stand
    // in the water it was authored to drain into.
    let (atlas, surface) = world_of(20260809, 64);
    let mut mouths = 0;
    for river in &atlas.hydro.rivers {
        if !matches!(river.sink, HydroSink::Ocean) || !river.at_sink {
            continue;
        }
        let mouth = *river.points.last().expect("a river has a mouth");
        let column = surface.column(GlobalXZ::at(mouth.x as f64, mouth.y as f64));
        let coast = surface.hydro_index().coast_signed(surface.hydro(), mouth);
        assert!(
            column.is_wet() && coast < 0.0,
            "ocean-bound river {} stops {coast:.0} m inland at ({:.0}, {:.0})",
            river.id,
            mouth.x,
            mouth.y
        );
        mouths += 1;
    }
    assert!(mouths > 0, "expected an ocean-bound river");
}

#[test]
fn a_lake_bound_river_reaches_its_lake() {
    let (atlas, surface) = world_of(20260809, 64);
    let mut mouths = 0;
    for river in &atlas.hydro.rivers {
        if !river.at_sink {
            continue;
        }
        let HydroSink::Lake { lake_id } = river.sink else {
            continue;
        };
        let mouth = *river.points.last().expect("a river has a mouth");
        let Some((_, sd)) = surface.hydro_index().nearest_lake(surface.hydro(), mouth) else {
            panic!(
                "lake-bound river {} has no lake near ({:.0}, {:.0})",
                river.id, mouth.x, mouth.y
            );
        };
        assert!(
            sd >= -river.half_width_m,
            "lake-bound river {} stops {sd:.0} m short of lake {lake_id} at ({:.0}, {:.0})",
            river.id,
            mouth.x,
            mouth.y
        );
        mouths += 1;
    }
    assert!(mouths > 0, "expected a lake-bound river");
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
            if hydro.cell_coasts[az * size + ax].is_empty() && hydro.is_atlas_ocean(az * size + ax)
            {
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

#[test]
fn dungeon_generate_starts_before_the_player_arrives() {
    let (atlas, surface) = world_of(20260816, 64);
    let pin = *surface
        .dungeon_pins()
        .first()
        .expect("atlas 20260816 size 64 must plant a dungeon");
    let bounds = AtlasBounds::of(&atlas);
    let request = WorldEntryRequest::at_global(bounds, pin.at).expect("dungeon pin");
    let mut world = World::new();
    let mut session = WorldSession::new(Arc::clone(&surface)).with_instant_travel();
    session.begin_entry(&mut world, request).expect("entry");
    for _ in 0..40 {
        session.step(&mut world, WalkInput::IDLE).expect("update");
        if session.dungeon_generating() {
            let status = session
                .dungeon_build_status()
                .expect("HUD names a dungeon that is still being cut");
            assert!(
                status.contains("cutting"),
                "dungeon HUD should say it is cutting, got {status}"
            );
            return;
        }
    }
    panic!("dungeon generate did not start during entry");
}
