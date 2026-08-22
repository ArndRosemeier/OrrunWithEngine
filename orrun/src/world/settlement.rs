//! Settlements on the continent: lab packing, door-sill seating, no flattened pads.
//!
//! Atlas settlement nodes are pins. Each nearby pin is packed at its atlas
//! tier (hamlet … port) on a worker thread, scored against the real ground,
//! then each dwelling is a Modular medieval kit seated with its door at grade.
//! Kit plinths take the downhill air. The heightfield is not rewritten — that
//! was the Godot trap that buried doors or left houses floating.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;

use engine::collision::ColliderLayer;
use engine::color::Color;
use engine::contact::ContactSnapshot;
use engine::error::{EngineError, EngineResult};
use engine::mesh::Mesh;
use engine::model::Model;
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
    castle_layout, generate_dwelling, plan_on, spec_for, DwellingBrief, HamletError, HamletLabConfig,
    Plan2D, Plot, Shape, ShapeKind, DOOR_SINK_M, FOUNDATION_M,
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
pub(crate) const ROAD_CLEAR_M: f32 = 4.0;
/// Extra metres past the outermost house. Atlas dirt still pauses here; the cut ribbon does not.
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
    /// Linear multiply for house paint variety (white = authored look).
    tint: Color,
    door_id: Option<u64>,
}

struct SeatedCity {
    hamlet: HamletStand,
    plots: Vec<BuildingPlot>,
    standing: Vec<Standing>,
    doors: Vec<HouseDoor>,
}

struct CampLive {
    tent: EntityId,
    ring: EntityId,
    tent_at: GlobalPosition,
    ring_at: GlobalPosition,
}

struct YardPose {
    tent_xz: GlobalXZ,
    ring_xz: GlobalXZ,
    yaw_deg: f32,
}

/// A seated dwelling door the player can open.
#[derive(Clone, Debug)]
pub struct HouseDoor {
    pub id: u64,
    pub brief: DwellingBrief,
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

    /// Feet stand clear of the front wall, on the street side of the leaf.
    ///
    /// Uses house −local Z (the opening face), not `at - house_at`, so a
    /// high leaf origin or jetty leaf cannot pull the stand onto the roof.
    pub fn outside_stand(&self) -> GlobalXZ {
        let (ox, oz) = kit::yaw_xz(0.0, -(self.half_z + 2.2), self.house_yaw_deg);
        GlobalXZ::at(self.house_at.x + f64::from(ox), self.house_at.z + f64::from(oz))
    }

    /// Just inside the threshold, used to walk through the open portal.
    pub fn enter_stand(&self) -> GlobalXZ {
        let (ix, iz) = kit::yaw_xz(0.0, -(self.half_z - 0.75), self.house_yaw_deg);
        GlobalXZ::at(self.house_at.x + f64::from(ix), self.house_at.z + f64::from(iz))
    }

    /// Mid-doorway look target (handle height), never the roof.
    pub fn look_target(&self) -> glam::Vec3 {
        glam::Vec3::new(self.at.x as f32, self.floor_y + 1.15, self.at.z as f32)
    }

    /// Look into the room from the threshold: halfway to the house centre, waist height.
    pub fn room_look_target(&self) -> glam::Vec3 {
        glam::Vec3::new(
            (self.at.x + self.house_at.x) as f32 * 0.5,
            self.floor_y + 0.85,
            (self.at.z + self.house_at.z) as f32 * 0.5,
        )
    }
}

/// One seated hamlet, in world metres.
#[derive(Clone, Debug, PartialEq)]
pub struct HamletStand {
    pub at: GlobalXZ,
    /// Disk that covers the packed houses. Atlas samples still pause inside;
    /// the village street is the dirt cut to the well, stored on `cut`.
    pub radius: f32,
    pub houses: Vec<GlobalXZ>,
    /// World-XZ polyline from the hamlet rim to the plaza / well.
    pub cut: Vec<Vec2>,
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
    plans: HashMap<i32, (Plan2D, Vec<Vec2>)>,
    surface: Arc<ContinentalSurface>,
    ponds: Arc<PondField>,
    ground: ContactSnapshot,
    piece_ids: Vec<PieceId>,
    castle_piece_ids: Vec<PieceId>,
    castle_recipes: Arc<HashMap<String, Vec<PlacedMesh>>>,
}

struct PackResult {
    plans: HashMap<i32, (Plan2D, Vec<Vec2>)>,
    cities: Vec<(i32, SeatedCity)>,
    /// Pins that could not be laid out this pass; do not respawn every frame.
    failed: Vec<i32>,
}

struct Pending {
    job: JoinHandle<PackResult>,
}

/// Live hamlets around the player.
pub struct SettlementLayer {
    pieces: Vec<PieceProto>,
    castle_pieces: Vec<PieceProto>,
    castle_recipes: Arc<HashMap<String, Vec<PlacedMesh>>>,
    well: EntityId,
    seed: i32,
    seated: HashMap<i32, SeatedCity>,
    standing: Vec<Standing>,
    plans: HashMap<i32, (Plan2D, Vec<Vec2>)>,
    hamlets: Vec<HamletStand>,
    plot_index: Arc<BuildingIndex>,
    tiles: HashMap<TileKey, EntityId>,
    tile_items: HashMap<TileKey, Vec<(GlobalPosition, f32, Color)>>,
    tile_queue: VecDeque<TileKey>,
    queued: HashSet<TileKey>,
    pending: Option<Pending>,
    /// Layout failed for these pins while they were in reach; cleared when they leave.
    unseatable: HashSet<i32>,
    doors: Vec<HouseDoor>,
    hidden_door: Option<u64>,
    camps: HashMap<i32, CampLive>,
    tent_mesh: Option<Mesh>,
    ring_mesh: Option<Mesh>,
}

impl SettlementLayer {
    /// Upload kit piece meshes and the well once; nothing is seated yet.
    pub fn install(world: &mut World, seed: i32) -> Result<Self, SettlementError> {
        let pieces = spawn_batches(world, kit::PIECE_GLBS, kit::load_piece_mesh)?;
        let castle_catalog = castle_kit::catalog();
        let castle_recipes = castle_kit::castle_recipes(&castle_catalog)?;
        let castle_pieces =
            spawn_batches(world, castle_kit::PIECE_GLBS, castle_kit::load_piece_mesh)?;

        let well = world.spawn_instanced(well_mesh()?);
        Ok(Self {
            pieces,
            castle_pieces,
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
            unseatable: HashSet::new(),
            doors: Vec::new(),
            hidden_door: None,
            camps: HashMap::new(),
            tent_mesh: None,
            ring_mesh: None,
        })
    }

    pub fn placed_count(&self) -> usize {
        self.hamlets.iter().map(|h| h.houses.len()).sum()
    }

    /// Seated hamlets in the current window, for footbridges across a split river.
    pub fn hamlets(&self) -> &[HamletStand] {
        &self.hamlets
    }

    /// Tent + fire-ring pair on the nearest matching seated hamlet yard.
    pub fn hamlet_camp(&self, hamlet_at: GlobalXZ) -> Option<(GlobalPosition, GlobalPosition)> {
        let id = self.pin_id_at(hamlet_at)?;
        let camp = self.camps.get(&id)?;
        Some((camp.tent_at, camp.ring_at))
    }

    pub fn hamlet_has_camp(&self, hamlet_at: GlobalXZ) -> bool {
        self.hamlet_camp(hamlet_at).is_some()
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
        self.despawn_all_camps(world);
        world.set_instances(self.well, &[])?;
        self.standing.clear();
        self.seated.clear();
        self.plans.clear();
        self.unseatable.clear();
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
        self.drop_far_camps(world, &nearby_ids);

        if plots_changed {
            self.rebuild_live();
            self.sync_building_colliders(world);
            self.sync_tile_set(world, focus)?;
        }

        if self.pending.is_none() {
            let new_pins: Vec<SettlementPin> = nearby
                .into_iter()
                .filter(|pin| {
                    !self.seated.contains_key(&pin.id) && !self.unseatable.contains(&pin.id)
                })
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
        self.stand_camps(world, surface)?;

        let loading = self.staging(focus) || self.pending.is_some();
        let budget = if loading {
            MAX_TILES_LOADING
        } else {
            MAX_TILES_PER_FRAME
        };
        self.drain_tiles(world, focus, budget)?;
        Ok(plots_changed)
    }


    fn pin_id_at(&self, hamlet_at: GlobalXZ) -> Option<i32> {
        self.seated.iter().find_map(|(id, city)| {
            let d = city.hamlet.at;
            if (d.x - hamlet_at.x).abs() < 0.75 && (d.z - hamlet_at.z).abs() < 0.75 {
                Some(*id)
            } else {
                None
            }
        })
    }

    fn despawn_all_camps(&mut self, world: &mut World) {
        for camp in self.camps.drain().map(|(_, c)| c) {
            world.despawn(camp.tent);
            world.despawn(camp.ring);
        }
    }

    fn drop_far_camps(&mut self, world: &mut World, nearby_ids: &HashSet<i32>) {
        let stale: Vec<i32> = self
            .camps
            .keys()
            .copied()
            .filter(|id| !nearby_ids.contains(id) || !self.seated.contains_key(id))
            .collect();
        for id in stale {
            if let Some(camp) = self.camps.remove(&id) {
                world.despawn(camp.tent);
                world.despawn(camp.ring);
            }
        }
    }

    fn ensure_camp_meshes(&mut self) -> EngineResult<(Mesh, Mesh)> {
        if self.tent_mesh.is_none() {
            self.tent_mesh = Some(load_prop_mesh("props/tent_canvas_small.glb")?);
        }
        if self.ring_mesh.is_none() {
            self.ring_mesh = Some(load_prop_mesh("props/campfire_ring.glb")?);
        }
        Ok((
            self.tent_mesh.clone().expect("tent mesh"),
            self.ring_mesh.clone().expect("ring mesh"),
        ))
    }

    fn stand_camps(
        &mut self,
        world: &mut World,
        surface: &ContinentalSurface,
    ) -> EngineResult<()> {
        let seated_ids: Vec<i32> = self.seated.keys().copied().collect();
        for id in seated_ids {
            if self.camps.contains_key(&id) {
                continue;
            }
            if !pin_is_tier0(surface, id) {
                continue;
            }
            let Some(city) = self.seated.get(&id) else {
                continue;
            };
            let houses: Vec<HousePlot> = city
                .plots
                .iter()
                .filter_map(|plot| match plot {
                    BuildingPlot::House(h) => Some(*h),
                    _ => None,
                })
                .collect();
            let hamlet = city.hamlet.clone();
            let seed = plan_seed(self.seed, id);
            let Some(pose) = pick_yard_pair(&hamlet, &houses, seed) else {
                continue;
            };
            let (tent_mesh, ring_mesh) = self.ensure_camp_meshes()?;
            let tent_y = surface.column(pose.tent_xz).ground();
            let ring_y = surface.column(pose.ring_xz).ground();
            let tent_at = GlobalPosition::at(pose.tent_xz.x, f64::from(tent_y), pose.tent_xz.z);
            let ring_at = GlobalPosition::at(pose.ring_xz.x, f64::from(ring_y), pose.ring_xz.z);
            let tent = world.spawn_anchored(
                tent_mesh,
                GlobalPlace::at(tent_at).with_yaw_deg(pose.yaw_deg),
            )?;
            let ring = world.spawn_anchored(
                ring_mesh,
                GlobalPlace::at(ring_at).with_yaw_deg(pose.yaw_deg),
            )?;
            self.camps.insert(
                id,
                CampLive {
                    tent,
                    ring,
                    tent_at,
                    ring_at,
                },
            );
        }
        Ok(())
    }

    fn install_cities(&mut self, bake: PackResult, nearby_ids: &HashSet<i32>) -> bool {
        self.plans.extend(bake.plans);
        for id in bake.failed {
            if nearby_ids.contains(&id) {
                self.unseatable.insert(id);
            }
        }
        let mut changed = false;
        for (id, city) in bake.cities {
            if nearby_ids.contains(&id) {
                self.seated.insert(id, city);
                self.unseatable.remove(&id);
                changed = true;
            }
        }
        changed
    }

    fn drop_far_pins(&mut self, nearby_ids: &HashSet<i32>) -> bool {
        self.unseatable.retain(|id| nearby_ids.contains(id));
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
                .push((item.at, item.yaw_deg, item.tint));
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
        let mut failed = Vec::new();
        for pin in self.pins {
            if !plans.contains_key(&pin.id) {
                match layout_for(self.seed, pin, &self.surface, &self.ponds) {
                    Ok(layout) => {
                        plans.insert(pin.id, layout);
                    }
                    Err(err) => {
                        eprintln!("hamlet at node {} failed: {err}", pin.id);
                        failed.push(pin.id);
                        continue;
                    }
                }
            }
            let (plan, cut) = &plans[&pin.id];
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
                        cut: cut.clone(),
                    },
                    plots,
                    standing,
                    doors,
                },
            ));
        }
        PackResult {
            plans,
            cities,
            failed,
        }
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
) -> Result<(Plan2D, Vec<Vec2>), HamletError> {
    let mut config = layout_config(pin);
    config.seed = plan_seed(world_seed, pin.id);
    let plot = GroundPlot {
        surface,
        ponds,
        ground: None,
        origin: pin.at,
        roads: nearby_road_segs(surface, pin.at, config.max_settle_radius + 24.0),
    };
    let mut plan = plan_on(&config, Some(&plot))?;
    let cut = apply_road_cut(&mut plan, surface, ponds, pin, &config);
    Ok((plan, cut))
}

/// Cut a dirt corridor from the nearest atlas road on the rim to the plaza.
/// Deletes blocking `ShapeKind::House` OBBs. Retries once with a sideways offset
/// if fewer than 4 dwellings remain.
fn apply_road_cut(
    plan: &mut Plan2D,
    surface: &ContinentalSurface,
    ponds: &PondField,
    pin: SettlementPin,
    config: &crate::hamlet::HamletLabConfig,
) -> Vec<Vec2> {
    let polyline = cut_polyline(plan, surface, ponds, pin, config);
    if polyline.len() < 2 {
        return Vec::new();
    }
    let original = plan.clone();
    let first = filter_houses_on_cut(plan, pin, &polyline);
    if first >= 4 {
        return polyline;
    }
    let dir = polyline[polyline.len() - 1] - polyline[0];
    let perp = if dir.length_squared() > 1e-8 {
        Vec2::new(-dir.y, dir.x).normalize() * config.alley
    } else {
        Vec2::new(config.alley, 0.0)
    };
    let retry_line: Vec<Vec2> = polyline.iter().map(|p| *p + perp).collect();
    let mut retry = original.clone();
    let second = filter_houses_on_cut(&mut retry, pin, &retry_line);
    let keep_retry = second >= 4 || (first < 4 && second > first);
    if keep_retry {
        *plan = retry;
        retry_line
    } else {
        polyline
    }
}

fn cut_polyline(
    plan: &Plan2D,
    surface: &ContinentalSurface,
    ponds: &PondField,
    pin: SettlementPin,
    config: &crate::hamlet::HamletLabConfig,
) -> Vec<Vec2> {
    let origin = Vec2::new(pin.at.x as f32, pin.at.z as f32);
    let plaza = origin + plan.plaza;
    let dry = |p: Vec2| {
        let at = GlobalXZ::at(f64::from(p.x), f64::from(p.y));
        let mut column = surface.column(at);
        ponds.carve(at, &mut column);
        !column.is_wet()
    };
    let mut segs = nearby_road_segs(surface, pin.at, config.max_settle_radius + 24.0);
    if segs.is_empty() {
        segs = nearby_road_segs(surface, pin.at, 1200.0);
    }
    if segs.is_empty() {
        for road in surface.roads() {
            for w in road.points.windows(2) {
                segs.push((w[0], w[1]));
            }
        }
    }
    if segs.is_empty() {
        return Vec::new();
    }
    let mut best_q = None;
    let mut best_ab = None;
    let mut best_d = f32::MAX;
    let mut found_dry = false;
    for &(a, b) in &segs {
        let q = closest_on_seg(plaza, a, b);
        let is_dry = dry(q);
        if found_dry && !is_dry {
            continue;
        }
        let d = plaza.distance(q);
        if is_dry && !found_dry {
            found_dry = true;
            best_d = d;
            best_q = Some(q);
            best_ab = Some((a, b));
            continue;
        }
        if d < best_d {
            best_d = d;
            best_q = Some(q);
            best_ab = Some((a, b));
        }
    }
    let Some(q) = best_q else {
        return Vec::new();
    };
    let mut to_q = q - plaza;
    if to_q.length_squared() < 1e-6 {
        if let Some((a, b)) = best_ab {
            to_q = b - a;
        }
    }
    if to_q.length_squared() < 1e-6 {
        return Vec::new();
    }
    let rim_r = if plan.built_envelope > 1.0 {
        plan.built_envelope
    } else {
        config.max_settle_radius
    };
    let rim = plaza + to_q.normalize() * rim_r;
    vec![rim, plaza]
}

fn filter_houses_on_cut(plan: &mut Plan2D, pin: SettlementPin, cut: &[Vec2]) -> u32 {
    let origin = Vec2::new(pin.at.x as f32, pin.at.z as f32);
    let radius = ROAD_CLEAR_M * 0.5;
    plan.shapes.retain(|shape| {
        if shape.kind != ShapeKind::House {
            return true;
        }
        if shape.catalog_id == "Well" {
            return true;
        }
        let center = origin + shape.center;
        !obb_hits_stadium(center, shape.half_size, shape.yaw, cut, radius)
    });
    plan.house_count = plan
        .shapes
        .iter()
        .filter(|s| s.kind == ShapeKind::House && s.dwelling.is_some())
        .count() as u32;
    plan.house_count
}

fn closest_on_seg(p: Vec2, a: Vec2, b: Vec2) -> Vec2 {
    let d = b - a;
    let len2 = d.length_squared();
    if len2 < 1e-8 {
        return a;
    }
    let t = ((p - a).dot(d) / len2).clamp(0.0, 1.0);
    a + d * t
}

fn obb_hits_stadium(center: Vec2, half: Vec2, yaw: f32, cut: &[Vec2], radius: f32) -> bool {
    cut.windows(2)
        .any(|w| dist_obb_seg(center, half, yaw, w[0], w[1]) <= radius)
}

fn dist_obb_seg(center: Vec2, half: Vec2, yaw: f32, a: Vec2, b: Vec2) -> f32 {
    let (s, c) = yaw.sin_cos();
    let to_local = |p: Vec2| {
        let d = p - center;
        Vec2::new(d.x * c - d.y * s, d.x * s + d.y * c)
    };
    dist_aabb_seg(half, to_local(a), to_local(b))
}

fn dist_aabb_seg(half: Vec2, a: Vec2, b: Vec2) -> f32 {
    let d = b - a;
    let len = d.length();
    if len < 1e-6 {
        return dist_aabb_point(half, a);
    }
    let n = ((len / 0.25).ceil() as usize).max(1);
    let mut best = f32::MAX;
    for i in 0..=n {
        let t = i as f32 / n as f32;
        best = best.min(dist_aabb_point(half, a + d * t));
    }
    best
}

fn dist_aabb_point(half: Vec2, p: Vec2) -> f32 {
    let dx = (p.x.abs() - half.x).max(0.0);
    let dz = (p.y.abs() - half.y).max(0.0);
    (dx * dx + dz * dz).sqrt()
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
    let catalog = kit::catalog();
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
                &catalog,
                piece_ids,
                world_seed,
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
    catalog: &modular::prelude::Catalog,
    piece_ids: &[PieceId],
    world_seed: i32,
    out: &mut Vec<Standing>,
    houses: &mut Vec<GlobalXZ>,
    plots: &mut Vec<BuildingPlot>,
    doors: &mut Vec<HouseDoor>,
) {
    if let Some(brief) = shape.dwelling {
        seat_dwelling(
            shape,
            brief,
            pin,
            plot,
            catalog,
            piece_ids,
            world_seed,
            out,
            houses,
            plots,
            doors,
        );
        return;
    }

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
            tint: Color::WHITE,
            door_id: None,
        });
        return;
    }
    if spec.is_civic() {
        return;
    }
    panic!(
        "house shape '{}' has no dwelling brief and is not a civic",
        shape.catalog_id
    );
}

fn seat_dwelling(
    shape: &Shape,
    brief: DwellingBrief,
    pin: SettlementPin,
    plot: &GroundPlot<'_>,
    catalog: &modular::prelude::Catalog,
    piece_ids: &[PieceId],
    world_seed: i32,
    out: &mut Vec<Standing>,
    houses: &mut Vec<GlobalXZ>,
    plots: &mut Vec<BuildingPlot>,
    doors: &mut Vec<HouseDoor>,
) {
    let sample = crate::hamlet::sample_footprint(
        plot,
        shape.center,
        shape.half_size.x,
        shape.half_size.y,
        shape.yaw,
    );
    let Some(seat) = crate::hamlet::seat_building(&sample, FOUNDATION_M) else {
        return;
    };
    let yaw_deg = shape.yaw.to_degrees();
    let x = pin.at.x + f64::from(shape.center.x);
    let z = pin.at.z + f64::from(shape.center.y);
    houses.push(GlobalXZ::at(x, z));

    let seed = house_seed(world_seed, pin.id, shape.center.x, shape.center.y);
    let tint = house_tint(seed);
    let places = generate_dwelling(catalog, brief, seed)
        .unwrap_or_else(|err| panic!("dwelling {} failed: {err}", brief.label()));
    let floor_y = seat.floor_z - DOOR_SINK_M;
    plots.push(BuildingPlot::House(HousePlot {
        at: GlobalXZ::at(x, z),
        half_x: shape.half_size.x,
        half_z: shape.half_size.y,
        yaw: shape.yaw,
        floor_y,
    }));
    let id = door_key(pin.id, x, z);
    for item in &places {
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
                brief,
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
            tint,
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
            tint: Color::WHITE,
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


const TENT_HALF_X: f32 = 1.1;
const TENT_HALF_Z: f32 = 0.8;
const RING_RADIUS_M: f32 = 0.74;
const CAMP_GAP_M: f32 = 1.0;
const CAMP_SEP_M: f32 = TENT_HALF_Z + CAMP_GAP_M + RING_RADIUS_M;
const WELL_KEEP_M: f32 = 1.45;

fn pin_is_tier0(surface: &ContinentalSurface, pin_id: i32) -> bool {
    surface
        .settlements()
        .iter()
        .any(|pin| pin.id == pin_id && pin.tier <= 1)
}

fn load_prop_mesh(rel: &str) -> EngineResult<Mesh> {
    let mut tried = Vec::new();
    if let Some(dir) = std::env::var_os("ORRUN_ASSETS") {
        tried.push(PathBuf::from(dir));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            tried.push(dir.join("assets"));
        }
    }
    tried.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets"));
    for root in &tried {
        let path = root.join(rel);
        if path.is_file() {
            return Model::load(&path).map_err(|source| {
                EngineError::Model(format!("camp mesh {} failed: {source}", path.display()))
            });
        }
    }
    Err(EngineError::Model(format!("camp mesh missing: {rel}")))
}

fn plaza_xz(hamlet: &HamletStand) -> Vec2 {
    hamlet
        .cut
        .last()
        .copied()
        .unwrap_or(Vec2::new(hamlet.at.x as f32, hamlet.at.z as f32))
}

fn camp_angles(hamlet: &HamletStand, seed: u64) -> Vec<f32> {
    let plaza = plaza_xz(hamlet);
    let along = if hamlet.cut.len() >= 2 {
        let d = plaza - hamlet.cut[0];
        if d.length_squared() > 1e-6 {
            d.normalize()
        } else {
            Vec2::X
        }
    } else {
        Vec2::X
    };
    let perp = Vec2::new(-along.y, along.x);
    let prefer = perp.y.atan2(perp.x);
    let mut angles = Vec::new();
    if seed & 1 == 0 {
        angles.push(prefer);
        angles.push(prefer + std::f32::consts::PI);
    } else {
        angles.push(prefer + std::f32::consts::PI);
        angles.push(prefer);
    }
    let jitter = ((seed >> 3) & 15) as f32 * (std::f32::consts::TAU / 16.0);
    for i in 0..16 {
        let a = jitter + i as f32 * (std::f32::consts::TAU / 16.0);
        if !angles.iter().any(|b| angle_near(*b, a)) {
            angles.push(a);
        }
    }
    angles
}

fn angle_near(a: f32, b: f32) -> bool {
    let d = (a - b + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
    d.abs() < 0.08
}

fn pick_yard_pair(hamlet: &HamletStand, houses: &[HousePlot], seed: u64) -> Option<YardPose> {
    let plaza = plaza_xz(hamlet);
    let distances = [4.6_f32, 5.4, 6.2, 3.9, 7.0, 8.0, 3.4, 8.8];
    let yaws = [0.0_f32, 90.0, 180.0, 270.0];
    for dist in distances {
        for ang in camp_angles(hamlet, seed) {
            let mid = plaza + Vec2::new(ang.cos(), ang.sin()) * dist;
            for extra in yaws {
                let yaw_deg = ang.to_degrees() + extra;
                if let Some(pose) = try_yard_pair(mid, yaw_deg, hamlet, houses) {
                    return Some(pose);
                }
            }
        }
    }
    None
}

fn try_yard_pair(
    mid: Vec2,
    yaw_deg: f32,
    hamlet: &HamletStand,
    houses: &[HousePlot],
) -> Option<YardPose> {
    let (fx, fz) = kit::yaw_xz(0.0, 1.0, yaw_deg);
    let half = CAMP_SEP_M * 0.5;
    let tent = mid - Vec2::new(fx, fz) * half;
    let ring = mid + Vec2::new(fx, fz) * half;
    let tent_xz = GlobalXZ::at(f64::from(tent.x), f64::from(tent.y));
    let ring_xz = GlobalXZ::at(f64::from(ring.x), f64::from(ring.y));
    if !pair_legal(tent, ring, yaw_deg, hamlet, houses) {
        return None;
    }
    Some(YardPose {
        tent_xz,
        ring_xz,
        yaw_deg,
    })
}

fn pair_legal(
    tent: Vec2,
    ring: Vec2,
    yaw_deg: f32,
    hamlet: &HamletStand,
    houses: &[HousePlot],
) -> bool {
    let yaw_rad = yaw_deg.to_radians();
    let mut samples = obb_samples(tent, yaw_deg, TENT_HALF_X, TENT_HALF_Z);
    samples.extend(disk_samples(ring, RING_RADIUS_M));
    let plaza = plaza_xz(hamlet);
    for p in &samples {
        if !hamlet.covers(*p, 0.0) {
            return false;
        }
        if houses.iter().any(|h| h.blocks_prop(*p)) {
            return false;
        }
        let v = Vec2::new(p.x as f32, p.z as f32);
        if v.distance(plaza) < WELL_KEEP_M {
            return false;
        }
    }
    if obb_hits_stadium(
        tent,
        Vec2::new(TENT_HALF_X, TENT_HALF_Z),
        yaw_rad,
        &hamlet.cut,
        ROAD_CLEAR_M * 0.5,
    ) {
        return false;
    }
    if obb_hits_stadium(
        ring,
        Vec2::new(RING_RADIUS_M, RING_RADIUS_M),
        0.0,
        &hamlet.cut,
        ROAD_CLEAR_M * 0.5,
    ) {
        return false;
    }
    true
}

fn obb_samples(center: Vec2, yaw_deg: f32, half_x: f32, half_z: f32) -> Vec<GlobalXZ> {
    let mut out = Vec::new();
    for ix in [-1.0_f32, 0.0, 1.0] {
        for iz in [-1.0_f32, 0.0, 1.0] {
            let (dx, dz) = kit::yaw_xz(ix * half_x, iz * half_z, yaw_deg);
            out.push(GlobalXZ::at(
                f64::from(center.x + dx),
                f64::from(center.y + dz),
            ));
        }
    }
    out
}

fn disk_samples(center: Vec2, radius: f32) -> Vec<GlobalXZ> {
    let mut out = vec![GlobalXZ::at(f64::from(center.x), f64::from(center.y))];
    for i in 0..8 {
        let a = i as f32 * (std::f32::consts::TAU / 8.0);
        out.push(GlobalXZ::at(
            f64::from(center.x + a.cos() * radius),
            f64::from(center.y + a.sin() * radius),
        ));
    }
    out
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

/// Plaster/roof multiply so neighbouring dwellings read as different paint.
/// Values are linear RGB factors — keep them strong enough to beat flat lighting
/// on shared cream + timber albedos.
fn house_tint(seed: u64) -> Color {
    // Linear RGB multiply factors. Keep pairs ≥~0.3 L1 apart so cream plaster
    // still reads as different paint under flat daylight.
    const PALETTES: [Color; 5] = [
        Color::WHITE,
        Color {
            r: 1.0,
            g: 0.78,
            b: 0.52,
            a: 1.0,
        }, // warm sandstone
        Color {
            r: 0.68,
            g: 0.76,
            b: 0.95,
            a: 1.0,
        }, // cool slate
        Color {
            r: 0.72,
            g: 0.92,
            b: 0.68,
            a: 1.0,
        }, // sage
        Color {
            r: 1.0,
            g: 0.55,
            b: 0.70,
            a: 1.0,
        }, // clay rose
    ];
    PALETTES[(seed % PALETTES.len() as u64) as usize]
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

fn places_of(items: &[(GlobalPosition, f32, Color)], origin: RenderOrigin) -> Vec<Place> {
    let mut places = Vec::with_capacity(items.len());
    for &(at, yaw_deg, tint) in items {
        let Ok(render) = at.to_render(origin) else {
            continue;
        };
        let p = render.vec3();
        places.push(Place::new(p.x, p.y, p.z).with_yaw_deg(yaw_deg).with_tint(tint));
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
            cut: Vec::new(),
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
    fn yard_pair_sits_in_plaza_off_house_and_road_cut() {
        let hamlet = HamletStand {
            at: GlobalXZ::at(0.0, 0.0),
            radius: 22.0,
            houses: vec![GlobalXZ::at(12.0, 0.0)],
            cut: vec![Vec2::new(-20.0, 0.0), Vec2::new(0.0, 0.0)],
        };
        let house = HousePlot {
            at: GlobalXZ::at(12.0, 0.0),
            half_x: 4.0,
            half_z: 3.5,
            yaw: 0.0,
            floor_y: 0.0,
        };
        let pose = pick_yard_pair(&hamlet, &[house], plan_seed(1, 1)).expect("legal yard pose");
        assert!(hamlet.covers(pose.tent_xz, 0.0));
        assert!(hamlet.covers(pose.ring_xz, 0.0));
        assert!(!house.contains_xz(pose.tent_xz));
        assert!(!house.contains_xz(pose.ring_xz));
        assert!(!house.blocks_prop(pose.tent_xz));
        assert!(!house.blocks_prop(pose.ring_xz));
        let tent = Vec2::new(pose.tent_xz.x as f32, pose.tent_xz.z as f32);
        let ring = Vec2::new(pose.ring_xz.x as f32, pose.ring_xz.z as f32);
        assert!(!obb_hits_stadium(
            tent,
            Vec2::new(TENT_HALF_X, TENT_HALF_Z),
            pose.yaw_deg.to_radians(),
            &hamlet.cut,
            ROAD_CLEAR_M * 0.5,
        ));
        assert!(!obb_hits_stadium(
            ring,
            Vec2::new(RING_RADIUS_M, RING_RADIUS_M),
            0.0,
            &hamlet.cut,
            ROAD_CLEAR_M * 0.5,
        ));
        let gap = tent.distance(ring);
        assert!(gap > 2.0 && gap < 3.2, "pair separation {gap}");
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
    fn house_tint_palettes_are_visibly_apart() {
        let mut colors = Vec::new();
        for seed in 0..5u64 {
            colors.push(house_tint(seed));
        }
        // Same seed family cycles the five paints.
        assert_eq!(house_tint(0).r, house_tint(5).r);
        let mut min_dist = f32::MAX;
        for i in 0..colors.len() {
            for j in (i + 1)..colors.len() {
                let a = colors[i];
                let b = colors[j];
                let d = (a.r - b.r).abs() + (a.g - b.g).abs() + (a.b - b.b).abs();
                min_dist = min_dist.min(d);
            }
        }
        assert!(
            min_dist > 0.28,
            "house paints too close to notice (min_dist={min_dist})"
        );
    }

    #[test]
    fn house_and_castle_tiles_do_not_share_a_batch() {
        let at = GlobalPosition::at(130.0, 0.0, 10.0);
        let house = tile_key(&Standing {
            kind: Kind::HousePiece(0),
            at,
            yaw_deg: 0.0,
            tint: Color::WHITE,
            door_id: None,
        })
        .expect("house piece");
        let castle = tile_key(&Standing {
            kind: Kind::CastlePiece(0),
            at,
            yaw_deg: 0.0,
            tint: Color::WHITE,
            door_id: None,
        })
        .expect("castle piece");
        assert_ne!(house, castle);
        assert!(!house.castle);
        assert!(castle.castle);
    }

    #[test]
    fn a_house_obb_on_the_cut_is_a_hit() {
        let center = Vec2::new(0.0, 0.0);
        let half = Vec2::new(4.0, 3.0);
        let cut = [Vec2::new(-10.0, 0.0), Vec2::new(10.0, 0.0)];
        assert!(obb_hits_stadium(center, half, 0.0, &cut, ROAD_CLEAR_M * 0.5));
        let far = [Vec2::new(-10.0, 20.0), Vec2::new(10.0, 20.0)];
        assert!(!obb_hits_stadium(center, half, 0.0, &far, ROAD_CLEAR_M * 0.5));
    }

    #[test]
    fn filter_houses_keeps_castle_and_drops_a_blocker() {
        let mut plan = Plan2D {
            shapes: vec![
                Shape {
                    kind: ShapeKind::Castle,
                    center: Vec2::new(30.0, 0.0),
                    half_size: Vec2::new(8.0, 8.0),
                    yaw: 0.0,
                    radius: 8.0,
                    catalog_id: "castle_keep_8x6".into(),
                    dwelling: None,
                    polygon: Vec::new(),
                },
                Shape {
                    kind: ShapeKind::House,
                    center: Vec2::ZERO,
                    half_size: Vec2::new(4.0, 3.0),
                    yaw: 0.0,
                    radius: 4.0,
                    catalog_id: String::new(),
                    dwelling: Some(crate::hamlet::DwellingBrief::new(
                        3,
                        2,
                        1,
                        crate::hamlet::HouseTheme::Any,
                    )),
                    polygon: Vec::new(),
                },
                Shape {
                    kind: ShapeKind::House,
                    center: Vec2::new(0.0, 18.0),
                    half_size: Vec2::new(4.0, 3.0),
                    yaw: 0.0,
                    radius: 4.0,
                    catalog_id: String::new(),
                    dwelling: Some(crate::hamlet::DwellingBrief::new(
                        3,
                        2,
                        1,
                        crate::hamlet::HouseTheme::Any,
                    )),
                    polygon: Vec::new(),
                },
            ],
            plaza: Vec2::ZERO,
            house_count: 2,
            ..Plan2D::default()
        };
        let pin = SettlementPin {
            id: 1,
            at: GlobalXZ::at(0.0, 0.0),
            tier: 0,
            population: 8,
        };
        let cut = [Vec2::new(-8.0, 0.0), Vec2::new(8.0, 0.0)];
        filter_houses_on_cut(&mut plan, pin, &cut);
        assert_eq!(
            plan.shapes.iter().filter(|s| s.kind == ShapeKind::Castle).count(),
            1
        );
        assert!(
            !plan
                .shapes
                .iter()
                .any(|s| s.kind == ShapeKind::House && s.center == Vec2::ZERO),
            "house on the cut should be deleted"
        );
        assert!(
            plan.shapes
                .iter()
                .any(|s| s.kind == ShapeKind::House && s.center.y > 10.0),
            "off-cut house should stay"
        );
    }
}
