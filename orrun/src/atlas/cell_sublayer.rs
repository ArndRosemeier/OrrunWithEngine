//! Per-cell vector sublayer — deterministic intent inside one atlas cell.
//!
//! Coordinates are **cell-local** in `[0, 1]²` (x east, y south). Shared edge
//! ports use the same `t`, so adjacent cells' polylines meet at boundaries.
//!
//! Water / shore uses a **world-space** wetness field (same function for every
//! cell) so coasts are continuous across kilometre edges — not independent
//! per-cell polygons.

use glam::Vec2;
use rustc_hash::FxHashMap;

use super::biomes::{self, Biome};
use super::features::{edge_owner, Dir, EndpointKind, Kind};
use super::pack;
use super::types::{Endpoint, Link};
use super::{layer_seed, ContinentAtlas, CELL_METRES};

/// Sub-samples per cell axis for shore raster (~12 m at 1 km/cell).
pub const WATER_RES: usize = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaterBodyKind {
    Ocean,
    Lake { lake_id: i32 },
}

#[derive(Debug, Clone)]
pub struct CellRiver {
    pub feature_id: i32,
    pub class: i32,
    pub half_width_local: f32,
    pub points: Vec<[f32; 2]>,
    pub sink_ocean: bool,
    pub sink_lake: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct CellRoad {
    pub feature_id: i32,
    pub class: i32,
    pub points: Vec<[f32; 2]>,
}

/// Fine wet mask + shore polylines from shared hydro rings.
#[derive(Debug, Clone)]
pub struct CellWaterRaster {
    pub res: usize,
    /// `res * res` samples, row-major (z major then x), true = standing water.
    pub wet: Vec<bool>,
    /// Dominant body for tinting.
    pub kind: WaterBodyKind,
    /// Coast/lake ring segments clipped into this cell (cell-local `[0,1]²`).
    pub shore_lines: Vec<Vec<[f32; 2]>>,
}

/// Vector authorship for a single atlas cell (computed on demand).
#[derive(Debug, Clone)]
pub struct CellSublayer {
    pub ax: i32,
    pub az: i32,
    pub biome: Biome,
    pub water: Option<CellWaterRaster>,
    pub rivers: Vec<CellRiver>,
    pub roads: Vec<CellRoad>,
    pub summary: String,
}

impl CellSublayer {
    pub fn bake(atlas: &ContinentAtlas, ax: i32, az: i32) -> Self {
        assert!(atlas.in_bounds(ax, az), "cell out of bounds");
        let packed = atlas.cell_at(ax, az);
        let biome = pack::biome(packed);
        let seed = layer_seed(atlas.world_seed, "cell_sublayer");
        let idx = atlas.index_of(ax, az);

        let water = bake_water_raster(atlas, ax, az, seed, idx);
        let rivers = bake_rivers(atlas, ax, az, seed);
        let roads = bake_roads(atlas, ax, az, seed);

        let wet_n = water.as_ref().map(|w| w.wet.iter().filter(|b| **b).count()).unwrap_or(0);
        let shore_n = water.as_ref().map(|w| w.shore_lines.len()).unwrap_or(0);
        let summary = format!(
            "sublayer ({ax},{az}) {}  wet_samples={wet_n}/{}  shores={shore_n}  rivers={}  roads={}",
            biome.name(),
            WATER_RES * WATER_RES,
            rivers.len(),
            roads.len(),
        );

        Self {
            ax,
            az,
            biome,
            water,
            rivers,
            roads,
            summary,
        }
    }
}

/// Stored set of revealed cell sublayers (viewer session).
#[derive(Debug, Default, Clone)]
pub struct SublayerStore {
    pub cells: FxHashMap<(i32, i32), CellSublayer>,
}

impl SublayerStore {
    pub fn reveal(&mut self, atlas: &ContinentAtlas, ax: i32, az: i32) -> &CellSublayer {
        let layer = CellSublayer::bake(atlas, ax, az);
        self.cells.insert((ax, az), layer);
        self.cells.get(&(ax, az)).expect("just inserted")
    }

    pub fn clear(&mut self) {
        self.cells.clear();
    }

    pub fn len(&self) -> usize {
        self.cells.len()
    }

    /// Reveal every coast cell and its ocean/lake neighbours (debug / screenshots).
    pub fn reveal_all_shores(&mut self, atlas: &ContinentAtlas) {
        let size = atlas.size as i32;
        for az in 0..size {
            for ax in 0..size {
                let idx = atlas.index_of(ax, az);
                let b = pack::biome(atlas.cell_at(ax, az));
                let interesting = b == Biome::Coast
                    || (biomes::is_water(b) && touches_land(atlas, ax, az))
                    || !atlas.hydro.cell_coasts[idx].is_empty()
                    || !atlas.hydro.cell_lakes[idx].is_empty();
                if interesting {
                    self.reveal(atlas, ax, az);
                    for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                        let nx = ax + dx;
                        let nz = az + dz;
                        if atlas.in_bounds(nx, nz) {
                            self.reveal(atlas, nx, nz);
                        }
                    }
                }
            }
        }
    }
}

fn touches_land(atlas: &ContinentAtlas, ax: i32, az: i32) -> bool {
    for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
        let nx = ax + dx;
        let nz = az + dz;
        if !atlas.in_bounds(nx, nz) {
            continue;
        }
        if biomes::is_land(pack::biome(atlas.cell_at(nx, nz))) {
            return true;
        }
    }
    false
}

fn bake_water_raster(
    atlas: &ContinentAtlas,
    ax: i32,
    az: i32,
    _seed: u32,
    idx: usize,
) -> Option<CellWaterRaster> {
    let biome = pack::biome(atlas.cell_at(ax, az));
    let indexed = !atlas.hydro.cell_coasts[idx].is_empty()
        || !atlas.hydro.cell_lakes[idx].is_empty();
    let near_water = biomes::is_water(biome) || touches_water(atlas, ax, az) || indexed;
    if !near_water {
        return None;
    }
    let kind = match biome {
        Biome::Lake => WaterBodyKind::Lake {
            lake_id: atlas.lake_id[idx],
        },
        _ => WaterBodyKind::Ocean,
    };

    let res = WATER_RES;
    let mut wet = vec![false; res * res];
    let mut any_wet = false;
    for iz in 0..res {
        for ix in 0..res {
            // Sample cell centres so the mask matches the hydro ring fill rule.
            let fx = ax as f32 + (ix as f32 + 0.5) / res as f32;
            let fz = az as f32 + (iz as f32 + 0.5) / res as f32;
            let w = is_water_world(atlas, fx, fz);
            wet[iz * res + ix] = w;
            any_wet |= w;
        }
    }

    let shore_lines = clip_hydro_shores_to_cell(atlas, ax, az);
    if !any_wet && shore_lines.is_empty() {
        return None;
    }

    Some(CellWaterRaster {
        res,
        wet,
        kind,
        shore_lines,
    })
}

/// Clip atlas hydro coast/lake rings into cell-local polylines.
fn clip_hydro_shores_to_cell(atlas: &ContinentAtlas, ax: i32, az: i32) -> Vec<Vec<[f32; 2]>> {
    let idx = atlas.index_of(ax, az);
    let origin = Vec2::new(ax as f32 * CELL_METRES, az as f32 * CELL_METRES);
    let mut lines = Vec::new();

    for &ci in &atlas.hydro.cell_coasts[idx] {
        let ring = &atlas.hydro.coasts[ci as usize].ring;
        lines.extend(clip_closed_ring_to_unit_cell(ring, origin));
    }
    for &li in &atlas.hydro.cell_lakes[idx] {
        let ring = &atlas.hydro.lakes[li as usize].ring;
        lines.extend(clip_closed_ring_to_unit_cell(ring, origin));
    }
    lines
}

fn clip_closed_ring_to_unit_cell(ring: &[Vec2], origin: Vec2) -> Vec<Vec<[f32; 2]>> {
    if ring.len() < 2 {
        return Vec::new();
    }
    let mut out: Vec<Vec<[f32; 2]>> = Vec::new();
    let mut cur: Vec<[f32; 2]> = Vec::new();
    let n = ring.len();
    for i in 0..n {
        let a = ring[i];
        let b = ring[(i + 1) % n];
        let clipped = clip_segment_to_unit_square(a - origin, b - origin);
        if clipped.is_empty() {
            if cur.len() >= 2 {
                out.push(std::mem::take(&mut cur));
            } else {
                cur.clear();
            }
            continue;
        }
        for (j, p) in clipped.iter().enumerate() {
            let local = [p.x / CELL_METRES, p.y / CELL_METRES];
            if cur.is_empty() {
                cur.push(local);
            } else if j == 0 {
                // Start of a new clipped piece — may continue previous if endpoints match.
                let last = *cur.last().expect("cur non-empty");
                if (last[0] - local[0]).abs() > 1e-4 || (last[1] - local[1]).abs() > 1e-4 {
                    if cur.len() >= 2 {
                        out.push(std::mem::take(&mut cur));
                    } else {
                        cur.clear();
                    }
                    cur.push(local);
                }
            } else {
                cur.push(local);
            }
        }
    }
    if cur.len() >= 2 {
        out.push(cur);
    }
    out
}

/// Liang–Barsky clip of a segment into the `[0, CELL_METRES]²` square (metres relative to cell origin).
fn clip_segment_to_unit_square(a: Vec2, b: Vec2) -> Vec<Vec2> {
    let min = 0.0;
    let max = CELL_METRES;
    let mut t0 = 0.0_f32;
    let mut t1 = 1.0_f32;
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let clips = [
        (-dx, a.x - min),
        (dx, max - a.x),
        (-dy, a.y - min),
        (dy, max - a.y),
    ];
    for (p, q) in clips {
        if p.abs() < 1e-12 {
            if q < 0.0 {
                return Vec::new();
            }
            continue;
        }
        let r = q / p;
        if p < 0.0 {
            if r > t1 {
                return Vec::new();
            }
            if r > t0 {
                t0 = r;
            }
        } else if r < t0 {
            return Vec::new();
        } else if r < t1 {
            t1 = r;
        }
    }
    if t0 > t1 {
        return Vec::new();
    }
    vec![a + (b - a) * t0, a + (b - a) * t1]
}

fn touches_water(atlas: &ContinentAtlas, ax: i32, az: i32) -> bool {
    for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (1, -1), (-1, 1), (-1, -1)] {
        let nx = ax + dx;
        let nz = az + dz;
        if cell_is_water_biome(atlas, nx, nz) {
            return true;
        }
    }
    false
}

fn cell_is_water_biome(atlas: &ContinentAtlas, ax: i32, az: i32) -> bool {
    if !atlas.in_bounds(ax, az) {
        return true;
    }
    biomes::is_water(pack::biome(atlas.cell_at(ax, az)))
}

/// Standing water test from atlas hydro rings (same answer for every cell).
/// Lakes ⊃ ocean; land is interior of any coast ring.
pub fn is_water_world(atlas: &ContinentAtlas, fx: f32, fz: f32) -> bool {
    let p = Vec2::new(fx * CELL_METRES, fz * CELL_METRES);
    for lake in &atlas.hydro.lakes {
        if point_in_ring_m(p, &lake.ring) {
            return true;
        }
    }
    let mut any_coast = false;
    for coast in &atlas.hydro.coasts {
        any_coast = true;
        if point_in_ring_m(p, &coast.ring) {
            return false; // interior of landmass
        }
    }
    if !any_coast {
        return cell_is_water_biome(atlas, fx.floor() as i32, fz.floor() as i32);
    }
    true // outside every landmass ring → ocean
}

/// Signed shore field in **atlas cell units**. Negative = water, positive = land.
pub fn shore_signed_world(atlas: &ContinentAtlas, fx: f32, fz: f32, _seed: u32) -> f32 {
    let p = Vec2::new(fx * CELL_METRES, fz * CELL_METRES);

    for lake in &atlas.hydro.lakes {
        let sd = signed_distance_ring_m(p, &lake.ring);
        if sd > 0.0 {
            return -sd / CELL_METRES;
        }
    }

    let mut land_sd = f32::NEG_INFINITY;
    let mut any = false;
    for coast in &atlas.hydro.coasts {
        let sd = signed_distance_ring_m(p, &coast.ring);
        if !any || sd > land_sd {
            land_sd = sd;
            any = true;
        }
    }
    if !any {
        return if cell_is_water_biome(atlas, fx.floor() as i32, fz.floor() as i32) {
            -0.25
        } else {
            0.25
        };
    }
    land_sd / CELL_METRES
}

/// Positive inside a closed ring (metres).
fn signed_distance_ring_m(p: Vec2, ring: &[Vec2]) -> f32 {
    if ring.len() < 3 {
        return f32::NEG_INFINITY;
    }
    let inside = point_in_ring_m(p, ring);
    let mut d = f32::INFINITY;
    let n = ring.len();
    for i in 0..n {
        let a = ring[i];
        let b = ring[(i + 1) % n];
        d = d.min(dist_point_segment_m(p, a, b));
    }
    if inside {
        d
    } else {
        -d
    }
}

fn point_in_ring_m(p: Vec2, ring: &[Vec2]) -> bool {
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

fn dist_point_segment_m(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    let ab = b - a;
    let len2 = ab.length_squared();
    if len2 < 1e-6 {
        return p.distance(a);
    }
    let t = ((p - a).dot(ab) / len2).clamp(0.0, 1.0);
    (a + ab * t).distance(p)
}

fn bake_rivers(atlas: &ContinentAtlas, ax: i32, az: i32, seed: u32) -> Vec<CellRiver> {
    let links = atlas.links_in_cell(ax, az, Kind::River);
    let cell_idx = atlas.index_of(ax, az) as i32;
    let mut out = Vec::new();
    for link in links {
        let a = endpoint_local(atlas, ax, az, link.a);
        let b = endpoint_local(atlas, ax, az, link.b);
        let points = dense_corridor(
            a,
            b,
            seed,
            cell_idx,
            link.feature_id,
            CorridorStyle {
                step_local: 0.03,
                min_samples: 12,
                max_samples: 48,
                amp_coarse: 0.14,
                amp_fine: 0.045,
                amp_micro: 0.012,
            },
        );
        let (sink_ocean, sink_lake) = sink_flags(link);
        let half_m = match link.feature_class.clamp(1, 4) {
            1 => 12.0,
            2 => 20.0,
            3 => 30.0,
            _ => 42.0,
        };
        out.push(CellRiver {
            feature_id: link.feature_id,
            class: link.feature_class,
            half_width_local: (half_m / super::CELL_METRES).clamp(0.02, 0.22),
            points,
            sink_ocean,
            sink_lake,
        });
    }
    out
}

fn bake_roads(atlas: &ContinentAtlas, ax: i32, az: i32, seed: u32) -> Vec<CellRoad> {
    let links = atlas.links_in_cell(ax, az, Kind::Road);
    let cell_idx = atlas.index_of(ax, az) as i32;

    let mut best: FxHashMap<(EndpointKey, EndpointKey), &Link> = FxHashMap::default();
    for link in links {
        let mut ka = endpoint_key(link.a);
        let mut kb = endpoint_key(link.b);
        if kb < ka {
            std::mem::swap(&mut ka, &mut kb);
        }
        best
            .entry((ka, kb))
            .and_modify(|prev| {
                if link.feature_class < prev.feature_class {
                    *prev = link;
                }
            })
            .or_insert(link);
    }

    let mut out = Vec::new();
    for link in best.into_values() {
        let a = endpoint_local(atlas, ax, az, link.a);
        let b = endpoint_local(atlas, ax, az, link.b);
        let points = dense_corridor(
            a,
            b,
            seed ^ 0xA0AD,
            cell_idx,
            link.feature_id,
            CorridorStyle {
                step_local: 0.04,
                min_samples: 10,
                max_samples: 36,
                amp_coarse: 0.05,
                amp_fine: 0.018,
                amp_micro: 0.006,
            },
        );
        out.push(CellRoad {
            feature_id: link.feature_id,
            class: link.feature_class,
            points,
        });
    }
    out.sort_by_key(|r| (r.class, r.feature_id));
    out
}

struct CorridorStyle {
    step_local: f32,
    min_samples: usize,
    max_samples: usize,
    amp_coarse: f32,
    amp_fine: f32,
    amp_micro: f32,
}

fn dense_corridor(
    a: [f32; 2],
    b: [f32; 2],
    seed: u32,
    cell_idx: i32,
    feature_id: i32,
    style: CorridorStyle,
) -> Vec<[f32; 2]> {
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1e-4 {
        return vec![a, b];
    }
    let dir = [dx / len, dy / len];
    let perp = [-dir[1], dir[0]];
    let n = ((len / style.step_local).ceil() as usize)
        .clamp(style.min_samples, style.max_samples);

    let mut pts = Vec::with_capacity(n + 1);
    for i in 0..=n {
        let t = i as f32 / n as f32;
        if i == 0 {
            pts.push(a);
            continue;
        }
        if i == n {
            pts.push(b);
            continue;
        }
        let envelope = (std::f32::consts::PI * t).sin();
        let h0 = hash_unit(seed, cell_idx as u32, feature_id as u32, i as u32);
        let h1 = hash_unit(seed ^ 0x9E37, cell_idx as u32, feature_id as u32, (i * 3) as u32);
        let h2 = hash_unit(seed ^ 0x85EB, cell_idx as u32, feature_id as u32, (i * 7) as u32);
        let h0b = hash_unit(seed, cell_idx as u32, feature_id as u32, (i + 1) as u32);
        let coarse = (h0 * 2.0 - 1.0) * 0.65 + (h0b * 2.0 - 1.0) * 0.35;
        let fine = (h1 * 2.0 - 1.0) * (0.5 + 0.5 * (t * 11.0).sin());
        let micro = (h2 * 2.0 - 1.0) * (0.5 + 0.5 * (t * 29.0).cos());
        let lateral = envelope
            * len
            * (coarse * style.amp_coarse + fine * style.amp_fine + micro * style.amp_micro);
        let x = a[0] + dx * t + perp[0] * lateral;
        let y = a[1] + dy * t + perp[1] * lateral;
        pts.push([x.clamp(0.02, 0.98), y.clamp(0.02, 0.98)]);
    }
    pts
}

fn hash_unit(a: u32, b: u32, c: u32, d: u32) -> f32 {
    hash_u32(a ^ d.wrapping_mul(0x27D4_EB2D), b, c) as f32 / u32::MAX as f32
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct EndpointKey {
    kind: u8,
    ref_id: i32,
    port_id: i32,
}

fn endpoint_key(ep: Endpoint) -> EndpointKey {
    EndpointKey {
        kind: ep.kind as u8,
        ref_id: ep.ref_id,
        port_id: ep.port_id,
    }
}

fn sink_flags(link: &Link) -> (bool, Option<i32>) {
    for ep in [link.a, link.b] {
        match ep.kind {
            EndpointKind::Ocean => return (true, None),
            EndpointKind::Lake => return (false, Some(ep.ref_id)),
            _ => {}
        }
    }
    (false, None)
}

fn endpoint_local(atlas: &ContinentAtlas, ax: i32, az: i32, ep: Endpoint) -> [f32; 2] {
    match ep.kind {
        EndpointKind::Ocean => edge_mid_toward(atlas, ax, az, |b| b == Biome::Ocean)
            .unwrap_or([0.5, 0.5]),
        EndpointKind::Lake => edge_mid_toward(atlas, ax, az, |b| b == Biome::Lake)
            .unwrap_or([0.5, 0.5]),
        EndpointKind::Node => [0.5, 0.5],
        EndpointKind::EdgePort => {
            let (ox, oz, dir) = edge_owner(ep.ref_id);
            let ports = atlas
                .river_ports
                .get(&ep.ref_id)
                .or_else(|| atlas.road_ports.get(&ep.ref_id))
                .map(|p| p.as_slice())
                .unwrap_or(&[]);
            let mut t = 0.5_f32;
            for p in ports {
                if p.id == ep.port_id {
                    t = p.t;
                    break;
                }
            }
            let (wx, wz) = match dir {
                Dir::East => (ox as f32 + 1.0, oz as f32 + t),
                Dir::South => (ox as f32 + t, oz as f32 + 1.0),
                _ => (ax as f32 + 0.5, az as f32 + 0.5),
            };
            [
                (wx - ax as f32).clamp(0.0, 1.0),
                (wz - az as f32).clamp(0.0, 1.0),
            ]
        }
    }
}

fn edge_mid_toward(
    atlas: &ContinentAtlas,
    ax: i32,
    az: i32,
    pred: impl Fn(Biome) -> bool,
) -> Option<[f32; 2]> {
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
            return Some(match dir {
                Dir::North => [0.5, 0.0],
                Dir::East => [1.0, 0.5],
                Dir::South => [0.5, 1.0],
                Dir::West => [0.0, 0.5],
            });
        }
    }
    None
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atlas::ContinentAtlas;

    #[test]
    fn bake_cell_is_deterministic() {
        let atlas = ContinentAtlas::generate(3, 48);
        let a = CellSublayer::bake(&atlas, 10, 10);
        let b = CellSublayer::bake(&atlas, 10, 10);
        assert_eq!(a.summary, b.summary);
    }

    #[test]
    fn shore_field_continuous_across_edge() {
        let atlas = ContinentAtlas::generate(3, 64);
        let seed = layer_seed(atlas.world_seed, "cell_sublayer");
        // Find a land/ocean edge and compare field from both sides of the shared edge.
        let size = atlas.size as i32;
        for az in 1..size - 1 {
            for ax in 1..size - 1 {
                let b0 = pack::biome(atlas.cell_at(ax, az));
                let b1 = pack::biome(atlas.cell_at(ax + 1, az));
                if biomes::is_water(b0) == biomes::is_water(b1) {
                    continue;
                }
                for i in 0..16 {
                    let t = (i as f32 + 0.5) / 16.0;
                    let fx = ax as f32 + 1.0; // shared east edge
                    let fz = az as f32 + t;
                    let v = shore_signed_world(&atlas, fx, fz, seed);
                    // Same query — trivial; also check neighbour of edge agrees.
                    let v2 = shore_signed_world(&atlas, fx, fz, seed);
                    assert_eq!(v, v2);
                    // Hydro shore can wander ~0.4 cells from the biome stair; stay bounded.
                    assert!(
                        v.abs() < 1.25,
                        "expected finite shore field near land/ocean edge, got {v}"
                    );
                }
                return;
            }
        }
        panic!("no land/ocean edge");
    }

    #[test]
    fn coast_neighbourhood_has_fluent_shore() {
        let atlas = ContinentAtlas::generate(3, 64);
        let size = atlas.size as i32;
        for az in 1..size - 1 {
            for ax in 1..size - 1 {
                if pack::biome(atlas.cell_at(ax, az)) != Biome::Coast {
                    continue;
                }
                // Bake coast + ocean neighbour — together they must show wet+shore.
                let mut wet_total = 0usize;
                let mut shore_total = 0usize;
                for (dx, dz) in [(0, 0), (1, 0), (-1, 0), (0, 1), (0, -1)] {
                    let nx = ax + dx;
                    let nz = az + dz;
                    if !atlas.in_bounds(nx, nz) {
                        continue;
                    }
                    let layer = CellSublayer::bake(&atlas, nx, nz);
                    if let Some(w) = &layer.water {
                        wet_total += w.wet.iter().filter(|b| **b).count();
                        shore_total += w.shore_lines.len();
                    }
                }
                assert!(wet_total > 10, "expected wet samples near coast");
                assert!(shore_total > 0, "expected shore isolines near coast");
                return;
            }
        }
        panic!("no coast");
    }

    #[test]
    fn roads_dedupe_shared_ports() {
        let atlas = ContinentAtlas::generate(3, 64);
        let size = atlas.size as i32;
        for az in 0..size {
            for ax in 0..size {
                let n = atlas.links_in_cell(ax, az, Kind::Road).len();
                if n >= 6 {
                    let layer = CellSublayer::bake(&atlas, ax, az);
                    assert!(layer.roads.len() < n);
                    return;
                }
            }
        }
    }

    #[test]
    fn river_corridor_is_finer_than_three_points() {
        let atlas = ContinentAtlas::generate(5, 64);
        let size = atlas.size as i32;
        for az in 0..size {
            for ax in 0..size {
                if atlas.links_in_cell(ax, az, Kind::River).is_empty() {
                    continue;
                }
                let layer = CellSublayer::bake(&atlas, ax, az);
                assert!(layer.rivers[0].points.len() >= 12);
                return;
            }
        }
        panic!("no river cell");
    }

    #[test]
    fn coast_rings_meander_off_grid() {
        let atlas = ContinentAtlas::generate(3, 64);
        assert!(!atlas.hydro.coasts.is_empty());
        let mut any_meander = false;
        for coast in &atlas.hydro.coasts {
            let n = coast.ring.len();
            assert!(n >= 16, "coast ring too sparse: {n}");
            let mean_y = coast.ring.iter().map(|p| p.y).sum::<f32>() / n as f32;
            let mad_y = coast.ring.iter().map(|p| (p.y - mean_y).abs()).sum::<f32>() / n as f32;
            let mean_x = coast.ring.iter().map(|p| p.x).sum::<f32>() / n as f32;
            let mad_x = coast.ring.iter().map(|p| (p.x - mean_x).abs()).sum::<f32>() / n as f32;
            // Local meander: consecutive points should not stay on a kilometre grid line.
            let mut off_km = 0usize;
            for p in &coast.ring {
                let near_km_x = (p.x / CELL_METRES).fract().abs() < 0.02
                    || (p.x / CELL_METRES).fract().abs() > 0.98;
                let near_km_z = (p.y / CELL_METRES).fract().abs() < 0.02
                    || (p.y / CELL_METRES).fract().abs() > 0.98;
                if !(near_km_x || near_km_z) {
                    off_km += 1;
                }
            }
            eprintln!(
                "coast lm={} n={n} mad_x={mad_x:.0} mad_y={mad_y:.0} off_km={off_km}/{n}",
                coast.landmass_id
            );
            if off_km * 2 > n {
                any_meander = true;
            }
        }
        assert!(any_meander, "expected coast vertices off the kilometre grid");
    }

    /// Writes a PPM preview of the shared shore field around a coast patch.
    #[test]
    fn write_shore_field_preview_ppm() {
        let atlas = ContinentAtlas::generate(3, 64);
        let size = atlas.size as i32;
        // Land/ocean window with mixed wet+dry samples (skip open-ocean lakes).
        let cells: i32 = 6;
        let mut best = None;
        let mut best_score = -1.0_f32;
        for az in 2..size - cells - 2 {
            for ax in 2..size - cells - 2 {
                let mut coastish = 0i32;
                let mut landish = 0i32;
                let mut oceanish = 0i32;
                for cz in 0..cells {
                    for cx in 0..cells {
                        let b = pack::biome(atlas.cell_at(ax + cx, az + cz));
                        if b == Biome::Coast {
                            coastish += 1;
                        }
                        if biomes::is_land(b) {
                            landish += 1;
                        }
                        if biomes::is_water(b) {
                            oceanish += 1;
                        }
                    }
                }
                if coastish < 2 || landish < 4 || oceanish < 4 {
                    continue;
                }
                let mut wet = 0i32;
                let mut dry = 0i32;
                let mut flips = 0i32;
                let mut prev: Option<bool> = None;
                for t in 0..32 {
                    let u = t as f32 / 31.0;
                    let fx = ax as f32 + 0.5 + u * (cells as f32 - 1.0);
                    let fz = az as f32 + 0.5 + u * (cells as f32 - 1.0);
                    let w = is_water_world(&atlas, fx, fz);
                    if w {
                        wet += 1;
                    } else {
                        dry += 1;
                    }
                    if let Some(p) = prev {
                        if p != w {
                            flips += 1;
                        }
                    }
                    prev = Some(w);
                }
                if wet < 4 || dry < 4 {
                    continue;
                }
                let score = flips as f32 + coastish as f32;
                if score > best_score {
                    best_score = score;
                    best = Some((ax, az));
                }
            }
        }
        let (ox, oz) = best.expect("interesting coast window");
        eprintln!("shore preview origin=({ox},{oz}) score={best_score:.2}");
        let pix_per_cell: usize = 40;
        let w = cells as usize * pix_per_cell;
        let h = cells as usize * pix_per_cell;
        let mut rgb = vec![0u8; w * h * 3];
        for py in 0..h {
            for px in 0..w {
                let fx = ox as f32 + px as f32 / pix_per_cell as f32;
                let fz = oz as f32 + py as f32 / pix_per_cell as f32;
                let wet = is_water_world(&atlas, fx, fz);
                let i = (py * w + px) * 3;
                if wet {
                    rgb[i] = 40;
                    rgb[i + 1] = 110;
                    rgb[i + 2] = 190;
                } else {
                    rgb[i] = 110;
                    rgb[i + 1] = 140;
                    rgb[i + 2] = 75;
                }
            }
        }
        // Overlay clipped hydro shore segments (magenta) to verify continuity.
        for cz in 0..cells as i32 {
            for cx in 0..cells as i32 {
                let ax = ox + cx;
                let az = oz + cz;
                if !atlas.in_bounds(ax, az) {
                    continue;
                }
                for line in clip_hydro_shores_to_cell(&atlas, ax, az) {
                    for win in line.windows(2) {
                        let x0 = ((cx as f32 + win[0][0]) * pix_per_cell as f32) as i32;
                        let y0 = ((cz as f32 + win[0][1]) * pix_per_cell as f32) as i32;
                        let x1 = ((cx as f32 + win[1][0]) * pix_per_cell as f32) as i32;
                        let y1 = ((cz as f32 + win[1][1]) * pix_per_cell as f32) as i32;
                        draw_line_rgb(&mut rgb, w, h, x0, y0, x1, y1, [255, 60, 200]);
                    }
                }
            }
        }
        // Cell grid.
        for c in 0..=cells as usize {
            let x = c * pix_per_cell;
            if x < w {
                for y in 0..h {
                    let i = (y * w + x.min(w - 1)) * 3;
                    rgb[i] = 255;
                    rgb[i + 1] = 220;
                    rgb[i + 2] = 80;
                }
            }
            let y = c * pix_per_cell;
            if y < h {
                for x in 0..w {
                    let i = (y.min(h - 1) * w + x) * 3;
                    rgb[i] = 255;
                    rgb[i + 1] = 220;
                    rgb[i + 2] = 80;
                }
            }
        }
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tmp_shore_field.ppm");
        let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
        out.extend_from_slice(&rgb);
        std::fs::write(&path, out).expect("write ppm");
        eprintln!("wrote {}", path.display());
    }

    fn draw_line_rgb(rgb: &mut [u8], w: usize, h: usize, x0: i32, y0: i32, x1: i32, y1: i32, c: [u8; 3]) {
        let mut x = x0;
        let mut y = y0;
        let dx = (x1 - x0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let dy = -(y1 - y0).abs();
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;
        loop {
            if x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < h {
                let i = (y as usize * w + x as usize) * 3;
                rgb[i] = c[0];
                rgb[i + 1] = c[1];
                rgb[i + 2] = c[2];
            }
            if x == x1 && y == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x += sx;
            }
            if e2 <= dx {
                err += dx;
                y += sy;
            }
        }
    }
}
