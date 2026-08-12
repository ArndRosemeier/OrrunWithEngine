//! How the continent is dressed: ground and water materials.
//!
//! Both the game and the diagnostic binaries install the same look, so a
//! screenshot from `continent` is a screenshot of the game.

use engine::color::rgb;
use engine::texture::{TerrainAlbedo, TerrainMaterialDesc, WaterMaterialDesc};
use engine::world::{Haze, Sky, World};

use super::chunk_mesh::WATER_DEPTH_SCALE_M;
use super::world_stream::FAR_VIEW_M;

/// Ground albedo resolution. Large enough that a tile carries pebble-scale
/// detail at the seven metres it is stretched over.
const ALBEDO_SIZE: u32 = 512;

/// Distance at which ground at eye level is indistinguishable from sky.
///
/// Well short of the thirty kilometres of terrain behind it, and that is the
/// point: the outermost chunks have to already be sky by the time they run out,
/// or the edge of the streamed world is visible as an edge. It only holds at low
/// altitude — the air thins with height, so from a summit the same setting shows
/// peaks two or three times further out.
const VISIBILITY_M: f32 = 12_000.0;

/// Where the light comes from. Also decides which hillsides are the dry ones,
/// so the scatter reads it rather than guessing.
pub const SUN_DIR: (f32, f32, f32) = (0.62, 0.68, 0.38);

/// Mid-morning sun, a cool northern sky, and enough fill that a north slope
/// is still readable.
///
/// The sky, the haze, and the clear colour share one horizon so distant ground
/// dissolves into the same band the dome is already showing. Nothing casts
/// shadows yet, so ambient is doing the work of bounced light.
pub fn install_daylight(world: &mut World) {
    let sky = Sky::daylight();
    world.set_sun(SUN_DIR, 0.34);
    world.light.color = sky.sun_color.to_vec3();
    world.set_sky(Some(sky));
    world.set_clear_color(sky.horizon);
    world.set_haze(Some(
        Haze::new(sky.horizon, VISIBILITY_M).thinning_above(0.0, 1_400.0),
    ));
    world
        .set_view_distance(FAR_VIEW_M)
        .expect("a horizon-scale view distance");
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
            // Shaded faces hold snow lower; sunny ones stay rock longer. Full
            // cover is a band, not a contour, and steep faces shed it.
            snow_line_m: 1_050.0,
            snow_full_m: 2_100.0,
            snow_slope_start: 0.32,
            snow_slope_end: 0.68,
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

#[cfg(test)]
mod tests {
    use super::*;
    use engine::world::World;

    #[test]
    fn daylight_haze_matches_the_sky_horizon() {
        let mut world = World::default();
        install_daylight(&mut world);
        let sky = world.sky().expect("sky");
        let haze = world.haze().expect("haze");
        assert_eq!(sky.horizon, haze.color);
        assert_eq!(world.clear_color, sky.horizon);
        assert!(world.sky().is_some());
    }
}
