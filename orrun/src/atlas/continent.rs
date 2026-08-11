//! ContinentAtlas — generate, validate, accessors.

use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};
use thiserror::Error;

use super::biomes::{self, Biome};
use super::classify;
use super::features::{Dir, EndpointKind, Kind};
use super::lakes::{self, LakeScratch};
use super::landmask::{self, collar_cells};
use super::nodes;
use super::orogen;
use super::pack;
use super::population;
use super::rivers::{self, RiverGraph};
use super::roads::{self, RoadGraph};
use super::hydro::HydroVectors;
use super::types::{Crossing, GraphNode, Lake, Link, Port};
use super::cardinal;

pub const SIZE: usize = 1000;
pub const CELL_METRES: f32 = 1000.0;
pub const SCHEMA_VERSION: i32 = 5;
pub const SEA_SURFACE_Z: i32 = 0;

#[derive(Debug, Error)]
pub enum AtlasError {
    #[error("atlas validation failed: {0}")]
    Validation(String),
}

/// Immutable continental climate + major river/road continuity.
#[derive(Debug, Clone)]
pub struct ContinentAtlas {
    pub schema_version: i32,
    pub world_seed: i32,
    pub size: usize,
    pub content_hash: i32,
    pub sea_surface_z: i32,
    pub cells: Vec<i32>,
    pub landmass_id: Vec<i32>,
    pub lake_id: Vec<i32>,
    pub lakes: Vec<Lake>,
    pub nodes: Vec<GraphNode>,
    pub crossings: Vec<Crossing>,
    pub primary_road_edges: Vec<(i32, i32)>,
    pub river_ports: FxHashMap<i32, Vec<Port>>,
    pub road_ports: FxHashMap<i32, Vec<Port>>,
    pub river_links: FxHashMap<i32, Vec<Link>>,
    pub road_links: FxHashMap<i32, Vec<Link>>,
    pub river_receiver: Vec<i32>,
    pub mouth_distance: Vec<i32>,
    /// Deterministic vector hydrology (rivers / lakes / coasts as curves).
    pub hydro: Arc<HydroVectors>,
}

impl ContinentAtlas {
    pub fn generate(world_seed: i32, size: usize) -> Self {
        assert!(size >= 32, "atlas size must be >= 32, got {size}");
        let count = size * size;
        let mut cells = vec![0i32; count];
        let mut landmass_id = vec![-1i32; count];
        let mut lake_scratch = LakeScratch::new(count);

        let planes = landmask::build_landmask(world_seed, size);
        let mut land = planes.land;
        let mut elev_code = planes.elev_code;
        let mut humidity = planes.humidity;
        let mut relief = planes.relief;

        orogen::apply_orogens(
            world_seed,
            size,
            &land,
            &mut elev_code,
            &mut humidity,
            &mut relief,
        );
        lakes::build_lakes(world_seed, size, &land, &mut elev_code, &mut lake_scratch);
        lakes::merge_coastal_lakes_into_ocean(size, &mut land, &mut elev_code, &mut lake_scratch);
        lakes::promote_inland_seas_to_lakes(size, &land, &mut elev_code, &mut lake_scratch);
        lakes::label_landmasses(size, &land, &lake_scratch.lake_id, &mut landmass_id);
        classify::classify_and_pack(
            world_seed,
            size,
            &land,
            &elev_code,
            &mut humidity,
            &mut relief,
            &lake_scratch.lake_id,
            &mut cells,
        );

        let mut river_graph = RiverGraph::new(count);
        rivers::build_rivers(
            world_seed,
            size,
            &mut cells,
            &mut elev_code,
            &lake_scratch.lake_id,
            &lake_scratch.lakes,
            &mut river_graph,
        );

        let mut mouth_distance = Vec::new();
        population::seed_population(
            world_seed,
            size,
            &mut cells,
            &river_graph.links,
            &mut mouth_distance,
        );

        let mut nodes = Vec::new();
        nodes::seed_nodes(
            world_seed,
            size,
            &cells,
            &mut landmass_id,
            &lake_scratch.lake_id,
            &mut nodes,
        );

        let mut road_graph = RoadGraph::new();
        roads::build_roads(
            world_seed,
            size,
            &cells,
            &elev_code,
            &nodes,
            &river_graph.links,
            &mut road_graph,
        );
        let crossings = roads::find_crossings(&river_graph.links, &road_graph.links);

        let mut atlas = Self {
            schema_version: SCHEMA_VERSION,
            world_seed,
            size,
            content_hash: 0,
            sea_surface_z: SEA_SURFACE_Z,
            cells,
            landmass_id,
            lake_id: lake_scratch.lake_id,
            lakes: lake_scratch.lakes,
            nodes,
            crossings,
            primary_road_edges: road_graph.primary_edges,
            river_ports: river_graph.ports,
            road_ports: road_graph.ports,
            river_links: river_graph.links,
            road_links: road_graph.links,
            river_receiver: river_graph.receiver,
            mouth_distance,
            hydro: Arc::new(HydroVectors {
                sea_surface_z: SEA_SURFACE_Z as f32,
                rivers: Vec::new(),
                lakes: Vec::new(),
                coasts: Vec::new(),
                cell_rivers: Vec::new(),
                cell_lakes: Vec::new(),
                cell_coasts: Vec::new(),
            }),
        };
        atlas.hydro = Arc::new(HydroVectors::bake(&atlas));
        atlas.content_hash = atlas.compute_hash();
        atlas
    }

    #[inline]
    pub fn index_of(&self, ax: i32, az: i32) -> usize {
        az as usize * self.size + ax as usize
    }

    #[inline]
    pub fn in_bounds(&self, ax: i32, az: i32) -> bool {
        ax >= 0 && az >= 0 && (ax as usize) < self.size && (az as usize) < self.size
    }

    pub fn cell_at(&self, ax: i32, az: i32) -> i32 {
        self.cells[self.index_of(ax, az)]
    }

    pub fn is_ocean(&self, ax: i32, az: i32) -> bool {
        pack::biome(self.cell_at(ax, az)) == Biome::Ocean
    }

    pub fn is_lake(&self, ax: i32, az: i32) -> bool {
        pack::biome(self.cell_at(ax, az)) == Biome::Lake
    }

    pub fn ports_on_edge(&self, ax: i32, az: i32, dir: Dir, kind: Kind) -> &[Port] {
        let key = super::features::edge_key(ax, az, dir, self.size);
        let store = match kind {
            Kind::River => &self.river_ports,
            Kind::Road => &self.road_ports,
        };
        store.get(&key).map(|v| v.as_slice()).unwrap_or(&[])
    }

    pub fn links_in_cell(&self, ax: i32, az: i32, kind: Kind) -> &[Link] {
        let idx = self.index_of(ax, az) as i32;
        let store = match kind {
            Kind::River => &self.river_links,
            Kind::Road => &self.road_links,
        };
        store.get(&idx).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Empty vec means all invariants hold.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if self.schema_version != SCHEMA_VERSION {
            errors.push("schema_version mismatch".into());
        }
        if self.cells.len() != self.size * self.size {
            errors.push("cells size wrong".into());
            return errors;
        }

        let mut ocean_border = 0;
        let mut border_total = 0;
        let mut land_count = 0;
        let collar = collar_cells(self.size);
        for az in 0..self.size {
            for ax in 0..self.size {
                let biome = pack::biome(self.cell_at(ax as i32, az as i32));
                if biomes::is_land(biome) || biome == Biome::Lake {
                    land_count += 1;
                }
                let on_border = (ax as i32) < collar
                    || (az as i32) < collar
                    || (ax as i32) >= (self.size as i32) - collar
                    || (az as i32) >= (self.size as i32) - collar;
                if on_border {
                    border_total += 1;
                    if biome == Biome::Ocean {
                        ocean_border += 1;
                    }
                }
            }
        }
        if land_count < self.size * self.size / 20 {
            errors.push(format!("too little land ({land_count} cells)"));
        }
        if border_total > 0 && (ocean_border as f32) / (border_total as f32) < 0.7 {
            errors.push(format!(
                "ocean collar weak ({:.0}% ocean on border)",
                100.0 * ocean_border as f32 / border_total as f32
            ));
        }
        if self.lakes.is_empty() {
            errors.push("no atlas lakes".into());
        }
        if self.hydro.coasts.is_empty() {
            errors.push("hydro: no coast rings".into());
        }
        if self.hydro.lakes.is_empty() && !self.lakes.is_empty() {
            errors.push("hydro: atlas lakes missing outlines".into());
        }
        for r in &self.hydro.rivers {
            match r.sink {
                super::hydro::HydroSink::Ocean | super::hydro::HydroSink::Lake { .. } => {}
            }
            if r.points.len() < 2 {
                errors.push(format!("hydro: river {} too short", r.id));
            }
        }

        for lake in &self.lakes {
            if lake.spill_cell < 0 {
                errors.push(format!("lake {} has no spill", lake.id));
            }
            if !self.lake_is_connected(lake) {
                errors.push(format!("lake {} is not contiguous", lake.id));
            }
            if self.lake_touches_ocean(lake) {
                errors.push(format!(
                    "lake {} touches ocean instead of being ocean",
                    lake.id
                ));
            }
        }
        if let Some(cell) = self.first_inland_ocean_cell() {
            let ax = (cell % self.size) as i32;
            let az = (cell / self.size) as i32;
            errors.push(format!(
                "inland ocean cell at ({ax},{az}) — must be lake or open sea"
            ));
        }

        errors.extend(self.validate_edge_agreement(Kind::River));
        errors.extend(self.validate_edge_agreement(Kind::Road));
        errors.extend(self.validate_river_termination());
        errors.extend(self.validate_river_monotonicity());
        errors.extend(self.validate_road_backbone());
        errors.extend(self.validate_population());
        errors
    }

    fn validate_population(&self) -> Vec<String> {
        let mut errors = Vec::new();
        let mut land = 0;
        let mut occupied = 0;
        let mut water_occupied = 0;
        for &cell in &self.cells {
            let pop = pack::population(cell);
            if biomes::is_land(pack::biome(cell)) {
                land += 1;
                if pop > 0 {
                    occupied += 1;
                }
            } else if pop > 0 {
                water_occupied += 1;
            }
        }
        if water_occupied > 0 {
            errors.push(format!("population on {water_occupied} water cells"));
        }
        if land == 0 {
            return errors;
        }
        if occupied == 0 {
            errors.push("no populated land cells".into());
        }
        if occupied as f32 / land as f32 > 0.5 {
            errors.push(format!(
                "population not sparse ({:.0}% of land occupied)",
                100.0 * occupied as f32 / land as f32
            ));
        }
        errors
    }

    fn validate_edge_agreement(&self, kind: Kind) -> Vec<String> {
        let mut errors = Vec::new();
        let label = if kind == Kind::River { "river" } else { "road" };
        for az in 0..self.size {
            for ax in 0..self.size.saturating_sub(1) {
                let a = self.ports_on_edge(ax as i32, az as i32, Dir::East, kind);
                let b = self.ports_on_edge(ax as i32 + 1, az as i32, Dir::West, kind);
                if a.len() != b.len() {
                    errors.push(format!(
                        "{label} east ports disagree at {ax},{az} ({} vs {})",
                        a.len(),
                        b.len()
                    ));
                    if errors.len() > 12 {
                        return errors;
                    }
                    continue;
                }
                for i in 0..a.len() {
                    if (a[i].t - b[i].t).abs() > 0.001 || a[i].feature_class != b[i].feature_class
                    {
                        errors.push(format!("port payload mismatch at {ax},{az} east #{i}"));
                    }
                }
            }
        }
        for az in 0..self.size.saturating_sub(1) {
            for ax in 0..self.size {
                let a = self.ports_on_edge(ax as i32, az as i32, Dir::South, kind);
                let b = self.ports_on_edge(ax as i32, az as i32 + 1, Dir::North, kind);
                if a.len() != b.len() {
                    errors.push(format!(
                        "{label} south ports disagree at {ax},{az} ({} vs {})",
                        a.len(),
                        b.len()
                    ));
                    if errors.len() > 12 {
                        return errors;
                    }
                }
            }
        }
        errors
    }

    fn validate_river_termination(&self) -> Vec<String> {
        let mut errors = Vec::new();
        let mut port_used: FxHashSet<(i32, i32)> = FxHashSet::default();
        let mut cells_with_sink_link: FxHashSet<i32> = FxHashSet::default();
        for (&cell_idx, links) in &self.river_links {
            for link in links {
                if link.a.kind == EndpointKind::EdgePort {
                    port_used.insert((link.a.ref_id, link.a.port_id));
                }
                if link.b.kind == EndpointKind::EdgePort {
                    port_used.insert((link.b.ref_id, link.b.port_id));
                }
                if matches!(link.b.kind, EndpointKind::Ocean | EndpointKind::Lake) {
                    cells_with_sink_link.insert(cell_idx);
                }
            }
        }
        for (&key, ports) in &self.river_ports {
            for port in ports {
                if !port_used.contains(&(key, port.id)) {
                    errors.push(format!("dangling river port {key}:{}", port.id));
                    if errors.len() > 20 {
                        return errors;
                    }
                }
            }
        }

        if self.river_receiver.len() == self.cells.len() {
            for &idx in self.river_links.keys() {
                let mut guard = 0;
                let mut walk = idx;
                let mut seen: FxHashSet<i32> = FxHashSet::default();
                let mut reached = false;
                while walk >= 0 && guard < self.size * self.size {
                    guard += 1;
                    if !seen.insert(walk) {
                        break;
                    }
                    let biome = pack::biome(self.cells[walk as usize]);
                    if matches!(biome, Biome::Ocean | Biome::Lake) {
                        reached = true;
                        break;
                    }
                    if cells_with_sink_link.contains(&walk) {
                        reached = true;
                        break;
                    }
                    let nxt = self.river_receiver[walk as usize];
                    if nxt < 0 {
                        reached = matches!(
                            self.receiver_sink_biome(walk),
                            Biome::Ocean | Biome::Lake
                        );
                        break;
                    }
                    let nxt_biome = pack::biome(self.cells[nxt as usize]);
                    if matches!(nxt_biome, Biome::Ocean | Biome::Lake) {
                        reached = true;
                        break;
                    }
                    walk = nxt;
                }
                if !reached {
                    errors.push(format!(
                        "river vanishes at cell {},{}",
                        idx % self.size as i32,
                        idx / self.size as i32
                    ));
                    if errors.len() > 20 {
                        return errors;
                    }
                }
            }
        }
        errors
    }

    fn receiver_sink_biome(&self, cell: i32) -> Biome {
        let ax = cell % self.size as i32;
        let az = cell / self.size as i32;
        for k in 0..4 {
            let (dx, dz) = cardinal(k);
            let nx = ax + dx;
            let nz = az + dz;
            if !self.in_bounds(nx, nz) {
                continue;
            }
            let biome = pack::biome(self.cells[self.index_of(nx, nz)]);
            if matches!(biome, Biome::Ocean | Biome::Lake) {
                return biome;
            }
        }
        Biome::Plains
    }

    fn validate_river_monotonicity(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if self.river_receiver.len() != self.cells.len() {
            return errors;
        }
        for idx in 0..self.cells.len() {
            let biome = pack::biome(self.cells[idx]);
            if !biomes::is_land(biome) {
                continue;
            }
            if !self.river_links.contains_key(&(idx as i32)) {
                continue;
            }
            let down = self.river_receiver[idx];
            if down < 0 {
                continue;
            }
            let e0 = pack::elevation(self.cells[idx]);
            let down_biome = pack::biome(self.cells[down as usize]);
            if matches!(down_biome, Biome::Ocean | Biome::Lake) {
                continue;
            }
            let e1 = pack::elevation(self.cells[down as usize]);
            if e1 > e0 {
                errors.push(format!(
                    "river climbs {},{} ({e0}) -> {},{} ({e1})",
                    idx % self.size,
                    idx / self.size,
                    down as usize % self.size,
                    down as usize / self.size
                ));
                if errors.len() > 20 {
                    return errors;
                }
            }
        }
        errors
    }

    fn validate_road_backbone(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if self.nodes.is_empty() {
            errors.push("no road nodes".into());
            return errors;
        }

        let mut by_mass: FxHashMap<i32, Vec<i32>> = FxHashMap::default();
        for node in &self.nodes {
            by_mass.entry(node.landmass).or_default().push(node.id);
        }

        for (mass, ids) in &by_mass {
            if ids.len() < 2 {
                continue;
            }
            let mut parent: FxHashMap<i32, i32> = ids.iter().map(|&id| (id, id)).collect();
            for &(a, b) in &self.primary_road_edges {
                if parent.contains_key(&a) && parent.contains_key(&b) {
                    uf_union(&mut parent, a, b);
                }
            }
            let root = uf_find(&mut parent, ids[0]);
            let connected = ids
                .iter()
                .filter(|&&id| uf_find(&mut parent, id) == root)
                .count();
            if connected < ids.len() {
                errors.push(format!(
                    "landmass {mass} primary roads not connected ({connected} of {})",
                    ids.len()
                ));
            }
        }

        let min_nodes = 8.max(self.size / 20);
        if self.nodes.len() < min_nodes {
            errors.push(format!(
                "too few road nodes ({} < {min_nodes})",
                self.nodes.len()
            ));
        }
        let mut expected_primary_edges = 0;
        for ids in by_mass.values() {
            if ids.len() >= 2 {
                expected_primary_edges += ids.len() - 1;
            }
        }
        if self.primary_road_edges.len() < expected_primary_edges {
            errors.push(format!(
                "primary road edges starved ({} edges, need {expected_primary_edges})",
                self.primary_road_edges.len()
            ));
        }
        let min_road_cells = 24.max(self.nodes.len() * 4);
        if self.road_links.len() < min_road_cells {
            errors.push(format!(
                "road cell coverage starved ({} cells < {min_road_cells})",
                self.road_links.len()
            ));
        }
        let mut through_cells = 0;
        for links in self.road_links.values() {
            if links.iter().any(|link| {
                link.a.kind == EndpointKind::EdgePort
                    && link.b.kind == EndpointKind::EdgePort
                    && link.a.ref_id != link.b.ref_id
            }) {
                through_cells += 1;
            }
        }
        if through_cells < 8.max(min_road_cells / 2) {
            errors.push(format!(
                "road through-links starved ({through_cells} cells with distinct edge ports)"
            ));
        }
        errors
    }

    fn lake_is_connected(&self, lake: &Lake) -> bool {
        if lake.cells.is_empty() {
            return false;
        }
        let mut membership = vec![0u8; self.size * self.size];
        for &cell in &lake.cells {
            membership[cell as usize] = 1;
        }
        let mut queue = vec![lake.cells[0]];
        membership[lake.cells[0] as usize] = 2;
        let mut head = 0;
        while head < queue.len() {
            let cell = queue[head];
            head += 1;
            let ax = cell % self.size as i32;
            let az = cell / self.size as i32;
            for k in 0..4 {
                let (dx, dz) = cardinal(k);
                let nx = ax + dx;
                let nz = az + dz;
                if !self.in_bounds(nx, nz) {
                    continue;
                }
                let nb = self.index_of(nx, nz);
                if membership[nb] != 1 {
                    continue;
                }
                membership[nb] = 2;
                queue.push(nb as i32);
            }
        }
        queue.len() == lake.cells.len()
    }

    fn lake_touches_ocean(&self, lake: &Lake) -> bool {
        for &cell in &lake.cells {
            let ax = cell % self.size as i32;
            let az = cell / self.size as i32;
            for k in 0..4 {
                let (dx, dz) = cardinal(k);
                let nx = ax + dx;
                let nz = az + dz;
                if !self.in_bounds(nx, nz) {
                    return true;
                }
                if pack::biome(self.cells[self.index_of(nx, nz)]) == Biome::Ocean {
                    return true;
                }
            }
        }
        false
    }

    /// First ocean cell not 4-connected to the atlas border through ocean.
    pub(crate) fn first_inland_ocean_cell(&self) -> Option<usize> {
        let count = self.size * self.size;
        let mut open = vec![false; count];
        let mut stack = Vec::new();
        for az in 0..self.size {
            for ax in 0..self.size {
                if ax != 0 && az != 0 && ax + 1 != self.size && az + 1 != self.size {
                    continue;
                }
                let idx = az * self.size + ax;
                if pack::biome(self.cells[idx]) == Biome::Ocean {
                    open[idx] = true;
                    stack.push(idx);
                }
            }
        }
        while let Some(idx) = stack.pop() {
            let ax = (idx % self.size) as i32;
            let az = (idx / self.size) as i32;
            for k in 0..4 {
                let (dx, dz) = cardinal(k);
                let nx = ax + dx;
                let nz = az + dz;
                if !self.in_bounds(nx, nz) {
                    continue;
                }
                let nb = self.index_of(nx, nz);
                if open[nb] || pack::biome(self.cells[nb]) != Biome::Ocean {
                    continue;
                }
                open[nb] = true;
                stack.push(nb);
            }
        }
        (0..count).find(|&i| pack::biome(self.cells[i]) == Biome::Ocean && !open[i])
    }

    fn compute_hash(&self) -> i32 {
        let mut h = self.world_seed as i64;
        h = h.wrapping_mul(31).wrapping_add(self.size as i64);
        h = h.wrapping_mul(31).wrapping_add(self.schema_version as i64);
        h = h.wrapping_mul(31).wrapping_add(self.lakes.len() as i64);
        h = h.wrapping_mul(31).wrapping_add(self.nodes.len() as i64);
        h = h.wrapping_mul(31).wrapping_add(self.river_ports.len() as i64);
        h = h.wrapping_mul(31).wrapping_add(self.road_ports.len() as i64);
        h = h.wrapping_mul(31).wrapping_add(self.crossings.len() as i64);
        h = h.wrapping_mul(31).wrapping_add(self.hydro.rivers.len() as i64);
        h = h.wrapping_mul(31).wrapping_add(self.hydro.lakes.len() as i64);
        h = h.wrapping_mul(31).wrapping_add(self.hydro.coasts.len() as i64);
        let step = (self.size / 32).max(1);
        for az in (0..self.size).step_by(step) {
            for ax in (0..self.size).step_by(step) {
                h = h
                    .wrapping_mul(31)
                    .wrapping_add(self.cells[self.index_of(ax as i32, az as i32)] as i64);
            }
        }
        h as i32
    }
}

fn uf_find(parent: &mut FxHashMap<i32, i32>, mut x: i32) -> i32 {
    while parent[&x] != x {
        let p = parent[&x];
        let pp = parent[&p];
        parent.insert(x, pp);
        x = pp;
    }
    x
}

fn uf_union(parent: &mut FxHashMap<i32, i32>, a: i32, b: i32) {
    let ra = uf_find(parent, a);
    let rb = uf_find(parent, b);
    if ra != rb {
        parent.insert(rb, ra);
    }
}
