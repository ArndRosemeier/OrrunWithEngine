//! Landmask + elevation / humidity / relief from layered 2D noise.

use engine::proc::Noise;

use super::{layer_seed, lerp, smoothstep};

const OCEAN_COLLAR_FULL: i32 = 48;
const SIZE_FULL: i32 = 1000;

pub struct LandmaskPlanes {
    pub land: Vec<u8>,
    pub elev_code: Vec<u8>,
    pub humidity: Vec<u8>,
    pub relief: Vec<u8>,
}

pub fn collar_cells(size: usize) -> i32 {
    6.max(OCEAN_COLLAR_FULL * size as i32 / SIZE_FULL)
}

/// How far each cell is from open water, in cells.
///
/// Two chamfer sweeps: exact enough for a climate gradient, and cheap enough to
/// run on every atlas build.
fn distance_to_sea(size: usize, land: &[u8]) -> Vec<f32> {
    let far = (size * 2) as f32;
    let mut d: Vec<f32> = land
        .iter()
        .map(|&l| if l == 0 { 0.0 } else { far })
        .collect();
    let diag = std::f32::consts::SQRT_2;
    let at = |x: usize, z: usize| z * size + x;

    for z in 0..size {
        for x in 0..size {
            let i = at(x, z);
            let mut best = d[i];
            if z > 0 {
                best = best.min(d[at(x, z - 1)] + 1.0);
                if x > 0 {
                    best = best.min(d[at(x - 1, z - 1)] + diag);
                }
                if x + 1 < size {
                    best = best.min(d[at(x + 1, z - 1)] + diag);
                }
            }
            if x > 0 {
                best = best.min(d[at(x - 1, z)] + 1.0);
            }
            d[i] = best;
        }
    }
    for z in (0..size).rev() {
        for x in (0..size).rev() {
            let i = at(x, z);
            let mut best = d[i];
            if z + 1 < size {
                best = best.min(d[at(x, z + 1)] + 1.0);
                if x > 0 {
                    best = best.min(d[at(x - 1, z + 1)] + diag);
                }
                if x + 1 < size {
                    best = best.min(d[at(x + 1, z + 1)] + diag);
                }
            }
            if x + 1 < size {
                best = best.min(d[at(x + 1, z)] + 1.0);
            }
            d[i] = best;
        }
    }
    d
}

pub fn build_landmask(world_seed: i32, size: usize) -> LandmaskPlanes {
    let count = size * size;
    let continent = Noise::new(layer_seed(world_seed, "atlas_continent"));
    let coast_cut = Noise::new(layer_seed(world_seed, "atlas_coast_cut"));
    let peninsula = Noise::new(layer_seed(world_seed, "atlas_peninsula"));
    let mountain = Noise::new(layer_seed(world_seed, "atlas_mountain"));
    let moist = Noise::new(layer_seed(world_seed, "atlas_moist"));
    let relief_n = Noise::new(layer_seed(world_seed, "atlas_relief"));
    let warp = Noise::new(layer_seed(world_seed, "atlas_warp"));
    let warp2 = Noise::new(layer_seed(world_seed, "atlas_warp2"));

    let half = size as f32 * 0.5;
    let collar = collar_cells(size);
    let soft_margin = collar as f32 + size as f32 * 0.04;
    let size_i = size as i32;

    let mut land = vec![0u8; count];
    let mut elev_code = vec![0u8; count];
    let mut humidity = vec![0u8; count];
    let mut relief = vec![0u8; count];
    // Kept for the climate pass, which needs the whole landmask before it can
    // say how far a cell is from the sea.
    let mut weather = vec![0.0f32; count];
    let mut alpine_f = vec![0.0f32; count];

    for az in 0..size {
        for ax in 0..size {
            let idx = az * size + ax;
            let edge_d = (ax as i32)
                .min(size_i - 1 - ax as i32)
                .min(az as i32)
                .min(size_i - 1 - az as i32);
            let dxn = (ax as f32 - half) / half;
            let dzn = (az as f32 - half) / half;
            let radial = (dxn * dxn + dzn * dzn).sqrt();
            let ax_f = ax as f32;
            let az_f = az as f32;

            let wx = ax_f
                + warp.fbm2(ax_f * 0.0035, az_f * 0.0035, 4, 2.0, 0.5) * size as f32 * 0.08
                + warp2.fbm2(az_f * 0.0016, ax_f * 0.0016, 3, 2.0, 0.5) * size as f32 * 0.05;
            let wz = az_f
                + warp.fbm2((ax_f + 40.0) * 0.0035, (az_f - 17.0) * 0.0035, 4, 2.0, 0.5)
                    * size as f32
                    * 0.08
                + warp2.fbm2((ax_f - 11.0) * 0.0016, (az_f + 27.0) * 0.0016, 3, 2.0, 0.5)
                    * size as f32
                    * 0.05;

            let cont = continent.fbm2(wx * 0.0024, wz * 0.0024, 6, 2.0, 0.5);
            let pen = peninsula.fbm2(wx * 0.7 * 0.0065, wz * 0.7 * 0.0065, 4, 2.0, 0.5);
            let cut = coast_cut.ridged2(wx * 0.011, wz * 0.011, 4, 2.0, 0.5) * 0.5 + 0.5;
            let mut mass = 1.0 - (radial * 0.88).clamp(0.0, 1.20);
            mass = smoothstep(-0.08, 0.82, mass);
            let mut landness = cont * 0.66 + pen * 0.22 + mass * 0.56;
            landness -= cut * lerp(0.06, 0.30, radial.clamp(0.0, 1.0));
            let edge_fade =
                smoothstep(0.0, soft_margin + size as f32 * 0.06, edge_d as f32).powf(1.35);
            landness *= edge_fade;
            let is_land = landness > 0.08;
            land[idx] = if is_land { 1 } else { 0 };

            if !is_land {
                let depth = (0.55 - landness).clamp(0.0, 1.0) * 32.0;
                elev_code[idx] = (depth as i32).clamp(0, 32) as u8;
                humidity[idx] = 255;
                relief[idx] = 0;
                continue;
            }

            let ridge =
                mountain.ridged2(wx * 0.9 * 0.0045, wz * 0.9 * 0.0045, 4, 2.0, 0.5) * 0.5 + 0.5;
            let alpine = ridge.powf(1.35) * smoothstep(0.2, 0.7, landness);
            let mut code_f = 48.0 + landness * 70.0 + alpine * 130.0;
            code_f += relief_n.fbm2(wx * 0.008, wz * 0.008, 3, 2.0, 0.5) * 10.0;
            elev_code[idx] = (code_f as i32).clamp(33, 255) as u8;
            relief[idx] = ((alpine * 50.0
                + relief_n.fbm2(wz * 0.008, wx * 0.008, 3, 2.0, 0.5) * 8.0
                + 4.0) as i32)
                .clamp(0, 63) as u8;

            weather[idx] = moist.fbm2(wx * 0.0035, wz * 0.0035, 3, 2.0, 0.5) * 0.5 + 0.5;
            alpine_f[idx] = alpine;
        }
    }

    let sea_cells = distance_to_sea(size, &land);
    for idx in 0..count {
        if land[idx] == 0 {
            continue;
        }
        // Weather noise alone clusters around its own mean, which put the whole
        // continent inside one biome band; rainfall needs geography behind it.
        let maritime = 1.0 - smoothstep(2.0, 30.0, sea_cells[idx]);
        let mut h = 0.5 + (weather[idx] - 0.5) * 1.35;
        h = h * 0.62 + maritime * 0.38 + 0.10;
        // Only the high ridge casts a shadow; the gentle uplands stay green.
        h -= smoothstep(0.45, 0.95, alpine_f[idx]) * 0.30;
        humidity[idx] = ((h * 255.0) as i32).clamp(0, 255) as u8;
    }

    LandmaskPlanes {
        land,
        elev_code,
        humidity,
        relief,
    }
}
