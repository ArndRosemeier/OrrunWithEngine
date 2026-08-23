//! Atlas volumetric cave mouths: hillside bowls, background chamber
//! generation via `cave_gen`, portal into the interior space.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::Instant;

use cave_gen::{
    cathedral_reference, entrance_reference, validate_socket, CaveChamber, ChamberBrief,
};
use engine::collision::{ColliderLayer, ColliderShape, StaticCollider};
use engine::color::Color;
use engine::error::{EngineError, EngineResult};
use engine::mesh::Mesh;
use engine::place::GlobalPlace;
use engine::portal::{PortalId, PortalSettings};
use engine::space::{GlobalPosition, GlobalXZ};
use engine::world::{EntityId, World};
use engine::{FieldGpuContext, SpaceId};
use glam::Vec3;
use thiserror::Error;

use super::footprint::{BuildingPlot, CavePlot};
use super::surface::{ContinentalSurface, CaveMouthPin};

/// Keep generated chambers until the player walks this far away.
const CACHE_M: f64 = 1_000.0;
/// Mouth bowl radius on the heightfield.
const MOUTH_HALF_M: f32 = 7.0;
/// Portal opening half-width inside the bowl.
const PORTAL_HALF_M: f32 = 2.6;
/// How deep the carved dish goes below the rim.
const BOWL_DEPTH_M: f32 = 3.4;
/// Open the interior once the walker is this close to the mouth.
pub const CAVE_LIVE_OPEN_M: f64 = 14.0;
/// Hint / sole reach around a seated mouth.
const MOUTH_REACH_M: f64 = 6.0;
const CAVE_COLLIDER_LAYER: ColliderLayer = 8;

#[derive(Debug, Error)]
pub enum CaveError {
    #[error(transparent)]
    Engine(#[from] EngineError),

    #[error("cave generation failed for mouth {mouth}: {why}")]
    Generation { mouth: i32, why: String },
}

enum BuildMsg {
    Ready {
        id: i32,
        /// Rock shell + formations + polish, authored chamber-local.
        mesh: Mesh,
        landing_y: f32,
        landing_yaw: f32,
        /// Chamber-local exit socket for the interior portal.
        exit: cave_gen::ExitSocket,
        gen_ms: f32,
    },
    Failed { id: i32, why: String },
}

struct Pending {
    rx: Receiver<BuildMsg>,
}

struct SeatedMouth {
    pin: CaveMouthPin,
    plot: CavePlot,
    collar: Vec<EntityId>,
    built: Option<BuiltChamber>,
}

struct BuiltChamber {
    mesh: Mesh,
    landing_y: f32,
    landing_yaw: f32,
    /// Chamber-local exit socket the interior portal seats against.
    exit: cave_gen::ExitSocket,
    /// Total generation time, for the HUD.
    gen_ms: f32,
}

struct LiveCave {
    mouth_id: i32,
    space: SpaceId,
    entities: Vec<EntityId>,
    hatch_out: EntityId,
    portal: PortalId,
    landing_y: f32,
    landing_yaw: f32,
    /// Chamber-local exit socket (interior portal anchor).
    exit: cave_gen::ExitSocket,
    mouth_at: GlobalPosition,
}

/// Nearby volumetric cave mouths: hillside bowls first, chambers generated
/// on a worker thread when approached, one live portal each.
pub struct CaveLayer {
    seated: HashMap<i32, SeatedMouth>,
    pending: Option<Pending>,
    queued: std::collections::HashSet<i32>,
    live: Option<LiveCave>,
    hint: Option<String>,
    started: Option<Instant>,
    hatch_armed: bool,
}

impl CaveLayer {
    pub fn install() -> Self {
        Self {
            seated: HashMap::new(),
            pending: None,
            queued: std::collections::HashSet::new(),
            live: None,
            hint: None,
            started: None,
            hatch_armed: false,
        }
    }

    pub fn hint(&self) -> Option<&str> {
        self.hint.as_deref()
    }

    pub fn generating(&self) -> bool {
        self.pending.is_some() || self.seated.values().any(|s| s.built.is_none())
    }

    /// HUD line while a nearby chamber is still growing.
    pub fn build_status(&self) -> Option<String> {
        if !self.generating() {
            return None;
        }
        let elapsed = self.started.map(|t| t.elapsed().as_secs()).unwrap_or(0);
        let clock = if elapsed == 0 {
            String::new()
        } else {
            format!("  {elapsed}s")
        };
        let mut cutting: Vec<&CaveMouthPin> =
            self.seated.values().filter(|s| s.built.is_none()).map(|s| &s.pin).collect();
        cutting.sort_by_key(|pin| pin.id);
        let line = match cutting.as_slice() {
            [] => "growing cave chambers…".into(),
            [pin] => format!("growing a {} cavern…", pin.tier_name()),
            many => {
                let names = many
                    .iter()
                    .map(|pin| pin.tier_name())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("growing {} caverns ({names})…", many.len())
            }
        };
        Some(format!("{line}{clock}"))
    }

    pub fn plots(&self) -> Vec<BuildingPlot> {
        self.seated
            .values()
            .map(|s| BuildingPlot::Cave(s.plot))
            .collect()
    }

    pub fn seated_count(&self) -> usize {
        self.seated.len()
    }

    pub fn has_live(&self) -> bool {
        self.live.is_some()
    }

    pub fn live_mouth_id(&self) -> Option<i32> {
        self.live.as_ref().map(|live| live.mouth_id)
    }

    /// True when the walker is living in a generated cave interior. The
    /// session switches to full 3D collision movement there (the heightfield
    /// has no ground inside a chamber).
    pub fn living_in_cave(&self, world: &World) -> bool {
        self.live
            .as_ref()
            .is_some_and(|live| world.living_in() == live.space)
    }

    /// Generation time of the chamber backing the nearest seated mouth, if any.
    pub fn last_gen_ms(&self, feet: GlobalPosition) -> Option<f32> {
        let focus = GlobalXZ::at(feet.x, feet.z);
        let near = self
            .seated
            .values()
            .filter(|s| s.pin.at.distance(focus) <= MOUTH_REACH_M * 4.0)
            .min_by_key(|s| s.pin.id)?;
        Some(near.built.as_ref()?.gen_ms)
    }

    /// Seat bowls immediately and start chamber jobs for mouths in reach.
    pub fn follow(
        &mut self,
        world: &mut World,
        surface: &Arc<ContinentalSurface>,
        focus: GlobalXZ,
    ) -> Result<bool, CaveError> {
        let nearby: Vec<CaveMouthPin> = surface
            .cave_pins()
            .iter()
            .copied()
            .filter(|pin| pin.at.distance(focus) <= CACHE_M)
            .collect();
        let nearby_ids: std::collections::HashSet<i32> =
            nearby.iter().map(|p| p.id).collect();
        let mut plots_changed = false;

        if let Some(pending) = self.pending.take() {
            loop {
                match pending.rx.try_recv() {
                    Ok(BuildMsg::Ready {
                        id,
                        mesh,
                        landing_y,
                        landing_yaw,
                        exit,
                        gen_ms,
                    }) => {
                        self.queued.remove(&id);
                        if let Some(seated) = self.seated.get_mut(&id) {
                            seated.built = Some(BuiltChamber {
                                mesh,
                                landing_y,
                                landing_yaw,
                                exit,
                                gen_ms,
                            });
                        }
                    }
                    Ok(BuildMsg::Failed { id, why }) => {
                        panic!("cave mouth {id} worker failed: {why}");
                    }
                    Err(TryRecvError::Empty) => {
                        self.pending = Some(pending);
                        break;
                    }
                    Err(TryRecvError::Disconnected) => {
                        if !self.queued.is_empty() {
                            panic!(
                                "cave worker exited while still growing {:?}",
                                self.queued
                            );
                        }
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
            if self.live.as_ref().is_some_and(|l| l.mouth_id == id) {
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
            let need: Vec<CaveMouthPin> = nearby
                .iter()
                .copied()
                .filter(|pin| {
                    self.seated
                        .get(&pin.id)
                        .is_some_and(|s| s.built.is_none() && !self.queued.contains(&pin.id))
                })
                .collect();
            if !need.is_empty() {
                for pin in &need {
                    self.queued.insert(pin.id);
                }
                if self.started.is_none() {
                    self.started = Some(Instant::now());
                }
                let (tx, rx) = mpsc::channel();
                self.pending = Some(Pending { rx });
                std::thread::Builder::new()
                    .name("caves".into())
                    .spawn(move || {
                        for pin in need {
                            let msg = match std::panic::catch_unwind(|| build_chamber(&pin)) {
                                Ok(built) => built,
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
                    .expect("cave thread");
            }
        }

        Ok(plots_changed)
    }

    pub fn frame(
        &mut self,
        world: &mut World,
        feet: GlobalPosition,
        _yaw_degrees: f32,
    ) -> Result<(), CaveError> {
        self.hint = None;
        let focus = GlobalXZ::at(feet.x, feet.z);

        // Keep-live policy mirrors dungeons.
        let inside = self
            .live
            .as_ref()
            .is_some_and(|live| world.living_in() == live.space);
        if let Some(live) = self.live.as_ref() {
            let keep = inside
                || self
                    .seated
                    .get(&live.mouth_id)
                    .is_some_and(|s| s.pin.at.distance(focus) <= CACHE_M);
            if !keep {
                self.evict_live(world)?;
            }
        }

        if self.live.is_none() {
            if let Some(id) = self
                .nearest_within(feet, CAVE_LIVE_OPEN_M)
                .filter(|s| s.built.is_some())
                .map(|s| s.pin.id)
            {
                self.begin_live(world, id)?;
            }
        }

        if self.nearest_within(feet, MOUTH_REACH_M).is_none() {
            self.hatch_armed = true;
        }
        Ok(())
    }

    fn nearest_within(&self, feet: GlobalPosition, metres: f64) -> Option<&SeatedMouth> {
        let focus = GlobalXZ::at(feet.x, feet.z);
        self.seated
            .values()
            .filter(|s| s.pin.at.distance(focus) <= metres)
            .min_by_key(|s| s.pin.id)
    }

    fn begin_live(&mut self, world: &mut World, mouth_id: i32) -> Result<(), CaveError> {
        let seated = self.seated.get_mut(&mouth_id).expect("seated mouth");
        let built = seated
            .built
            .as_mut()
            .expect("begin_live requires a built chamber");
        let plot = seated.plot;
        let exit = built.exit;
        let landing_y = built.landing_y;
        let landing_yaw = built.landing_yaw;
        let mesh = built.mesh.clone();
        let space = world.space(format!("cave-{mouth_id}"))?;
        // Portal quad at the bowl's uphill wall; the chamber's throat mates
        // with it (entrance brief exits +X toward its tip past x=60).
        let portal_xz = portal_world_xz(plot);
        let hatch_y = plot.floor_y + 1.9;
        let hatch_out = world.spawn_anchored(
            Mesh::opening(PORTAL_HALF_M * 2.0, PORTAL_HALF_M * 2.0).expect("cave hatch"),
            GlobalPlace::at(GlobalPosition::at(portal_xz.0, f64::from(hatch_y), portal_xz.1)),
        )?;
        world.in_space(space)?;
        // The interior portal quad sits just inside the carved throat mouth:
        // the exit points +X, so the in-side hatch faces back out (yaw 90°,
        // pitch 90° mirrors the dungeon hatch orientation pair).
        let local_in = exit.position - exit.direction * 1.2;
        let hatch_in = world.spawn_anchored(
            Mesh::opening(PORTAL_HALF_M * 2.0, PORTAL_HALF_M * 2.0).expect("cave hatch"),
            GlobalPlace::at(GlobalPosition::at(
                f64::from(local_in.x),
                f64::from(landing_y + PORTAL_HALF_M),
                f64::from(local_in.z),
            ))
            .with_yaw_deg(90.0)
            .with_pitch_deg(90.0),
        )?;
        let entity = world.spawn_in(space, mesh)?;
        let entities = vec![hatch_in, entity];
        world.in_space(SpaceId::DEFAULT)?;
        let portal = world.create_portal(hatch_out, hatch_in, PortalSettings::TELEPORTING)?;
        let mouth_at = GlobalPosition::at(portal_xz.0, f64::from(hatch_y), portal_xz.1);
        let live = LiveCave {
            mouth_id,
            space,
            entities,
            hatch_out,
            portal,
            landing_y,
            landing_yaw,
            exit,
            mouth_at,
        };
        // Solid rock ring around the chamber so walkers cannot leave through
        // walls; centred on the chamber-local exit socket, in the cave space.
        let colliders = chamber_wall_colliders(&live);
        world
            .collision_mut()
            .replace_layer(CAVE_COLLIDER_LAYER, colliders)
            .expect("cave colliders");
        self.live = Some(live);
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
        world.collision_mut().clear_layer(CAVE_COLLIDER_LAYER);
        self.hatch_armed = false;
        Ok(())
    }

    pub fn settle_after_travel(
        &self,
        world: &mut World,
        body: &engine::collision::ActorBody,
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
            let landing = GlobalPosition::at(
                live.mouth_at.x,
                f64::from(live.landing_y),
                live.mouth_at.z,
            );
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
            if let Some(seated) = self.seated.get(&live.mouth_id) {
                let yaw = seated.plot.approach_yaw;
                let dist = seated.plot.half + 1.2;
                feet.x = seated.plot.at.x + f64::from(yaw.sin() * dist);
                feet.y = f64::from(seated.plot.rim_y);
                feet.z = seated.plot.at.z + f64::from(yaw.cos() * dist);
            }
        }
    }
}

/// Bowl centre offset of the portal: at the uphill wall (local −Z is
/// downhill since `approach_yaw` points uphill... see `seat_mouth`).
fn portal_world_xz(plot: CavePlot) -> (f64, f64) {
    let yaw = plot.approach_yaw.to_radians();
    (
        plot.at.x - f64::from(yaw.sin() * plot.half * 0.45),
        plot.at.z - f64::from(yaw.cos() * plot.half * 0.45),
    )
}

fn seat_mouth(
    world: &mut World,
    surface: &ContinentalSurface,
    pin: CaveMouthPin,
) -> Result<SeatedMouth, CaveError> {
    let rim_y = surface.column(pin.at).ground();
    let approach_yaw = downhill_yaw(surface, pin.at);
    let floor_y = rim_y - BOWL_DEPTH_M;
    let plot = CavePlot {
        at: pin.at,
        half: MOUTH_HALF_M,
        floor_y,
        rim_y,
        approach_yaw,
    };
    let collar = spawn_collar(world, &plot)?;
    Ok(SeatedMouth {
        pin,
        plot,
        collar,
        built: None,
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

/// Low rock lip ringing the bowl so the dish reads as a mouth from the map.
fn spawn_collar(world: &mut World, plot: &CavePlot) -> Result<Vec<EntityId>, CaveError> {
    const SEGMENTS: usize = 14;
    const T: f32 = 0.55;
    const H: f32 = 0.75;
    let stones = [
        Color::rgb(92, 86, 78),
        Color::rgb(104, 97, 87),
        Color::rgb(82, 80, 76),
    ];
    let radius = plot.half - T;
    let arc = std::f32::consts::TAU * radius / SEGMENTS as f32 * 1.06;
    let mut ids = Vec::new();
    for i in 0..SEGMENTS {
        let angle = std::f32::consts::TAU * i as f32 / SEGMENTS as f32;
        let from_approach = (angle - plot.approach_yaw + std::f32::consts::PI)
            .rem_euclid(std::f32::consts::TAU)
            - std::f32::consts::PI;
        if from_approach.abs() <= std::f32::consts::TAU / SEGMENTS as f32 {
            continue;
        }
        let dx = angle.sin() * radius;
        let dz = angle.cos() * radius;
        let at = GlobalPosition::at(
            plot.at.x + f64::from(dx),
            f64::from(plot.rim_y - H * 0.5),
            plot.at.z + f64::from(dz),
        );
        ids.push(
            world.spawn_anchored(
                Mesh::box_at(Vec3::ZERO, Vec3::ONE, stones[i % stones.len()])?,
                GlobalPlace::at(at)
                    .with_yaw_deg(angle.to_degrees())
                    .with_stretch(Vec3::new(arc, H, T)),
            )?,
        );
    }
    Ok(ids)
}

/// Generate one chamber off-thread with its own headless GPU context.
fn build_chamber(pin: &CaveMouthPin) -> BuildMsg {
    let outcome = (|| -> Result<(Mesh, f32, f32, cave_gen::ExitSocket, f32), String> {
        let brief: ChamberBrief = if pin.tier == 1 {
            cathedral_brief(pin.seed)
        } else {
            entrance_brief(pin.seed)
        };
        let ctx = FieldGpuContext::try_new().map_err(|e| e.to_string())?;
        let mut gpu_field = engine::GpuField::new(brief.voxel_size);
        let color = Color::rgb(132, 122, 108);
        let chamber = CaveChamber::generate(&mut gpu_field, &ctx, brief.clone(), color)
            .map_err(|e| e.to_string())?;
        let painted = chamber.painted.as_ref().expect("generate paints the field");
        for socket in &chamber.brief.exits {
            validate_socket(socket, chamber.brief.extent, |p| painted.sample_density(p))
                .map_err(|e| e.to_string())?;
        }
        let mesh = chamber.to_mesh(&ctx, color).map_err(|e| e.to_string())?;
        let gen_ms = chamber.timings().total_ms();

        // Landing: scan down the throat axis from mid-chamber height for the
        // first solid floor; the walker stands on it (eye height added later).
        let exit = &chamber.brief.exits[0];
        let probe_xz = exit.position - exit.direction * 2.5;
        let step = brief.voxel_size * 0.5;
        let mut floor = brief.extent.y * 0.55;
        loop {
            if painted.sample_density(Vec3::new(probe_xz.x, floor, probe_xz.z)) > 0.0 {
                break;
            }
            floor -= step;
            assert!(
                floor > 0.0,
                "cave mouth {} throat has no floor within the chamber",
                pin.id
            );
        }
        // The entrance brief exits +X; yaw 90° looks +X into the void.
        let exit = *exit;
        Ok((mesh, floor + 1.7, 90.0, exit, gen_ms))
    })();
    match outcome {
        Ok((mesh, landing_y, landing_yaw, exit, gen_ms)) => BuildMsg::Ready {
            id: pin.id,
            mesh,
            landing_y,
            landing_yaw,
            exit,
            gen_ms,
        },
        Err(why) => BuildMsg::Failed { id: pin.id, why },
    }
}

fn entrance_brief(seed: u64) -> ChamberBrief {
    let mut brief = entrance_reference().expect("entrance reference");
    brief.seed = seed;
    brief
}

fn cathedral_brief(seed: u64) -> ChamberBrief {
    let mut brief = cathedral_reference(seed).expect("cathedral reference");
    brief.seed = seed;
    brief
}

fn chamber_wall_colliders(live: &LiveCave) -> Vec<StaticCollider> {
    // Perimeter ring keeps the walker inside the chamber volume; the exact
    // shell collision arrives with walker-vs-density work. The ring is
    // centred on the interior portal (chamber-local exit), sized to the
    // entrance chamber extent, and lives in the cave space.
    let exit = live.exit;
    let mut out = Vec::new();
    for deg in (0..360).step_by(20) {
        let rad = (deg as f32).to_radians();
        let lx = exit.position.x + rad.cos() * 58.0;
        let lz = exit.position.z + rad.sin() * 58.0;
        out.push(
            StaticCollider::new(
                engine::space::GlobalXZ::at(f64::from(lx), f64::from(lz)),
                0.0,
                ColliderShape::Box {
                    half_x: 3.0,
                    half_z: 3.0,
                },
            )
            .with_y_span(f64::from(live.landing_y - 4.0), f64::from(live.landing_y + 40.0))
            .in_space(live.space),
        );
    }
    out
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<&str>()
        .copied()
        .map(str::to_string)
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".into())
}
