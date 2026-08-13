//! The canonical continental surface.
//!
//! One authority answers every question about the ground: atlas overlays, chunk
//! meshes, spawn search, and the walker's feet all call [`ContinentalSurface`].
//! There is no second sampler to drift away from it.
//!
//! Water is a **continuous signed field**, not a set of flags. For each body
//! (ocean, lake, river) the field is
//!
//! ```text
//! min(sheet_height - ground, geometric_margin_into_the_body)
//! ```
//!
//! and the column takes the maximum over the bodies. The depth term guarantees
//! there is a real basin under the sheet; the geometric term keeps the water
//! inside the authored hydro geometry. The zero contour of that field *is* the
//! shoreline, so the 2D overlay, the water mesh, and wetness tests cannot
//! disagree about where the coast runs.

use std::sync::Arc;

use engine::proc::Noise;
use engine::space::GlobalXZ;
use engine::surface::{SurfaceSample, SurfaceSource, WATER_CLEARANCE};
use glam::Vec2;
use thiserror::Error;

use super::atlas_fields::AtlasFields;
use super::coords::AtlasBounds;
use super::hydro_geom::{HydroIndex, OCEAN_SHELF_DEPTH, SHORE_BAND_M};
use super::rng::CellRng;
use crate::atlas::hydro::HydroVectors;
use crate::atlas::{
    bake_road_paths, cell_population, ContinentAtlas, NodeKind, RoadPath, CELL_METRES,
};

/// Minimum water depth wherever a sheet is present.
pub const MIN_WATER_DEPTH: f32 = 0.35;
/// Lake bed depth below the sheet at the deepest point.
pub const LAKE_BED_DEPTH: f32 = 14.0;
/// Width of the shallow apron inside a lake ring.
pub const LAKE_SHALLOW_M: f32 = 90.0;
/// Width of the bank blend outside a lake ring.
pub const LAKE_BANK_M: f32 = 70.0;
/// Ocean floor just outside the coast rim.
pub const OCEAN_NEAR_FLOOR: f32 = 8.0;
/// River thalweg depth below the sheet.
pub const RIVER_THALWEG_DEPTH: f32 = 14.0;
/// How far the river sheet sits below the structural land, forming banks.
pub const RIVER_VALLEY_BANK: f32 = 16.0;
/// Steep cut-bank width just outside the wet channel.
pub const RIVER_CUT_BANK_M: f32 = 28.0;
/// Land right behind the shore is lifted this far out of the sea.
pub const INLAND_FREEBOARD: f32 = 2.5;
/// Above this height land tints as rock.
pub const ROCK_HEIGHT: f32 = 600.0;
/// Below `sea + this` land tints as sand.
pub const SAND_BAND: f32 = 4.0;

const SWELL_HEIGHT: f32 = 12.0;
const HILL_HEIGHT: f32 = 38.0;
const RIPPLE_HEIGHT: f32 = 8.0;
const GRIT_HEIGHT: f32 = 2.2;
const MOUNTAIN_DETAIL: f32 = 90.0;
const WARP_STRENGTH: f32 = 70.0;
/// Extra domain warp of the loft in high relief. Without it the kilometre
/// interpolant, the neighbourhood contrast, and the swell all share one
/// wavelength and the range reads as a sine.
const ALPINE_WARP_M: f32 = 340.0;
const MACRO_NEAR_M: f32 = 880.0;
const MACRO_FAR_M: f32 = 2_150.0;
/// How hard high-relief loft is pulled away from that neighbourhood.
const MACRO_CONTRAST: f32 = 2.45;
/// Height where the land starts to read alpine, and where that is complete.
const ALPINE_M: f32 = 450.0;
const ALPINE_FULL_M: f32 = 1_500.0;

/// Isolated plains knolls: tall enough to read as a hill, small enough that
/// the atlas never had to know they were there.
const KNOLL_LOW_M: f32 = 52.0;
const KNOLL_HIGH_M: f32 = 96.0;
/// Low-frequency edge warp that keeps a hill's footprint from being a perfect
/// ellipse without turning a noise contour into the landform itself.
const KNOLL_EDGE_WARP_FREQ: f32 = 0.00038;
const KNOLL_EDGE_WARP_X_SCALE: f32 = 1.08;
const KNOLL_EDGE_WARP_Z_SCALE: f32 = 0.88;
/// One independently jittered hill site per cell. Unlike a slow noise gate,
/// this cannot cluster all hills into one district and leave a forest flat for
/// tens of kilometres.
const KNOLL_CELL_M: f32 = 2_800.0;
const KNOLL_JITTER_M: f32 = 900.0;
const KNOLL_RADIUS_LOW_M: f32 = 540.0;
const KNOLL_RADIUS_HIGH_M: f32 = 920.0;
const KNOLL_CLIFF_CHANCE: f32 = 0.16;

/// Rare, finite ravines. Each active site owns one cut with two shoulders;
/// there is no thresholded noise contour that can repeat across the world.
const RAVINE_CELL_M: f32 = 11_000.0;
const RAVINE_CHANCE: f32 = 0.32;
const RAVINE_JITTER_M: f32 = 3_000.0;
const RAVINE_HALF_LENGTH_LOW_M: f32 = 700.0;
const RAVINE_HALF_LENGTH_HIGH_M: f32 = 1_500.0;
const RAVINE_HALF_WIDTH_LOW_M: f32 = 70.0;
const RAVINE_HALF_WIDTH_HIGH_M: f32 = 130.0;
const RAVINE_DEPTH_LOW_M: f32 = 24.0;
const RAVINE_DEPTH_HIGH_M: f32 = 42.0;

/// How far mountain detail is pulled along its own derivative (drainage).
const ERODE_M: f32 = 240.0;

#[derive(Debug, Error)]
pub enum SurfaceError {
    #[error("atlas has land but no coast rings; the shoreline would be undefined")]
    MissingCoastRings,

    #[error("lake {id} sits at {surface_z} m, below sea level {sea} m")]
    LakeBelowSea { id: i32, surface_z: f32, sea: f32 },

    #[error("river {id} has a non-finite or sub-sea surface height {surface_z}")]
    BadRiverSheet { id: i32, surface_z: f32 },

    #[error("surface probe at ({x} m, {z} m) produced a non-finite ground height")]
    NonFiniteProbe { x: f64, z: f64 },
}

/// Which body a wet column belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaterBody {
    Ocean,
    Lake {
        id: i32,
    },
    River {
        class: i32,
    },
    /// A sub-atlas basin, filled to the height it would spill at.
    Pond,
}

/// What the sub-atlas water layer asks of a column it runs through.
///
/// The layer knows its own geometry and nothing about the landform, so it
/// states the sheet, how far the bed sits under it here, and how far inside the
/// water this point is. Everything else — which body wins, the clearance
/// contract — stays with the column.
#[derive(Clone, Copy, Debug)]
pub struct WaterCarve {
    pub sheet_z: f32,
    /// Bed below the sheet at this exact spot; negative on the bank above it.
    pub depth_m: f32,
    /// Metres into the water; negative outside it, in the bank blend.
    pub margin_m: f32,
    pub body: WaterBody,
}

/// Albedo class for chunk vertex tinting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceMaterial {
    Bed,
    Sand,
    Grass,
    Rock,
}

/// A fully resolved column of the continental surface.
#[derive(Clone, Copy, Debug)]
pub struct SurfaceColumn {
    ground: f32,
    /// Signed water field: `>= 0` is wet, and the magnitude is metres.
    wetness: f32,
    /// Sheet height of the governing body (only meaningful when wet).
    sheet: f32,
    body: Option<WaterBody>,
}

impl SurfaceColumn {
    pub fn ground(self) -> f32 {
        self.ground
    }

    /// Lower this column's land surface, leaving water as it is.
    pub(super) fn cap_ground(&mut self, y: f32) {
        if self.ground > y {
            self.ground = y;
        }
    }

    /// Signed water field used for contouring; `>= 0` means standing water.
    pub fn wetness(self) -> f32 {
        self.wetness
    }

    /// The same field as the marching squares see it.
    pub fn contour_wetness(self) -> f32 {
        self.wetness
    }

    /// Sink a sub-atlas basin into this column.
    ///
    /// Atlas hydrology wins outright: where an ocean, lake or river already
    /// stands, a pond has no business arguing about the sheet height.
    pub(super) fn carve(&mut self, carve: WaterCarve) {
        if self.body.is_some() {
            return;
        }
        let target = carve.sheet_z - carve.depth_m;
        self.ground = self.ground.min(target);
        if carve.margin_m < 0.0 {
            // Still report the distance to the bank. The contour that draws a
            // pond interpolates against the dry side, and the atlas figure out
            // here is the distance to the sea — kilometres — which would pull
            // every crossing onto the wet lattice point and shrink the pond to
            // the cells it happens to cover.
            self.wetness = self.wetness.max(carve.margin_m);
            return;
        }
        self.sheet = carve.sheet_z;
        self.body = Some(carve.body);
        self.ground = self
            .ground
            .min(self.sheet - MIN_WATER_DEPTH.max(WATER_CLEARANCE));
        self.wetness = (self.sheet - self.ground).min(carve.margin_m);
    }

    pub fn is_wet(self) -> bool {
        self.body.is_some()
    }

    pub fn body(self) -> Option<WaterBody> {
        self.body
    }

    /// Height of the water sheet where one exists.
    pub fn water_top(self) -> Option<f32> {
        self.body.map(|_| self.sheet)
    }

    /// Sheet height to interpolate against when contouring, wet or dry.
    ///
    /// Dry columns still report the sheet the nearest body *would* have, so a
    /// water polygon crossing the shoreline has a defined height at both ends.
    pub fn sheet_hint(self) -> f32 {
        self.sheet
    }

    pub fn material(self, sea: f32) -> SurfaceMaterial {
        if self.is_wet() {
            SurfaceMaterial::Bed
        } else if self.ground > ROCK_HEIGHT {
            SurfaceMaterial::Rock
        } else if self.ground < sea + SAND_BAND {
            SurfaceMaterial::Sand
        } else {
            SurfaceMaterial::Grass
        }
    }

    pub fn to_sample(self) -> SurfaceSample {
        match self.body {
            Some(_) => SurfaceSample::wet(self.ground, self.sheet),
            None => SurfaceSample::dry(self.ground),
        }
    }

    pub fn walk_height(self) -> f32 {
        match self.body {
            Some(_) => self.sheet,
            None => self.ground,
        }
    }
}

/// Structural terrain detail shared by every consumer of the surface.
///
/// Atlas loft is a kilometre-smooth field. Everything that makes a place — a
/// knoll on the plain, a scarp, a hanging bench — lives here, so the walked
/// mesh and the thirty-kilometre horizon are looking at the same landform.
/// Adding more FBM cannot put a break in a slope; the knolls and ravines
/// are the breaks. Both are gated on **peaks** of a blob field. Isolines of
/// a noise field stack as concentric fins, which is not a hill. River valleys
/// pass `detail_amt = 0` and those features fade, so a hill cannot dam an
/// authored channel.
#[derive(Clone, Debug)]
struct TerrainDetail {
    swell: Noise,
    hills: Noise,
    ripples: Noise,
    grit: Noise,
    mountain: Noise,
    warp_a: Noise,
    warp_b: Noise,
    knolls: Noise,
    knoll_seed: u64,
    ravine_dir: Noise,
    ravine_seed: u64,
    erode: Noise,
}

impl TerrainDetail {
    fn new(world_seed: i32) -> Self {
        let seed = world_seed as u32;
        Self {
            swell: Noise::new(seed ^ 0xA11CE),
            hills: Noise::new(seed ^ 0x4111_5711),
            ripples: Noise::new(seed ^ 0x2199_1E55),
            grit: Noise::new(seed ^ 0x6717_0001),
            mountain: Noise::new(seed ^ 0xB01D),
            warp_a: Noise::new(seed ^ 0xC0DE),
            warp_b: Noise::new(seed ^ 0xD00D),
            knolls: Noise::new(seed ^ 0x6E01_11A1),
            knoll_seed: u64::from(seed) ^ 0x6E01_11A1_D1A6_0007,
            ravine_dir: Noise::new(seed ^ 0x5CA9_9001),
            ravine_seed: u64::from(seed) ^ 0xFA01_7C11_5CA9_9001,
            erode: Noise::new(seed ^ 0xE80D_E001),
        }
    }

    fn elevation(&self, fields: &AtlasFields, x: f32, z: f32, detail_amt: f32) -> f32 {
        let relief = fields.sample_smooth(&fields.relief01, x, z).clamp(0.0, 1.0);
        let e_atlas = fields.sample_smooth(&fields.elevation_m, x, z);
        let alpine_hint = smoothstep(ALPINE_M, ALPINE_FULL_M, e_atlas) * relief;
        let warp_m = WARP_STRENGTH + ALPINE_WARP_M * alpine_hint;
        let warp_x = self.warp_a.sample2(x * 0.00032, z * 0.00032) * warp_m;
        let warp_z = self.warp_b.sample2(x * 0.00032 + 40.0, z * 0.00032) * warp_m;
        let px = x + warp_x;
        let pz = z + warp_z;

        let base = self.steepen_base(fields, px, pz, relief);
        let alpine = smoothstep(ALPINE_M, ALPINE_FULL_M, base);
        let (ex, ez) = self.eroded_point(px, pz, alpine * relief);
        let mut h = base + self.grain(ex, ez, relief, alpine, detail_amt);
        h += self.orogeny(ex, ez, relief, alpine, detail_amt);
        h += self.knolls(px, pz, relief, alpine, detail_amt);
        h += self.ravines(px, pz, relief, alpine, detail_amt);
        h
    }

    #[cfg(test)]
    fn debug_layers(
        &self,
        fields: &AtlasFields,
        x: f32,
        z: f32,
    ) -> (f32, f32, f32, f32, f32, f32, f32) {
        let relief = fields.sample_smooth(&fields.relief01, x, z).clamp(0.0, 1.0);
        let e_atlas = fields.sample_smooth(&fields.elevation_m, x, z);
        let alpine_hint = smoothstep(ALPINE_M, ALPINE_FULL_M, e_atlas) * relief;
        let warp_m = WARP_STRENGTH + ALPINE_WARP_M * alpine_hint;
        let warp_x = self.warp_a.sample2(x * 0.00032, z * 0.00032) * warp_m;
        let warp_z = self.warp_b.sample2(x * 0.00032 + 40.0, z * 0.00032) * warp_m;
        let px = x + warp_x;
        let pz = z + warp_z;
        let base = self.steepen_base(fields, px, pz, relief);
        let alpine = smoothstep(ALPINE_M, ALPINE_FULL_M, base);
        let (ex, ez) = self.eroded_point(px, pz, alpine * relief);
        (
            relief,
            e_atlas,
            base,
            self.grain(ex, ez, relief, alpine, 1.0),
            self.orogeny(ex, ez, relief, alpine, 1.0),
            self.knolls(px, pz, relief, alpine, 1.0),
            self.ravines(px, pz, relief, alpine, 1.0),
        )
    }

    /// Quiet floor: swell at a kilometre, a little FBM, grit underfoot.
    /// Plains used to wear 20 m of hill noise everywhere; that was the sine.
    fn grain(&self, px: f32, pz: f32, relief: f32, alpine: f32, detail_amt: f32) -> f32 {
        let swell = self.swell.sample2(px * 0.0008, pz * 0.0008)
            * SWELL_HEIGHT
            * lerp(1.0, 0.4, relief)
            * lerp(1.0, 0.12, alpine);
        let hills = self.hills.fbm2(px * 0.0024, pz * 0.0024, 3, 2.05, 0.5)
            * HILL_HEIGHT
            * lerp(0.14, 1.0, relief)
            * lerp(1.0, 0.22, alpine)
            * lerp(0.35, 1.0, detail_amt);
        let ripples = self.ripples.fbm2(px * 0.006, pz * 0.006, 3, 2.1, 0.5)
            * RIPPLE_HEIGHT
            * lerp(0.32, 1.0, relief)
            * detail_amt;
        let grit = self.grit.fbm2(px * 0.02, pz * 0.02, 2, 2.2, 0.5) * GRIT_HEIGHT * detail_amt;
        swell + hills + ripples + grit
    }

    /// Pull later mountain octaves along the derivative of an erosion field so
    /// crests bunch, skip, and drain instead of stacking at one wavelength.
    fn eroded_point(&self, x: f32, z: f32, amount: f32) -> (f32, f32) {
        if amount < 0.04 {
            return (x, z);
        }
        let e = 22.0;
        let freq = 0.00095;
        let n = self.erode.sample2(x * freq, z * freq);
        let nx = self.erode.sample2((x + e) * freq, z * freq);
        let nz = self.erode.sample2(x * freq, (z + e) * freq);
        let k = ERODE_M * amount;
        let x1 = x + (nx - n) / e * k;
        let z1 = z + (nz - n) / e * k;
        let f2 = 0.0024;
        let e2 = 12.0;
        let m = self.erode.sample2(x1 * f2 + 17.0, z1 * f2);
        let mx = self.erode.sample2((x1 + e2) * f2 + 17.0, z1 * f2);
        let mz = self.erode.sample2(x1 * f2 + 17.0, (z1 + e2) * f2);
        let k2 = ERODE_M * 0.38 * amount;
        (x1 + (mx - m) / e2 * k2, z1 + (mz - m) / e2 * k2)
    }

    /// Rounded mountain grain. Ridged noise puts a wall on every isoline of
    /// the field — a comb of fins — so it is not used here. Cliffs come from
    /// knolls, which are peaks, not contours.
    fn orogeny(&self, px: f32, pz: f32, relief: f32, alpine: f32, detail_amt: f32) -> f32 {
        if relief < 0.08 && alpine < 0.05 {
            return 0.0;
        }
        let n = self.mountain.fbm2(px * 0.0009, pz * 0.0009, 3, 2.05, 0.5);
        n * MOUNTAIN_DETAIL
            * lerp(0.06, 0.80, relief)
            * lerp(0.18, 1.20, alpine)
            * lerp(0.25, 1.0, detail_amt)
    }

    /// Occasional bulky knolls, independently jittered across large cells.
    ///
    /// Every site is a full two-dimensional mound hundreds of metres across.
    /// Some have a shorter windward flank, but retain the long backslope that
    /// makes the feature a hill. Noise gates and face remaps are deliberately
    /// absent: their contour boundaries were the parallel fins seen in flight.
    fn knolls(&self, x: f32, z: f32, relief: f32, alpine: f32, detail_amt: f32) -> f32 {
        let plains = (1.0 - alpine) * lerp(1.0, 0.22, relief);
        let upland = alpine * relief * 0.80;
        let want = (plains + upland) * detail_amt;
        if want < 0.06 {
            return 0.0;
        }
        let cell_x = (x / KNOLL_CELL_M).floor() as i64;
        let cell_z = (z / KNOLL_CELL_M).floor() as i64;
        let mut height_m = 0.0_f32;

        for dz in -1_i64..=1_i64 {
            for dx in -1_i64..=1_i64 {
                let site_x = cell_x + dx;
                let site_z = cell_z + dz;
                let mut rng = CellRng::new(self.knoll_seed, site_x, site_z);
                let centre_x = (site_x as f32 + 0.5) * KNOLL_CELL_M
                    + rng.range(-KNOLL_JITTER_M, KNOLL_JITTER_M);
                let centre_z = (site_z as f32 + 0.5) * KNOLL_CELL_M
                    + rng.range(-KNOLL_JITTER_M, KNOLL_JITTER_M);
                let rel_x = x - centre_x;
                let rel_z = z - centre_z;
                const MAX_REACH_M: f32 = KNOLL_RADIUS_HIGH_M * 1.24;
                if rel_x.abs() > MAX_REACH_M || rel_z.abs() > MAX_REACH_M {
                    continue;
                }
                let radius_m = rng.range(KNOLL_RADIUS_LOW_M, KNOLL_RADIUS_HIGH_M);
                let across_radius_m = radius_m * rng.range(0.72, 1.0);
                let angle_rad = rng.range(0.0, std::f32::consts::TAU);
                let amplitude_m = rng.range(KNOLL_LOW_M, KNOLL_HIGH_M);
                let has_cliff = rng.unit() < KNOLL_CLIFF_CHANCE;
                let cliff_position_m = radius_m * rng.range(0.34, 0.48);
                let cliff_width_m = rng.range(20.0, 42.0);
                let cliff_drop_m = amplitude_m * rng.range(0.30, 0.40);

                let (sin_a, cos_a) = angle_rad.sin_cos();
                let along_m = rel_x * cos_a + rel_z * sin_a;
                let across_m = -rel_x * sin_a + rel_z * cos_a;
                let mut radial =
                    ((along_m / radius_m).powi(2) + (across_m / across_radius_m).powi(2)).sqrt();
                let edge_warp = self.knolls.sample2(
                    x * KNOLL_EDGE_WARP_FREQ * KNOLL_EDGE_WARP_X_SCALE,
                    z * KNOLL_EDGE_WARP_FREQ * KNOLL_EDGE_WARP_Z_SCALE,
                ) * 0.08
                    * smoothstep(0.20, 0.85, radial);
                radial += edge_warp;
                if radial >= 1.0 {
                    continue;
                }
                let body = smoothstep(0.0, 1.0, 1.0 - radial);
                let mut candidate_m = body * amplitude_m;

                if has_cliff && along_m > cliff_position_m - cliff_width_m {
                    // A single planar scarp on the outward half of this mound.
                    // The lateral mask caps both ends; the apron follows the
                    // original hill body to zero, so no second wall appears at
                    // the footprint edge.
                    let lateral = (across_m.abs() / across_radius_m).clamp(0.0, 1.0);
                    let lateral_mask = 1.0 - smoothstep(0.38, 0.82, lateral);
                    let face_step = smoothstep(
                        cliff_position_m - cliff_width_m * 0.5,
                        cliff_position_m + cliff_width_m * 0.5,
                        along_m,
                    );
                    let face_radial = ((cliff_position_m / radius_m).powi(2)
                        + (across_m / across_radius_m).powi(2))
                    .sqrt();
                    let face_body = smoothstep(0.0, 1.0, 1.0 - face_radial).max(0.10);
                    let apron = (body / face_body).clamp(0.0, 1.0);
                    candidate_m =
                        (candidate_m - cliff_drop_m * face_step * lateral_mask * apron).max(0.0);
                }

                height_m = height_m.max(candidate_m);
            }
        }

        height_m * want
    }

    /// Rare finite ravines, each with a cut and a raised shoulder on both sides.
    ///
    /// Sites are independently hashed and bounded in both axes. Noise only
    /// meanders the centreline; it never decides where a wall exists.
    fn ravines(&self, x: f32, z: f32, relief: f32, alpine: f32, detail_amt: f32) -> f32 {
        let want = detail_amt * lerp(0.65, 1.0, relief) * lerp(1.0, 0.40, alpine);
        if want < 0.08 {
            return 0.0;
        }
        let cell_x = (x / RAVINE_CELL_M).floor() as i64;
        let cell_z = (z / RAVINE_CELL_M).floor() as i64;
        let mut contribution_m = 0.0_f32;

        for dz in -1_i64..=1_i64 {
            for dx in -1_i64..=1_i64 {
                let site_x = cell_x + dx;
                let site_z = cell_z + dz;
                let mut rng = CellRng::new(self.ravine_seed, site_x, site_z);
                if rng.unit() >= RAVINE_CHANCE {
                    continue;
                }
                let centre_x = (site_x as f32 + 0.5) * RAVINE_CELL_M
                    + rng.range(-RAVINE_JITTER_M, RAVINE_JITTER_M);
                let centre_z = (site_z as f32 + 0.5) * RAVINE_CELL_M
                    + rng.range(-RAVINE_JITTER_M, RAVINE_JITTER_M);
                let rel_x = x - centre_x;
                let rel_z = z - centre_z;
                const MAX_REACH_M: f32 = RAVINE_HALF_LENGTH_HIGH_M + RAVINE_HALF_WIDTH_HIGH_M * 3.0;
                if rel_x.abs() > MAX_REACH_M || rel_z.abs() > MAX_REACH_M {
                    continue;
                }

                let angle_rad = rng.range(0.0, std::f32::consts::TAU);
                let half_length_m = rng.range(RAVINE_HALF_LENGTH_LOW_M, RAVINE_HALF_LENGTH_HIGH_M);
                let half_width_m = rng.range(RAVINE_HALF_WIDTH_LOW_M, RAVINE_HALF_WIDTH_HIGH_M);
                let depth_m = rng.range(RAVINE_DEPTH_LOW_M, RAVINE_DEPTH_HIGH_M);
                let phase = rng.range(-80.0, 80.0);
                let (sin_a, cos_a) = angle_rad.sin_cos();
                let along_m = rel_x * cos_a + rel_z * sin_a;
                if along_m.abs() >= half_length_m {
                    continue;
                }
                let envelope = 1.0 - smoothstep(0.62, 1.0, along_m.abs() / half_length_m);
                let wobble_m = self
                    .ravine_dir
                    .sample2((along_m + phase) * 0.00075, phase * 0.013)
                    * half_width_m
                    * 0.38
                    * envelope;
                let across_m = (-rel_x * sin_a + rel_z * cos_a - wobble_m).abs();

                let cut_t = (across_m / half_width_m).clamp(0.0, 1.0);
                let cut = (1.0 - cut_t).powf(1.35);
                let shoulder_u =
                    ((across_m - half_width_m) / (half_width_m * 1.20)).clamp(0.0, 1.0);
                let shoulder = (4.0 * shoulder_u * (1.0 - shoulder_u)).powi(2);
                let ravine_m = (-depth_m * cut + depth_m * 0.42 * shoulder) * envelope * want;
                if ravine_m.abs() > contribution_m.abs() {
                    contribution_m = ravine_m;
                }
            }
        }

        contribution_m
    }

    /// Pull atlas loft away from a neighbourhood in high relief.
    ///
    /// Two radii, rotated by the warp, so the contrast is not a 1.4 km Laplacian
    /// of a bicubic field — which is a sine.
    fn steepen_base(&self, fields: &AtlasFields, x: f32, z: f32, relief: f32) -> f32 {
        let e0 = fields.sample_smooth(&fields.elevation_m, x, z);
        if relief < 0.04 || e0 < 8.0 {
            return e0;
        }
        let turn = self.warp_b.sample2(x * 0.00019, z * 0.00019) * std::f32::consts::PI;
        let jitter = self.warp_a.sample2(x * 0.00021, z * 0.00021);
        let near = self.arm_avg(fields, x, z, MACRO_NEAR_M * (1.0 + 0.16 * jitter), turn);
        let far = self.arm_avg(
            fields,
            x,
            z,
            MACRO_FAR_M * (1.0 + 0.10 * jitter),
            turn + 0.7,
        );
        let e_avg = 0.58 * near + 0.42 * far;
        let alpine = smoothstep(ALPINE_M, ALPINE_FULL_M, e0.max(e_avg));
        let contrast = lerp(1.0, MACRO_CONTRAST, relief) * lerp(1.0, 1.22, alpine);
        let delta = e0 - e_avg;
        let amount = if delta >= 0.0 {
            contrast
        } else {
            lerp(1.0, contrast, 0.55)
        };
        let steep = e_avg + delta * amount;
        steep.max(e0 - 280.0)
    }

    fn arm_avg(&self, fields: &AtlasFields, x: f32, z: f32, r: f32, angle: f32) -> f32 {
        let (c, s) = (angle.cos(), angle.sin());
        let at = |dx: f32, dz: f32| fields.sample_bilinear(&fields.elevation_m, x + dx, z + dz);
        0.25 * (at(c * r, s * r) + at(-c * r, -s * r) + at(-s * r, c * r) + at(s * r, -c * r))
    }
}

/// Atlas settlement node, in world metres. The 3D layer plants a layout here.
#[derive(Clone, Copy, Debug)]
pub struct SettlementPin {
    pub id: i32,
    pub at: GlobalXZ,
    /// 0=hamlet … 3=port, from population and river-mouth distance.
    pub tier: u8,
    pub population: i32,
}

/// Godot `VillageTier.classify`: inland low-pop is a hamlet; a river mouth is a port.
pub fn classify_settlement(population: i32, mouth_dist: i32) -> u8 {
    if mouth_dist == 0 && population >= 10 {
        3
    } else if mouth_dist == 0 || population >= 11 {
        2
    } else if population >= 9 || mouth_dist > 0 {
        1
    } else {
        0
    }
}

fn pins_from_atlas(atlas: &ContinentAtlas) -> Arc<[SettlementPin]> {
    atlas
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Settlement)
        .map(|n| {
            let at = GlobalXZ::at(
                (n.ax as f64 + 0.5) * f64::from(CELL_METRES),
                (n.az as f64 + 0.5) * f64::from(CELL_METRES),
            );
            let pop = cell_population(atlas.cell_at(n.ax, n.az));
            let mouth = atlas.mouth_distance[atlas.index_of(n.ax, n.az)];
            SettlementPin {
                id: n.id,
                at,
                tier: classify_settlement(pop, mouth),
                population: pop,
            }
        })
        .collect()
}

/// Single authority for terrain and hydrology in world metres.
#[derive(Clone)]
pub struct ContinentalSurface {
    fields: Arc<AtlasFields>,
    hydro: Arc<HydroVectors>,
    index: Arc<HydroIndex>,
    bounds: AtlasBounds,
    sea_surface_z: f32,
    world_seed: i32,
    detail: TerrainDetail,
    settlements: Arc<[SettlementPin]>,
    roads: Arc<[RoadPath]>,
}

impl ContinentalSurface {
    /// Build and validate the surface for a generated atlas.
    pub fn new(atlas: &ContinentAtlas) -> Result<Self, SurfaceError> {
        Self::with_fields(atlas, Arc::new(AtlasFields::build(atlas)))
    }

    pub fn with_fields(
        atlas: &ContinentAtlas,
        fields: Arc<AtlasFields>,
    ) -> Result<Self, SurfaceError> {
        let bounds = AtlasBounds::of(atlas);
        let surface = Self {
            fields,
            index: Arc::new(HydroIndex::build(&atlas.hydro, bounds.size())),
            hydro: Arc::clone(&atlas.hydro),
            bounds,
            sea_surface_z: atlas.hydro.sea_surface_z,
            world_seed: atlas.world_seed,
            detail: TerrainDetail::new(atlas.world_seed),
            settlements: pins_from_atlas(atlas),
            roads: bake_road_paths(atlas).into(),
        };
        surface.validate()?;
        Ok(surface)
    }

    fn validate(&self) -> Result<(), SurfaceError> {
        let sea = self.sea_surface_z;
        for lake in &self.hydro.lakes {
            if !lake.surface_z.is_finite() || lake.surface_z < sea {
                return Err(SurfaceError::LakeBelowSea {
                    id: lake.id,
                    surface_z: lake.surface_z,
                    sea,
                });
            }
        }
        for river in &self.hydro.rivers {
            if let Some(bad) = river
                .surface_z
                .iter()
                .find(|z| !z.is_finite() || **z < sea - 1e-3)
            {
                return Err(SurfaceError::BadRiverSheet {
                    id: river.id,
                    surface_z: *bad,
                });
            }
        }
        let has_land = self.fields.elevation_m.iter().any(|e| *e > sea + 1.0);
        if has_land && self.hydro.coasts.is_empty() {
            return Err(SurfaceError::MissingCoastRings);
        }

        // Probe a coarse lattice: a broken field must fail at construction, not
        // halfway through streaming a chunk the player already walked into.
        let span = self.bounds.metres();
        let steps = 11;
        for iz in 0..steps {
            for ix in 0..steps {
                let p = GlobalXZ::at(
                    span * (ix as f64 + 0.5) / steps as f64,
                    span * (iz as f64 + 0.5) / steps as f64,
                );
                let column = self.column(p);
                if !column.ground.is_finite() || !column.wetness.is_finite() {
                    return Err(SurfaceError::NonFiniteProbe { x: p.x, z: p.z });
                }
            }
        }
        Ok(())
    }

    pub fn fields(&self) -> &AtlasFields {
        &self.fields
    }

    /// Seed everything derived from this continent hangs off.
    pub fn world_seed(&self) -> i32 {
        self.world_seed
    }

    pub fn hydro(&self) -> &HydroVectors {
        &self.hydro
    }

    /// Spatial index over the hydro outlines, for callers that need geometry
    /// (nearest river, coast distance) rather than a resolved column.
    pub fn hydro_index(&self) -> &HydroIndex {
        &self.index
    }

    pub fn bounds(&self) -> AtlasBounds {
        self.bounds
    }

    pub fn sea_surface_z(&self) -> f32 {
        self.sea_surface_z
    }

    /// Settlement nodes on this continent, in world metres.
    pub fn settlements(&self) -> &[SettlementPin] {
        &self.settlements
    }

    /// Highest-tier settlement, then highest population, then stable id.
    ///
    /// Same order as Godot `SettlementLayout.spawn_plaza_largest`: port before
    /// town before village before hamlet.
    pub fn largest_settlement(&self) -> Option<SettlementPin> {
        self.settlements
            .iter()
            .copied()
            .max_by_key(|pin| (pin.tier, pin.population, pin.id))
    }

    /// Atlas roads in world metres, meandered like the map overlay.
    pub fn roads(&self) -> &[RoadPath] {
        &self.roads
    }

    /// Dry structural height with no hydro carving (used for bank references).
    pub fn base_ground(&self, p: GlobalXZ) -> f32 {
        self.detail
            .elevation(&self.fields, p.x as f32, p.z as f32, 1.0)
    }

    #[cfg(test)]
    pub(super) fn debug_layers(&self, x: f32, z: f32) -> (f32, f32, f32, f32, f32, f32, f32) {
        self.detail.debug_layers(&self.fields, x, z)
    }

    /// Fully resolved column at `p`.
    ///
    /// Every tier of the visibility ladder, from the four-metre ground under the
    /// player's feet to the thirty-kilometre horizon, reads this one function.
    /// Each is a downsample of the same landform, so the only way two of them
    /// can disagree is the spacing of their samples — and a cheap stand-in that
    /// skipped the hydrology would disagree by kilometres, because the atlas
    /// elevation it would have to read is the land as laid down, before the
    /// coast rings decided what the sea covers.
    pub fn column(&self, p: GlobalXZ) -> SurfaceColumn {
        let x = p.x as f32;
        let z = p.z as f32;
        let xz = Vec2::new(x, z);
        let sea = self.sea_surface_z;

        let base_raw = self.detail.elevation(&self.fields, x, z, 1.0);
        let mut ground = base_raw;

        // --- Coast: both sides meet the sea exactly at the rim ----------------
        //
        // The rim is the waterline, so the bed must reach sea level there and the
        // land must come down to it. Stepping straight to a shelf depth (or
        // keeping the full inland height up to the rim) would put a wall around
        // every island.
        let coast_sd = self.index.coast_signed(&self.hydro, xz);
        if coast_sd < 0.0 {
            let seaward = (-coast_sd).min(2_000.0);
            let shelf = smoothstep(0.0, SHORE_BAND_M, seaward);
            let depth = OCEAN_NEAR_FLOOR
                + OCEAN_SHELF_DEPTH * shelf
                + ((seaward - SHORE_BAND_M).max(0.0) * 0.02).min(40.0);
            // Blend from the waterline, not from the raw field: an atlas ocean
            // cell already reads tens of metres down, and taking it directly at
            // the rim left a cliff between the last dry sample and the first wet
            // one.
            ground = lerp(sea, ground.min(sea - depth), shelf);
        } else {
            let dryness = smoothstep(0.0, SHORE_BAND_M, coast_sd);
            let beach = sea + INLAND_FREEBOARD * dryness;
            ground = lerp(beach, ground.max(beach), dryness);
        }

        // --- Lakes: shallow apron inside the ring, bank blend outside --------
        let lake_hit = self.index.nearest_lake(&self.hydro, xz);
        let mut lake_sheet = None;
        if let Some((lake, sd)) = lake_hit {
            let sheet = lake.surface_z.max(sea);
            if sd >= 0.0 {
                let deep = smoothstep(0.0, LAKE_SHALLOW_M, sd);
                let target = lerp(sheet - MIN_WATER_DEPTH, sheet - LAKE_BED_DEPTH, deep);
                ground = ground.min(target);
            } else {
                // Meet the sheet at the rim so the shore has no wall or spill.
                let away = smoothstep(0.0, LAKE_BANK_M, -sd);
                ground = lerp(sheet, ground, away);
            }
            lake_sheet = Some((lake.id, sheet, sd));
        }

        // --- Rivers: wide valley, cut bank, inner channel --------------------
        let river_hit = self.index.nearest_river(&self.hydro, xz);
        let mut river_sheet = None;
        if let Some(hit) = river_hit {
            let channel_w = hit.half_width.max(1.0);
            let valley_r = valley_radius(hit.class, channel_w);
            if hit.dist < valley_r {
                let smooth = self.detail.elevation(&self.fields, x, z, 0.0);
                let detail_blend = smoothstep(channel_w * 0.5, valley_r, hit.dist);
                let structural = lerp(smooth, base_raw, detail_blend);
                // The sheet may only sit below structural land: banks are what
                // we leave uncarved, so an authored height that is too high is
                // pulled down rather than floated over the valley.
                let sheet = hit
                    .sheet_z
                    .max(sea)
                    .min(structural - RIVER_VALLEY_BANK)
                    .max(sea);
                let target = if hit.dist < channel_w {
                    let t = (hit.dist / channel_w).clamp(0.0, 1.0);
                    sheet - RIVER_THALWEG_DEPTH * (1.0 - t * t).max(0.2)
                } else if hit.dist < channel_w + RIVER_CUT_BANK_M {
                    let u = ((hit.dist - channel_w) / RIVER_CUT_BANK_M).clamp(0.0, 1.0);
                    lerp(sheet + 0.5, structural, smoothstep(0.0, 1.0, u))
                } else {
                    let u = ((hit.dist - channel_w - RIVER_CUT_BANK_M)
                        / (valley_r - channel_w - RIVER_CUT_BANK_M).max(1.0))
                    .clamp(0.0, 1.0);
                    lerp(structural - 1.5, structural, smoothstep(0.0, 1.0, u))
                };
                ground = ground.min(target);
                river_sheet = Some((hit.class, sheet, channel_w - hit.dist));
            }
        }

        // --- Signed water field: depth ∧ distance into the body --------------
        let mut wetness = f32::NEG_INFINITY;
        let mut sheet = sea;
        let mut body = None;

        let ocean = (sea - ground).min(-coast_sd);
        if ocean > wetness {
            wetness = ocean;
            sheet = sea;
            body = Some(WaterBody::Ocean);
        }
        if let Some((id, lake_top, sd)) = lake_sheet {
            let signed = (lake_top - ground).min(sd);
            if signed > wetness {
                wetness = signed;
                sheet = lake_top;
                body = Some(WaterBody::Lake { id });
            }
        }
        if let Some((class, river_top, margin)) = river_sheet {
            let signed = (river_top - ground).min(margin);
            if signed > wetness {
                wetness = signed;
                sheet = river_top;
                body = Some(WaterBody::River { class });
            }
        }

        if wetness >= 0.0 {
            // Guarantee the clearance contract by construction.
            ground = ground.min(sheet - MIN_WATER_DEPTH.max(WATER_CLEARANCE));
        } else {
            body = None;
        }

        SurfaceColumn {
            ground,
            wetness,
            sheet,
            body,
        }
    }

    /// Column used by a drawn grid whose cells are wider than a river.
    ///
    /// A 125 m quad can straddle a 68 m channel without any corner landing in
    /// it. Interpolating four dry banks then lays a solid slab over the river.
    /// For an unresolved channel, vertices within half a cell diagonal of it
    /// are conservatively capped to the real centreline bed. The ordinary
    /// four- and twenty-five-metre grids resolve the channel and are unchanged.
    pub(super) fn column_for_grid(&self, p: GlobalXZ, sample_m: f32, sink_m: f32) -> SurfaceColumn {
        let mut column = self.column(p);
        let xz = Vec2::new(p.x as f32, p.z as f32);
        let Some(hit) = self.index.nearest_river(&self.hydro, xz) else {
            return column;
        };
        let channel_width_m = hit.half_width.max(1.0) * 2.0;
        if sample_m <= channel_width_m {
            return column;
        }
        let half_diagonal_m = sample_m * std::f32::consts::FRAC_1_SQRT_2;
        if hit.dist > hit.half_width + half_diagonal_m {
            return column;
        }

        let centreline = self.column(GlobalXZ::at(f64::from(hit.at.x), f64::from(hit.at.y)));
        // The builder subtracts `sink_m` after sampling. Include it here so the
        // rendered coarse bed lands at the canonical centreline bed.
        column.cap_ground(centreline.ground() + sink_m);
        column
    }

    pub fn sample(&self, p: GlobalXZ) -> SurfaceSample {
        self.column(p).to_sample()
    }

    /// Signed water field; `>= 0` is standing water.
    pub fn wetness(&self, p: GlobalXZ) -> f32 {
        self.column(p).wetness()
    }

    /// An upper bound on `column(p).wetness()`, from the outlines alone.
    ///
    /// Every body contributes `min(depth, distance)` to the signed water field,
    /// so the distance halves on their own can only overstate it. That makes
    /// this a sound way to prove a spot dry, and it costs three index lookups
    /// where a column costs several octaves of noise on top of them — which is
    /// the difference between forty thousand reed candidates being affordable
    /// and not.
    pub fn water_reach(&self, p: GlobalXZ) -> f32 {
        let xz = Vec2::new(p.x as f32, p.z as f32);
        let mut reach = -self.index.coast_signed(&self.hydro, xz);
        if let Some((_, sd)) = self.index.nearest_lake(&self.hydro, xz) {
            reach = reach.max(sd);
        }
        if let Some(hit) = self.index.nearest_river(&self.hydro, xz) {
            reach = reach.max(hit.half_width.max(1.0) - hit.dist);
        }
        reach
    }

    pub fn is_wet(&self, p: GlobalXZ) -> bool {
        self.column(p).is_wet()
    }

    /// Height a walker stands at: the sheet when wet, the ground otherwise.
    pub fn walk_height(&self, p: GlobalXZ) -> f32 {
        self.column(p).walk_height()
    }
}

impl SurfaceSource for ContinentalSurface {
    fn sample(&self, x: f32, z: f32) -> SurfaceSample {
        self.column(GlobalXZ::at(x as f64, z as f64)).to_sample()
    }
}

impl std::fmt::Debug for ContinentalSurface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContinentalSurface")
            .field("atlas_cells", &self.bounds.size())
            .field("sea_surface_z", &self.sea_surface_z)
            .field("rivers", &self.hydro.rivers.len())
            .field("lakes", &self.hydro.lakes.len())
            .field("coasts", &self.hydro.coasts.len())
            .finish()
    }
}

/// Distance from a river centreline at which the valley has faded out.
fn valley_radius(class: i32, channel_w: f32) -> f32 {
    let by_class: f32 = match class {
        0 => 220.0,
        1 => 160.0,
        2 => 120.0,
        _ => 90.0,
    };
    by_class.max(channel_w * 4.0)
}

#[inline]
pub(super) fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[inline]
pub(super) fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0 + 1e-6)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
