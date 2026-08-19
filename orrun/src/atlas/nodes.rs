//! Typed road / settlement node seeding.

use rand::Rng;
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use rustc_hash::FxHashMap;

use super::biomes;
use super::features::NodeKind;
use super::landmask::collar_cells;
use super::pack;
use super::population::{cell_is_settlement_flat, classify_settlement_site, SettlementSite};
use super::types::{GraphNode, Link};
use super::{feature_hash, layer_seed};

const SIZE_FULL: i32 = 1000;
const PRIMARY_NODE_TARGET: i32 = 72;
const SETTLEMENT_TARGET_FULL: i32 = 100;
const DUNGEON_TARGET_FULL: i32 = 50;
const SETTLEMENT_MIN_POP: i32 = 7;

pub fn seed_nodes(
    world_seed: i32,
    size: usize,
    cells: &mut [i32],
    landmass_id: &mut [i32],
    lake_id: &[i32],
    river_links: &FxHashMap<i32, Vec<Link>>,
    mouth_distance: &[i32],
    nodes: &mut Vec<GraphNode>,
) {
    let mut rng = ChaCha8Rng::seed_from_u64(u64::from(layer_seed(world_seed, "atlas_nodes")));
    let target = (PRIMARY_NODE_TARGET * size as i32 / SIZE_FULL)
        .max(size as i32 / 10)
        .clamp(12, 96) as usize;
    let attempts = target * 120;
    let mut occupied: FxHashMap<i32, bool> = FxHashMap::default();
    let mut collar = collar_cells(size) + 2;
    if collar >= size as i32 / 2 {
        collar = 2.max(size as i32 / 8);
    }
    let spacing = 3.max(size as i32 / 36);

    seed_settlement_nodes(
        world_seed,
        size,
        cells,
        landmass_id,
        lake_id,
        river_links,
        mouth_distance,
        settlement_target(size),
        &mut occupied,
        spacing,
        &mut rng,
        nodes,
    );

    for _ in 0..attempts {
        if nodes.len() >= target {
            break;
        }
        let ax = rng.gen_range(collar..size as i32 - collar);
        let az = rng.gen_range(collar..size as i32 - collar);
        try_add_node(
            world_seed,
            size,
            cells,
            landmass_id,
            lake_id,
            ax,
            az,
            &mut occupied,
            spacing,
            &mut rng,
            nodes,
        );
    }

    if nodes.len() < target {
        let step = spacing.max(4);
        let mut ax = collar + step / 2;
        while ax < size as i32 - collar && nodes.len() < target {
            let mut az = collar + step / 2;
            while az < size as i32 - collar && nodes.len() < target {
                try_add_node(
                    world_seed,
                    size,
                    cells,
                    landmass_id,
                    lake_id,
                    ax,
                    az,
                    &mut occupied,
                    spacing,
                    &mut rng,
                    nodes,
                );
                az += step;
            }
            ax += step;
        }
    }

    if nodes.len() < 2 {
        let want = target.max(4);
        'outer: for az in 0..size {
            for ax in 0..size {
                if nodes.len() >= want {
                    break 'outer;
                }
                try_add_node(
                    world_seed,
                    size,
                    cells,
                    landmass_id,
                    lake_id,
                    ax as i32,
                    az as i32,
                    &mut occupied,
                    (spacing / 2).max(2),
                    &mut rng,
                    nodes,
                );
            }
        }
    }

    seed_dungeon_nodes(
        world_seed,
        size,
        cells,
        landmass_id,
        lake_id,
        &mut occupied,
        spacing,
        nodes,
    );
}

fn settlement_target(size: usize) -> usize {
    ((SETTLEMENT_TARGET_FULL * size as i32) / SIZE_FULL)
        .max(4)
        .clamp(4, 220) as usize
}

fn dungeon_target(size: usize) -> usize {
    ((DUNGEON_TARGET_FULL * size as i32) / SIZE_FULL)
        .max(2)
        .clamp(2, 120) as usize
}

fn settlement_pop_for_site(site: SettlementSite, rng: &mut ChaCha8Rng) -> i32 {
    let (lo, hi) = match site {
        SettlementSite::RiverMouth => (10, 15),
        SettlementSite::Confluence => (8, 14),
        SettlementSite::NearRiver => (6, 12),
        SettlementSite::Inland => (4, 10),
    };
    let mut pop = rng.gen_range(lo..=hi);
    if site == SettlementSite::Inland && rng.gen::<f32>() < 0.06 {
        pop = rng.gen_range(10..=15);
    }
    if site == SettlementSite::RiverMouth && rng.gen::<f32>() < 0.08 {
        pop = rng.gen_range(6..=9);
    }
    pop.clamp(1, 15)
}

fn site_from_tier(tier: u8) -> SettlementSite {
    match tier {
        0 => SettlementSite::RiverMouth,
        1 => SettlementSite::Confluence,
        2 => SettlementSite::NearRiver,
        _ => SettlementSite::Inland,
    }
}

fn site_bucket_index(site: SettlementSite) -> usize {
    match site {
        SettlementSite::RiverMouth => 0,
        SettlementSite::Confluence => 1,
        SettlementSite::NearRiver => 2,
        SettlementSite::Inland => 3,
    }
}

fn pick_settlement_site(rng: &mut ChaCha8Rng, buckets: &[Vec<i32>; 4]) -> Option<SettlementSite> {
    let mut total = 0.0f32;
    for (tier, bucket) in buckets.iter().enumerate() {
        if bucket.is_empty() {
            continue;
        }
        total += site_from_tier(tier as u8).weight();
    }
    if total <= 0.0 {
        return None;
    }
    let mut cursor = rng.gen::<f32>() * total;
    for (tier, bucket) in buckets.iter().enumerate() {
        if bucket.is_empty() {
            continue;
        }
        let site = site_from_tier(tier as u8);
        cursor -= site.weight();
        if cursor <= 0.0 {
            return Some(site);
        }
    }
    None
}

fn seed_settlement_nodes(
    world_seed: i32,
    size: usize,
    cells: &mut [i32],
    landmass_id: &mut [i32],
    lake_id: &[i32],
    river_links: &FxHashMap<i32, Vec<Link>>,
    mouth_distance: &[i32],
    target: usize,
    occupied: &mut FxHashMap<i32, bool>,
    spacing: i32,
    rng: &mut ChaCha8Rng,
    nodes: &mut Vec<GraphNode>,
) {
    let mut buckets: [Vec<i32>; 4] = [vec![], vec![], vec![], vec![]];
    for az in 0..size {
        for ax in 0..size {
            let idx = az * size + ax;
            let packed = cells[idx];
            let biome = pack::biome(packed);
            if !biomes::is_land(biome) {
                continue;
            }
            if !cell_is_settlement_flat(ax as i32, az as i32, size, cells) {
                continue;
            }
            if near_lake(ax as i32, az as i32, size, lake_id)
                && biome == super::biomes::Biome::Wetland
            {
                continue;
            }
            let site =
                classify_settlement_site(ax as i32, az as i32, size, river_links, mouth_distance);
            buckets[site_bucket_index(site)].push(idx as i32);
        }
    }

    let max_attempts = target * 500;
    let mut placed = 0usize;
    for _ in 0..max_attempts {
        if placed >= target {
            break;
        }
        let Some(site) = pick_settlement_site(rng, &buckets) else {
            break;
        };
        let bucket = &buckets[site_bucket_index(site)];
        if bucket.is_empty() {
            continue;
        }
        let pick = bucket[rng.gen_range(0..bucket.len())];
        let ax = pick % size as i32;
        let az = pick / size as i32;
        if try_add_settlement(
            world_seed,
            size,
            cells,
            landmass_id,
            ax,
            az,
            site,
            occupied,
            spacing,
            rng,
            nodes,
        ) {
            placed += 1;
        }
    }
}

fn stamp_cell_population(cells: &mut [i32], idx: usize, pop: i32) {
    let packed = cells[idx];
    cells[idx] = pack::pack(
        pack::elevation(packed),
        pack::humidity(packed),
        pack::biome(packed),
        pack::relief(packed),
        pop,
    );
}

fn try_add_settlement(
    world_seed: i32,
    size: usize,
    cells: &mut [i32],
    landmass_id: &mut [i32],
    ax: i32,
    az: i32,
    site: SettlementSite,
    occupied: &mut FxHashMap<i32, bool>,
    spacing: i32,
    rng: &mut ChaCha8Rng,
    nodes: &mut Vec<GraphNode>,
) -> bool {
    if ax < 0 || az < 0 || ax as usize >= size || az as usize >= size {
        return false;
    }
    let idx = az as usize * size + ax as usize;
    if occupied.contains_key(&(idx as i32)) {
        return false;
    }
    let biome = pack::biome(cells[idx]);
    if !biomes::is_land(biome) || !cell_is_settlement_flat(ax, az, size, cells) {
        return false;
    }
    for node in nodes.iter() {
        if node.kind == NodeKind::Settlement
            && (node.ax - ax).abs() + (node.az - az).abs() < spacing
        {
            return false;
        }
    }

    let mut mass = landmass_id[idx];
    if mass < 0 {
        mass = 0;
        landmass_id[idx] = 0;
    }

    let pop = settlement_pop_for_site(site, rng);
    stamp_cell_population(cells, idx, pop);

    let kind = NodeKind::Settlement;
    let id = feature_hash(&[
        &world_seed.to_string(),
        "node",
        &ax.to_string(),
        &az.to_string(),
        &(kind as u8).to_string(),
    ]);
    nodes.push(GraphNode {
        id,
        kind,
        cell: idx as i32,
        ax,
        az,
        landmass: mass,
    });
    occupied.insert(idx as i32, true);
    true
}

fn try_add_node(
    world_seed: i32,
    size: usize,
    cells: &[i32],
    landmass_id: &mut [i32],
    lake_id: &[i32],
    ax: i32,
    az: i32,
    occupied: &mut FxHashMap<i32, bool>,
    spacing: i32,
    rng: &mut ChaCha8Rng,
    nodes: &mut Vec<GraphNode>,
) {
    if ax < 0 || az < 0 || ax as usize >= size || az as usize >= size {
        return;
    }
    let idx = az as usize * size + ax as usize;
    let biome = pack::biome(cells[idx]);
    if !biomes::is_land(biome) {
        return;
    }
    if occupied.contains_key(&(idx as i32)) {
        return;
    }
    let mut mass = landmass_id[idx];
    if mass < 0 {
        mass = 0;
        landmass_id[idx] = 0;
    }

    let kind = if biome == super::biomes::Biome::Coast {
        NodeKind::CoastalGate
    } else if near_lake(ax, az, size, lake_id) {
        NodeKind::LakeShore
    } else if pack::relief(cells[idx]) > 24 && pack::elevation(cells[idx]) > 140 {
        NodeKind::Pass
    } else if rng.gen::<f32>() < 0.08 {
        NodeKind::ClaimReserved
    } else {
        NodeKind::Landmark
    };

    for node in nodes.iter() {
        if (node.ax - ax).abs() + (node.az - az).abs() < spacing {
            return;
        }
    }

    let id = feature_hash(&[
        &world_seed.to_string(),
        "node",
        &ax.to_string(),
        &az.to_string(),
        &(kind as u8).to_string(),
    ]);
    nodes.push(GraphNode {
        id,
        kind,
        cell: idx as i32,
        ax,
        az,
        landmass: mass,
    });
    occupied.insert(idx as i32, true);
}

fn seed_dungeon_nodes(
    world_seed: i32,
    size: usize,
    cells: &[i32],
    landmass_id: &mut [i32],
    lake_id: &[i32],
    occupied: &mut FxHashMap<i32, bool>,
    spacing: i32,
    nodes: &mut Vec<GraphNode>,
) {
    let mut rng = ChaCha8Rng::seed_from_u64(u64::from(layer_seed(world_seed, "atlas_dungeons")));
    let target = dungeon_target(size);
    let dungeon_spacing = spacing.max(4).min(size as i32 / 6).max(3);
    let collar = (collar_cells(size) + 2).min(size as i32 / 4).max(2);
    let attempts = target * 400;
    for _ in 0..attempts {
        if nodes.iter().filter(|n| n.kind == NodeKind::Dungeon).count() >= target {
            return;
        }
        let ax = rng.gen_range(collar..size as i32 - collar);
        let az = rng.gen_range(collar..size as i32 - collar);
        try_add_dungeon(
            world_seed,
            size,
            cells,
            landmass_id,
            lake_id,
            ax,
            az,
            occupied,
            dungeon_spacing,
            nodes,
        );
    }
}

fn try_add_dungeon(
    world_seed: i32,
    size: usize,
    cells: &[i32],
    landmass_id: &mut [i32],
    lake_id: &[i32],
    ax: i32,
    az: i32,
    occupied: &mut FxHashMap<i32, bool>,
    spacing: i32,
    nodes: &mut Vec<GraphNode>,
) {
    if ax < 0 || az < 0 || ax as usize >= size || az as usize >= size {
        return;
    }
    let idx = az as usize * size + ax as usize;
    if occupied.contains_key(&(idx as i32)) {
        return;
    }
    let biome = pack::biome(cells[idx]);
    if !matches!(
        biome,
        biomes::Biome::Plains
            | biomes::Biome::Forest
            | biomes::Biome::Arid
            | biomes::Biome::Alpine
            | biomes::Biome::Tundra
    ) {
        return;
    }
    if near_lake(ax, az, size, lake_id) {
        return;
    }
    if pack::relief(cells[idx]) > 32 {
        return;
    }
    if pack::elevation(cells[idx]) < 30 {
        return;
    }
    if !cell_is_settlement_flat(ax, az, size, cells) {
        return;
    }
    if pack::population(cells[idx]) >= SETTLEMENT_MIN_POP {
        return;
    }
    for node in nodes.iter() {
        let need = if node.kind == NodeKind::Settlement {
            spacing + 1
        } else {
            spacing
        };
        if (node.ax - ax).abs() + (node.az - az).abs() < need {
            return;
        }
    }
    let mut mass = landmass_id[idx];
    if mass < 0 {
        mass = 0;
        landmass_id[idx] = 0;
    }
    let kind = NodeKind::Dungeon;
    let id = feature_hash(&[
        &world_seed.to_string(),
        "node",
        &ax.to_string(),
        &az.to_string(),
        &(kind as u8).to_string(),
    ]);
    nodes.push(GraphNode {
        id,
        kind,
        cell: idx as i32,
        ax,
        az,
        landmass: mass,
    });
    occupied.insert(idx as i32, true);
}

fn near_lake(ax: i32, az: i32, size: usize, lake_id: &[i32]) -> bool {
    for dz in -2..=2 {
        for dx in -2..=2 {
            let nx = ax + dx;
            let nz = az + dz;
            if nx >= 0
                && nz >= 0
                && (nx as usize) < size
                && (nz as usize) < size
                && lake_id[nz as usize * size + nx as usize] >= 0
            {
                return true;
            }
        }
    }
    false
}
