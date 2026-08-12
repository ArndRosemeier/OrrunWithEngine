//! Unpacked atlas float grids for continuous continental sampling.
//!
//! Water shores are **not** derived from these grids — see atlas `HydroVectors`.

use crate::atlas::pack;
use crate::atlas::{Biome, ContinentAtlas, CELL_METRES};

/// Immutable interpolatable atlas climate layers (built once after atlas generate).
#[derive(Clone, Debug)]
pub struct AtlasFields {
    pub size: usize,
    pub sea_surface_z: f32,
    pub elevation_m: Vec<f32>,
    pub humidity01: Vec<f32>,
    pub relief01: Vec<f32>,
    /// Fraction of the cell the atlas expects under canopy.
    pub canopy01: Vec<f32>,
}

impl AtlasFields {
    pub fn build(atlas: &ContinentAtlas) -> Self {
        let size = atlas.size;
        let count = size * size;
        let sea_surface_z = atlas.sea_surface_z as f32;
        let mut elevation_m = vec![0.0; count];
        let mut humidity01 = vec![0.0; count];
        let mut relief01 = vec![0.0; count];
        let mut canopy01 = vec![0.0; count];

        for i in 0..count {
            let cell = atlas.cells[i];
            elevation_m[i] = pack::elevation_to_metres(pack::elevation(cell)) as f32;
            humidity01[i] = pack::humidity(cell) as f32 / 255.0;
            relief01[i] = pack::relief(cell) as f32 / 63.0;
            canopy01[i] = canopy_of(pack::biome(cell), humidity01[i]);
        }

        Self {
            size,
            sea_surface_z,
            elevation_m,
            humidity01,
            relief01,
            canopy01,
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

    /// Catmull-Rom bicubic — C1-ish atlas sampling so km cells don't shade as flat slabs.
    pub fn sample_bicubic(&self, field: &[f32], world_x: f32, world_z: f32) -> f32 {
        let fx = world_x / CELL_METRES - 0.5;
        let fz = world_z / CELL_METRES - 0.5;
        let x0 = fx.floor() as i32;
        let z0 = fz.floor() as i32;
        let tx = fx - x0 as f32;
        let tz = fz - z0 as f32;
        let mut col = [0.0_f32; 4];
        for j in 0..4 {
            let mut row = [0.0_f32; 4];
            for i in 0..4 {
                row[i] = field[self.index(x0 + i as i32 - 1, z0 + j as i32 - 1)];
            }
            col[j] = catmull_rom(row[0], row[1], row[2], row[3], tx);
        }
        catmull_rom(col[0], col[1], col[2], col[3], tz)
    }

    pub fn sample_smooth(&self, field: &[f32], world_x: f32, world_z: f32) -> f32 {
        self.sample_bicubic(field, world_x, world_z)
    }
}

/// How much of a cell the biome expects under trees, before local terrain has
/// its say.
///
/// Humidity moves the figure but cannot invent a forest on a desert cell: the
/// biome classification already weighed rainfall, and second-guessing it here
/// would put woodland where the atlas overlay shows none.
fn canopy_of(biome: Biome, humidity01: f32) -> f32 {
    let base = match biome {
        Biome::Ocean | Biome::Lake => 0.0,
        Biome::Coast => 0.06,
        Biome::Plains => 0.14,
        Biome::Forest => 0.82,
        Biome::Wetland => 0.28,
        Biome::Arid => 0.02,
        Biome::Alpine => 0.12,
        Biome::Tundra => 0.05,
    };
    (base * (0.6 + 0.7 * humidity01.clamp(0.0, 1.0))).clamp(0.0, 1.0)
}

#[inline]
fn catmull_rom(p0: f32, p1: f32, p2: f32, p3: f32, t: f32) -> f32 {
    let t2 = t * t;
    let t3 = t2 * t;
    0.5 * ((2.0 * p1)
        + (-p0 + p2) * t
        + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t2
        + (-p0 + 3.0 * p1 - 3.0 * p2 + p3) * t3)
}
