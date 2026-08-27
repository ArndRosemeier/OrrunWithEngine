//! Biome classification and climate packing.

use engine::proc::Noise;

use super::biomes::Biome;
use super::pack;
use super::{cardinal, layer_seed, lerp};

pub fn classify_and_pack(
    world_seed: i32,
    size: usize,
    land: &[u8],
    elev_code: &[u8],
    humidity: &mut [u8],
    relief: &mut [u8],
    lake_id: &[i32],
    cells: &mut [i32],
) {
    let temp_n = Noise::new(layer_seed(world_seed, "atlas_temp"));
    for az in 0..size {
        for ax in 0..size {
            let idx = az * size + ax;
            let biome = if lake_id[idx] >= 0 {
                humidity[idx] = 255;
                relief[idx] = 0;
                Biome::Lake
            } else if land[idx] != 0 {
                let mut b = classify_land(
                    ax as i32,
                    az as i32,
                    size,
                    elev_code[idx] as i32,
                    humidity[idx] as i32,
                    relief[idx] as i32,
                    &temp_n,
                );
                if touches_ocean(ax as i32, az as i32, size, land, lake_id) {
                    b = Biome::Coast;
                }
                b
            } else {
                humidity[idx] = 255;
                relief[idx] = 0;
                Biome::Ocean
            };
            cells[idx] = pack::pack(
                elev_code[idx] as i32,
                humidity[idx] as i32,
                biome,
                relief[idx] as i32,
                0,
            );
        }
    }
}

fn touches_ocean(ax: i32, az: i32, size: usize, land: &[u8], lake_id: &[i32]) -> bool {
    for k in 0..4 {
        let (dx, dz) = cardinal(k);
        let nx = ax + dx;
        let nz = az + dz;
        if nx < 0 || nz < 0 || nx as usize >= size || nz as usize >= size {
            return true;
        }
        let nb = nz as usize * size + nx as usize;
        if land[nb] == 0 && lake_id[nb] < 0 {
            return true;
        }
    }
    false
}

fn classify_land(
    ax: i32,
    az: i32,
    size: usize,
    elev: i32,
    hum: i32,
    rel: i32,
    temp_n: &Noise,
) -> Biome {
    let mut temp = temp_n.fbm2(ax as f32 * 0.0025, az as f32 * 0.0025, 2, 2.0, 0.5) * 0.5 + 0.5;
    let latitude_denominator = size
        .checked_sub(1)
        .expect("atlas size is validated before biome classification");
    temp = lerp(temp, 0.15, az as f32 / latitude_denominator as f32 * 0.35);
    if elev >= 181 || (elev >= 160 && rel > 28) {
        return Biome::Alpine;
    }
    if temp < 0.28 && elev > 60 {
        return Biome::Tundra;
    }
    if hum < 90 && rel > 10 {
        return Biome::Arid;
    }
    if hum > 170 && elev < 100 {
        return Biome::Wetland;
    }
    // Closed canopy is a rainfall question, not a relief one: flat, wet ground
    // grows the deepest timber of all. Plains are what is left in the middle.
    if hum > 138 {
        return Biome::Forest;
    }
    Biome::Plains
}
