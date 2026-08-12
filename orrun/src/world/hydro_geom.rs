//! Continuous queries against atlas [`HydroVectors`].
//!
//! Every query goes through [`HydroIndex`], which indexes the *authored* dense
//! outlines. The atlas overlay draws those same outlines, so the 2D shoreline
//! and the 3D one are the same curve by construction.

use glam::Vec2;

use super::ring_field::{RingField, SegmentField};
use crate::atlas::hydro::{HydroVectors, LakeOutline};

pub const OCEAN_SHELF_DEPTH: f32 = 70.0;
pub const SHORE_BAND_M: f32 = 130.0;

/// How far from the centerline we still evaluate river distance (valley + channel).
pub const RIVER_QUERY_PAD_M: f32 = 280.0;

/// Reported signed distance where no coast ring is anywhere near: open ocean.
pub const OPEN_OCEAN_SD: f32 = -1.0e5;
/// Range over which coast distance is resolved exactly; it saturates past this.
/// Covers the beach band and the whole continental-shelf ramp.
pub const COAST_QUERY_M: f32 = 2_400.0;
/// Range over which lake distance is resolved exactly.
pub const LAKE_QUERY_M: f32 = 400.0;

#[derive(Clone, Copy, Debug)]
pub struct RiverHit {
    pub dist: f32,
    pub half_width: f32,
    pub sheet_z: f32,
    pub class: i32,
}

/// Spatial indices over the atlas hydro outlines.
///
/// Built once per surface; a continental coast ring has tens of thousands of
/// vertices and is queried once per surface column.
#[derive(Debug)]
pub struct HydroIndex {
    size: usize,
    coasts: Vec<RingField>,
    lakes: Vec<RingField>,
    rivers: Vec<Option<SegmentField>>,
}

impl HydroIndex {
    pub fn build(hydro: &HydroVectors, size: usize) -> Self {
        Self {
            size,
            coasts: hydro
                .coasts
                .iter()
                .map(|c| RingField::build(&c.ring).expect("coast ring is a valid outline"))
                .collect(),
            lakes: hydro
                .lakes
                .iter()
                .map(|l| RingField::build(&l.ring).expect("lake ring is a valid outline"))
                .collect(),
            rivers: hydro
                .rivers
                .iter()
                .map(|r| SegmentField::build(&r.points, false))
                .collect(),
        }
    }

    /// Signed distance to coast: **positive = land interior**, negative = ocean.
    ///
    /// The query point is domain-warped so the zero contour is not the long
    /// straight atlas-cell edges that survive smoothing on a continental ring.
    pub fn coast_signed(&self, hydro: &HydroVectors, p: Vec2) -> f32 {
        let q = p + shore_domain_warp(p);
        let idx = hydro.cell_index(self.size, q.x, q.y);
        let ids = neighborhood(self.size, idx, &hydro.cell_coasts);
        if ids.is_empty() {
            // Coast stamps cover each ring's AABB (+pad). Outside every stamp is
            // open ocean — do **not** fall back to every coast ring. That made
            // offshore chunk bakes take multi-second hitches.
            return OPEN_OCEAN_SD;
        }
        let mut best = f32::NEG_INFINITY;
        for ci in ids {
            best = best.max(self.coasts[ci as usize].signed_distance(q, COAST_QUERY_M));
        }
        best
    }

    /// Governing lake near `p` with its signed ring distance (positive = inside).
    ///
    /// Unlike a plain inside test this also reports lakes the point is just
    /// outside of, so the bank can be blended up to the sheet rather than
    /// ending in a wall.
    pub fn nearest_lake<'h>(
        &self,
        hydro: &'h HydroVectors,
        p: Vec2,
    ) -> Option<(&'h LakeOutline, f32)> {
        let q = p + shore_domain_warp(p) * 0.45;
        let idx = hydro.cell_index(self.size, q.x, q.y);
        let mut best: Option<(&'h LakeOutline, f32)> = None;
        for li in neighborhood(self.size, idx, &hydro.cell_lakes) {
            let sd = self.lakes[li as usize].signed_distance(q, LAKE_QUERY_M);
            if !sd.is_finite() {
                continue;
            }
            if best.map(|(_, s)| sd > s).unwrap_or(true) {
                best = Some((&hydro.lakes[li as usize], sd));
            }
        }
        best
    }

    pub fn nearest_river(&self, hydro: &HydroVectors, p: Vec2) -> Option<RiverHit> {
        let idx = hydro.cell_index(self.size, p.x, p.y);
        let mut best: Option<RiverHit> = None;
        for ri in neighborhood(self.size, idx, &hydro.cell_rivers) {
            let river = &hydro.rivers[ri as usize];
            let Some(field) = &self.rivers[ri as usize] else {
                continue;
            };
            let max_d = (river.half_width_m * 5.0).max(RIVER_QUERY_PAD_M);
            let Some(hit) = field.nearest_within(p, max_d) else {
                continue;
            };
            if best.map(|b| hit.distance < b.dist).unwrap_or(true) {
                let za = river.surface_z[hit.segment];
                let zb = river.surface_z[hit.segment + 1];
                best = Some(RiverHit {
                    dist: hit.distance,
                    half_width: river.half_width_m,
                    sheet_z: za + (zb - za) * hit.t,
                    class: river.class,
                });
            }
        }
        best
    }
}

/// Exhaustive coast SD (all rings, no index) — reference for the index tests.
#[cfg(test)]
pub fn coast_signed_full(hydro: &HydroVectors, p: Vec2) -> f32 {
    let q = p + shore_domain_warp(p);
    let mut best = f32::NEG_INFINITY;
    for coast in &hydro.coasts {
        best = best.max(signed_distance_ring(q, &coast.ring));
    }
    best
}

fn shore_domain_warp(p: Vec2) -> Vec2 {
    crate::atlas::hydro::shore_domain_warp(p)
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

/// Exact ring signed distance by walking every edge; positive inside.
///
/// Only the tests use it — production queries go through [`HydroIndex`] — and
/// it stays exact so it can serve as their reference.
#[cfg(test)]
pub fn signed_distance_ring(p: Vec2, ring: &[Vec2]) -> f32 {
    use super::ring_field::point_segment_dist;
    if ring.len() < 3 {
        return f32::NEG_INFINITY;
    }
    let n = ring.len();
    let mut d = f32::INFINITY;
    let mut inside = false;
    for i in 0..n {
        let a = ring[i];
        let b = ring[(i + 1) % n];
        d = d.min(point_segment_dist(p, a, b).1);
        if (a.y > p.y) != (b.y > p.y) {
            let x_int = (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x;
            if p.x < x_int {
                inside = !inside;
            }
        }
    }
    if inside {
        d
    } else {
        -d
    }
}
