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
use crate::atlas::hydro::HydroVectors;
use crate::atlas::{bake_road_paths, cell_population, ContinentAtlas, NodeKind, RoadPath, CELL_METRES};

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
/// Secondary crests and hanging valleys, shorter than the loft wave.
const SPUR_DETAIL: f32 = 48.0;
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
    /// A sub-atlas channel, seated and drawn on the same lattice as the land.
    Brook,
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
    /// Cut or fill to the profile. A brook on a hillside has to raise a floor
    /// on the downhill side or the sheet is a pane over the slope; a pond
    /// never fills, it only cuts.
    pub seat: bool,
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

    /// Sink a sub-atlas channel or basin into this column.
    ///
    /// Atlas hydrology wins outright: where an ocean, lake or river already
    /// stands, a brook has no business arguing about the sheet height — it was
    /// traced until it reached exactly this water and stopped.
    pub(super) fn carve(&mut self, carve: WaterCarve) {
        if self.body.is_some() {
            return;
        }
        let target = carve.sheet_z - carve.depth_m;
        if carve.seat {
            self.ground = target;
        } else {
            self.ground = self.ground.min(target);
        }
        if carve.margin_m < 0.0 {
            // Still report the distance to the bank. The contour that draws a
            // pond interpolates against the dry side, and the atlas figure out
            // here is the distance to the sea — kilometres — which would pull
            // every crossing onto the wet lattice point and shrink the pond to
            // the cells it happens to cover.
            self.wetness = self.wetness.max(carve.margin_m);
            if carve.seat {
                self.sheet = carve.sheet_z;
            }
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
#[derive(Clone, Debug)]
struct TerrainDetail {
    swell: Noise,
    hills: Noise,
    ripples: Noise,
    grit: Noise,
    mountain: Noise,
    spurs: Noise,
    warp_a: Noise,
    warp_b: Noise,
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
            spurs: Noise::new(seed ^ 0x5B09),
            warp_a: Noise::new(seed ^ 0xC0DE),
            warp_b: Noise::new(seed ^ 0xD00D),
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
        let swell = self.swell.sample2(px * 0.0008, pz * 0.0008)
            * SWELL_HEIGHT
            * lerp(1.0, 0.4, relief)
            * lerp(1.0, 0.12, alpine);
        let hills = self.hills.fbm2(px * 0.0024, pz * 0.0024, 4, 2.05, 0.5)
            * HILL_HEIGHT
            * lerp(0.55, 1.0, relief)
            * lerp(1.0, 0.22, alpine)
            * lerp(0.35, 1.0, detail_amt);
        let ripples = self.ripples.fbm2(px * 0.006, pz * 0.006, 3, 2.1, 0.5)
            * RIPPLE_HEIGHT
            * lerp(0.7, 1.0, relief)
            * detail_amt;
        let grit = self.grit.fbm2(px * 0.02, pz * 0.02, 2, 2.2, 0.5) * GRIT_HEIGHT * detail_amt;
        // Crests sit off the loft wavelength (~700 m, not 1.4 km) so amplifying
        // the atlas cannot keep a parallel wave.
        let ridge01 = self.mountain.ridged2(px * 0.0014, pz * 0.0014, 4, 2.18, 0.46) * 0.5 + 0.5;
        let shaped = ridge01.clamp(0.0, 1.0).powf(lerp(1.45, 2.15, alpine));
        let ridge = (shaped - 0.40)
            * MOUNTAIN_DETAIL
            * lerp(0.35, 1.0, relief)
            * lerp(0.50, 1.85, alpine)
            * lerp(0.25, 1.0, detail_amt);
        let spur01 = self.spurs.ridged2(px * 0.0038, pz * 0.0038, 3, 2.25, 0.5) * 0.5 + 0.5;
        let spur = (spur01.clamp(0.0, 1.0).powf(1.85) - 0.38)
            * SPUR_DETAIL
            * relief
            * alpine
            * lerp(0.20, 1.0, detail_amt);
        base + swell + hills + ripples + grit + ridge + spur
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
