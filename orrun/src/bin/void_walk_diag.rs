//! Headless walk from a resolved spawn: no holes underfoot, no stalled frames.
//!
//! Usage: `cargo run -p orrun --release --bin void_walk_diag -- [seed] [size]`
//! Exits non-zero when the ground vanishes, a frame stalls, or a bake fails.

use std::sync::Arc;
use std::time::{Duration, Instant};

use engine::space::GlobalXZ;
use engine::world::World;
use glam::Vec2;
use orrun::atlas::ContinentAtlas;
use orrun::world::{
    resolve_spawn, AtlasBounds, BrookWindow, ContinentalSurface, MapPoint, ScatterCatalog,
    ScatterLayer, WorldEntryRequest, WorldStream,
};

fn entry_point(atlas: &ContinentAtlas, bounds: AtlasBounds) -> MapPoint {
    let p = atlas
        .hydro
        .rivers
        .iter()
        .max_by_key(|r| r.points.len())
        .map(|r| r.points[r.points.len() / 2])
        .map(|v| GlobalXZ::at(v.x as f64, v.y as f64))
        .unwrap_or_else(|| {
            let mid = bounds.metres() * 0.5;
            GlobalXZ::at(mid, mid)
        });
    MapPoint::from_global(bounds, p).expect("entry inside the atlas")
}

fn main() {
    let mut args = std::env::args().skip(1);
    let seed = args.next().and_then(|s| s.parse().ok()).unwrap_or(1);
    let size = args
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(64usize)
        .clamp(32, 512);

    eprintln!("atlas seed={seed} size={size}");
    let atlas = ContinentAtlas::generate(seed, size);
    let surface = Arc::new(ContinentalSurface::new(&atlas).expect("canonical surface"));
    let bounds = surface.bounds();
    let request = WorldEntryRequest::at(entry_point(&atlas, bounds));
    let mut brooks = BrookWindow::new(Arc::clone(&surface));
    brooks.settle(request.requested());
    let pose = resolve_spawn(&surface, &brooks.field(), request).expect("spawn");
    let dir = pose.heading().direction();
    eprintln!(
        "spawn=({:.0},{:.0}) yaw={:.0}° offset={:.0}m",
        pose.ground().x,
        pose.ground().z,
        pose.heading().degrees(),
        pose.offset_m()
    );

    let mut world = World::new();
    let mut stream = WorldStream::new(Arc::clone(&surface), brooks.shared());
    // Cover as well as ground: sowing a window is the heaviest thing that
    // happens while walking, and the point of this walk is that nothing heavy
    // happens on the frame it is asked for.
    let mut scatter = ScatterLayer::install(
        &mut world,
        &ScatterCatalog::discover().expect("prop catalogue"),
        seed,
    )
    .expect("prop meshes");
    let mut p = pose.ground();

    let t0 = Instant::now();
    stream.prepare_entry(&mut world, p).expect("entry ring");
    eprintln!(
        "startup chunks={} dt={:?}",
        stream.resident_count(),
        t0.elapsed()
    );

    // A sprint at 28 m/s on a paced clock: streaming has to keep up with real
    // time, not with however fast this loop can spin. Long enough to leave the
    // brook window behind and be handed a new one, which is the last thing in
    // this pipeline that could block a frame.
    const FRAMES: usize = 1_800;
    let step = 28.0 / 60.0;
    let frame_budget = Duration::from_micros(16_667);
    let mut rebases = 0usize;
    let mut worst = (Duration::ZERO, 0usize);
    for i in 0..FRAMES {
        let tick = Instant::now();
        p = GlobalXZ::at(p.x + dir.x as f64 * step, p.z + dir.y as f64 * step);
        let frame_t0 = Instant::now();
        brooks.follow(p);
        let rebased = stream.maybe_rebase(&mut world, p).expect("rebase");
        if rebased {
            rebases += 1;
        }
        stream
            .sync(&mut world, p, Some(Vec2::new(dir.x, dir.y)))
            .expect("sync");
        scatter
            .follow(&mut world, &stream, &surface, &brooks.field(), p, rebased)
            .expect("cover");
        let dt = frame_t0.elapsed();
        // The first sow happens on the calling thread on purpose, behind what
        // would be a loading screen; after that nothing may.
        if i > 0 && dt > worst.0 {
            worst = (dt, i);
        }

        if dt > Duration::from_millis(250) {
            eprintln!("STALL frame={i} dt={dt:?}");
            std::process::exit(5);
        }
        // The required ring is streamed asynchronously; give it a moment before
        // declaring a hole, but never accept one that persists.
        if stream.contact_height(p).is_none() {
            let wait = Instant::now();
            while stream.contact_height(p).is_none() {
                stream.sync(&mut world, p, None).expect("sync");
                if wait.elapsed() > Duration::from_secs(2) {
                    eprintln!("VOID frame={i} at ({:.0},{:.0})", p.x, p.z);
                    std::process::exit(4);
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        if i % 180 == 0 {
            eprintln!(
                "frame {i} pos=({:.0},{:.0}) chunks={} pending={} props={} sow={:.0}ms dt={dt:?}",
                p.x,
                p.z,
                stream.resident_count(),
                stream.pending_count(),
                scatter.placed_count(),
                scatter.sow_ms(),
            );
        }
        if let Some(rest) = frame_budget.checked_sub(tick.elapsed()) {
            std::thread::sleep(rest);
        }
    }
    eprintln!(
        "OK chunks={} rebases={} end=({:.0},{:.0}) worst frame {:?} at {}",
        stream.resident_count(),
        rebases,
        p.x,
        p.z,
        worst.0,
        worst.1,
    );
}
