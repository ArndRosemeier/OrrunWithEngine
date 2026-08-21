//! Deterministic vector hydrology baked from the atlas climate/graph.
//!
//! Step 1 of landscape: compact curves (river ribbons, lake rings, coast rings).
//! Step 2 (world sampler) turns those into continuous `ground` / `water_top`.

use engine::proc::Noise;
use glam::Vec2;
use rustc_hash::FxHashMap;

use super::biomes::{self, Biome};
use super::features::{edge_owner, Dir, EndpointKind, Kind};
use super::pack;
use super::types::{Endpoint, Link};
use super::{layer_seed, ContinentAtlas, CELL_METRES};

const COAST_RESAMPLE_M: f32 = 40.0;
const COAST_SMOOTH_ITERS: usize = 14;
const COAST_SMOOTH_HALF_WIN: usize = 5;
const LAKE_RESAMPLE_M: f32 = 45.0;
const LAKE_SMOOTH_ITERS: usize = 9;
const LAKE_SMOOTH_HALF_WIN: usize = 3;
const RIVER_MEANDER_FRAC: f32 = 0.18;
const RIVER_CURVE_SPACING_M: f32 = 36.0;
/// How far a mouth may walk past the last land cell to meet the meandered shore.
const MOUTH_MAX_M: f32 = 1_500.0;
const MOUTH_STEP_M: f32 = 24.0;

/// River-like shore meander: amplitudes (m) and wavelengths along the perimeter (m).
#[derive(Clone, Copy)]
struct ShoreMeander {
    coarse_m: f32,
    mid_m: f32,
    fine_m: f32,
    micro_m: f32,
    coarse_len_m: f32,
    mid_len_m: f32,
    fine_len_m: f32,
    micro_len_m: f32,
    post_smooth: usize,
    max_turn_deg: f32,
}

const COAST_MEANDER: ShoreMeander = ShoreMeander {
    // Coarse = bay/headland swing; mid/fine/micro = river-like grit inside a km cell.
    coarse_m: 380.0,
    mid_m: 200.0,
    fine_m: 100.0,
    micro_m: 40.0,
    coarse_len_m: 2100.0,
    mid_len_m: 780.0,
    fine_len_m: 340.0,
    micro_len_m: 105.0,
    post_smooth: 1,
    max_turn_deg: 58.0,
};

const LAKE_MEANDER: ShoreMeander = ShoreMeander {
    coarse_m: 120.0,
    mid_m: 85.0,
    fine_m: 45.0,
    micro_m: 22.0,
    coarse_len_m: 1200.0,
    mid_len_m: 520.0,
    fine_len_m: 240.0,
    micro_len_m: 90.0,
    post_smooth: 1,
    max_turn_deg: 52.0,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HydroSink {
    Ocean,
    Lake { lake_id: i32 },
}

#[derive(Debug, Clone)]
pub struct RiverPolyline {
    pub id: i32,
    pub class: i32,
    pub points: Vec<Vec2>,
    pub surface_z: Vec<f32>,
    pub half_width_m: f32,
    pub sink: HydroSink,
    /// Last point is an ocean or lake endpoint, not a confluence with another reach.
    pub(crate) at_sink: bool,
}

#[derive(Debug, Clone)]
pub struct LakeOutline {
    pub id: i32,
    pub surface_z: f32,
    /// The one authored outline: drawn by the atlas and queried by the surface.
    pub ring: Vec<Vec2>,
}

#[derive(Debug, Clone)]
pub struct CoastRing {
    pub landmass_id: i32,
    /// The one authored outline: drawn by the atlas and queried by the surface.
    pub ring: Vec<Vec2>,
}

/// Atlas-authored vector hydrology (schema companion to the climate grid).
#[derive(Debug, Clone)]
pub struct HydroVectors {
    pub sea_surface_z: f32,
    pub rivers: Vec<RiverPolyline>,
    pub lakes: Vec<LakeOutline>,
    pub coasts: Vec<CoastRing>,
    /// Per atlas cell: river indices that overlap the cell (expanded by width).
    pub cell_rivers: Vec<Vec<u32>>,
    pub cell_lakes: Vec<Vec<u32>>,
    pub cell_coasts: Vec<Vec<u32>>,
    /// Packed atlas ocean bit per cell. Coast rings can miss inland cells;
    /// this is the fallback so land is never treated as open ocean.
    pub atlas_ocean: Vec<u8>,
}

impl HydroVectors {
    pub fn bake(atlas: &ContinentAtlas) -> Self {
        let sea = atlas.sea_surface_z as f32;
        let seed = layer_seed(atlas.world_seed, "hydro_vectors");
        let mut rivers = build_rivers(atlas, sea, seed);
        let lakes = build_lakes(atlas, seed);
        let coasts = build_coasts(atlas, seed);
        // Polylines stop at the last land cell; the shoreline is a meandered
        // ring. Walk each mouth out until it is actually in the water it drains
        // to, or the overlay's "ocean" endpoint is a cul-de-sac on the beach.
        reach_sinks(&mut rivers, &lakes, &coasts, sea);
        let (cell_rivers, cell_lakes, cell_coasts) =
            build_cell_index(atlas.size, &rivers, &lakes, &coasts);
        let atlas_ocean: Vec<u8> = atlas
            .cells
            .iter()
            .map(|&cell| u8::from(pack::biome(cell) == Biome::Ocean))
            .collect();
        let hydro = Self {
            sea_surface_z: sea,
            rivers,
            lakes,
            coasts,
            cell_rivers,
            cell_lakes,
            cell_coasts,
            atlas_ocean,
        };
        hydro.validate_or_panic();
        hydro
    }

    fn validate_or_panic(&self) {
        for (i, r) in self.rivers.iter().enumerate() {
            assert!(
                r.points.len() >= 2 && r.points.len() == r.surface_z.len(),
                "river {i} bad polyline"
            );
            assert!(r.half_width_m > 0.0, "river {i} zero width");
            match r.sink {
                HydroSink::Ocean | HydroSink::Lake { .. } => {}
            }
        }
        for (i, l) in self.lakes.iter().enumerate() {
            assert!(l.ring.len() >= 4, "lake {i} ring too short");
        }
        for (i, c) in self.coasts.iter().enumerate() {
            assert!(c.ring.len() >= 4, "coast {i} ring too short");
        }
        assert_eq!(
            self.atlas_ocean.len(),
            self.cell_coasts.len(),
            "atlas ocean mask must cover every hydro cell"
        );
    }

    #[inline]
    pub fn is_atlas_ocean(&self, idx: usize) -> bool {
        self.atlas_ocean[idx] != 0
    }

    pub fn grid_size(&self) -> usize {
        let n = self.atlas_ocean.len();
        let size = (n as f64).sqrt() as usize;
        assert_eq!(size * size, n, "hydro atlas mask is not a square grid");
        size
    }

    #[inline]
    pub fn cell_index(&self, size: usize, world_x: f32, world_z: f32) -> usize {
        let ax = (world_x / CELL_METRES).floor() as i32;
        let az = (world_z / CELL_METRES).floor() as i32;
        let s = size as i32;
        let x = ax.clamp(0, s - 1) as usize;
        let z = az.clamp(0, s - 1) as usize;
        z * size + x
    }

    /// Land if a coast ring contains the point, otherwise if the atlas cell is
    /// not ocean. Rings win near a traced shoreline; the atlas bit covers
    /// inland cells no fragment ring was stamped onto.
    pub(crate) fn contains_land(&self, size: usize, p: Vec2) -> bool {
        let q = p + shore_domain_warp(p);
        let idx = self.cell_index(size, q.x, q.y);
        if self.cell_coasts[idx]
            .iter()
            .any(|&coast_id| point_in_ring(q, &self.coasts[coast_id as usize].ring))
        {
            return true;
        }
        !self.is_atlas_ocean(idx)
    }
}

fn half_width_for_class(class: i32) -> f32 {
    match class.clamp(1, 4) {
        1 => 10.0,
        2 => 16.0,
        3 => 24.0,
        _ => 34.0,
    }
}

fn build_rivers(atlas: &ContinentAtlas, sea: f32, seed: u32) -> Vec<RiverPolyline> {
    let size = atlas.size as i32;

    struct Seg {
        a_edge: Option<(i32, i32)>,
        b_edge: Option<(i32, i32)>,
        pts: Vec<Vec2>,
        zs: Vec<f32>,
        class: i32,
        sink: HydroSink,
    }

    let mut segs: Vec<Seg> = Vec::new();
    for (&cell_idx, links) in &atlas.river_links {
        let ax = cell_idx % size;
        let az = cell_idx / size;
        for link in links {
            if link.kind != Kind::River {
                continue;
            }
            let Some(sink) = link_sink(link) else {
                continue;
            };
            let a = endpoint_metres(atlas, ax, az, link.a);
            let b = endpoint_metres(atlas, ax, az, link.b);
            let za = sheet_for_endpoint(atlas, sea, link.a, ax, az);
            let zb = sheet_for_endpoint(atlas, sea, link.b, ax, az);
            let (pts, zs) = meander_controls(a, b, za, zb, seed, cell_idx, link.feature_id);
            segs.push(Seg {
                a_edge: edge_port_key(link.a),
                b_edge: edge_port_key(link.b),
                pts,
                zs,
                class: link.feature_class,
                sink,
            });
        }
    }

    let mut by_start: FxHashMap<(i32, i32), Vec<usize>> = FxHashMap::default();
    let mut by_end: FxHashMap<(i32, i32), Vec<usize>> = FxHashMap::default();
    for (i, seg) in segs.iter().enumerate() {
        if let Some(k) = seg.a_edge {
            by_start.entry(k).or_default().push(i);
        }
        if let Some(k) = seg.b_edge {
            by_end.entry(k).or_default().push(i);
        }
    }

    let unique_succ = |i: usize| -> Option<usize> {
        let k = segs[i].b_edge?;
        let starts = by_start.get(&k)?;
        let ends = by_end.get(&k)?;
        if starts.len() == 1 && ends.len() == 1 {
            Some(starts[0])
        } else {
            None
        }
    };
    let unique_pred = |i: usize| -> Option<usize> {
        let k = segs[i].a_edge?;
        let starts = by_start.get(&k)?;
        let ends = by_end.get(&k)?;
        if starts.len() == 1 && ends.len() == 1 {
            Some(ends[0])
        } else {
            None
        }
    };

    let mut used = vec![false; segs.len()];
    let mut out = Vec::new();
    let mut next_id = 1_i32;
    for start in 0..segs.len() {
        if used[start] {
            continue;
        }
        let mut cur = start;
        while let Some(pred) = unique_pred(cur) {
            if used[pred] {
                break;
            }
            cur = pred;
        }
        let mut chain = Vec::new();
        loop {
            if used[cur] {
                break;
            }
            used[cur] = true;
            chain.push(cur);
            match unique_succ(cur) {
                Some(n) if !used[n] => cur = n,
                _ => break,
            }
        }
        if chain.is_empty() {
            continue;
        }

        let mut points = Vec::new();
        let mut surface_z = Vec::new();
        let mut class = 1_i32;
        let mut sink = segs[chain[0]].sink;
        for (ci, &si) in chain.iter().enumerate() {
            let seg = &segs[si];
            class = class.max(seg.class);
            sink = seg.sink;
            let start_j = if ci == 0 { 0 } else { 1 };
            for j in start_j..seg.pts.len() {
                points.push(seg.pts[j]);
                surface_z.push(seg.zs[j]);
            }
        }
        let (points, surface_z) = fluent_open_river(&points, &surface_z, RIVER_CURVE_SPACING_M);
        if points.len() < 2 {
            continue;
        }
        out.push(RiverPolyline {
            id: next_id,
            class,
            points,
            surface_z,
            half_width_m: half_width_for_class(class),
            sink,
            at_sink: segs[*chain.last().expect("a stitched reach has a last cell")]
                .b_edge
                .is_none(),
        });
        next_id += 1;
    }
    out
}

fn edge_port_key(ep: Endpoint) -> Option<(i32, i32)> {
    match ep.kind {
        EndpointKind::EdgePort => Some((ep.ref_id, ep.port_id)),
        _ => None,
    }
}

/// Smooth a stitched river control polyline into a continuous open curve.
fn fluent_open_river(points: &[Vec2], zs: &[f32], spacing_m: f32) -> (Vec<Vec2>, Vec<f32>) {
    assert_eq!(points.len(), zs.len());
    if points.len() < 2 {
        return (points.to_vec(), zs.to_vec());
    }
    let (smooth_p, smooth_z) = chaikin_open(points, zs, 2);
    let curved = catmull_rom_open(&smooth_p, spacing_m.max(12.0));
    let zs_out = sample_z_along(&smooth_p, &smooth_z, &curved);
    (curved, zs_out)
}

fn chaikin_open(points: &[Vec2], zs: &[f32], iters: usize) -> (Vec<Vec2>, Vec<f32>) {
    if points.len() < 3 {
        return (points.to_vec(), zs.to_vec());
    }
    let mut cur_p = points.to_vec();
    let mut cur_z = zs.to_vec();
    for _ in 0..iters {
        let n = cur_p.len();
        let mut next_p = Vec::with_capacity(n * 2);
        let mut next_z = Vec::with_capacity(n * 2);
        next_p.push(cur_p[0]);
        next_z.push(cur_z[0]);
        for i in 0..n - 1 {
            let a = cur_p[i];
            let b = cur_p[i + 1];
            let za = cur_z[i];
            let zb = cur_z[i + 1];
            next_p.push(a * 0.75 + b * 0.25);
            next_z.push(za * 0.75 + zb * 0.25);
            next_p.push(a * 0.25 + b * 0.75);
            next_z.push(za * 0.25 + zb * 0.75);
        }
        next_p.push(cur_p[n - 1]);
        next_z.push(cur_z[n - 1]);
        cur_p = next_p;
        cur_z = next_z;
    }
    (cur_p, cur_z)
}

fn catmull_rom_open(controls: &[Vec2], spacing_m: f32) -> Vec<Vec2> {
    let n = controls.len();
    if n < 2 {
        return controls.to_vec();
    }
    if n == 2 {
        let mut out = vec![controls[0]];
        let seg = controls[1] - controls[0];
        let len = seg.length();
        if len > spacing_m {
            let steps = (len / spacing_m).ceil() as usize;
            for s in 1..steps {
                out.push(controls[0] + seg * (s as f32 / steps as f32));
            }
        }
        out.push(controls[1]);
        return out;
    }
    let mut out = Vec::new();
    for i in 0..n - 1 {
        let p0 = if i == 0 {
            controls[0] * 2.0 - controls[1]
        } else {
            controls[i - 1]
        };
        let p1 = controls[i];
        let p2 = controls[i + 1];
        let p3 = if i + 2 < n {
            controls[i + 2]
        } else {
            controls[n - 1] * 2.0 - controls[n - 2]
        };
        let seg_len = p1.distance(p2).max(1.0);
        let steps = ((seg_len / spacing_m).ceil() as usize).clamp(2, 48);
        let s0 = if i == 0 { 0 } else { 1 };
        for s in s0..steps {
            let t = s as f32 / steps as f32;
            out.push(catmull_rom_point(p0, p1, p2, p3, t));
        }
    }
    out.push(controls[n - 1]);
    out
}

fn sample_z_along(controls: &[Vec2], zs: &[f32], samples: &[Vec2]) -> Vec<f32> {
    assert_eq!(controls.len(), zs.len());
    if controls.len() < 2 {
        return vec![zs.first().copied().unwrap_or(0.0); samples.len()];
    }
    let mut cum = vec![0.0_f32; controls.len()];
    for i in 1..controls.len() {
        cum[i] = cum[i - 1] + controls[i].distance(controls[i - 1]);
    }
    samples
        .iter()
        .map(|p| {
            // Nearest projection onto control polyline (arc parameter).
            let mut best_d = f32::MAX;
            let mut best_s = 0.0_f32;
            for i in 0..controls.len() - 1 {
                let a = controls[i];
                let b = controls[i + 1];
                let ab = b - a;
                let len_sq = ab.length_squared().max(1e-8);
                let t = ((p - a).dot(ab) / len_sq).clamp(0.0, 1.0);
                let q = a + ab * t;
                let d = p.distance_squared(q);
                if d < best_d {
                    best_d = d;
                    best_s = cum[i] + (cum[i + 1] - cum[i]) * t;
                }
            }
            // Piecewise-linear z by arc length.
            let mut i = 0usize;
            while i + 1 < cum.len() && cum[i + 1] < best_s {
                i += 1;
            }
            if i + 1 >= cum.len() {
                return *zs.last().expect("non-empty");
            }
            let span = (cum[i + 1] - cum[i]).max(1e-6);
            let u = ((best_s - cum[i]) / span).clamp(0.0, 1.0);
            zs[i] + (zs[i + 1] - zs[i]) * u
        })
        .collect()
}

fn link_sink(link: &Link) -> Option<HydroSink> {
    for ep in [link.a, link.b] {
        match ep.kind {
            EndpointKind::Ocean => return Some(HydroSink::Ocean),
            EndpointKind::Lake => {
                return Some(HydroSink::Lake { lake_id: ep.ref_id });
            }
            _ => {}
        }
    }
    // Mid-reach links drain via the atlas tree; treat as ocean-bound.
    Some(HydroSink::Ocean)
}

fn sheet_for_endpoint(atlas: &ContinentAtlas, sea: f32, ep: Endpoint, ax: i32, az: i32) -> f32 {
    match ep.kind {
        EndpointKind::Ocean => sea,
        EndpointKind::Lake => atlas
            .lakes
            .iter()
            .find(|l| l.id == ep.ref_id)
            .map(|l| l.surface_z as f32)
            .unwrap_or(sea),
        EndpointKind::EdgePort => {
            if let Some(ports) = atlas.river_ports.get(&ep.ref_id) {
                for p in ports {
                    if p.id == ep.port_id {
                        return p.surface_z as f32;
                    }
                }
            }
            pack::elevation_to_metres(pack::elevation(atlas.cell_at(ax, az))) as f32
        }
        EndpointKind::Node => {
            pack::elevation_to_metres(pack::elevation(atlas.cell_at(ax, az))) as f32
        }
    }
}

/// Several meander controls between cell endpoints (pinned) for a natural bend.
fn meander_controls(
    a: Vec2,
    b: Vec2,
    za: f32,
    zb: f32,
    seed: u32,
    cell_idx: i32,
    feature_id: i32,
) -> (Vec<Vec2>, Vec<f32>) {
    let ab = b - a;
    let len = ab.length();
    if len < 8.0 {
        return (vec![a, b], vec![za, zb]);
    }
    let dir = ab / len;
    let perp = Vec2::new(-dir.y, dir.x);
    let h0 = hash_u32(seed, cell_idx as u32, feature_id as u32);
    let mut pts = vec![a];
    let mut zs = vec![za];
    // Two interior bends — enough for Catmull to look like a river, not a chevron.
    for k in 1..=2 {
        let t = k as f32 / 3.0;
        let h = hash_u32(h0, k as u32, feature_id as u32);
        let side = if ((h >> k) & 1) == 0 { 1.0 } else { -1.0 };
        let amp = len
            * RIVER_MEANDER_FRAC
            * (0.45 + 0.55 * ((h >> 3) as f32 / u32::MAX as f32))
            * (std::f32::consts::PI * t).sin();
        let phase = ((h >> 8) as f32 / u32::MAX as f32 - 0.5) * 0.15;
        let tt = (t + phase).clamp(0.12, 0.88);
        pts.push(a + dir * (len * tt) + perp * (side * amp));
        zs.push(za + (zb - za) * tt);
    }
    pts.push(b);
    zs.push(zb);
    (pts, zs)
}

fn build_lakes(atlas: &ContinentAtlas, seed: u32) -> Vec<LakeOutline> {
    let size = atlas.size;
    let mut out = Vec::new();
    for lake in &atlas.lakes {
        let mut mask = vec![false; size * size];
        for &c in &lake.cells {
            if (c as usize) < mask.len() {
                mask[c as usize] = true;
            }
        }
        let rings = extract_rings(size, &mask);
        let Some(raw) = rings.into_iter().max_by_key(|r| r.len()) else {
            continue;
        };
        let mut ring = fluent_ring(
            &raw,
            LAKE_RESAMPLE_M,
            LAKE_SMOOTH_ITERS,
            LAKE_SMOOTH_HALF_WIN,
        );
        meander_ring(
            &mut ring,
            seed ^ (lake.id as u32).wrapping_mul(0x9E37),
            LAKE_MEANDER,
        );
        ensure_ring_contains_centroid(&mut ring);
        if ring.len() >= 4 {
            out.push(LakeOutline {
                id: lake.id,
                surface_z: lake.surface_z as f32,
                ring,
            });
        }
    }
    out
}

fn build_coasts(atlas: &ContinentAtlas, seed: u32) -> Vec<CoastRing> {
    let size = atlas.size;
    // Group landmass cells (exclude ocean + lake biomes).
    let mut by_mass: FxHashMap<i32, Vec<bool>> = FxHashMap::default();
    for az in 0..size {
        for ax in 0..size {
            let idx = az * size + ax;
            let lm = atlas.landmass_id[idx];
            if lm < 0 {
                continue;
            }
            let biome = pack::biome(atlas.cells[idx]);
            if !biomes::is_land(biome) {
                continue;
            }
            let entry = by_mass
                .entry(lm)
                .or_insert_with(|| vec![false; size * size]);
            entry[idx] = true;
        }
    }
    let mut out = Vec::new();
    for (landmass_id, mask) in by_mass {
        let rings = extract_rings(size, &mask);
        let Some(raw) = rings.into_iter().max_by_key(|r| r.len()) else {
            continue;
        };
        let mut ring = fluent_ring(
            &raw,
            COAST_RESAMPLE_M,
            COAST_SMOOTH_ITERS,
            COAST_SMOOTH_HALF_WIN,
        );
        meander_ring(
            &mut ring,
            seed ^ (landmass_id as u32).wrapping_mul(0xC0A57),
            COAST_MEANDER,
        );
        // Landmass centroid must sit inside the ring (positive winding).
        ensure_ring_contains_centroid(&mut ring);
        if ring.len() >= 4 {
            out.push(CoastRing { landmass_id, ring });
        }
    }
    out
}

/// Dissolve kilometre stairs: midpoints → smooth → simplify → Catmull-Rom → Chaikin.
fn fluent_ring(raw: &[Vec2], spacing_m: f32, smooth_iters: usize, half_win: usize) -> Vec<Vec2> {
    let mid = edge_midpoints(raw);
    let mut ring = resample_closed(&mid, spacing_m);
    for _ in 0..smooth_iters {
        ring = moving_average_closed(&ring, half_win);
    }
    // Keep gentle control points, then rebuild a C1 curve so long stairs become arcs.
    let controls = simplify_closed(&ring, spacing_m * 3.5);
    let curved = catmull_rom_closed(&controls, spacing_m);
    chaikin(&curved, 1)
}

fn simplify_closed(ring: &[Vec2], min_spacing_m: f32) -> Vec<Vec2> {
    if ring.len() < 4 {
        return ring.to_vec();
    }
    let mut out = vec![ring[0]];
    for p in ring.iter().skip(1) {
        if out.last().expect("non-empty").distance(*p) >= min_spacing_m {
            out.push(*p);
        }
    }
    if out.len() >= 2 && out.last().unwrap().distance(out[0]) < min_spacing_m * 0.5 {
        out.pop();
    }
    if out.len() < 4 {
        ring.to_vec()
    } else {
        out
    }
}

fn catmull_rom_closed(controls: &[Vec2], spacing_m: f32) -> Vec<Vec2> {
    let n = controls.len();
    if n < 4 {
        return controls.to_vec();
    }
    let mut out = Vec::new();
    for i in 0..n {
        let p0 = controls[(i + n - 1) % n];
        let p1 = controls[i];
        let p2 = controls[(i + 1) % n];
        let p3 = controls[(i + 2) % n];
        let seg_len = p1.distance(p2).max(1.0);
        let steps = ((seg_len / spacing_m).ceil() as usize).clamp(2, 64);
        for s in 0..steps {
            let t = s as f32 / steps as f32;
            out.push(catmull_rom_point(p0, p1, p2, p3, t));
        }
    }
    out
}

fn catmull_rom_point(p0: Vec2, p1: Vec2, p2: Vec2, p3: Vec2, t: f32) -> Vec2 {
    let t2 = t * t;
    let t3 = t2 * t;
    0.5 * ((2.0 * p1)
        + (-p0 + p2) * t
        + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t2
        + (-p0 + 3.0 * p1 - 3.0 * p2 + p3) * t3)
}

fn edge_midpoints(ring: &[Vec2]) -> Vec<Vec2> {
    let n = ring.len();
    if n < 2 {
        return ring.to_vec();
    }
    (0..n)
        .map(|i| (ring[i] + ring[(i + 1) % n]) * 0.5)
        .collect()
}

/// Extract closed boundary rings (metres) for a boolean cell mask (interior=true).
fn extract_rings(size: usize, mask: &[bool]) -> Vec<Vec<Vec2>> {
    let mut edges: FxHashMap<(i32, i32), (i32, i32)> = FxHashMap::default();
    let s = size as i32;
    for az in 0..s {
        for ax in 0..s {
            let idx = (az * s + ax) as usize;
            if !mask[idx] {
                continue;
            }
            // Missing neighbour → directed edge, interior on the left.
            if !interior(mask, size, ax + 1, az) {
                insert_edge(&mut edges, ax + 1, az, ax + 1, az + 1);
            }
            if !interior(mask, size, ax, az + 1) {
                insert_edge(&mut edges, ax + 1, az + 1, ax, az + 1);
            }
            if !interior(mask, size, ax - 1, az) {
                insert_edge(&mut edges, ax, az + 1, ax, az);
            }
            if !interior(mask, size, ax, az - 1) {
                insert_edge(&mut edges, ax, az, ax + 1, az);
            }
        }
    }

    let mut rings = Vec::new();
    while let Some((&start, _)) = edges.iter().next() {
        let mut ring_cells = Vec::new();
        let mut cur = start;
        loop {
            ring_cells.push(cur);
            let Some(next) = edges.remove(&cur) else {
                break;
            };
            cur = next;
            if cur == start {
                break;
            }
            if ring_cells.len() > size * size * 4 {
                break;
            }
        }
        if ring_cells.len() >= 4 {
            let metres: Vec<Vec2> = ring_cells
                .into_iter()
                .map(|(x, z)| Vec2::new(x as f32 * CELL_METRES, z as f32 * CELL_METRES))
                .collect();
            rings.push(metres);
        }
    }
    rings
}

fn interior(mask: &[bool], size: usize, ax: i32, az: i32) -> bool {
    if ax < 0 || az < 0 || ax >= size as i32 || az >= size as i32 {
        return false;
    }
    mask[az as usize * size + ax as usize]
}

fn insert_edge(edges: &mut FxHashMap<(i32, i32), (i32, i32)>, x0: i32, z0: i32, x1: i32, z1: i32) {
    // Collinear merges happen naturally when chaining; overwrite is rare on grid.
    edges.insert((x0, z0), (x1, z1));
}

fn chaikin(points: &[Vec2], iters: usize) -> Vec<Vec2> {
    if points.len() < 3 {
        return points.to_vec();
    }
    let mut cur = points.to_vec();
    // Treat as closed.
    for _ in 0..iters {
        let n = cur.len();
        let mut next = Vec::with_capacity(n * 2);
        for i in 0..n {
            let a = cur[i];
            let b = cur[(i + 1) % n];
            next.push(a * 0.75 + b * 0.25);
            next.push(a * 0.25 + b * 0.75);
        }
        cur = next;
    }
    cur
}

fn resample_closed(points: &[Vec2], spacing_m: f32) -> Vec<Vec2> {
    if points.len() < 3 || spacing_m < 1.0 {
        return points.to_vec();
    }
    let n = points.len();
    let mut lengths = Vec::with_capacity(n);
    let mut total = 0.0_f32;
    for i in 0..n {
        let d = points[i].distance(points[(i + 1) % n]);
        lengths.push(d);
        total += d;
    }
    if total < spacing_m * 4.0 {
        return points.to_vec();
    }
    let count = ((total / spacing_m).ceil() as usize).max(8);
    let step = total / count as f32;
    let mut out = Vec::with_capacity(count);
    let mut i = 0usize;
    let mut seg_t = 0.0_f32;
    let mut dist = 0.0_f32;
    out.push(points[0]);
    for _ in 1..count {
        dist += step;
        while i < n && seg_t + lengths[i] < dist {
            seg_t += lengths[i];
            i += 1;
        }
        if i >= n {
            break;
        }
        let local = (dist - seg_t) / lengths[i].max(1e-3);
        let a = points[i];
        let b = points[(i + 1) % n];
        out.push(a + (b - a) * local.clamp(0.0, 1.0));
    }
    out
}

fn moving_average_closed(points: &[Vec2], half_win: usize) -> Vec<Vec2> {
    let n = points.len();
    if n < 3 || half_win == 0 {
        return points.to_vec();
    }
    let mut out = Vec::with_capacity(n);
    let win = half_win as i32;
    for i in 0..n {
        let mut sum = Vec2::ZERO;
        let mut wsum = 0.0_f32;
        for k in -win..=win {
            let j = (i as i32 + k).rem_euclid(n as i32) as usize;
            // Triangular weights favour the centre.
            let w = (half_win + 1 - k.unsigned_abs() as usize) as f32;
            sum += points[j] * w;
            wsum += w;
        }
        out.push(sum / wsum);
    }
    out
}

/// Displace a closed shore along local normals with multi-octave arc-length noise
/// (same spirit as river corridors: coarse bays, fine coves, micro grit).
fn meander_ring(ring: &mut [Vec2], seed: u32, style: ShoreMeander) {
    if ring.len() < 3 {
        return;
    }
    let n = ring.len();
    let mut centroid = Vec2::ZERO;
    for p in ring.iter() {
        centroid += *p;
    }
    centroid /= n as f32;

    let mut along = vec![0.0_f32; n];
    for i in 1..n {
        along[i] = along[i - 1] + ring[i - 1].distance(ring[i]);
    }
    let noise = Noise::new(seed);
    let grit = Noise::new(seed ^ 0x9E37_79B9);
    let phase_a = hash_u32(seed, 1, 0xC0A5) as f32 / u32::MAX as f32 * std::f32::consts::TAU;
    let phase_b = hash_u32(seed, 2, 0xBEEF) as f32 / u32::MAX as f32 * std::f32::consts::TAU;

    let mut normals = vec![Vec2::ZERO; n];
    let mut tip = vec![1.0_f32; n];
    for i in 0..n {
        let prev = ring[(i + n - 1) % n];
        let next = ring[(i + 1) % n];
        let tang = (next - prev).normalize_or_zero();
        let mut nrm = Vec2::new(-tang.y, tang.x);
        if nrm.dot(ring[i] - centroid) < 0.0 {
            nrm = -nrm;
        }
        normals[i] = nrm;
        let u = (ring[i] - prev).normalize_or_zero();
        let v = (next - ring[i]).normalize_or_zero();
        let ang = u.dot(v).clamp(-1.0, 1.0).acos();
        // Soften only true hairpins; straight and gently bent shores get full meander.
        tip[i] = (1.0 - ((ang - 0.85).max(0.0) / 1.0)).clamp(0.35, 1.0);
    }

    for i in 0..n {
        let s = along[i];
        let u_c = s / style.coarse_len_m.max(1.0);
        let u_mid = s / style.mid_len_m.max(1.0);
        let u_f = s / style.fine_len_m.max(1.0);
        let u_m = s / style.micro_len_m.max(1.0);
        // Explicit bay/headland swings (guaranteed amplitude) + noisy detail.
        let bay = (s / style.coarse_len_m.max(1.0) * std::f32::consts::TAU + phase_a).sin() * 0.62
            + (s / (style.coarse_len_m.max(1.0) * 1.7) * std::f32::consts::TAU + phase_b).sin()
                * 0.38;
        let wx = noise.sample2(ring[i].x * 0.0004, ring[i].y * 0.0004) * 0.45;
        let coarse_n = noise.fbm2(u_c + wx, 0.17, 3, 2.05, 0.52);
        let mid = noise.fbm2(u_mid * 1.05, 0.91 + wx * 0.5, 2, 2.0, 0.55);
        let fine = noise.fbm2(u_f * 1.15, 1.9 + wx, 2, 2.0, 0.55);
        let micro = grit.sample2(u_m * 1.4, 3.4) * 0.65 + grit.sample2(u_m * 2.7, 5.1) * 0.35;
        let lateral = (bay * 0.7 + coarse_n * 0.3) * style.coarse_m
            + mid * style.mid_m
            + fine * style.fine_m
            + micro * style.micro_m;
        let drift =
            mid * style.mid_m * 0.18 + fine * style.fine_m * 0.25 + micro * style.micro_m * 0.2;
        let nrm = normals[i];
        let tang = Vec2::new(-nrm.y, nrm.x);
        let w = tip[i];
        ring[i] += nrm * (lateral * w) + tang * (drift * w);
    }

    for _ in 0..style.post_smooth {
        let softened = moving_average_closed(ring, 1);
        ring.copy_from_slice(&softened);
    }
    despike_closed(ring, style.max_turn_deg);
}

/// Replace vertices whose turning angle exceeds `max_turn_deg` with a midpoint.
fn despike_closed(ring: &mut [Vec2], max_turn_deg: f32) {
    let n = ring.len();
    if n < 4 {
        return;
    }
    let max_turn = max_turn_deg.to_radians();
    for _ in 0..12 {
        let mut changed = false;
        let snapshot: Vec<Vec2> = ring.to_vec();
        for i in 0..n {
            let a = snapshot[(i + n - 1) % n];
            let b = snapshot[i];
            let c = snapshot[(i + 1) % n];
            let u = (b - a).normalize_or_zero();
            let v = (c - b).normalize_or_zero();
            let ang = u.dot(v).clamp(-1.0, 1.0).acos();
            if ang > max_turn {
                ring[i] = (a + c) * 0.5;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
}

fn ensure_ring_contains_centroid(ring: &mut [Vec2]) {
    if ring.len() < 3 {
        return;
    }
    let mut c = Vec2::ZERO;
    for p in ring.iter() {
        c += *p;
    }
    c /= ring.len() as f32;
    if !point_in_ring(c, ring) {
        ring.reverse();
    }
}

fn point_in_ring(p: Vec2, ring: &[Vec2]) -> bool {
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

type CellIdLists = Vec<Vec<u32>>;

fn build_cell_index(
    size: usize,
    rivers: &[RiverPolyline],
    lakes: &[LakeOutline],
    coasts: &[CoastRing],
) -> (CellIdLists, CellIdLists, CellIdLists) {
    let mut cell_rivers = vec![Vec::new(); size * size];
    let mut cell_lakes = vec![Vec::new(); size * size];
    let mut cell_coasts = vec![Vec::new(); size * size];

    for (ri, river) in rivers.iter().enumerate() {
        // Pad covers outer valley carve (~4× channel / class radii), not just wet width.
        let pad = (river.half_width_m * 5.0).max(240.0);
        stamp_polyline(&mut cell_rivers, size, ri as u32, &river.points, pad);
    }
    for (li, lake) in lakes.iter().enumerate() {
        stamp_ring_aabb(&mut cell_lakes, size, li as u32, &lake.ring, 80.0);
    }
    for (ci, coast) in coasts.iter().enumerate() {
        // Wide pad so nearshore ocean keeps an indexed coast id. Beyond the
        // pad, `coast_signed` treats empty index as open ocean (no full-ring scan).
        stamp_ring_aabb(&mut cell_coasts, size, ci as u32, &coast.ring, 4_000.0);
    }
    (cell_rivers, cell_lakes, cell_coasts)
}

fn stamp_polyline(index: &mut [Vec<u32>], size: usize, id: u32, points: &[Vec2], pad_m: f32) {
    for w in points.windows(2) {
        let a = w[0];
        let b = w[1];
        let min_x = a.x.min(b.x) - pad_m;
        let max_x = a.x.max(b.x) + pad_m;
        let min_z = a.y.min(b.y) - pad_m;
        let max_z = a.y.max(b.y) + pad_m;
        stamp_aabb(index, size, id, min_x, max_x, min_z, max_z);
    }
}

fn stamp_ring_aabb(index: &mut [Vec<u32>], size: usize, id: u32, ring: &[Vec2], pad_m: f32) {
    if ring.is_empty() {
        return;
    }
    let mut min_x = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut min_z = f32::INFINITY;
    let mut max_z = f32::NEG_INFINITY;
    for p in ring {
        min_x = min_x.min(p.x);
        max_x = max_x.max(p.x);
        min_z = min_z.min(p.y);
        max_z = max_z.max(p.y);
    }
    stamp_aabb(
        index,
        size,
        id,
        min_x - pad_m,
        max_x + pad_m,
        min_z - pad_m,
        max_z + pad_m,
    );
}

fn stamp_aabb(
    index: &mut [Vec<u32>],
    size: usize,
    id: u32,
    min_x: f32,
    max_x: f32,
    min_z: f32,
    max_z: f32,
) {
    let s = size as i32;
    let ax0 = (min_x / CELL_METRES).floor() as i32;
    let ax1 = (max_x / CELL_METRES).floor() as i32;
    let az0 = (min_z / CELL_METRES).floor() as i32;
    let az1 = (max_z / CELL_METRES).floor() as i32;
    for az in az0..=az1 {
        for ax in ax0..=ax1 {
            if ax < 0 || az < 0 || ax >= s || az >= s {
                continue;
            }
            let idx = az as usize * size + ax as usize;
            if !index[idx].contains(&id) {
                index[idx].push(id);
            }
        }
    }
}

fn endpoint_metres(atlas: &ContinentAtlas, ax: i32, az: i32, ep: Endpoint) -> Vec2 {
    match ep.kind {
        EndpointKind::Ocean => water_edge_metres(atlas, ax, az, |b| b == Biome::Ocean)
            .unwrap_or_else(|| cell_centre(ax, az)),
        EndpointKind::Lake => water_edge_metres(atlas, ax, az, |b| b == Biome::Lake)
            .unwrap_or_else(|| cell_centre(ax, az)),
        EndpointKind::Node => cell_centre(ax, az),
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

/// Midpoint of the cell edge that faces `pred` water. The map overlay already
/// puts mouths here; the 3D polyline used to sit on the land-cell centre, half
/// a kilometre short of the sea.
fn water_edge_metres(
    atlas: &ContinentAtlas,
    ax: i32,
    az: i32,
    pred: impl Fn(Biome) -> bool,
) -> Option<Vec2> {
    for dir in [Dir::East, Dir::South, Dir::West, Dir::North] {
        let (dx, dz) = dir.delta();
        let nx = ax + dx;
        let nz = az + dz;
        let hit = if !atlas.in_bounds(nx, nz) {
            pred(Biome::Ocean)
        } else {
            pred(pack::biome(atlas.cell_at(nx, nz)))
        };
        if hit {
            let (mx, mz) = match dir {
                Dir::North => (ax as f32 + 0.5, az as f32),
                Dir::East => (ax as f32 + 1.0, az as f32 + 0.5),
                Dir::South => (ax as f32 + 0.5, az as f32 + 1.0),
                Dir::West => (ax as f32, az as f32 + 0.5),
            };
            return Some(Vec2::new(mx * CELL_METRES, mz * CELL_METRES));
        }
    }
    None
}

/// Walk each mouth from the last land cell into the water it was authored to
/// drain into. Coast and lake rings meander hundreds of metres off the atlas
/// grid, so an endpoint on the cell edge is still a beach.
fn reach_sinks(
    rivers: &mut [RiverPolyline],
    lakes: &[LakeOutline],
    coasts: &[CoastRing],
    sea: f32,
) {
    let coast_boxes: Vec<(Vec2, Vec2)> =
        coasts.iter().map(|c| padded_aabb(&c.ring, 250.0)).collect();
    for river in rivers.iter_mut() {
        if !river.at_sink {
            continue;
        }
        let Some(heading) = mouth_heading(&river.points) else {
            continue;
        };
        match river.sink {
            HydroSink::Ocean => {
                extend_mouth(river, heading, sea, |p| {
                    !inside_any_coast(p, coasts, &coast_boxes)
                });
            }
            HydroSink::Lake { lake_id } => {
                let Some(lake) = lakes.iter().find(|l| l.id == lake_id) else {
                    continue;
                };
                let sheet = lake.surface_z.max(sea);
                extend_mouth(river, heading, sheet, |p| {
                    point_in_ring(p + shore_domain_warp(p) * 0.45, &lake.ring)
                });
            }
        }
    }
}

fn mouth_heading(points: &[Vec2]) -> Option<Vec2> {
    let last = *points.last()?;
    for p in points.iter().rev().skip(1) {
        let d = last - *p;
        if d.length_squared() > 16.0 {
            return Some(d.normalize());
        }
    }
    None
}

fn extend_mouth(
    river: &mut RiverPolyline,
    heading: Vec2,
    sheet: f32,
    in_water: impl Fn(Vec2) -> bool,
) {
    let start_len = river.points.len();
    let mut at = *river.points.last().expect("a river has points");
    let past = river.half_width_m.max(24.0);
    let mut travelled = 0.0;
    let mut past_water = 0.0;
    let mut seen_water = in_water(at);
    let mut dir = heading;
    while travelled < MOUTH_MAX_M {
        if !seen_water && travelled >= MOUTH_STEP_M * 4.0 {
            if let Some(toward) = nearest_water_dir(at, &in_water) {
                let blended = dir * 0.35 + toward * 0.65;
                dir = if blended.length_squared() > 1e-6 {
                    blended.normalize()
                } else {
                    toward
                };
            }
        }
        at += dir * MOUTH_STEP_M;
        travelled += MOUTH_STEP_M;
        river.points.push(at);
        river.surface_z.push(sheet);
        if in_water(at) {
            seen_water = true;
            past_water += MOUTH_STEP_M;
            if past_water >= past {
                return;
            }
        } else if seen_water {
            return;
        }
    }
    // Blind extension that never found water leaves a worse beach stub — retract.
    if !seen_water {
        river.points.truncate(start_len);
        river.surface_z.truncate(start_len);
    }
}

/// Fan-search for ocean/lake water near a stuck mouth. Headings alone can run
/// along a bay for the whole budget while the shore sits a few hundred metres
/// to the side.
fn nearest_water_dir(at: Vec2, in_water: &impl Fn(Vec2) -> bool) -> Option<Vec2> {
    let mut best: Option<(f32, Vec2)> = None;
    const DIRS: usize = 16;
    for i in 0..DIRS {
        let ang = std::f32::consts::TAU * (i as f32) / (DIRS as f32);
        let dir = Vec2::new(ang.cos(), ang.sin());
        for &dist in &[48.0_f32, 96.0, 192.0, 288.0, 400.0, 560.0] {
            let p = at + dir * dist;
            if !in_water(p) {
                continue;
            }
            if best.is_none_or(|(best_d, _)| dist < best_d) {
                best = Some((dist, dir));
            }
            break;
        }
    }
    best.map(|(_, dir)| dir)
}

fn inside_any_coast(p: Vec2, coasts: &[CoastRing], boxes: &[(Vec2, Vec2)]) -> bool {
    let q = p + shore_domain_warp(p);
    for (coast, (mn, mx)) in coasts.iter().zip(boxes) {
        if q.x < mn.x || q.y < mn.y || q.x > mx.x || q.y > mx.y {
            continue;
        }
        if point_in_ring(q, &coast.ring) {
            return true;
        }
    }
    false
}

fn padded_aabb(ring: &[Vec2], pad: f32) -> (Vec2, Vec2) {
    let mut min = Vec2::splat(f32::INFINITY);
    let mut max = Vec2::splat(f32::NEG_INFINITY);
    for p in ring {
        min = min.min(*p);
        max = max.max(*p);
    }
    (min - Vec2::splat(pad), max + Vec2::splat(pad))
}

/// Domain warp applied to coast/lake queries so the waterline is not the
/// kilometre-cell stair the ring was extracted from. The 3D sampler uses the
/// same offset; mouths have to as well or they stop on the beach.
pub(crate) fn shore_domain_warp(p: Vec2) -> Vec2 {
    let a = (p.x * 0.0019).sin() * (p.y * 0.0014).cos();
    let b = (p.x * 0.0031 + 1.7).cos() * (p.y * 0.0026).sin();
    let c = (p.x * 0.006 + p.y * 0.005).sin();
    Vec2::new(a, b) * 110.0 + Vec2::new(b, -a) * 55.0 + Vec2::new(c, -c) * 35.0
}

fn cell_centre(ax: i32, az: i32) -> Vec2 {
    Vec2::new(
        (ax as f32 + 0.5) * CELL_METRES,
        (az as f32 + 0.5) * CELL_METRES,
    )
}

fn hash_u32(a: u32, b: u32, c: u32) -> u32 {
    let mut x = a
        .wrapping_mul(0x9E37_79B9)
        .wrapping_add(b)
        .wrapping_mul(0x85EB_CA6B)
        .wrapping_add(c);
    x ^= x >> 16;
    x = x.wrapping_mul(0x7FEB_352D);
    x ^= x >> 15;
    x
}
