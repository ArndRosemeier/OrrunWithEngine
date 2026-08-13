//! The atlas overlay for one cell — a 2D projection of the 3D world.
//!
//! Nothing here generates terrain. The wet raster is sampled from the canonical
//! [`ContinentalSurface`], and the shore, river, and road lines are the same
//! vectors the chunk builder carves from, clipped to the cell. If the map shows
//! water in a spot, the ground there is wet when you walk it, because both
//! answers come from one field.
//!
//! Coordinates are **cell-local** in `[0, 1]²` (x east, y south).

use engine::space::GlobalXZ;
use glam::Vec2;
use rayon::prelude::*;
use rustc_hash::FxHashMap;

use super::biomes::{self, Biome};
use super::features::{edge_owner, Dir, EndpointKind, Kind};
use super::pack;
use super::types::Endpoint;
use super::{layer_seed, ContinentAtlas, CELL_METRES};
use crate::world::{AtlasCell, ContinentalSurface, WaterBody};

/// Sub-samples per cell axis for the shore raster (~16 m at 1 km/cell).
pub const WATER_RES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaterBodyKind {
    Ocean,
    Lake { lake_id: i32 },
    River,
}

/// A canonical river clipped into one cell.
#[derive(Debug, Clone)]
pub struct OverlayRiver {
    pub id: i32,
    pub class: i32,
    pub half_width_local: f32,
    pub points: Vec<[f32; 2]>,
}

#[derive(Debug, Clone)]
pub struct OverlayRoad {
    pub feature_id: i32,
    pub class: i32,
    pub points: Vec<[f32; 2]>,
}

/// Wet mask plus shore isolines for one cell.
#[derive(Debug, Clone)]
pub struct CellWaterRaster {
    pub res: usize,
    /// `res * res` samples, row-major (z major then x), true = standing water.
    pub wet: Vec<bool>,
    /// Dominant body, for tinting.
    pub kind: WaterBodyKind,
    /// Coast/lake rings clipped into this cell (cell-local `[0,1]²`).
    pub shore_lines: Vec<Vec<[f32; 2]>>,
}

/// Cacheable 2D view of one atlas cell.
#[derive(Debug, Clone)]
pub struct AtlasCellOverlay {
    pub cell: AtlasCell,
    pub biome: Biome,
    pub water: Option<CellWaterRaster>,
    pub rivers: Vec<OverlayRiver>,
    pub roads: Vec<OverlayRoad>,
    pub summary: String,
}

impl AtlasCellOverlay {
    pub fn bake(atlas: &ContinentAtlas, surface: &ContinentalSurface, cell: AtlasCell) -> Self {
        let (ax, az) = (cell.ax(), cell.az());
        let biome = pack::biome(atlas.cell_at(ax, az));
        let seed = layer_seed(atlas.world_seed, "cell_overlay");

        let water = bake_water_raster(atlas, surface, ax, az);
        let rivers = clip_rivers(atlas, ax, az);
        let roads = bake_roads(atlas, ax, az, seed);

        let wet_n = water
            .as_ref()
            .map(|w| w.wet.iter().filter(|b| **b).count())
            .unwrap_or(0);
        let shore_n = water.as_ref().map(|w| w.shore_lines.len()).unwrap_or(0);
        let summary = format!(
            "overlay ({ax},{az}) {}  wet={wet_n}/{}  shores={shore_n}  rivers={}  roads={}",
            biome.name(),
            WATER_RES * WATER_RES,
            rivers.len(),
            roads.len(),
        );

        Self {
            cell,
            biome,
            water,
            rivers,
            roads,
            summary,
        }
    }

    pub fn ax(&self) -> i32 {
        self.cell.ax()
    }

    pub fn az(&self) -> i32 {
        self.cell.az()
    }

    pub fn origin_world(&self) -> Vec2 {
        Vec2::new(
            self.ax() as f32 * CELL_METRES,
            self.az() as f32 * CELL_METRES,
        )
    }

    pub fn local_to_world(&self, local: [f32; 2]) -> Vec2 {
        self.origin_world() + Vec2::new(local[0] * CELL_METRES, local[1] * CELL_METRES)
    }
}

/// Revealed overlays for a viewer session.
#[derive(Debug, Default, Clone)]
pub struct OverlayStore {
    pub cells: FxHashMap<(i32, i32), AtlasCellOverlay>,
}

impl OverlayStore {
    pub fn reveal(
        &mut self,
        atlas: &ContinentAtlas,
        surface: &ContinentalSurface,
        cell: AtlasCell,
    ) -> &AtlasCellOverlay {
        let key = (cell.ax(), cell.az());
        let overlay = AtlasCellOverlay::bake(atlas, surface, cell);
        self.cells.insert(key, overlay);
        self.cells.get(&key).expect("just inserted")
    }

    pub fn clear(&mut self) {
        self.cells.clear();
    }

    pub fn len(&self) -> usize {
        self.cells.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    pub fn contains(&self, ax: i32, az: i32) -> bool {
        self.cells.contains_key(&(ax, az))
    }

    /// Reveal every coast cell and its water neighbours (debug / screenshots).
    pub fn reveal_all_shores(&mut self, atlas: &ContinentAtlas, surface: &ContinentalSurface) {
        let bounds = surface.bounds();
        let size = atlas.size as i32;
        for az in 0..size {
            for ax in 0..size {
                let idx = atlas.index_of(ax, az);
                let b = pack::biome(atlas.cell_at(ax, az));
                let interesting = b == Biome::Coast
                    || !atlas.hydro.cell_coasts[idx].is_empty()
                    || !atlas.hydro.cell_lakes[idx].is_empty();
                if !interesting {
                    continue;
                }
                for (dx, dz) in [(0, 0), (1, 0), (-1, 0), (0, 1), (0, -1)] {
                    if let Ok(cell) = AtlasCell::new(bounds, ax + dx, az + dz) {
                        self.reveal(atlas, surface, cell);
                    }
                }
            }
        }
    }
}

fn bake_water_raster(
    atlas: &ContinentAtlas,
    surface: &ContinentalSurface,
    ax: i32,
    az: i32,
) -> Option<CellWaterRaster> {
    let idx = atlas.index_of(ax, az);
    let biome = pack::biome(atlas.cell_at(ax, az));
    let indexed = !atlas.hydro.cell_coasts[idx].is_empty()
        || !atlas.hydro.cell_lakes[idx].is_empty()
        || !atlas.hydro.cell_rivers[idx].is_empty();
    if !biomes::is_water(biome) && !indexed {
        return None;
    }

    let res = WATER_RES;
    let origin = Vec2::new(ax as f32 * CELL_METRES, az as f32 * CELL_METRES);
    let step = CELL_METRES as f64 / res as f64;
    let columns: Vec<Option<WaterBody>> = (0..res * res)
        .into_par_iter()
        .map(|i| {
            let ix = i % res;
            let iz = i / res;
            let p = GlobalXZ::at(
                origin.x as f64 + (ix as f64 + 0.5) * step,
                origin.y as f64 + (iz as f64 + 0.5) * step,
            );
            surface.column(p).body()
        })
        .collect();

    let wet: Vec<bool> = columns.iter().map(|b| b.is_some()).collect();
    let shore_lines = clip_hydro_shores_to_cell(atlas, ax, az);
    if !wet.iter().any(|w| *w) && shore_lines.is_empty() {
        return None;
    }

    let kind = dominant_body(&columns);
    Some(CellWaterRaster {
        res,
        wet,
        kind,
        shore_lines,
    })
}

fn dominant_body(columns: &[Option<WaterBody>]) -> WaterBodyKind {
    let mut ocean = 0usize;
    let mut river = 0usize;
    let mut lakes: FxHashMap<i32, usize> = FxHashMap::default();
    for body in columns.iter().flatten() {
        match body {
            WaterBody::Ocean => ocean += 1,
            WaterBody::River { .. } => river += 1,
            WaterBody::Lake { id } => *lakes.entry(*id).or_default() += 1,
            // Sub-atlas water is below the resolution of a map cell and never
            // reaches the overlay: it lives in the window the session carries,
            // not in the surface these columns come from.
            WaterBody::Pond => {}
        }
    }
    let best_lake = lakes.into_iter().max_by_key(|(id, n)| (*n, *id));
    let lake_n = best_lake.map(|(_, n)| n).unwrap_or(0);
    if lake_n >= ocean && lake_n >= river && lake_n > 0 {
        return WaterBodyKind::Lake {
            lake_id: best_lake.expect("lake counted").0,
        };
    }
    if river > ocean {
        WaterBodyKind::River
    } else {
        WaterBodyKind::Ocean
    }
}

/// Clip canonical coast/lake rings into cell-local polylines.
fn clip_hydro_shores_to_cell(atlas: &ContinentAtlas, ax: i32, az: i32) -> Vec<Vec<[f32; 2]>> {
    let idx = atlas.index_of(ax, az);
    let origin = Vec2::new(ax as f32 * CELL_METRES, az as f32 * CELL_METRES);
    let mut lines = Vec::new();
    for &ci in &atlas.hydro.cell_coasts[idx] {
        let ring = &atlas.hydro.coasts[ci as usize].ring;
        lines.extend(clip_polyline_to_cell(ring, origin, true));
    }
    for &li in &atlas.hydro.cell_lakes[idx] {
        let ring = &atlas.hydro.lakes[li as usize].ring;
        lines.extend(clip_polyline_to_cell(ring, origin, true));
    }
    lines
}

/// Clip canonical river polylines into the cell (no second meander pass).
fn clip_rivers(atlas: &ContinentAtlas, ax: i32, az: i32) -> Vec<OverlayRiver> {
    let idx = atlas.index_of(ax, az);
    let origin = Vec2::new(ax as f32 * CELL_METRES, az as f32 * CELL_METRES);
    let mut out = Vec::new();
    for &ri in &atlas.hydro.cell_rivers[idx] {
        let river = &atlas.hydro.rivers[ri as usize];
        for points in clip_polyline_to_cell(&river.points, origin, false) {
            out.push(OverlayRiver {
                id: river.id,
                class: river.class,
                half_width_local: river.half_width_m / CELL_METRES,
                points,
            });
        }
    }
    out.sort_by_key(|r| (r.id, r.points.len()));
    out
}

/// Split a polyline into the pieces that lie inside the cell square.
fn clip_polyline_to_cell(points: &[Vec2], origin: Vec2, closed: bool) -> Vec<Vec<[f32; 2]>> {
    if points.len() < 2 {
        return Vec::new();
    }
    let n = points.len();
    let segments = if closed { n } else { n - 1 };
    let mut out: Vec<Vec<[f32; 2]>> = Vec::new();
    let mut cur: Vec<[f32; 2]> = Vec::new();
    for i in 0..segments {
        let a = points[i];
        let b = points[(i + 1) % n];
        let clipped = clip_segment_to_cell(a - origin, b - origin);
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
                continue;
            }
            if j == 0 {
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

/// Liang–Barsky clip into `[0, CELL_METRES]²` (metres relative to cell origin).
fn clip_segment_to_cell(a: Vec2, b: Vec2) -> Vec<Vec2> {
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

fn bake_roads(atlas: &ContinentAtlas, ax: i32, az: i32, seed: u32) -> Vec<OverlayRoad> {
    let links = atlas.links_in_cell(ax, az, Kind::Road);
    let cell_idx = atlas.index_of(ax, az) as i32;

    let mut out = Vec::new();
    for link in super::road_geom::unique_links(links) {
        let a = endpoint_local(atlas, ax, az, link.a);
        let b = endpoint_local(atlas, ax, az, link.b);
        let points = super::road_geom::meander_cell_corridor(
            Vec2::new(a[0], a[1]),
            Vec2::new(b[0], b[1]),
            seed ^ 0xA0AD,
            cell_idx,
            link.feature_id,
        )
        .into_iter()
        .map(|p| [p.x, p.y])
        .collect();
        out.push(OverlayRoad {
            feature_id: link.feature_id,
            class: link.feature_class,
            points,
        });
    }
    out.sort_by_key(|r| (r.class, r.feature_id));
    out
}

fn endpoint_local(atlas: &ContinentAtlas, ax: i32, az: i32, ep: Endpoint) -> [f32; 2] {
    match ep.kind {
        EndpointKind::Ocean => {
            edge_mid_toward(atlas, ax, az, |b| b == Biome::Ocean).unwrap_or([0.5, 0.5])
        }
        EndpointKind::Lake => {
            edge_mid_toward(atlas, ax, az, |b| b == Biome::Lake).unwrap_or([0.5, 0.5])
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::AtlasBounds;

    fn atlas_and_surface(seed: i32, size: usize) -> (ContinentAtlas, ContinentalSurface) {
        let atlas = ContinentAtlas::generate(seed, size);
        let surface = ContinentalSurface::new(&atlas).expect("surface");
        (atlas, surface)
    }

    #[test]
    fn overlay_bake_is_deterministic() {
        let (atlas, surface) = atlas_and_surface(3, 48);
        let cell = AtlasCell::new(AtlasBounds::of(&atlas), 10, 10).unwrap();
        let a = AtlasCellOverlay::bake(&atlas, &surface, cell);
        let b = AtlasCellOverlay::bake(&atlas, &surface, cell);
        assert_eq!(a.summary, b.summary);
    }

    #[test]
    fn coast_neighbourhood_shows_water_and_shore_lines() {
        let (atlas, surface) = atlas_and_surface(3, 64);
        let bounds = AtlasBounds::of(&atlas);
        let size = atlas.size as i32;
        for az in 1..size - 1 {
            for ax in 1..size - 1 {
                if pack::biome(atlas.cell_at(ax, az)) != Biome::Coast {
                    continue;
                }
                let mut wet_total = 0usize;
                let mut shore_total = 0usize;
                for (dx, dz) in [(0, 0), (1, 0), (-1, 0), (0, 1), (0, -1)] {
                    let Ok(cell) = AtlasCell::new(bounds, ax + dx, az + dz) else {
                        continue;
                    };
                    let overlay = AtlasCellOverlay::bake(&atlas, &surface, cell);
                    if let Some(w) = &overlay.water {
                        wet_total += w.wet.iter().filter(|b| **b).count();
                        shore_total += w.shore_lines.len();
                    }
                }
                assert!(wet_total > 10, "expected wet samples near a coast");
                assert!(shore_total > 0, "expected shore isolines near a coast");
                return;
            }
        }
        panic!("no coast cell in the atlas");
    }

    #[test]
    fn roads_dedupe_shared_ports() {
        let (atlas, surface) = atlas_and_surface(3, 64);
        let bounds = AtlasBounds::of(&atlas);
        let size = atlas.size as i32;
        for az in 0..size {
            for ax in 0..size {
                let n = atlas.links_in_cell(ax, az, Kind::Road).len();
                if n >= 6 {
                    let cell = AtlasCell::new(bounds, ax, az).unwrap();
                    let overlay = AtlasCellOverlay::bake(&atlas, &surface, cell);
                    assert!(overlay.roads.len() < n);
                    return;
                }
            }
        }
    }

    #[test]
    fn overlay_rivers_follow_the_canonical_polylines() {
        let (atlas, surface) = atlas_and_surface(5, 64);
        let bounds = AtlasBounds::of(&atlas);
        let river = atlas.hydro.rivers.first().expect("a river");
        let mid = river.points[river.points.len() / 2];
        let cell = AtlasCell::new(
            bounds,
            (mid.x / CELL_METRES).floor() as i32,
            (mid.y / CELL_METRES).floor() as i32,
        )
        .unwrap();
        let overlay = AtlasCellOverlay::bake(&atlas, &surface, cell);
        assert!(
            !overlay.rivers.is_empty(),
            "the cell holding a river reach must draw it"
        );
        for line in &overlay.rivers {
            for p in &line.points {
                let world = overlay.local_to_world(*p);
                let d = river
                    .points
                    .iter()
                    .map(|q| q.distance(world))
                    .fold(f32::INFINITY, f32::min);
                assert!(
                    d < 60.0,
                    "overlay river drifted {d:.0} m from the canonical line"
                );
            }
        }
    }

    #[test]
    fn coast_rings_meander_off_grid() {
        let atlas = ContinentAtlas::generate(3, 64);
        assert!(!atlas.hydro.coasts.is_empty());
        let mut any_meander = false;
        for coast in &atlas.hydro.coasts {
            let n = coast.ring.len();
            assert!(n >= 16, "coast ring too sparse: {n}");
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
            if off_km * 2 > n {
                any_meander = true;
            }
        }
        assert!(
            any_meander,
            "expected coast vertices off the kilometre grid"
        );
    }

    #[test]
    fn coast_meander_has_local_wiggle() {
        let atlas = ContinentAtlas::generate(3, 96);
        let coast = atlas.hydro.coasts.first().expect("coast");
        let n = coast.ring.len();
        assert!(n >= 32);
        let mut sum = 0.0_f32;
        let mut count = 0usize;
        let step = (n / 40).max(1);
        for i in 0..n {
            let a = coast.ring[(i + n - step) % n];
            let b = coast.ring[i];
            let c = coast.ring[(i + step) % n];
            let chord = c - a;
            let len = chord.length().max(1.0);
            let nrm = Vec2::new(-chord.y, chord.x) / len;
            let mid = (a + c) * 0.5;
            sum += (b - mid).dot(nrm).abs();
            count += 1;
        }
        let mean = sum / count as f32;
        assert!(
            mean > 35.0,
            "expected lively shore meander (>35m local wiggle), got {mean:.1}m"
        );
    }
}
