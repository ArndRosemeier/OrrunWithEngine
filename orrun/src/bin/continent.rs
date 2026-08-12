//! Direct entry into the walkable continent, bypassing the map UI.
//!
//! Usage: `cargo run -p orrun --bin continent -- [seed] [size] [where]`
//! where `where` is `river` (default), `coast`, `inland`, `forest`, `ocean`,
//! `summit`, `brook`, `pond`, or `x,z`.
//! Controls: click to look · Esc hands the mouse back · W/S walk · Q/E
//! sidestep · A/D turn · Shift sprint · F fly (W follows the look) · Space jump · Esc with
//! a free cursor quits.
//!
//! This is the same `WorldSession` the game uses — there is no second spawn or
//! streaming path to keep in sync. `ocean` exists to show that an impossible
//! selection fails loudly rather than dropping the player somewhere else.

use std::sync::Arc;

use engine::prelude::*;
use engine::space::GlobalXZ;
use glam::Vec2;
use orrun::atlas::ContinentAtlas;
use orrun::world::{
    install_daylight, install_materials, BrookField, ContinentalSurface, Fall, GroundCover,
    Heading, MapPoint, SessionState, Terminus, WorldEntryRequest, WorldSession,
};

/// Which part of the continent to enter at.
#[derive(Clone, Copy, Debug)]
enum Target {
    River,
    Coast,
    Inland,
    Forest,
    Ocean,
    Summit,
    Brook,
    Pond,
    Exact(f64, f64),
}

impl Target {
    fn parse(text: &str) -> Self {
        match text {
            "river" => Self::River,
            "coast" => Self::Coast,
            "inland" => Self::Inland,
            "forest" => Self::Forest,
            "ocean" => Self::Ocean,
            "summit" => Self::Summit,
            "brook" => Self::Brook,
            "pond" => Self::Pond,
            other => {
                let (x, z) = other
                    .split_once(',')
                    .expect("entry must be river|coast|inland|forest|ocean|summit|brook|pond|x,z");
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

/// The deepest timber: the dry, gentle column carrying the most canopy.
///
/// `Inland` finds the driest spot on the continent, which is the one place
/// grass is meant to look like straw; a forest entry is what the ground cover
/// wants to be judged against.
fn deepest_timber(surface: &ContinentalSurface) -> GlobalXZ {
    let probe = 192usize;
    let step = surface.bounds().metres() / probe as f64;
    let mut best = f32::NEG_INFINITY;
    let mut at = GlobalXZ::at(step * 0.5, step * 0.5);
    for iz in 0..probe {
        for ix in 0..probe {
            let p = GlobalXZ::at((ix as f64 + 0.5) * step, (iz as f64 + 0.5) * step);
            let column = surface.column(p);
            if column.is_wet() {
                continue;
            }
            let cover = GroundCover::sample(surface, p, column.ground(), Fall::default(), 0.0);
            if cover.tree > best {
                best = cover.tree;
                at = p;
            }
        }
    }
    at
}

/// The highest walkable ground: the view the visibility ladder is judged from.
fn highest_ground(surface: &ContinentalSurface) -> GlobalXZ {
    let probe = 192usize;
    let step = surface.bounds().metres() / probe as f64;
    let mut best = f32::NEG_INFINITY;
    let mut at = GlobalXZ::at(step * 0.5, step * 0.5);
    for iz in 0..probe {
        for ix in 0..probe {
            let p = GlobalXZ::at((ix as f64 + 0.5) * step, (iz as f64 + 0.5) * step);
            let column = surface.column(p);
            if column.is_wet() || column.ground() <= best {
                continue;
            }
            best = column.ground();
            at = p;
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

/// The bank of a real brook, or the shore of a real pond.
///
/// Sub-atlas water is a window around the player rather than a continental
/// layer, so there is no map to look it up in: the only way to stand beside one
/// is to trace a window first and pick out of it.
fn sub_atlas_water(surface: &ContinentalSurface, want_pond: bool) -> (GlobalXZ, Heading) {
    let mid = surface.bounds().metres() / 2.0;
    let field = BrookField::build(surface, GlobalXZ::at(mid, mid));
    // Stand off the water and look back at it. The spawn resolver faces the
    // nearest water by itself, but it asks the surface, and the surface has
    // never heard of a brook.
    let wet = |p: Vec2| {
        field.water_reach(p) >= 0.0
            || surface
                .column(GlobalXZ::at(p.x as f64, p.y as f64))
                .is_wet()
    };
    // Out to a bearing that is dry, then back in to the shoreline and a few
    // paces off it. Standing off by the whole reach of the water puts a bank
    // between the camera and it as often as not.
    let dry_near = |at: Vec2, reach: f32| -> (GlobalXZ, Heading) {
        for turn in 0..16 {
            let angle = std::f32::consts::TAU * (turn as f32 / 16.0);
            let away = Vec2::new(angle.cos(), angle.sin());
            if wet(at + away * reach) {
                continue;
            }
            let mut stand = at + away * reach;
            while stand.distance(at) > 2.0 && !wet(stand - away * 2.0) {
                stand -= away * 2.0;
            }
            // Right on the bank. A brook lies most of a metre below the field it
            // runs through, so from ten paces back its own near bank hides it,
            // and the shot comes out as a meadow with reeds in it.
            let stand = stand + away * 2.0;
            return (
                GlobalXZ::at(stand.x as f64, stand.y as f64),
                Heading::towards(at - stand).expect("a look direction"),
            );
        }
        (
            GlobalXZ::at(at.x as f64, at.y as f64),
            Heading::towards(Vec2::X).expect("a look direction"),
        )
    };

    if want_pond {
        let pond = field
            .ponds()
            .iter()
            .max_by(|a, b| a.reach_m().total_cmp(&b.reach_m()))
            .expect("the window holds at least one pond");
        eprintln!(
            "pond {:.0} m across, sheet {:.1} m",
            pond.reach_m() * 2.0,
            pond.sheet_z()
        );
        return dry_near(pond.centre(), pond.reach_m() + 14.0);
    }

    let brook = field
        .brooks()
        .iter()
        .filter(|b| b.terminus() == Terminus::Water)
        .max_by(|a, b| a.length_m().total_cmp(&b.length_m()))
        .or_else(|| {
            field
                .brooks()
                .iter()
                .max_by(|a, b| a.length_m().total_cmp(&b.length_m()))
        })
        .expect("the window holds at least one brook");
    eprintln!(
        "brook {:.0} m long ending in {:?}",
        brook.length_m(),
        brook.terminus()
    );
    // Mid-course: at the mouth the receiving river fills the frame, and at the
    // spring the channel is still narrowing. Stand off past the water itself.
    let points = brook.points();
    dry_near(points[points.len() / 2], 20.0)
}

/// Where to enter, and which way to look if the target knows better than the
/// spawn resolver does.
fn entry_position(
    atlas: &ContinentAtlas,
    surface: &ContinentalSurface,
    target: Target,
) -> (GlobalXZ, Option<Heading>) {
    if let Target::Brook | Target::Pond = target {
        let (at, look) = sub_atlas_water(surface, matches!(target, Target::Pond));
        return (at, Some(look));
    }
    let at = match target {
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
        Target::Forest => deepest_timber(surface),
        Target::Ocean => extreme_column(surface, true),
        Target::Summit => highest_ground(surface),
        Target::Brook | Target::Pond => unreachable!("handled above"),
    };
    (at, None)
}

fn main() {
    let args = parse_args();
    eprintln!("generating atlas seed={} size={}…", args.seed, args.size);
    let atlas = Arc::new(ContinentAtlas::generate(args.seed, args.size));
    let atlas_took = std::time::Instant::now();
    let surface = Arc::new(ContinentalSurface::new(&atlas).expect("canonical surface"));
    let bounds = surface.bounds();
    eprintln!(
        "atlas ready: lakes={} rivers={} coasts={} hash={:#x}",
        atlas.hydro.lakes.len(),
        atlas.hydro.rivers.len(),
        atlas.hydro.coasts.len(),
        atlas.content_hash as u32
    );
    // Building the surface is dead time between the map and the world, so it is
    // worth seeing whenever anything moves work into it.
    eprintln!(
        "surface ready in {:.2}s",
        atlas_took.elapsed().as_secs_f32()
    );

    let (at, look) = entry_position(&atlas, &surface, args.target);
    let point = MapPoint::from_global(bounds, at).expect("entry point is outside the atlas");
    eprintln!(
        "entry target {:?} at ({:.0}, {:.0})",
        args.target, at.x, at.z
    );
    let request = match look {
        Some(heading) => WorldEntryRequest::at(point).facing(heading),
        None => WorldEntryRequest::at(point),
    };
    let sea = surface.sea_surface_z();
    let mut session = WorldSession::new(Arc::clone(&surface));
    let mut announced = false;
    let mut reported = false;
    // Entry is the one moment everything happens at once — brook window, chunk
    // bakes, the first sowing — so the longest frame of the first seconds is the
    // number that says whether entering the world hitches.
    let mut worst = (0.0f32, 0.0f32);

    Engine::run("Orrun — Continent", move |world, frame| {
        if frame.first {
            install_daylight(world);
            install_materials(world, args.seed, sea);

            session
                .begin_entry(world, request)
                .expect("entry point must resolve to walkable ground");
        }

        session.update(world, frame).expect("world session");

        if !frame.first && frame.dt > worst.0 {
            worst = (frame.dt, frame.time);
        }

        if !announced && session.state() == SessionState::World {
            announced = true;
            let pose = session
                .spawn()
                .expect("a spawn, once the world is standing");
            let g = pose.ground();
            eprintln!(
                "entering at ({:.0}, {:.0}) yaw={:.0}° ({:.0} m from the request)",
                g.x,
                g.z,
                pose.heading().degrees(),
                pose.offset_m()
            );
            let p = session.player_position().expect("player");
            eprintln!(
                "world ready: standing at ({:.0}, {:.1}, {:.0}) with {} chunks",
                p.x,
                p.y,
                p.z,
                session.stream().resident_count()
            );
        }

        // What the visibility ladder actually costs, once it has settled.
        if !reported && announced && frame.time > 1.5 {
            reported = true;
            eprintln!(
                "steady at {:.0} fps: chunks near={} distant={:?} pending={}, \
                 {} props sown in {:.0} ms",
                frame.fps,
                session.stream().resident_count(),
                session.stream().distant_resident_counts(),
                session.stream().pending_count(),
                session.scattered_count(),
                session.sow_ms(),
            );
            eprintln!(
                "longest frame since entry {:.0} ms, at {:.2} s",
                worst.0 * 1000.0,
                worst.1
            );
        }
    });
}
