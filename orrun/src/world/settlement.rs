//! Settlements on the continent: lab packing, door-sill seating, no flattened pads.
//!
//! Atlas settlement nodes are pins. Each nearby pin is packed at its atlas
//! tier (hamlet … port) on a worker thread, scored against the real ground,
//! then each dwelling is a Modular medieval kit seated with its door at grade.
//! Kit plinths take the downhill air. The heightfield is not rewritten — that
//! was the Godot trap that buried doors or left houses floating.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::thread::JoinHandle;

use engine::collision::ColliderLayer;
use engine::color::Color;
use engine::contact::ContactSnapshot;
use engine::error::{EngineError, EngineResult};
use engine::mesh::Mesh;
use engine::place::{GlobalPlace, Place};
use engine::space::{GlobalPosition, GlobalXZ, RenderOrigin};
use engine::world::{EntityId, World};
use glam::{Vec2, Vec3};
use modular::prelude::{PieceId, PlacedMesh};
use thiserror::Error;

use super::footprint::{BuildingIndex, BuildingPlot, CastlePlot, HousePlot};
use super::ponds::PondField;
use super::surface::{ContinentalSurface, SettlementPin, SurfaceColumn};
use super::world_stream::{WorldStream, NEAR};
use crate::hamlet::castle_kit;
use crate::hamlet::kit::{self, KitError};
use crate::hamlet::{
    castle_layout, plan_on, spec_for, HamletError, HamletLabConfig, Plan2D, Plot, Shape, ShapeKind,
    DOOR_SINK_M,
};

/// How far from the player a pin still gets a layout. Sized with the walked ring.
const REACH_M: f64 = NEAR.covers_m();
/// Kit instances are batched per this cell so frustum culling can drop a block.
const TILE_M: f64 = 128.0;
/// Loading waits until tiles this close are on the GPU; the rest stream in.
const NEARBY_TILE_M: f64 = 220.0;
/// Tile uploads per walking frame.
const MAX_TILES_PER_FRAME: usize = 16;
/// Tile uploads per loading frame, so the first street is up before walking.
const MAX_TILES_LOADING: usize = 64;
/// Keep house footprints off the atlas road bed (half of a primary ribbon plus a wall).
const ROAD_CLEAR_M: f32 = 4.0;
/// Extra metres past the outermost house where the dirt ribbon still pauses.
pub const HAMLET_ROAD_PAD_M: f32 = 10.0;
/// Engine collider layer for house walls and castle curtains.
const COLLIDER_LAYER: ColliderLayer = 2;

#[derive(Debug, Error)]
pub enum SettlementError {
    #[error(transparent)]
    Kit(#[from] KitError),

    #[error(transparent)]
    Engine(#[from] EngineError),

    #[error(transparent)]
    Hamlet(#[from] HamletError),
}

struct PieceProto {
    piece: PieceId,
    entity: EntityId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct TileKey {
    castle: bool,
    piece: usize,
    tx: i32,
    tz: i32,
}

#[derive(Clone, Copy)]
enum Kind {
    HousePiece(usize),
    CastlePiece(usize),
    Well,
}

#[derive(Clone, Copy)]
struct Standing {
    kind: Kind,
    at: GlobalPosition,
    yaw_deg: f32,
    door_id: Option<u64>,
}

struct SeatedCity {
    hamlet: HamletStand,
    plots: Vec<BuildingPlot>,
    standing: Vec<Standing>,
    doors: Vec<HouseDoor>,
}

/// A seated dwelling door the player can open.
#[derive(Clone, Debug)]
pub struct HouseDoor {
    pub id: u64,
    pub catalog_id: String,
    pub seed: u64,
    pub leaf_piece: String,
    pub at: GlobalPosition,
    pub closed_yaw_deg: f32,
    pub opening_width: f32,
    pub house_at: GlobalXZ,
    pub house_yaw_deg: f32,
    pub floor_y: f32,
    pub half_z: f32,
}

impl HouseDoor {
    pub fn closed_place(&self) -> GlobalPlace {
        GlobalPlace::at(self.at).with_yaw_deg(self.closed_yaw_deg)
    }

    pub fn leaf_place(&self, yaw_deg: f32) -> GlobalPlace {
        GlobalPlace::at(self.at).with_yaw_deg(yaw_deg)
    }

    pub fn opening_out(&self) -> GlobalPlace {
        GlobalPlace::at(self.opening_at()).with_yaw_deg(self.house_yaw_deg + 180.0)
    }

    fn opening_at(&self) -> GlobalPosition {
        let (dx, dz) = kit::yaw_xz(0.0, -self.half_z, self.house_yaw_deg);
        GlobalPosition::at(
            self.house_at.x + f64::from(dx),
            f64::from(self.floor_y + 1.1),
            self.house_at.z + f64::from(dz),
        )
    }
}

/// One seated hamlet, in world metres.
#[derive(Clone, Debug, PartialEq)]
pub struct HamletStand {
    pub at: GlobalXZ,
    /// Disk that covers the packed houses, for roads to pause inside.
    pub radius: f32,
    pub houses: Vec<GlobalXZ>,
}

impl HamletStand {
    /// True when `p` is inside the packed houses, plus `pad` metres beyond.
    pub fn covers(&self, p: GlobalXZ, pad: f32) -> bool {
        let dx = p.x - self.at.x;
        let dz = p.z - self.at.z;
        let r = f64::from(self.radius + pad);
        dx * dx + dz * dz < r * r
    }
}

struct GroundPlot<'a> {
    surface: &'a ContinentalSurface,
    ponds: &'a PondField,
    ground: Option<&'a ContactSnapshot>,
    origin: GlobalXZ,
    roads: Vec<(Vec2, Vec2)>,
}

impl GroundPlot<'_> {
    fn world(&self, local: Vec2) -> GlobalXZ {
        GlobalXZ::at(
            self.origin.x + f64::from(local.x),
            self.origin.z + f64::from(local.y),
        )
    }

    fn column(&self, p: GlobalXZ) -> SurfaceColumn {
        let mut column = self.surface.column(p);
        self.ponds.carve(p, &mut column);
        column
    }
}

impl Plot for GroundPlot<'_> {
    fn height(&self, p: Vec2) -> f32 {
        let at = self.world(p);
        if let Some(ground) = self.ground {
            if let Some(h) = ground.height_at(at) {
                return h;
            }
        }
        self.column(at).ground()
    }

    fn wetness(&self, p: Vec2) -> f32 {
        let at = self.world(p);
        let hydro = self.column(at).wetness();
        if on_road(
            Vec2::new(at.x as f32, at.z as f32),
            &self.roads,
            ROAD_CLEAR_M,
        ) {
            hydro.max(1.0)
        } else {
            hydro
        }
    }
}

struct Packing {
    seed: i32,
    pins: Vec<SettlementPin>,
    plans: HashMap<i32, Plan2D>,
    surface: Arc<ContinentalSurface>,
    ponds: Arc<PondField>,
    ground: ContactSnapshot,
    piece_ids: Vec<PieceId>,
    castle_piece_ids: Vec<PieceId>,
    dwelling_recipes: Arc<kit::DwellingRecipes>,
    castle_recipes: Arc<HashMap<String, Vec<PlacedMesh>>>,
}

struct PackResult {
    plans: HashMap<i32, Plan2D>,
    cities: Vec<(i32, SeatedCity)>,
}

struct Pending {
    job: JoinHandle<PackResult>,
}

/// Live hamlets around the player.
pub struct SettlementLayer {
    pieces: Vec<PieceProto>,
    castle_pieces: Vec<PieceProto>,
    dwelling_recipes: Arc<kit::DwellingRecipes>,
    castle_recipes: Arc<HashMap<String, Vec<PlacedMesh>>>,
    well: EntityId,
    seed: i32,
    seated: HashMap<i32, SeatedCity>,
    standing: Vec<Standing>,
    plans: HashMap<i32, Plan2D>,
    hamlets: Vec<HamletStand>,
    plot_index: Arc<BuildingIndex>,
    tiles: HashMap<TileKey, EntityId>,
    tile_items: HashMap<TileKey, Vec<(GlobalPosition, f32)>>,
    tile_queue: VecDeque<TileKey>,
    queued: HashSet<TileKey>,
    pending: Option<Pending>,
    doors: Vec<HouseDoor>,
    hidden_door: Option<u64>,
}

impl SettlementLayer {
    /// Upload kit piece meshes and the well once; nothing is seated yet.
    pub fn install(world: &mut World, seed: i32) -> Result<Self, SettlementError> {
        let catalog = kit::catalog();
        let dwelling_recipes = Arc::new(kit::DwellingRecipes::roll(&catalog)?);
        let pieces = spawn_batches(world, kit::PIECE_GLBS, kit::load_piece_mesh)?;
        let castle_catalog = castle_kit::catalog();
        let castle_recipes = castle_kit::castle_recipes(&castle_catalog)?;
        let castle_pieces =
            spawn_batches(world, castle_kit::PIECE_GLBS, castle_kit::load_piece_mesh)?;

        let well = world.spawn_instanced(well_mesh()?);
        Ok(Self {
            pieces,
            castle_pieces,
            dwelling_recipes,
            castle_recipes: Arc::new(castle_recipes),
            well,
            seed,
            seated: HashMap::new(),
            standing: Vec::new(),
            plans: HashMap::new(),
            hamlets: Vec::new(),
            plot_index: Arc::new(BuildingIndex::new(Vec::new())),
            tiles: HashMap::new(),
            tile_items: HashMap::new(),
            tile_queue: VecDeque::new(),
            queued: HashSet::new(),
            pending: None,
            doors: Vec::new(),
            hidden_door: None,
        })
    }

    pub fn placed_count(&self) -> usize {
        self.hamlets.iter().map(|h| h.houses.len()).sum()
    }

    /// Seated hamlets in the current window, for footbridges across a split river.
    pub fn hamlets(&self) -> &[HamletStand] {
        &self.hamlets
    }

    /// Doors on seated dwellings.
    pub fn doors(&self) -> &[HouseDoor] {
        &self.doors
    }

    /// Hide the instanced leaf that a unique swinging entity now owns.
    pub fn hide_leaf(&mut self, world: &mut World, door_id: Option<u64>) -> EngineResult<()> {
        if self.hidden_door == door_id {
            return Ok(());
        }
        self.hidden_door = door_id;
        self.rebuild_tile_items();
        self.rewrite_tiles(world)
    }

    /// House and castle footprints, for interior ground caps and scatter.
    pub fn plots(&self) -> &[BuildingPlot] {
        self.plot_index.plots()
    }

    /// Shared spatial index. Scatter and fauna clone the Arc, not the plots.
    pub fn plot_index(&self) -> Arc<BuildingIndex> {
        Arc::clone(&self.plot_index)
    }

    /// A layout is packing on a worker. The loading screen waits; walking does not.
    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }

    /// Nearby kit tiles are not on the GPU yet. Loading waits; walking streams them.
    pub fn staging(&self, focus: GlobalXZ) -> bool {
        self.tile_queue
            .iter()
            .any(|key| tile_dist_m(*key, focus) <= NEARBY_TILE_M)
    }

    pub fn tile_gpu_count(&self) -> usize {
        self.tiles.len()
    }

    pub fn tile_backlog(&self) -> usize {
        self.tile_queue.len()
    }

    pub fn clear(&mut self, world: &mut World) -> EngineResult<()> {
        self.pending = None;
        for entity in self.tiles.values().copied() {
            world.despawn(entity);
        }
        self.tiles.clear();
        self.tile_items.clear();
        self.tile_queue.clear();
        self.queued.clear();
        world.set_instances(self.well, &[])?;
        self.standing.clear();
        self.seated.clear();
        self.plans.clear();
        self.hamlets.clear();
        self.doors.clear();
        self.hidden_door = None;
        self.plot_index = Arc::new(BuildingIndex::new(Vec::new()));
        self.sync_building_colliders(world);
        Ok(())
    }

    /// Keep hamlets around the player. New pins pack once on a worker; seated
    /// cities stay until they leave reach. Kit instances upload by spatial tile.
    pub fn follow(
        &mut self,
        world: &mut World,
        stream: &WorldStream,
        surface: &Arc<ContinentalSurface>,
        ponds: &Arc<PondField>,
        focus: GlobalXZ,
        rebased: bool,
    ) -> EngineResult<bool> {
        let nearby = nearby_pins(surface, focus);
        let nearby_ids: HashSet<i32> = nearby.iter().map(|pin| pin.id).collect();
        let mut plots_changed = false;

        if let Some(pending) = self.pending.take() {
            if pending.job.is_finished() {
                let bake = pending.job.join().expect("settlement thread");
                if self.install_cities(bake, &nearby_ids) {
                    plots_changed = true;
                }
            } else {
                self.pending = Some(pending);
            }
        }

        if self.drop_far_pins(&nearby_ids) {
            plots_changed = true;
        }

        if plots_changed {
            self.rebuild_live();
            self.sync_building_colliders(world);
            self.sync_tile_set(world, focus)?;
        }

        if self.pending.is_none() {
            let new_pins: Vec<SettlementPin> = nearby
                .into_iter()
                .filter(|pin| !self.seated.contains_key(&pin.id))
                .collect();
            if !new_pins.is_empty() {
                let mut plans = HashMap::new();
                for pin in &new_pins {
                    if let Some(plan) = self.plans.get(&pin.id) {
                        plans.insert(pin.id, plan.clone());
                    }
                }
                let packing = Packing {
                    seed: self.seed,
                    pins: new_pins,
                    plans,
                    surface: Arc::clone(surface),
                    ponds: Arc::clone(ponds),
                    ground: stream.contact_snapshot(),
                    piece_ids: self
                        .pieces
                        .iter()
                        .map(|batch| batch.piece.clone())
                        .collect(),
                    castle_piece_ids: self
                        .castle_pieces
                        .iter()
                        .map(|batch| batch.piece.clone())
                        .collect(),
                    dwelling_recipes: Arc::clone(&self.dwelling_recipes),
                    castle_recipes: Arc::clone(&self.castle_recipes),
                };
                self.pending = Some(Pending {
                    job: std::thread::Builder::new()
                        .name("settlements".into())
                        .spawn(move || packing.pack())
                        .expect("settlement thread"),
                });
            }
        }

        if rebased {
            self.rewrite_tiles(world)?;
            self.stand_wells(world)?;
        } else if plots_changed {
            self.stand_wells(world)?;
        }

        let loading = self.staging(focus) || self.pending.is_some();
        let budget = if loading {
            MAX_TILES_LOADING
        } else {
            MAX_TILES_PER_FRAME
        };
        self.drain_tiles(world, focus, budget)?;
        Ok(plots_changed)
    }

    fn install_cities(&mut self, bake: PackResult, nearby_ids: &HashSet<i32>) -> bool {
        self.plans.extend(bake.plans);
        let mut changed = false;
        for (id, city) in bake.cities {
            if nearby_ids.contains(&id) {
                self.seated.insert(id, city);
                changed = true;
            }
        }
        changed
    }

    fn drop_far_pins(&mut self, nearby_ids: &HashSet<i32>) -> bool {
        let before = self.seated.len();
        self.seated.retain(|id, _| nearby_ids.contains(id));
        before != self.seated.len()
    }

    fn rebuild_live(&mut self) {
        self.standing.clear();
        self.hamlets.clear();
        self.doors.clear();
        let mut plots = Vec::new();
        for city in self.seated.values() {
            self.standing.extend_from_slice(&city.standing);
            self.hamlets.push(city.hamlet.clone());
            self.doors.extend_from_slice(&city.doors);
            plots.extend_from_slice(&city.plots);
        }
        self.plot_index = Arc::new(BuildingIndex::new(plots));
        self.rebuild_tile_items();
    }

    fn sync_building_colliders(&self, world: &mut World) {
        world
            .collision_mut()
            .replace_layer(COLLIDER_LAYER, self.plot_index.colliders())
            .expect("building colliders");
    }

    fn rebuild_tile_items(&mut self) {
        self.tile_items.clear();
        for item in &self.standing {
            if item.door_id.is_some() && item.door_id == self.hidden_door {
                continue;
            }
            let Some(key) = tile_key(item) else {
                continue;
            };
            self.tile_items
                .entry(key)
                .or_default()
                .push((item.at, item.yaw_deg));
        }
    }

    fn sync_tile_set(&mut self, world: &mut World, focus: GlobalXZ) -> EngineResult<()> {
        let live: HashSet<TileKey> = self.tile_items.keys().copied().collect();
        self.tiles.retain(|key, entity| {
            if live.contains(key) {
                true
            } else {
                world.despawn(*entity);
                false
            }
        });
        self.tile_queue
            .retain(|key| live.contains(key) && !self.tiles.contains_key(key));
        self.queued
            .retain(|key| live.contains(key) && !self.tiles.contains_key(key));
        for key in live {
            if self.tiles.contains_key(&key) || self.queued.contains(&key) {
                continue;
            }
            self.queued.insert(key);
            self.tile_queue.push_back(key);
        }
        let mut queued: Vec<TileKey> = self.tile_queue.drain(..).collect();
        queued.sort_by_key(|key| {
            let near = if tile_dist_m(*key, focus) <= NEARBY_TILE_M {
                0
            } else {
                1
            };
            (near, tile_dist_key(*key, focus))
        });
        self.tile_queue = queued.into();
        Ok(())
    }

    fn rewrite_tiles(&mut self, world: &mut World) -> EngineResult<()> {
        let origin = world.render_origin();
        for (key, entity) in &self.tiles {
            let Some(items) = self.tile_items.get(key) else {
                continue;
            };
            let places = places_of(items, origin);
            world.set_instances(*entity, &places)?;
        }
        Ok(())
    }

    fn drain_tiles(
        &mut self,
        world: &mut World,
        focus: GlobalXZ,
        budget: usize,
    ) -> EngineResult<()> {
        if !self.tile_queue.is_empty() {
            let mut queued: Vec<TileKey> = self.tile_queue.drain(..).collect();
            queued.sort_by_key(|key| {
                let near = if tile_dist_m(*key, focus) <= NEARBY_TILE_M {
                    0
                } else {
                    1
                };
                (near, tile_dist_key(*key, focus))
            });
            self.tile_queue = queued.into();
        }
        let origin = world.render_origin();
        let mut left = budget;
        while left > 0 {
            let Some(key) = self.tile_queue.pop_front() else {
                break;
            };
            self.queued.remove(&key);
            if self.tiles.contains_key(&key) {
                continue;
            }
            let Some(items) = self.tile_items.get(&key) else {
                continue;
            };
            let proto = if key.castle {
                self.castle_pieces[key.piece].entity
            } else {
                self.pieces[key.piece].entity
            };
            let entity = world.spawn_instanced_like(proto)?;
            let places = places_of(items, origin);
            world.set_instances(entity, &places)?;
            self.tiles.insert(key, entity);
            left -= 1;
        }
        Ok(())
    }

    fn stand_wells(&mut self, world: &mut World) -> EngineResult<()> {
        let origin = world.render_origin();
        let mut places = Vec::new();
        for item in &self.standing {
            if !matches!(item.kind, Kind::Well) {
                continue;
            }
            let Ok(render) = item.at.to_render(origin) else {
                continue;
            };
            let at = render.vec3();
            places.push(Place::new(at.x, at.y, at.z).with_yaw_deg(item.yaw_deg));
        }
        world.set_instances(self.well, &places)?;
        Ok(())
    }
}

impl Packing {
    fn pack(self) -> PackResult {
        let mut plans = self.plans;
        let mut cities = Vec::new();
        for pin in self.pins {
            let plan = plans.entry(pin.id).or_insert_with(|| {
                layout_for(self.seed, pin, &self.surface, &self.ponds)
                    .unwrap_or_else(|err| panic!("hamlet at node {} failed: {err}", pin.id))
            });
            let mut standing = Vec::new();
            let mut houses = Vec::new();
            let mut plots = Vec::new();
            let mut doors = Vec::new();
            seat_plan(
                plan,
                pin,
                &self.surface,
                &self.ponds,
                &self.ground,
                &self.piece_ids,
                &self.castle_piece_ids,
                self.seed,
                &self.dwelling_recipes,
                &self.castle_recipes,
                &mut standing,
                &mut houses,
                &mut plots,
                &mut doors,
            );
            let radius = hamlet_radius(pin.at, &houses);
            cities.push((
                pin.id,
                SeatedCity {
                    hamlet: HamletStand {
                        at: pin.at,
                        radius,
                        houses,
                    },
                    plots,
                    standing,
                    doors,
                },
            ));
        }
        PackResult { plans, cities }
    }
}

fn nearby_pins(surface: &ContinentalSurface, focus: GlobalXZ) -> Vec<SettlementPin> {
    let reach_sq = REACH_M * REACH_M;
    surface
        .settlements()
        .iter()
        .copied()
        .filter(|pin| {
            let dx = pin.at.x - focus.x;
            let dz = pin.at.z - focus.z;
            dx * dx + dz * dz <= reach_sq
        })
        .collect()
}

fn layout_for(
    world_seed: i32,
    pin: SettlementPin,
    surface: &ContinentalSurface,
    ponds: &PondField,
) -> Result<Plan2D, HamletError> {
    let mut config = layout_config(pin);
    config.seed = plan_seed(world_seed, pin.id);
    let plot = GroundPlot {
        surface,
        ponds,
        ground: None,
        origin: pin.at,
        roads: nearby_road_segs(surface, pin.at, config.max_settle_radius + 24.0),
    };
    plan_on(&config, Some(&plot))
}

fn seat_plan(
    plan: &Plan2D,
    pin: SettlementPin,
    surface: &ContinentalSurface,
    ponds: &PondField,
    ground: &ContactSnapshot,
    piece_ids: &[PieceId],
    castle_piece_ids: &[PieceId],
    world_seed: i32,
    dwelling_recipes: &kit::DwellingRecipes,
    castle_recipes: &HashMap<String, Vec<PlacedMesh>>,
    out: &mut Vec<Standing>,
    houses: &mut Vec<GlobalXZ>,
    plots: &mut Vec<BuildingPlot>,
    doors: &mut Vec<HouseDoor>,
) {
    let plot = GroundPlot {
        surface,
        ponds,
        ground: Some(ground),
        origin: pin.at,
        roads: nearby_road_segs(surface, pin.at, layout_config(pin).max_settle_radius + 24.0),
    };
    for shape in &plan.shapes {
        match shape.kind {
            ShapeKind::Market => continue,
            ShapeKind::Castle => seat_castle(
                shape,
                pin,
                &plot,
                castle_piece_ids,
                castle_recipes,
                out,
                houses,
                plots,
            ),
            ShapeKind::House => seat_house(
                shape,
                pin,
                &plot,
                piece_ids,
                world_seed,
                dwelling_recipes,
                out,
                houses,
                plots,
                doors,
            ),
        }
    }
}

fn seat_house(
    shape: &Shape,
    pin: SettlementPin,
    plot: &GroundPlot<'_>,
    piece_ids: &[PieceId],
    world_seed: i32,
    dwelling_recipes: &kit::DwellingRecipes,
    out: &mut Vec<Standing>,
    houses: &mut Vec<GlobalXZ>,
    plots: &mut Vec<BuildingPlot>,
    doors: &mut Vec<HouseDoor>,
) {
    let Some(spec) = spec_for(&shape.catalog_id) else {
        panic!(
            "planned building '{}' is not in the catalog",
            shape.catalog_id
        );
    };
    let sample = crate::hamlet::sample_footprint(
        plot,
        shape.center,
        shape.half_size.x,
        shape.half_size.y,
        shape.yaw,
    );
    let Some(seat) = crate::hamlet::seat_building(&sample, spec.foundation_m) else {
        return;
    };
    let yaw_deg = shape.yaw.to_degrees();
    let x = pin.at.x + f64::from(shape.center.x);
    let z = pin.at.z + f64::from(shape.center.y);
    houses.push(GlobalXZ::at(x, z));

    if spec.id == "Well" {
        out.push(Standing {
            kind: Kind::Well,
            at: GlobalPosition::at(x, f64::from(seat.floor_z), z),
            yaw_deg,
            door_id: None,
        });
        return;
    }
    if spec.is_civic() {
        return;
    }

    let seed = house_seed(world_seed, pin.id, shape.center.x, shape.center.y);
    let recipe = dwelling_recipes.get(spec.id, seed);
    let floor_y = seat.floor_z - DOOR_SINK_M;
    plots.push(BuildingPlot::House(HousePlot {
        at: GlobalXZ::at(x, z),
        half_x: shape.half_size.x,
        half_z: shape.half_size.y,
        yaw: shape.yaw,
        floor_y,
    }));
    let id = door_key(pin.id, x, z);
    for item in recipe {
        let p = item.place.position;
        let (dx, dz) = kit::yaw_xz(p.x, p.z, yaw_deg);
        let index = piece_ids
            .iter()
            .position(|pid| *pid == item.piece)
            .unwrap_or_else(|| panic!("kit piece {} was not uploaded", item.piece));
        let leaf = item.piece.as_str() == "door_plank" || item.piece.as_str() == "door_sturdy";
        let at = GlobalPosition::at(
            x + f64::from(dx),
            f64::from(floor_y + p.y),
            z + f64::from(dz),
        );
        if leaf {
            doors.push(HouseDoor {
                id,
                catalog_id: spec.id.to_string(),
                seed,
                leaf_piece: item.piece.to_string(),
                at,
                closed_yaw_deg: yaw_deg + item.place.yaw_degrees,
                opening_width: if item.piece.as_str() == "door_sturdy" {
                    1.05
                } else {
                    1.1
                },
                house_at: GlobalXZ::at(x, z),
                house_yaw_deg: yaw_deg,
                floor_y,
                half_z: shape.half_size.y,
            });
        }
        out.push(Standing {
            kind: Kind::HousePiece(index),
            at,
            yaw_deg: yaw_deg + item.place.yaw_degrees,
            door_id: leaf.then_some(id),
        });
    }
}

fn seat_castle(
    shape: &Shape,
    pin: SettlementPin,
    plot: &GroundPlot<'_>,
    castle_piece_ids: &[PieceId],
    castle_recipes: &HashMap<String, Vec<PlacedMesh>>,
    out: &mut Vec<Standing>,
    houses: &mut Vec<GlobalXZ>,
    plots: &mut Vec<BuildingPlot>,
) {
    let Some(spec) = spec_for(&shape.catalog_id) else {
        panic!(
            "planned castle '{}' is not in the catalog",
            shape.catalog_id
        );
    };
    if !spec.is_castle() {
        panic!("'{}' is not a castle", spec.id);
    }
    let layout = castle_layout(&shape.catalog_id)
        .unwrap_or_else(|| panic!("'{}' has no castle layout", shape.catalog_id));
    let sample = crate::hamlet::sample_castle_footprint(plot, shape.center, shape.yaw, layout);
    let Some(seat) = crate::hamlet::seat_building(&sample, spec.foundation_m) else {
        panic!(
            "castle '{}' at node {} refused its plot (wet, steep, or too much relief)",
            spec.id, pin.id
        );
    };
    let yaw_deg = shape.yaw.to_degrees();
    let x = pin.at.x + f64::from(shape.center.x);
    let z = pin.at.z + f64::from(shape.center.y);
    houses.push(GlobalXZ::at(x, z));
    let floor_y = seat.floor_z - DOOR_SINK_M;
    plots.push(BuildingPlot::Castle(CastlePlot {
        at: GlobalXZ::at(x, z),
        half_x: layout.size_x() * 0.5,
        half_z: layout.size_z() * 0.5,
        yaw: shape.yaw,
        floor_y,
        wall_m: layout.wall_m,
        keep_offset: layout.keep_offset,
        keep_half_x: layout.keep_half_x,
        keep_half_z: layout.keep_half_z,
    }));
    let recipe = castle_recipes
        .get(spec.id)
        .unwrap_or_else(|| panic!("castle '{}' has no modular recipe", spec.id));
    for item in recipe {
        let p = item.place.position;
        let (dx, dz) = kit::yaw_xz(p.x, p.z, yaw_deg);
        let index = castle_piece_ids
            .iter()
            .position(|id| *id == item.piece)
            .unwrap_or_else(|| panic!("castle kit piece {} was not uploaded", item.piece));
        out.push(Standing {
            kind: Kind::CastlePiece(index),
            at: GlobalPosition::at(
                x + f64::from(dx),
                f64::from(floor_y + p.y),
                z + f64::from(dz),
            ),
            yaw_deg: yaw_deg + item.place.yaw_degrees,
            door_id: None,
        });
    }
}

fn layout_config(pin: SettlementPin) -> HamletLabConfig {
    let mut config = HamletLabConfig::default();
    config.apply_tier_defaults(pin.tier);
    config.place_castle = true;
    config
}

fn plan_seed(world_seed: i32, node_id: i32) -> u64 {
    (world_seed as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (node_id as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9)
}

fn door_key(pin_id: i32, x: f64, z: f64) -> u64 {
    (pin_id as u64).wrapping_mul(0x9E37_79B9) ^ x.to_bits() ^ z.to_bits().rotate_left(17)
}

fn house_seed(world_seed: i32, node_id: i32, cx: f32, cz: f32) -> u64 {
    plan_seed(world_seed, node_id)
        ^ (u64::from(cx.to_bits())).wrapping_mul(0xD1B5_4A32_D192_ED03)
        ^ (u64::from(cz.to_bits())).rotate_left(21)
}

fn spawn_batches(
    world: &mut World,
    glbs: &[(&str, &str)],
    load: fn(&PieceId) -> Result<Mesh, KitError>,
) -> Result<Vec<PieceProto>, SettlementError> {
    let mut pieces = Vec::new();
    for (id, _) in glbs {
        let piece = PieceId::new(*id).unwrap_or_else(|err| panic!("{err}"));
        let mesh = load(&piece)?;
        pieces.push(PieceProto {
            piece,
            entity: world.spawn_instanced(mesh),
        });
    }
    Ok(pieces)
}

fn well_mesh() -> EngineResult<Mesh> {
    Mesh::box_at(
        Vec3::new(0.0, 0.5, 0.0),
        Vec3::new(1.6, 1.0, 1.6),
        Color::rgb(118, 110, 98),
    )
}

fn tile_key(item: &Standing) -> Option<TileKey> {
    let (tx, tz) = (
        (item.at.x / TILE_M).floor() as i32,
        (item.at.z / TILE_M).floor() as i32,
    );
    match item.kind {
        Kind::HousePiece(piece) => Some(TileKey {
            castle: false,
            piece,
            tx,
            tz,
        }),
        Kind::CastlePiece(piece) => Some(TileKey {
            castle: true,
            piece,
            tx,
            tz,
        }),
        Kind::Well => None,
    }
}

fn tile_center(key: TileKey) -> GlobalXZ {
    GlobalXZ::at(
        (f64::from(key.tx) + 0.5) * TILE_M,
        (f64::from(key.tz) + 0.5) * TILE_M,
    )
}

fn tile_dist_m(key: TileKey, focus: GlobalXZ) -> f64 {
    let at = tile_center(key);
    let dx = at.x - focus.x;
    let dz = at.z - focus.z;
    (dx * dx + dz * dz).sqrt()
}

fn tile_dist_key(key: TileKey, focus: GlobalXZ) -> i64 {
    let at = tile_center(key);
    let dx = at.x - focus.x;
    let dz = at.z - focus.z;
    (dx * dx + dz * dz).round() as i64
}

fn places_of(items: &[(GlobalPosition, f32)], origin: RenderOrigin) -> Vec<Place> {
    let mut places = Vec::with_capacity(items.len());
    for &(at, yaw_deg) in items {
        let Ok(render) = at.to_render(origin) else {
            continue;
        };
        let p = render.vec3();
        places.push(Place::new(p.x, p.y, p.z).with_yaw_deg(yaw_deg));
    }
    places
}

fn hamlet_radius(at: GlobalXZ, houses: &[GlobalXZ]) -> f32 {
    let mut r = 20.0_f32;
    for h in houses {
        let dx = (h.x - at.x) as f32;
        let dz = (h.z - at.z) as f32;
        r = r.max((dx * dx + dz * dz).sqrt());
    }
    r + HAMLET_ROAD_PAD_M
}

fn nearby_road_segs(
    surface: &ContinentalSurface,
    origin: GlobalXZ,
    reach: f32,
) -> Vec<(Vec2, Vec2)> {
    let o = Vec2::new(origin.x as f32, origin.z as f32);
    let mut segs = Vec::new();
    for road in surface.roads() {
        for w in road.points.windows(2) {
            if dist_point_seg(o, w[0], w[1]) <= reach {
                segs.push((w[0], w[1]));
            }
        }
    }
    segs
}

fn on_road(p: Vec2, segs: &[(Vec2, Vec2)], clear: f32) -> bool {
    segs.iter().any(|&(a, b)| dist_point_seg(p, a, b) < clear)
}

fn dist_point_seg(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    let d = b - a;
    let len2 = d.length_squared();
    if len2 < 1e-8 {
        return p.distance(a);
    }
    let t = ((p - a).dot(d) / len2).clamp(0.0, 1.0);
    p.distance(a + d * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pin(tier: u8) -> SettlementPin {
        SettlementPin {
            id: 1,
            at: GlobalXZ::at(0.0, 0.0),
            tier,
            population: 12,
        }
    }

    #[test]
    fn covers_rejects_a_point_inside_the_hamlet_disk() {
        let hamlet = HamletStand {
            at: GlobalXZ::at(0.0, 0.0),
            radius: 20.0,
            houses: vec![],
        };
        let inside = GlobalXZ::at(0.0, 5.0);
        assert!(
            hamlet.covers(inside, HAMLET_ROAD_PAD_M),
            "a point inside the packed disk plus road pad must be covered"
        );
        let outside = GlobalXZ::at(0.0, 80.0);
        assert!(!hamlet.covers(outside, HAMLET_ROAD_PAD_M));
    }

    #[test]
    fn a_town_layout_asks_for_more_houses_than_a_hamlet() {
        let hamlet = layout_config(pin(0));
        let town = layout_config(pin(2));
        assert_eq!(hamlet.tier, 0);
        assert_eq!(town.tier, 2);
        assert!(town.dwelling_min > hamlet.dwelling_max);
        assert!(town.max_settle_radius > hamlet.max_settle_radius);
        assert!(town.market_radius > hamlet.market_radius);
        assert!(hamlet.place_castle);
        assert!(town.place_castle);
        assert!(crate::hamlet::castle_id_for_tier(hamlet.tier).is_none());
        assert_eq!(
            crate::hamlet::castle_id_for_tier(town.tier),
            Some("castle_keep_12x10")
        );
    }

    #[test]
    fn a_port_layout_asks_for_a_thousand_houses() {
        let port = layout_config(pin(3));
        assert_eq!(port.tier, 3);
        assert!(port.dwelling_min >= 500);
        assert!(port.dwelling_max >= 1000);
        assert_eq!(
            crate::hamlet::castle_id_for_tier(port.tier),
            Some("castle_keep_16x14")
        );
    }

    #[test]
    fn a_point_on_a_road_centreline_is_blocked() {
        let segs = [(Vec2::ZERO, Vec2::new(100.0, 0.0))];
        assert!(on_road(Vec2::new(50.0, 0.0), &segs, ROAD_CLEAR_M));
        assert!(!on_road(Vec2::new(50.0, 8.0), &segs, ROAD_CLEAR_M));
    }

    #[test]
    fn hamlet_radius_covers_the_outer_house() {
        let at = GlobalXZ::at(0.0, 0.0);
        let houses = vec![GlobalXZ::at(30.0, 0.0), GlobalXZ::at(0.0, 10.0)];
        let r = hamlet_radius(at, &houses);
        assert!(r >= 40.0);
        assert!(r < 50.0);
    }

    #[test]
    fn house_and_castle_tiles_do_not_share_a_batch() {
        let at = GlobalPosition::at(130.0, 0.0, 10.0);
        let house = tile_key(&Standing {
            kind: Kind::HousePiece(0),
            at,
            yaw_deg: 0.0,
            door_id: None,
        })
        .expect("house piece");
        let castle = tile_key(&Standing {
            kind: Kind::CastlePiece(0),
            at,
            yaw_deg: 0.0,
            door_id: None,
        })
        .expect("castle piece");
        assert_ne!(house, castle);
        assert!(!house.castle);
        assert!(castle.castle);
    }
}
