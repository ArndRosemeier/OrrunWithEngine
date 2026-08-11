//! Continental surface: pure function of metres → [`SurfaceSample`].
//!
//! Water comes only from atlas [`HydroVectors`] (ribbons / rings), never from
//! wet rectangles on the climate grid.

use std::sync::Arc;

use engine::proc::Noise;
use engine::surface::{SurfaceSample, SurfaceSource, WATER_CLEARANCE};
use glam::Vec2;

use super::atlas_fields::AtlasFields;
use super::hydro_geom::{
    coast_signed, lake_at, nearest_river, ESTUARY_BLEND_METRES, LAKE_BED_DEPTH, OCEAN_FLOOR_MARGIN,
    OCEAN_SHELF_DEPTH, SHORE_BAND_M,
};
use crate::atlas::features::{edge_owner, Dir, EndpointKind, Kind};
use crate::atlas::hydro::HydroVectors;
use crate::atlas::{ContinentAtlas, CELL_METRES};

const SWELL_HEIGHT: f32 = 9.0;
const MOUNTAIN_DETAIL: f32 = 55.0;
const WARP_STRENGTH: f32 = 180.0;
const TRUNK_BANK_RISE: f32 = 6.0;
const INLAND_FREEBOARD: f32 = 2.5;
const FREEBOARD_SPAN: f32 = 26.0;

#[derive(Clone, Debug)]
struct ValleySeg {
    a: Vec2,
    b: Vec2,
    class: i32,
    bed_a: f32,
    bed_b: f32,
}

/// Pure continental sampler (atlas climate + vector hydro + detail).
#[derive(Clone)]
pub struct ContinentalSurface {
    fields: Arc<AtlasFields>,
    hydro: Arc<HydroVectors>,
    atlas_size: usize,
    valleys: Arc<[ValleySeg]>,
    swell: Noise,
    mountain: Noise,
    warp_a: Noise,
    warp_b: Noise,
}

impl ContinentalSurface {
    pub fn new(atlas: &ContinentAtlas, fields: AtlasFields) -> Self {
        let seed = atlas.world_seed as u32;
        let valleys = build_valley_segs(atlas, &fields);
        Self {
            fields: Arc::new(fields),
            hydro: Arc::clone(&atlas.hydro),
            atlas_size: atlas.size,
            valleys: valleys.into(),
            swell: Noise::new(seed ^ 0xA11CE),
            mountain: Noise::new(seed ^ 0xB01D),
            warp_a: Noise::new(seed ^ 0xC0DE),
            warp_b: Noise::new(seed ^ 0xD00D),
        }
    }

    pub fn fields(&self) -> &AtlasFields {
        &self.fields
    }

    pub fn hydro(&self) -> &HydroVectors {
        &self.hydro
    }

    fn base_height(&self, world_x: f32, world_z: f32) -> f32 {
        let warp_x = self.warp_a.sample2(world_x * 0.00035, world_z * 0.00035) * WARP_STRENGTH;
        let warp_z =
            self.warp_b.sample2(world_x * 0.00035 + 40.0, world_z * 0.00035) * WARP_STRENGTH;
        let px = world_x + warp_x;
        let pz = world_z + warp_z;

        let relief = self
            .fields
            .sample_smooth(&self.fields.relief01, world_x, world_z)
            .clamp(0.0, 1.0);
        let base = self
            .fields
            .sample_smooth(&self.fields.elevation_m, world_x, world_z);

        let swell = self.swell.sample2(px * 0.0008, pz * 0.0008)
            * SWELL_HEIGHT
            * lerp(1.0, 0.4, relief);
        let ridge01 = self.mountain.sample2(px * 0.0009, pz * 0.0009) * 0.5 + 0.5;
        let shaped = ridge01.clamp(0.0, 1.0).powf(1.35);
        let ridge = (shaped - 0.35) * MOUNTAIN_DETAIL * lerp(0.35, 1.0, relief);

        base + swell + ridge
    }

    fn carve_valleys(&self, height: f32, world_x: f32, world_z: f32) -> f32 {
        let mut out = height;
        let p = Vec2::new(world_x, world_z);
        for seg in self.valleys.iter() {
            let radius = valley_radius(seg.class);
            let (t, dist) = point_segment_dist(p, seg.a, seg.b);
            if dist >= radius {
                continue;
            }
            let relief = self
                .fields
                .sample_smooth(&self.fields.relief01, world_x, world_z)
                .clamp(0.0, 1.0);
            let detail_amp = MOUNTAIN_DETAIL * lerp(0.35, 1.0, relief) * 0.45
                + SWELL_HEIGHT * lerp(1.0, 0.4, relief) * 0.25;
            let min_gutter = TRUNK_BANK_RISE + detail_amp;
            let atlas_floor = lerp(seg.bed_a, seg.bed_b, t) + TRUNK_BANK_RISE;
            let mut floor_z = atlas_floor.min(height - min_gutter).min(height);
            floor_z = floor_z.max(self.fields.sea_surface_z);
            let ramp = smoothstep(0.0, radius, dist);
            out = out.min(lerp(floor_z, height, ramp));
        }
        out
    }

    /// Full column sample with water–rim marriage from vector hydro.
    pub fn sample_at(&self, world_x: f32, world_z: f32) -> SurfaceSample {
        let p = Vec2::new(world_x, world_z);
        let mut ground = self.base_height(world_x, world_z);
        ground = self.carve_valleys(ground, world_x, world_z);

        let sea = self.hydro.sea_surface_z;
        let coast = coast_signed(&self.hydro, self.atlas_size, p);
        // Hard reject only absurd ring failures (ocean claimed on high interior).
        let atlas_elev = self
            .fields
            .sample_bilinear(&self.fields.elevation_m, world_x, world_z);
        let ocean_here = coast < 0.0 && !(atlas_elev > 120.0 && coast > -80.0);

        let mut water_top = f32::NEG_INFINITY;

        // --- Ocean (outside coast rings) ------------------------------------
        if ocean_here {
            water_top = sea;
            let wetness = smoothstep(0.0, SHORE_BAND_M, -coast);
            let depth = (OCEAN_FLOOR_MARGIN + OCEAN_SHELF_DEPTH * wetness) * wetness;
            ground = ground.min(sea - depth);
        } else {
            let band = 1.0 - smoothstep(0.0, SHORE_BAND_M, coast.max(0.0));
            if band > 0.0 && coast >= 0.0 {
                let dryness = smoothstep(0.0, SHORE_BAND_M, coast);
                let freeboard = (INLAND_FREEBOARD + FREEBOARD_SPAN * dryness) * dryness;
                ground = lerp(ground, ground.max(sea + freeboard), band);
            }
            ground = ground.max(sea);
        }

        // --- Lakes (inside lake rings) --------------------------------------
        if let Some(lake) = lake_at(&self.hydro, self.atlas_size, p) {
            // Sheet must meet local banks — never float hundreds of metres up.
            let sheet = lake.surface_z.min(ground - 0.05).max(sea);
            water_top = water_top.max(sheet);
            let bed = sheet - LAKE_BED_DEPTH.max(WATER_CLEARANCE + 0.05);
            ground = ground.min(bed);
        }

        // --- Rivers (distance to meander polyline) --------------------------
        if let Some(hit) = nearest_river(&self.hydro, self.atlas_size, p) {
            if hit.dist < hit.half_width {
                let estuary = if ocean_here || coast <= 0.0 {
                    1.0
                } else {
                    1.0 - (coast / ESTUARY_BLEND_METRES).clamp(0.0, 1.0)
                };
                // Authored atlas Z can disagree with detailed ground; clamp to banks.
                let authored = lerp(hit.sheet_z, sea, estuary);
                let sheet = authored.min(ground - 0.05).max(sea);
                let u = (1.0 - (hit.dist / hit.half_width).clamp(0.0, 1.0)).powf(1.4);
                let bed_depth = lerp(0.15, 1.8, u);
                let bed = sheet - bed_depth.max(WATER_CLEARANCE + 0.05);
                let bank = 1.0 - (hit.dist / hit.half_width).clamp(0.0, 1.0);
                ground = lerp(ground, ground.min(bed), bank.powf(0.65));
                water_top = water_top.max(sheet);
            }
        }

        // Drainage-surface enforce.
        if water_top.is_finite() {
            let floor = water_top - WATER_CLEARANCE - 1e-3;
            if ground > floor {
                ground = floor;
            }
        }

        let sample = SurfaceSample { ground, water_top };
        if sample.is_wet() {
            let floor = water_top - WATER_CLEARANCE - 1e-3;
            SurfaceSample::wet_body(ground.min(floor), water_top)
        } else {
            SurfaceSample::dry(ground)
        }
    }
}

impl SurfaceSource for ContinentalSurface {
    fn sample(&self, x: f32, z: f32) -> SurfaceSample {
        self.sample_at(x, z)
    }
}

fn valley_radius(class: i32) -> f32 {
    match class {
        0 => 220.0,
        1 => 140.0,
        _ => 90.0,
    }
}

fn build_valley_segs(atlas: &ContinentAtlas, fields: &AtlasFields) -> Vec<ValleySeg> {
    let size = atlas.size as i32;
    let mut out = Vec::new();
    for (&cell_idx, links) in &atlas.river_links {
        let ax = cell_idx % size;
        let az = cell_idx / size;
        for link in links {
            if link.kind != Kind::River {
                continue;
            }
            let a = endpoint_metres(atlas, ax, az, link.a);
            let b = endpoint_metres(atlas, ax, az, link.b);
            let bed_a = fields.elevation_m[atlas.index_of(ax, az)];
            out.push(ValleySeg {
                a,
                b,
                class: link.feature_class,
                bed_a,
                bed_b: bed_a,
            });
        }
    }
    out
}

fn endpoint_metres(
    atlas: &ContinentAtlas,
    ax: i32,
    az: i32,
    ep: crate::atlas::types::Endpoint,
) -> Vec2 {
    match ep.kind {
        EndpointKind::Ocean | EndpointKind::Lake | EndpointKind::Node => cell_centre(ax, az),
        EndpointKind::EdgePort => {
            let (ox, oz, dir) = edge_owner(ep.ref_id);
            let ports = atlas
                .river_ports
                .get(&ep.ref_id)
                .map(|p| p.as_slice())
                .unwrap_or(&[]);
            let mut t = 0.5_f32;
            for p in ports {
                if p.id == ep.port_id {
                    t = p.t;
                    break;
                }
            }
            let (mx, mz) = match dir {
                Dir::East => (ox as f32 + 1.0, oz as f32 + t),
                Dir::South => (ox as f32 + t, oz as f32 + 1.0),
                _ => (ax as f32 + 0.5, az as f32 + 0.5),
            };
            Vec2::new(mx * CELL_METRES, mz * CELL_METRES)
        }
    }
}

fn cell_centre(ax: i32, az: i32) -> Vec2 {
    Vec2::new((ax as f32 + 0.5) * CELL_METRES, (az as f32 + 0.5) * CELL_METRES)
}

fn point_segment_dist(p: Vec2, a: Vec2, b: Vec2) -> (f32, f32) {
    let ab = b - a;
    let len2 = ab.length_squared();
    if len2 < 1e-6 {
        return (0.0, p.distance(a));
    }
    let t = ((p - a).dot(ab) / len2).clamp(0.0, 1.0);
    let q = a + ab * t;
    (t, p.distance(q))
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Find a walkable land spawn near atlas centre.
pub fn find_land_spawn(surface: &ContinentalSurface, atlas: &ContinentAtlas) -> (f32, f32) {
    let size = atlas.size as i32;
    let cx = size / 2;
    let cz = size / 2;
    for radius in 0..size {
        for dz in -radius..=radius {
            for dx in -radius..=radius {
                if dx.abs() != radius && dz.abs() != radius && radius > 0 {
                    continue;
                }
                let ax = cx + dx;
                let az = cz + dz;
                if !atlas.in_bounds(ax, az) {
                    continue;
                }
                let wx = (ax as f32 + 0.5) * CELL_METRES;
                let wz = (az as f32 + 0.5) * CELL_METRES;
                let s = surface.sample_at(wx, wz);
                if !s.is_wet() && s.ground > surface.fields().sea_surface_z + 2.0 {
                    return (wx, wz);
                }
            }
        }
    }
    (cx as f32 * CELL_METRES, cz as f32 * CELL_METRES)
}

/// Spawn pose overlooking water: `(x, z, yaw_degrees)` with yaw facing the sea/river.
pub fn find_water_view_spawn(surface: &ContinentalSurface, atlas: &ContinentAtlas) -> (f32, f32, f32) {
    let hydro = surface.hydro();
    if let Some(coast) = hydro.coasts.first() {
        let n = coast.ring.len();
        let mut c = Vec2::ZERO;
        for q in &coast.ring {
            c += *q;
        }
        c /= n as f32;
        let step = (n / 32).max(1);
        for k in 0..32 {
            let rim = coast.ring[(k * step) % n];
            let out = (rim - c).normalize_or_zero();
            if out.length_squared() < 1e-6 {
                continue;
            }
            // Walk from well inland through the warped shore; sit on last dry.
            let mut last_dry: Option<Vec2> = None;
            for i in 0..120 {
                let pt = c + out * (i as f32 * 40.0);
                let s = surface.sample_at(pt.x, pt.y);
                if s.is_wet() {
                    if let Some(d) = last_dry {
                        let yaw = out.x.atan2(out.y).to_degrees();
                        return (d.x, d.y, yaw);
                    }
                    break;
                }
                if s.ground > hydro.sea_surface_z + 1.0 {
                    last_dry = Some(pt);
                }
            }
        }
    }
    if let Some(river) = hydro.rivers.first() {
        if river.points.len() >= 2 {
            let a = river.points[0];
            let b = river.points[1];
            let dir = (b - a).normalize_or_zero();
            let perp = Vec2::new(-dir.y, dir.x);
            for sign in [1.0_f32, -1.0] {
                let p = a + perp * ((river.half_width_m + 10.0) * sign);
                let s = surface.sample_at(p.x, p.y);
                if !s.is_wet() {
                    let toward = -perp * sign;
                    let yaw = toward.x.atan2(toward.y).to_degrees();
                    return (p.x, p.y, yaw);
                }
            }
        }
    }
    let (x, z) = find_land_spawn(surface, atlas);
    (x, z, 20.0)
}
