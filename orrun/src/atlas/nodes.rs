//! Typed road / settlement node seeding.

use rand::Rng;
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use rustc_hash::FxHashMap;

use super::biomes;
use super::features::NodeKind;
use super::landmask::collar_cells;
use super::pack;
use super::population::{cell_is_settlement_flat, flatness_fitness};
use super::types::GraphNode;
use super::{feature_hash, layer_seed};

const SIZE_FULL: i32 = 1000;
const PRIMARY_NODE_TARGET: i32 = 72;
const SETTLEMENT_MIN_POP: i32 = 7;

pub fn seed_nodes(
    world_seed: i32,
    size: usize,
    cells: &[i32],
    landmass_id: &mut [i32],
    lake_id: &[i32],
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
        (target * 6 / 10).clamp(2, target),
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

fn seed_settlement_nodes(
    world_seed: i32,
    size: usize,
    cells: &[i32],
    landmass_id: &mut [i32],
    lake_id: &[i32],
    budget: usize,
    occupied: &mut FxHashMap<i32, bool>,
    spacing: i32,
    rng: &mut ChaCha8Rng,
    nodes: &mut Vec<GraphNode>,
) {
    let peaks = population_peaks(size, cells);
    for &(_pop, idx) in &peaks {
        if nodes.len() >= budget {
            break;
        }
        try_add_node(
            world_seed,
            size,
            cells,
            landmass_id,
            lake_id,
            idx % size as i32,
            idx / size as i32,
            occupied,
            spacing,
            rng,
            nodes,
        );
    }

    let mut hosted: FxHashMap<i32, bool> = FxHashMap::default();
    for node in nodes.iter() {
        if node.kind == NodeKind::Settlement {
            hosted.insert(node.landmass, true);
        }
    }

    struct Best {
        pop: i32,
        fitness: f32,
        idx: i32,
    }
    let mut best_flat: FxHashMap<i32, Best> = FxHashMap::default();
    for az in 0..size {
        for ax in 0..size {
            let idx = az * size + ax;
            let pop = pack::population(cells[idx]);
            if pop < SETTLEMENT_MIN_POP {
                continue;
            }
            if !cell_is_settlement_flat(ax as i32, az as i32, size, cells) {
                continue;
            }
            let mass = landmass_id[idx];
            if mass < 0 || hosted.contains_key(&mass) {
                continue;
            }
            let fitness = flatness_fitness(ax as i32, az as i32, size, cells);
            let take = match best_flat.get(&mass) {
                None => true,
                Some(prev) => {
                    pop > prev.pop
                        || (pop == prev.pop
                            && (fitness > prev.fitness
                                || ((fitness - prev.fitness).abs() < 1e-6
                                    && (idx as i32) < prev.idx)))
                }
            };
            if take {
                best_flat.insert(
                    mass,
                    Best {
                        pop,
                        fitness,
                        idx: idx as i32,
                    },
                );
            }
        }
    }

    for (mass, pick) in best_flat {
        if hosted.contains_key(&mass) {
            continue;
        }
        let before = nodes.len();
        try_add_node(
            world_seed,
            size,
            cells,
            landmass_id,
            lake_id,
            pick.idx % size as i32,
            pick.idx / size as i32,
            occupied,
            (spacing / 2).max(2),
            rng,
            nodes,
        );
        if nodes.len() > before {
            hosted.insert(mass, true);
        }
    }
}

fn population_peaks(size: usize, cells: &[i32]) -> Vec<(i32, i32)> {
    let mut peaks = Vec::new();
    let radius = 2i32;
    for az in 0..size {
        for ax in 0..size {
            let idx = az * size + ax;
            let pop = pack::population(cells[idx]);
            if pop < SETTLEMENT_MIN_POP {
                continue;
            }
            if !cell_is_settlement_flat(ax as i32, az as i32, size, cells) {
                continue;
            }
            let mut is_peak = true;
            'scan: for dz in -radius..=radius {
                for dx in -radius..=radius {
                    if dx == 0 && dz == 0 {
                        continue;
                    }
                    let nx = ax as i32 + dx;
                    let nz = az as i32 + dz;
                    if nx < 0 || nz < 0 || nx as usize >= size || nz as usize >= size {
                        continue;
                    }
                    let nb = nz as usize * size + nx as usize;
                    let npop = pack::population(cells[nb]);
                    if npop > pop || (npop == pop && (nb as i32) < idx as i32) {
                        is_peak = false;
                        break 'scan;
                    }
                }
            }
            if is_peak {
                peaks.push((pop, idx as i32));
            }
        }
    }
    peaks.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| {
                let fa = flatness_fitness(a.1 % size as i32, a.1 / size as i32, size, cells);
                let fb = flatness_fitness(b.1 % size as i32, b.1 / size as i32, size, cells);
                fb.partial_cmp(&fa).unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.1.cmp(&b.1))
    });
    peaks
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

    let kind = if pack::population(cells[idx]) >= SETTLEMENT_MIN_POP
        && cell_is_settlement_flat(ax, az, size, cells)
    {
        NodeKind::Settlement
    } else if biome == super::biomes::Biome::Coast {
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
    let target = (8 * size as i32 / SIZE_FULL).max(2).clamp(2, 16) as usize;
    let dungeon_spacing = spacing.max(6);
    let collar = (collar_cells(size) + 3).min(size as i32 / 4).max(2);
    let attempts = target * 80;
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
    if pack::relief(cells[idx]) > 28 {
        return;
    }
    if pack::elevation(cells[idx]) < 50 {
        return;
    }
    if pack::population(cells[idx]) >= SETTLEMENT_MIN_POP {
        return;
    }
    for node in nodes.iter() {
        let need = if node.kind == NodeKind::Settlement {
            spacing + 2
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
