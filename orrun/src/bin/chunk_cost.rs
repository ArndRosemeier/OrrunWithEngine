//! Microbench one terrain chunk bake: land, water, and contact together.
//!
//! Usage: `cargo run -p orrun --release --bin chunk_cost -- [seed] [size]`

use std::sync::Arc;
use std::time::Instant;

use engine::chunk_stream::ChunkBuilder;
use engine::space::{ChunkLayer, GlobalXZ};
use orrun::atlas::ContinentAtlas;
use orrun::world::{chunk_of, ContinentalSurface, TerrainChunkBuilder, CHUNK_SPAN_M};

fn main() {
    let mut args = std::env::args().skip(1);
    let seed = args.next().and_then(|s| s.parse().ok()).unwrap_or(1);
    let size = args.next().and_then(|s| s.parse().ok()).unwrap_or(64usize);

    let atlas = ContinentAtlas::generate(seed, size);
    let surface = Arc::new(ContinentalSurface::new(&atlas).expect("canonical surface"));
    let builder = TerrainChunkBuilder::new(Arc::clone(&surface));

    let river = atlas
        .hydro
        .rivers
        .iter()
        .max_by_key(|r| r.points.len())
        .map(|r| r.points[r.points.len() / 2]);
    let mid = surface.bounds().metres() * 0.5;
    let probes = [
        (
            "river",
            river
                .map(|p| GlobalXZ::at(p.x as f64, p.y as f64))
                .unwrap_or(GlobalXZ::at(mid, mid)),
        ),
        ("inland", GlobalXZ::at(mid, mid)),
        ("offshore", GlobalXZ::at(CHUNK_SPAN_M * 0.5, mid)),
    ];

    for (label, p) in probes {
        let coord = chunk_of(p);
        let t0 = Instant::now();
        let payload = builder.build(coord).expect("build");
        let dt = t0.elapsed();
        let Some(payload) = payload else {
            eprintln!("{label} {coord:?}: outside the atlas");
            continue;
        };
        let land = payload.layer(ChunkLayer::Land).expect("land layer");
        let water = payload.layer(ChunkLayer::Water);
        let (ymin, ymax) = land
            .positions
            .iter()
            .fold((f32::MAX, f32::MIN), |(lo, hi), v| {
                (lo.min(v.y), hi.max(v.y))
            });
        let column = surface.column(p);
        eprintln!(
            "{label} {coord:?} bake={dt:?} land_tris={} water_idx={} y=[{ymin:.1},{ymax:.1}] \
             centre: ground={:.1} wetness={:+.1} body={:?}",
            land.triangle_count(),
            water.map(|w| w.index_count()).unwrap_or(0),
            column.ground(),
            column.wetness(),
            column.body(),
        );
    }

    // Sanity: a continent should be mostly dry. A wet fraction near 1 means the
    // signed water field is inverted or the sea level is wrong.
    let probe = 256usize;
    let world = surface.bounds().metres();
    let step = world / probe as f64;
    let mut wet = 0usize;
    for iz in 0..probe {
        for ix in 0..probe {
            let p = GlobalXZ::at((ix as f64 + 0.5) * step, (iz as f64 + 0.5) * step);
            if surface.is_wet(p) {
                wet += 1;
            }
        }
    }
    eprintln!(
        "wet fraction over the whole atlas: {:.1}%",
        100.0 * wet as f64 / (probe * probe) as f64
    );
}
