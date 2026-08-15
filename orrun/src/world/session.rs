//! One in-process life cycle: atlas → travel → walking the world.
//!
//! The session owns the player, the stream, and the transition between looking
//! at the map and standing on it. Control is withheld until the ground under
//! the spawn is *actually* resident, because the drawn chunk carries the walk
//! surface — a guessed spawn height would drop the player through the terrain
//! or leave them hovering. Trees and house walls are separate colliders on
//! that same world, and the player body collides with them by default.
//!
//! The view is first person: there is no avatar to draw, the camera *is* the
//! player. The mouse is captured only once they click in the world, and the
//! window gives it back on Escape or when it loses focus.

use std::sync::Arc;
use std::time::Instant;

use engine::camera::{Camera, MAX_PITCH_DEGREES};
use engine::collision::ActorBody;
use engine::error::EngineError;
use engine::place::GlobalPlace;
use engine::space::{GlobalPosition, GlobalXZ, RenderOrigin};
use engine::world::{EntityId, Frame, Haze, Sky, World};
use engine::{Key, MouseButton, SpaceId};
use glam::{Vec2, Vec3};
use thiserror::Error;

use super::coords::{Heading, CHUNK_SPAN_M};
use super::doors::DoorLayer;
use super::entry::{resolve_spawn, EntryError, SpawnPose, WorldEntryRequest};
use super::fauna::{FaunaError, FaunaLayer};
use super::footprint::BuildingIndex;
use super::look::install_daylight;
use super::paths::PathLayer;
use super::ponds::{PondField, PondWindow};
use super::scatter::{ScatterCatalog, ScatterError, ScatterLayer};
use super::settlement::{HamletStand, SettlementError, SettlementLayer};
use super::surface::ContinentalSurface;
use super::travel::{
    travel_view, ContinentProxySpec, TravelPhase, TravelSource, TravelTimings, TravelView,
};
use super::world_stream::{WorldStream, FAR_VIEW_M};

/// Gap between the contact height and the soles, so rounding never buries them.
const FOOT_CLEARANCE_M: f32 = 0.05;
/// Camera height above the feet.
const EYE_HEIGHT_M: f32 = 1.7;
const WALK_SPEED: f32 = 10.0;
const SPRINT_SPEED: f32 = 28.0;
const FLY_SPEED: f32 = 40.0;
const FLY_SPRINT_SPEED: f32 = 160.0;
/// Take-off speed. At [`GRAVITY`] this clears about a metre and a half.
const JUMP_SPEED: f32 = 8.0;
const GRAVITY: f32 = 22.0;
const TURN_DEGREES_PER_S: f32 = 120.0;
/// Degrees per unit of raw pointer motion.
const MOUSE_DEGREES_PER_COUNT: f32 = 0.12;

#[derive(Debug, Error)]
pub enum SessionError {
    #[error(transparent)]
    Entry(#[from] EntryError),

    #[error(transparent)]
    Engine(#[from] EngineError),

    #[error(transparent)]
    Scatter(#[from] ScatterError),

    #[error(transparent)]
    Settlement(#[from] SettlementError),

    #[error(transparent)]
    Fauna(#[from] FaunaError),

    #[error("no world has been entered yet")]
    NoWorld,

    #[error("travel {phase} toward ({x:.0} m, {z:.0} m): {detail}")]
    Travel {
        phase: TravelPhase,
        x: f64,
        z: f64,
        detail: String,
    },

    #[error("spawn chunk at ({x:.0} m, {z:.0} m) is resident but carries no contact grid")]
    MissingContact { x: f64, z: f64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState {
    /// Looking at the map.
    Atlas,
    /// Flying the atlas trip: ascent, proxy, hold, descent.
    Travel,
    /// Walking.
    World,
}

/// Walking on the ground, or flying free of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Locomotion {
    Walk,
    Fly,
}

impl Locomotion {
    fn toggled(self) -> Self {
        match self {
            Self::Walk => Self::Fly,
            Self::Fly => Self::Walk,
        }
    }
}

/// One frame of movement intent, independent of how it was produced.
#[derive(Clone, Copy, Debug)]
pub struct WalkInput {
    /// Unit move vector in world space. Walking keeps this on the XZ plane;
    /// flying points it along the look, so W follows the gaze up or down.
    pub direction: Vec3,
    /// Space went down this frame: jump, if the feet are on the ground.
    pub jump: bool,
    /// Seconds this intent covers, for gravity.
    pub dt: f32,
    /// Metres to travel this frame at the current speed.
    pub step_m: f32,
    pub yaw_delta_degrees: f32,
    pub pitch_delta_degrees: f32,
    /// F went down this frame: swap between walking and flying.
    pub toggle_fly: bool,
    /// The player clicked in the world, which is how they ask for the mouse to
    /// be captured for looking.
    pub capture_look: bool,
    /// E went down this frame: toggle the door in reach. That frame E is not a strafe.
    pub interact: bool,
    /// Space went down during travel: skip the current cinematic beat.
    /// Never skips destination readiness.
    pub skip_travel: bool,
}

impl WalkInput {
    pub const IDLE: Self = Self {
        direction: Vec3::ZERO,
        jump: false,
        dt: 0.0,
        step_m: 0.0,
        yaw_delta_degrees: 0.0,
        pitch_delta_degrees: 0.0,
        toggle_fly: false,
        capture_look: false,
        interact: false,
        skip_travel: false,
    };

    /// Read one frame of first-person controls.
    ///
    /// W/S and Up/Down walk (or fly along the look), Q/E sidestep, A/D and
    /// Left/Right turn, the mouse looks, Shift sprints, F toggles flying,
    /// Space jumps while walking, and E toggles a door in reach (that frame
    /// E is not a strafe).
    ///
    /// `mouse_look` says whether the pointer belongs to the game this frame.
    /// Raw motion arrives whether or not it does, and turning the view with a
    /// cursor the player is using elsewhere is how a first-person camera ends
    /// up spinning on its own.
    pub fn from_frame(
        frame: &Frame,
        yaw_degrees: f32,
        pitch_degrees: f32,
        mode: Locomotion,
        mouse_look: bool,
    ) -> Self {
        let keys = &frame.input;
        let interact = keys.pressed(Key::E);
        let forward = (keys.axis(Key::S, Key::W) + keys.axis(Key::Down, Key::Up)).clamp(-1.0, 1.0);
        let strafe = if interact {
            if keys.down(Key::Q) {
                -1.0
            } else {
                0.0
            }
        } else {
            keys.axis(Key::Q, Key::E).clamp(-1.0, 1.0)
        };
        let right = Camera::right_xz(yaw_degrees);
        let dir = match mode {
            Locomotion::Fly => {
                let look = Camera::direction(yaw_degrees, pitch_degrees);
                (look * forward + right * strafe).normalize_or_zero()
            }
            Locomotion::Walk => {
                let facing = Camera::facing_xz(yaw_degrees);
                (right * strafe + facing * forward).normalize_or_zero()
            }
        };

        let sprint = keys.down(Key::Shift);
        let speed = match (mode, sprint) {
            (Locomotion::Walk, false) => WALK_SPEED,
            (Locomotion::Walk, true) => SPRINT_SPEED,
            (Locomotion::Fly, false) => FLY_SPEED,
            (Locomotion::Fly, true) => FLY_SPRINT_SPEED,
        };

        let steer = (keys.axis(Key::A, Key::D) + keys.axis(Key::Left, Key::Right)).clamp(-1.0, 1.0);
        let look = if mouse_look {
            keys.mouse_delta()
        } else {
            Vec2::ZERO
        };

        Self {
            direction: dir,
            jump: keys.pressed(Key::Space),
            dt: frame.dt,
            step_m: speed * frame.dt,
            yaw_delta_degrees: turn_degrees(steer, look.x, frame.dt),
            // Raw motion counts +y downward; pushing the mouse away looks up.
            pitch_delta_degrees: -look.y * MOUSE_DEGREES_PER_COUNT,
            toggle_fly: keys.pressed(Key::F),
            capture_look: keys.mouse_clicked(MouseButton::Left),
            interact,
            skip_travel: keys.pressed(Key::Space),
        }
    }
}

/// Yaw change for one frame of steering, positive meaning "turn right".
///
/// Yaw grows toward +X, but screen-right at yaw 0 is −X, so turning right has
/// to subtract: get this backwards and the mouse and the turn keys both fight
/// the view.
pub(super) fn turn_degrees(steer_right: f32, mouse_dx: f32, dt: f32) -> f32 {
    -(steer_right * TURN_DEGREES_PER_S * dt + mouse_dx * MOUSE_DEGREES_PER_COUNT)
}

#[derive(Clone, Copy, Debug)]
struct Player {
    /// Where the feet are: the eye sits [`EYE_HEIGHT_M`] above this.
    position: GlobalPosition,
    yaw_degrees: f32,
    pitch_degrees: f32,
    mode: Locomotion,
    /// Last horizontal movement direction, used to bias streaming.
    heading: Vec2,
    /// Vertical speed while a jump is in the air. Zero on the ground and in flight.
    vy: f32,
    airborne: bool,
    /// Capsule the engine slides against trees and walls. Collision is on.
    body: ActorBody,
}

impl Player {
    fn eye(&self) -> GlobalPosition {
        GlobalPosition::at(
            self.position.x,
            self.position.y + EYE_HEIGHT_M as f64,
            self.position.z,
        )
    }
}

struct InstalledProxy {
    land: EntityId,
    sea: EntityId,
    marker: EntityId,
}

struct TravelState {
    phase: TravelPhase,
    elapsed: f32,
    request: WorldEntryRequest,
    source: Option<TravelSource>,
    approach: SpawnPose,
    handed_off: bool,
    destination_ready: bool,
    revealed_destination: bool,
    handoffs: u32,
}

pub struct WorldSession {
    surface: Arc<ContinentalSurface>,
    /// Sub-atlas water around the player, scanned off the main thread.
    ponds: PondWindow,
    stream: WorldStream,
    /// Ground cover, once the prop meshes have been uploaded.
    scatter: Option<ScatterLayer>,
    /// Hamlets around the player, once the house meshes have been uploaded.
    settlements: Option<SettlementLayer>,
    /// Draped roads and measured bridges.
    paths: Option<PathLayer>,
    /// Near-player wildlife, once the animal meshes have been loaded.
    fauna: Option<FaunaLayer>,
    /// Swinging house leaves and the one live portal interior.
    doors: DoorLayer,
    state: SessionState,
    /// The request being loaded, until the water under it has been scanned and
    /// the spawn it resolves to is known.
    entering: Option<WorldEntryRequest>,
    spawn: Option<SpawnPose>,
    player: Option<Player>,
    timings: TravelTimings,
    travel: Option<TravelState>,
    travel_space: Option<SpaceId>,
    proxy_spec: Option<ContinentProxySpec>,
    proxy: Option<InstalledProxy>,
}

impl WorldSession {
    pub fn new(surface: Arc<ContinentalSurface>) -> Self {
        let ponds = PondWindow::new(Arc::clone(&surface));
        let stream = WorldStream::new(Arc::clone(&surface), ponds.shared());
        Self {
            surface,
            ponds,
            stream,
            scatter: None,
            settlements: None,
            paths: None,
            fauna: None,
            doors: DoorLayer::new(),
            state: SessionState::Atlas,
            entering: None,
            spawn: None,
            player: None,
            timings: TravelTimings::cinematic(),
            travel: None,
            travel_space: None,
            proxy_spec: None,
            proxy: None,
        }
    }

    /// Headless and unit tests wait on streaming, not on the camera script.
    pub fn with_instant_travel(mut self) -> Self {
        self.timings = TravelTimings::instant();
        self
    }

    pub fn set_travel_timings(&mut self, timings: TravelTimings) {
        self.timings = timings;
    }

    /// Attach a proxy built off the render thread. Travel will upload it once.
    pub fn attach_proxy(&mut self, spec: ContinentProxySpec) {
        self.proxy_spec = Some(spec);
    }

    pub fn surface(&self) -> &ContinentalSurface {
        &self.surface
    }

    /// Packed hamlets currently seated around the player.
    pub fn hamlets(&self) -> &[HamletStand] {
        self.settlements
            .as_ref()
            .map(SettlementLayer::hamlets)
            .unwrap_or(&[])
    }

    pub fn state(&self) -> SessionState {
        self.state
    }

    pub fn spawn(&self) -> Option<SpawnPose> {
        self.spawn
    }

    /// Where the session is taking the player: the resolved spawn once it is
    /// known, and until then the point that was asked for.
    pub fn destination(&self) -> Option<GlobalXZ> {
        if let Some(travel) = &self.travel {
            return Some(
                self.spawn
                    .map(|pose| pose.ground())
                    .unwrap_or_else(|| travel.request.requested()),
            );
        }
        self.spawn
            .map(|pose| pose.ground())
            .or_else(|| self.entering.map(|request| request.requested()))
    }

    pub fn travel_phase(&self) -> Option<TravelPhase> {
        self.travel.as_ref().map(|t| t.phase)
    }

    /// How many times this trip reset the destination stream. A legal trip is 1.
    pub fn travel_handoffs(&self) -> u32 {
        self.travel.as_ref().map(|t| t.handoffs).unwrap_or(0)
    }

    pub fn destination_ready(&self) -> bool {
        self.travel.as_ref().is_some_and(|t| t.destination_ready)
    }

    /// 0 is a clear frame, 1 is a full speed/cloud veil.
    pub fn travel_veil(&self) -> f32 {
        self.travel_view_now().map(|v| v.veil).unwrap_or(0.0)
    }

    pub fn stream(&self) -> &WorldStream {
        &self.stream
    }

    /// The sub-atlas water the world is currently being cut with.
    pub fn ponds(&self) -> Arc<PondField> {
        self.ponds.field()
    }

    fn plot_index(&self) -> Arc<BuildingIndex> {
        self.settlements
            .as_ref()
            .map(SettlementLayer::plot_index)
            .unwrap_or_else(|| Arc::new(BuildingIndex::new(Vec::new())))
    }

    /// Global position of the player, once they exist.
    pub fn player_position(&self) -> Option<GlobalPosition> {
        self.player.map(|p| p.position)
    }

    /// Resolve the entry point and start the atlas trip.
    ///
    /// Fails before anything is torn down when the selection has no valid
    /// spawn, so a bad click leaves the atlas exactly as it was. Source
    /// terrain stays up through the ascent; the destination stream is reset
    /// once, at the haze handoff.
    pub fn begin_entry(
        &mut self,
        world: &mut World,
        request: WorldEntryRequest,
    ) -> Result<(), SessionError> {
        // The spawn this resolves to is not the one the player gets: the water
        // under it is not scanned yet, and a pond the resolver cannot see is a
        // pond it will stand them in. It is here to answer the one question that
        // has to be answered before anything is torn down — whether the request
        // has any walkable ground at all — and to say where the render origin
        // goes, which the true spawn will be a few metres from.
        let approach = resolve_spawn(&self.surface, &self.ponds.field(), request)?;
        let source = self.player.map(|p| TravelSource {
            eye: p.eye(),
            yaw_degrees: p.yaw_degrees,
            pitch_degrees: p.pitch_degrees,
        });
        self.ensure_travel_space(world)?;
        self.ensure_proxy(world)?;

        let first_entry = source.is_none();
        self.travel = Some(TravelState {
            phase: if first_entry {
                TravelPhase::Transfer
            } else {
                TravelPhase::Ascent
            },
            elapsed: 0.0,
            request,
            source,
            approach,
            handed_off: false,
            destination_ready: false,
            revealed_destination: false,
            handoffs: 0,
        });
        self.entering = Some(request);
        self.spawn = None;
        self.state = SessionState::Travel;
        if first_entry {
            self.enter_proxy_space(world)?;
            self.handoff_destination(world)?;
        }
        Ok(())
    }

    /// Go back to the map without discarding the loaded world.
    pub fn return_to_atlas(&mut self) {
        if self.state == SessionState::World {
            self.state = SessionState::Atlas;
        }
    }

    /// Re-enter the world that is already streamed.
    pub fn resume(&mut self) -> Result<(), SessionError> {
        if self.player.is_none() {
            return Err(SessionError::NoWorld);
        }
        self.state = SessionState::World;
        Ok(())
    }

    /// Fraction of the entry ring that is resident, for the loading screen.
    pub fn loading_progress(&self) -> f32 {
        let Some(spawn) = self.spawn else {
            return 0.0;
        };
        if self.stream.required_ready(spawn.ground()) {
            return 1.0;
        }
        let pending = self.stream.pending_count() as f32;
        let resident = self.stream.resident_count() as f32;
        (resident / (resident + pending).max(1.0)).clamp(0.0, 0.95)
    }

    /// What the loading screen should say. Progress stays at 0 until spawn is
    /// known, which used to read as a stuck ground streamer while water scanned.
    pub fn loading_status(&self) -> String {
        if let Some(travel) = &self.travel {
            if !travel.handed_off {
                return "rising…".into();
            }
            if travel.destination_ready {
                return match travel.phase {
                    TravelPhase::Hold => "holding above the stand…".into(),
                    TravelPhase::Descent => "descending…".into(),
                    other => format!("{other}…"),
                };
            }
        }
        if self.spawn.is_none() {
            if self.scatter.is_none() {
                return "loading props…".into();
            }
            return "scanning water…".into();
        }
        if self.settlements.as_ref().is_some_and(SettlementLayer::busy) {
            return "seating hamlet…".into();
        }
        if self.scatter.as_ref().is_some_and(ScatterLayer::busy) {
            return "growing cover…".into();
        }
        if self.fauna.as_ref().is_some_and(FaunaLayer::busy) {
            return "reading animals…".into();
        }
        if self.fauna.as_ref().is_some_and(FaunaLayer::filling) {
            return "wildlife…".into();
        }
        format!(
            "streaming ground… {:.0}%   ({} chunks resident)",
            self.loading_progress() * 100.0,
            self.stream.resident_count()
        )
    }

    /// Advance the session for one rendered frame.
    ///
    /// Mouse-look is taken, never assumed: the pointer is captured when the
    /// player clicks in the world, and the window hands it back on Escape or
    /// when it loses focus. Grabbing it at startup, as this used to, pins the
    /// cursor of somebody who has not even entered the world yet.
    pub fn update(&mut self, world: &mut World, frame: &Frame) -> Result<(), SessionError> {
        let (yaw, pitch, mode) = self
            .player
            .map(|p| (p.yaw_degrees, p.pitch_degrees, p.mode))
            .unwrap_or((0.0, 0.0, Locomotion::Walk));
        let looking = world.pointer_lock();
        self.step(
            world,
            WalkInput::from_frame(frame, yaw, pitch, mode, looking),
        )
    }

    /// Advance the session with explicit intent (also the headless path).
    pub fn step(&mut self, world: &mut World, input: WalkInput) -> Result<(), SessionError> {
        // Whatever anyone asked for, the map and the loading screen need a
        // cursor the player can use; in the world they have to ask for it.
        if self.state == SessionState::World {
            if input.capture_look {
                world.set_pointer_lock(true);
            }
        } else {
            world.set_pointer_lock(false);
        }
        match self.state {
            SessionState::Atlas => Ok(()),
            SessionState::Travel => self.update_travel(world, input),
            SessionState::World => self.update_world(world, input),
        }
    }

    fn update_travel(&mut self, world: &mut World, input: WalkInput) -> Result<(), SessionError> {
        match self.update_travel_inner(world, input) {
            Ok(()) => Ok(()),
            Err(err @ SessionError::Travel { .. }) => Err(err),
            Err(err) => Err(self.wrap_travel_error(err)),
        }
    }

    fn wrap_travel_error(&self, err: SessionError) -> SessionError {
        let dest = self.destination().unwrap_or(GlobalXZ::at(0.0, 0.0));
        SessionError::Travel {
            phase: self
                .travel
                .as_ref()
                .map(|t| t.phase)
                .unwrap_or(TravelPhase::Hold),
            x: dest.x,
            z: dest.z,
            detail: err.to_string(),
        }
    }

    fn update_travel_inner(
        &mut self,
        world: &mut World,
        input: WalkInput,
    ) -> Result<(), SessionError> {
        if self.travel.is_none() {
            panic!("SessionState::Travel without a travel record");
        }
        self.assert_proxy_resident(world);
        if self.travel.as_ref().expect("travel").handed_off {
            if self.update_loading(world)? {
                self.travel.as_mut().expect("travel").destination_ready = true;
            }
        }
        self.place_travel_marker(world)?;

        loop {
            let phase = self.travel.as_ref().expect("travel").phase;
            let elapsed = self.travel.as_ref().expect("travel").elapsed;
            let duration = self.timings.duration_of(phase);
            let skip = input.skip_travel;
            let ready = self.travel.as_ref().expect("travel").destination_ready;
            let beat_done = match phase {
                TravelPhase::Hold => ready,
                TravelPhase::Descent => ready && (skip || elapsed >= duration),
                _ => skip || elapsed >= duration,
            };
            if !beat_done {
                break;
            }
            match phase {
                TravelPhase::Ascent => {
                    self.enter_proxy_space(world)?;
                    self.handoff_destination(world)?;
                    self.advance_phase(TravelPhase::Transfer);
                }
                TravelPhase::Transfer => self.advance_phase(TravelPhase::Hold),
                TravelPhase::Hold => {
                    if !ready {
                        panic!("travel hold ended before the destination ring was ready");
                    }
                    self.reveal_destination(world)?;
                    self.advance_phase(TravelPhase::Descent);
                }
                TravelPhase::Descent => {
                    if !ready {
                        panic!("travel descent cannot land before the destination is ready");
                    }
                    self.land_from_travel(world)?;
                    return Ok(());
                }
            }
        }

        if let Some(travel) = self.travel.as_mut() {
            travel.elapsed += input.dt;
        }
        if let Some(view) = self.travel_view_now() {
            self.apply_travel_view(world, view)?;
        }
        Ok(())
    }

    fn advance_phase(&mut self, next: TravelPhase) {
        let travel = self.travel.as_mut().expect("travel");
        travel.phase = next;
        travel.elapsed = 0.0;
    }

    fn handoff_destination(&mut self, world: &mut World) -> Result<(), SessionError> {
        let travel = self.travel.as_ref().expect("travel");
        if travel.handed_off {
            panic!(
                "destination stream was reset twice during travel toward ({:.0}, {:.0})",
                travel.request.requested().x,
                travel.request.requested().z
            );
        }
        let request = travel.request;
        let approach = travel.approach;

        self.doors.evict(world)?;
        if let Some(scatter) = self.scatter.as_mut() {
            scatter.clear(world)?;
        }
        if let Some(settlements) = self.settlements.as_mut() {
            settlements.clear(world)?;
        }
        if let Some(paths) = self.paths.as_mut() {
            paths.clear(world)?;
        }
        if let Some(fauna) = self.fauna.as_mut() {
            fauna.clear(world)?;
        }

        self.stream.reset(world);
        world.set_render_origin(RenderOrigin::snapped(approach.ground(), CHUNK_SPAN_M)?)?;
        self.spawn = None;
        self.player = None;
        self.entering = Some(request);
        let travel = self.travel.as_mut().expect("travel");
        travel.handed_off = true;
        travel.handoffs += 1;
        Ok(())
    }

    fn enter_proxy_space(&mut self, world: &mut World) -> Result<(), SessionError> {
        let space = self.travel_space.expect("travel space");
        world.live_in(space)?;
        world.set_shadows(None);
        Ok(())
    }

    fn reveal_destination(&mut self, world: &mut World) -> Result<(), SessionError> {
        world.live_in(SpaceId::DEFAULT)?;
        world.set_view_distance(FAR_VIEW_M)?;
        if let Some(travel) = self.travel.as_mut() {
            travel.revealed_destination = true;
        }
        Ok(())
    }

    fn land_from_travel(&mut self, world: &mut World) -> Result<(), SessionError> {
        let player = self.player.ok_or(SessionError::NoWorld)?;
        install_daylight(world);
        world.live_in(SpaceId::DEFAULT)?;
        world.look_first_person_global(player.eye(), player.yaw_degrees, player.pitch_degrees)?;
        self.travel = None;
        self.state = SessionState::World;
        Ok(())
    }

    fn ensure_travel_space(&mut self, world: &mut World) -> Result<(), SessionError> {
        if self.travel_space.is_some() {
            return Ok(());
        }
        let space = world.space("travel")?;
        world.set_space_draws_environment(space, true)?;
        self.travel_space = Some(space);
        Ok(())
    }

    fn ensure_proxy(&mut self, world: &mut World) -> Result<(), SessionError> {
        if self.proxy.is_some() {
            return Ok(());
        }
        if self.proxy_spec.is_none() {
            self.proxy_spec = Some(ContinentProxySpec::build(&self.surface));
        }
        let spec = self.proxy_spec.as_ref().expect("proxy spec");
        let space = self.travel_space.expect("travel space");
        let prev = world.spawning_in();
        world.in_space(space)?;
        let land =
            world.spawn_anchored(spec.land_mesh()?, GlobalPlace::at(GlobalPosition::ORIGIN))?;
        let sea =
            world.spawn_anchored(spec.sea_mesh()?, GlobalPlace::at(GlobalPosition::ORIGIN))?;
        let marker = world.spawn_anchored(
            spec.marker_mesh()?,
            GlobalPlace::at(GlobalPosition::at(0.0, 1_200.0, 0.0)),
        )?;
        world.in_space(prev)?;
        self.proxy = Some(InstalledProxy { land, sea, marker });
        Ok(())
    }

    fn assert_proxy_resident(&self, world: &World) {
        let Some(proxy) = &self.proxy else {
            panic!("travel started without an uploaded continent proxy");
        };
        world.entity(proxy.land).expect("continent proxy land mesh");
        world.entity(proxy.sea).expect("continent proxy sea mesh");
        world
            .entity(proxy.marker)
            .expect("continent proxy destination marker");
    }

    fn place_travel_marker(&mut self, world: &mut World) -> Result<(), SessionError> {
        let Some(dest) = self.destination() else {
            return Ok(());
        };
        let Some(spec) = self.proxy_spec.as_ref() else {
            return Ok(());
        };
        let Some(proxy) = self.proxy.as_ref() else {
            return Ok(());
        };
        let y = f64::from(spec.height_at(dest) + 1_200.0);
        world.set_anchored_place(
            proxy.marker,
            GlobalPlace::at(GlobalPosition::at(dest.x, y, dest.z)),
        )?;
        Ok(())
    }

    fn travel_view_now(&self) -> Option<TravelView> {
        let travel = self.travel.as_ref()?;
        let spec = self.proxy_spec.as_ref()?;
        let from = travel
            .source
            .map(|s| s.eye.horizontal())
            .unwrap_or_else(|| travel.request.requested());
        let to = self
            .spawn
            .map(|p| p.ground())
            .unwrap_or_else(|| travel.request.requested());
        let landing = self.player.map(|p| p.eye());
        let heading = self
            .player
            .map(|p| p.yaw_degrees)
            .or_else(|| self.spawn.map(|p| p.heading().degrees()))
            .unwrap_or(0.0);
        let duration = self.timings.duration_of(travel.phase);
        let t = if duration.is_finite() && duration > 0.0 {
            (travel.elapsed / duration).clamp(0.0, 1.0)
        } else {
            1.0
        };
        Some(travel_view(
            travel.phase,
            t,
            travel.elapsed,
            travel.source,
            from,
            to,
            landing,
            heading,
            spec.extent_m(),
        ))
    }

    fn apply_travel_view(&self, world: &mut World, view: TravelView) -> Result<(), SessionError> {
        world.set_view_distance(view.view_distance_m)?;
        world.look_at_global(view.look.eye, view.look.target)?;
        world.camera.fov_y_degrees = view.fov_y_degrees;
        world.camera.near = view.near_m;
        let sky = world.sky().unwrap_or_else(Sky::daylight);
        world.set_haze(Some(
            Haze::new(sky.horizon, view.haze_visibility_m).thinning_above(0.0, 4_000.0),
        ));
        Ok(())
    }

    fn update_loading(&mut self, world: &mut World) -> Result<bool, SessionError> {
        // Water first, and off this thread. Ground baked before the ponds were
        // known would have to be thrown away, so nothing else starts until the
        // window covers the spawn. The window reaches kilometres and the resolver
        // searches metres, so the requested point centres both.
        if let Some(request) = self.entering {
            // Start the water window first so it runs while prop GLBs are read.
            if self.scatter.is_none() {
                let _ = self.ponds.traced(request.requested());
                let catalog = ScatterCatalog::discover()?;
                self.scatter = Some(ScatterLayer::install(
                    world,
                    &catalog,
                    self.surface.world_seed(),
                )?);
                self.settlements =
                    Some(SettlementLayer::install(world, self.surface.world_seed())?);
                self.paths = Some(PathLayer::new());
                self.fauna = Some(FaunaLayer::install(self.surface.world_seed())?);
            }
            if !self.ponds.traced(request.requested()) {
                return Ok(false);
            }
            let pose = resolve_spawn(&self.surface, &self.ponds.field(), request)?;
            self.spawn = Some(pose);
            self.player = Some(Player {
                position: pose.position(),
                yaw_degrees: pose.heading().degrees(),
                pitch_degrees: 0.0,
                mode: Locomotion::Walk,
                heading: pose.heading().direction(),
                vy: 0.0,
                airborne: false,
                body: ActorBody::player(),
            });
            self.entering = None;
        }
        let spawn = self.spawn.ok_or(SessionError::NoWorld)?;
        let focus = spawn.ground();
        self.stream.sync(world, focus, None)?;
        if !self.stream.required_ready(focus) {
            return Ok(false);
        }
        let rebuilt = if let Some(settlements) = self.settlements.as_mut() {
            settlements.follow(
                world,
                &self.stream,
                &self.surface,
                &self.ponds.field(),
                focus,
                false,
            )?
        } else {
            false
        };
        if self.settlements.as_ref().is_some_and(SettlementLayer::busy) {
            return Ok(false);
        }
        if rebuilt {
            let plots = self.plot_index();
            self.stream.set_house_plots(world, (*plots).clone())?;
            self.stream.sync(world, focus, None)?;
            if !self.stream.required_ready(focus) {
                return Ok(false);
            }
        }
        if self.settlements.as_ref().is_some_and(|s| s.staging(focus)) {
            return Ok(false);
        }
        let plots = self.plot_index();
        if let Some(scatter) = self.scatter.as_mut() {
            let t = Instant::now();
            scatter.follow(
                world,
                &self.stream,
                &self.surface,
                &self.ponds.field(),
                focus,
                &plots,
                false,
            )?;
            world.hitch_span(
                "scatter",
                hitch_ms(t),
                format!(
                    "placed={} backlog={} far_queue={} sow_ms={:.1} busy={}",
                    scatter.placed_count(),
                    scatter.upload_backlog(),
                    scatter.far_backlog(),
                    scatter.sow_ms(),
                    scatter.busy(),
                ),
            );
        }
        if let Some(fauna) = self.fauna.as_mut() {
            let t = Instant::now();
            let hamlets = self
                .settlements
                .as_ref()
                .map(SettlementLayer::hamlets)
                .unwrap_or(&[]);
            fauna.follow(
                world,
                &self.stream,
                &self.surface,
                &self.ponds.field(),
                plots.as_ref(),
                hamlets,
                focus,
                focus,
                0.0,
            )?;
            world.hitch_span(
                "fauna",
                hitch_ms(t),
                format!(
                    "agents={} born={} died={} backlog={} loading={}",
                    fauna.agent_count(),
                    fauna.last_born(),
                    fauna.last_died(),
                    fauna.filling(),
                    fauna.busy(),
                ),
            );
            if fauna.busy() {
                return Ok(false);
            }
        }
        let Some(ground) = self.stream.contact_height(focus) else {
            return Err(SessionError::MissingContact {
                x: focus.x,
                z: focus.z,
            });
        };

        let position = GlobalPosition::at(focus.x, (ground + FOOT_CLEARANCE_M) as f64, focus.z);
        self.player = Some(Player {
            position,
            yaw_degrees: spawn.heading().degrees(),
            pitch_degrees: 0.0,
            mode: Locomotion::Walk,
            heading: spawn.heading().direction(),
            vy: 0.0,
            airborne: false,
            body: ActorBody::player(),
        });
        Ok(true)
    }

    fn update_world(&mut self, world: &mut World, input: WalkInput) -> Result<(), SessionError> {
        let mut player = self.player.ok_or(SessionError::NoWorld)?;

        if input.toggle_fly {
            player.mode = player.mode.toggled();
            player.vy = 0.0;
            player.airborne = false;
        }
        player.yaw_degrees = wrap_degrees(player.yaw_degrees + input.yaw_delta_degrees);
        player.pitch_degrees = (player.pitch_degrees + input.pitch_delta_degrees)
            .clamp(-MAX_PITCH_DEGREES, MAX_PITCH_DEGREES);

        let step = input.step_m as f64;
        let mut dx = 0.0;
        let mut dz = 0.0;
        if input.direction.length_squared() > 0.0 {
            dx = input.direction.x as f64 * step;
            dz = input.direction.z as f64 * step;
            if player.mode == Locomotion::Fly {
                player.position.y += input.direction.y as f64 * step;
            }
            let flat = Vec2::new(input.direction.x, input.direction.z);
            if flat.length_squared() > 0.0 {
                player.heading = flat.normalize();
            }
        }
        match player.mode {
            Locomotion::Walk => {
                let to = world.move_actor(&player.body, player.position.horizontal(), dx, dz);
                player.position.x = to.x;
                player.position.z = to.z;
            }
            Locomotion::Fly => {
                player.position.x += dx;
                player.position.z += dz;
            }
        }

        let foot = player.position.horizontal();
        // Before the streamer, so a chunk is never baked against a window that
        // has stopped reaching it.
        let t = Instant::now();
        self.ponds.follow(foot);
        world.hitch_span("ponds", hitch_ms(t), String::new());

        let t = Instant::now();
        let rebased = self.stream.maybe_rebase(world, foot)?;
        let rebuilt = if let Some(settlements) = self.settlements.as_mut() {
            settlements.follow(
                world,
                &self.stream,
                &self.surface,
                &self.ponds.field(),
                foot,
                rebased,
            )?
        } else {
            false
        };
        if rebuilt {
            let plots = self.plot_index();
            self.stream.set_house_plots(world, (*plots).clone())?;
        }
        self.stream.sync(world, foot, Some(player.heading))?;
        world.hitch_span(
            "stream",
            hitch_ms(t),
            format!(
                "resident={} pending={} walked_pending={} rebase={rebased} hamlet_rebuild={rebuilt} houses={} tiles={}/{}",
                self.stream.resident_count(),
                self.stream.pending_count(),
                self.stream.walked_pending_count(),
                self.settlements.as_ref().map_or(0, SettlementLayer::placed_count),
                self.settlements.as_ref().map_or(0, SettlementLayer::tile_gpu_count),
                self.settlements.as_ref().map_or(0, SettlementLayer::tile_backlog),
            ),
        );
        let plots = self.plot_index();
        if let Some(scatter) = self.scatter.as_mut() {
            let t = Instant::now();
            scatter.follow(
                world,
                &self.stream,
                &self.surface,
                &self.ponds.field(),
                foot,
                &plots,
                rebased,
            )?;
            world.hitch_span(
                "scatter",
                hitch_ms(t),
                format!(
                    "placed={} backlog={} far_queue={} sow_ms={:.1} busy={}",
                    scatter.placed_count(),
                    scatter.upload_backlog(),
                    scatter.far_backlog(),
                    scatter.sow_ms(),
                    scatter.busy(),
                ),
            );
        }
        let hamlets = self
            .settlements
            .as_ref()
            .map_or(&[][..], SettlementLayer::hamlets);
        if let Some(paths) = self.paths.as_mut() {
            let t = Instant::now();
            paths.follow(
                world,
                &self.surface,
                &self.ponds.field(),
                hamlets,
                foot,
                self.stream.resident_count(),
                self.stream.walked_pending_count(),
                rebased,
            )?;
            world.hitch_span("paths", hitch_ms(t), format!("hamlets={}", hamlets.len()));
        }
        if let Some(fauna) = self.fauna.as_mut() {
            let t = Instant::now();
            fauna.follow(
                world,
                &self.stream,
                &self.surface,
                &self.ponds.field(),
                &plots,
                hamlets,
                foot,
                foot,
                input.dt,
            )?;
            world.hitch_span(
                "fauna",
                hitch_ms(t),
                format!(
                    "agents={} born={} died={} backlog={} loading={}",
                    fauna.agent_count(),
                    fauna.last_born(),
                    fauna.last_died(),
                    fauna.filling(),
                    fauna.busy(),
                ),
            );
        }

        self.doors.evict_if_missing(
            world,
            self.settlements
                .as_ref()
                .map(SettlementLayer::doors)
                .unwrap_or(&[]),
        )?;
        self.doors.frame(
            world,
            self.settlements
                .as_ref()
                .map(SettlementLayer::doors)
                .unwrap_or(&[]),
            player.position,
            player.yaw_degrees,
            input.interact,
            input.dt,
        )?;
        let hidden_leaf = self.doors.hidden_leaf();
        if let Some(settlements) = self.settlements.as_mut() {
            settlements.hide_leaf(world, hidden_leaf)?;
        }

        // Probe with the actor's centre, not the soles. The latter sit only a
        // few centimetres above the opening's lower edge and can miss the
        // rectangle on uneven door terrain.
        let portal_probe_y = f64::from(player.body.height * 0.5);
        let portal_probe = GlobalPosition::at(
            player.position.x,
            player.position.y + portal_probe_y,
            player.position.z,
        );
        let mut local = world.to_render(portal_probe)?;
        let mut yaw = player.yaw_degrees;
        if let Some(entered) = world.travel(&mut local, &mut yaw) {
            let landed_probe = world.to_global(local)?;
            player.position = GlobalPosition::at(
                landed_probe.x,
                landed_probe.y - portal_probe_y,
                landed_probe.z,
            );
            player.yaw_degrees = yaw;
            self.doors
                .settle_after_travel(world, &player.body, entered, &mut player.position);
        }

        match player.mode {
            // Only the resident bake may move the player vertically: falling
            // back to a fresh surface query here would put the feet on a
            // different surface than the one being drawn. Indoors the house
            // floor is the contact, not the outdoor plot cap.
            Locomotion::Walk => {
                let indoor = self.doors.indoor_floor_y(world, player.position);
                let ground = if indoor.is_some() {
                    indoor
                } else {
                    let stand = player.position.horizontal();
                    let terrain = self.stream.contact_height(stand);
                    let deck = self.paths.as_ref().and_then(|p| p.deck_height(stand));
                    match (terrain, deck) {
                        (Some(t), Some(d)) => Some(t.max(d)),
                        (Some(t), None) => Some(t),
                        (None, Some(d)) => Some(d),
                        (None, None) => None,
                    }
                };
                apply_walk_height(&mut player, ground, input.jump, input.dt);
            }
            Locomotion::Fly => {}
        }

        world.look_first_person_global(player.eye(), player.yaw_degrees, player.pitch_degrees)?;

        self.player = Some(player);
        Ok(())
    }

    /// Ground height reported by the resident mesh under a global point.
    pub fn contact_height(&self, p: GlobalXZ) -> Option<f32> {
        self.stream.contact_height(p)
    }

    /// Compass heading the player is facing.
    pub fn player_heading(&self) -> Option<Heading> {
        self.player
            .and_then(|p| Heading::from_degrees(p.yaw_degrees).ok())
    }

    /// How the player is currently getting around.
    pub fn locomotion(&self) -> Option<Locomotion> {
        self.player.map(|p| p.mode)
    }

    /// HUD line when a house door is in reach.
    pub fn door_hint(&self) -> Option<&str> {
        self.doors.hint()
    }

    /// Where the camera sits, once the player exists.
    pub fn eye_position(&self) -> Option<GlobalPosition> {
        self.player.map(|p| p.eye())
    }

    /// Grass, stones, and trees standing around the player.
    pub fn scattered_count(&self) -> usize {
        self.scatter.as_ref().map_or(0, ScatterLayer::placed_count)
    }

    /// Live animals around the player.
    pub fn fauna_count(&self) -> usize {
        self.fauna.as_ref().map_or(0, FaunaLayer::agent_count)
    }

    /// What the last sow of ground cover took on its own thread.
    pub fn sow_ms(&self) -> f32 {
        self.scatter.as_ref().map_or(0.0, ScatterLayer::sow_ms)
    }
}

/// Keep yaw in [0, 360) so it stays exact after hours of turning.
///
/// `rem_euclid` can round a hair below zero up to a full turn, which is one
/// past what [`Heading`] accepts.
fn wrap_degrees(degrees: f32) -> f32 {
    let wrapped = degrees.rem_euclid(360.0);
    if wrapped >= 360.0 {
        0.0
    } else {
        wrapped
    }
}

fn hitch_ms(start: Instant) -> f32 {
    start.elapsed().as_secs_f32() * 1000.0
}

fn apply_walk_height(player: &mut Player, ground: Option<f32>, jump: bool, dt: f32) {
    let floor = ground.map(|g| (g + FOOT_CLEARANCE_M) as f64);
    if !player.airborne {
        if let Some(floor) = floor {
            player.position.y = floor;
        }
        player.vy = 0.0;
        if jump && floor.is_some() {
            player.vy = JUMP_SPEED;
            player.airborne = true;
        } else {
            return;
        }
    }
    player.vy -= GRAVITY * dt;
    player.position.y += f64::from(player.vy) * f64::from(dt);
    if let Some(floor) = floor {
        if player.position.y <= floor {
            player.position.y = floor;
            player.vy = 0.0;
            player.airborne = false;
        }
    }
}
