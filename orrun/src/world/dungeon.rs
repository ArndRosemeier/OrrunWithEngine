//! Atlas dungeon mouths: heightfield pits, background generate, floor hatches.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::Instant;

use dungeon_kit::design::{generate, DungeonSpec, Flavour};
use dungeon_kit::physics::{colliders_for, GroundPlan, FLOOR_TOP};
use dungeon_kit::validate::validate_layout;
use dungeon_kit::{
    has_authored_mesh, landing_floor_y, landing_look_yaw, load_catalog, DUNGEON_CELL_XZ_METRES,
    DUNGEON_STOREY_METRES,
};
use engine::collision::{ActorBody, ColliderLayer, ColliderShape, StaticCollider};
use engine::color::Color;
use engine::error::{EngineError, EngineResult};
use engine::mesh::Mesh;
use engine::model::Model;
use engine::place::GlobalPlace;
use engine::portal::{PortalId, PortalSettings};
use engine::space::{GlobalPosition, GlobalXZ};
use engine::world::{EntityId, World};
use engine::SpaceId;
use glam::Vec3;
use modular::prelude::{Cell, CellVolume, PieceId, PlacedMesh};
use thiserror::Error;

use super::footprint::{BuildingPlot, DungeonPlot};
use super::surface::{ContinentalSurface, DungeonPin};
use super::world_stream::WorldStream;

/// Keep a generated mouth (and its live interior) until the player walks this far.
const CACHE_M: f64 = 1_000.0;
const PIT_DEPTH_M: f32 = 2.8;
const MOUTH_HALF_M: f32 = DUNGEON_CELL_XZ_METRES * 0.5;
const RAMP_LEN_M: f32 = 8.0;
const RAMP_HALF_M: f32 = 1.6;
const HATCH_LIFT_M: f32 = 0.18;
const RETURN_CEILING_INSET_M: f32 = 0.2;
const COLLAR_T: f32 = 0.48;
const COLLAR_H: f32 = 0.62;
const COLLAR_EMBED_M: f32 = 0.08;
const COLLAR_SEGMENTS: usize = 12;
const COVER_LAYER: ColliderLayer = 5;
const DUNGEON_LAYER: ColliderLayer = 6;
/// Soles / hint: standing on the hatch, not the approach ramp.
const HATCH_REACH_M: f64 = 6.0;
/// Open the interior once, from the rim or the ramp. Keep it out to [`CACHE_M`].
pub const LIVE_OPEN_M: f64 = 16.0;

#[derive(Debug, Error)]
pub enum DungeonError {
    #[error(transparent)]
    Engine(#[from] EngineError),

    #[error(
        "dungeon piece {piece} not found (tried {tried}). From C:\\Projekte\\AssetGenerator run: python tools/ag.py generate {stem} then python tools/sync_props.py"
    )]
    MissingPiece {
        piece: String,
        stem: String,
        tried: String,
    },

    #[error("dungeon mesh {path} failed to load: {source}")]
    BadPiece {
        path: PathBuf,
        #[source]
        source: EngineError,
    },

    #[error("{why}")]
    MissingInterior { why: String },
}

#[derive(Clone)]
struct OwnedLayout {
    placed: Vec<PlacedMesh>,
    used_seed: u64,
    meshes: HashMap<PieceId, Mesh>,
    mesh_err: Option<String>,
    /// Local-space chamber centres (5 m XZ clusters of placed meshes). T2+ only.
    skulls: Vec<Vec3>,
    /// Farthest-from-mouth (tie: fattest) cluster. Mage pack excludes this.
    heart: Option<Vec3>,
}

enum BuildMsg {
    Ready { id: i32, layout: OwnedLayout },
    Failed { id: i32, why: String },
}

struct SeatedMouth {
    pin: DungeonPin,
    plot: DungeonPlot,
    collar: Vec<EntityId>,
    layout: Option<OwnedLayout>,
}

struct LiveDungeon {
    pin_id: i32,
    space: SpaceId,
    entities: Vec<EntityId>,
    hatch_out: EntityId,
    portal: PortalId,
    landing_y: f32,
    landing_yaw: f32,
    mouth_at: GlobalPosition,
    skulls: Vec<GlobalXZ>,
    heart: Option<GlobalXZ>,
}

#[derive(Clone, Copy)]
struct DungeonPlacement {
    mouth_world: GlobalPosition,
    mouth_local: Vec3,
}

impl DungeonPlacement {
    fn at(self, local: Vec3) -> GlobalPosition {
        GlobalPosition::at(
            self.mouth_world.x + f64::from(local.x - self.mouth_local.x),
            self.mouth_world.y + f64::from(local.y - self.mouth_local.y),
            self.mouth_world.z + f64::from(local.z - self.mouth_local.z),
        )
    }

    fn y_offset(self) -> f64 {
        self.mouth_world.y - f64::from(self.mouth_local.y)
    }
}

struct Pending {
    rx: Receiver<BuildMsg>,
}

/// Nearby dungeon mouths: pits first, layouts on a worker, one live hatch.
pub struct DungeonLayer {
    seated: HashMap<i32, SeatedMouth>,
    pending: Option<Pending>,
    queued: HashSet<i32>,
    live: Option<LiveDungeon>,
    hint: Option<String>,
    generating: bool,
    started: Option<Instant>,
    hatch_armed: bool,
    /// Last hatch mouth is the shrine. No new mesh.
    last_shrine: Option<GlobalPlace>,
}

impl DungeonLayer {
    pub fn install() -> Self {
        Self {
            seated: HashMap::new(),
            pending: None,
            queued: HashSet::new(),
            live: None,
            hint: None,
            generating: false,
            started: None,
            hatch_armed: false,
            last_shrine: None,
        }
    }

    pub fn hint(&self) -> Option<&str> {
        self.hint.as_deref()
    }

    pub fn generating(&self) -> bool {
        self.pending.is_some() || self.seated.values().any(|s| s.layout.is_none())
    }

    /// HUD line while a nearby layout is still on the worker.
    pub fn build_status(&self) -> Option<String> {
        if !self.generating() {
            return None;
        }
        let mut cutting: Vec<&DungeonPin> = self
            .seated
            .values()
            .filter(|s| s.layout.is_none())
            .map(|s| &s.pin)
            .collect();
        cutting.sort_by_key(|pin| pin.id);
        let elapsed = self.started.map(|t| t.elapsed().as_secs()).unwrap_or(0);
        let clock = if elapsed == 0 {
            String::new()
        } else {
            format!("  {elapsed}s")
        };
        let line = match cutting.as_slice() {
            [] => "cutting dungeon shafts…".into(),
            [pin] => format!("cutting a {} dungeon…", pin.tier_name()),
            many => {
                let names = many
                    .iter()
                    .map(|pin| pin.tier_name())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("cutting {} dungeon shafts ({names})…", many.len())
            }
        };
        Some(format!("{line}{clock}"))
    }

    pub fn plots(&self) -> Vec<BuildingPlot> {
        self.seated
            .values()
            .map(|s| BuildingPlot::Dungeon(s.plot))
            .collect()
    }

    pub fn seated_count(&self) -> usize {
        self.seated.len()
    }

    pub fn ready_count(&self) -> usize {
        self.seated.values().filter(|s| s.layout.is_some()).count()
    }

    pub fn has_live(&self) -> bool {
        self.live.is_some()
    }

    pub fn live_pin_id(&self) -> Option<i32> {
        self.live.as_ref().map(|live| live.pin_id)
    }

    /// World XZ of T2 chamber skulls on the live interior, if any.
    pub fn live_skulls(&self) -> Vec<GlobalXZ> {
        self.live
            .as_ref()
            .map(|live| live.skulls.clone())
            .unwrap_or_default()
    }

    /// Heart cluster (farthest from mouth, fattest on a tie). Mage pack excludes it.
    pub fn live_heart(&self) -> Option<GlobalXZ> {
        self.live.as_ref().and_then(|live| live.heart)
    }

    pub fn pin_seated(&self, id: i32) -> bool {
        self.seated.contains_key(&id)
    }

    pub fn pin_ready(&self, id: i32) -> bool {
        self.seated.get(&id).is_some_and(|s| s.layout.is_some())
    }

    pub fn pin_failed(&self, _id: i32) -> bool {
        false
    }

    pub fn landing_yaw(&self) -> Option<f32> {
        self.live.as_ref().map(|live| live.landing_yaw)
    }

    pub fn near_hatch(&self, feet: GlobalPosition) -> bool {
        self.nearest_within(feet, HATCH_REACH_M).is_some()
    }

    /// Last hatch mouth IS the shrine. No new shrine mesh.
    pub fn shrine_place(&self, feet: GlobalPosition) -> Option<GlobalPlace> {
        let seated = self.nearest_within(feet, HATCH_REACH_M)?;
        Some(GlobalPlace::at(GlobalPosition::at(
            seated.plot.at.x,
            f64::from(seated.plot.floor_y + HATCH_LIFT_M),
            seated.plot.at.z,
        )))
    }

    /// False until the player has stood off the hatch, so a spawn on the pit
    /// does not immediately teleport into the shaft.
    pub fn hatch_armed(&self) -> bool {
        self.hatch_armed
    }

    pub fn indoor_floor_y(&self, world: &World, _feet: GlobalPosition) -> Option<f32> {
        let live = self.live.as_ref()?;
        if world.living_in() != live.space {
            return None;
        }
        Some(live.landing_y)
    }

    pub fn clear(&mut self, world: &mut World) -> EngineResult<()> {
        self.evict_live(world)?;
        for seated in self.seated.values() {
            for &id in &seated.collar {
                world.despawn(id);
            }
        }
        self.seated.clear();
        self.queued.clear();
        self.pending = None;
        self.generating = false;
        self.started = None;
        self.hatch_armed = false;
        self.hint = None;
        world.collision_mut().clear_layer(COVER_LAYER);
        world.collision_mut().clear_layer(DUNGEON_LAYER);
        Ok(())
    }

    /// Seat pits immediately and start generate jobs for mouths in reach.
    pub fn follow(
        &mut self,
        world: &mut World,
        _stream: &WorldStream,
        surface: &Arc<ContinentalSurface>,
        focus: GlobalXZ,
    ) -> Result<bool, DungeonError> {
        let nearby: Vec<DungeonPin> = surface
            .dungeon_pins()
            .iter()
            .copied()
            .filter(|pin| pin.at.distance(focus) <= CACHE_M)
            .collect();
        let nearby_ids: HashSet<i32> = nearby.iter().map(|p| p.id).collect();
        let mut plots_changed = false;

        if let Some(pending) = self.pending.take() {
            loop {
                match pending.rx.try_recv() {
                    Ok(BuildMsg::Ready { id, layout }) => {
                        self.queued.remove(&id);
                        if let Some(seated) = self.seated.get_mut(&id) {
                            seated.layout = Some(layout);
                        }
                    }
                    Ok(BuildMsg::Failed { id, why }) => {
                        panic!("dungeon {id} worker failed: {why}");
                    }
                    Err(TryRecvError::Empty) => {
                        self.pending = Some(pending);
                        break;
                    }
                    Err(TryRecvError::Disconnected) => {
                        if !self.queued.is_empty() {
                            let left: Vec<String> =
                                self.queued.iter().map(|id| id.to_string()).collect();
                            panic!(
                                "dungeon worker exited while still cutting {}",
                                left.join(", ")
                            );
                        }
                        self.generating = false;
                        self.started = None;
                        break;
                    }
                }
            }
        }

        let dropped: Vec<i32> = self
            .seated
            .keys()
            .copied()
            .filter(|id| !nearby_ids.contains(id))
            .collect();
        for id in dropped {
            if self.live.as_ref().is_some_and(|l| l.pin_id == id) {
                self.evict_live(world)?;
            }
            if let Some(seated) = self.seated.remove(&id) {
                for entity in seated.collar {
                    world.despawn(entity);
                }
                plots_changed = true;
            }
            self.queued.remove(&id);
        }

        for pin in &nearby {
            if self.seated.contains_key(&pin.id) {
                continue;
            }
            let seated = seat_mouth(world, surface, *pin)?;
            self.seated.insert(pin.id, seated);
            plots_changed = true;
        }

        if self.pending.is_none() {
            let need: Vec<DungeonPin> = nearby
                .into_iter()
                .filter(|pin| {
                    self.seated
                        .get(&pin.id)
                        .is_some_and(|s| s.layout.is_none() && !self.queued.contains(&pin.id))
                })
                .collect();
            if !need.is_empty() {
                for pin in &need {
                    self.queued.insert(pin.id);
                }
                self.generating = true;
                if self.started.is_none() {
                    self.started = Some(Instant::now());
                }
                let (tx, rx) = mpsc::channel();
                self.pending = Some(Pending { rx });
                std::thread::Builder::new()
                    .name("dungeons".into())
                    .spawn(move || {
                        for pin in need {
                            let msg = match std::panic::catch_unwind(|| build_owned(pin)) {
                                Ok(layout) => BuildMsg::Ready { id: pin.id, layout },
                                Err(payload) => BuildMsg::Failed {
                                    id: pin.id,
                                    why: panic_message(payload),
                                },
                            };
                            if tx.send(msg).is_err() {
                                return;
                            }
                        }
                    })
                    .expect("dungeon thread");
            }
        }

        self.sync_covers(world);
        Ok(plots_changed)
    }

    pub fn frame(
        &mut self,
        world: &mut World,
        feet: GlobalPosition,
        _yaw_degrees: f32,
    ) -> Result<(), DungeonError> {
        self.hint = None;
        if let Some(seated) = self.nearest_within(feet, HATCH_REACH_M) {
            if seated.layout.is_none() {
                self.hint = Some("the shaft is still being cut".into());
            }
        }

        let focus = GlobalXZ::at(feet.x, feet.z);
        let inside = self
            .live
            .as_ref()
            .is_some_and(|live| world.living_in() == live.space);
        if let Some(live) = self.live.as_ref() {
            let keep = inside
                || self
                    .seated
                    .get(&live.pin_id)
                    .is_some_and(|s| s.pin.at.distance(focus) <= CACHE_M);
            if !keep {
                self.evict_live(world)?;
            }
        }

        if self.live.is_none() {
            if let Some(id) = self
                .nearest_within(feet, LIVE_OPEN_M)
                .filter(|s| s.layout.is_some())
                .map(|s| s.pin.id)
            {
                let layout = self.seated[&id]
                    .layout
                    .clone()
                    .expect("ready hatch has a layout");
                let pin = self.seated[&id].pin;
                let plot = self.seated[&id].plot;
                match self.begin_live(world, pin, plot, &layout) {
                    Ok(()) => {}
                    Err(err @ DungeonError::MissingInterior { .. }) => {
                        self.hint = Some(err.to_string());
                    }
                    Err(err) => return Err(err),
                }
            }
        }
        if self.nearest_within(feet, HATCH_REACH_M).is_none() {
            self.hatch_armed = true;
        }
        self.sync_covers(world);
        Ok(())
    }

    pub fn settle_after_travel(
        &self,
        world: &mut World,
        body: &ActorBody,
        entered: SpaceId,
        feet: &mut GlobalPosition,
        yaw_degrees: &mut f32,
    ) {
        let Some(live) = self.live.as_ref() else {
            return;
        };
        if entered == live.space {
            let look = live.landing_yaw.to_radians();
            let back = 1.4_f64;
            let landing =
                GlobalPosition::at(live.mouth_at.x, f64::from(live.landing_y), live.mouth_at.z);
            *feet = world
                .move_actor_3d(
                    body,
                    landing,
                    f64::from(look.sin()) * back,
                    0.0,
                    f64::from(look.cos()) * back,
                )
                .position;
            *yaw_degrees = live.landing_yaw;
        } else if entered == SpaceId::DEFAULT {
            if let Some(seated) = self.seated.get(&live.pin_id) {
                let yaw = seated.plot.approach_yaw;
                let dist = seated.plot.half + 1.2;
                feet.x = seated.plot.at.x + f64::from(yaw.sin() * dist);
                feet.y = f64::from(seated.plot.rim_y);
                feet.z = seated.plot.at.z + f64::from(yaw.cos() * dist);
            }
        }
    }

    fn nearest_within(&self, feet: GlobalPosition, max_m: f64) -> Option<&SeatedMouth> {
        let mut best: Option<(&SeatedMouth, f64)> = None;
        for seated in self.seated.values() {
            let dx = feet.x - seated.plot.at.x;
            let dz = feet.z - seated.plot.at.z;
            let d = (dx * dx + dz * dz).sqrt();
            if d > max_m {
                continue;
            }
            if best.as_ref().is_none_or(|(_, best_d)| d < *best_d) {
                best = Some((seated, d));
            }
        }
        best.map(|(s, _)| s)
    }

    fn begin_live(
        &mut self,
        world: &mut World,
        pin: DungeonPin,
        plot: DungeonPlot,
        layout: &OwnedLayout,
    ) -> Result<(), DungeonError> {
        if let Some(why) = &layout.mesh_err {
            return Err(DungeonError::MissingInterior { why: why.clone() });
        }
        let mouth = layout
            .placed
            .iter()
            .find(|item| item.piece.as_str() == "mouth")
            .unwrap_or_else(|| panic!("dungeon {} has no mouth", pin.id));
        let hatch_y = plot.floor_y + HATCH_LIFT_M;
        let placement = DungeonPlacement {
            mouth_world: GlobalPosition::at(plot.at.x, f64::from(hatch_y), plot.at.z),
            mouth_local: mouth.place.position,
        };
        let landing_yaw = landing_look_yaw(&layout.placed);
        let landing_y =
            (f64::from(landing_floor_y(&layout.placed)) + FLOOR_TOP + placement.y_offset()) as f32;
        let return_y =
            landing_y + DUNGEON_STOREY_METRES - FLOOR_TOP as f32 - RETURN_CEILING_INSET_M;
        let space = world.space(format!("dungeon-{}", pin.id))?;
        let opening =
            Mesh::opening(DUNGEON_CELL_XZ_METRES, DUNGEON_CELL_XZ_METRES).expect("dungeon hatch");
        let hatch_out = world.spawn_anchored(
            opening.clone(),
            GlobalPlace::at(GlobalPosition::at(plot.at.x, f64::from(hatch_y), plot.at.z))
                .with_pitch_deg(-90.0),
        )?;
        world.in_space(space)?;
        let hatch_in = world.spawn_anchored(
            opening,
            GlobalPlace::at(GlobalPosition::at(
                placement.mouth_world.x,
                f64::from(return_y),
                placement.mouth_world.z,
            ))
            .with_yaw_deg(mouth.place.yaw_degrees)
            .with_pitch_deg(90.0),
        )?;
        let mut entities = spawn_layout(world, &layout.placed, &layout.meshes, placement)?;
        entities.push(hatch_in);
        world.in_space(SpaceId::DEFAULT)?;
        let portal = world.create_portal(hatch_out, hatch_in, PortalSettings::TELEPORTING)?;
        self.last_shrine = Some(
            GlobalPlace::at(GlobalPosition::at(plot.at.x, f64::from(hatch_y), plot.at.z))
                .with_yaw_deg(mouth.place.yaw_degrees),
        );
        let ground = GroundPlan::from_places(&layout.placed);
        let colliders: Vec<StaticCollider> = colliders_for(&layout.placed, &ground)
            .into_iter()
            .map(|mut c| {
                let at = placement.at(Vec3::new(c.at.x as f32, 0.0, c.at.z as f32));
                c.at = at.horizontal();
                c.min_y += placement.y_offset();
                c.max_y += placement.y_offset();
                c.in_space(space)
            })
            .collect();
        world
            .collision_mut()
            .replace_layer(DUNGEON_LAYER, colliders)
            .expect("dungeon colliders");
        let skulls = layout
            .skulls
            .iter()
            .copied()
            .map(|local| {
                let at = placement.at(local);
                GlobalXZ::at(at.x, at.z)
            })
            .collect();
        let heart = layout.heart.map(|local| {
            let at = placement.at(local);
            GlobalXZ::at(at.x, at.z)
        });
        self.live = Some(LiveDungeon {
            pin_id: pin.id,
            space,
            entities,
            hatch_out,
            portal,
            landing_y,
            landing_yaw,
            mouth_at: placement.mouth_world,
            skulls,
            heart,
        });
        let _ = layout.used_seed;
        Ok(())
    }

    fn evict_live(&mut self, world: &mut World) -> EngineResult<()> {
        let Some(live) = self.live.take() else {
            return Ok(());
        };
        world.destroy_portal(live.portal)?;
        world.despawn(live.hatch_out);
        for id in live.entities {
            world.despawn(id);
        }
        if world.living_in() == live.space {
            world.live_in(SpaceId::DEFAULT)?;
        }
        world.collision_mut().clear_layer(DUNGEON_LAYER);
        self.hatch_armed = false;
        Ok(())
    }

    fn sync_covers(&self, world: &mut World) {
        let live_id = self.live.as_ref().map(|l| l.pin_id);
        let covers: Vec<StaticCollider> = self
            .seated
            .values()
            .filter(|s| s.layout.is_none() || live_id != Some(s.pin.id) || !self.hatch_armed)
            .map(|s| {
                StaticCollider::new(
                    s.plot.at,
                    0.0,
                    ColliderShape::Box {
                        half_x: MOUTH_HALF_M,
                        half_z: MOUTH_HALF_M,
                    },
                )
                .with_y_span(
                    f64::from(s.plot.floor_y - 0.05),
                    f64::from(s.plot.floor_y + HATCH_LIFT_M + 0.2),
                )
            })
            .collect();
        world
            .collision_mut()
            .replace_layer(COVER_LAYER, covers)
            .expect("hatch cover colliders");
    }
}

fn seat_mouth(
    world: &mut World,
    surface: &ContinentalSurface,
    pin: DungeonPin,
) -> Result<SeatedMouth, DungeonError> {
    let rim_y = surface.column(pin.at).ground();
    let approach_yaw = downhill_yaw(surface, pin.at);
    let plot = DungeonPlot {
        at: pin.at,
        half: MOUTH_HALF_M,
        floor_y: rim_y - PIT_DEPTH_M,
        rim_y,
        approach_yaw,
        ramp_len: RAMP_LEN_M,
        ramp_half: RAMP_HALF_M,
    };
    let collar = spawn_collar(world, &plot)?;
    Ok(SeatedMouth {
        pin,
        plot,
        collar,
        layout: None,
    })
}

fn downhill_yaw(surface: &ContinentalSurface, at: GlobalXZ) -> f32 {
    let here = surface.column(at).ground();
    let step = 6.0_f64;
    let dirs = [0.0_f32, 90.0, 180.0, 270.0];
    let mut best = dirs[0];
    let mut best_h = here;
    for deg in dirs {
        let rad = deg.to_radians();
        let p = GlobalXZ::at(
            at.x + step * f64::from(rad.sin()),
            at.z + step * f64::from(rad.cos()),
        );
        let h = surface.column(p).ground();
        if h < best_h {
            best_h = h;
            best = rad;
        }
    }
    best
}

fn spawn_collar(world: &mut World, plot: &DungeonPlot) -> EngineResult<Vec<EntityId>> {
    let stones = [
        collar_block(Color::rgb(92, 86, 78)),
        collar_block(Color::rgb(104, 97, 87)),
        collar_block(Color::rgb(82, 80, 76)),
    ];
    // Keep the whole collar inside the excavated disk. Its datum is the
    // dungeon mouth at the pit floor, not the untouched terrain at the rim.
    let radius = plot.half - COLLAR_T * 0.55;
    let arc = std::f32::consts::TAU * radius / COLLAR_SEGMENTS as f32 * 1.08;
    let mut ids = Vec::new();
    for i in 0..COLLAR_SEGMENTS {
        let angle = std::f32::consts::TAU * i as f32 / COLLAR_SEGMENTS as f32;
        let from_approach = (angle - plot.approach_yaw + std::f32::consts::PI)
            .rem_euclid(std::f32::consts::TAU)
            - std::f32::consts::PI;
        // Three missing blocks make the ramp read as an intentional entrance.
        if from_approach.abs() <= std::f32::consts::TAU / COLLAR_SEGMENTS as f32 {
            continue;
        }
        let dx = angle.sin() * radius;
        let dz = angle.cos() * radius;
        let segment_xz = GlobalXZ::at(plot.at.x + f64::from(dx), plot.at.z + f64::from(dz));
        let height = COLLAR_H * (0.88 + 0.04 * (i % 3) as f32);
        let at = GlobalPosition::at(
            segment_xz.x,
            f64::from(plot.floor_y + height * 0.5 - COLLAR_EMBED_M),
            segment_xz.z,
        );
        ids.push(
            world.spawn_anchored(
                stones[i % stones.len()].clone(),
                GlobalPlace::at(at)
                    .with_yaw_deg(angle.to_degrees())
                    .with_stretch(Vec3::new(arc, height, COLLAR_T)),
            )?,
        );
    }
    Ok(ids)
}

fn collar_block(color: Color) -> Mesh {
    Mesh::box_at(Vec3::ZERO, Vec3::ONE, color).expect("collar block")
}

fn build_owned(pin: DungeonPin) -> OwnedLayout {
    let mut layout = generate_layout(pin);
    match try_preload(&layout.placed) {
        Ok(meshes) => layout.meshes = meshes,
        Err(err) => layout.mesh_err = Some(err.to_string()),
    }
    layout
}

fn try_preload(placed: &[PlacedMesh]) -> Result<HashMap<PieceId, Mesh>, DungeonError> {
    let mut meshes = HashMap::new();
    for item in placed {
        if !has_authored_mesh(&item.piece) || meshes.contains_key(&item.piece) {
            continue;
        }
        meshes.insert(item.piece.clone(), load_dungeon_mesh(&item.piece)?);
    }
    Ok(meshes)
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<&str>()
        .copied()
        .map(str::to_string)
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".into())
}

fn chamber_clusters(positions: impl IntoIterator<Item = Vec3>) -> Vec<(Vec3, u32)> {
    const CELL: f32 = 5.0;
    let mut buckets: HashMap<(i32, i32), (Vec3, u32)> = HashMap::new();
    for p in positions {
        let key = ((p.x / CELL).floor() as i32, (p.z / CELL).floor() as i32);
        let entry = buckets.entry(key).or_insert((Vec3::ZERO, 0));
        entry.0 += p;
        entry.1 += 1;
    }
    buckets
        .into_values()
        .map(|(sum, n)| (sum / n as f32, n))
        .collect()
}

fn pick_heart(clusters: &[(Vec3, u32)], mouth: Option<Vec3>) -> Option<Vec3> {
    clusters
        .iter()
        .max_by(|(a, an), (b, bn)| {
            let da = mouth.map(|m| (a.x - m.x).hypot(a.z - m.z)).unwrap_or(0.0);
            let db = mouth.map(|m| (b.x - m.x).hypot(b.z - m.z)).unwrap_or(0.0);
            da.partial_cmp(&db)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(an.cmp(bn))
        })
        .map(|(c, _)| *c)
}

fn generate_layout(pin: DungeonPin) -> OwnedLayout {
    let catalog = load_catalog();
    let (shell, density, attempts) = spec_for(pin.tier);
    let spec = DungeonSpec {
        shell,
        seed: pin.seed,
        flavour: Flavour::Any,
        density,
    };
    for offset in 0..attempts {
        let mut next = spec;
        next.seed = pin.seed.wrapping_add(offset);
        let Ok(assembly) = generate(&catalog, next) else {
            continue;
        };
        validate_layout(&assembly)
            .unwrap_or_else(|err| panic!("dungeon {} closed layout is invalid:\n{err}", pin.id));
        let placed = assembly
            .places()
            .unwrap_or_else(|err| panic!("dungeon {} places: {err}", pin.id));
        let clusters = if pin.tier >= 2 {
            chamber_clusters(placed.iter().map(|item| item.place.position))
        } else {
            Vec::new()
        };
        let skulls: Vec<Vec3> = clusters.iter().map(|(c, _)| *c).collect();
        let heart = if pin.tier >= 2 {
            let mouth = placed
                .iter()
                .find(|item| item.piece.as_str() == "mouth")
                .map(|item| item.place.position);
            pick_heart(&clusters, mouth)
        } else {
            None
        };
        return OwnedLayout {
            placed,
            used_seed: next.seed,
            meshes: HashMap::new(),
            mesh_err: None,
            skulls,
            heart,
        };
    }
    panic!(
        "no closed dungeon for pin {} in {attempts} seeds from {} (tier {})",
        pin.id, pin.seed, pin.tier
    );
}

fn spec_for(tier: u8) -> (CellVolume, f32, u64) {
    let (extent, depth, density, attempts) = match tier {
        2 => (15, -5, 0.36, 80),
        1 => (11, -5, 0.38, 64),
        _ => (7, -4, 0.40, 48),
    };
    let shell = CellVolume::new(Cell::new(0, depth, 0), Cell::new(extent, 0, extent))
        .unwrap_or_else(|err| panic!("dungeon shell: {err}"));
    (shell, density, attempts)
}

fn spawn_layout(
    world: &mut World,
    placed: &[PlacedMesh],
    meshes: &HashMap<PieceId, Mesh>,
    placement: DungeonPlacement,
) -> EngineResult<Vec<EntityId>> {
    let mut ids = Vec::new();
    for item in placed {
        if !has_authored_mesh(&item.piece) {
            continue;
        }
        let mesh = meshes.get(&item.piece).cloned().unwrap_or_else(|| {
            panic!(
                "dungeon piece {} was not preloaded on the worker",
                item.piece
            )
        });
        let at = placement.at(item.place.position);
        ids.push(world.spawn_anchored(
            mesh,
            GlobalPlace::at(at).with_yaw_deg(item.place.yaw_degrees),
        )?);
    }
    Ok(ids)
}

fn load_dungeon_mesh(piece: &PieceId) -> Result<Mesh, DungeonError> {
    let file = format!("dungeon_{}.glb", piece.as_str());
    let tried = dungeon_search_paths(&file);
    let path = tried.iter().find(|p| p.is_file()).cloned();
    let Some(path) = path else {
        return Err(DungeonError::MissingPiece {
            piece: piece.to_string(),
            stem: Path::new(&file)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or(file.clone()),
            tried: tried
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", "),
        });
    };
    let base = path.parent().unwrap_or_else(|| Path::new("."));
    Model::load_with(&path, base, &engine::EngineLimits::default())
        .map_err(|source| DungeonError::BadPiece { path, source })
}

fn dungeon_search_paths(file: &str) -> Vec<PathBuf> {
    let mut tried = Vec::new();
    if let Some(dir) = std::env::var_os("ORRUN_ASSETS") {
        tried.push(PathBuf::from(dir).join("kit").join("dungeon").join(file));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            tried.push(dir.join("assets").join("kit").join("dungeon").join(file));
        }
    }
    tried.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join("kit")
            .join("dungeon")
            .join(file),
    );
    tried.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("AssetGenerator")
            .join("assets")
            .join("out")
            .join(file),
    );
    tried
}

impl DungeonLayer {
    /// Last hatch mouth. Death returns here; no extra shrine mesh.
    pub fn shrine(&self) -> Option<GlobalPlace> {
        self.last_shrine
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atlas::ContinentAtlas;
    use crate::world::surface::ContinentalSurface;

    #[test]
    fn five_metre_cells_yield_one_skull_per_cluster() {
        let skulls = chamber_clusters([
            Vec3::new(0.1, 1.0, 0.2),
            Vec3::new(1.0, 1.0, 1.0),
            Vec3::new(20.0, 0.0, 20.0),
        ]);
        assert_eq!(skulls.len(), 2);
    }

    #[test]
    fn tier2_layout_seeds_one_skull_per_chamber_cluster() {
        let pin = DungeonPin {
            id: 7,
            at: GlobalXZ::at(0.0, 0.0),
            tier: 2,
            seed: 11,
        };
        let layout = generate_layout(pin);
        if layout.placed.is_empty() {
            return;
        }
        assert!(
            !layout.skulls.is_empty(),
            "tier-2 layout with {} placed meshes must seed at least one skull",
            layout.placed.len()
        );
    }

    #[test]
    fn compact_generate_is_deterministic() {
        let pin = DungeonPin {
            id: 1,
            at: GlobalXZ::at(0.0, 0.0),
            tier: 0,
            seed: 11,
        };
        let a = generate_layout(pin);
        let b = generate_layout(pin);
        assert_eq!(a.used_seed, b.used_seed);
        assert_eq!(a.placed.len(), b.placed.len());
        let mouth_a = a
            .placed
            .iter()
            .find(|item| item.piece.as_str() == "mouth")
            .expect("mouth");
        let mouth_b = b
            .placed
            .iter()
            .find(|item| item.piece.as_str() == "mouth")
            .expect("mouth");
        assert_eq!(mouth_a.place.position, mouth_b.place.position);
    }

    #[test]
    fn dungeon_local_coordinates_are_anchored_under_the_atlas_mouth() {
        let plot = DungeonPlot {
            at: GlobalXZ::at(123_456.75, 87_654.25),
            half: MOUTH_HALF_M,
            floor_y: 91.0,
            rim_y: 93.8,
            approach_yaw: 0.0,
            ramp_len: RAMP_LEN_M,
            ramp_half: RAMP_HALF_M,
        };
        let local_mouth = Vec3::new(12.5, 0.0, 2.5);
        let placement = DungeonPlacement {
            mouth_world: GlobalPosition::at(
                plot.at.x,
                f64::from(plot.floor_y + HATCH_LIFT_M),
                plot.at.z,
            ),
            mouth_local: local_mouth,
        };

        let mouth = placement.at(local_mouth);
        assert!((mouth.x - plot.at.x).abs() < 1e-6);
        assert!((mouth.z - plot.at.z).abs() < 1e-6);
        assert!((mouth.y - f64::from(plot.floor_y + HATCH_LIFT_M)).abs() < 1e-6);

        let room = placement.at(local_mouth + Vec3::new(5.0, -4.0, 10.0));
        assert!((room.x - (plot.at.x + 5.0)).abs() < 1e-6);
        assert!((room.z - (plot.at.z + 10.0)).abs() < 1e-6);
        assert!((room.y - (mouth.y - 4.0)).abs() < 1e-6);
    }

    #[test]
    fn hatch_travel_stays_at_the_atlas_mouth_in_the_dungeon_space() {
        let pin = DungeonPin {
            id: 42,
            at: GlobalXZ::at(123_456.0, 87_654.0),
            tier: 0,
            seed: 11,
        };
        let plot = DungeonPlot {
            at: pin.at,
            half: MOUTH_HALF_M,
            floor_y: 91.0,
            rim_y: 93.8,
            approach_yaw: 0.0,
            ramp_len: RAMP_LEN_M,
            ramp_half: RAMP_HALF_M,
        };
        let mut layout = generate_layout(pin);
        for item in &layout.placed {
            if has_authored_mesh(&item.piece) && !layout.meshes.contains_key(&item.piece) {
                layout.meshes.insert(
                    item.piece.clone(),
                    Mesh::box_at(Vec3::ZERO, Vec3::ONE, Color::rgb(100, 100, 100))
                        .expect("test piece"),
                );
            }
        }

        let mut world = World::new();
        let mut layer = DungeonLayer::install();
        layer
            .begin_live(&mut world, pin, plot, &layout)
            .expect("live dungeon");
        let space = layer.live.as_ref().expect("live").space;
        let hatch_y = f64::from(plot.floor_y + HATCH_LIFT_M);
        let mut yaw = 0.0;
        let mut point = world
            .to_render(GlobalPosition::at(pin.at.x, hatch_y + 1.0, pin.at.z))
            .expect("above hatch");
        assert!(world.travel(&mut point, &mut yaw).is_none());
        point = world
            .to_render(GlobalPosition::at(pin.at.x, hatch_y - 1.0, pin.at.z))
            .expect("below hatch");
        assert_eq!(world.travel(&mut point, &mut yaw), Some(space));
        let landed = world.to_global(point).expect("dungeon point");
        assert!(
            landed.horizontal().distance(pin.at) < 2.0,
            "hatch moved the player to unrelated global coordinates: {landed:?}"
        );
        let mut feet = landed;
        layer.settle_after_travel(&mut world, &ActorBody::player(), space, &mut feet, &mut yaw);
        let landing_yaw = layer.live.as_ref().expect("live").landing_yaw;
        assert!(
            (yaw - landing_yaw).abs() < 1e-3,
            "entry faces {yaw}°, but the authored corridor faces {landing_yaw}°"
        );
        let look = yaw.to_radians();
        let moved = world.move_actor_3d(
            &ActorBody::player(),
            feet,
            f64::from(look.sin()) * 3.0,
            0.0,
            f64::from(look.cos()) * 3.0,
        );
        assert!(
            moved.position.horizontal().distance(feet.horizontal()) > 2.5,
            "entry collision blocks a visibly open corridor: {moved:?} from {feet:?}"
        );

        let body = ActorBody::player();
        let probe_y = f64::from(body.height);
        let return_y = feet.y + f64::from(DUNGEON_STOREY_METRES)
            - FLOOR_TOP
            - f64::from(RETURN_CEILING_INSET_M);
        assert!(
            return_y > feet.y + f64::from(body.height),
            "return opening must be overhead: return={return_y}, feet={feet:?}"
        );
        yaw = 137.0;
        let exit_yaw = yaw;
        let mut point = world
            .to_render(GlobalPosition::at(pin.at.x, return_y - 0.5, pin.at.z))
            .expect("below return hatch");
        assert!(world.travel(&mut point, &mut yaw).is_none());
        point = world
            .to_render(GlobalPosition::at(pin.at.x, return_y + 0.5, pin.at.z))
            .expect("above return hatch");
        assert_eq!(
            world.travel(&mut point, &mut yaw),
            Some(SpaceId::DEFAULT),
            "climbing through the ceiling hatch must leave the dungeon"
        );
        let landed_probe = world.to_global(point).expect("outside probe");
        let mut outside_feet =
            GlobalPosition::at(landed_probe.x, landed_probe.y - probe_y, landed_probe.z);
        layer.settle_after_travel(
            &mut world,
            &body,
            SpaceId::DEFAULT,
            &mut outside_feet,
            &mut yaw,
        );
        assert!(
            outside_feet.horizontal().distance(pin.at) < 5.0,
            "return opening did not lead back to its atlas mouth: {outside_feet:?}"
        );
        assert!(
            (yaw - exit_yaw).abs() < 1e-3,
            "leaving through a vertical hatch changed yaw from {exit_yaw}° to {yaw}°"
        );
    }

    #[test]
    fn atlas_dungeon_pins_miss_water_and_settlements() {
        let atlas = ContinentAtlas::generate(20260816, 64);
        let surface = ContinentalSurface::new(&atlas).expect("surface");
        let pins = surface.dungeon_pins();
        assert!(
            !pins.is_empty(),
            "seed 20260816 size 64 must plant dungeon mouths"
        );
        let settlements = surface.settlements();
        for pin in pins {
            let column = surface.column(pin.at);
            assert!(
                column.ground() > surface.sea_surface_z() + 1.0,
                "dungeon {} is in the sea",
                pin.id
            );
            let mouth = glam::Vec2::new(pin.at.x as f32, pin.at.z as f32);
            let road_distance = surface
                .roads()
                .iter()
                .flat_map(|road| road.points.windows(2))
                .map(|segment| {
                    let a = segment[0];
                    let b = segment[1];
                    let ab = b - a;
                    if ab.length_squared() <= f32::EPSILON {
                        return mouth.distance(a);
                    }
                    let t = ((mouth - a).dot(ab) / ab.length_squared()).clamp(0.0, 1.0);
                    mouth.distance(a + ab * t)
                })
                .fold(f32::INFINITY, f32::min);
            assert!(
                road_distance > 100.0,
                "dungeon {} is only {road_distance:.1} m from a road",
                pin.id
            );
            for house in settlements {
                let dx = pin.at.x - house.at.x;
                let dz = pin.at.z - house.at.z;
                assert!(
                    dx * dx + dz * dz > 80.0 * 80.0,
                    "dungeon {} sits on settlement {}",
                    pin.id,
                    house.id
                );
            }
        }
        let again = ContinentalSurface::new(&ContinentAtlas::generate(20260816, 64))
            .expect("surface")
            .dungeon_pins()
            .to_vec();
        assert_eq!(pins.to_vec(), again);
    }

    #[test]
    fn follow_does_not_run_generate_on_this_thread() {
        use crate::world::ponds::PondWindow;
        use crate::world::world_stream::WorldStream;
        use std::time::Duration;

        let atlas = ContinentAtlas::generate(20260816, 64);
        let surface = Arc::new(ContinentalSurface::new(&atlas).expect("surface"));
        let pin = *surface
            .dungeon_pins()
            .first()
            .expect("seed 20260816 size 64 must plant a dungeon");
        let ponds = PondWindow::new(Arc::clone(&surface));
        let stream = WorldStream::new(Arc::clone(&surface), ponds.shared());
        let mut world = World::new();
        let mut layer = DungeonLayer::install();
        let t0 = Instant::now();
        layer
            .follow(&mut world, &stream, &surface, pin.at)
            .expect("follow");
        let dt = t0.elapsed();
        assert!(
            layer.generating(),
            "follow must start a worker for the nearby mouth"
        );
        assert!(
            layer.ready_count() == 0,
            "layout must not be ready on the same call that started the worker"
        );
        assert!(
            dt < Duration::from_millis(750),
            "follow blocked for {dt:?}; generate ran on this thread"
        );
    }

    #[test]
    fn a_seated_dungeon_stays_cached_within_a_kilometre() {
        use crate::world::ponds::PondWindow;
        use crate::world::world_stream::WorldStream;

        let atlas = ContinentAtlas::generate(20260816, 64);
        let surface = Arc::new(ContinentalSurface::new(&atlas).expect("surface"));
        let pin = *surface
            .dungeon_pins()
            .first()
            .expect("seed 20260816 size 64 must plant a dungeon");
        let ponds = PondWindow::new(Arc::clone(&surface));
        let stream = WorldStream::new(Arc::clone(&surface), ponds.shared());
        let mut world = World::new();
        let mut layer = DungeonLayer::install();
        layer
            .follow(&mut world, &stream, &surface, pin.at)
            .expect("seat");
        assert_eq!(layer.seated_count(), 1);

        let mid = GlobalXZ::at(pin.at.x + 500.0, pin.at.z);
        layer
            .follow(&mut world, &stream, &surface, mid)
            .expect("keep");
        assert_eq!(
            layer.seated_count(),
            1,
            "a mouth 500 m away must stay cached"
        );

        let far = GlobalXZ::at(pin.at.x + 1_500.0, pin.at.z);
        layer
            .follow(&mut world, &stream, &surface, far)
            .expect("evict");
        assert_eq!(
            layer.seated_count(),
            0,
            "a mouth 1.5 km away must be dropped"
        );
    }
}
