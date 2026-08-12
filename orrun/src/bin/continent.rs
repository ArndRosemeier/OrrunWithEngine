//! Direct entry into the walkable continent, bypassing the map UI.
//!
//! Usage: `cargo run -p orrun --bin continent -- [seed] [size] [where]`
//! where `where` is `river` (default), `coast`, `inland`, `ocean`, or `x,z`.
//! Controls: WASD move · Q/E turn · Shift sprint · Esc quit.
//!
//! This is the same `WorldSession` the game uses — there is no second spawn or
//! streaming path to keep in sync. `ocean` exists to show that an impossible
//! selection fails loudly rather than dropping the player somewhere else.

use std::sync::Arc;

use engine::prelude::*;
use engine::space::GlobalXZ;
use glam::Vec2;
use orrun::atlas::ContinentAtlas;
use orrun::world::{ContinentalSurface, MapPoint, SessionState, WorldEntryRequest, WorldSession};

/// Which part of the continent to enter at.
#[derive(Clone, Copy, Debug)]
enum Target {
    River,
    Coast,
    Inland,
    Ocean,
    Exact(f64, f64),
}

impl Target {
    fn parse(text: &str) -> Self {
        match text {
            "river" => Self::River,
            "coast" => Self::Coast,
            "inland" => Self::Inland,
            "ocean" => Self::Ocean,
            other => {
                let (x, z) = other
                    .split_once(',')
                    .expect("entry must be river|coast|inland|ocean|x,z");
                Self::Exact(
                    x.trim().parse().expect("entry x in metres"),
                    z.trim().parse().expect("entry z in metres"),
                )
            }
        }
    }
}

struct Args {
    seed: i32,
    size: usize,
    target: Target,
}

fn parse_args() -> Args {
    let mut args = std::env::args().skip(1);
    let seed = args.next().and_then(|s| s.parse().ok()).unwrap_or(1);
    let size = args
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(128usize)
        .clamp(32, 512);
    let target = args
        .next()
        .map(|s| Target::parse(&s))
        .unwrap_or(Target::River);
    Args { seed, size, target }
}

/// Coarse scan for the driest (`Inland`) or deepest (`Ocean`) column.
fn extreme_column(surface: &ContinentalSurface, want_wet: bool) -> GlobalXZ {
    let probe = 192usize;
    let step = surface.bounds().metres() / probe as f64;
    let mut best = f32::NEG_INFINITY;
    let mut at = GlobalXZ::at(step * 0.5, step * 0.5);
    for iz in 0..probe {
        for ix in 0..probe {
            let p = GlobalXZ::at((ix as f64 + 0.5) * step, (iz as f64 + 0.5) * step);
            let wetness = surface.column(p).wetness();
            let score = if want_wet { wetness } else { -wetness };
            if score > best {
                best = score;
                at = p;
            }
        }
    }
    at
}

/// A gentle stretch of shoreline: the coast point whose hinterland is lowest.
///
/// The continent also has cliff coasts, where entry legitimately fails; those
/// are reachable with explicit `x,z` coordinates.
fn lowland_shore(atlas: &ContinentAtlas, surface: &ContinentalSurface) -> GlobalXZ {
    let coast = atlas
        .hydro
        .coasts
        .iter()
        .max_by_key(|c| c.ring.len())
        .expect("the atlas has a coastline");
    let mut centre = Vec2::ZERO;
    for p in &coast.ring {
        centre += *p;
    }
    centre /= coast.ring.len() as f32;

    let stride = (coast.ring.len() / 512).max(1);
    let mut best = f32::MAX;
    let mut at = GlobalXZ::at(centre.x as f64, centre.y as f64);
    for rim in coast.ring.iter().step_by(stride) {
        let inland = (centre - *rim).normalize_or_zero();
        if inland.length_squared() < 0.5 {
            continue;
        }
        // Close enough to the rim that the entry heading finds the sea.
        let probe = *rim + inland * 60.0;
        let column = surface.column(GlobalXZ::at(probe.x as f64, probe.y as f64));
        if column.is_wet() {
            continue;
        }
        if column.ground() < best {
            best = column.ground();
            at = GlobalXZ::at(probe.x as f64, probe.y as f64);
        }
    }
    at
}

fn entry_position(
    atlas: &ContinentAtlas,
    surface: &ContinentalSurface,
    target: Target,
) -> GlobalXZ {
    match target {
        Target::Exact(x, z) => GlobalXZ::at(x, z),
        // The middle of the longest reach: a lively place to look at the water.
        Target::River => atlas
            .hydro
            .rivers
            .iter()
            .max_by_key(|r| r.points.len())
            .map(|r| r.points[r.points.len() / 2])
            .map(|v| GlobalXZ::at(v.x as f64, v.y as f64))
            .expect("the atlas has at least one river"),
        Target::Coast => lowland_shore(atlas, surface),
        Target::Inland => extreme_column(surface, false),
        Target::Ocean => extreme_column(surface, true),
    }
}

fn main() {
    let args = parse_args();
    eprintln!("generating atlas seed={} size={}…", args.seed, args.size);
    let atlas = Arc::new(ContinentAtlas::generate(args.seed, args.size));
    let surface = Arc::new(ContinentalSurface::new(&atlas).expect("canonical surface"));
    let bounds = surface.bounds();
    eprintln!(
        "atlas ready: lakes={} rivers={} coasts={} hash={:#x}",
        atlas.hydro.lakes.len(),
        atlas.hydro.rivers.len(),
        atlas.hydro.coasts.len(),
        atlas.content_hash as u32
    );

    let at = entry_position(&atlas, &surface, args.target);
    let point = MapPoint::from_global(bounds, at).expect("entry point is outside the atlas");
    eprintln!(
        "entry target {:?} at ({:.0}, {:.0})",
        args.target, at.x, at.z
    );
    let request = WorldEntryRequest::at(point);
    let sea = surface.sea_surface_z();
    let mut session = WorldSession::new(Arc::clone(&surface));
    let mut announced = false;

    Engine::run("Orrun — Continent", move |world, frame| {
        if frame.first {
            world.set_clear_color(rgb(145, 195, 235));
            world.set_sun((0.7, 0.75, 0.4), 0.12);

            let seed = args.seed as u32;
            let grass = world
                .create_terrain_albedo(TerrainAlbedo::Grass, 256, seed)
                .expect("grass albedo");
            let sand = world
                .create_terrain_albedo(TerrainAlbedo::Sand, 256, seed ^ 0x51)
                .expect("sand albedo");
            let rock = world
                .create_terrain_albedo(TerrainAlbedo::Rock, 256, seed ^ 0x20C6)
                .expect("rock albedo");
            let material = world
                .create_terrain_material(TerrainMaterialDesc {
                    grass,
                    sand,
                    rock,
                    metres_per_tile: 7.0,
                    rock_slope_start: 0.36,
                    rock_slope_end: 0.70,
                    sand_height_band: 10.0,
                    sea_surface_z: sea,
                    tint_strength: 0.30,
                })
                .expect("terrain material");
            world.set_default_terrain_material(Some(material));

            let pose = session
                .begin_entry(world, request)
                .expect("entry point must resolve to walkable ground");
            let g = pose.ground();
            eprintln!(
                "entering at ({:.0}, {:.0}) yaw={:.0}° ({:.0} m from the request)",
                g.x,
                g.z,
                pose.heading().degrees(),
                pose.offset_m()
            );
        }

        session.update(world, frame).expect("world session");

        if !announced && session.state() == SessionState::World {
            announced = true;
            let p = session.player_position().expect("player");
            eprintln!(
                "world ready: standing at ({:.0}, {:.1}, {:.0}) with {} chunks",
                p.x,
                p.y,
                p.z,
                session.stream().resident_count()
            );
        }
    });
}
