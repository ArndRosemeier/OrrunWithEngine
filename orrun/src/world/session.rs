//! One in-process life cycle: atlas → loading → walking the world.
//!
//! The session owns the player, the stream, and the transition between looking
//! at the map and standing on it. Control is withheld until the ground under
//! the spawn is *actually* resident, because the drawn chunk carries the only
//! collision data there is — a guessed spawn height would drop the player
//! through the terrain or leave them hovering.
//!
//! The view is first person: there is no avatar to draw, the camera *is* the
//! player, and the mouse is captured for as long as they are in the world.

use std::sync::Arc;

use engine::camera::{Camera, MAX_PITCH_DEGREES};
use engine::error::EngineError;
use engine::space::{GlobalPosition, GlobalXZ, RenderOrigin};
use engine::world::{Frame, World};
use engine::Key;
use glam::Vec2;
use thiserror::Error;

use super::coords::{Heading, CHUNK_SPAN_M};
use super::entry::{resolve_spawn, EntryError, SpawnPose, WorldEntryRequest};
use super::scatter::{ScatterCatalog, ScatterError, ScatterLayer};
use super::surface::ContinentalSurface;
use super::world_stream::WorldStream;

/// Gap between the contact height and the soles, so rounding never buries them.
const FOOT_CLEARANCE_M: f32 = 0.05;
/// Camera height above the feet.
const EYE_HEIGHT_M: f32 = 1.7;
const WALK_SPEED: f32 = 10.0;
const SPRINT_SPEED: f32 = 28.0;
const FLY_SPEED: f32 = 40.0;
const FLY_SPRINT_SPEED: f32 = 160.0;
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

    #[error("no world has been entered yet")]
    NoWorld,

    #[error("spawn chunk at ({x:.0} m, {z:.0} m) is resident but carries no contact grid")]
    MissingContact { x: f64, z: f64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState {
    /// Looking at the map.
    Atlas,
    /// Entry accepted; streaming the ground the player will land on.
    Loading,
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
    /// Unit-ish direction on the XZ plane, already rotated into world space.
    pub direction: Vec2,
    /// Vertical intent while flying: +1 up, −1 down.
    pub lift: f32,
    /// Metres to travel this frame at the current speed.
    pub step_m: f32,
    pub yaw_delta_degrees: f32,
    pub pitch_delta_degrees: f32,
    /// F went down this frame: swap between walking and flying.
    pub toggle_fly: bool,
}

impl WalkInput {
    pub const IDLE: Self = Self {
        direction: Vec2::ZERO,
        lift: 0.0,
        step_m: 0.0,
        yaw_delta_degrees: 0.0,
        pitch_delta_degrees: 0.0,
        toggle_fly: false,
    };

    /// Read one frame of first-person controls.
    ///
    /// W/S and Up/Down walk, Q/E sidestep, A/D and Left/Right turn, the mouse
    /// looks, Shift sprints, F toggles flying, and Space/Ctrl climb and descend
    /// while airborne.
    pub fn from_frame(frame: &Frame, yaw_degrees: f32, mode: Locomotion) -> Self {
        let keys = &frame.input;
        let forward = (keys.axis(Key::S, Key::W) + keys.axis(Key::Down, Key::Up)).clamp(-1.0, 1.0);
        let strafe = keys.axis(Key::Q, Key::E).clamp(-1.0, 1.0);
        let facing = Camera::facing_xz(yaw_degrees);
        let right = Camera::right_xz(yaw_degrees);
        let dir = (right * strafe + facing * forward).normalize_or_zero();

        let sprint = keys.down(Key::Shift);
        let speed = match (mode, sprint) {
            (Locomotion::Walk, false) => WALK_SPEED,
            (Locomotion::Walk, true) => SPRINT_SPEED,
            (Locomotion::Fly, false) => FLY_SPEED,
            (Locomotion::Fly, true) => FLY_SPRINT_SPEED,
        };

        let steer = (keys.axis(Key::A, Key::D) + keys.axis(Key::Left, Key::Right)).clamp(-1.0, 1.0);
        let look = keys.mouse_delta();

        Self {
            direction: Vec2::new(dir.x, dir.z),
            lift: keys.axis(Key::Ctrl, Key::Space),
            step_m: speed * frame.dt,
            yaw_delta_degrees: turn_degrees(steer, look.x, frame.dt),
            // Raw motion counts +y downward; pushing the mouse away looks up.
            pitch_delta_degrees: -look.y * MOUSE_DEGREES_PER_COUNT,
            toggle_fly: keys.pressed(Key::F),
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

pub struct WorldSession {
    surface: Arc<ContinentalSurface>,
    stream: WorldStream,
    /// Ground cover, once the prop meshes have been uploaded.
    scatter: Option<ScatterLayer>,
    state: SessionState,
    spawn: Option<SpawnPose>,
    player: Option<Player>,
}

impl WorldSession {
    pub fn new(surface: Arc<ContinentalSurface>) -> Self {
        let stream = WorldStream::new(Arc::clone(&surface));
        Self {
            surface,
            stream,
            scatter: None,
            state: SessionState::Atlas,
            spawn: None,
            player: None,
        }
    }

    pub fn surface(&self) -> &ContinentalSurface {
        &self.surface
    }

    pub fn state(&self) -> SessionState {
        self.state
    }

    pub fn spawn(&self) -> Option<SpawnPose> {
        self.spawn
    }

    pub fn stream(&self) -> &WorldStream {
        &self.stream
    }

    /// Global position of the player, once they exist.
    pub fn player_position(&self) -> Option<GlobalPosition> {
        self.player.map(|p| p.position)
    }

    /// Resolve the entry point and start loading its ground.
    ///
    /// Fails before anything is torn down when the selection has no valid
    /// spawn, so a bad click leaves the atlas exactly as it was.
    pub fn begin_entry(
        &mut self,
        world: &mut World,
        request: WorldEntryRequest,
    ) -> Result<SpawnPose, SessionError> {
        let pose = resolve_spawn(&self.surface, request)?;

        // Prop meshes are uploaded once, on the first entry, so a player who
        // never leaves the map never pays for them.
        if self.scatter.is_none() {
            let catalog = ScatterCatalog::discover()?;
            self.scatter = Some(ScatterLayer::install(
                world,
                &catalog,
                self.surface.world_seed(),
            )?);
        }
        if let Some(scatter) = self.scatter.as_mut() {
            scatter.clear(world)?;
        }

        self.stream.reset(world);
        world.set_render_origin(RenderOrigin::snapped(pose.ground(), CHUNK_SPAN_M)?)?;
        self.spawn = Some(pose);
        self.player = Some(Player {
            position: pose.position(),
            yaw_degrees: pose.heading().degrees(),
            pitch_degrees: 0.0,
            mode: Locomotion::Walk,
            heading: pose.heading().direction(),
        });
        self.state = SessionState::Loading;
        Ok(pose)
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

    /// Advance the session for one rendered frame.
    pub fn update(&mut self, world: &mut World, frame: &Frame) -> Result<(), SessionError> {
        let (yaw, mode) = self
            .player
            .map(|p| (p.yaw_degrees, p.mode))
            .unwrap_or((0.0, Locomotion::Walk));
        self.step(world, WalkInput::from_frame(frame, yaw, mode))
    }

    /// Advance the session with explicit intent (also the headless path).
    pub fn step(&mut self, world: &mut World, input: WalkInput) -> Result<(), SessionError> {
        // The mouse only belongs to the game while the player is in it; the
        // atlas and the loading screen need a cursor.
        world.set_pointer_lock(self.state == SessionState::World);
        match self.state {
            SessionState::Atlas => Ok(()),
            SessionState::Loading => self.update_loading(world),
            SessionState::World => self.update_world(world, input),
        }
    }

    fn update_loading(&mut self, world: &mut World) -> Result<(), SessionError> {
        let spawn = self.spawn.ok_or(SessionError::NoWorld)?;
        let focus = spawn.ground();
        self.stream.sync(world, focus, None)?;
        if !self.stream.required_ready(focus) {
            return Ok(());
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
        });
        self.state = SessionState::World;
        Ok(())
    }

    fn update_world(&mut self, world: &mut World, input: WalkInput) -> Result<(), SessionError> {
        let mut player = self.player.ok_or(SessionError::NoWorld)?;

        if input.toggle_fly {
            player.mode = player.mode.toggled();
        }
        player.yaw_degrees = wrap_degrees(player.yaw_degrees + input.yaw_delta_degrees);
        player.pitch_degrees = (player.pitch_degrees + input.pitch_delta_degrees)
            .clamp(-MAX_PITCH_DEGREES, MAX_PITCH_DEGREES);

        let step = input.step_m as f64;
        if input.direction.length_squared() > 0.0 {
            player.position.x += input.direction.x as f64 * step;
            player.position.z += input.direction.y as f64 * step;
            player.heading = input.direction;
        }

        let foot = player.position.horizontal();
        let rebased = self.stream.maybe_rebase(world, foot)?;
        self.stream.sync(world, foot, Some(player.heading))?;
        if let Some(scatter) = self.scatter.as_mut() {
            scatter.follow(world, &self.stream, &self.surface, foot, rebased)?;
        }

        match player.mode {
            // Only the resident bake may move the player vertically: falling
            // back to a fresh surface query here would put the feet on a
            // different surface than the one being drawn.
            Locomotion::Walk => {
                if let Some(ground) = self.stream.contact_height(foot) {
                    player.position.y = (ground + FOOT_CLEARANCE_M) as f64;
                }
            }
            Locomotion::Fly => player.position.y += input.lift as f64 * step,
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

    /// Where the camera sits, once the player exists.
    pub fn eye_position(&self) -> Option<GlobalPosition> {
        self.player.map(|p| p.eye())
    }

    /// Grass, stones, and trees standing around the player.
    pub fn scattered_count(&self) -> usize {
        self.scatter.as_ref().map_or(0, ScatterLayer::placed_count)
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
