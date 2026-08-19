//! Sparse land occupancy after rivers.

use engine::proc::Noise;
use rustc_hash::FxHashMap;

use super::biomes::{self, Biome};
use super::pack;
use super::types::Link;
use super::{layer_seed, lerp, smoothstep, NEIGHBOR_DX, NEIGHBOR_DZ};

const POPULATION_THRESHOLD: f32 = 0.66;
const POPULATION_SCORE_SPAN: f32 = 1.0;
pub const POPULATION_MOUTH_RADIUS: i32 = 2;
pub const SETTLEMENT_SLOPE_REF: f32 = 0.02;
pub const SETTLEMENT_SLOPE_CLIFF: f32 = 0.12;
pub const CELL_METRES: f32 = 1000.0;

/// Where a settlement prefers to sit, for weighted seeding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettlementSite {
    RiverMouth,
    Confluence,
    NearRiver,
    Inland,
}

impl SettlementSite {
    pub fn weight(self) -> f32 {
        match self {
            Self::RiverMouth => 4.0,
            Self::Confluence => 2.5,
            Self::NearRiver => 1.5,
            Self::Inland => 0.55,
        }
    }
}

pub fn classify_settlement_site(
    ax: i32,
    az: i32,
    size: usize,
    river_links: &FxHashMap<i32, Vec<Link>>,
    mouth_distance: &[i32],
) -> SettlementSite {
    let idx = az as usize * size + ax as usize;
    if mouth_distance[idx] == 0 {
        return SettlementSite::RiverMouth;
    }
    if is_river_confluence(ax, az, size, river_links, mouth_distance) {
        return SettlementSite::Confluence;
    }
    if river_links.contains_key(&(idx as i32)) || touches_river(ax, az, size, river_links) {
        return SettlementSite::NearRiver;
    }
    SettlementSite::Inland
}

pub fn is_river_confluence(
    ax: i32,
    az: i32,
    size: usize,
    river_links: &FxHashMap<i32, Vec<Link>>,
    mouth_distance: &[i32],
) -> bool {
    let idx = az as usize * size + ax as usize;
    if mouth_distance[idx] == 0 || !river_links.contains_key(&(idx as i32)) {
        return false;
    }
    let mut river_neighbors = 0u32;
    for k in 0..8 {
        let nx = ax + NEIGHBOR_DX[k];
        let nz = az + NEIGHBOR_DZ[k];
        if nx >= 0
            && nz >= 0
            && (nx as usize) < size
            && (nz as usize) < size
            && river_links.contains_key(&(nz * size as i32 + nx))
        {
            river_neighbors += 1;
        }
    }
    river_neighbors >= 2
}

pub fn seed_population(
    world_seed: i32,
    size: usize,
    cells: &mut [i32],
    river_links: &FxHashMap<i32, Vec<Link>>,
    mouth_distance: &mut Vec<i32>,
) {
    *mouth_distance = river_mouth_distances(size, cells, river_links);
    let grain = Noise::new(layer_seed(world_seed, "atlas_population"));
    let region = Noise::new(layer_seed(world_seed, "atlas_pop_region"));

    for az in 0..size {
        for ax in 0..size {
            let idx = az * size + ax;
            let packed = cells[idx];
            let biome = pack::biome(packed);
            let pop = if biomes::is_land(biome) {
                let score = population_score(
                    ax as i32,
                    az as i32,
                    idx,
                    packed,
                    biome,
                    mouth_distance,
                    size,
                    cells,
                    river_links,
                    &grain,
                    &region,
                );
                if score > POPULATION_THRESHOLD {
                    let t = (score - POPULATION_THRESHOLD) / POPULATION_SCORE_SPAN;
                    (1 + (t * 15.0) as i32).clamp(1, 15)
                } else {
                    0
                }
            } else {
                0
            };
            cells[idx] = pack::pack(
                pack::elevation(packed),
                pack::humidity(packed),
                biome,
                pack::relief(packed),
                pop,
            );
        }
    }
}

fn river_mouth_distances(
    size: usize,
    cells: &[i32],
    river_links: &FxHashMap<i32, Vec<Link>>,
) -> Vec<i32> {
    let count = size * size;
    let mut dist = vec![-1i32; count];
    let mut frontier = Vec::new();
    for (&cell, links) in river_links {
        if cell_is_river_mouth(links) {
            dist[cell as usize] = 0;
            frontier.push(cell);
        }
    }
    let mut ring = 0;
    while ring < POPULATION_MOUTH_RADIUS && !frontier.is_empty() {
        let mut next = Vec::new();
        for cell in frontier {
            let cx = cell % size as i32;
            let cz = cell / size as i32;
            for k in 0..8 {
                let nx = cx + NEIGHBOR_DX[k];
                let nz = cz + NEIGHBOR_DZ[k];
                if nx < 0 || nz < 0 || nx as usize >= size || nz as usize >= size {
                    continue;
                }
                let nb = nz as usize * size + nx as usize;
                if dist[nb] >= 0 || !biomes::is_land(pack::biome(cells[nb])) {
                    continue;
                }
                dist[nb] = ring + 1;
                next.push(nb as i32);
            }
        }
        frontier = next;
        ring += 1;
    }
    dist
}

fn cell_is_river_mouth(links: &[Link]) -> bool {
    links.iter().any(|l| {
        matches!(
            l.b.kind,
            super::features::EndpointKind::Ocean | super::features::EndpointKind::Lake
        )
    })
}

fn population_score(
    ax: i32,
    az: i32,
    idx: usize,
    packed: i32,
    biome: Biome,
    mouth_distance: &[i32],
    size: usize,
    cells: &[i32],
    river_links: &FxHashMap<i32, Vec<Link>>,
    grain: &Noise,
    region: &Noise,
) -> f32 {
    let mouth_dist = mouth_distance[idx];
    let humidity = pack::humidity(packed) as f32 / 255.0;
    let relief = pack::relief(packed) as f32 / 63.0;
    let elevation = pack::elevation(packed);
    let flatness = flatness_fitness(ax, az, size, cells);
    let slope = local_slope(ax, az, size, cells);

    let mut score = smoothstep(0.28, 0.78, humidity) * 0.5;
    score -= relief * 0.65;
    if elevation > 190 {
        score -= 0.55;
    } else if elevation > 150 {
        score -= 0.22;
    }

    match biome {
        Biome::Arid => score -= 0.4,
        Biome::Alpine => score -= 0.6,
        Biome::Tundra => score -= 0.35,
        Biome::Wetland => score -= 0.1,
        Biome::Coast => score += 0.08,
        _ => {}
    }

    if river_links.contains_key(&(idx as i32)) {
        score += 0.32;
    } else if touches_river(ax, az, size, river_links) {
        score += 0.12;
    }

    if is_river_confluence(ax, az, size, river_links, mouth_distance) {
        score += 0.28;
    }

    if mouth_dist == 0 {
        score += 0.48;
    } else if mouth_dist > 0 {
        score += lerp(
            0.32,
            0.10,
            (mouth_dist - 1) as f32 / POPULATION_MOUTH_RADIUS as f32,
        );
    }

    score += region.fbm2(ax as f32 * 0.007, az as f32 * 0.007, 3, 2.0, 0.5) * 0.22;
    score += grain.fbm2(ax as f32 * 0.045, az as f32 * 0.045, 3, 2.0, 0.5) * 0.14;
    score *= lerp(0.05, 1.0, flatness);
    if slope > SETTLEMENT_SLOPE_CLIFF {
        score *= 0.05;
    }
    score
}

fn land_elev_m(ax: i32, az: i32, size: usize, cells: &[i32]) -> Option<f32> {
    if ax < 0 || az < 0 || ax as usize >= size || az as usize >= size {
        return None;
    }
    let packed = cells[az as usize * size + ax as usize];
    if !biomes::is_land(pack::biome(packed)) {
        return None;
    }
    Some(pack::elevation_to_metres(pack::elevation(packed)) as f32)
}

pub fn local_slope(ax: i32, az: i32, size: usize, cells: &[i32]) -> f32 {
    let Some(h0) = land_elev_m(ax, az, size, cells) else {
        return SETTLEMENT_SLOPE_CLIFF + 1.0;
    };
    let hx_lo = land_elev_m(ax - 1, az, size, cells);
    let hx_hi = land_elev_m(ax + 1, az, size, cells);
    let hz_lo = land_elev_m(ax, az - 1, size, cells);
    let hz_hi = land_elev_m(ax, az + 1, size, cells);
    let gx = match (hx_lo, hx_hi) {
        (Some(lo), Some(hi)) => (hi - lo) / (2.0 * CELL_METRES),
        (None, Some(hi)) => (hi - h0) / CELL_METRES,
        (Some(lo), None) => (h0 - lo) / CELL_METRES,
        _ => 0.0,
    };
    let gz = match (hz_lo, hz_hi) {
        (Some(lo), Some(hi)) => (hi - lo) / (2.0 * CELL_METRES),
        (None, Some(hi)) => (hi - h0) / CELL_METRES,
        (Some(lo), None) => (h0 - lo) / CELL_METRES,
        _ => 0.0,
    };
    (gx * gx + gz * gz).sqrt()
}

pub fn flatness_fitness(ax: i32, az: i32, size: usize, cells: &[i32]) -> f32 {
    let s = local_slope(ax, az, size, cells);
    let t = s / SETTLEMENT_SLOPE_REF;
    1.0 / (1.0 + t * t)
}

pub fn cell_is_settlement_flat(ax: i32, az: i32, size: usize, cells: &[i32]) -> bool {
    local_slope(ax, az, size, cells) <= SETTLEMENT_SLOPE_CLIFF
        && flatness_fitness(ax, az, size, cells) >= 0.35
}

fn touches_river(ax: i32, az: i32, size: usize, river_links: &FxHashMap<i32, Vec<Link>>) -> bool {
    for k in 0..8 {
        let nx = ax + NEIGHBOR_DX[k];
        let nz = az + NEIGHBOR_DZ[k];
        if nx >= 0
            && nz >= 0
            && (nx as usize) < size
            && (nz as usize) < size
            && river_links.contains_key(&(nz * size as i32 + nx))
        {
            return true;
        }
    }
    false
}
