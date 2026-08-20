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
use std::time::{Duration, Instant};

use engine::camera::{Camera, MAX_PITCH_DEGREES};
use engine::collision::ActorBody;
use engine::error::EngineError;
use engine::place::GlobalPlace;
use engine::space::{GlobalPosition, GlobalXZ, RenderOrigin};
use engine::world::{EntityId, Frame, Haze, Sky, World};
use engine::{Key, MouseButton, SpaceId};

use crate::controls::PressedActions;
use crate::settings::KeyBinds;
use glam::{Vec2, Vec3};
use thiserror::Error;

use super::coords::{Heading, CHUNK_SPAN_M};
use super::doors::DoorLayer;
use super::dungeon::{DungeonError, DungeonLayer};
use super::entry::{resolve_spawn, EntryError, SpawnPose, WorldEntryRequest};
use super::fauna::{FaunaError, FaunaLayer};
use super::footprint::BuildingIndex;
use super::look::install_daylight;
use super::paths::PathLayer;
use super::ponds::{PondField, PondWindow};
use super::scatter::{ScatterCatalog, ScatterError, ScatterLayer};
use super::settlement::{HamletStand, HouseDoor, SettlementError, SettlementLayer};
use super::villagers::VillagerLayer;
use super::footprint::HousePlot;
use super::surface::ContinentalSurface;
use super::travel::{
    travel_view, ContinentProxySpec, TravelPhase, TravelSource, TravelTimings, TravelView,
};
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
            actions: crate::controls::resolve_pressed(binds, keys),
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
    /// Clipped humans in seated tier-0 hamlets.
    villagers: Option<VillagerLayer>,
    /// Swinging house leaves and the one live portal interior.
    doors: DoorLayer,
    /// Atlas dungeon mouths: pits, background generate, floor hatches.
    dungeons: Option<DungeonLayer>,
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
    /// Soft lock + auto-attack. Empty hostiles until a fill/fixture registers them.
    combat: crate::combat::WorldCombat,
    combat_layer: super::combat_layer::CombatLayer,
    inventory: crate::inventory::Inventory,
    ground_loot: Vec<crate::loot::GroundPile>,
    bag_open: bool,
    loot_open: bool,
    loot_target: Option<i32>,
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
            villagers: None,
            doors: DoorLayer::new(),
            dungeons: None,
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
            combat: crate::combat::WorldCombat::specialist(1, crate::combat::Discipline::Martial),
            combat_layer: super::combat_layer::CombatLayer::install(),
            inventory: crate::inventory::Inventory::create_kit(),
            ground_loot: Vec::new(),
            bag_open: false,
            loot_open: false,
            loot_target: None,
            last_shrine: None,
            key_binds: KeyBinds::default(),
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

    pub fn combat(&self) -> &crate::combat::WorldCombat {
        &self.combat
    }

    pub fn combat_mut(&mut self) -> &mut crate::combat::WorldCombat {
        &mut self.combat
    }

    /// Soft lock id. Tab cycles; does not steal Esc or E.
    pub fn lock_id(&self) -> Option<i32> {
        self.combat.lock
    }

    /// Lock tell inspect: name + current HP. None if unlocked or name/hp unset.
    pub fn lock_name_hp(&self) -> Option<(&str, f64)> {
        let id = self.combat.lock?;
        let h = self.combat.hostiles.iter().find(|h| h.idx == id)?;
        if h.name.is_empty() {
            return None;
        }
        Some((h.name.as_str(), h.hp))
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
        self.loot_open
    }

    pub fn loot_target(&self) -> Option<i32> {
        self.loot_target.filter(|_| self.loot_open)
    }

    pub fn ground_pile(&self) -> Option<&crate::loot::GroundPile> {
        let idx = self.loot_target?;
        self.ground_loot.iter().find(|p| p.hostile_idx == idx)
    }

    pub fn sparkle_visible(&self, world: &World) -> bool {
        self.combat_layer.sparkle_visible(world)
    }

    pub fn close_loot(&mut self) {
        self.loot_open = false;
        self.loot_target = None;
    }

    pub fn take_loot_item(&mut self, world: &mut World, item_i: usize) {
        let Some(idx) = self.loot_target else {
            return;
        };
        let Some(pos) = self.ground_loot.iter().position(|p| p.hostile_idx == idx) else {
            return;
        };
        let mut pile = self.ground_loot[pos].clone();
        crate::loot::take_one(&mut self.inventory, &mut pile, item_i);
        self.finish_loot_take(world, pos, pile);
    }

    pub fn take_all_loot(&mut self, world: &mut World) {
        let Some(idx) = self.loot_target else {
            return;
        };
        let Some(pos) = self.ground_loot.iter().position(|p| p.hostile_idx == idx) else {
            return;
        };
        let mut pile = self.ground_loot[pos].clone();
        crate::loot::take_all(&mut self.inventory, &mut pile);
        self.finish_loot_take(world, pos, pile);
    }

    fn finish_loot_take(
        &mut self,
        world: &mut World,
        pos: usize,
        pile: crate::loot::GroundPile,
    ) {
        let idx = pile.hostile_idx;
        if pile.empty() {
            self.ground_loot.remove(pos);
            self.combat_layer.strip_sparkle(world, idx);
            self.close_loot();
        } else {
            self.ground_loot[pos] = pile;
        }
    }

    /// Playtester: force a visible family so sparkle is not coin-only.
    pub fn open_first_loot(&mut self) -> bool {
        let Some(idx) = self.ground_loot.first().map(|p| p.hostile_idx) else {
            return false;
        };
        self.loot_open = true;
        self.loot_target = Some(idx);
        true
    }

    pub fn force_visible_loot(&mut self, idx: i32) {
        if let Some(h) = self.combat.hostiles.iter().find(|h| h.idx == idx) {
            let site = self.loot_site_for(h.x, h.z);
            let pile = crate::loot::force_visible_pile(&h.mob_id, h.idx, site);
            self.ground_loot.retain(|p| p.hostile_idx != idx);
            self.ground_loot.push(pile);
        }
    }

    /// Dead-cone on the same left click. Not Tab. Sparkle must still be up.
    pub fn try_dead_loot(&mut self, player_x: f64, player_z: f64, facing_x: f64, facing_z: f64) -> bool {
        let sparkle_ids: Vec<i32> = self
            .ground_loot
            .iter()
            .filter(|p| !p.empty() && self.combat_layer.has_sparkle(p.hostile_idx))
            .map(|p| p.hostile_idx)
            .collect();
        let pairs: Vec<(i32, f64, f64)> = self
            .combat
            .hostiles
            .iter()
            .filter(|h| !h.alive && sparkle_ids.contains(&h.idx))
            .map(|h| (h.idx, h.x, h.z))
            .collect();
        let ids = crate::combat::tab_candidates(player_x, player_z, facing_x, facing_z, &pairs);
        let Some(idx) = ids.first().copied() else {
            return false;
        };
        self.loot_open = true;
        self.loot_target = Some(idx);
        true
    }

    fn loot_site_for(&self, x: f64, z: f64) -> Option<crate::loot::LootSite> {
        let site = self.overland_sites.iter().min_by(|a, b| {
            let da = (a.at.x - x).hypot(a.at.z - z);
            let db = (b.at.x - x).hypot(b.at.z - z);
            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
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

    fn sync_ground_loot(&mut self, world: &mut World) -> Result<(), SessionError> {
        let mut planned: Vec<(i32, engine::world::EntityId, f64, f64, f64, Option<crate::loot::GroundPile>)> = Vec::new();
        for h in &self.combat.hostiles {
            if h.alive {
                continue;
            }
            let Some(entity) = h.entity else {
                continue;
            };
            if !self.combat_layer.is_death_posed(entity) {
                continue;
            }
            let y = self
                .contact_height(GlobalXZ::at(h.x, h.z))
                .map(|g| (g + FOOT_CLEARANCE_M) as f64)
                .unwrap_or(0.0);
            if self.ground_loot.iter().any(|p| p.hostile_idx == h.idx) {
                if !self.combat_layer.has_sparkle(h.idx) {
                    planned.push((h.idx, entity, h.x, y, h.z, None));
                }
                continue;
            }
            let site = self.loot_site_for(h.x, h.z);
            let pile = crate::loot::roll_pile(&h.mob_id, h.idx, site);
            planned.push((h.idx, entity, h.x, y, h.z, Some(pile)));
        }
        for (idx, entity, x, y, z, pile) in planned {
            self.combat_layer.spawn_sparkle(world, idx, entity, x, y, z)?;
            if let Some(pile) = pile {
                self.ground_loot.push(pile);
            }
        }
        Ok(())
    }

    fn clear_ground_loot(&mut self, world: &mut World) {
        self.combat_layer.strip_all_sparkles(world);
        self.forget_ground_loot();
    }

    fn forget_ground_loot(&mut self) {
        self.ground_loot.clear();
        self.loot_open = false;
        self.loot_target = None;
    }

    pub fn fixture_mesh_visible(&self, world: &World) -> bool {
        self.combat_layer.mesh_visible(world)
    }

    pub fn player_hp(&self) -> f64 {
        self.combat.player.resources.hp
    }

    pub fn player_hp_max(&self) -> f64 {
        self.combat.player.resources.hp_max
    }

    pub fn player_mana(&self) -> f64 {
        self.combat.player.resources.mana
    }

    pub fn player_mana_max(&self) -> f64 {
        self.combat.player.resources.mana_max
    }

    /// Player HP/mana bars are always drawn in world_hud.
    pub fn player_hp_visible(&self) -> bool {
        true
    }

    pub fn attack_pip(&self) -> bool {
        self.combat_layer.attack_pip()
    }

    pub fn incoming_hit(&self) -> bool {
        self.combat_layer.incoming_hit()
    }

    /// Replay catalog anim_melee (Punch) on hostiles that have the clip.
    pub fn replay_melee(&mut self, world: &mut World) {
        self.combat_layer.replay_melee(world, &self.combat);
    }

    /// Replay catalog anim_weapon (Spellcast_Shoot) on the locked mesh. Fail-loud.
    pub fn replay_weapon(&mut self, world: &mut World) {
        self.combat_layer.replay_weapon(world, &self.combat);
    }

    pub fn hurt_flash(&self) -> bool {
        self.combat_layer.hurt_flash()
    }

    pub fn hp_ghost_frac(&self) -> Option<f32> {
        self.combat_layer.hp_ghost_frac()
    }

    pub fn slain_line(&self) -> Option<String> {
        self.combat.slain_by.as_ref().map(|n| format!("slain by {n}"))
    }

    pub fn swings_stopped(&self) -> bool {
        self.combat.dead
    }

    pub fn is_shaken(&self) -> bool {
        self.combat.player.shaken.as_ref().is_some_and(|s| s.remaining_s > 0.0)
    }

    pub fn combat_log(&self) -> Vec<String> {
        self.combat.log.lines().map(str::to_string).collect()
    }

    pub fn fail_tell(&self) -> Option<&'static str> {
        self.combat.fail_tell()
    }

    pub fn take_combat_sfx(&mut self) -> Vec<super::combat_layer::CombatSfx> {
        self.combat_layer.take_combat_sfx()
    }

    pub fn swing_whoosh(&self) -> bool {
        self.combat_layer.swing_whoosh()
    }

    pub fn hit_flash(&self) -> bool {
        self.combat_layer.hit_flash()
    }

    pub fn lock_ring_visible(&self, world: &World) -> bool {
        self.combat_layer.lock_ring_visible(world)
    }

    pub fn first_auto_hit(&self) -> Option<i32> {
        self.combat_layer.first_auto()
    }

    fn resolve_death(&mut self, world: &mut World) {
        let place = self.last_shrine().or_else(|| {
            self.spawn.map(|s| {
                GlobalPlace::at(s.position()).with_yaw_deg(s.heading().degrees())
            })
        });
        if let Some(place) = place {
            if let Some(player) = self.player.as_mut() {
                player.position = place.position;
                player.yaw_degrees = place.yaw_degrees;
                player.pitch_degrees = -15.0;
            }
        }
        self.combat.finish_death_respawn();
        self.combat.hostiles.clear();
        self.combat_layer.despawn_meshes(world);
    }

    pub fn last_shrine(&self) -> Option<GlobalPlace> {
        self.last_shrine.or_else(|| self.dungeons.as_ref().and_then(|d| d.shrine()))
    }

    pub fn combat_walk_speed(&self) -> f32 {
        WALK_SPEED
    }

    /// Reseats the L1 wolf line on the next world tick. Meshes despawn now.
    pub fn rearm_combat_fixtures(&mut self, world: &mut World) {
        self.combat_layer.despawn_meshes(world);
        self.clear_ground_loot(world);
        super::sites::despawn_site_props(world, &mut self.site_prop_ids);
        super::sites::clear_overland_sites(&mut self.combat);
        self.overland_sites.clear();
        self.combat_layer.request_wolf_fixture();
        self.combat_layer.skip_roster_pins();
        self.combat_layer.rearm();
        self.dungeon_skulls_for = None;
    }

    /// Reseats one published orc on the next world tick. Meshes despawn now.
    pub fn rearm_orc_fixture(&mut self, world: &mut World) {
        self.combat_layer.despawn_meshes(world);
        self.clear_ground_loot(world);
        super::sites::despawn_site_props(world, &mut self.site_prop_ids);
        super::sites::clear_overland_sites(&mut self.combat);
        self.overland_sites.clear();
        self.combat_layer.request_orc_fixture();
        self.combat_layer.skip_roster_pins();
        self.combat_layer.rearm();
        self.dungeon_skulls_for = None;
    }

    /// Reseats one published yeti on the next world tick. Meshes despawn now.
    pub fn rearm_yeti_fixture(&mut self, world: &mut World) {
        self.combat_layer.despawn_meshes(world);
        self.clear_ground_loot(world);
        super::sites::despawn_site_props(world, &mut self.site_prop_ids);
        super::sites::clear_overland_sites(&mut self.combat);
        self.overland_sites.clear();
        self.combat_layer.request_yeti_fixture();
        self.combat_layer.skip_roster_pins();
        self.combat_layer.rearm();
        self.dungeon_skulls_for = None;
    }

    /// Reseats one published demon on the next world tick. Meshes despawn now.
    pub fn rearm_demon_fixture(&mut self, world: &mut World) {
        self.combat_layer.despawn_meshes(world);
        self.clear_ground_loot(world);
        super::sites::despawn_site_props(world, &mut self.site_prop_ids);
        super::sites::clear_overland_sites(&mut self.combat);
        self.overland_sites.clear();
        self.combat_layer.request_demon_fixture();
        self.combat_layer.skip_roster_pins();
        self.combat_layer.rearm();
        self.dungeon_skulls_for = None;
    }

    /// Reseats one published blue_demon on the next world tick. Meshes despawn now.
    pub fn rearm_bluedemon_fixture(&mut self, world: &mut World) {
        self.combat_layer.despawn_meshes(world);
        self.clear_ground_loot(world);
        super::sites::despawn_site_props(world, &mut self.site_prop_ids);
        super::sites::clear_overland_sites(&mut self.combat);
        self.overland_sites.clear();
        self.combat_layer.request_bluedemon_fixture();
        self.combat_layer.skip_roster_pins();
        self.combat_layer.rearm();
        self.dungeon_skulls_for = None;
    }

    /// Reseats one published tribal_veteran on the next world tick. Meshes despawn now.
    pub fn rearm_tribal_veteran_fixture(&mut self, world: &mut World) {
        self.combat_layer.despawn_meshes(world);
        self.clear_ground_loot(world);
        super::sites::despawn_site_props(world, &mut self.site_prop_ids);
        super::sites::clear_overland_sites(&mut self.combat);
        self.overland_sites.clear();
        self.combat_layer.request_tribal_veteran_fixture();
        self.combat_layer.skip_roster_pins();
        self.combat_layer.rearm();
        self.dungeon_skulls_for = None;
    }

    pub fn rearm_bones_fixture(&mut self, world: &mut World) {
        self.combat_layer.despawn_meshes(world);
        self.clear_ground_loot(world);
        super::sites::despawn_site_props(world, &mut self.site_prop_ids);
        super::sites::clear_overland_sites(&mut self.combat);
        self.overland_sites.clear();
        self.combat_layer.request_bones_fixture();
        self.combat_layer.skip_roster_pins();
        self.combat_layer.rearm();
        self.combat.lock = None;
        self.dungeon_skulls_for = None;
    }

    pub fn rearm_mage_fixture(&mut self, world: &mut World) {
        self.combat_layer.despawn_meshes(world);
        self.clear_ground_loot(world);
        super::sites::despawn_site_props(world, &mut self.site_prop_ids);
        super::sites::clear_overland_sites(&mut self.combat);
        self.overland_sites.clear();
        self.combat_layer.request_mage_fixture();
        self.combat_layer.skip_roster_pins();
        self.combat_layer.rearm();
        self.combat.lock = None;
        self.dungeon_skulls_for = None;
    }

    pub fn key_binds(&self) -> &KeyBinds {
        &self.key_binds
    }

    pub fn set_key_binds(&mut self, binds: KeyBinds) {
        self.key_binds = binds;
    }

    pub fn apply_save(&mut self, stand: &crate::save::SavedStand) {
        self.combat.player.stats.level = stand.level;
        self.combat.player.xp = stand.xp;
        self.combat.player.resources.hp = stand.hp;
        self.combat.player.resources.mana = stand.mana;
        self.combat.player.stats.attrs = stand.attrs;
        self.combat.player.stats.ranks = stand.ranks;
        if stand.shaken_until > 0.0 {
            self.combat.player.shaken = Some(crate::combat::Shaken {
                remaining_s: stand.shaken_until,
            });
        } else {
            self.combat.player.shaken = None;
        }
        self.last_shrine = stand.last_shrine.map(|s| s.to_place());
        self.inventory = stand.inventory;
    }

    pub fn saved_full(&self, seed: i32, size: usize) -> Option<crate::save::SavedStand> {
        let (at, heading) = self.saved_stand()?;
        let p = &self.combat.player;
        let mut stand = crate::save::SavedStand::new(seed, size, at, heading);
        stand.level = p.stats.level;
        stand.xp = p.xp;
        stand.hp = p.resources.hp;
        stand.mana = p.resources.mana;
        stand.attrs = p.stats.attrs;
        stand.ranks = p.stats.ranks;
        stand.shaken_until = p.shaken.as_ref().map(|s| s.remaining_s).unwrap_or(0.0);
        stand.last_shrine = self.last_shrine().map(crate::save::SavedShrine::from_place);
        stand.inventory = self.inventory;
        Some(stand)
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
        let mut input = WalkInput::from_frame(frame, yaw, pitch, mode, looking, &self.key_binds);
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
            let ui_open = self.bag_open || self.loot_open;
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
        if let Some(villagers) = self.villagers.as_mut() {
            villagers.clear(world)?;
        }
        if let Some(dungeons) = self.dungeons.as_mut() {
            dungeons.clear(world)?;
        }

        self.stream.reset(world);
        world.set_render_origin(RenderOrigin::snapped(approach.ground(), CHUNK_SPAN_M)?)?;
        self.spawn = None;
        self.player = None;
        self.entering = Some(request);
        self.overworld = None;
        // Bodies died with the stream. Keep planned sites + bandit XZ.
        self.site_prop_ids.clear();
        self.combat_layer.forget_meshes();
        self.forget_ground_loot();
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
            self.remember_overworld_from(world, player.position.horizontal(), player.yaw_degrees);
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
                self.villagers = Some(VillagerLayer::new());
                self.dungeons = Some(DungeonLayer::install());
            }
            if let Some(dungeons) = self.dungeons.as_mut() {
                let rebuilt =
                    dungeons.follow(world, &self.stream, &self.surface, request.requested())?;
                if rebuilt {
                    let plots = self.plot_index();
                    self.stream.set_house_plots(world, (*plots).clone())?;
                }
            }
            if !self.ponds.traced(request.requested()) {
                return Ok(false);
            }
            let pose = resolve_spawn(&self.surface, &self.ponds.field(), request)?;
            self.spawn = Some(pose);
            if self.last_shrine.is_none() {
                self.last_shrine = Some(
                    GlobalPlace::at(pose.position()).with_yaw_deg(pose.heading().degrees()),
                );
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

    fn hostile_feet_y(&self, world: &World, player: &Player, h: &crate::combat::WorldHostile) -> f64 {
        if h.mob_id == "orc_skull" || super::combat_layer::is_bone_id(&h.mob_id) {
            if let Some(y) = self
                .dungeons
                .as_ref()
                .and_then(|d| d.indoor_floor_y(world, player.position))
            {
                return f64::from(y + FOOT_CLEARANCE_M);
            }
        }
        self.contact_height(GlobalXZ::at(h.x, h.z))
            .map(|g| (g + FOOT_CLEARANCE_M) as f64)
            .unwrap_or(player.position.y)
    }

    fn respawn_hostile_meshes(
        &mut self,
        world: &mut World,
        player: &Player,
    ) -> Result<(), SessionError> {
        let feet: Vec<f64> = self
            .combat
            .hostiles
            .iter()
            .map(|h| self.hostile_feet_y(world, player, h))
            .collect();
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
                super::combat_layer::clear_dungeon_skulls(&mut self.combat);
                super::combat_layer::clear_dungeon_bones(&mut self.combat);
                if self.combat_layer.fixture_ready() {
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
            super::combat_layer::clear_dungeon_skulls(&mut self.combat);
            super::combat_layer::clear_dungeon_bones(&mut self.combat);
        }
        let spots = self
            .dungeons
            .as_ref()
            .map(DungeonLayer::live_skulls)
            .unwrap_or_default();
        let heart = self.dungeons.as_ref().and_then(DungeonLayer::live_heart);
        if !spots.is_empty() {
            super::combat_layer::seat_dungeon_skulls(&mut self.combat, &spots);
            super::combat_layer::seat_dungeon_bones(&mut self.combat, &spots, heart);
            if self.combat_layer.fixture_ready() {
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
        if self.combat_layer.roster_pins_skipped() {
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
            self.site_prop_ids =
                super::sites::spawn_site_props(world, &self.surface, &sites)?;
            self.overland_sites = sites;
            self.roster_pins_seated = true;
            return Ok(true);
        }
        let want = self.overland_sites.len() * 2;
        let have = self
            .combat
            .hostiles
            .iter()
            .filter(|h| super::sites::is_bandit_id(&h.mob_id) && h.alive)
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

    fn update_world(&mut self, world: &mut World, input: WalkInput) -> Result<(), SessionError> {
        let mut player = self.player.ok_or(SessionError::NoWorld)?;

        if input.toggle_fly {
            player.mode = player.mode.toggled();
            player.vy = 0.0;
            player.airborne = false;
        }
        {
            let facing = Camera::facing_xz(player.yaw_degrees);
            if !self.combat_layer.fixture_ready() {
                if self.combat_layer.wants_orc() {
                    self.combat_layer.install_orc_fixture(
                        &mut self.combat,
                        player.position.x,
                        player.position.z,
                        facing.x as f64,
                        facing.z as f64,
                    );
                } else if self.combat_layer.wants_yeti() {
                    self.combat_layer.install_yeti_fixture(
                        &mut self.combat,
                        player.position.x,
                        player.position.z,
                        facing.x as f64,
                        facing.z as f64,
                    );
                } else if self.combat_layer.wants_demon() {
                    self.combat_layer.install_demon_fixture(
                        &mut self.combat,
                        player.position.x,
                        player.position.z,
                        facing.x as f64,
                        facing.z as f64,
                    );
                } else if self.combat_layer.wants_bluedemon() {
                    self.combat_layer.install_bluedemon_fixture(
                        &mut self.combat,
                        player.position.x,
                        player.position.z,
                        facing.x as f64,
                        facing.z as f64,
                    );
                } else if self.combat_layer.wants_tribal_veteran() {
                    self.combat_layer.install_tribal_veteran_fixture(
                        &mut self.combat,
                        player.position.x,
                        player.position.z,
                        facing.x as f64,
                        facing.z as f64,
                    );
                } else if self.combat_layer.wants_bones() {
                    self.combat_layer.install_bones_fixture(
                        &mut self.combat,
                        player.position.x,
                        player.position.z,
                        facing.x as f64,
                        facing.z as f64,
                    );
                } else if self.combat_layer.wants_mage() {
                    self.combat_layer.install_mage_fixture(
                        &mut self.combat,
                        player.position.x,
                        player.position.z,
                        facing.x as f64,
                        facing.z as f64,
                    );
                } else if self.combat_layer.roster_pins_skipped() {
                    self.combat_layer.install_l1_wolf_line(
                        &mut self.combat,
                        player.position.x,
                        player.position.z,
                        facing.x as f64,
                        facing.z as f64,
                    );
                } else {
                    // Taken Cairn / Woods Hut: do not wipe site hostiles with L1 wolves.
                    self.ensure_overland_sites(world, player.position)?;
                    self.combat_layer.hold_fixture();
                }
                let feet: Vec<f64> = self
                    .combat
                    .hostiles
                    .iter()
                    .map(|h| {
                        self.contact_height(GlobalXZ::at(h.x, h.z))
                            .map(|g| (g + FOOT_CLEARANCE_M) as f64)
                            .unwrap_or(player.position.y)
                    })
                    .collect();
                if let Err(err) = self.combat_layer.spawn_wolf_meshes(
                    world,
                    &mut self.combat,
                    &feet,
                    player.yaw_degrees,
                ) {
                    self.combat_layer.rearm();
                    return Err(err.into());
                }
            }
        }
        if self.combat_layer.fixture_ready()
            && !self.combat_layer.roster_pins_skipped()
        {
            let restamped = self.ensure_overland_sites(world, player.position)?;
            if restamped {
                self.respawn_hostile_meshes(world, &player)?;
            }
        }
        if input.tab || (input.capture_look && world.pointer_lock()) {
            let facing = Camera::facing_xz(player.yaw_degrees);
            let px = player.position.x;
            let pz = player.position.z;
            if input.tab {
                self.combat.press_tab(px, pz, facing.x as f64, facing.z as f64);
            } else if input.capture_look && world.pointer_lock() && !self.bag_open && !self.loot_open {
                // Dead-cone is not Tab. Same left click; sparkle still up.
                if self.try_dead_loot(px, pz, facing.x as f64, facing.z as f64) {
                    world.set_pointer_lock(false);
                } else {
                    self.combat.click_lock(px, pz, facing.x as f64, facing.z as f64);
                }
            }
        }
        for verb in input.actions.iter() {
            let facing = Camera::facing_xz(player.yaw_degrees);
            let started = self.combat.press_verb(
                verb,
                player.position.x,
                player.position.z,
                facing.x as f64,
                facing.z as f64,
            );
            if started && verb == crate::controls::Action::Potion {
                self.combat_layer.log_potion(&mut self.combat);
            }
            if started && verb == crate::controls::Action::Ward {
                self.combat_layer.log_ward(&mut self.combat);
            }
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
                if in_dungeon {
                    move_dungeon_walker(world, &mut player, dx, dz, input.jump, input.dt);
                } else {
                    let to = world.move_actor(&player.body, player.position.horizontal(), dx, dz);
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
            let py = player.position.y;
            let feet: Vec<(f64, f64, f64)> = self
                .combat
                .hostiles
                .iter()
                .map(|h| {
                    let y = self
                        .contact_height(GlobalXZ::at(h.x, h.z))
                        .map(|g| (g + FOOT_CLEARANCE_M) as f64)
                        .unwrap_or(py);
                    (h.x, h.z, y)
                })
                .collect();
            let ground_y = move |x: f64, z: f64| {
                feet.iter()
                    .min_by(|a, b| {
                        let da = (a.0 - x).hypot(a.1 - z);
                        let db = (b.0 - x).hypot(b.1 - z);
                        da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .map(|k| k.2)
                    .unwrap_or(py)
            };
            self.combat_layer.present(world, &self.combat, ground_y, input.dt)?;
            self.sync_ground_loot(world)?;
            if self.combat.dead
                && self.combat.player.resources.hp <= 0.0
                && self.combat.slain_hold_s <= 0.0
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
        self.ponds.follow(foot);
        world.hitch_span("ponds", hitch_ms(t), String::new());

        let t = Instant::now();
        let rebased = self.stream.maybe_rebase(world, foot)?;
        if rebased && self.combat_layer.fixture_ready() {
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
        self.sync_dungeon_skulls(world, &player)?;
        let hidden_leaf = self.doors.hidden_leaf();
        if let Some(settlements) = self.settlements.as_mut() {
            settlements.hide_leaf(world, hidden_leaf)?;
        }

        // Doors need the actor centre. The outdoor hatch needs the soles for
        // falling through; the dungeon ceiling hatch needs the actor's head.
        self.remember_overworld_from(world, player.position.horizontal(), player.yaw_degrees);
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
                if !in_dungeon {
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

        world.look_first_person_global(player.eye(), player.yaw_degrees, player.pitch_degrees)?;

        self.player = Some(player);
        Ok(())
    }

    /// Ground height reported by the resident mesh under a global point.
    pub fn contact_height(&self, p: GlobalXZ) -> Option<f32> {
        self.stream.contact_height(p)
    }

    /// Stand to write on exit: the last default-space feet, never a dungeon interior.
    pub fn saved_stand(&self) -> Option<(GlobalXZ, Heading)> {
        if let Some(stand) = self.overworld {
            return Some(stand);
        }
        let player = self.player?;
        let heading = Heading::from_degrees(player.yaw_degrees).ok()?;
        Some((player.position.horizontal(), heading))
    }

    fn remember_overworld_from(&mut self, world: &World, at: GlobalXZ, yaw_degrees: f32) {
        if world.living_in() != SpaceId::DEFAULT {
            return;
        }
        let Ok(heading) = Heading::from_degrees(yaw_degrees) else {
            return;
        };
        self.overworld = Some((at, heading));
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
        self.doors
            .hint()
            .or_else(|| self.dungeons.as_ref().and_then(DungeonLayer::hint))
    }

    /// True while a nearby dungeon layout is still on the worker.
    pub fn dungeon_generating(&self) -> bool {
        self.dungeons.as_ref().is_some_and(DungeonLayer::generating)
    }

    /// HUD line while a nearby dungeon is still being cut.
    pub fn dungeon_build_status(&self) -> Option<String> {
        self.dungeons.as_ref().and_then(DungeonLayer::build_status)
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

    /// Camera stand pulled in on the yard pair (tent canvas + ring stones).
    pub fn village_camp_stand(&self) -> Option<GlobalPosition> {
        let (tent, ring) = self.village_camp_pair()?;
        let mid_x = (tent.x + ring.x) * 0.5;
        let mid_z = (tent.z + ring.z) * 0.5;
        let ax = (ring.x - tent.x) as f32;
        let az = (ring.z - tent.z) as f32;
        let len = (ax * ax + az * az).sqrt().max(1e-6);
        let px = -az / len;
        let pz = ax / len;
        let stand = GlobalXZ::at(
            mid_x + f64::from(px * 4.6 - (ax / len) * 1.3),
            mid_z + f64::from(pz * 4.6 - (az / len) * 1.3),
        );
        let ground = self
            .contact_height(stand)
            .unwrap_or_else(|| self.surface.column(stand).ground());
        Some(GlobalPosition::at(stand.x, f64::from(ground + 1.85), stand.z))
    }

    pub fn village_camp_look(&self) -> Option<(Vec3, Vec3)> {
        let (tent, ring) = self.village_camp_pair()?;
        let pos = self.player_position()?;
        let eye = Vec3::new(pos.x as f32, pos.y as f32, pos.z as f32);
        let target = Vec3::new(
            ((tent.x + ring.x) * 0.5) as f32,
            ((tent.y + ring.y) * 0.5) as f32 + 0.7,
            ((tent.z + ring.z) * 0.5) as f32,
        );
        Some((eye, target))
    }


    pub fn village_human_count(&self) -> usize {
        self.villagers.as_ref().map_or(0, VillagerLayer::human_count)
    }

    pub fn village_human_mesh_count(&self, world: &World) -> usize {
        self.villagers
            .as_ref()
            .map_or(0, |v| v.mesh_count(world))
    }

    pub fn village_human_on_corridor(&self) -> bool {
        self.village_corridor_human().is_some()
    }

    pub fn village_corridor_human(&self) -> Option<engine::space::GlobalPosition> {
        let cut = self.village_cut();
        self.villagers.as_ref().and_then(|v| v.corridor_human(cut))
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

    pub fn overland_site(&self, kind: super::sites::SiteKind) -> Option<super::sites::OverlandSite> {
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
            .min_by(|a, b| {
                a.at.distance(from)
                    .partial_cmp(&b.at.distance(from))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
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
