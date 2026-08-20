//! Near-player wildlife: Quaternius animals on the walked ground.
//!
//! Fauna is not stored. A 48 m lattice around the player is rolled from the
//! world seed; a cell either hosts a flock or it does not. Agents live only
//! while the player is close, stand on the same contact grid the player walks,
//! and stay out of water, house plots, and hamlets.
//!
//! Predator/prey is for show: grazers flee the player and any hunter, wolves
//! and foxes chase, and a catch despawns the prey. No metabolism, no packs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;

use engine::anim::AnimatedModel;
use engine::collision::{ActorBody, CollisionWorld};
use engine::contact::ContactSnapshot;
use engine::error::{EngineError, EngineResult};
use engine::limits::EngineLimits;
use engine::place::Place;
use engine::space::{GlobalPosition, GlobalXZ};
use engine::world::{EntityId, World};
use glam::Vec2;
use serde::Deserialize;
use thiserror::Error;

use super::coords::Heading;
use super::footprint::BuildingIndex;
use super::ponds::PondField;
use super::rng::CellRng;
use super::settlement::HamletStand;
use super::surface::ContinentalSurface;
use super::world_stream::WorldStream;
use crate::atlas::Biome;

const SIM_RADIUS_M: f64 = 220.0;
const DESPAWN_M: f64 = 260.0;
const CELL_M: f64 = 48.0;
const MAX_AGENTS: usize = 48;
/// New bodies per follow. A flock used to land in one frame and hitch the GPU.
const SPAWN_PER_PASS: usize = 2;
const FOOT_CLEARANCE_M: f32 = 0.06;
const REFRESH_S: f32 = 0.5;
const SLOPE_STEP_M: f64 = 4.0;
const MOVE_ALIGN_DEG: f32 = 40.0;
const TURN_GRAZE_DEG: f32 = 60.0;
const TURN_FLEE_DEG: f32 = 160.0;
const TURN_CHASE_DEG: f32 = 126.0;
const ANIM_IDLE: f32 = 0.65;
const ANIM_EAT: f32 = 0.55;
const ANIM_WALK: f32 = 0.48;
const ANIM_RUN: f32 = 0.62;
const FAUNA_SALT: u64 = 0xF4A1_A001;
/// Wildlife stays this far past the packed-house disk.
const HAMLET_CLEAR_M: f32 = 24.0;

const BIOME_COUNT: usize = 9;

#[derive(Debug, Error)]
pub enum FaunaError {
    #[error("fauna catalog missing at {}", .0.display())]
    MissingCatalog(PathBuf),

    #[error("fauna mesh missing at {}", .0.display())]
    MissingMesh(PathBuf),

    #[error("fauna '{id}' clip '{clip}' is not in {}", .path.display())]
    MissingClip {
        id: String,
        clip: String,
        path: PathBuf,
    },

    #[error("fauna catalog: {0}")]
    Catalog(String),

    #[error("no fauna assets under {}", .0.display())]
    NoAssets(PathBuf),

    #[error(transparent)]
    Engine(#[from] EngineError),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaunaRole {
    Grazer,
    Predator,
    Livestock,
    Domestic,
}

impl FaunaRole {
    fn parse(raw: &str) -> Result<Self, FaunaError> {
        match raw {
            "grazer" => Ok(Self::Grazer),
            "predator" => Ok(Self::Predator),
            "livestock" => Ok(Self::Livestock),
            "domestic" => Ok(Self::Domestic),
            other => Err(FaunaError::Catalog(format!("unknown role '{other}'"))),
        }
    }

    fn is_prey(self) -> bool {
        matches!(self, Self::Grazer | Self::Livestock)
    }

    fn is_predator(self) -> bool {
        matches!(self, Self::Predator)
    }
}

#[derive(Clone, Debug)]
pub struct FaunaSpec {
    pub id: String,
    pub source: String,
    pub role: FaunaRole,
    pub wilderness_spawn: bool,
    pub footprint: f32,
    pub scale: f32,
    pub max_slope_deg: f32,
    pub avoid_water: f32,
    pub density: f32,
    pub biome_weight: [f32; BIOME_COUNT],
    pub flock_min: u32,
    pub flock_max: u32,
    pub walk_speed: f32,
    pub run_speed: f32,
    pub flee_radius: f32,
    pub hunt_range: f32,
    pub catch_radius: f32,
    pub anim_idle: String,
    pub anim_walk: String,
    pub anim_run: String,
    pub anim_eat: String,
}

impl FaunaSpec {
    pub fn is_prey(&self) -> bool {
        self.role.is_prey()
    }

    pub fn is_predator(&self) -> bool {
        self.role.is_predator()
    }

    fn biome_weight(&self, biome: Biome) -> f32 {
        self.biome_weight[biome as u8 as usize]
    }
}

#[derive(Clone, Debug)]
pub struct FaunaCatalog {
    specs: Vec<FaunaSpec>,
}

impl FaunaCatalog {
    pub fn load() -> Result<Self, FaunaError> {
        let path = catalog_path()?;
        Self::load_from(&path)
    }

    pub fn load_from(path: &Path) -> Result<Self, FaunaError> {
        let text = std::fs::read_to_string(path)
            .map_err(|_| FaunaError::MissingCatalog(path.to_path_buf()))?;
        let file: CatalogFile =
            serde_json::from_str(&text).map_err(|e| FaunaError::Catalog(e.to_string()))?;
        if file.fauna.is_empty() {
            return Err(FaunaError::Catalog("empty fauna array".into()));
        }
        let mut specs = Vec::with_capacity(file.fauna.len());
        let mut seen = HashMap::new();
        for entry in file.fauna {
            let spec = spec_from_entry(entry)?;
            if seen.insert(spec.id.clone(), ()).is_some() {
                return Err(FaunaError::Catalog(format!("duplicate id '{}'", spec.id)));
            }
            specs.push(spec);
        }
        Ok(Self { specs })
    }

    pub fn specs(&self) -> &[FaunaSpec] {
        &self.specs
    }

    pub fn wilderness(&self) -> impl Iterator<Item = (usize, &FaunaSpec)> {
        self.specs
            .iter()
            .enumerate()
            .filter(|(_, s)| s.wilderness_spawn)
    }

    pub fn spec(&self, id: &str) -> &FaunaSpec {
        self.specs
            .iter()
            .find(|s| s.id == id)
            .unwrap_or_else(|| panic!("unknown fauna id '{id}'"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Graze,
    Flee,
    Patrol,
    Chase,
    Cooldown,
    Eat,
}

struct Agent {
    id: u32,
    entity: EntityId,
    spec_i: usize,
    cell: (i32, i32),
    pos: GlobalPosition,
    yaw: f32,
    desired_yaw: f32,
    state: State,
    walking: bool,
    waypoint: GlobalXZ,
    hold_left: f32,
    eat_left: f32,
    cooldown_left: f32,
    flee_left: f32,
    flee_retarget: f32,
    walk_deadline: f32,
    state_time: f32,
    clip: String,
    rng: CellRng,
    /// Collision is off until a later pass turns it on for wildlife.
    body: ActorBody,
}

enum ModelSlot {
    Idle,
    Loading(JoinHandle<Result<Arc<AnimatedModel>, FaunaError>>),
    Ready(Arc<AnimatedModel>),
}

/// Live animals around the player.
pub struct FaunaLayer {
    catalog: FaunaCatalog,
    models: Vec<ModelSlot>,
    agents: Vec<Agent>,
    occupied: HashMap<(i32, i32), u32>,
    seed: u64,
    next_id: u32,
    refresh: f32,
    /// Lattice still has empty cells; keep filling next frame instead of waiting.
    spawn_backlog: bool,
    last_born: u32,
    last_died: u32,
}

impl std::fmt::Debug for FaunaLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FaunaLayer")
            .field("agents", &self.agents.len())
            .field("cells", &self.occupied.len())
            .field("loading", &self.busy())
            .finish()
    }
}

impl FaunaLayer {
    pub fn install(world_seed: i32) -> Result<Self, FaunaError> {
        let catalog = FaunaCatalog::load()?;
        let models = (0..catalog.specs.len()).map(|_| ModelSlot::Idle).collect();
        let mut layer = Self {
            catalog,
            models,
            agents: Vec::new(),
            occupied: HashMap::new(),
            seed: world_seed as u32 as u64 ^ FAUNA_SALT,
            next_id: 1,
            refresh: 0.0,
            spawn_backlog: true,
            last_born: 0,
            last_died: 0,
        };
        layer.kick_wilderness_loads()?;
        Ok(layer)
    }

    /// A wilderness mesh is still parsing. The loading screen waits; walking does not.
    pub fn busy(&self) -> bool {
        self.models
            .iter()
            .any(|slot| matches!(slot, ModelSlot::Loading(_)))
    }

    /// Meshes in flight, or the ring still has bodies to place this frame.
    pub fn filling(&self) -> bool {
        self.busy() || self.spawn_backlog
    }

    fn kick_wilderness_loads(&mut self) -> Result<(), FaunaError> {
        let root = fauna_dir()?;
        let indices: Vec<usize> = self.catalog.wilderness().map(|(i, _)| i).collect();
        for spec_i in indices {
            self.kick_load(spec_i, &root)?;
        }
        Ok(())
    }

    fn kick_load(&mut self, spec_i: usize, root: &Path) -> Result<(), FaunaError> {
        if !matches!(self.models[spec_i], ModelSlot::Idle) {
            return Ok(());
        }
        let spec = &self.catalog.specs[spec_i];
        let path = root.join(&spec.source);
        if !path.is_file() {
            return Err(FaunaError::MissingMesh(path));
        }
        let id = spec.id.clone();
        let clips = [
            spec.anim_idle.clone(),
            spec.anim_walk.clone(),
            spec.anim_run.clone(),
            spec.anim_eat.clone(),
        ];
        let root = root.to_path_buf();
        let job = std::thread::Builder::new()
            .name(format!("fauna-{id}"))
            .spawn(move || load_species_model(path, root, id, clips))
            .expect("fauna load thread");
        self.models[spec_i] = ModelSlot::Loading(job);
        Ok(())
    }

    fn poll_loads(&mut self) -> Result<(), FaunaError> {
        for i in 0..self.models.len() {
            let done = matches!(&self.models[i], ModelSlot::Loading(job) if job.is_finished());
            if !done {
                continue;
            }
            let ModelSlot::Loading(job) = std::mem::replace(&mut self.models[i], ModelSlot::Idle)
            else {
                continue;
            };
            self.models[i] = ModelSlot::Ready(job.join().expect("fauna load thread")?);
        }
        Ok(())
    }

    fn ready_model(&self, spec_i: usize) -> Option<Arc<AnimatedModel>> {
        match &self.models[spec_i] {
            ModelSlot::Ready(model) => Some(Arc::clone(model)),
            _ => None,
        }
    }

    pub fn agent_count(&self) -> usize {
        self.agents.len()
    }

    pub fn last_born(&self) -> u32 {
        self.last_born
    }

    pub fn last_died(&self) -> u32 {
        self.last_died
    }

    pub fn clear(&mut self, world: &mut World) -> EngineResult<()> {
        for agent in self.agents.drain(..) {
            world.despawn(agent.entity);
        }
        self.occupied.clear();
        self.refresh = 0.0;
        self.spawn_backlog = true;
        Ok(())
    }

    /// Spawn, despawn, and step animals. Never blocks.
    pub fn follow(
        &mut self,
        world: &mut World,
        stream: &WorldStream,
        surface: &ContinentalSurface,
        ponds: &PondField,
        plots: &BuildingIndex,
        hamlets: &[HamletStand],
        focus: GlobalXZ,
        player: GlobalXZ,
        dt: f32,
    ) -> Result<(), FaunaError> {
        self.last_born = 0;
        self.last_died = 0;
        self.poll_loads()?;
        let ground = stream.contact_snapshot();
        self.refresh += dt;
        if self.spawn_backlog || self.refresh >= REFRESH_S || self.agents.is_empty() {
            self.refresh = 0.0;
            self.spawn_backlog = self
                .refresh_population(world, surface, ponds, plots, hamlets, &ground, focus)?
                || self.busy();
        }
        self.tick(world, surface, ponds, plots, hamlets, &ground, player, dt)?;
        Ok(())
    }

    fn refresh_population(
        &mut self,
        world: &mut World,
        surface: &ContinentalSurface,
        ponds: &PondField,
        plots: &BuildingIndex,
        hamlets: &[HamletStand],
        ground: &ContactSnapshot,
        focus: GlobalXZ,
    ) -> Result<bool, FaunaError> {
        let drop_r = DESPAWN_M;
        let mut i = 0;
        while i < self.agents.len() {
            let at = self.agents[i].pos.horizontal();
            if xz_dist(at, focus) > drop_r || in_hamlet(hamlets, at) {
                self.despawn_at(world, i);
            } else {
                i += 1;
            }
        }

        if self.agents.len() >= MAX_AGENTS {
            return Ok(false);
        }

        let sim_r = SIM_RADIUS_M;
        let min_cx = ((focus.x - sim_r) / CELL_M).floor() as i32;
        let max_cx = ((focus.x + sim_r) / CELL_M).floor() as i32;
        let min_cz = ((focus.z - sim_r) / CELL_M).floor() as i32;
        let max_cz = ((focus.z + sim_r) / CELL_M).floor() as i32;
        let mut budget = SPAWN_PER_PASS;

        for cz in min_cz..=max_cz {
            for cx in min_cx..=max_cx {
                if self.agents.len() >= MAX_AGENTS {
                    return Ok(false);
                }
                if budget == 0 {
                    return Ok(true);
                }
                if self.occupied.contains_key(&(cx, cz)) {
                    continue;
                }
                let center = GlobalXZ::at(
                    (f64::from(cx) + 0.5) * CELL_M,
                    (f64::from(cz) + 0.5) * CELL_M,
                );
                if xz_dist(center, focus) > sim_r {
                    continue;
                }
                if in_hamlet(hamlets, center) {
                    continue;
                }
                if ground.height_at(center).is_none() {
                    continue;
                }
                self.try_spawn_cell(
                    world,
                    surface,
                    ponds,
                    plots,
                    hamlets,
                    ground,
                    (cx, cz),
                    center,
                    &mut budget,
                )?;
            }
        }
        Ok(false)
    }

    fn try_spawn_cell(
        &mut self,
        world: &mut World,
        surface: &ContinentalSurface,
        ponds: &PondField,
        plots: &BuildingIndex,
        hamlets: &[HamletStand],
        ground: &ContactSnapshot,
        cell: (i32, i32),
        center: GlobalXZ,
        budget: &mut usize,
    ) -> Result<(), FaunaError> {
        let mut rng = CellRng::new(self.seed, i64::from(cell.0), i64::from(cell.1));
        let mut pick: Option<(usize, f32)> = None;
        let mut total = 0.0f32;
        for (i, spec) in self.catalog.wilderness() {
            let score = suitability(spec, surface, ponds, plots, hamlets, ground, center);
            if score <= 0.0 {
                continue;
            }
            total += score;
            if rng.unit() * total <= score {
                pick = Some((i, score));
            }
        }
        let Some((spec_i, _)) = pick else {
            return Ok(());
        };
        if self.ready_model(spec_i).is_none() {
            return Ok(());
        }
        let flock_min = self.catalog.specs[spec_i].flock_min;
        let flock_max = self.catalog.specs[spec_i].flock_max;
        let flock = flock_min + (rng.unit() * (flock_max - flock_min + 1) as f32).floor() as u32;
        let flock = flock.clamp(flock_min, flock_max) as usize;
        let n = flock.min(MAX_AGENTS - self.agents.len()).min(*budget);
        if n == 0 {
            return Ok(());
        }

        let mut born = 0u32;
        for k in 0..n {
            let jitter = CELL_M * 0.3;
            let at = GlobalXZ::at(
                center.x + (rng.unit() as f64 * 2.0 - 1.0) * jitter,
                center.z + (rng.unit() as f64 * 2.0 - 1.0) * jitter,
            );
            let stand = if may_stand(
                &self.catalog.specs[spec_i],
                surface,
                ponds,
                plots,
                hamlets,
                ground,
                at,
            ) {
                at
            } else if may_stand(
                &self.catalog.specs[spec_i],
                surface,
                ponds,
                plots,
                hamlets,
                ground,
                center,
            ) {
                center
            } else {
                continue;
            };
            let Some(y) = ground.height_at(stand) else {
                continue;
            };
            let yaw = rng.unit() * 360.0;
            self.spawn_agent(
                world,
                spec_i,
                cell,
                GlobalPosition::at(stand.x, f64::from(y + FOOT_CLEARANCE_M), stand.z),
                yaw,
                CellRng::new(
                    self.seed ^ 0xA11E,
                    i64::from(cell.0) * 31 + k as i64,
                    i64::from(cell.1),
                ),
            )?;
            born += 1;
            self.last_born += 1;
            *budget = budget.saturating_sub(1);
        }
        if born > 0 {
            self.occupied.insert(cell, born);
        }
        Ok(())
    }

    fn spawn_agent(
        &mut self,
        world: &mut World,
        spec_i: usize,
        cell: (i32, i32),
        pos: GlobalPosition,
        yaw: f32,
        rng: CellRng,
    ) -> Result<(), FaunaError> {
        let model = self
            .ready_model(spec_i)
            .expect("spawn_agent requires a ready mesh");
        let scale = self.catalog.specs[spec_i].scale;
        let idle = self.catalog.specs[spec_i].anim_idle.clone();
        let predator = self.catalog.specs[spec_i].is_predator();
        let place = place_of(world, pos, yaw, scale)?;
        let entity = world.spawn_animated_shared(model, place)?;
        world.play_animation(entity, &idle)?;
        world.set_animation_speed(entity, ANIM_IDLE)?;
        let id = self.next_id;
        self.next_id += 1;
        let state = if predator {
            State::Patrol
        } else {
            State::Graze
        };
        self.agents.push(Agent {
            id,
            entity,
            spec_i,
            cell,
            pos,
            yaw,
            desired_yaw: yaw,
            state,
            walking: false,
            waypoint: pos.horizontal(),
            hold_left: if predator {
                2.0
            } else {
                6.0 + rng_unit_hint(id) * 10.0
            },
            eat_left: 0.0,
            cooldown_left: 0.0,
            flee_left: 0.0,
            flee_retarget: 0.0,
            walk_deadline: 20.0,
            state_time: 0.0,
            clip: idle,
            rng,
            body: ActorBody::new(0.4, 1.2),
        });
        Ok(())
    }

    fn despawn_at(&mut self, world: &mut World, index: usize) {
        let agent = self.agents.swap_remove(index);
        world.despawn(agent.entity);
        self.last_died += 1;
        if let Some(count) = self.occupied.get_mut(&agent.cell) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.occupied.remove(&agent.cell);
            }
        }
    }

    fn despawn_id(&mut self, world: &mut World, id: u32) {
        if let Some(i) = self.agents.iter().position(|a| a.id == id) {
            self.despawn_at(world, i);
        }
    }

    fn tick(
        &mut self,
        world: &mut World,
        surface: &ContinentalSurface,
        ponds: &PondField,
        plots: &BuildingIndex,
        hamlets: &[HamletStand],
        ground: &ContactSnapshot,
        player: GlobalXZ,
        dt: f32,
    ) -> EngineResult<()> {
        if dt <= 0.0 {
            for i in 0..self.agents.len() {
                self.sync_place(world, i)?;
            }
            return Ok(());
        }

        let mut caught: Vec<u32> = Vec::new();
        let n = self.agents.len();
        for i in 0..n {
            let catch = self.step_agent(
                i,
                surface,
                ponds,
                plots,
                hamlets,
                ground,
                player,
                world.collision(),
                dt,
            );
            if let Some(id) = catch {
                caught.push(id);
            }
        }
        for id in caught {
            self.despawn_id(world, id);
        }

        for i in 0..self.agents.len() {
            self.sync_anim(world, i)?;
            self.sync_place(world, i)?;
        }
        Ok(())
    }

    fn step_agent(
        &mut self,
        i: usize,
        surface: &ContinentalSurface,
        ponds: &PondField,
        plots: &BuildingIndex,
        hamlets: &[HamletStand],
        ground: &ContactSnapshot,
        player: GlobalXZ,
        collision: &CollisionWorld,
        dt: f32,
    ) -> Option<u32> {
        self.agents[i].state_time += dt;
        let spec_i = self.agents[i].spec_i;
        let predator = self.catalog.specs[spec_i].is_predator();
        let catch = if predator {
            self.tick_predator(i, dt)
        } else {
            self.tick_prey(i, player, dt);
            None
        };
        self.integrate(i, surface, ponds, plots, hamlets, ground, collision, dt);
        catch
    }

    fn tick_prey(&mut self, i: usize, player: GlobalXZ, dt: f32) {
        let flee_r = self.catalog.specs[self.agents[i].spec_i].flee_radius;
        let threat = self.nearest_threat(i, player, flee_r);
        if let Some(at) = threat {
            if self.agents[i].state != State::Flee {
                self.agents[i].state = State::Flee;
                self.agents[i].state_time = 0.0;
                self.agents[i].flee_left = self.agents[i].rng.range(5.0, 9.0);
                self.agents[i].flee_retarget = 0.0;
                self.agents[i].walking = true;
                self.agents[i].hold_left = 0.0;
            }
            self.agents[i].flee_retarget -= dt;
            if self.agents[i].flee_retarget <= 0.0 {
                self.retarget_flee(i, at);
                self.agents[i].flee_retarget = self.agents[i].rng.range(1.2, 2.4);
            }
            return;
        }

        if self.agents[i].state == State::Flee {
            if self.agents[i].state_time > self.agents[i].flee_left
                || (self.agents[i].state_time > 3.0 && self.near_waypoint(i, 3.0))
            {
                self.agents[i].state = State::Graze;
                self.idle_range(i, 8.0, 18.0);
            }
            return;
        }

        if self.agents[i].state == State::Eat {
            self.agents[i].eat_left -= dt;
            self.agents[i].walking = false;
            if self.agents[i].eat_left <= 0.0 {
                self.agents[i].state = State::Graze;
                self.idle_range(i, 5.0, 14.0);
            }
            return;
        }

        if self.agents[i].state != State::Graze {
            self.agents[i].state = State::Graze;
            self.idle_range(i, 4.0, 10.0);
            return;
        }

        if self.agents[i].walking {
            if self.near_waypoint(i, 2.2) {
                self.finish_walk(i);
            }
            return;
        }

        self.agents[i].hold_left -= dt;
        if self.agents[i].hold_left > 0.0 {
            return;
        }
        self.roll_graze(i);
    }

    fn tick_predator(&mut self, i: usize, dt: f32) -> Option<u32> {
        if self.agents[i].cooldown_left > 0.0 {
            self.agents[i].state = State::Cooldown;
            self.agents[i].cooldown_left -= dt;
            self.agents[i].walking = false;
            return None;
        }

        let hunt = self.catalog.specs[self.agents[i].spec_i].hunt_range;
        let catch_r = self.catalog.specs[self.agents[i].spec_i].catch_radius;
        if let Some((prey_id, at, dist)) = self.nearest_prey(i, hunt) {
            if dist <= catch_r {
                self.agents[i].cooldown_left = self.agents[i].rng.range(6.0, 12.0);
                self.agents[i].state = State::Cooldown;
                self.agents[i].walking = false;
                return Some(prey_id);
            }
            self.agents[i].state = State::Chase;
            self.agents[i].walking = true;
            self.agents[i].waypoint = at;
            self.face_waypoint(i);
            return None;
        }

        if self.agents[i].state == State::Chase {
            self.agents[i].state = State::Patrol;
            self.idle_range(i, 3.0, 8.0);
            return None;
        }

        if self.agents[i].state != State::Patrol && self.agents[i].state != State::Graze {
            self.agents[i].state = State::Patrol;
        }

        if self.agents[i].walking {
            if self.near_waypoint(i, 2.5)
                || self.agents[i].state_time > self.agents[i].walk_deadline
            {
                self.agents[i].walking = false;
                self.idle_range(i, 5.0, 14.0);
                self.agents[i].state_time = 0.0;
            }
            return None;
        }

        self.agents[i].hold_left -= dt;
        if self.agents[i].hold_left <= 0.0 {
            if self.agents[i].rng.unit() < 0.4 {
                self.idle_range(i, 4.0, 12.0);
            } else {
                self.begin_patrol(i);
            }
        }
        None
    }

    fn integrate(
        &mut self,
        i: usize,
        surface: &ContinentalSurface,
        ponds: &PondField,
        plots: &BuildingIndex,
        hamlets: &[HamletStand],
        ground: &ContactSnapshot,
        collision: &CollisionWorld,
        dt: f32,
    ) {
        let spec_i = self.agents[i].spec_i;
        let walk_speed = self.catalog.specs[spec_i].walk_speed;
        let run_speed = self.catalog.specs[spec_i].run_speed;
        let idle = self.agents[i].state == State::Eat
            || (!self.agents[i].walking
                && self.agents[i].state != State::Flee
                && self.agents[i].state != State::Chase);
        let turn = match self.agents[i].state {
            State::Flee => TURN_FLEE_DEG,
            State::Chase => TURN_CHASE_DEG,
            _ => TURN_GRAZE_DEG,
        };
        if !idle {
            self.face_waypoint(i);
        }
        self.agents[i].yaw = turn_toward(self.agents[i].yaw, self.agents[i].desired_yaw, turn * dt);

        if idle {
            self.snap_height(i, ground);
            return;
        }

        let align = angle_delta_deg(self.agents[i].yaw, self.agents[i].desired_yaw).abs();
        if align > MOVE_ALIGN_DEG {
            self.snap_height(i, ground);
            return;
        }

        let speed = if self.agents[i].state == State::Flee || self.agents[i].state == State::Chase {
            run_speed
        } else {
            walk_speed
        };
        let heading = Heading::from_degrees(self.agents[i].yaw)
            .map(|h| h.direction())
            .unwrap_or(Vec2::Y);
        let dx = f64::from(heading.x * speed * dt);
        let dz = f64::from(heading.y * speed * dt);
        let dest = collision.move_xz(
            &self.agents[i].body,
            self.agents[i].pos.horizontal(),
            dx,
            dz,
        );
        if may_stand(
            &self.catalog.specs[spec_i],
            surface,
            ponds,
            plots,
            hamlets,
            ground,
            dest,
        ) {
            if let Some(y) = ground.height_at(dest) {
                self.agents[i].pos.x = dest.x;
                self.agents[i].pos.z = dest.z;
                self.agents[i].pos.y = f64::from(y + FOOT_CLEARANCE_M);
                return;
            }
        }
        self.snap_height(i, ground);
    }

    fn snap_height(&mut self, i: usize, ground: &ContactSnapshot) {
        if let Some(y) = ground.height_at(self.agents[i].pos.horizontal()) {
            self.agents[i].pos.y = f64::from(y + FOOT_CLEARANCE_M);
        }
    }

    fn idle_range(&mut self, i: usize, lo: f32, hi: f32) {
        let seconds = self.agents[i].rng.range(lo, hi);
        self.begin_idle(i, seconds);
    }

    fn begin_idle(&mut self, i: usize, seconds: f32) {
        self.agents[i].walking = false;
        self.agents[i].hold_left = seconds;
        let fidget = self.agents[i].rng.range(-26.0, 26.0);
        self.agents[i].desired_yaw = wrap_deg(self.agents[i].yaw + fidget);
    }

    fn begin_eat(&mut self, i: usize) {
        self.agents[i].state = State::Eat;
        self.agents[i].walking = false;
        self.agents[i].eat_left = self.agents[i].rng.range(12.0, 28.0);
    }

    fn begin_walk(&mut self, i: usize) {
        self.agents[i].walking = true;
        self.agents[i].hold_left = 0.0;
        let radius = self.agents[i].rng.range(5.0, 12.0);
        self.agents[i].waypoint = offset_xz(&mut self.agents[i], radius);
        self.face_waypoint(i);
    }

    fn begin_patrol(&mut self, i: usize) {
        self.agents[i].state = State::Patrol;
        self.agents[i].walking = true;
        self.agents[i].hold_left = 0.0;
        self.agents[i].state_time = 0.0;
        self.agents[i].walk_deadline = self.agents[i].rng.range(14.0, 28.0);
        let radius = self.agents[i].rng.range(10.0, 22.0);
        self.agents[i].waypoint = offset_xz(&mut self.agents[i], radius);
        self.face_waypoint(i);
    }

    fn finish_walk(&mut self, i: usize) {
        self.agents[i].walking = false;
        let roll = self.agents[i].rng.unit();
        if roll < 0.62 {
            self.begin_eat(i);
        } else if roll < 0.92 {
            self.idle_range(i, 8.0, 22.0);
        } else {
            self.begin_walk(i);
        }
    }

    fn roll_graze(&mut self, i: usize) {
        let roll = self.agents[i].rng.unit();
        if roll < 0.55 {
            self.begin_eat(i);
        } else if roll < 0.82 {
            self.idle_range(i, 6.0, 16.0);
        } else {
            self.begin_walk(i);
        }
    }

    fn retarget_flee(&mut self, i: usize, threat: GlobalXZ) {
        let here = self.agents[i].pos.horizontal();
        let mut away = Vec2::new((here.x - threat.x) as f32, (here.z - threat.z) as f32);
        if away.length_squared() < 0.01 {
            away = Vec2::new(
                self.agents[i].rng.range(-1.0, 1.0),
                self.agents[i].rng.range(-1.0, 1.0),
            );
        }
        let dir = away.normalize_or_zero();
        let dist = self.agents[i].rng.range(16.0, 28.0) as f64;
        self.agents[i].waypoint = GlobalXZ::at(
            here.x + f64::from(dir.x) * dist,
            here.z + f64::from(dir.y) * dist,
        );
        self.face_waypoint(i);
    }

    fn face_waypoint(&mut self, i: usize) {
        let here = self.agents[i].pos.horizontal();
        let to = Vec2::new(
            (self.agents[i].waypoint.x - here.x) as f32,
            (self.agents[i].waypoint.z - here.z) as f32,
        );
        if to.length_squared() < 0.01 {
            return;
        }
        if let Ok(heading) = Heading::towards(to) {
            self.agents[i].desired_yaw = heading.degrees();
        }
    }

    fn near_waypoint(&self, i: usize, radius: f32) -> bool {
        xz_dist(self.agents[i].pos.horizontal(), self.agents[i].waypoint) <= f64::from(radius)
    }

    fn nearest_threat(&self, i: usize, player: GlobalXZ, radius: f32) -> Option<GlobalXZ> {
        if radius <= 0.0 {
            return None;
        }
        let here = self.agents[i].pos.horizontal();
        let mut best: Option<(f64, GlobalXZ)> = None;
        let player_d = xz_dist(here, player);
        if player_d <= f64::from(radius) {
            best = Some((player_d, player));
        }
        for (j, other) in self.agents.iter().enumerate() {
            if j == i || !self.catalog.specs[other.spec_i].is_predator() {
                continue;
            }
            let d = xz_dist(here, other.pos.horizontal());
            if d <= f64::from(radius) && best.map(|(b, _)| d < b).unwrap_or(true) {
                best = Some((d, other.pos.horizontal()));
            }
        }
        best.map(|(_, at)| at)
    }

    fn nearest_prey(&self, i: usize, range: f32) -> Option<(u32, GlobalXZ, f32)> {
        if range <= 0.0 {
            return None;
        }
        let here = self.agents[i].pos.horizontal();
        let mut best: Option<(u32, GlobalXZ, f32)> = None;
        for (j, other) in self.agents.iter().enumerate() {
            if j == i || !self.catalog.specs[other.spec_i].is_prey() {
                continue;
            }
            let d = xz_dist(here, other.pos.horizontal()) as f32;
            if d <= range && best.map(|(_, _, b)| d < b).unwrap_or(true) {
                best = Some((other.id, other.pos.horizontal(), d));
            }
        }
        best
    }

    fn sync_anim(&mut self, world: &mut World, i: usize) -> EngineResult<()> {
        let spec_i = self.agents[i].spec_i;
        let (clip, speed) = match self.agents[i].state {
            State::Eat => (self.catalog.specs[spec_i].anim_eat.clone(), ANIM_EAT),
            State::Flee | State::Chase => (self.catalog.specs[spec_i].anim_run.clone(), ANIM_RUN),
            _ if self.agents[i].walking => {
                (self.catalog.specs[spec_i].anim_walk.clone(), ANIM_WALK)
            }
            _ => (self.catalog.specs[spec_i].anim_idle.clone(), ANIM_IDLE),
        };
        if self.agents[i].clip != clip {
            world.play_animation(self.agents[i].entity, &clip)?;
            world.set_animation_speed(self.agents[i].entity, speed)?;
            self.agents[i].clip = clip;
        }
        Ok(())
    }

    fn sync_place(&self, world: &mut World, i: usize) -> EngineResult<()> {
        let spec = &self.catalog.specs[self.agents[i].spec_i];
        let place = place_of(world, self.agents[i].pos, self.agents[i].yaw, spec.scale)?;
        world.set_place(self.agents[i].entity, place)
    }
}

/// Hard reject: water, houses, hamlets, missing walked ground, or a slope the species will not climb.
pub fn may_stand(
    spec: &FaunaSpec,
    surface: &ContinentalSurface,
    ponds: &PondField,
    plots: &BuildingIndex,
    hamlets: &[HamletStand],
    ground: &ContactSnapshot,
    at: GlobalXZ,
) -> bool {
    if plots.blocks_prop(at) || in_hamlet(hamlets, at) {
        return false;
    }
    let mut column = surface.column(at);
    ponds.carve(at, &mut column);
    if column.is_wet() {
        return false;
    }
    if column.wetness() > -spec.avoid_water * 0.15 {
        return false;
    }
    let Some(_) = ground.height_at(at) else {
        return false;
    };
    slope_ok(ground, at, spec.max_slope_deg)
}

/// Soft 0–1 habitat score. Zero when [`may_stand`] fails.
pub fn suitability(
    spec: &FaunaSpec,
    surface: &ContinentalSurface,
    ponds: &PondField,
    plots: &BuildingIndex,
    hamlets: &[HamletStand],
    ground: &ContactSnapshot,
    at: GlobalXZ,
) -> f32 {
    if !may_stand(spec, surface, ponds, plots, hamlets, ground, at) {
        return 0.0;
    }
    let biome = surface.fields().biome_at(at.x as f32, at.z as f32);
    let weight = spec.biome_weight(biome);
    if weight <= 0.0 {
        return 0.0;
    }
    let humidity =
        surface
            .fields()
            .sample_smooth(&surface.fields().humidity01, at.x as f32, at.z as f32);
    let relief =
        surface
            .fields()
            .sample_smooth(&surface.fields().relief01, at.x as f32, at.z as f32);
    let mut score = weight;
    if spec.is_prey() {
        score *= (0.45 + humidity * 0.7).clamp(0.2, 1.15);
        score *= (1.1 - relief * 0.35).clamp(0.35, 1.15);
    } else if spec.is_predator() {
        score *= (0.55 + (1.0 - humidity) * 0.35 + relief * 0.25).clamp(0.25, 1.2);
    }
    (score * spec.density).clamp(0.0, 1.0)
}

fn in_hamlet(hamlets: &[HamletStand], p: GlobalXZ) -> bool {
    hamlets.iter().any(|h| h.covers(p, HAMLET_CLEAR_M))
}

fn slope_ok(ground: &ContactSnapshot, at: GlobalXZ, max_deg: f32) -> bool {
    let Some(h) = ground.height_at(at) else {
        return false;
    };
    let Some(hl) = ground.height_at(GlobalXZ::at(at.x - SLOPE_STEP_M, at.z)) else {
        return false;
    };
    let Some(hr) = ground.height_at(GlobalXZ::at(at.x + SLOPE_STEP_M, at.z)) else {
        return false;
    };
    let Some(hd) = ground.height_at(GlobalXZ::at(at.x, at.z - SLOPE_STEP_M)) else {
        return false;
    };
    let Some(hu) = ground.height_at(GlobalXZ::at(at.x, at.z + SLOPE_STEP_M)) else {
        return false;
    };
    let step = SLOPE_STEP_M as f32;
    let n = glam::Vec3::new(hl - hr, step * 2.0, hd - hu);
    if n.length_squared() < 1e-8 {
        return true;
    }
    n.normalize().y >= max_deg.to_radians().cos() && h.is_finite()
}

fn place_of(world: &World, pos: GlobalPosition, yaw: f32, scale: f32) -> EngineResult<Place> {
    let render = world.to_render(pos)?;
    Place::at(render.x, render.y, render.z)?
        .yaw_deg(yaw)?
        .scale(scale)
}

fn xz_dist(a: GlobalXZ, b: GlobalXZ) -> f64 {
    let dx = a.x - b.x;
    let dz = a.z - b.z;
    (dx * dx + dz * dz).sqrt()
}

fn wrap_deg(degrees: f32) -> f32 {
    let wrapped = degrees.rem_euclid(360.0);
    if wrapped >= 360.0 {
        0.0
    } else {
        wrapped
    }
}

fn angle_delta_deg(from: f32, to: f32) -> f32 {
    (to - from + 180.0).rem_euclid(360.0) - 180.0
}

fn turn_toward(yaw: f32, desired: f32, max_delta: f32) -> f32 {
    let delta = angle_delta_deg(yaw, desired).clamp(-max_delta, max_delta);
    wrap_deg(yaw + delta)
}

fn offset_xz(agent: &mut Agent, radius: f32) -> GlobalXZ {
    let ang = agent.rng.unit() * std::f32::consts::TAU;
    let dist = agent.rng.range(radius * 0.35, radius) as f64;
    GlobalXZ::at(
        agent.pos.x + f64::from(ang.cos()) * dist,
        agent.pos.z + f64::from(ang.sin()) * dist,
    )
}

fn rng_unit_hint(id: u32) -> f32 {
    ((id.wrapping_mul(0x9E37_79B9)) >> 8) as f32 / 16_777_216.0
}

fn load_species_model(
    path: PathBuf,
    root: PathBuf,
    id: String,
    clips: [String; 4],
) -> Result<Arc<AnimatedModel>, FaunaError> {
    let model = AnimatedModel::load_with(&path, &root, &EngineLimits::default())?;
    for clip in &clips {
        if model.find_clip(clip).is_none() {
            return Err(FaunaError::MissingClip {
                id,
                clip: clip.clone(),
                path,
            });
        }
    }
    Ok(Arc::new(model))
}

fn fauna_dir() -> Result<PathBuf, FaunaError> {
    let mut tried = Vec::new();
    if let Some(dir) = std::env::var_os("ORRUN_ASSETS") {
        tried.push(PathBuf::from(dir).join("fauna"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            tried.push(dir.join("assets").join("fauna"));
        }
    }
    tried.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join("fauna"),
    );
    for root in &tried {
        if root.is_dir() {
            return Ok(root.clone());
        }
    }
    Err(FaunaError::NoAssets(
        tried.into_iter().next().unwrap_or_default(),
    ))
}

fn catalog_path() -> Result<PathBuf, FaunaError> {
    let mut tried = Vec::new();
    if let Some(dir) = std::env::var_os("ORRUN_ASSETS") {
        tried.push(PathBuf::from(dir).join("catalog").join("fauna.json"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            tried.push(dir.join("assets").join("catalog").join("fauna.json"));
        }
    }
    tried.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join("catalog")
            .join("fauna.json"),
    );
    for path in &tried {
        if path.is_file() {
            return Ok(path.clone());
        }
    }
    Err(FaunaError::MissingCatalog(
        tried.into_iter().next().unwrap_or_default(),
    ))
}

#[derive(Deserialize)]
struct CatalogFile {
    fauna: Vec<CatalogEntry>,
}

#[derive(Deserialize)]
struct CatalogEntry {
    id: String,
    source: String,
    role: String,
    wilderness_spawn: bool,
    footprint: f32,
    scale: f32,
    max_slope_deg: f32,
    avoid_water: f32,
    #[serde(default)]
    biomes: BiomeWeights,
    density: f32,
    flock_min: u32,
    flock_max: u32,
    walk_speed: f32,
    run_speed: f32,
    flee_radius: f32,
    hunt_range: f32,
    catch_radius: f32,
    anims: CatalogAnims,
}

#[derive(Deserialize, Default)]
struct BiomeWeights {
    #[serde(default)]
    plains: f32,
    #[serde(default)]
    forest: f32,
    #[serde(default)]
    arid: f32,
    #[serde(default)]
    alpine: f32,
    #[serde(default)]
    coast: f32,
    #[serde(default)]
    wetland: f32,
    #[serde(default)]
    tundra: f32,
}

#[derive(Deserialize)]
struct CatalogAnims {
    idle: String,
    walk: String,
    run: String,
    eat: String,
}

fn spec_from_entry(entry: CatalogEntry) -> Result<FaunaSpec, FaunaError> {
    if entry.id.is_empty() {
        return Err(FaunaError::Catalog("empty fauna id".into()));
    }
    if entry.flock_min == 0 || entry.flock_min > entry.flock_max {
        return Err(FaunaError::Catalog(format!(
            "'{}' flock_min {} flock_max {}",
            entry.id, entry.flock_min, entry.flock_max
        )));
    }
    if entry.scale <= 0.0 || !entry.scale.is_finite() {
        return Err(FaunaError::Catalog(format!(
            "'{}' scale must be > 0",
            entry.id
        )));
    }
    let mut biome_weight = [0.0f32; BIOME_COUNT];
    biome_weight[Biome::Plains as u8 as usize] = entry.biomes.plains;
    biome_weight[Biome::Forest as u8 as usize] = entry.biomes.forest;
    biome_weight[Biome::Arid as u8 as usize] = entry.biomes.arid;
    biome_weight[Biome::Alpine as u8 as usize] = entry.biomes.alpine;
    biome_weight[Biome::Coast as u8 as usize] = entry.biomes.coast;
    biome_weight[Biome::Wetland as u8 as usize] = entry.biomes.wetland;
    biome_weight[Biome::Tundra as u8 as usize] = entry.biomes.tundra;
    Ok(FaunaSpec {
        id: entry.id,
        source: entry.source,
        role: FaunaRole::parse(&entry.role)?,
        wilderness_spawn: entry.wilderness_spawn,
        footprint: entry.footprint,
        scale: entry.scale,
        max_slope_deg: entry.max_slope_deg,
        avoid_water: entry.avoid_water,
        density: entry.density,
        biome_weight,
        flock_min: entry.flock_min,
        flock_max: entry.flock_max,
        walk_speed: entry.walk_speed,
        run_speed: entry.run_speed,
        flee_radius: entry.flee_radius,
        hunt_range: entry.hunt_range,
        catch_radius: entry.catch_radius,
        anim_idle: entry.anims.idle,
        anim_walk: entry.anims.walk,
        anim_run: entry.anims.run,
        anim_eat: entry.anims.eat,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atlas::ContinentAtlas;
    use crate::world::footprint::{BuildingPlot, HousePlot};
    use crate::world::ponds::PondField;
    use crate::world::settlement::HamletStand;
    use crate::world::surface::ContinentalSurface;
    use engine::space::GlobalXZ;

    fn catalog() -> FaunaCatalog {
        FaunaCatalog::load().expect("vendored fauna catalog")
    }

    fn surface() -> (ContinentalSurface, PondField) {
        let atlas = ContinentAtlas::generate(20260809, 64);
        let surface = ContinentalSurface::new(&atlas).expect("surface");
        let ponds = PondField::empty(GlobalXZ::at(0.0, 0.0));
        (surface, ponds)
    }

    #[test]
    fn catalog_names_the_orrun_species() {
        let catalog = catalog();
        assert_eq!(catalog.specs().len(), 12);
        for id in ["deer", "stag", "wolf", "fox", "horse", "horse_white"] {
            assert!(
                catalog.spec(id).wilderness_spawn,
                "{id} should roam the wilderness"
            );
        }
        for id in ["cow", "bull", "donkey", "alpaca", "husky", "shiba"] {
            assert!(
                !catalog.spec(id).wilderness_spawn,
                "{id} is settlement-only"
            );
        }
        assert!(catalog.spec("deer").is_prey());
        assert!(catalog.spec("wolf").is_predator());
        assert_eq!(catalog.spec("deer").biome_weight(Biome::Ocean), 0.0);
        assert!(catalog.spec("deer").biome_weight(Biome::Forest) > 0.5);
    }

    #[test]
    fn wilderness_meshes_carry_the_clips_the_catalog_names() {
        let catalog = catalog();
        let root = fauna_dir().expect("fauna dir");
        for id in ["deer", "stag", "wolf", "fox", "horse"] {
            let spec = catalog.spec(id);
            let path = root.join(spec.source.replace('/', std::path::MAIN_SEPARATOR_STR));
            let model = AnimatedModel::load_with(&path, &root, &EngineLimits::default())
                .unwrap_or_else(|e| panic!("load {id}: {e}"));
            for clip in [
                &spec.anim_idle,
                &spec.anim_walk,
                &spec.anim_run,
                &spec.anim_eat,
            ] {
                assert!(model.find_clip(clip).is_some(), "{id} missing clip {clip}");
            }
            let tinted = model.meshes.iter().any(|m| {
                m.colors
                    .iter()
                    .any(|c| c.x < 0.95 || c.y < 0.95 || c.z < 0.95)
            });
            assert!(
                tinted,
                "{id} stayed clay-white; baseColorFactor was dropped"
            );
        }
    }

    #[test]
    fn habitat_rejects_water() {
        let catalog = catalog();
        let deer = catalog.spec("deer");
        let (surface, ponds) = surface();
        let ground = ContactSnapshot::default();
        let span = surface.bounds().metres();
        for iz in 0..40 {
            for ix in 0..40 {
                let at = GlobalXZ::at(
                    span * (ix as f64 + 0.5) / 40.0,
                    span * (iz as f64 + 0.5) / 40.0,
                );
                if surface.column(at).is_wet() {
                    assert!(
                        !may_stand(
                            deer,
                            &surface,
                            &ponds,
                            &BuildingIndex::new(Vec::new()),
                            &[],
                            &ground,
                            at
                        ),
                        "deer stood on water at {at:?}"
                    );
                    assert_eq!(
                        suitability(
                            deer,
                            &surface,
                            &ponds,
                            &BuildingIndex::new(Vec::new()),
                            &[],
                            &ground,
                            at
                        ),
                        0.0
                    );
                    return;
                }
            }
        }
        panic!("seed 20260809 had no wet sample");
    }

    #[test]
    fn habitat_rejects_a_house_plot() {
        let catalog = catalog();
        let deer = catalog.spec("deer");
        let (surface, ponds) = surface();
        let ground = ContactSnapshot::default();
        let mut dry = None;
        let span = surface.bounds().metres();
        for iz in 0..40 {
            for ix in 0..40 {
                let at = GlobalXZ::at(
                    span * (ix as f64 + 0.5) / 40.0,
                    span * (iz as f64 + 0.5) / 40.0,
                );
                if !surface.column(at).is_wet() {
                    dry = Some(at);
                    break;
                }
            }
            if dry.is_some() {
                break;
            }
        }
        let at = dry.expect("dry ground");
        let plot = HousePlot {
            at,
            half_x: 4.0,
            half_z: 4.0,
            yaw: 0.0,
            floor_y: 10.0,
        };
        assert!(
            !may_stand(
                deer,
                &surface,
                &ponds,
                &BuildingIndex::new(vec![BuildingPlot::House(plot)]),
                &[],
                &ground,
                at
            ),
            "deer stood inside a house"
        );
    }

    #[test]
    fn habitat_rejects_a_hamlet_yard() {
        let catalog = catalog();
        let deer = catalog.spec("deer");
        let (surface, ponds) = surface();
        let ground = ContactSnapshot::default();
        let mut dry = None;
        let span = surface.bounds().metres();
        for iz in 0..40 {
            for ix in 0..40 {
                let at = GlobalXZ::at(
                    span * (ix as f64 + 0.5) / 40.0,
                    span * (iz as f64 + 0.5) / 40.0,
                );
                if !surface.column(at).is_wet() {
                    dry = Some(at);
                    break;
                }
            }
            if dry.is_some() {
                break;
            }
        }
        let at = dry.expect("dry ground");
        let hamlet = HamletStand {
            at,
            radius: 20.0,
            houses: vec![at],
            cut: Vec::new(),
        };
        let hamlets = [hamlet];
        let empty = BuildingIndex::new(Vec::new());
        assert!(
            !may_stand(deer, &surface, &ponds, &empty, &hamlets, &ground, at),
            "deer stood in the hamlet"
        );
        let near = GlobalXZ::at(at.x + 30.0, at.z);
        assert!(
            !may_stand(deer, &surface, &ponds, &empty, &hamlets, &ground, near),
            "deer stood next to the hamlet"
        );
    }
}
