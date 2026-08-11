//! Walkable continental surface — atlas → surface sample → streamed chunks.
//!
//! Usage: `cargo run -p orrun --bin continent -- [seed] [size]`
//! Controls: WASD move · Q/E turn · Shift sprint · Esc quit

use std::sync::Arc;

use engine::prelude::*;
use orrun::atlas::ContinentAtlas;
use orrun::world::{find_water_view_spawn, AtlasFields, ContinentalSurface};

fn walker_mesh() -> Mesh {
    let mut m = Mesh::new();
    m.add_box((0.0, 0.55, 0.0), (0.55, 1.1, 0.35), rgb(55, 90, 160))
        .unwrap();
    m.add_box((0.0, 1.35, 0.0), (0.4, 0.4, 0.4), rgb(220, 190, 160))
        .unwrap();
    m
}

fn parse_args() -> (i32, usize) {
    let mut args = std::env::args().skip(1);
    let seed = args
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let size = args
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(128)
        .clamp(32, 512);
    (seed, size)
}

fn main() {
    let (seed, size) = parse_args();
    eprintln!("generating atlas seed={seed} size={size}…");
    let atlas = ContinentAtlas::generate(seed, size);
    eprintln!(
        "atlas ready: lakes={} rivers={} coasts={} nodes={} hash={:#x}",
        atlas.hydro.lakes.len(),
        atlas.hydro.rivers.len(),
        atlas.hydro.coasts.len(),
        atlas.nodes.len(),
        atlas.content_hash as u32
    );
    let fields = AtlasFields::build(&atlas);
    let surface = Arc::new(ContinentalSurface::new(&atlas, fields));
    let (sx, sz, spawn_yaw) = find_water_view_spawn(&surface, &atlas);
    let look = surface.sample_at(sx + spawn_yaw.to_radians().sin() * 40.0, sz + spawn_yaw.to_radians().cos() * 40.0);
    eprintln!(
        "spawn at ({sx:.0}, {sz:.0}) yaw={spawn_yaw:.0}  lookahead wet={} water_top={}",
        look.is_wet(),
        look.water_top
    );

    let style = SurfaceMeshStyle {
        chunk_cells: 48,
        cell_size: 3.0,
        water_iso_cell: 1.5,
        rock_height: 600.0,
        ..SurfaceMeshStyle::default()
    };
    let terrain = SurfaceTerrain::new(surface.clone(), style);
    // Instant surround (radius 2 ≈ 25 chunks), then grow — never bake a whole sector.
    let target_radius = 5;
    let mut stream = SurfaceStream::new(terrain, 2).with_budgets(14, 8);
    let mut grow_radius = 2_i32;
    let mut last_grow_t = 0.0_f32;

    let mut pos = Vec3::new(sx, 0.0, sz);
    pos.y = stream.height_at(pos.x, pos.z) + 0.05;
    let mut yaw = spawn_yaw;
    let mut walker: Option<EntityId> = None;

    Engine::run("Orrun — Continent", move |world, frame| {
        if frame.first {
            world.set_clear_color(rgb(145, 195, 235));
            world.set_sun((0.45, 1.0, 0.25), 0.28);
            walker = Some(world.spawn(walker_mesh()));
            stream.sync_blocking(world, pos);
        }

        if grow_radius < target_radius && frame.time - last_grow_t > 0.12 {
            grow_radius += 1;
            stream.radius = grow_radius;
            last_grow_t = frame.time;
        }

        yaw += frame.input.yaw_sign() * 90.0 * frame.dt;
        let dir = frame.input.move_dir_xz(yaw);
        if dir.length_squared() > 0.0 {
            let speed = if frame.input.down(Key::Shift) {
                28.0
            } else {
                10.0
            };
            pos += dir * speed * frame.dt;
        }
        pos.y = stream.height_at(pos.x, pos.z) + 0.05;
        stream.sync(world, pos);

        if let Some(id) = walker {
            world
                .set_place(id, Place::new(pos.x, pos.y, pos.z).with_yaw_deg(yaw))
                .expect("walker");
        }
        world.look_follow(pos, yaw, 18.0, 10.0);
    });
}
