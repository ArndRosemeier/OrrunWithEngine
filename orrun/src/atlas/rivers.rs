//! Major river graph via priority-flood drainage.

use rand::Rng;
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use rustc_hash::FxHashMap;

use super::biomes::{self, Biome};
use super::features::{edge_key, edge_owner, Dir, Kind};
use super::pack;
use super::types::{Endpoint, Lake, Link, Port};
use super::{cardinal, feature_hash, layer_seed, lerp};

const SIZE_FULL: i32 = 1000;
const RIVER_ACCUM_THRESHOLD: f32 = 180.0;

pub struct RiverGraph {
    pub ports: FxHashMap<i32, Vec<Port>>,
    pub links: FxHashMap<i32, Vec<Link>>,
    pub receiver: Vec<i32>,
}

impl RiverGraph {
    pub fn new(count: usize) -> Self {
        Self {
            ports: FxHashMap::default(),
            links: FxHashMap::default(),
            receiver: vec![-1; count],
        }
    }
}

pub fn build_rivers(
    world_seed: i32,
    size: usize,
    cells: &mut [i32],
    elev_code: &mut [u8],
    lake_id: &[i32],
    lakes: &[Lake],
    graph: &mut RiverGraph,
) {
    let count = size * size;
    let mut accum = vec![1.0f32; count];
    for r in &mut graph.receiver {
        *r = -1;
    }

    let flood_order = priority_flood_fill(
        world_seed,
        size,
        cells,
        elev_code,
        lake_id,
        lakes,
        &mut graph.receiver,
    );
    rewrite_cell_elevations(cells, elev_code);

    for &idx in flood_order.iter().rev() {
        let down = graph.receiver[idx];
        if down >= 0 && biomes::is_land(pack::biome(cells[down as usize])) {
            accum[down as usize] += accum[idx];
        }
    }

    let threshold = 12.0f32.max(RIVER_ACCUM_THRESHOLD * size as f32 / SIZE_FULL as f32);
    let mut channel = vec![0u8; count];
    for i in 0..count {
        if biomes::is_land(pack::biome(cells[i])) && accum[i] >= threshold {
            channel[i] = 1;
        }
    }

    for i in 0..count {
        if channel[i] == 0 {
            continue;
        }
        let mut walk = i as i32;
        let mut guard = 0;
        while walk >= 0 && guard < count {
            guard += 1;
            channel[walk as usize] = 1;
            let down = graph.receiver[walk as usize];
            if down < 0 {
                break;
            }
            let down_biome = pack::biome(cells[down as usize]);
            if matches!(down_biome, Biome::Ocean | Biome::Lake) {
                break;
            }
            walk = down;
        }
    }

    let mut river_serial = 0i32;
    for az in 0..size {
        for ax in 0..size {
            let idx = az * size + ax;
            if channel[idx] == 0 {
                continue;
            }
            let down = graph.receiver[idx];
            if down < 0 {
                continue;
            }
            let dx = (down % size as i32) - ax as i32;
            let dz = (down / size as i32) - az as i32;
            if dx.abs() + dz.abs() != 1 {
                continue;
            }
            let Some(dir) = dir_from_delta(dx, dz) else {
                continue;
            };

            let down_biome = pack::biome(cells[down as usize]);
            let feature_class = ((accum[idx].max(2.0).ln() / 3.0f32.ln()) as i32).clamp(1, 4);
            let mut surface_z = pack::elevation_to_metres(elev_code[idx] as i32);
            let out_endpoint = match down_biome {
                Biome::Ocean => Endpoint::ocean(),
                Biome::Lake => {
                    let lid = lake_id[down as usize].max(0);
                    surface_z = lakes[lid as usize].surface_z;
                    Endpoint::lake(lid)
                }
                _ => {
                    if channel[down as usize] == 0 {
                        continue;
                    }
                    let port = ensure_river_port(
                        world_seed,
                        size,
                        ax as i32,
                        az as i32,
                        dir,
                        feature_class,
                        surface_z,
                        river_serial,
                        &mut graph.ports,
                    );
                    river_serial += 1;
                    Endpoint::edge_port(edge_key(ax as i32, az as i32, dir, size), port.id)
                }
            };

            let mut inflows = 0;
            for k in 0..4 {
                let (ndx, ndz) = cardinal(k);
                let nx = ax as i32 + ndx;
                let nz = az as i32 + ndz;
                if nx < 0 || nz < 0 || nx as usize >= size || nz as usize >= size {
                    continue;
                }
                let nb = nz as usize * size + nx as usize;
                if channel[nb] == 0 || graph.receiver[nb] != idx as i32 {
                    continue;
                }
                let Some(idir) = dir_from_delta(ax as i32 - nx, az as i32 - nz) else {
                    continue;
                };
                let in_port = ensure_river_port(
                    world_seed,
                    size,
                    nx,
                    nz,
                    idir,
                    feature_class,
                    surface_z,
                    river_serial,
                    &mut graph.ports,
                );
                river_serial += 1;
                let in_endpoint = Endpoint::edge_port(edge_key(nx, nz, idir, size), in_port.id);
                add_river_link(
                    world_seed,
                    idx as i32,
                    in_endpoint,
                    out_endpoint,
                    feature_class,
                    &mut graph.links,
                );
                inflows += 1;
            }

            if inflows == 0 {
                let source = Endpoint::node(-1 - idx as i32);
                add_river_link(
                    world_seed,
                    idx as i32,
                    source,
                    out_endpoint,
                    feature_class,
                    &mut graph.links,
                );
            }
        }
    }
}

fn rewrite_cell_elevations(cells: &mut [i32], elev_code: &[u8]) {
    for i in 0..cells.len() {
        let packed = cells[i];
        cells[i] = pack::pack(
            elev_code[i] as i32,
            pack::humidity(packed),
            pack::biome(packed),
            pack::relief(packed),
            pack::population(packed),
        );
    }
}

fn priority_flood_fill(
    world_seed: i32,
    size: usize,
    cells: &[i32],
    elev_code: &mut [u8],
    lake_id: &[i32],
    lakes: &[Lake],
    receiver: &mut [i32],
) -> Vec<usize> {
    let count = size * size;
    let mut closed = vec![false; count];
    let mut flood_order = Vec::new();
    let mut buckets: Vec<Vec<i32>> = (0..256).map(|_| Vec::new()).collect();
    let mut open_min = 256i32;

    for i in 0..count {
        let biome = pack::biome(cells[i]);
        if matches!(biome, Biome::Ocean | Biome::Lake) {
            let mut e = (elev_code[i] as i32).clamp(0, 255);
            if biome == Biome::Lake && lake_id[i] >= 0 {
                e = lakes[lake_id[i] as usize].surface_code;
                elev_code[i] = e as u8;
            }
            buckets[e as usize].push(i as i32);
            closed[i] = true;
            open_min = open_min.min(e);
        }
    }

    while open_min < 256 {
        while open_min < 256 && buckets[open_min as usize].is_empty() {
            open_min += 1;
        }
        if open_min >= 256 {
            break;
        }
        let cell = buckets[open_min as usize].pop().expect("bucket non-empty");
        let ax = cell % size as i32;
        let az = cell / size as i32;
        let ce = elev_code[cell as usize] as i32;
        if biomes::is_land(pack::biome(cells[cell as usize])) {
            flood_order.push(cell as usize);
        }
        let first_dir = {
            let key = format!("{world_seed}:flood:{ax}:{az}");
            (feature_hash(&[&key]) as u32 % 4) as usize
        };
        for step in 0..4 {
            let k = (first_dir + step) % 4;
            let (dx, dz) = cardinal(k);
            let nx = ax + dx;
            let nz = az + dz;
            if nx < 0 || nz < 0 || nx as usize >= size || nz as usize >= size {
                continue;
            }
            let nb = nz as usize * size + nx as usize;
            if closed[nb] {
                continue;
            }
            let biome = pack::biome(cells[nb]);
            if matches!(biome, Biome::Ocean | Biome::Lake) {
                closed[nb] = true;
                continue;
            }
            let ne = (elev_code[nb] as i32).max(ce);
            elev_code[nb] = ne as u8;
            receiver[nb] = cell;
            closed[nb] = true;
            buckets[ne as usize].push(nb as i32);
            open_min = open_min.min(ne);
        }
    }
    flood_order
}

fn dir_from_delta(dx: i32, dz: i32) -> Option<Dir> {
    if dx == 0 && dz == 0 {
        return None;
    }
    if dx.abs() >= dz.abs() {
        Some(if dx > 0 { Dir::East } else { Dir::West })
    } else {
        Some(if dz > 0 { Dir::South } else { Dir::North })
    }
}

fn ensure_river_port(
    world_seed: i32,
    size: usize,
    ax: i32,
    az: i32,
    dir: Dir,
    feature_class: i32,
    surface_z: i32,
    serial: i32,
    ports: &mut FxHashMap<i32, Vec<Port>>,
) -> Port {
    let key = edge_key(ax, az, dir, size);
    let entry = ports.entry(key).or_default();
    if !entry.is_empty() {
        let mut best_i = 0;
        for i in 0..entry.len() {
            if entry[i].feature_class < feature_class {
                entry[i].feature_class = feature_class;
            }
            if entry[i].surface_z == 0 {
                entry[i].surface_z = surface_z;
            }
            if entry[i].feature_class >= entry[best_i].feature_class {
                best_i = i;
            }
        }
        return entry[best_i];
    }
    let (ox, oy, oz) = edge_owner(key);
    let seed_name = format!("edge_r_{ox}_{oy}_{}_{serial}", oz as u8);
    let mut rng = ChaCha8Rng::seed_from_u64(u64::from(layer_seed(world_seed, &seed_name)));
    let port = Port {
        id: 0,
        t: lerp(0.28, 0.72, rng.gen()),
        kind: Kind::River,
        feature_class,
        flow_sign: 1,
        surface_z,
        feature_id: feature_hash(&[&world_seed.to_string(), "river", &key.to_string(), "0"]),
    };
    entry.push(port);
    port
}

fn add_river_link(
    world_seed: i32,
    cell: i32,
    a: Endpoint,
    b: Endpoint,
    feature_class: i32,
    links: &mut FxHashMap<i32, Vec<Link>>,
) {
    let link = Link {
        a,
        b,
        kind: Kind::River,
        feature_class,
        feature_id: feature_hash(&[
            &world_seed.to_string(),
            "rlink",
            &cell.to_string(),
            &a.ref_id.to_string(),
            &b.ref_id.to_string(),
        ]),
    };
    links.entry(cell).or_default().push(link);
}
