//! Continuous queries against atlas [`HydroVectors`].

use glam::Vec2;

use crate::atlas::hydro::{HydroVectors, LakeOutline, RiverPolyline};

pub const ESTUARY_BLEND_METRES: f32 = 220.0;
pub const OCEAN_SHELF_DEPTH: f32 = 70.0;
pub const OCEAN_FLOOR_MARGIN: f32 = 4.0;
pub const LAKE_BED_DEPTH: f32 = 12.0;
pub const SHORE_BAND_M: f32 = 130.0;

#[derive(Clone, Copy, Debug)]
pub struct RiverHit {
    pub dist: f32,
    pub half_width: f32,
    pub sheet_z: f32,
}

/// Signed distance to coast: **positive = land interior**, negative = ocean.
///
/// Query point is domain-warped so the zero contour is not the long straight
/// atlas-cell edges that survive Chaikin on a continental ring.
pub fn coast_signed(hydro: &HydroVectors, size: usize, p: Vec2) -> f32 {
    let q = p + shore_domain_warp(p);
    let idx = hydro.cell_index(size, q.x, q.y);
    let mut best = f32::NEG_INFINITY;
    let mut any = false;
    let mut ids = neighborhood(size, idx, &hydro.cell_coasts);
    if ids.is_empty() {
        ids.extend(0..hydro.coasts.len() as u32);
    }
    for ci in ids {
        let coast = &hydro.coasts[ci as usize];
        let sd = signed_distance_ring(q, &coast.ring);
        if !any || sd > best {
            best = sd;
            any = true;
        }
    }
    if any {
        best
    } else {
        -1.0
    }
}

fn shore_domain_warp(p: Vec2) -> Vec2 {
    // Metres — enough to break kilometre-scale ruler edges at walker scale.
    let a = (p.x * 0.0019).sin() * (p.y * 0.0014).cos();
    let b = (p.x * 0.0031 + 1.7).cos() * (p.y * 0.0026).sin();
    let c = (p.x * 0.006 + p.y * 0.005).sin();
    Vec2::new(a, b) * 110.0 + Vec2::new(b, -a) * 55.0 + Vec2::new(c, -c) * 35.0
}

pub fn lake_at(hydro: &HydroVectors, size: usize, p: Vec2) -> Option<&LakeOutline> {
    let q = p + shore_domain_warp(p) * 0.45;
    let idx = hydro.cell_index(size, q.x, q.y);
    let mut best: Option<(&LakeOutline, f32)> = None;
    for li in neighborhood(size, idx, &hydro.cell_lakes) {
        let lake = &hydro.lakes[li as usize];
        let sd = signed_distance_ring(q, &lake.ring);
        if sd >= 0.0 {
            let score = sd;
            if best.map(|(_, s)| score > s).unwrap_or(true) {
                best = Some((lake, score));
            }
        }
    }
    best.map(|(l, _)| l)
}

pub fn nearest_river(hydro: &HydroVectors, size: usize, p: Vec2) -> Option<RiverHit> {
    let idx = hydro.cell_index(size, p.x, p.y);
    let mut best: Option<RiverHit> = None;
    for ri in neighborhood(size, idx, &hydro.cell_rivers) {
        let river = &hydro.rivers[ri as usize];
        if let Some(hit) = river_hit(river, p) {
            if best.map(|b| hit.dist < b.dist).unwrap_or(true) {
                best = Some(hit);
            }
        }
    }
    best
}

fn river_hit(river: &RiverPolyline, p: Vec2) -> Option<RiverHit> {
    let mut best_d = f32::INFINITY;
    let mut best_sheet = 0.0;
    for (i, w) in river.points.windows(2).enumerate() {
        let a = w[0];
        let b = w[1];
        let (t, d) = point_segment_dist(p, a, b);
        if d < best_d {
            best_d = d;
            let za = river.surface_z[i];
            let zb = river.surface_z[i + 1];
            best_sheet = za + (zb - za) * t;
        }
    }
    if !best_d.is_finite() {
        return None;
    }
    Some(RiverHit {
        dist: best_d,
        half_width: river.half_width_m,
        sheet_z: best_sheet,
    })
}

fn neighborhood(size: usize, idx: usize, table: &[Vec<u32>]) -> Vec<u32> {
    let ax = (idx % size) as i32;
    let az = (idx / size) as i32;
    let s = size as i32;
    let mut ids = Vec::new();
    for dz in -1..=1 {
        for dx in -1..=1 {
            let x = ax + dx;
            let z = az + dz;
            if x < 0 || z < 0 || x >= s || z >= s {
                continue;
            }
            let i = z as usize * size + x as usize;
            for &id in &table[i] {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
    }
    ids
}

/// Positive inside (to the left of directed edges / CCW winding).
pub fn signed_distance_ring(p: Vec2, ring: &[Vec2]) -> f32 {
    if ring.len() < 3 {
        return f32::NEG_INFINITY;
    }
    let inside = point_in_ring(p, ring);
    let mut d = f32::INFINITY;
    let n = ring.len();
    for i in 0..n {
        let a = ring[i];
        let b = ring[(i + 1) % n];
        let (_, dist) = point_segment_dist(p, a, b);
        d = d.min(dist);
    }
    if inside {
        d
    } else {
        -d
    }
}

fn point_in_ring(p: Vec2, ring: &[Vec2]) -> bool {
    // Ray cast +X
    let mut inside = false;
    let n = ring.len();
    for i in 0..n {
        let a = ring[i];
        let b = ring[(i + 1) % n];
        let cond = (a.y > p.y) != (b.y > p.y);
        if cond {
            let x_int = (b.x - a.x) * (p.y - a.y) / (b.y - a.y + 1e-12) + a.x;
            if p.x < x_int {
                inside = !inside;
            }
        }
    }
    inside
}

pub fn point_segment_dist(p: Vec2, a: Vec2, b: Vec2) -> (f32, f32) {
    let ab = b - a;
    let len2 = ab.length_squared();
    if len2 < 1e-6 {
        return (0.0, p.distance(a));
    }
    let t = ((p - a).dot(ab) / len2).clamp(0.0, 1.0);
    let q = a + ab * t;
    (t, p.distance(q))
}
