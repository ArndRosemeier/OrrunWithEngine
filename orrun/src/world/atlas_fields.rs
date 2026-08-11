//! Unpacked atlas float grids for continuous continental sampling.
//!
//! Water shores are **not** derived from these grids — see atlas `HydroVectors`.

use crate::atlas::pack;
use crate::atlas::{ContinentAtlas, CELL_METRES};

/// Immutable interpolatable atlas climate layers (built once after atlas generate).
#[derive(Clone, Debug)]
pub struct AtlasFields {
    pub size: usize,
    pub sea_surface_z: f32,
    pub elevation_m: Vec<f32>,
    pub humidity01: Vec<f32>,
    pub relief01: Vec<f32>,
}

impl AtlasFields {
    pub fn build(atlas: &ContinentAtlas) -> Self {
        let size = atlas.size;
        let count = size * size;
        let sea_surface_z = atlas.sea_surface_z as f32;
        let mut elevation_m = vec![0.0; count];
        let mut humidity01 = vec![0.0; count];
        let mut relief01 = vec![0.0; count];

        for i in 0..count {
            let cell = atlas.cells[i];
            elevation_m[i] = pack::elevation_to_metres(pack::elevation(cell)) as f32;
            humidity01[i] = pack::humidity(cell) as f32 / 255.0;
            relief01[i] = pack::relief(cell) as f32 / 63.0;
        }

        Self {
            size,
            sea_surface_z,
            elevation_m,
            humidity01,
            relief01,
        }
    }

    #[inline]
    fn index(&self, ax: i32, az: i32) -> usize {
        let s = self.size as i32;
        let x = ax.clamp(0, s - 1) as usize;
        let z = az.clamp(0, s - 1) as usize;
        z * self.size + x
    }

    pub fn sample_bilinear(&self, field: &[f32], world_x: f32, world_z: f32) -> f32 {
        let fx = world_x / CELL_METRES - 0.5;
        let fz = world_z / CELL_METRES - 0.5;
        let x0 = fx.floor() as i32;
        let z0 = fz.floor() as i32;
        let tx = fx - x0 as f32;
        let tz = fz - z0 as f32;
        let v00 = field[self.index(x0, z0)];
        let v10 = field[self.index(x0 + 1, z0)];
        let v01 = field[self.index(x0, z0 + 1)];
        let v11 = field[self.index(x0 + 1, z0 + 1)];
        let a = v00 + (v10 - v00) * tx;
        let b = v01 + (v11 - v01) * tx;
        a + (b - a) * tz
    }

    pub fn sample_smooth(&self, field: &[f32], world_x: f32, world_z: f32) -> f32 {
        let mut sum = 0.0;
        let mut wsum = 0.0;
        for dz in -1..=1 {
            for dx in -1..=1 {
                let w = if dx == 0 && dz == 0 {
                    4.0
                } else if dx == 0 || dz == 0 {
                    2.0
                } else {
                    1.0
                };
                let sx = world_x + dx as f32 * CELL_METRES * 0.35;
                let sz = world_z + dz as f32 * CELL_METRES * 0.35;
                sum += self.sample_bilinear(field, sx, sz) * w;
                wsum += w;
            }
        }
        sum / wsum
    }
}
