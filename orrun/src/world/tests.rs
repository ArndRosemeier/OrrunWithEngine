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
    chunk_span, resolve_spawn, AtlasBounds, AtlasCell, Brook, BrookField, BrookWindow,
    ContinentalSurface, EntryError, Locomotion, MapPoint, PropClass, ScatterCatalog, SessionState,
    Terminus, TerrainChunkBuilder, WalkInput, WorldEntryRequest, WorldSession, WorldStream,
    CHUNK_SAMPLE_M, CHUNK_SPAN_M, MIN_WATER_DEPTH,
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
    assert!(catalog.count_of(PropClass::Bush) >= 6);

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
#[ignore = "diagnostic: what a window of brooks costs and what it holds"]
fn what_a_brook_window_costs() {
    for seed in [3, 20260809] {
        let surface = surface_of(seed);
        let mid = surface.bounds().metres() / 2.0;
        let started = Instant::now();
        let field = BrookField::build(&surface, GlobalXZ::at(mid, mid));
        let took = started.elapsed().as_secs_f32() * 1000.0;
        let mut ends: std::collections::BTreeMap<String, usize> = Default::default();
        for brook in field.brooks() {
            *ends.entry(format!("{:?}", brook.terminus())).or_default() += 1;
        }
        let length: f32 = field.brooks().iter().map(Brook::length_m).sum();
        eprintln!(
            "seed {seed}: {} brooks ({:.1} km), {} ponds in {took:.0} ms — {ends:?}",
            field.brooks().len(),
            length / 1000.0,
            field.ponds().len(),
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
    // Real ranges bunch, skip, and throw spurs. Walk a few alpine transects
    // and refuse a profile whose peaks keep the loft wavelength.
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
        for i in 2..samples - 2 {
            if h[i] > h[i - 1]
                && h[i] > h[i + 1]
                && h[i] >= h[i - 2]
                && h[i] >= h[i + 2]
                && h[i] - h[i - 1].min(h[i + 1]) > 12.0
            {
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

fn local_slope(surface: &ContinentalSurface, x: f64, z: f64, step: f64) -> f32 {
    let h = surface.base_ground(GlobalXZ::at(x, z));
    let hx = surface.base_ground(GlobalXZ::at(x + step, z));
    let hz = surface.base_ground(GlobalXZ::at(x, z + step));
    let gx = (hx - h) / step as f32;
    let gz = (hz - h) / step as f32;
    (gx * gx + gz * gz).sqrt()
}

#[test]
fn a_brook_is_the_same_brook_whichever_window_found_it() {
    // The whole basis of a moving window: a chunk baked before a rebuild and
    // its neighbour baked after must be looking at the same water. Two windows
    // a kilometre and a half apart share a lot of ground, and every brook on
    // that ground has to come out identical in both.
    let surface = surface_of(20260809);
    let mid = surface.bounds().metres() / 2.0;
    let here = GlobalXZ::at(mid, mid);
    let there = GlobalXZ::at(mid + 1_500.0, mid - 900.0);
    let a = BrookField::build(&surface, here);
    let b = BrookField::build(&surface, there);

    let shared = Vec2::new(mid as f32 + 700.0, mid as f32 - 400.0);
    let near = |field: &BrookField| -> Vec<Vec<[f32; 3]>> {
        let mut found: Vec<Vec<[f32; 3]>> = field
            .brooks()
            .iter()
            .filter(|brook| brook.points().iter().any(|p| p.distance(shared) < 1_500.0))
            .map(|brook| {
                brook
                    .points()
                    .iter()
                    .zip(brook.sheets())
                    .map(|(p, z)| [p.x, p.y, *z])
                    .collect()
            })
            .collect();
        found.sort_by(|l, r| l[0].partial_cmp(&r[0]).expect("finite brooks"));
        found
    };
    let (from_here, from_there) = (near(&a), near(&b));
    assert!(
        from_here.len() >= 8,
        "expected a good sample of brooks, got {}",
        from_here.len()
    );
    assert_eq!(
        from_here, from_there,
        "the two windows disagree about the brooks on the ground they share"
    );
}

#[test]
fn a_brook_never_runs_uphill_and_always_ends_somewhere() {
    let surface = surface_of(20260809);
    let mid = surface.bounds().metres() / 2.0;
    let field = BrookField::build(&surface, GlobalXZ::at(mid, mid));
    assert!(field.brooks().len() > 100);

    for brook in field.brooks() {
        for (i, pair) in brook.sheets().windows(2).enumerate() {
            if i + 2 == brook.sheets().len() && brook.terminus() != Terminus::Soaks {
                continue;
            }
            assert!(
                pair[1] <= pair[0],
                "a sheet rose {:.2} m at step {i} of a {:?} brook",
                pair[1] - pair[0],
                brook.terminus()
            );
        }
        assert!(
            brook.length_m() <= super::MAX_BROOK_LEN_M as f32 * 1.05,
            "a brook ran {:.0} m past its cap",
            brook.length_m()
        );
    }

    // Soaking away is a real end for a small brook, but it is the least
    // convincing one, so it must not be what most of them do.
    let soaks = field
        .brooks()
        .iter()
        .filter(|b| b.terminus() == Terminus::Soaks)
        .count();
    assert!(
        soaks * 2 < field.brooks().len(),
        "{soaks} of {} brooks just stop",
        field.brooks().len()
    );
}

#[test]
fn a_brook_lies_in_the_ground_it_runs_through() {
    // The landform the trace follows is a 90 m average, which in a valley is
    // the height of the hills around it. The sheet used to sit on that average,
    // and the water hung in the air. It has to sit on the ground that is there.
    let surface = surface_of(20260809);
    let mid = surface.bounds().metres() / 2.0;
    let field = BrookField::build(&surface, GlobalXZ::at(mid, mid));
    assert!(field.brooks().len() > 100);

    for brook in field.brooks() {
        let n = brook.points().len();
        for (i, (p, sheet)) in brook.points().iter().zip(brook.sheets()).enumerate() {
            if i + 1 == n && brook.terminus() != Terminus::Soaks {
                continue;
            }
            let ground = surface.base_ground(GlobalXZ::at(p.x as f64, p.y as f64));
            assert!(
                *sheet <= ground,
                "a {:?} brook's sheet sits {:.2} m above the ground at ({:.0}, {:.0})",
                brook.terminus(),
                *sheet - ground,
                p.x,
                p.y
            );
        }
    }
}

#[test]
fn a_brook_meets_the_water_it_runs_into_without_a_step() {
    let surface = surface_of(20260809);
    let mid = surface.bounds().metres() / 2.0;
    let field = BrookField::build(&surface, GlobalXZ::at(mid, mid));
    let mut checked = 0;
    for brook in field.brooks() {
        if brook.terminus() != Terminus::Water {
            continue;
        }
        let mouth = *brook.points().last().expect("a brook has points");
        let sheet = *brook.sheets().last().expect("a brook has sheets");
        let column = surface.column(GlobalXZ::at(mouth.x as f64, mouth.y as f64));
        let top = column.water_top().expect("a brook stopped at water");
        assert!(
            (sheet - top).abs() < 1e-3,
            "a brook arrives {:.2} m {} the body it joins",
            (sheet - top).abs(),
            if sheet > top { "above" } else { "below" }
        );
        checked += 1;
    }
    assert!(checked > 5, "only {checked} brooks reached open water");
}

#[test]
fn a_pond_holds_water_over_its_whole_floor() {
    let surface = surface_of(20260809);
    let mid = surface.bounds().metres() / 2.0;
    let field = BrookField::build(&surface, GlobalXZ::at(mid, mid));
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
                field.carve(
                    GlobalXZ::at(p.x as f64, p.y as f64),
                    &mut column,
                    super::BrookDetail::Basins,
                );
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
        let field = BrookField::build(&surface, focus);
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
    let field = BrookField::build(&surface, GlobalXZ::at(mid, mid));
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
    // cut it identically or there is a step in the streambed at the seam.
    let (_atlas, surface) = world_of(20260809, 48);
    let mid = surface.bounds().metres() / 2.0;
    let here = GlobalXZ::at(mid, mid);
    let shared: super::SharedBrooks = Arc::new(std::sync::RwLock::new(Arc::new(
        BrookField::build(&surface, here),
    )));
    let builder = TerrainChunkBuilder::new(Arc::clone(&surface))
        .with_brooks(Arc::clone(&shared), super::BrookDetail::Channels);

    let coord = busiest_chunk(&shared.read().expect("field"), here);
    let before = builder.build(coord).expect("build").expect("content");

    // Move the window as far as it can go while still speaking for this chunk.
    let there = GlobalXZ::at(mid + 2_000.0, mid + 1_200.0);
    *shared.write().expect("field") = Arc::new(BrookField::build(&surface, there));
    let after = builder.build(coord).expect("build").expect("content");

    let (land_a, water_a) = split_layers(&before);
    let (land_b, water_b) = split_layers(&after);
    heights_agree(&land_a.positions, &land_b.positions, "the ground");
    let (water_a, water_b) = (
        water_a.expect("channels in the busiest chunk"),
        water_b.expect("channels in the busiest chunk"),
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

/// The chunk near `focus` with the most channel running through it.
///
/// A seam test over ground with no brook on it proves nothing, so the test
/// picks its own subject rather than hoping.
fn busiest_chunk(field: &BrookField, focus: GlobalXZ) -> ChunkCoord {
    let span = chunk_span();
    let centre = ChunkCoord::containing(focus, span);
    let mut best = (0usize, centre);
    for dz in -3..=3 {
        for dx in -3..=3 {
            let coord = ChunkCoord::new(centre.x + dx, centre.z + dz);
            let origin = coord.origin(span);
            let min = Vec2::new(origin.x as f32, origin.z as f32);
            let steps = field.channels_in(min, min + Vec2::splat(span.metres() as f32));
            if steps > best.0 {
                best = (steps, coord);
            }
        }
    }
    assert!(best.0 > 0, "no chunk near the window centre has a brook");
    best.1
}

#[test]
fn a_channel_is_contoured_with_the_rest_of_the_water() {
    // A brook is seated on the same 4 m columns as the land, so the marching
    // squares that draw every other body have to see it as wet. Hiding it from
    // them was what forced a second mesh, and that mesh is what floated.
    let (_atlas, surface) = world_of(20260809, 48);
    let mid = surface.bounds().metres() / 2.0;
    let here = GlobalXZ::at(mid, mid);
    let field = Arc::new(BrookField::build(&surface, here));

    let mut carved = 0;
    for brook in field.brooks().iter().take(40) {
        for point in brook.points() {
            let p = GlobalXZ::at(point.x as f64, point.y as f64);
            let mut column = surface.column(p);
            if column.is_wet() {
                continue;
            }
            field.carve(p, &mut column, super::BrookDetail::Channels);
            let Some(super::WaterBody::Brook) = column.body() else {
                continue;
            };
            assert!(column.wetness() >= 0.0, "a channel is not wet");
            assert!(
                column.contour_wetness() >= 0.0,
                "a channel would be hidden from the contour that draws it"
            );
            let top = column.water_top().expect("a wet channel has a sheet");
            assert!(
                column.ground() < top,
                "a channel's sheet sits in the ground, not over it"
            );
            assert!(
                top <= surface.base_ground(p),
                "a channel's sheet floats {:.2} m above the uncarved ground",
                top - surface.base_ground(p)
            );
            carved += 1;
        }
    }
    assert!(carved > 100, "only {carved} points landed in a channel");

    // The floating mouth: the centreline can sit on a terrace while the
    // channel's width covers the bank dropping into a river. Those columns
    // have to glue too, or the contour is a pane over dry dirt.
    let mut beside = 0;
    for brook in field
        .brooks()
        .iter()
        .filter(|b| b.terminus() == Terminus::Water)
    {
        for point in brook.points().iter().rev().take(12) {
            for (dx, dz) in [
                (0.0, 0.0),
                (4.0, 0.0),
                (-4.0, 0.0),
                (0.0, 4.0),
                (0.0, -4.0),
                (8.0, 0.0),
                (-8.0, 0.0),
                (0.0, 8.0),
                (0.0, -8.0),
            ] {
                let p = GlobalXZ::at(point.x as f64 + dx, point.y as f64 + dz);
                let mut column = surface.column(p);
                if column.is_wet() {
                    continue;
                }
                let atlas_ground = column.ground();
                field.carve(p, &mut column, super::BrookDetail::Channels);
                if column.body() != Some(super::WaterBody::Brook) {
                    continue;
                }
                let top = column.water_top().expect("a wet channel has a sheet");
                assert!(
                    column.ground() < top,
                    "a mouth bed sits above its own sheet at ({:.0}, {:.0})",
                    p.x,
                    p.z
                );
                assert!(
                    top - column.ground() < 2.0,
                    "a mouth is a {:.2} m chasm at ({:.0}, {:.0})",
                    top - column.ground(),
                    p.x,
                    p.z
                );
                assert!(
                    top <= atlas_ground + 2.5 + 0.05,
                    "a mouth sheet floats {:.2} m above the ground at ({:.0}, {:.0})",
                    top - atlas_ground,
                    p.x,
                    p.z
                );
                beside += 1;
            }
        }
    }
    assert!(beside > 20, "only {beside} mouth columns were in a channel");

    let shared: super::SharedBrooks = Arc::new(std::sync::RwLock::new(Arc::clone(&field)));
    let builder = TerrainChunkBuilder::new(Arc::clone(&surface))
        .with_brooks(shared, super::BrookDetail::Channels);
    let coord = busiest_chunk(&field, here);
    let payload = builder.build(coord).expect("build").expect("content");
    let water = split_layers(&payload).1.expect("a drawn channel");
    assert!(
        water.indices.len() >= 6,
        "the busiest chunk on the map has no water triangles"
    );
}

#[test]
fn only_reeds_stand_in_the_shallows_and_no_tree_stands_in_a_channel() {
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
    // A brook's own margin is a metre or two, so keeping trees out of the
    // channel is exactly the tree margin.
    assert!(!PropClass::Tree.stands_in(-1.0));
    assert!(PropClass::Tree.stands_in(-8.0));
}

#[test]
fn bank_cover_needs_a_bank() {
    let (_atlas, surface) = world_of(20260809, 48);
    let p = dry_inland(&surface);
    let ground = surface.column(p).ground();
    let dry = super::GroundCover::sample(&surface, p, ground, super::Fall::default(), 0.0)
        .with_water(-500.0);
    let wet = super::GroundCover::sample(&surface, p, ground, super::Fall::default(), 0.0)
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
    let field = BrookField::build(&surface, dry_inland(&surface));
    let probe = 220usize;
    let step = surface.bounds().metres() / probe as f64;
    let mut wet = 0usize;
    for iz in 0..probe {
        for ix in 0..probe {
            let p = GlobalXZ::at((ix as f64 + 0.5) * step, (iz as f64 + 0.5) * step);
            let mut column = surface.column(p);
            field.carve(p, &mut column, super::BrookDetail::Channels);
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
    let brooks = BrookField::build(&surface, request.requested());
    let pose = resolve_spawn(&surface, &brooks, request).expect("spawn beside the river");

    let mut standing = surface.column(pose.ground());
    brooks.carve(pose.ground(), &mut standing, super::BrookDetail::Channels);
    assert!(!standing.is_wet(), "spawned in water");
    assert!(
        pose.offset_m() <= 480.0,
        "resolver wandered {} m from the request",
        pose.offset_m()
    );
    let again = resolve_spawn(&surface, &brooks, request).expect("spawn");
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
    // No brook has ever reached open ocean, so an empty window is the truth
    // here and saves tracing a continent to prove it.
    assert!(matches!(
        resolve_spawn(&surface, &BrookField::empty(p), request),
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
    session.begin_entry(&mut world, request).expect("entry");
    assert_eq!(session.state(), SessionState::Loading);
    // The spawn is not known at the moment of the request: the water under it has
    // to be traced first, and that happens behind the loading screen.
    assert!(session.spawn().is_none());

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
    let brooks = BrookField::build(&surface, GlobalXZ::at(mid.x as f64, mid.y as f64));
    let pose = resolve_spawn(&surface, &brooks, request).expect("spawn");

    let mut world = World::new();
    let mut stream = WorldStream::new(
        Arc::clone(&surface),
        BrookWindow::new(Arc::clone(&surface)).shared(),
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
        BrookWindow::new(Arc::clone(&surface)).shared(),
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
        let coast = surface
            .hydro_index()
            .coast_signed(surface.hydro(), mouth);
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
        let Some((_, sd)) = surface
            .hydro_index()
            .nearest_lake(surface.hydro(), mouth)
        else {
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
