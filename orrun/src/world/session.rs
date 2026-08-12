//! One in-process life cycle: atlas → loading → walking the world.
//!
//! The session owns the player, the stream, and the transition between looking
//! at the map and standing on it. Control is withheld until the ground under
//! the spawn is *actually* resident, because the drawn chunk carries the only
//! collision data there is — a guessed spawn height would drop the player
//! through the terrain or leave them hovering.

use std::sync::Arc;

use engine::error::EngineError;
use engine::mesh::Mesh;
use engine::place::GlobalPlace;
use engine::space::{GlobalPosition, GlobalXZ, RenderOrigin};
use engine::world::{EntityId, Frame, World};
use engine::Key;
use glam::Vec2;
use thiserror::Error;

use super::coords::{Heading, CHUNK_SPAN_M};
use super::entry::{resolve_spawn, EntryError, SpawnPose, WorldEntryRequest};
use super::surface::ContinentalSurface;
use super::world_stream::WorldStream;

/// Eye/foot offset so the walker mesh rests on the ground.
const FOOT_CLEARANCE_M: f32 = 0.05;
const WALK_SPEED: f32 = 10.0;
const SPRINT_SPEED: f32 = 28.0;
const TURN_DEGREES_PER_S: f32 = 90.0;
const CAMERA_DISTANCE: f32 = 14.0;
const CAMERA_HEIGHT: f32 = 7.0;

#[derive(Debug, Error)]
pub enum SessionError {
    #[error(transparent)]
    Entry(#[from] EntryError),

    #[error(transparent)]
    Engine(#[from] EngineError),

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

/// One frame of walking intent, independent of how it was produced.
#[derive(Clone, Copy, Debug)]
pub struct WalkInput {
    /// Unit-ish direction on the XZ plane, already rotated into world space.
    pub direction: Vec2,
    pub speed_m_s: f32,
    pub yaw_delta_degrees: f32,
}

impl WalkInput {
    pub const IDLE: Self = Self {
        direction: Vec2::ZERO,
        speed_m_s: 0.0,
        yaw_delta_degrees: 0.0,
    };

    /// Read WASD / Q / E / Shift for one frame.
    pub fn from_frame(frame: &Frame, yaw_degrees: f32) -> Self {
        let dir = frame.input.move_dir_xz(yaw_degrees);
        let speed = if frame.input.down(Key::Shift) {
            SPRINT_SPEED
        } else {
            WALK_SPEED
        };
        Self {
            direction: Vec2::new(dir.x, dir.z),
            speed_m_s: speed * frame.dt,
            yaw_delta_degrees: frame.input.yaw_sign() * TURN_DEGREES_PER_S * frame.dt,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Player {
    position: GlobalPosition,
    yaw_degrees: f32,
    /// Last horizontal movement direction, used to bias streaming.
    heading: Vec2,
}

pub struct WorldSession {
    surface: Arc<ContinentalSurface>,
    stream: WorldStream,
    state: SessionState,
    spawn: Option<SpawnPose>,
    player: Option<Player>,
    walker: Option<EntityId>,
}

impl WorldSession {
    pub fn new(surface: Arc<ContinentalSurface>) -> Self {
        let stream = WorldStream::new(Arc::clone(&surface));
        Self {
            surface,
            stream,
            state: SessionState::Atlas,
            spawn: None,
            player: None,
            walker: None,
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

        self.stream.reset(world);
        world.set_render_origin(RenderOrigin::snapped(pose.ground(), CHUNK_SPAN_M)?)?;
        self.spawn = Some(pose);
        self.player = Some(Player {
            position: pose.position(),
            yaw_degrees: pose.heading().degrees(),
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
        let yaw = self.player.map(|p| p.yaw_degrees).unwrap_or_default();
        self.step(world, WalkInput::from_frame(frame, yaw))
    }

    /// Advance the session with explicit intent (also the headless path).
    pub fn step(&mut self, world: &mut World, input: WalkInput) -> Result<(), SessionError> {
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
        let player = Player {
            position,
            yaw_degrees: spawn.heading().degrees(),
            heading: spawn.heading().direction(),
        };
        self.player = Some(player);
        let place = GlobalPlace::at(position).with_yaw_deg(player.yaw_degrees);
        match self.walker {
            Some(id) => world.set_anchored_place(id, place)?,
            None => self.walker = Some(world.spawn_anchored(walker_mesh(), place)?),
        }
        self.state = SessionState::World;
        Ok(())
    }

    fn update_world(&mut self, world: &mut World, input: WalkInput) -> Result<(), SessionError> {
        let mut player = self.player.ok_or(SessionError::NoWorld)?;

        player.yaw_degrees += input.yaw_delta_degrees;
        if input.direction.length_squared() > 0.0 {
            let step = input.speed_m_s as f64;
            player.position.x += input.direction.x as f64 * step;
            player.position.z += input.direction.y as f64 * step;
            player.heading = input.direction;
        }

        let foot = player.position.horizontal();
        self.stream.maybe_rebase(world, foot)?;
        self.stream.sync(world, foot, Some(player.heading))?;

        // Only the resident bake may move the player vertically: falling back to
        // a fresh surface query here would put the feet on a different surface
        // than the one being drawn.
        if let Some(ground) = self.stream.contact_height(foot) {
            player.position.y = (ground + FOOT_CLEARANCE_M) as f64;
        }

        if let Some(id) = self.walker {
            world.set_anchored_place(
                id,
                GlobalPlace::at(player.position).with_yaw_deg(player.yaw_degrees),
            )?;
        }
        world.look_follow_global(
            player.position,
            player.yaw_degrees,
            CAMERA_DISTANCE,
            CAMERA_HEIGHT,
        )?;

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
}

fn walker_mesh() -> Mesh {
    let mut m = Mesh::new();
    m.add_box(
        (0.0, 0.55, 0.0),
        (0.55, 1.1, 0.35),
        engine::color::rgb(55, 90, 160),
    )
    .expect("walker body");
    m.add_box(
        (0.0, 1.35, 0.0),
        (0.4, 0.4, 0.4),
        engine::color::rgb(220, 190, 160),
    )
    .expect("walker head");
    m
}
