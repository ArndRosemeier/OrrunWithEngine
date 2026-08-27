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

use std::collections::VecDeque;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use engine::camera::{Camera, MAX_PITCH_DEGREES};
use engine::collision::ActorBody;
use engine::error::EngineError;
use engine::place::GlobalPlace;
use engine::space::{GlobalPosition, GlobalXZ, RenderOrigin};
use engine::world::{EntityId, Frame, Haze, Sky, World};
use engine::{EngineResult, Key, MouseButton, SpaceId};

use crate::controls::PressedActions;
use crate::settings::KeyBinds;
use glam::{Vec2, Vec3};
use thiserror::Error;

use super::cave::CaveError;
use super::coords::{CoordError, Heading, CHUNK_SPAN_M};
use super::doors::DoorLayer;
use super::dungeon::{DungeonError, DungeonLayer};
use super::entry::{resolve_spawn, EntryError, SpawnPose, WorldEntryRequest};
use super::fauna::{FaunaError, FaunaLayer};
use super::footprint::BuildingIndex;
use super::footprint::HousePlot;
use super::look::install_daylight;
use super::paths::PathLayer;
use super::ponds::{PondField, PondWindow};
use super::scatter::{ScatterCatalog, ScatterError, ScatterLayer};
use super::settlement::{HamletStand, HouseDoor, SettlementError, SettlementLayer};
use super::surface::ContinentalSurface;
use super::travel::{
    travel_view, ContinentProxySpec, TravelPhase, TravelSource, TravelTimings, TravelView,
};
use super::villagers::VillagerLayer;
use super::world_stream::{WorldStream, FAR_VIEW_M};

/// Gap between the contact height and the soles, so rounding never buries them.
const FOOT_CLEARANCE_M: f32 = 0.05;
/// Camera height above the feet.
const EYE_HEIGHT_M: f32 = 1.7;
const WALK_SPEED: f32 = crate::combat::WALK_MPS as f32;
const SPRINT_SPEED: f32 = crate::combat::SPRINT_MPS as f32;
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

    #[error(transparent)]
    Dungeon(#[from] DungeonError),

    #[error(transparent)]
    Cave(#[from] CaveError),

    #[error(transparent)]
    Coordinate(#[from] CoordError),

    #[error(transparent)]
    PlayerSave(#[from] crate::combat::PlayerSaveError),

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

/// Request to seat one canonical GameData mob once world placement is available.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeldFixtureRequest {
    mob_id: crate::gamedata::MobId,
}

impl HeldFixtureRequest {
    pub fn new(mob_id: crate::gamedata::MobId) -> Self {
        Self { mob_id }
    }

    pub fn mob_id(&self) -> &crate::gamedata::MobId {
        &self.mob_id
    }
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
#[derive(Clone, Debug)]
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
    /// Tab: cycle lock. Does not steal Escape or E.
    pub tab: bool,
    /// I: toggle the bag. Does not steal G / Tab / Esc / E / Q.
    pub bag: bool,
    /// Shift. Combat walk 4.5 / sprint 7. Not fly.
    pub sprint: bool,
    /// Combat actions resolved from the current binds this frame.
    pub actions: PressedActions,
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
        tab: false,
        bag: false,
        sprint: false,
        actions: PressedActions::NONE,
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
        binds: &KeyBinds,
        roster: &[crate::gamedata::ActionId],
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
            tab: keys.pressed(Key::Tab),
            bag: keys.pressed(Key::I),
            sprint,
            actions: crate::controls::resolve_pressed(binds, roster, keys),
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

const LEVEL_UP_NOTICE_S: f32 = 2.4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LevelUpNotice {
    name: String,
    level: i32,
}

impl LevelUpNotice {
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn level(&self) -> i32 {
        self.level
    }
}

fn level_up_notice(
    data: &crate::gamedata::GameData,
    event: crate::progression::LevelUpEvent,
) -> LevelUpNotice {
    let name = match event.proficiency {
        crate::progression::Proficiency::Skill(id) => data
            .skill(&id)
            .unwrap_or_else(|| panic!("level-up references unknown skill {id}"))
            .name()
            .to_string(),
        crate::progression::Proficiency::Hp => "HP".to_string(),
        crate::progression::Proficiency::Mana => "Mana".to_string(),
    };
    LevelUpNotice {
        name,
        level: event.new_level,
    }
}
enum GroundAuthority {
    Overland {
        contact: engine::contact::ContactSnapshot,
        surface: Arc<ContinentalSurface>,
        ponds: Arc<PondField>,
        off_window_ponds: Arc<RwLock<Vec<Arc<PondField>>>>,
    },
    Indoor {
        floor_y: f32,
    },
}

impl GroundAuthority {
    fn feet_y(&mut self, at: GlobalXZ) -> EngineResult<f64> {
        match self {
            Self::Indoor { floor_y } => Ok(f64::from(*floor_y + FOOT_CLEARANCE_M)),
            Self::Overland {
                contact,
                surface,
                ponds,
                off_window_ponds,
            } => {
                if let Some(ground) = contact.height_at(at) {
                    return Ok(f64::from(ground + FOOT_CLEARANCE_M));
                }
                let cached = {
                    let cache = off_window_ponds.read().expect("off-window pond cache");
                    cache.iter().find(|field| field.covers(at, 0.0)).cloned()
                };
                let governing_ponds = if ponds.covers(at, 0.0) {
                    Arc::clone(ponds)
                } else if let Some(field) = cached {
                    field
                } else {
                    let field = Arc::new(PondField::build_covering(surface, at, 1.0));
                    off_window_ponds
                        .write()
                        .expect("off-window pond cache")
                        .push(Arc::clone(&field));
                    field
                };
                let mut column = surface.column(at);
                governing_ponds.carve(at, &mut column);
                let ground = column.ground();
                if !ground.is_finite() {
                    return Err(EngineError::InvalidValue(format!(
                        "authoritative overland ground at ({:.3}, {:.3}) is non-finite",
                        at.x, at.z,
                    )));
                }
                Ok(f64::from(ground + FOOT_CLEARANCE_M))
            }
        }
    }
}

pub struct WorldSession {
    game_data: Arc<crate::gamedata::GameData>,
    surface: Arc<ContinentalSurface>,
    /// Sub-atlas water around the player, scanned off the main thread.
    ponds: PondWindow,
    /// Deterministic pond fields for combat-owned overland actors outside the streaming window.
    combat_ground_ponds: Arc<RwLock<Vec<Arc<PondField>>>>,
    stream: WorldStream,
    /// Ground cover, once the prop meshes have been uploaded.
    scatter: Option<ScatterLayer>,
    /// Hamlets around the player, once the house meshes have been uploaded.
    settlements: Option<SettlementLayer>,
    /// Draped roads and measured bridges.
    paths: Option<PathLayer>,
    /// Near-player wildlife, once the animal meshes have been loaded.
    fauna: Option<FaunaLayer>,
    /// Clipped humans in seated tier-0 hamlets.
    villagers: Option<VillagerLayer>,
    /// Swinging house leaves and the one live portal interior.
    doors: DoorLayer,
    /// Atlas dungeon mouths: pits, background generate, floor hatches.
    dungeons: Option<DungeonLayer>,
    /// Volumetric cave mouths: hillside bowls, lazy chamber generation, portals.
    caves: super::cave::CaveLayer,
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
    /// Last stand in the default space. A hatch teleport must not overwrite this.
    overworld: Option<(GlobalXZ, Heading)>,
    /// Target lock and canonical combat state.
    combat: crate::combat::WorldCombat,
    combat_hud: crate::combat::CombatHudSnapshot,
    combat_layer: super::combat_layer::CombatLayer,
    encounter: super::encounter::EncounterDirector,
    inventory: crate::inventory::Inventory,
    corpses: super::corpse::CorpseLifecycle,
    bag_open: bool,
    summon_open: bool,
    skill_open: bool,
    level_up_queue: VecDeque<LevelUpNotice>,
    current_level_up: Option<(LevelUpNotice, f32)>,
    last_shrine: Option<GlobalPlace>,
    key_binds: KeyBinds,
    /// Overland SettlementPin roster mobs are seated once after the default L1 wolves.
    roster_pins_seated: bool,
    overland_sites: Vec<super::sites::OverlandSite>,
    site_prop_ids: Vec<EntityId>,
    /// Live dungeon pin whose orc_skull hostiles are already in the list.
    dungeon_skulls_for: Option<i32>,
}

impl WorldSession {
    pub fn new(surface: Arc<ContinentalSurface>) -> Self {
        Self::with_game_data(
            surface,
            Arc::new(
                crate::gamedata::GameData::load(
                    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("../data/OrrunGameData.xml"),
                )
                .expect("canonical GameData"),
            ),
        )
    }

    pub fn with_game_data(
        surface: Arc<ContinentalSurface>,
        game_data: Arc<crate::gamedata::GameData>,
    ) -> Self {
        let ponds = PondWindow::new(Arc::clone(&surface));
        let stream = WorldStream::new(Arc::clone(&surface), ponds.shared());
        let combat = crate::combat::WorldCombat::with_game_data(Arc::clone(&game_data));
        let combat_hud = combat.hud_snapshot();
        Self {
            surface,
            ponds,
            combat_ground_ponds: Arc::new(RwLock::new(Vec::new())),
            stream,
            scatter: None,
            settlements: None,
            paths: None,
            fauna: None,
            villagers: None,
            doors: DoorLayer::new(),
            dungeons: None,
            caves: super::cave::CaveLayer::install(),
            state: SessionState::Atlas,
            entering: None,
            spawn: None,
            player: None,
            timings: TravelTimings::cinematic(),
            travel: None,
            travel_space: None,
            proxy_spec: None,
            proxy: None,
            overworld: None,
            game_data: Arc::clone(&game_data),
            combat,
            combat_hud,
            combat_layer: super::combat_layer::CombatLayer::install(),
            encounter: super::encounter::EncounterDirector::default(),
            inventory: crate::inventory::Inventory::create_kit(),
            corpses: super::corpse::CorpseLifecycle::default(),
            bag_open: false,
            summon_open: false,
            skill_open: false,
            level_up_queue: VecDeque::new(),
            current_level_up: None,
            last_shrine: None,
            key_binds: KeyBinds::for_default_profile(&game_data),
            roster_pins_seated: false,
            overland_sites: Vec::new(),
            site_prop_ids: Vec::new(),
            dungeon_skulls_for: None,
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

    pub fn game_data(&self) -> &Arc<crate::gamedata::GameData> {
        &self.game_data
    }

    pub fn combat_hud_snapshot(&self) -> &crate::combat::CombatHudSnapshot {
        &self.combat_hud
    }

    pub fn combat(&self) -> &crate::combat::WorldCombat {
        &self.combat
    }

    pub fn combat_mut(&mut self) -> &mut crate::combat::WorldCombat {
        &mut self.combat
    }

    /// Soft lock id. Tab cycles; does not steal Esc or E.
    pub fn lock_id(&self) -> Option<i32> {
        self.combat.lock_id()
    }

    /// Lock tell inspect: name + current HP. None if unlocked or name/hp unset.
    pub fn lock_name_hp(&self) -> Option<(&str, f64)> {
        let id = self.combat.lock_id()?;
        let h = self.combat.hostiles().iter().find(|h| h.idx == id)?;
        if h.name.is_empty() {
            return None;
        }
        Some((h.name.as_str(), self.combat.hostile_hp(h.actor_id())))
    }

    pub fn inventory(&self) -> &crate::inventory::Inventory {
        &self.inventory
    }

    pub fn inventory_mut(&mut self) -> &mut crate::inventory::Inventory {
        &mut self.inventory
    }

    pub fn bag_open(&self) -> bool {
        self.bag_open
    }

    pub fn set_bag_open(&mut self, open: bool) {
        self.bag_open = open;
    }

    pub fn loot_open(&self) -> bool {
        self.corpses.is_open()
    }

    pub fn loot_target(&self) -> Option<crate::combat::ActorId> {
        self.corpses.selected()
    }

    pub fn skill_open(&self) -> bool {
        self.skill_open
    }
    pub fn set_skill_open(&mut self, open: bool) {
        self.skill_open = open;
    }
    pub fn current_level_up_notice(&self) -> Option<&LevelUpNotice> {
        self.current_level_up.as_ref().map(|(notice, _)| notice)
    }
    fn queue_level_ups(&mut self, events: Vec<crate::progression::LevelUpEvent>) {
        self.level_up_queue.extend(
            events
                .into_iter()
                .map(|event| level_up_notice(&self.game_data, event)),
        );
        self.start_next_level_up_notice();
    }
    fn advance_level_up_notices(&mut self, dt: f32) {
        if let Some((_, elapsed)) = &mut self.current_level_up {
            *elapsed += dt;
            if *elapsed >= LEVEL_UP_NOTICE_S {
                self.current_level_up = None;
            }
        }
        self.start_next_level_up_notice();
    }
    fn start_next_level_up_notice(&mut self) {
        if self.current_level_up.is_none() {
            self.current_level_up = self.level_up_queue.pop_front().map(|notice| (notice, 0.0));
        }
    }
    pub fn summon_open(&self) -> bool {
        self.summon_open
    }

    pub fn set_summon_open(&mut self, open: bool) {
        self.summon_open = open;
    }

    pub fn summonable_mobs(&self) -> Vec<(crate::gamedata::MobId, String)> {
        self.game_data
            .mobs()
            .iter()
            .filter(|mob| crate::combat::catalog::mesh_spec(mob.id().as_str()).is_some())
            .map(|mob| (mob.id().clone(), mob.name().to_string()))
            .collect()
    }

    /// Queue one typed canonical mob for held fixture seating after world availability.
    pub fn request_held_fixture(&mut self, request: HeldFixtureRequest) {
        let mob_id = request.mob_id().clone();
        self.validate_encounter_mob(&mob_id);
        self.encounter
            .plan(super::encounter::EncounterPlan::Single { mob_id, held: true });
    }

    fn validate_encounter_mob(&self, mob_id: &crate::gamedata::MobId) {
        self.game_data
            .mob(mob_id)
            .unwrap_or_else(|| panic!("encounter requested unknown mob {mob_id}"));
        assert!(
            crate::combat::catalog::mesh_spec(mob_id.as_str()).is_some(),
            "encounter requested mob without combat mesh {mob_id}"
        );
    }

    pub fn summon_mob(
        &mut self,
        world: &mut World,
        mob_id: &crate::gamedata::MobId,
    ) -> Result<i32, SessionError> {
        let player = self.player.ok_or(SessionError::NoWorld)?;
        let definition = self
            .game_data
            .mob(mob_id)
            .unwrap_or_else(|| panic!("summon requested unknown mob {mob_id}"));
        if crate::combat::catalog::mesh_spec(definition.id().as_str()).is_none() {
            panic!("summon requested mob without combat mesh {mob_id}");
        }
        let facing = Camera::facing_xz(player.yaw_degrees);
        let x = player.position.x + f64::from(facing.x) * 30.0;
        let z = player.position.z + f64::from(facing.z) * 30.0;
        let idx = self
            .combat
            .hostiles()
            .iter()
            .map(|hostile| hostile.idx)
            .max()
            .unwrap_or(-1)
            + 1;
        let hostile = self
            .combat
            .hostile_metadata(definition.id(), idx, x, z, x, z);
        self.combat.add_hostile(hostile);
        self.combat_layer.mark_presentation_ready();
        self.respawn_hostile_meshes(world, &player)?;
        Ok(idx)
    }

    pub fn ground_pile(&self) -> Option<&crate::loot::GroundPile> {
        self.corpses.pile()
    }

    pub fn sparkle_visible(&self, world: &World) -> bool {
        self.corpses.marker_visible(world)
    }

    pub fn close_loot(&mut self) {
        self.corpses.close();
    }

    pub fn take_loot_item(&mut self, world: &mut World, item_i: usize) {
        let Some(actor_id) = self.corpses.selected() else {
            return;
        };
        let mut pile = self.corpses.pile().expect("selected corpse pile").clone();
        crate::loot::take_one(&mut self.inventory, &mut pile, item_i)
            .unwrap_or_else(|error| panic!("taking loot item {item_i} failed: {error}"));
        self.finish_loot_take(world, actor_id, pile);
    }

    pub fn take_all_loot(&mut self, world: &mut World) {
        let Some(actor_id) = self.corpses.selected() else {
            return;
        };
        let mut pile = self.corpses.pile().expect("selected corpse pile").clone();
        crate::loot::take_all(&mut self.inventory, &mut pile)
            .unwrap_or_else(|error| panic!("taking all loot failed: {error}"));
        self.finish_loot_take(world, actor_id, pile);
    }

    fn finish_loot_take(
        &mut self,
        world: &mut World,
        actor_id: crate::combat::ActorId,
        pile: crate::loot::GroundPile,
    ) {
        if pile.empty() {
            self.corpses.finish_loot(world, actor_id);
            self.close_loot();
        } else {
            self.corpses.override_reconciled_pile(pile);
        }
    }

    /// Playtester: force a visible family so sparkle is not coin-only.
    pub fn open_first_loot(&mut self) -> bool {
        let Some(actor_id) = self.corpses.first_actor_id() else {
            return false;
        };
        self.corpses
            .open(actor_id)
            .expect("selected known corpse pile");
        true
    }

    pub fn force_visible_loot(&mut self, actor_id: crate::combat::ActorId) {
        let hostile = self
            .combat
            .hostiles()
            .iter()
            .find(|hostile| hostile.actor_id() == actor_id)
            .unwrap_or_else(|| panic!("force-visible loot requested missing actor {actor_id:?}"));
        assert!(
            !self.combat.hostile_is_alive(actor_id),
            "force-visible loot requires dead actor {actor_id:?}"
        );
        assert!(
            self.corpses.contains(actor_id),
            "force-visible loot requires reconciled corpse {actor_id:?}"
        );
        let site = self.loot_site_for(hostile.x, hostile.z);
        let pile = crate::loot::force_visible_pile(&hostile.mob_id, actor_id, site);
        self.corpses.override_reconciled_pile(pile);
    }

    /// Dead-cone on the same left click. Not Tab. Sparkle must still be up.
    pub fn try_dead_loot(
        &mut self,
        player_x: f64,
        player_z: f64,
        facing_x: f64,
        facing_z: f64,
    ) -> bool {
        let pairs: Vec<(i32, f64, f64)> = self
            .combat
            .hostiles()
            .iter()
            .filter(|h| {
                !self.combat.hostile_is_alive(h.actor_id())
                    && self.corpses.contains(h.actor_id())
                    && self.corpses.has_marker(h.actor_id())
                    && !self.corpses.is_looted(h.actor_id())
            })
            .map(|h| (h.idx, h.x, h.z))
            .collect();
        let ids = crate::combat::tab_candidates(player_x, player_z, facing_x, facing_z, &pairs);
        let Some(idx) = ids.first().copied() else {
            return false;
        };
        let actor_id = self
            .combat
            .hostiles()
            .iter()
            .find(|h| h.idx == idx)
            .expect("loot candidate actor")
            .actor_id();
        self.corpses
            .open(actor_id)
            .expect("selected known corpse pile");
        true
    }

    fn loot_site_for(&self, x: f64, z: f64) -> Option<crate::loot::LootSite> {
        let site = self.overland_sites.iter().min_by(|a, b| {
            let da = (a.at.x - x).hypot(a.at.z - z);
            let db = (b.at.x - x).hypot(b.at.z - z);
            da.total_cmp(&db)
        })?;
        let d = (site.at.x - x).hypot(site.at.z - z);
        if d > 48.0 {
            return None;
        }
        Some(match site.kind {
            super::sites::SiteKind::TakenCairn => crate::loot::LootSite::Cairn,
            super::sites::SiteKind::WoodsHut => crate::loot::LootSite::Hut,
        })
    }

    fn reconcile_corpses(&mut self, world: &mut World) -> Result<(), SessionError> {
        let mut records = Vec::new();
        for hostile in self.combat.hostiles() {
            let actor_id = hostile.actor_id();
            if self.combat.hostile_is_alive(actor_id)
                || self.corpses.is_looted(actor_id)
                || self.corpses.contains(actor_id)
            {
                continue;
            }
            let presentation = self.combat.presentations().get(actor_id);
            let Some(entity) = dead_loot_presentation(
                actor_id,
                presentation.map_or(
                    crate::combat::HostilePresentationSource::Headless,
                    |binding| binding.source(),
                ),
                presentation.map(|binding| binding.entity()),
            )?
            else {
                continue;
            };
            let ground = self
                .contact_height(GlobalXZ::at(hostile.x, hostile.z))
                .ok_or(SessionError::MissingContact {
                    x: hostile.x,
                    z: hostile.z,
                })?;
            records.push(super::corpse::DeadActorRecord::new(
                actor_id,
                hostile.mob_id.clone(),
                GlobalPosition::at(hostile.x, f64::from(ground + FOOT_CLEARANCE_M), hostile.z),
                entity,
                self.loot_site_for(hostile.x, hostile.z),
            ));
        }
        for record in records {
            self.corpses.reconcile(world, record).map_err(|error| {
                SessionError::Engine(EngineError::Model(format!(
                    "corpse reconciliation failed: {error}"
                )))
            })?;
        }
        Ok(())
    }
    pub fn fixture_mesh_visible(&self, world: &World) -> bool {
        self.combat_layer.mesh_visible(world)
    }

    /// Harness-side seating check that does not require borrowing the render world.
    pub fn fixture_mesh_visible_from_session(&self) -> bool {
        !self.combat.hostiles().is_empty()
            && self.combat.hostiles().iter().all(|hostile| {
                self.combat
                    .presentations()
                    .get(hostile.actor_id())
                    .map(|b| b.entity())
                    .is_some()
            })
    }

    pub fn fixture_encounter_held(&self) -> bool {
        self.encounter.is_held()
    }

    /// Start a presentation fixture only after its harness has verified readiness.
    pub fn start_fixture_encounter(&mut self) {
        self.encounter.activate(&mut self.combat);
    }

    pub fn player_hp(&self) -> f64 {
        self.combat.player_hp()
    }

    pub fn player_hp_max(&self) -> f64 {
        self.combat.player_hp_max()
    }

    pub fn player_mana(&self) -> f64 {
        self.combat.player_mana()
    }

    pub fn player_mana_max(&self) -> f64 {
        self.combat.player_mana_max()
    }

    /// Player HP/mana bars are always drawn in world_hud.
    pub fn player_hp_visible(&self) -> bool {
        true
    }

    pub fn attack_pip(&self) -> bool {
        self.combat_layer.attack_pip()
    }

    pub fn hurt_flash(&self) -> bool {
        self.combat_layer.hurt_flash()
    }

    pub fn hp_ghost_frac(&self) -> Option<f32> {
        self.combat_layer.hp_ghost_frac()
    }

    pub fn slain_line(&self) -> Option<String> {
        self.combat
            .slain_by()
            .as_ref()
            .map(|n| format!("slain by {n}"))
    }

    pub fn swings_stopped(&self) -> bool {
        self.combat.is_dead()
    }

    pub fn is_shaken(&self) -> bool {
        self.combat
            .player()
            .shaken
            .as_ref()
            .is_some_and(|s| s.remaining_s > 0.0)
    }

    pub fn combat_log(&self) -> Vec<String> {
        self.combat.log_lines()
    }

    pub fn fail_tell(&self) -> Option<&'static str> {
        self.combat.fail_tell()
    }

    pub fn take_combat_presentation_events(
        &mut self,
    ) -> Vec<super::combat_layer::CombatPresentationEvent> {
        self.combat_layer.take_presentation_events()
    }

    pub fn take_combat_sfx(&mut self) -> Vec<super::combat_layer::CombatSfx> {
        self.combat_layer.take_combat_sfx()
    }

    pub fn lock_ring_visible(&self, world: &World) -> bool {
        self.combat_layer.lock_ring_visible(world)
    }

    fn resolve_death(&mut self, world: &mut World) {
        let place = self.last_shrine().or_else(|| {
            self.spawn
                .map(|s| GlobalPlace::at(s.position()).with_yaw_deg(s.heading().degrees()))
        });
        if let Some(place) = place {
            if let Some(player) = self.player.as_mut() {
                player.position = place.position;
                player.yaw_degrees = place.yaw_degrees;
                player.pitch_degrees = -15.0;
            }
        }
        self.combat.finish_death_respawn();
        self.combat.clear_hostiles();
        self.corpses.clear(world);
        self.combat_layer.despawn_meshes(world, &mut self.combat);
    }

    pub fn last_shrine(&self) -> Option<GlobalPlace> {
        self.last_shrine
            .or_else(|| self.dungeons.as_ref().and_then(|d| d.shrine()))
    }

    pub fn combat_walk_speed(&self) -> f32 {
        WALK_SPEED
    }

    /// Drop fixture meshes and props, clear transient death state, and return
    /// to the world spawn before seating the next fixture.
    fn prepare_planned_encounter(&mut self, world: &mut World) {
        assert!(
            self.encounter.transition() == super::encounter::EncounterTransition::Planned,
            "encounter preparation requires a planned transition"
        );
        for mob_id in self.encounter.mob_ids() {
            self.validate_encounter_mob(&mob_id);
        }
        self.begin_fixture_rearm(world);
        self.encounter.prepare();
    }

    fn apply_encounter_plan(&mut self, world: &mut World, plan: super::encounter::EncounterPlan) {
        self.encounter.plan(plan);
        self.prepare_planned_encounter(world);
    }

    fn begin_fixture_rearm(&mut self, world: &mut World) {
        self.combat_layer.despawn_meshes(world, &mut self.combat);
        if let Some(fauna) = self.fauna.as_mut() {
            fauna
                .clear(world, &mut self.combat)
                .unwrap_or_else(|err| panic!("fixture rearm failed to clear fauna: {err}"));
        }
        self.corpses.clear(world);
        super::sites::despawn_site_props(world, &mut self.site_prop_ids);
        super::sites::clear_overland_sites(&mut self.combat);
        self.overland_sites.clear();
        self.roster_pins_seated = false;
        self.combat.player_mut().shaken = None;
        self.encounter.set_skip_roster_pins(true);
        self.dungeon_skulls_for = None;
        if let Some(spawn) = self.spawn {
            if let Some(player) = self.player.as_mut() {
                player.position = spawn.position();
                player.yaw_degrees = spawn.heading().degrees();
                player.pitch_degrees = -12.0;
                player.vy = 0.0;
                player.airborne = false;
            }
        }
    }

    /// Leave fixture-only mode and reseat Taken Cairn / Woods Hut on the next tick.
    pub fn restore_overland_sites(&mut self, world: &mut World) {
        self.combat_layer.despawn_meshes(world, &mut self.combat);
        self.corpses.clear(world);
        super::sites::despawn_site_props(world, &mut self.site_prop_ids);
        self.combat.clear_hostiles();
        self.combat.set_lock(None);
        self.overland_sites.clear();
        self.roster_pins_seated = false;
        self.encounter.set_skip_roster_pins(false);
        self.encounter
            .plan(super::encounter::EncounterPlan::NormalWorld);
        self.encounter.prepare();
        self.combat_layer.reset_presentation_state();
        self.dungeon_skulls_for = None;
    }

    /// Reseats the L1 wolf line on the next world tick. Meshes despawn now.
    pub fn rearm_combat_fixtures(&mut self, world: &mut World) {
        self.apply_encounter_plan(world, super::encounter::EncounterPlan::WolfLine);
    }

    pub fn rearm_held_mob_fixture(
        &mut self,
        world: &mut World,
        mobs: Vec<super::encounter::HeldMobFixture>,
    ) {
        self.apply_encounter_plan(world, super::encounter::EncounterPlan::Held(mobs));
    }

    pub fn player_progression(&self) -> &crate::progression::ActorProgression {
        self.combat.player_progression()
    }

    pub fn ground_loot_count(&self) -> usize {
        self.corpses.count()
    }

    pub fn hostile_death_posed(&self, idx: i32) -> bool {
        self.combat
            .hostiles()
            .iter()
            .find(|hostile| hostile.idx == idx)
            .and_then(|hostile| {
                self.combat
                    .presentations()
                    .get(hostile.actor_id())
                    .map(|b| b.entity())
            })
            .is_some_and(|entity| self.combat_layer.is_death_posed(entity))
    }

    /// Reseats one published orc on the next world tick. Meshes despawn now.
    pub fn rearm_orc_fixture(&mut self, world: &mut World) {
        self.apply_encounter_plan(world, super::encounter::EncounterPlan::Orc);
    }

    /// Reseats one published yeti on the next world tick. Meshes despawn now.
    pub fn rearm_yeti_fixture(&mut self, world: &mut World) {
        self.apply_encounter_plan(world, super::encounter::EncounterPlan::Yeti);
    }

    /// Reseats one published demon on the next world tick. Meshes despawn now.
    pub fn rearm_demon_fixture(&mut self, world: &mut World) {
        self.apply_encounter_plan(world, super::encounter::EncounterPlan::Demon);
    }

    /// Reseats one published blue_demon on the next world tick. Meshes despawn now.
    pub fn rearm_bluedemon_fixture(&mut self, world: &mut World) {
        self.apply_encounter_plan(world, super::encounter::EncounterPlan::BlueDemon);
    }

    /// Reseats one published tribal_veteran on the next world tick. Meshes despawn now.
    pub fn rearm_tribal_veteran_fixture(&mut self, world: &mut World) {
        self.apply_encounter_plan(world, super::encounter::EncounterPlan::TribalVeteran);
    }

    pub fn rearm_bones_fixture(&mut self, world: &mut World) {
        self.apply_encounter_plan(world, super::encounter::EncounterPlan::Bones);
        self.combat.set_lock(None);
    }

    pub fn rearm_mage_fixture(&mut self, world: &mut World) {
        self.apply_encounter_plan(world, super::encounter::EncounterPlan::Mage);
        self.combat.set_lock(None);
    }

    pub fn key_binds(&self) -> &KeyBinds {
        &self.key_binds
    }

    pub fn set_key_binds(&mut self, binds: KeyBinds) {
        self.key_binds = binds;
    }

    pub fn apply_save(
        &mut self,
        stand: &crate::save::SavedStand,
    ) -> Result<(), crate::combat::PlayerSaveError> {
        self.combat.restore_player_save(&stand.player)?;
        self.last_shrine = stand.last_shrine.map(crate::save::SavedShrine::to_place);
        self.inventory = stand.inventory;
        Ok(())
    }

    pub fn saved_full(
        &self,
        seed: i32,
        size: usize,
    ) -> Result<Option<crate::save::SavedStand>, SessionError> {
        let Some((at, heading)) = self.saved_stand()? else {
            return Ok(None);
        };
        let player = self.combat.export_player_save()?;
        Ok(Some(crate::save::SavedStand::new(
            seed,
            size,
            at,
            heading,
            player,
            self.last_shrine().map(crate::save::SavedShrine::from_place),
            self.inventory,
        )))
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
        let mut plots = self
            .settlements
            .as_ref()
            .map(|s| s.plots().to_vec())
            .unwrap_or_default();
        if let Some(dungeons) = &self.dungeons {
            plots.extend(dungeons.plots());
        }
        plots.extend(self.caves.plots());
        Arc::new(BuildingIndex::new(plots))
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
        let core = self.loading_core();
        match self.dungeon_build_status() {
            Some(dungeon) => format!("{dungeon}  ·  {core}"),
            None => core,
        }
    }

    fn loading_core(&self) -> String {
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
        let roster = self.combat.player_action_roster();
        let mut input =
            WalkInput::from_frame(frame, yaw, pitch, mode, looking, &self.key_binds, roster);
        if world.bind_listen() {
            input.actions = crate::controls::PressedActions::NONE;
        }
        self.step(world, input)
    }

    /// Advance the session with explicit intent (also the headless path).
    pub fn step(&mut self, world: &mut World, input: WalkInput) -> Result<(), SessionError> {
        // Whatever anyone asked for, the map and the loading screen need a
        // cursor the player can use; in the world they have to ask for it.
        if self.state == SessionState::World {
            if input.bag {
                self.bag_open = !self.bag_open;
                if self.bag_open {
                    world.set_pointer_lock(false);
                }
            }
            let ui_open =
                self.bag_open || self.corpses.is_open() || self.skill_open || self.summon_open;
            if ui_open {
                world.set_pointer_lock(false);
            } else if input.capture_look {
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
        if self.travel.as_ref().expect("travel").handed_off && self.update_loading(world)? {
            self.travel.as_mut().expect("travel").destination_ready = true;
        }

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
            fauna.clear(world, &mut self.combat)?;
        }
        if let Some(villagers) = self.villagers.as_mut() {
            villagers.clear(world)?;
        }
        if let Some(dungeons) = self.dungeons.as_mut() {
            dungeons.clear(world)?;
        }

        self.combat_layer.reset_vfx(world)?;
        self.stream.reset(world);
        world.set_render_origin(RenderOrigin::snapped(approach.ground(), CHUNK_SPAN_M)?)?;
        self.spawn = None;
        self.player = None;
        self.entering = Some(request);
        self.overworld = None;
        // Travel destroys the rendered world and starts a new encounter context.
        // Do not retain the old fixture latch or hostile roster: that would skip
        // destination seating and leave the new bodies without combat anchors.
        self.site_prop_ids.clear();
        self.combat_layer.forget_meshes();
        self.encounter.set_skip_roster_pins(false);
        self.combat_layer.reset_presentation_state();
        self.combat.clear_hostiles();
        self.combat.set_lock(None);
        self.overland_sites.clear();
        self.roster_pins_seated = false;
        self.dungeon_skulls_for = None;
        self.corpses.close();
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
        if let Some(player) = self.player {
            self.remember_overworld_from(world, player.position.horizontal(), player.yaw_degrees)?;
        }
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
        world.in_space(prev)?;
        self.proxy = Some(InstalledProxy { land, sea });
        Ok(())
    }

    fn assert_proxy_resident(&self, world: &World) {
        let Some(proxy) = &self.proxy else {
            panic!("travel started without an uploaded continent proxy");
        };
        world.entity(proxy.land).expect("continent proxy land mesh");
        world.entity(proxy.sea).expect("continent proxy sea mesh");
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
        world.set_camera_lens(view.fov_y_degrees, view.near_m);
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
                self.ponds.traced(request.requested())?;
                let catalog = ScatterCatalog::discover()?;
                self.scatter = Some(ScatterLayer::install(
                    world,
                    &catalog,
                    self.surface.world_seed(),
                )?);
                self.settlements =
                    Some(SettlementLayer::install(world, self.surface.world_seed())?);
                self.paths = Some(PathLayer::new());
                self.fauna = Some(FaunaLayer::install(
                    self.surface.world_seed(),
                    &self.game_data,
                )?);
                self.villagers = Some(VillagerLayer::new());
                self.dungeons = Some(DungeonLayer::install());
                self.caves = super::cave::CaveLayer::install();
            }
            if let Some(dungeons) = self.dungeons.as_mut() {
                let rebuilt =
                    dungeons.follow(world, &self.stream, &self.surface, request.requested())?;
                if rebuilt {
                    let plots = self.plot_index();
                    self.stream.set_house_plots(world, (*plots).clone())?;
                }
            }
            if self
                .caves
                .follow(world, &self.surface, request.requested())?
            {
                let plots = self.plot_index();
                self.stream.set_house_plots(world, (*plots).clone())?;
            }
            if !self.ponds.traced(request.requested())? {
                return Ok(false);
            }
            let pose = resolve_spawn(&self.surface, &self.ponds.field(), request)?;
            self.spawn = Some(pose);
            if self.last_shrine.is_none() {
                self.last_shrine =
                    Some(GlobalPlace::at(pose.position()).with_yaw_deg(pose.heading().degrees()));
            }
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
        if let Some(dungeons) = self.dungeons.as_mut() {
            let rebuilt = dungeons.follow(world, &self.stream, &self.surface, focus)?;
            if rebuilt {
                let plots = self.plot_index();
                self.stream.set_house_plots(world, (*plots).clone())?;
            }
        }
        if self.caves.follow(world, &self.surface, focus)? {
            let plots = self.plot_index();
            self.stream.set_house_plots(world, (*plots).clone())?;
        }
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
            let hamlets = self
                .settlements
                .as_ref()
                .map(SettlementLayer::hamlets)
                .unwrap_or(&[]);
            scatter.follow(
                world,
                &self.stream,
                &self.surface,
                &self.ponds.field(),
                focus,
                &plots,
                hamlets,
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
                &mut self.combat,
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
        if let Some(villagers) = self.villagers.as_mut() {
            let hamlets = self
                .settlements
                .as_ref()
                .map(SettlementLayer::hamlets)
                .unwrap_or(&[]);
            let doors = self
                .settlements
                .as_ref()
                .map(SettlementLayer::doors)
                .unwrap_or(&[]);
            villagers.follow(
                world,
                &self.stream,
                &self.surface,
                hamlets,
                doors,
                plots.plots(),
                0.0,
            )?;
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

    fn ground_authority(&self, world: &World) -> Result<GroundAuthority, SessionError> {
        if world.living_in() == SpaceId::DEFAULT {
            return Ok(GroundAuthority::Overland {
                contact: self.stream.contact_snapshot(),
                surface: Arc::clone(&self.surface),
                ponds: self.ponds.field(),
                off_window_ponds: Arc::clone(&self.combat_ground_ponds),
            });
        }

        let player = self.player.ok_or(SessionError::NoWorld)?;
        if let Some(floor_y) = self
            .dungeons
            .as_ref()
            .and_then(|dungeon| dungeon.indoor_floor_y(world, player.position))
            .or_else(|| self.doors.indoor_floor_y(world, player.position))
        {
            return Ok(GroundAuthority::Indoor { floor_y });
        }

        Err(SessionError::Engine(EngineError::InvalidValue(format!(
            "combat ground requested in space {:?}, which has no authoritative height surface",
            world.living_in(),
        ))))
    }

    fn hostile_feet_y(
        &self,
        world: &World,
        h: &crate::combat::WorldHostile,
    ) -> Result<f64, SessionError> {
        self.ground_authority(world)?
            .feet_y(GlobalXZ::at(h.x, h.z))
            .map_err(Into::into)
    }

    fn respawn_hostile_meshes(
        &mut self,
        world: &mut World,
        player: &Player,
    ) -> Result<(), SessionError> {
        let feet: Vec<f64> = self
            .combat
            .hostiles()
            .iter()
            .map(|h| self.hostile_feet_y(world, h))
            .collect::<Result<Vec<_>, _>>()?;
        self.combat_layer
            .spawn_wolf_meshes(world, &mut self.combat, &feet, player.yaw_degrees)
            .map_err(Into::into)
    }

    fn sync_dungeon_skulls(
        &mut self,
        world: &mut World,
        player: &Player,
    ) -> Result<(), SessionError> {
        let live_id = self.dungeons.as_ref().and_then(DungeonLayer::live_pin_id);
        let in_space = self
            .dungeons
            .as_ref()
            .and_then(|d| d.indoor_floor_y(world, player.position))
            .is_some();
        if live_id.is_none() {
            if self.dungeon_skulls_for.take().is_some() {
                super::encounter::clear_dungeon_skulls(&mut self.combat);
                super::encounter::clear_dungeon_bones(&mut self.combat);
                if self.combat_layer.presentation_ready() {
                    self.respawn_hostile_meshes(world, player)?;
                }
            }
            return Ok(());
        }
        let id = live_id.expect("live dungeon");
        if !in_space || self.dungeon_skulls_for == Some(id) {
            return Ok(());
        }
        if self.dungeon_skulls_for.is_some() {
            super::encounter::clear_dungeon_skulls(&mut self.combat);
            super::encounter::clear_dungeon_bones(&mut self.combat);
        }
        let spots = self
            .dungeons
            .as_ref()
            .map(DungeonLayer::live_skulls)
            .unwrap_or_default();
        let heart = self.dungeons.as_ref().and_then(DungeonLayer::live_heart);
        if !spots.is_empty() {
            super::encounter::seat_dungeon_skulls(&mut self.combat, &spots);
            super::encounter::seat_dungeon_bones(&mut self.combat, &spots, heart);
            if self.combat_layer.presentation_ready() {
                self.respawn_hostile_meshes(world, player)?;
            }
        }
        self.dungeon_skulls_for = Some(id);
        Ok(())
    }

    fn ensure_overland_sites(
        &mut self,
        world: &mut World,
        player_pos: engine::space::GlobalPosition,
    ) -> Result<bool, SessionError> {
        if self.encounter.skip_roster_pins() {
            return Ok(false);
        }
        let overland = self
            .dungeons
            .as_ref()
            .and_then(|d| d.indoor_floor_y(world, player_pos))
            .is_none();
        if !overland {
            return Ok(false);
        }
        if !self.roster_pins_seated {
            let pins = self.surface.settlements();
            let hamlets = self
                .settlements
                .as_ref()
                .map(SettlementLayer::hamlets)
                .unwrap_or(&[]);
            let sites = super::sites::plan_overland_sites(&self.surface, pins, hamlets);
            super::sites::clear_overland_sites(&mut self.combat);
            super::sites::seat_overland_sites(&mut self.combat, &sites);
            super::sites::despawn_site_props(world, &mut self.site_prop_ids);
            self.site_prop_ids = super::sites::spawn_site_props(world, &self.surface, &sites)?;
            self.overland_sites = sites;
            self.roster_pins_seated = true;
            return Ok(true);
        }
        let want = self.overland_sites.len() * 2;
        let have = self
            .combat
            .hostiles()
            .iter()
            .filter(|h| super::sites::is_bandit_id(&h.mob_id))
            .count();
        if have < want {
            super::sites::clear_overland_sites(&mut self.combat);
            super::sites::seat_overland_sites(&mut self.combat, &self.overland_sites);
        }
        if self.site_prop_ids.is_empty() && !self.overland_sites.is_empty() {
            self.site_prop_ids =
                super::sites::spawn_site_props(world, &self.surface, &self.overland_sites)?;
            return Ok(true);
        }
        Ok(have < want)
    }

    fn seat_planned_encounter(&mut self, world: &mut World) -> Result<(), SessionError> {
        if self.encounter.transition() == super::encounter::EncounterTransition::Planned {
            self.prepare_planned_encounter(world);
        }
        if self.encounter.transition() != super::encounter::EncounterTransition::Prepared {
            return Ok(());
        }
        let player = self.player.ok_or(SessionError::NoWorld)?;
        let facing = Camera::facing_xz(player.yaw_degrees);
        self.encounter.seat(
            &mut self.combat,
            player.position.x,
            player.position.z,
            f64::from(facing.x),
            f64::from(facing.z),
        );
        if let Err(error) = self.respawn_hostile_meshes(world, &player) {
            self.combat_layer.despawn_meshes(world, &mut self.combat);
            self.combat_layer.reset_presentation_state();
            return Err(error);
        }
        self.combat_layer.mark_presentation_ready();
        Ok(())
    }

    fn update_world(&mut self, world: &mut World, input: WalkInput) -> Result<(), SessionError> {
        self.advance_level_up_notices(input.dt);
        self.seat_planned_encounter(world)?;
        let mut player = self.player.ok_or(SessionError::NoWorld)?;

        if input.toggle_fly {
            player.mode = player.mode.toggled();
            player.vy = 0.0;
            player.airborne = false;
        }
        // Phase 1: providers update population and register canonical actors before simulation.
        let pre_sim_foot = player.position.horizontal();
        let pre_sim_plots = self.plot_index();
        let pre_sim_hamlets = self
            .settlements
            .as_ref()
            .map_or(&[][..], SettlementLayer::hamlets);
        if let Some(fauna) = self.fauna.as_mut() {
            fauna.follow(
                world,
                &self.stream,
                &self.surface,
                &self.ponds.field(),
                &pre_sim_plots,
                pre_sim_hamlets,
                pre_sim_foot,
                pre_sim_foot,
                input.dt,
                &mut self.combat,
            )?;
        }
        // Phase 2: authoritative fixed-step simulation; phase 3 projects every provider pose below.
        if !self.combat_layer.presentation_ready() {
            if self.encounter.transition() == super::encounter::EncounterTransition::Planned {
                self.seat_planned_encounter(world)?;
            } else if self.encounter.is_normal_world() {
                self.ensure_overland_sites(world, player.position)?;
                self.respawn_hostile_meshes(world, &player)?;
                self.combat_layer.mark_presentation_ready();
            }
        }
        if self.combat_layer.presentation_ready()
            && self.encounter.is_normal_world()
            && !self.encounter.skip_roster_pins()
        {
            if self.ensure_overland_sites(world, player.position)? {
                self.respawn_hostile_meshes(world, &player)?;
            }
        }
        if input.tab || (input.capture_look && world.pointer_lock()) {
            let facing = Camera::facing_xz(player.yaw_degrees);
            let px = player.position.x;
            let pz = player.position.z;
            if input.tab {
                self.combat
                    .press_tab(px, pz, facing.x as f64, facing.z as f64);
            } else if input.capture_look
                && world.pointer_lock()
                && !self.bag_open
                && !self.corpses.is_open()
            {
                // Dead-cone is not Tab. Same left click; sparkle still up.
                if self.try_dead_loot(px, pz, facing.x as f64, facing.z as f64) {
                    world.set_pointer_lock(false);
                } else {
                    self.combat
                        .click_lock(px, pz, facing.x as f64, facing.z as f64);
                }
            }
        }
        for action_id in input.actions.iter() {
            let facing = Camera::facing_xz(player.yaw_degrees);
            self.combat.press_action(
                action_id,
                player.position.x,
                player.position.z,
                facing.x as f64,
                facing.z as f64,
            );
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
                let in_dungeon = self
                    .dungeons
                    .as_ref()
                    .and_then(|d| d.indoor_floor_y(world, player.position))
                    .is_some();
                if self.caves.living_in_cave(world) || in_dungeon {
                    move_dungeon_walker(world, &mut player, dx, dz, input.jump, input.dt);
                } else {
                    let to = world.move_actor(&player.body, player.position, dx, dz);
                    player.position.x = to.x;
                    player.position.z = to.z;
                }
            }
            Locomotion::Fly => {
                player.position.x += dx;
                player.position.z += dz;
            }
        }

        {
            let facing = Camera::facing_xz(player.yaw_degrees);
            self.combat_layer.tick(
                &mut self.combat,
                player.position.x,
                player.position.z,
                facing.x as f64,
                facing.z as f64,
                f64::from(input.dt),
            );
            let level_ups = self.combat_layer.take_player_level_ups();
            self.queue_level_ups(level_ups);
            let fauna_cues = self.combat_layer.take_fauna_animation_cues();
            if fauna_cues.is_empty() {
                // No fauna presentation work this frame.
            } else {
                let fauna = self.fauna.as_mut().ok_or_else(|| {
                    SessionError::Fauna(super::fauna::FaunaError::Catalog(
                        "fauna animation cues exist without an installed FaunaLayer".into(),
                    ))
                })?;
                fauna.present_combat_cues(world, &self.combat, fauna_cues)?;
            }
            let mut transform_ground = self.ground_authority(world)?;
            self.combat_layer
                .sync_hostile_transforms(world, &self.combat, move |x, z| {
                    transform_ground.feet_y(GlobalXZ::at(x, z))
                })?;
            let mut presentation_ground = self.ground_authority(world)?;
            self.combat_layer.present(
                world,
                &self.combat,
                player.position,
                move |x, z| presentation_ground.feet_y(GlobalXZ::at(x, z)),
                input.dt,
            )?;
            self.reconcile_corpses(world)?;
            if self.combat.is_dead()
                && self.combat.player_hp() <= 0.0
                && self.combat.slain_hold_s() <= 0.0
            {
                self.resolve_death(world);
            }
            if let Some(d) = self.dungeons.as_ref() {
                if let Some(place) = d.shrine() {
                    self.last_shrine = Some(place);
                }
            }
        }
        let foot = player.position.horizontal();
        // Before the streamer, so a chunk is never baked against a window that
        // has stopped reaching it.
        let t = Instant::now();
        self.ponds.follow(foot)?;
        world.hitch_span("ponds", hitch_ms(t), String::new());

        let t = Instant::now();
        let rebased = self.stream.maybe_rebase(world, foot)?;
        if rebased {
            self.combat_layer.reset_vfx(world)?;
        }
        if rebased && self.combat_layer.presentation_ready() {
            if let Some(player) = self.player {
                self.respawn_hostile_meshes(world, &player)?;
            }
        }
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
        let t_dungeon = Instant::now();
        let dungeon_rebuilt = if let Some(dungeons) = self.dungeons.as_mut() {
            dungeons.follow(world, &self.stream, &self.surface, foot)?
        } else {
            false
        };
        world.hitch_span(
            "dungeons",
            hitch_ms(t_dungeon),
            format!(
                "generating={} seated={} ready={}",
                self.dungeon_generating(),
                self.dungeon_seated_count(),
                self.dungeon_ready_count(),
            ),
        );
        if rebuilt || dungeon_rebuilt {
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
            let hamlets = self
                .settlements
                .as_ref()
                .map_or(&[][..], SettlementLayer::hamlets);
            scatter.follow(
                world,
                &self.stream,
                &self.surface,
                &self.ponds.field(),
                foot,
                &plots,
                hamlets,
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
        if let Some(villagers) = self.villagers.as_mut() {
            let doors = self
                .settlements
                .as_ref()
                .map(SettlementLayer::doors)
                .unwrap_or(&[]);
            villagers.follow(
                world,
                &self.stream,
                &self.surface,
                hamlets,
                doors,
                plots.plots(),
                input.dt,
            )?;
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
        if let Some(dungeons) = self.dungeons.as_mut() {
            let t = Instant::now();
            dungeons.frame(world, player.position, player.yaw_degrees)?;
            world.hitch_span(
                "dungeon_frame",
                hitch_ms(t),
                format!(
                    "generating={} ready={}",
                    dungeons.generating(),
                    dungeons.ready_count()
                ),
            );
        }
        {
            let t = Instant::now();
            self.caves.follow(
                world,
                &self.surface,
                GlobalXZ::at(player.position.x, player.position.z),
            )?;
            self.caves
                .frame(world, player.position, player.yaw_degrees)?;
            world.hitch_span(
                "cave_frame",
                hitch_ms(t),
                format!("generating={}", self.caves.generating()),
            );
        }
        self.sync_dungeon_skulls(world, &player)?;
        let hidden_leaf = self.doors.hidden_leaf();
        if let Some(settlements) = self.settlements.as_mut() {
            settlements.hide_leaf(world, hidden_leaf)?;
        }

        // Doors need the actor centre. The outdoor hatch needs the soles for
        // falling through; the dungeon ceiling hatch needs the actor's head.
        self.remember_overworld_from(world, player.position.horizontal(), player.yaw_degrees)?;
        let near_hatch = self
            .dungeons
            .as_ref()
            .is_some_and(|d| d.near_hatch(player.position));
        let hatch_armed = self
            .dungeons
            .as_ref()
            .is_some_and(DungeonLayer::hatch_armed);
        let in_dungeon = self
            .dungeons
            .as_ref()
            .and_then(|d| d.indoor_floor_y(world, player.position))
            .is_some();
        let portal_probe_y = if near_hatch && hatch_armed {
            0.12
        } else if in_dungeon {
            f64::from(player.body.height)
        } else {
            f64::from(player.body.height * 0.5)
        };
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
            if self.doors.hidden_leaf().is_some() {
                self.doors
                    .settle_after_travel(world, &player.body, entered, &mut player.position);
            } else if let Some(dungeons) = &self.dungeons {
                dungeons.settle_after_travel(
                    world,
                    &player.body,
                    entered,
                    &mut player.position,
                    &mut player.yaw_degrees,
                );
            }
            self.caves.settle_after_travel(
                world,
                &player.body,
                entered,
                &mut player.position,
                &mut player.yaw_degrees,
            );
        }

        match player.mode {
            // Only the resident bake may move the player vertically: falling
            // back to a fresh surface query here would put the feet on a
            // different surface than the one being drawn. Indoors the house
            // floor is the contact, not the outdoor plot cap.
            Locomotion::Walk => {
                let in_dungeon = self
                    .dungeons
                    .as_ref()
                    .and_then(|d| d.indoor_floor_y(world, player.position))
                    .is_some();
                if !in_dungeon && !self.caves.living_in_cave(world) {
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
            }
            Locomotion::Fly => {}
        }

        // The lantern is always lit; outdoors the sun drowns it, in a cave it
        // is what lets you see. Swap to a headlamp's cooler cast underground.
        let torch = if self.caves.living_in_cave(world) {
            engine::world::TorchLight::headlamp()
        } else {
            engine::world::TorchLight::lantern()
        };
        world.set_torch(Some(torch));

        world.look_first_person_global(player.eye(), player.yaw_degrees, player.pitch_degrees)?;
        let mut head_positions = std::collections::BTreeMap::new();
        for hostile in self.combat.hostiles() {
            let feet = self.hostile_feet_y(world, hostile)?;
            assert!(
                head_positions
                    .insert(hostile.actor_id(), (hostile.x, feet + 1.55, hostile.z))
                    .is_none(),
                "duplicate HUD head anchor"
            );
        }
        self.combat_hud = self
            .combat
            .hud_snapshot()
            .with_head_positions(&head_positions)
            .with_presentation(
                self.combat_layer.attack_pip(),
                self.combat_layer.hurt_flash(),
                self.combat_layer.hp_ghost_frac(),
            );

        self.player = Some(player);
        Ok(())
    }

    /// Ground height reported by the resident mesh under a global point.
    pub fn contact_height(&self, p: GlobalXZ) -> Option<f32> {
        self.stream.contact_height(p)
    }

    /// Stand to write on exit: the last default-space feet, never a dungeon interior.
    pub fn saved_stand(&self) -> Result<Option<(GlobalXZ, Heading)>, CoordError> {
        if let Some(stand) = self.overworld {
            return Ok(Some(stand));
        }
        let Some(player) = self.player else {
            return Ok(None);
        };
        let heading = Heading::from_degrees(player.yaw_degrees)?;
        Ok(Some((player.position.horizontal(), heading)))
    }

    fn remember_overworld_from(
        &mut self,
        world: &World,
        at: GlobalXZ,
        yaw_degrees: f32,
    ) -> Result<(), CoordError> {
        if world.living_in() != SpaceId::DEFAULT {
            return Ok(());
        }
        let heading = Heading::from_degrees(yaw_degrees)?;
        self.overworld = Some((at, heading));
        Ok(())
    }

    /// Compass heading the player is facing.
    pub fn player_heading(&self) -> Result<Option<Heading>, CoordError> {
        self.player
            .map(|p| Heading::from_degrees(p.yaw_degrees))
            .transpose()
    }

    /// How the player is currently getting around.
    pub fn locomotion(&self) -> Option<Locomotion> {
        self.player.map(|p| p.mode)
    }

    /// True while the player is living in a generated cave interior.
    pub fn in_cave(&self, world: &World) -> bool {
        let Some(player) = self.player else {
            return false;
        };
        let _ = player;
        world.living_in() != SpaceId::DEFAULT && self.caves.has_live()
    }

    /// HUD line when a house door is in reach.
    pub fn door_hint(&self) -> Option<&str> {
        self.doors
            .hint()
            .or_else(|| self.dungeons.as_ref().and_then(DungeonLayer::hint))
    }

    /// True while a house portal is live (leaf hidden / swung open).
    pub fn house_portal_live(&self) -> bool {
        self.doors.hidden_leaf().is_some()
    }

    /// True while the player stands inside a live house portal space.
    pub fn in_house(&self, world: &World) -> bool {
        let Some(player) = self.player else {
            return false;
        };
        self.doors.indoor_floor_y(world, player.position).is_some()
    }

    /// Indoor floor height when living in an open house; none outdoors.
    pub fn house_indoor_floor_y(&self, world: &World) -> Option<f32> {
        let player = self.player?;
        self.doors.indoor_floor_y(world, player.position)
    }

    /// Nearest seated house door to `from`, if any.
    pub fn nearest_village_door(&self, from: GlobalXZ) -> Option<&HouseDoor> {
        self.village_doors().iter().min_by(|a, b| {
            a.at.horizontal()
                .distance(from)
                .total_cmp(&b.at.horizontal().distance(from))
        })
    }

    /// True while a nearby dungeon layout is still on the worker.
    pub fn dungeon_generating(&self) -> bool {
        self.dungeons.as_ref().is_some_and(DungeonLayer::generating)
    }

    /// HUD line while a nearby dungeon is still being cut.
    pub fn dungeon_build_status(&self) -> Option<String> {
        self.dungeons.as_ref().and_then(DungeonLayer::build_status)
    }

    /// HUD line while a nearby cave chamber is still growing.
    pub fn cave_build_status(&self) -> Option<String> {
        self.caves.build_status()
    }

    /// HUD hint at a seated cave mouth.
    pub fn cave_hint(&self) -> Option<&str> {
        self.caves.hint()
    }

    pub fn dungeon_ready_count(&self) -> usize {
        self.dungeons.as_ref().map_or(0, DungeonLayer::ready_count)
    }

    pub fn dungeon_seated_count(&self) -> usize {
        self.dungeons.as_ref().map_or(0, DungeonLayer::seated_count)
    }

    pub fn dungeon_has_live(&self) -> bool {
        self.dungeons.as_ref().is_some_and(DungeonLayer::has_live)
    }

    pub fn hatch_armed(&self) -> bool {
        self.dungeons
            .as_ref()
            .is_some_and(DungeonLayer::hatch_armed)
    }

    pub fn near_hatch(&self) -> bool {
        match (self.dungeons.as_ref(), self.player) {
            (Some(dungeons), Some(player)) => dungeons.near_hatch(player.position),
            _ => false,
        }
    }

    pub fn dungeon_pin_seated(&self, id: i32) -> bool {
        self.dungeons.as_ref().is_some_and(|d| d.pin_seated(id))
    }

    pub fn dungeon_pin_ready(&self, id: i32) -> bool {
        self.dungeons.as_ref().is_some_and(|d| d.pin_ready(id))
    }

    pub fn dungeon_pin_failed(&self, id: i32) -> bool {
        self.dungeons.as_ref().is_some_and(|d| d.pin_failed(id))
    }

    pub fn dungeon_landing_yaw(&self) -> Option<f32> {
        self.dungeons.as_ref().and_then(DungeonLayer::landing_yaw)
    }

    pub fn in_dungeon(&self, world: &World) -> bool {
        self.dungeons
            .as_ref()
            .and_then(|d| {
                self.player
                    .and_then(|p| d.indoor_floor_y(world, p.position))
            })
            .is_some()
    }

    /// Player / SavedStand facing. 0 = +Z, growing toward +X.
    ///
    /// This is not the last move direction stored on the player.
    pub fn player_yaw_degrees(&self) -> Option<f32> {
        self.player.map(|p| p.yaw_degrees)
    }

    pub fn player_pitch_degrees(&self) -> Option<f32> {
        self.player.map(|p| p.pitch_degrees)
    }

    pub fn feet_on_ground(&self) -> bool {
        self.player
            .is_some_and(|p| p.mode == Locomotion::Walk && !p.airborne)
    }

    /// Headless and unit tests wait for streaming, not a short wall clock.
    pub fn wait_until_world(&mut self, world: &mut World) {
        self.wait_until_world_for(world, Duration::from_secs(90));
    }

    pub fn wait_until_world_for(&mut self, world: &mut World, budget: Duration) {
        let deadline = Instant::now() + budget;
        while Instant::now() < deadline {
            self.step(world, WalkInput::IDLE).expect("update");
            if self.state() == SessionState::World {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!(
            "the entry ring never became resident (spawn={} progress={:.0}% status={} resident={} pending={} inflight={})",
            self.spawn().is_some(),
            self.loading_progress() * 100.0,
            self.loading_status(),
            self.stream().resident_count(),
            self.stream().pending_count(),
            self.stream().inflight_count(),
        );
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

    /// Nearest seated tier-0 hamlet to the player (or the first seated one).
    pub fn nearest_tier0_hamlet(&self) -> Option<&HamletStand> {
        let focus = self
            .player
            .map(|p| p.position.horizontal())
            .or_else(|| self.spawn.map(|s| s.ground()))?;
        let mut best: Option<(&HamletStand, f64)> = None;
        for hamlet in self.hamlets() {
            if !self.pin_is_tier0(hamlet.at) {
                continue;
            }
            let d = hamlet.at.distance(focus);
            if best.map(|(_, bd)| d < bd).unwrap_or(true) {
                best = Some((hamlet, d));
            }
        }
        best.map(|(h, _)| h)
    }

    pub fn village_well(&self) -> Option<GlobalXZ> {
        self.nearest_tier0_hamlet().map(|h| h.at)
    }

    pub fn village_cut(&self) -> &[glam::Vec2] {
        self.nearest_tier0_hamlet()
            .map(|h| h.cut.as_slice())
            .unwrap_or(&[])
    }

    pub fn village_dwelling_count(&self) -> usize {
        self.nearest_tier0_hamlet()
            .map(|h| h.houses.len())
            .unwrap_or(0)
    }

    pub fn village_has_well(&self) -> bool {
        self.nearest_tier0_hamlet().is_some()
    }

    pub fn village_has_camp(&self) -> bool {
        let Some(hamlet) = self.nearest_tier0_hamlet() else {
            return false;
        };
        self.settlements
            .as_ref()
            .is_some_and(|s| s.hamlet_has_camp(hamlet.at))
    }

    pub fn village_camp_pair(&self) -> Option<(GlobalPosition, GlobalPosition)> {
        let hamlet = self.nearest_tier0_hamlet()?;
        self.settlements.as_ref()?.hamlet_camp(hamlet.at)
    }

    /// Stand at the well/plaza, three-quarter off the cut like village.png.
    /// Houses stay in frame; tent canvas + ring sit in the yard. Not a dirt crane.
    pub fn village_camp_stand(&self) -> Option<GlobalPosition> {
        let (tent, ring) = self.village_camp_pair()?;
        let hamlet = self.nearest_tier0_hamlet()?;
        let plaza = hamlet
            .cut
            .last()
            .map(|p| GlobalXZ::at(f64::from(p.x), f64::from(p.y)))
            .unwrap_or(hamlet.at);
        let mid_x = (tent.x + ring.x) * 0.5;
        let mid_z = (tent.z + ring.z) * 0.5;
        let (along_x, along_z) = if hamlet.cut.len() >= 2 {
            let a = hamlet.cut[0];
            let b = hamlet.cut[hamlet.cut.len() - 1];
            let tx = b.x - a.x;
            let tz = b.y - a.y;
            let len = (tx * tx + tz * tz).sqrt().max(1e-6);
            (tx / len, tz / len)
        } else {
            let tx = (mid_x - plaza.x) as f32;
            let tz = (mid_z - plaza.z) as f32;
            let len = (tx * tx + tz * tz).sqrt().max(1e-6);
            (tx / len, tz / len)
        };
        // Back along the street from the well, then 3.6 m off the cut (village three-quarter).
        let stand = GlobalXZ::at(
            plaza.x - f64::from(along_x * 3.2 + (-along_z) * 3.6),
            plaza.z - f64::from(along_z * 3.2 + along_x * 3.6),
        );
        let ground = self.surface.column(stand).ground();
        Some(GlobalPosition::at(
            stand.x,
            f64::from(ground + EYE_HEIGHT_M),
            stand.z,
        ))
    }

    pub fn village_camp_look(&self) -> Option<(Vec3, Vec3)> {
        let (tent, ring) = self.village_camp_pair()?;
        let pos = self.player_position()?;
        let eye = Vec3::new(pos.x as f32, pos.y as f32, pos.z as f32);
        let target = Vec3::new(
            ((tent.x + ring.x) * 0.5) as f32,
            ((tent.y + ring.y) * 0.5) as f32 + 1.35,
            ((tent.z + ring.z) * 0.5) as f32,
        );
        Some((eye, target))
    }

    pub fn village_human_count(&self) -> usize {
        self.villagers
            .as_ref()
            .map_or(0, VillagerLayer::human_count)
    }

    pub fn village_human_mesh_count(&self, world: &World) -> usize {
        self.villagers.as_ref().map_or(0, |v| v.mesh_count(world))
    }

    pub fn village_human_on_corridor(&self) -> bool {
        self.village_corridor_human().is_some()
    }

    pub fn village_corridor_human(&self) -> Option<engine::space::GlobalPosition> {
        let cut = self.village_cut();
        self.villagers.as_ref().and_then(|v| v.corridor_human(cut))
    }

    pub fn village_walk_mps() -> f32 {
        VillagerLayer::walk_mps()
    }

    pub fn village_walker_speed_mps(&self) -> Option<f32> {
        self.villagers
            .as_ref()
            .and_then(VillagerLayer::walker_speed_mps)
    }

    pub fn ribbon_faces(&self) -> usize {
        self.paths.as_ref().map_or(0, |p| p.ribbon_faces())
    }

    pub fn has_ribbon_mesh(&self) -> bool {
        self.paths.as_ref().is_some_and(|p| p.has_ribbon_mesh())
    }

    pub fn village_house_plots(&self) -> Vec<HousePlot> {
        let Some(hamlet) = self.nearest_tier0_hamlet() else {
            return Vec::new();
        };
        self.settlements
            .as_ref()
            .map(|s| {
                s.plots()
                    .iter()
                    .filter_map(|p| match p {
                        super::footprint::BuildingPlot::House(h) if hamlet.covers(h.at, 0.0) => {
                            Some(*h)
                        }
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn village_doors(&self) -> &[HouseDoor] {
        self.settlements
            .as_ref()
            .map(SettlementLayer::doors)
            .unwrap_or(&[])
    }

    fn pin_is_tier0(&self, at: GlobalXZ) -> bool {
        self.surface.settlements().iter().any(|pin| {
            pin.tier <= 1 && (pin.at.x - at.x).abs() < 0.75 && (pin.at.z - at.z).abs() < 0.75
        })
    }

    /// Closest hamlet/village pin (tier 0, or atlas leftover tier 1).
    pub fn overland_sites(&self) -> &[super::sites::OverlandSite] {
        &self.overland_sites
    }

    pub fn overland_site(
        &self,
        kind: super::sites::SiteKind,
    ) -> Option<super::sites::OverlandSite> {
        self.overland_sites.iter().copied().find(|s| s.kind == kind)
    }

    pub fn site_prop_count(&self) -> usize {
        self.site_prop_ids.len()
    }

    pub fn nearest_tier0_pin(&self, from: GlobalXZ) -> Option<super::surface::SettlementPin> {
        self.surface
            .settlements()
            .iter()
            .filter(|p| p.tier <= 1)
            .min_by(|a, b| a.at.distance(from).total_cmp(&b.at.distance(from)))
            .copied()
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

fn move_dungeon_walker(world: &World, player: &mut Player, dx: f64, dz: f64, jump: bool, dt: f32) {
    if jump && !player.airborne {
        player.vy = JUMP_SPEED;
        player.airborne = true;
    }
    player.vy -= GRAVITY * dt;
    let moved = world.move_actor_3d(
        &player.body,
        player.position,
        dx,
        f64::from(player.vy * dt),
        dz,
    );
    player.position = moved.position;
    if moved.grounded {
        player.vy = 0.0;
        player.airborne = false;
    } else {
        player.airborne = true;
    }
    if moved.hit_ceiling && player.vy > 0.0 {
        player.vy = 0.0;
    }
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

fn dead_loot_presentation(
    actor_id: crate::combat::ActorId,
    owner: crate::combat::HostilePresentationSource,
    entity: Option<EntityId>,
) -> Result<Option<EntityId>, SessionError> {
    match (owner, entity) {
        (crate::combat::HostilePresentationSource::Headless, None) => Ok(None),
        (crate::combat::HostilePresentationSource::Headless, Some(entity)) => {
            Err(SessionError::Engine(EngineError::Model(format!(
                "headless dead hostile {actor_id:?} unexpectedly owns presentation entity {entity}"
            ))))
        }
        (_, Some(entity)) => Ok(Some(entity)),
        (owner, None) => Err(SessionError::Engine(EngineError::Model(format!(
            "visible dead hostile {actor_id:?} with presentation owner {owner:?} has no entity during loot synchronization"
        )))),
    }
}

#[cfg(test)]
mod level_up_notice_tests {
    use super::*;
    use crate::progression::{LevelUpEvent, LevelUpResult, Proficiency};

    fn canonical_data() -> crate::gamedata::GameData {
        crate::gamedata::GameData::load(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../data/OrrunGameData.xml"),
        )
        .expect("canonical GameData")
    }

    #[test]
    fn overland_ground_authority_resolves_outside_resident_contact() {
        let atlas = crate::atlas::ContinentAtlas::generate(1, 48);
        let surface =
            Arc::new(crate::world::ContinentalSurface::new(&atlas).expect("canonical surface"));
        let ponds = Arc::new(PondField::empty(GlobalXZ::at(0.0, 0.0)));
        let at = GlobalXZ::at(15_000.192, 12_510.531);
        assert!(!ponds.covers(at, 0.0));
        let mut authority = GroundAuthority::Overland {
            contact: engine::contact::ContactSnapshot::default(),
            surface,
            ponds,
            off_window_ponds: Arc::new(RwLock::new(Vec::new())),
        };

        let feet = authority
            .feet_y(at)
            .expect("procedural authority must resolve off-window overland ground");
        assert!(feet.is_finite());
    }

    #[test]
    fn saved_full_preserves_no_save_due_as_none() {
        let atlas = crate::atlas::ContinentAtlas::generate(1, 48);
        let surface =
            Arc::new(crate::world::ContinentalSurface::new(&atlas).expect("canonical surface"));
        let session = WorldSession::new(surface);

        assert_eq!(session.saved_full(1, 48).expect("no export is due"), None);
    }

    #[test]
    fn saved_full_propagates_player_export_failure() {
        let atlas = crate::atlas::ContinentAtlas::generate(1, 48);
        let surface =
            Arc::new(crate::world::ContinentalSurface::new(&atlas).expect("canonical surface"));
        let mut session = WorldSession::new(surface);
        session.overworld = Some((
            GlobalXZ::at(12.0, -7.0),
            Heading::from_degrees(90.0).expect("finite heading"),
        ));
        session.combat.set_player_hp(0.0);

        assert!(matches!(
            session.saved_full(1, 48),
            Err(SessionError::PlayerSave(
                crate::combat::PlayerSaveError::DeadPlayer(0.0)
            ))
        ));
    }

    #[test]
    fn canonical_world_session_seats_every_live_combat_fixture_identity() {
        let atlas = crate::atlas::ContinentAtlas::generate(1, 48);
        let surface =
            Arc::new(crate::world::ContinentalSurface::new(&atlas).expect("canonical surface"));
        let mut session = WorldSession::new(surface);
        for (index, id) in crate::combat::catalog::LIVE_COMBAT_MOB_IDS
            .iter()
            .enumerate()
        {
            let runtime_index = i32::try_from(index).expect("live fixture index");
            session.combat.add_arena_mob(
                &crate::gamedata::MobId::new(*id),
                runtime_index,
                1.0 + index as f64,
                0.0,
                1.0 + index as f64,
                0.0,
            );
        }
        assert_eq!(
            session.combat.hostiles().len(),
            crate::combat::catalog::LIVE_COMBAT_MOB_IDS.len()
        );
    }

    #[test]
    fn typed_events_use_player_facing_notice_names() {
        let data = canonical_data();
        let skill = data.skills().first().expect("canonical skill");
        let skill_notice = level_up_notice(
            &data,
            LevelUpEvent {
                proficiency: Proficiency::Skill(skill.id().clone()),
                old_level: 1,
                new_level: 2,
                result: LevelUpResult::Skill,
            },
        );
        assert_eq!(skill_notice.name(), skill.name());
        assert_eq!(skill_notice.level(), 2);

        let hp_notice = level_up_notice(
            &data,
            LevelUpEvent {
                proficiency: Proficiency::Hp,
                old_level: 2,
                new_level: 3,
                result: LevelUpResult::Resource {
                    max_before: 112.0,
                    max_after: 124.0,
                },
            },
        );
        assert_eq!(hp_notice.name(), "HP");
    }

    #[test]
    fn headless_dead_hostile_is_explicitly_ignored_for_loot_presentation() {
        let actor = crate::combat::ActorId::from_runtime_index(7);
        let result = dead_loot_presentation(
            actor,
            crate::combat::HostilePresentationSource::Headless,
            None,
        )
        .expect("headless actor is a legitimate no-presentation state");
        assert_eq!(result, None);
    }

    #[test]
    fn visible_dead_hostile_without_entity_is_loud() {
        let actor = crate::combat::ActorId::from_runtime_index(8);
        let err = dead_loot_presentation(
            actor,
            crate::combat::HostilePresentationSource::CombatLayer,
            None,
        )
        .expect_err("visible owner without entity must fail");
        assert!(err.to_string().contains("has no entity"), "{err}");
        assert!(err.to_string().contains("CombatLayer"), "{err}");
    }

    #[test]
    fn headless_dead_hostile_with_entity_is_loud() {
        let actor = crate::combat::ActorId::from_runtime_index(9);
        let mut world = World::new();
        let mesh = engine::mesh::Mesh::new();
        let entity = world.spawn(mesh);
        let err = dead_loot_presentation(
            actor,
            crate::combat::HostilePresentationSource::Headless,
            Some(entity),
        )
        .expect_err("headless actor cannot own a visible entity");
        assert!(err.to_string().contains("unexpectedly owns"), "{err}");
    }
}
