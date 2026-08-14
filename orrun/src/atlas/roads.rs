//! Road MST + A* routing with edge ports and links.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use rand::Rng;
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use rustc_hash::{FxHashMap, FxHashSet};

use super::biomes::{self, Biome};
use super::features::{edge_key, edge_owner, Dir, EndpointKind, Kind, NodeKind, RoadClass};
use super::pack;
use super::types::{Crossing, Endpoint, GraphNode, Link, Port};
use super::{cardinal, feature_hash, layer_seed, lerp, NEIGHBOR_DX, NEIGHBOR_DZ};

pub struct RoadGraph {
    pub ports: FxHashMap<i32, Vec<Port>>,
    pub links: FxHashMap<i32, Vec<Link>>,
    pub primary_edges: Vec<(i32, i32)>,
}

impl RoadGraph {
    pub fn new() -> Self {
        Self {
            ports: FxHashMap::default(),
            links: FxHashMap::default(),
            primary_edges: Vec::new(),
        }
    }
}

pub fn build_roads(
    world_seed: i32,
    size: usize,
    cells: &[i32],
    elev_code: &[u8],
    nodes: &[GraphNode],
    river_links: &FxHashMap<i32, Vec<Link>>,
    graph: &mut RoadGraph,
) {
    if nodes.len() < 2 {
        return;
    }

    let river_adjacent = river_adjacency_mask(size, cells, river_links);
    let mut channel_mask = vec![0u8; size * size];
    for &cell in river_links.keys() {
        channel_mask[cell as usize] = 1;
    }

    let mut by_mass: FxHashMap<i32, Vec<usize>> = FxHashMap::default();
    for (i, node) in nodes.iter().enumerate() {
        by_mass.entry(node.landmass).or_default().push(i);
    }

    let towns: FxHashSet<i32> = nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Settlement)
        .map(|n| n.cell)
        .collect();

    let mut road_serial = 0i32;
    for (_mass, mut indices) in by_mass {
        if indices.len() < 2 {
            continue;
        }
        indices.sort_by_key(|&i| nodes[i].id);
        let group: Vec<&GraphNode> = indices.iter().map(|&i| &nodes[i]).collect();
        let n = group.len();
        let mut in_tree = vec![false; n];
        in_tree[densest_node_index(&group, cells)] = true;
        let mut edges = Vec::new();
        for _ in 0..n - 1 {
            let mut best_i = None;
            let mut best_j = None;
            let mut best_w = f32::INFINITY;
            for i in 0..n {
                if !in_tree[i] {
                    continue;
                }
                for j in 0..n {
                    if in_tree[j] {
                        continue;
                    }
                    let w = road_pair_weight(group[i], group[j], cells);
                    if w < best_w {
                        best_w = w;
                        best_i = Some(i);
                        best_j = Some(j);
                    }
                }
            }
            let (Some(i), Some(j)) = (best_i, best_j) else {
                break;
            };
            in_tree[j] = true;
            edges.push((i, j));
        }

        let mut linked: FxHashSet<(i32, i32)> = FxHashSet::default();
        for (i, j) in edges {
            let a = group[i];
            let b = group[j];
            if route_and_stamp_road(
                world_seed,
                size,
                cells,
                elev_code,
                &river_adjacent,
                &channel_mask,
                a,
                b,
                RoadClass::Primary,
                road_serial,
                &towns,
                graph,
            ) {
                graph.primary_edges.push((a.id, b.id));
                linked.insert(pair_key(a.id, b.id));
                road_serial += 1;
            }
        }

        for i in 0..n {
            let ni = group[i];
            let mut best: Vec<(f32, usize)> = Vec::new();
            for (j, nj) in group.iter().enumerate() {
                if i == j {
                    continue;
                }
                best.push((road_pair_weight(ni, nj, cells), j));
            }
            best.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(Ordering::Equal));
            let spur_budget = if ni.kind == NodeKind::Settlement {
                0
            } else {
                2
            };
            let mut added = 0;
            for &(_w, j) in &best {
                if added >= spur_budget {
                    break;
                }
                let nj = group[j];
                let key = pair_key(ni.id, nj.id);
                if linked.contains(&key) {
                    continue;
                }
                if route_and_stamp_road(
                    world_seed,
                    size,
                    cells,
                    elev_code,
                    &river_adjacent,
                    &channel_mask,
                    ni,
                    nj,
                    RoadClass::Secondary,
                    road_serial,
                    &towns,
                    graph,
                ) {
                    linked.insert(key);
                    road_serial += 1;
                    added += 1;
                }
            }
        }
    }
}

fn pair_key(a: i32, b: i32) -> (i32, i32) {
    if a < b {
        (a, b)
    } else {
        (b, a)
    }
}

fn river_adjacency_mask(
    size: usize,
    cells: &[i32],
    river_links: &FxHashMap<i32, Vec<Link>>,
) -> Vec<u8> {
    let mut mask = vec![0u8; size * size];
    for &cell in river_links.keys() {
        let cx = cell % size as i32;
        let cz = cell / size as i32;
        for k in 0..8 {
            let nx = cx + NEIGHBOR_DX[k];
            let nz = cz + NEIGHBOR_DZ[k];
            if nx < 0 || nz < 0 || nx as usize >= size || nz as usize >= size {
                continue;
            }
            let nb = nz as usize * size + nx as usize;
            if !river_links.contains_key(&(nb as i32)) && biomes::is_land(pack::biome(cells[nb])) {
                mask[nb] = 1;
            }
        }
    }
    mask
}

fn densest_node_index(group: &[&GraphNode], cells: &[i32]) -> usize {
    let mut best = 0;
    let mut best_pop = -1;
    for (i, node) in group.iter().enumerate() {
        let pop = pack::population(cells[node.cell as usize]);
        if pop > best_pop {
            best_pop = pop;
            best = i;
        }
    }
    best
}

fn road_pair_weight(a: &GraphNode, b: &GraphNode, cells: &[i32]) -> f32 {
    let d = ((a.ax - b.ax).abs() + (a.az - b.az).abs()) as f32;
    d * road_node_factor(a, cells) * road_node_factor(b, cells)
}

fn road_node_factor(node: &GraphNode, cells: &[i32]) -> f32 {
    let mut factor = lerp(
        1.0,
        0.72,
        pack::population(cells[node.cell as usize]) as f32 / 15.0,
    );
    if node.kind == NodeKind::Settlement {
        factor *= 0.85;
    }
    factor
}

fn route_and_stamp_road(
    world_seed: i32,
    size: usize,
    cells: &[i32],
    elev_code: &[u8],
    river_adjacent: &[u8],
    river_channel: &[u8],
    a: &GraphNode,
    b: &GraphNode,
    road_class: RoadClass,
    serial: i32,
    towns: &FxHashSet<i32>,
    graph: &mut RoadGraph,
) -> bool {
    let mut path = road_astar(
        size,
        cells,
        elev_code,
        river_adjacent,
        river_channel,
        a.cell,
        b.cell,
    );
    if path.len() < 2 {
        path = road_bresenham(size, cells, a.ax, a.az, b.ax, b.az);
    }
    if path.len() < 2 {
        return false;
    }
    stamp_road_path(
        world_seed, size, cells, &path, road_class, a.id, b.id, serial, towns, graph,
    );
    true
}

#[derive(Copy, Clone)]
struct OpenNode {
    f: f32,
    cell: i32,
}

impl PartialEq for OpenNode {
    fn eq(&self, other: &Self) -> bool {
        self.cell == other.cell
    }
}
impl Eq for OpenNode {}
impl PartialOrd for OpenNode {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for OpenNode {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .f
            .partial_cmp(&self.f)
            .unwrap_or(Ordering::Equal)
            .then_with(|| self.cell.cmp(&other.cell))
    }
}

pub fn road_astar(
    size: usize,
    cells: &[i32],
    elev_code: &[u8],
    river_adjacent: &[u8],
    river_channel: &[u8],
    start: i32,
    goal: i32,
) -> Vec<i32> {
    let count = size * size;
    if start < 0 || goal < 0 || start as usize >= count || goal as usize >= count {
        return Vec::new();
    }
    if start == goal {
        return vec![start];
    }

    let mut gscore = vec![f32::INFINITY; count];
    let mut came = vec![-1i32; count];
    let mut closed = vec![false; count];
    gscore[start as usize] = 0.0;

    let goal_ax = goal % size as i32;
    let goal_az = goal / size as i32;
    let mut open = BinaryHeap::new();
    open.push(OpenNode {
        f: ((goal_ax - start % size as i32).abs() + (goal_az - start / size as i32).abs()) as f32,
        cell: start,
    });

    let mut expansions = 0i32;
    while let Some(OpenNode { cell: current, .. }) = open.pop() {
        if expansions >= 20000 {
            break;
        }
        expansions += 1;
        if current == goal {
            return reconstruct(&came, current);
        }
        let cu = current as usize;
        if closed[cu] {
            continue;
        }
        closed[cu] = true;

        let cx = current % size as i32;
        let cz = current / size as i32;
        for k in 0..4 {
            let (dx, dz) = cardinal(k);
            let nx = cx + dx;
            let nz = cz + dz;
            if nx < 0 || nz < 0 || nx as usize >= size || nz as usize >= size {
                continue;
            }
            let nb = nz * size as i32 + nx;
            let nbu = nb as usize;
            if closed[nbu] {
                continue;
            }
            let cell_word = cells[nbu];
            let b = pack::biome(cell_word);
            if matches!(b, Biome::Ocean | Biome::Lake) {
                continue;
            }
            let mut step = 1.45;
            step += pack::relief(cell_word) as f32 * 0.06;
            step += (elev_code[nbu] as i32 - elev_code[cu] as i32).abs() as f32 * 0.08;
            if b == Biome::Alpine {
                step += 0.5;
            }
            if river_channel[nbu] != 0 {
                step += 0.55;
            } else if river_adjacent[nbu] != 0 {
                step -= 0.2;
            }
            step -= (pack::population(cell_word) as f32 * 0.035).min(0.45);
            let tentative = gscore[cu] + step.max(1.0);
            if tentative >= gscore[nbu] {
                continue;
            }
            came[nbu] = current;
            gscore[nbu] = tentative;
            let h = (nx - goal_ax).abs() as f32 + (nz - goal_az).abs() as f32;
            open.push(OpenNode {
                f: tentative + h,
                cell: nb,
            });
        }
    }
    Vec::new()
}

fn reconstruct(came: &[i32], mut current: i32) -> Vec<i32> {
    let mut path = vec![current];
    while came[current as usize] >= 0 {
        current = came[current as usize];
        path.push(current);
    }
    path.reverse();
    path
}

fn road_bresenham(size: usize, cells: &[i32], ax0: i32, az0: i32, ax1: i32, az1: i32) -> Vec<i32> {
    let mut path = Vec::new();
    let mut x = ax0;
    let mut z = az0;
    let dx = (ax1 - ax0).abs();
    let dz = (az1 - az0).abs();
    let sx = if ax0 < ax1 { 1 } else { -1 };
    let sz = if az0 < az1 { 1 } else { -1 };
    let mut err = dx - dz;
    loop {
        if x < 0 || z < 0 || x as usize >= size || z as usize >= size {
            return Vec::new();
        }
        let idx = z as usize * size + x as usize;
        if pack::biome(cells[idx]) == Biome::Ocean {
            return Vec::new();
        }
        path.push(idx as i32);
        if x == ax1 && z == az1 {
            break;
        }
        let e2 = err * 2;
        if e2 > -dz {
            err -= dz;
            x += sx;
        }
        if e2 < dx {
            err += dx;
            z += sz;
        }
    }
    path
}

fn stamp_road_path(
    world_seed: i32,
    size: usize,
    cells: &[i32],
    path: &[i32],
    road_class: RoadClass,
    node_a: i32,
    node_b: i32,
    serial: i32,
    towns: &FxHashSet<i32>,
    graph: &mut RoadGraph,
) {
    let feature_id = feature_hash(&[
        &world_seed.to_string(),
        "road",
        &node_a.to_string(),
        &node_b.to_string(),
        &serial.to_string(),
    ]);
    for i in 0..path.len() {
        let cell = path[i];
        let ax = cell % size as i32;
        let az = cell / size as i32;
        let surface_z = pack::elevation_to_metres(pack::elevation(cells[cell as usize]));
        let ea = if i == 0 {
            Endpoint::node(node_a)
        } else {
            let prev = path[i - 1];
            let Some(back_dir) =
                dir_from_delta((prev % size as i32) - ax, (prev / size as i32) - az)
            else {
                continue;
            };
            let in_port = ensure_road_port(
                world_seed,
                size,
                ax,
                az,
                back_dir,
                road_class,
                surface_z,
                &mut graph.ports,
            );
            Endpoint::edge_port(edge_key(ax, az, back_dir, size), in_port.id)
        };
        let eb = if i + 1 == path.len() {
            Endpoint::node(node_b)
        } else {
            let next_cell = path[i + 1];
            let Some(forward) = dir_from_delta(
                (next_cell % size as i32) - ax,
                (next_cell / size as i32) - az,
            ) else {
                continue;
            };
            let out_port = ensure_road_port(
                world_seed,
                size,
                ax,
                az,
                forward,
                road_class,
                surface_z,
                &mut graph.ports,
            );
            Endpoint::edge_port(edge_key(ax, az, forward, size), out_port.id)
        };
        let link = Link {
            a: ea,
            b: eb,
            kind: Kind::Road,
            feature_class: road_class as i32,
            feature_id,
        };
        let slot = graph.links.entry(cell).or_default();
        if already_has_undirected(slot, &link) {
            continue;
        }
        if towns.contains(&cell) && !plaza_allows_link(slot, ax, az, &link) {
            continue;
        }
        slot.push(link);
    }
}

fn already_has_undirected(existing: &[Link], new: &Link) -> bool {
    existing
        .iter()
        .any(|l| (l.a == new.a && l.b == new.b) || (l.a == new.b && l.b == new.a))
}

/// A town is a spur or a through-road, not a hub of MST/spur exits.
fn plaza_allows_link(existing: &[Link], ax: i32, az: i32, new: &Link) -> bool {
    let Some(new_dir) = plaza_exit_dir(ax, az, new) else {
        return true;
    };
    let mut exits = Vec::new();
    for link in existing {
        let Some(dir) = plaza_exit_dir(ax, az, link) else {
            continue;
        };
        if !exits.contains(&dir) {
            exits.push(dir);
        }
    }
    if exits.contains(&new_dir) {
        return false;
    }
    match exits.len() {
        0 => true,
        1 => exits[0].opposite() == new_dir,
        _ => false,
    }
}

pub(crate) fn plaza_exit_dir(ax: i32, az: i32, link: &Link) -> Option<Dir> {
    let ep = match (link.a.kind, link.b.kind) {
        (EndpointKind::EdgePort, EndpointKind::Node) => link.a,
        (EndpointKind::Node, EndpointKind::EdgePort) => link.b,
        _ => return None,
    };
    let (ox, oz, dir) = edge_owner(ep.ref_id);
    if ox == ax && oz == az {
        Some(dir)
    } else if ox == ax - 1 && oz == az && dir == Dir::East {
        Some(Dir::West)
    } else if ox == ax && oz == az - 1 && dir == Dir::South {
        Some(Dir::North)
    } else {
        None
    }
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

fn ensure_road_port(
    world_seed: i32,
    size: usize,
    ax: i32,
    az: i32,
    dir: Dir,
    road_class: RoadClass,
    surface_z: i32,
    ports: &mut FxHashMap<i32, Vec<Port>>,
) -> Port {
    let key = edge_key(ax, az, dir, size);
    let entry = ports.entry(key).or_default();
    if !entry.is_empty() {
        let mut best_i = 0;
        for i in 0..entry.len() {
            if entry[i].feature_class > entry[best_i].feature_class {
                best_i = i;
            }
            if (road_class as i32) < entry[i].feature_class {
                entry[i].feature_class = road_class as i32;
            }
        }
        return entry[best_i];
    }
    let (ox, oy, oz) = edge_owner(key);
    let seed_name = format!("edge_p_{ox}_{oy}_{}_{}", oz as u8, 0);
    let mut rng = ChaCha8Rng::seed_from_u64(u64::from(layer_seed(world_seed, &seed_name)));
    let port = Port {
        id: 0,
        t: lerp(0.3, 0.7, rng.gen()),
        kind: Kind::Road,
        feature_class: road_class as i32,
        flow_sign: 0,
        surface_z,
        feature_id: feature_hash(&[&world_seed.to_string(), "rport", &key.to_string(), "0"]),
    };
    entry.push(port);
    port
}

pub fn find_crossings(
    river_links: &FxHashMap<i32, Vec<Link>>,
    road_links: &FxHashMap<i32, Vec<Link>>,
) -> Vec<Crossing> {
    let mut crossings = Vec::new();
    for (&cell, road_list) in road_links {
        let Some(river_list) = river_links.get(&cell) else {
            continue;
        };
        if road_list.is_empty() || river_list.is_empty() {
            continue;
        }
        let road = &road_list[0];
        let river = &river_list[0];
        crossings.push(Crossing {
            id: crossings.len() as i32,
            cell,
            river_id: river.feature_id,
            road_id: road.feature_id,
            river_class: river.feature_class,
            road_class: road.feature_class,
        });
    }
    crossings
}
