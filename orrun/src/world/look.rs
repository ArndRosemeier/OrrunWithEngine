//! How the continent is dressed: ground and water materials.
//!
//! Both the game and the diagnostic binaries install the same look, so a
//! screenshot from `continent` is a screenshot of the game.

use engine::color::rgb;
use engine::texture::{TerrainAlbedo, TerrainMaterialDesc, WaterMaterialDesc};
use engine::world::World;

use super::chunk_mesh::WATER_DEPTH_SCALE_M;

/// Ground albedo resolution. Large enough that a tile carries pebble-scale
/// detail at the seven metres it is stretched over.
const ALBEDO_SIZE: u32 = 512;

/// Mid-morning sun, and enough sky light that a north slope is still readable.
///
/// Nothing casts shadows yet, so ambient is doing the work of bounced light:
/// too little and the ground turns to mud wherever it faces away from the sun.
pub fn install_daylight(world: &mut World) {
    world.set_sun((0.62, 0.68, 0.38), 0.34);
    world.set_clear_color(rgb(150, 196, 232));
}

/// Install the terrain and water materials streamed chunks are drawn with.
pub fn install_materials(world: &mut World, seed: i32, sea_surface_z: f32) {
    let tex_seed = seed as u32;
    let grass = world
        .create_terrain_albedo(TerrainAlbedo::Grass, ALBEDO_SIZE, tex_seed)
        .expect("grass albedo");
    let sand = world
        .create_terrain_albedo(TerrainAlbedo::Sand, ALBEDO_SIZE, tex_seed ^ 0x51)
        .expect("sand albedo");
    let rock = world
        .create_terrain_albedo(TerrainAlbedo::Rock, ALBEDO_SIZE, tex_seed ^ 0x20C6)
        .expect("rock albedo");
    let ground = world
        .create_terrain_material(TerrainMaterialDesc {
            grass,
            sand,
            rock,
            metres_per_tile: 7.0,
            rock_slope_start: 0.36,
            rock_slope_end: 0.70,
            sand_height_band: 10.0,
            sea_surface_z,
            tint_strength: 0.30,
        })
        .expect("terrain material");
    world.set_default_terrain_material(Some(ground));

    let water = world
        .create_water_material(WaterMaterialDesc {
            shallow: rgb(122, 178, 176),
            deep: rgb(12, 42, 72),
            // Must match the scale the sheet encodes its depth with.
            depth_scale_m: WATER_DEPTH_SCALE_M,
            wave_length_m: 6.0,
            wave_steepness: 0.40,
            wave_speed_m_s: 0.85,
            // Depth, not distance: a shelving beach turns even a hand's depth
            // into tens of metres of surf, so this stays small.
            foam_width_m: 0.30,
            glint: 1.1,
        })
        .expect("water material");
    world.set_default_water_material(Some(water));
}
