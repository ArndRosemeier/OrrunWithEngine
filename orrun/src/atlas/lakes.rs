//! Atlas lake basins with spill rims.

use engine::proc::Noise;
use rand::Rng;
use rand_chacha::ChaCha8Rng;
use rand::SeedableRng;
use rustc_hash::FxHashSet;

use super::landmask::collar_cells;
use super::types::Lake;
use super::{layer_seed, lerp, cardinal, NEIGHBOR_DX, NEIGHBOR_DZ};
use super::pack;

const SIZE_FULL: i32 = 1000;
const LAKE_MIN_CELLS: usize = 6;
const LAKE_MAX_CELLS_FULL: usize = 450;

pub struct LakeScratch {
    pub lakes: Vec<Lake>,
    pub lake_id: Vec<i32>,
}

impl LakeScratch {
    pub fn new(count: usize) -> Self {
        Self {
            lakes: Vec::new(),
            lake_id: vec![-1; count],
        }
    }
}

fn lake_max_cells(size: usize) -> usize {
    (size * size / 90).clamp(48, LAKE_MAX_CELLS_FULL)
}

pub fn build_lakes(
    world_seed: i32,
    size: usize,
    land: &[u8],
    elev_code: &mut [u8],
    scratch: &mut LakeScratch,
) {
    let want = 4.max(22 * size as i32 / SIZE_FULL) as usize;
    let mut collar = collar_cells(size) + 4;
    if collar >= size as i32 / 2 {
        collar = 2.max(size as i32 / 8);
    }
    let mut rng = ChaCha8Rng::seed_from_u64(u64::from(layer_seed(world_seed, "atlas_lake_seeds")));
    let step = 10.max(size as i32 / want.max(1) as i32);
    let mut ax = collar + step / 2;
    while ax < size as i32 - collar && scratch.lakes.len() < want {
        let mut az = collar + step / 2;
        while az < size as i32 - collar && scratch.lakes.len() < want {
            let jx = (ax + rng.gen_range(-step / 3..=step / 3)).clamp(collar, size as i32 - collar - 1);
            let jz = (az + rng.gen_range(-step / 3..=step / 3)).clamp(collar, size as i32 - collar - 1);
            try_place_lake(world_seed, size, jx, jz, land, elev_code, scratch, &mut rng);
            az += step;
        }
        ax += step;
    }
    let mut scans = 0;
    while scratch.lakes.len() < want && scans < size * 4 {
        scans += 1;
        try_place_lake(
            world_seed,
            size,
            rng.gen_range(collar..size as i32 - collar),
            rng.gen_range(collar..size as i32 - collar),
            land,
            elev_code,
            scratch,
            &mut rng,
        );
    }
}

fn in_bounds(ax: i32, az: i32, size: usize) -> bool {
    ax >= 0 && az >= 0 && (ax as usize) < size && (az as usize) < size
}

fn index_of(ax: i32, az: i32, size: usize) -> usize {
    az as usize * size + ax as usize
}

fn try_place_lake(
    world_seed: i32,
    size: usize,
    ax: i32,
    az: i32,
    land: &[u8],
    elev_code: &mut [u8],
    scratch: &mut LakeScratch,
    rng: &mut ChaCha8Rng,
) {
    if !in_bounds(ax, az, size) {
        return;
    }
    let idx = index_of(ax, az, size);
    if land[idx] == 0 || scratch.lake_id[idx] >= 0 {
        return;
    }
    if !is_local_low(ax, az, size, elev_code, land) && rng.gen::<f32>() > 0.35 {
        return;
    }

    let max_cells = lake_max_cells(size);
    let t = rng.gen::<f32>().powf(2.2);
    let lo = LAKE_MIN_CELLS;
    let hi = max_cells;
    let target = (lerp(lo as f32, hi as f32, t) as usize).clamp(lo, hi);
    let scale = size.max(64) as f32 / 96.0;
    let mut rx = rng.gen_range(1.2..7.5) * scale * lerp(0.55, 1.4, t);
    let mut rz = rng.gen_range(1.2..7.5) * scale * lerp(0.55, 1.4, t);
    if t > 0.30 || rng.gen::<f32>() < 0.4 {
        if rng.gen::<f32>() < 0.5 {
            rx *= rng.gen_range(1.8..3.2);
            rz *= rng.gen_range(0.30..0.65);
        } else {
            rz *= rng.gen_range(1.8..3.2);
            rx *= rng.gen_range(0.30..0.65);
        }
    }
    let rot = rng.gen::<f32>() * std::f32::consts::TAU;
    let cos_r = rot.cos();
    let sin_r = rot.sin();
    let shape_name = format!("atlas_lake_shape_{ax}_{az}");
    let noise = Noise::new(layer_seed(world_seed, &shape_name));
    let depth_carve = rng.gen_range(3..=14);

    let mut basin = Vec::new();
    let span = (rx.max(rz) * 1.6).ceil() as i32 + 2;
    for dz in -span..=span {
        for dx in -span..=span {
            let nx = ax + dx;
            let nz = az + dz;
            if !in_bounds(nx, nz, size) {
                continue;
            }
            let nb = index_of(nx, nz, size);
            if land[nb] == 0 || scratch.lake_id[nb] >= 0 {
                continue;
            }
            let lx = dx as f32 * cos_r + dz as f32 * sin_r;
            let lz = -dx as f32 * sin_r + dz as f32 * cos_r;
            let mut ellipse = (lx * lx) / (rx * rx) + (lz * lz) / (rz * rz);
            ellipse += noise.sample2(nx as f32 * 0.35, nz as f32 * 0.35) * 0.45;
            if ellipse <= 1.0 {
                basin.push(nb as i32);
            }
        }
    }

    if basin.len() < target {
        grow_lake_basin(
            &mut basin,
            target,
            size,
            land,
            elev_code,
            &scratch.lake_id,
            elev_code[idx] as i32 + depth_carve,
        );
    } else if basin.len() > target {
        basin = trim_lake_basin(
            &basin,
            target,
            size,
            ax,
            az,
            rx,
            rz,
            cos_r,
            sin_r,
            &noise,
        );
        if basin.len() < target {
            grow_lake_basin(
                &mut basin,
                target,
                size,
                land,
                elev_code,
                &scratch.lake_id,
                elev_code[idx] as i32 + depth_carve,
            );
        }
    }

    if basin.len() < LAKE_MIN_CELLS || basin.len() > max_cells {
        return;
    }

    let mut spill_cell = -1i32;
    let mut spill_out = -1i32;
    let mut rim_min = 999i32;
    let basin_set: FxHashSet<i32> = basin.iter().copied().collect();
    for &cell in &basin {
        let cx = cell % size as i32;
        let cz = cell / size as i32;
        for k in 0..4 {
            let (dx, dz) = cardinal(k);
            let nx = cx + dx;
            let nz = cz + dz;
            if !in_bounds(nx, nz, size) {
                continue;
            }
            let nb = index_of(nx, nz, size) as i32;
            if land[nb as usize] == 0 {
                if (elev_code[nb as usize] as i32) < rim_min {
                    rim_min = elev_code[nb as usize] as i32;
                    spill_cell = cell;
                    spill_out = nb;
                }
                continue;
            }
            if scratch.lake_id[nb as usize] >= 0 || basin_set.contains(&nb) {
                continue;
            }
            let ne = elev_code[nb as usize] as i32;
            if ne < rim_min {
                rim_min = ne;
                spill_cell = cell;
                spill_out = nb;
            }
        }
    }

    if spill_cell < 0 {
        spill_cell = basin[0];
        rim_min = elev_code[idx] as i32 + 6;
    }
    let surface_code = (rim_min - 1).clamp(34, 250);
    let lake = Lake {
        id: scratch.lakes.len() as i32,
        cells: basin.clone(),
        spill_cell,
        surface_code,
        surface_z: pack::elevation_to_metres(surface_code),
    };
    let lake_id = lake.id;
    scratch.lakes.push(lake);
    for &cell in &basin {
        scratch.lake_id[cell as usize] = lake_id;
        let cx = cell % size as i32;
        let cz = cell / size as i32;
        let dist = (((cx - ax) * (cx - ax) + (cz - az) * (cz - az)) as f32).sqrt();
        let bed = surface_code - 2 - (depth_carve - dist as i32).clamp(0, depth_carve);
        elev_code[cell as usize] = bed.clamp(33, surface_code - 1) as u8;
    }
    if spill_out >= 0
        && land[spill_out as usize] != 0
        && scratch.lake_id[spill_out as usize] < 0
    {
        elev_code[spill_out as usize] = (elev_code[spill_out as usize] as i32)
            .min(surface_code + 1)
            .clamp(33, 255) as u8;
    }
}

fn is_local_low(ax: i32, az: i32, size: usize, elev_code: &[u8], land: &[u8]) -> bool {
    let e = elev_code[index_of(ax, az, size)] as i32;
    let mut lower_or_eq = 0;
    for k in 0..8 {
        let nx = ax + NEIGHBOR_DX[k];
        let nz = az + NEIGHBOR_DZ[k];
        if !in_bounds(nx, nz, size) {
            continue;
        }
        let nb = index_of(nx, nz, size);
        if land[nb] == 0 {
            continue;
        }
        if (elev_code[nb] as i32) <= e {
            lower_or_eq += 1;
        }
    }
    lower_or_eq <= 2
}

fn grow_lake_basin(
    basin: &mut Vec<i32>,
    target: usize,
    size: usize,
    land: &[u8],
    elev_code: &[u8],
    lake_id: &[i32],
    max_elev: i32,
) {
    let mut in_basin: FxHashSet<i32> = basin.iter().copied().collect();
    let mut guard = 0;
    while basin.len() < target && guard < target * 8 {
        guard += 1;
        let mut best = -1i32;
        let mut best_e = 999i32;
        for &cell in basin.iter() {
            let cx = cell % size as i32;
            let cz = cell / size as i32;
            for k in 0..4 {
                let (dx, dz) = cardinal(k);
                let nx = cx + dx;
                let nz = cz + dz;
                if !in_bounds(nx, nz, size) {
                    continue;
                }
                let nb = index_of(nx, nz, size) as i32;
                if in_basin.contains(&nb) || land[nb as usize] == 0 || lake_id[nb as usize] >= 0 {
                    continue;
                }
                let ne = elev_code[nb as usize] as i32;
                if ne > max_elev {
                    continue;
                }
                if ne < best_e {
                    best_e = ne;
                    best = nb;
                }
            }
        }
        if best < 0 {
            break;
        }
        basin.push(best);
        in_basin.insert(best);
    }
}

fn trim_lake_basin(
    basin: &[i32],
    target: usize,
    size: usize,
    ax: i32,
    az: i32,
    rx: f32,
    rz: f32,
    cos_r: f32,
    sin_r: f32,
    noise: &Noise,
) -> Vec<i32> {
    let count = size * size;
    let mut allowed = vec![0u8; count];
    let mut state = vec![0u8; count];
    for &cell in basin {
        allowed[cell as usize] = 1;
    }
    let mut start = index_of(ax, az, size);
    if allowed[start] == 0 {
        let mut nearest_d2 = i32::MAX;
        for &cell in basin {
            let cx = cell % size as i32;
            let cz = cell / size as i32;
            let d2 = (cx - ax) * (cx - ax) + (cz - az) * (cz - az);
            if d2 < nearest_d2 {
                nearest_d2 = d2;
                start = cell as usize;
            }
        }
    }
    let phase = {
        let h = layer_seed(ax, &format!("{az}"));
        (h % 1000) as f32 / 1000.0 * std::f32::consts::TAU
    };
    let mut frontier = vec![start as i32];
    state[start] = 1;
    let mut kept = Vec::new();
    while !frontier.is_empty() && kept.len() < target {
        let mut best_i = 0;
        let mut best_score = 1.0e12;
        for (i, &candidate) in frontier.iter().enumerate() {
            let score = lake_trim_score(candidate, size, ax, az, rx, rz, cos_r, sin_r, noise, phase);
            if score < best_score {
                best_score = score;
                best_i = i;
            }
        }
        let cell = frontier.swap_remove(best_i);
        state[cell as usize] = 2;
        kept.push(cell);
        let cx = cell % size as i32;
        let cz = cell / size as i32;
        for k in 0..4 {
            let (dx, dz) = cardinal(k);
            let nx = cx + dx;
            let nz = cz + dz;
            if !in_bounds(nx, nz, size) {
                continue;
            }
            let nb = index_of(nx, nz, size);
            if allowed[nb] == 0 || state[nb] != 0 {
                continue;
            }
            state[nb] = 1;
            frontier.push(nb as i32);
        }
    }
    kept
}

fn lake_trim_score(
    cell: i32,
    size: usize,
    ax: i32,
    az: i32,
    rx: f32,
    rz: f32,
    cos_r: f32,
    sin_r: f32,
    noise: &Noise,
    phase: f32,
) -> f32 {
    let cx = cell % size as i32;
    let cz = cell / size as i32;
    let dx = (cx - ax) as f32;
    let dz = (cz - az) as f32;
    let lx = dx * cos_r + dz * sin_r;
    let lz = -dx * sin_r + dz * cos_r;
    let ellipse = (lx * lx) / (rx * rx) + (lz * lz) / (rz * rz);
    let shore_noise = noise.sample2(cx as f32 * 0.35, cz as f32 * 0.35) * 0.48;
    let lobes = (lz.atan2(lx) * 3.0 + phase).sin() * 0.16;
    ellipse + shore_noise + lobes
}

/// Convert sea-connected lake basins into ocean before packing.
pub fn merge_coastal_lakes_into_ocean(
    size: usize,
    land: &mut [u8],
    elev_code: &mut [u8],
    scratch: &mut LakeScratch,
) {
    if scratch.lakes.is_empty() {
        return;
    }
    let n = scratch.lakes.len();
    let mut coastal = vec![0u8; n];

    for lake in &scratch.lakes {
        for &cell in &lake.cells {
            let ax = cell % size as i32;
            let az = cell / size as i32;
            for k in 0..4 {
                let (dx, dz) = cardinal(k);
                let nx = ax + dx;
                let nz = az + dz;
                if !in_bounds(nx, nz, size) {
                    coastal[lake.id as usize] = 1;
                    break;
                }
                let nb = index_of(nx, nz, size);
                if land[nb] == 0 && scratch.lake_id[nb] < 0 {
                    coastal[lake.id as usize] = 1;
                    break;
                }
            }
            if coastal[lake.id as usize] != 0 {
                break;
            }
        }
    }

    let mut changed = true;
    while changed {
        changed = false;
        for lake in &scratch.lakes {
            if coastal[lake.id as usize] != 0 {
                continue;
            }
            'outer: for &cell in &lake.cells {
                let ax = cell % size as i32;
                let az = cell / size as i32;
                for k in 0..4 {
                    let (dx, dz) = cardinal(k);
                    let nx = ax + dx;
                    let nz = az + dz;
                    if !in_bounds(nx, nz, size) {
                        continue;
                    }
                    let other_id = scratch.lake_id[index_of(nx, nz, size)];
                    if other_id >= 0
                        && other_id != lake.id
                        && coastal[other_id as usize] != 0
                    {
                        coastal[lake.id as usize] = 1;
                        changed = true;
                        break 'outer;
                    }
                }
            }
        }
    }

    let mut kept = Vec::new();
    for lake in scratch.lakes.drain(..) {
        if coastal[lake.id as usize] != 0 {
            for &cell in &lake.cells {
                land[cell as usize] = 0;
                scratch.lake_id[cell as usize] = -1;
                elev_code[cell as usize] = elev_code[cell as usize].min(32);
            }
            continue;
        }
        let mut lake = lake;
        lake.id = kept.len() as i32;
        for &cell in &lake.cells {
            scratch.lake_id[cell as usize] = lake.id;
        }
        kept.push(lake);
    }
    scratch.lakes = kept;
}

/// Water cells not reachable from the atlas border via open ocean become lakes.
///
/// The landmask can leave enclosed `land==0` pockets inland; without this step
/// they classify as ocean and grow fake Coast rings.
pub fn promote_inland_seas_to_lakes(
    size: usize,
    land: &[u8],
    elev_code: &mut [u8],
    scratch: &mut LakeScratch,
) {
    let count = size * size;
    let mut ocean = vec![false; count];
    let mut stack = Vec::new();

    for az in 0..size {
        for ax in 0..size {
            if ax != 0 && az != 0 && ax + 1 != size && az + 1 != size {
                continue;
            }
            let idx = az * size + ax;
            if land[idx] == 0 && scratch.lake_id[idx] < 0 {
                ocean[idx] = true;
                stack.push(idx);
            }
        }
    }

    while let Some(idx) = stack.pop() {
        let ax = (idx % size) as i32;
        let az = (idx / size) as i32;
        for k in 0..4 {
            let (dx, dz) = cardinal(k);
            let nx = ax + dx;
            let nz = az + dz;
            if !in_bounds(nx, nz, size) {
                continue;
            }
            let nb = index_of(nx, nz, size);
            if ocean[nb] || land[nb] != 0 || scratch.lake_id[nb] >= 0 {
                continue;
            }
            ocean[nb] = true;
            stack.push(nb);
        }
    }

    let mut seen = vec![false; count];
    for start in 0..count {
        if land[start] != 0
            || scratch.lake_id[start] >= 0
            || ocean[start]
            || seen[start]
        {
            continue;
        }
        let mut basin = Vec::new();
        stack.clear();
        stack.push(start);
        seen[start] = true;
        while let Some(idx) = stack.pop() {
            basin.push(idx as i32);
            let ax = (idx % size) as i32;
            let az = (idx / size) as i32;
            for k in 0..4 {
                let (dx, dz) = cardinal(k);
                let nx = ax + dx;
                let nz = az + dz;
                if !in_bounds(nx, nz, size) {
                    continue;
                }
                let nb = index_of(nx, nz, size);
                if seen[nb]
                    || land[nb] != 0
                    || scratch.lake_id[nb] >= 0
                    || ocean[nb]
                {
                    continue;
                }
                seen[nb] = true;
                stack.push(nb);
            }
        }
        commit_inland_sea_as_lake(size, &basin, land, elev_code, scratch);
    }
}

fn commit_inland_sea_as_lake(
    size: usize,
    basin: &[i32],
    land: &[u8],
    elev_code: &mut [u8],
    scratch: &mut LakeScratch,
) {
    if basin.is_empty() {
        return;
    }
    let basin_set: FxHashSet<i32> = basin.iter().copied().collect();
    let mut spill_cell = basin[0];
    let mut rim_min = 999i32;
    for &cell in basin {
        let cx = cell % size as i32;
        let cz = cell / size as i32;
        for k in 0..4 {
            let (dx, dz) = cardinal(k);
            let nx = cx + dx;
            let nz = cz + dz;
            if !in_bounds(nx, nz, size) {
                continue;
            }
            let nb = index_of(nx, nz, size);
            if basin_set.contains(&(nb as i32)) {
                continue;
            }
            // Spill over dry land rim only.
            if land[nb] == 0 || scratch.lake_id[nb] >= 0 {
                continue;
            }
            let ne = elev_code[nb] as i32;
            if ne < rim_min {
                rim_min = ne;
                spill_cell = cell;
            }
        }
    }
    if rim_min >= 999 {
        // Fully enclosed by other lakes / degenerate — invent a modest rim.
        rim_min = (elev_code[basin[0] as usize] as i32 + 8).clamp(40, 180);
    }
    let surface_code = (rim_min - 1).clamp(34, 250);
    let lake_id = scratch.lakes.len() as i32;
    scratch.lakes.push(Lake {
        id: lake_id,
        cells: basin.to_vec(),
        spill_cell,
        surface_code,
        surface_z: pack::elevation_to_metres(surface_code),
    });
    let depth = (surface_code - 34).clamp(2, 12);
    for &cell in basin {
        scratch.lake_id[cell as usize] = lake_id;
        let bed = (surface_code - depth).clamp(33, surface_code - 1);
        elev_code[cell as usize] = bed as u8;
    }
}

pub fn label_landmasses(
    size: usize,
    land: &[u8],
    lake_id: &[i32],
    landmass_id: &mut [i32],
) {
    let count = size * size;
    let mut seen = vec![0u8; count];
    let mut stack = Vec::new();
    let mut mass = 0i32;
    for start in 0..count {
        if land[start] == 0 || lake_id[start] >= 0 || seen[start] != 0 {
            continue;
        }
        stack.clear();
        stack.push(start);
        seen[start] = 1;
        let mut cells_in_mass = 0;
        while let Some(cell) = stack.pop() {
            landmass_id[cell] = mass;
            cells_in_mass += 1;
            let cx = (cell % size) as i32;
            let cz = (cell / size) as i32;
            for k in 0..4 {
                let (dx, dz) = cardinal(k);
                let nx = cx + dx;
                let nz = cz + dz;
                if !in_bounds(nx, nz, size) {
                    continue;
                }
                let nb = index_of(nx, nz, size);
                if seen[nb] != 0 || land[nb] == 0 || lake_id[nb] >= 0 {
                    continue;
                }
                seen[nb] = 1;
                stack.push(nb);
            }
        }
        if cells_in_mass > 0 {
            mass += 1;
        }
    }
}
