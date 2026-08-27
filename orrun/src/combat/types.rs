//! Live combat state driven by canonical action resolution.

use thiserror::Error;

use super::log::CombatLog;
use super::math::*;
use crate::gamedata::{
    ActionId, ActionTarget, FactionId, MobDefinition, MobId, MobMode, MovementSpec, PlayerProfile,
};
use crate::progression::{ActorProgression, ActorProgressionSnapshot, ProgressionError};
use crate::resolution::{Actor, Resolution, Resolver, TargetSelection};

#[derive(Clone, Debug, PartialEq)]
pub struct PlayerSaveSnapshot {
    progression: ActorProgressionSnapshot,
    hp: f64,
    mana: f64,
}

impl PlayerSaveSnapshot {
    pub fn new(progression: ActorProgressionSnapshot, hp: f64, mana: f64) -> Self {
        Self {
            progression,
            hp,
            mana,
        }
    }
    pub fn progression(&self) -> &ActorProgressionSnapshot {
        &self.progression
    }
    pub fn hp(&self) -> f64 {
        self.hp
    }
    pub fn mana(&self) -> f64 {
        self.mana
    }
}

impl serde::Serialize for PlayerSaveSnapshot {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(serde::Serialize)]
        struct Dto<'a> {
            progression: &'a ActorProgressionSnapshot,
            hp: f64,
            mana: f64,
        }
        Dto {
            progression: &self.progression,
            hp: self.hp,
            mana: self.mana,
        }
        .serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for PlayerSaveSnapshot {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Dto {
            progression: ActorProgressionSnapshot,
            hp: f64,
            mana: f64,
        }
        let dto = Dto::deserialize(deserializer)?;
        Ok(Self::new(dto.progression, dto.hp, dto.mana))
    }
}

#[derive(Debug, Clone, PartialEq, Error)]
pub enum PlayerSaveError {
    #[error(transparent)]
    Progression(#[from] ProgressionError),
    #[error("saved player {resource} must be finite, got {value}")]
    NonFiniteResource { resource: &'static str, value: f64 },
    #[error("saved player HP must be greater than zero, got {0}")]
    DeadPlayer(f64),
    #[error("saved player {resource} {value} is outside 0..={maximum}")]
    ResourceOutOfRange {
        resource: &'static str,
        value: f64,
        maximum: f64,
    },
}

#[derive(Clone, Debug)]
pub struct CombatResources {
    hp: f64,
    hp_max: f64,
    mana: f64,
    mana_max: f64,
}

impl CombatResources {
    pub fn from_progression(progression: &ActorProgression) -> Self {
        Self {
            hp: progression.hp_max(),
            hp_max: progression.hp_max(),
            mana: progression.mana_max(),
            mana_max: progression.mana_max(),
        }
    }

    pub fn regen_combat(&mut self, dt: f64) {
        self.mana = self.max_mana_cap(self.mana + MANA_REGEN_COMBAT_PER_S * dt);
    }

    pub fn regen_ooc(&mut self, dt: f64) {
        self.mana = self.max_mana_cap(self.mana + MANA_REGEN_OOC_PER_S * dt);
    }

    pub fn hp(&self) -> f64 {
        self.hp
    }
    pub fn hp_max(&self) -> f64 {
        self.hp_max
    }
    pub fn mana(&self) -> f64 {
        self.mana
    }
    pub fn mana_max(&self) -> f64 {
        self.mana_max
    }
    pub fn set_hp(&mut self, hp: f64) {
        self.hp = hp;
    }
    pub fn set_mana(&mut self, mana: f64) {
        self.mana = mana;
    }
    fn max_mana_cap(&self, mana: f64) -> f64 {
        self.mana_max.min(mana)
    }
}

#[derive(Clone, Debug)]
pub struct Shaken {
    pub remaining_s: f64,
}

impl Shaken {
    pub fn from_death() -> Self {
        Self {
            remaining_s: SHAKEN_S,
        }
    }

    fn tick(&mut self, dt: f64) {
        self.remaining_s = (self.remaining_s - dt).max(0.0);
    }
}

#[derive(Clone, Debug)]
pub struct Aggro {
    pub sight_m: f64,
    pub hear_m: f64,
    pub leash_m: f64,
    pub social_m: f64,
}

impl Default for Aggro {
    fn default() -> Self {
        Self {
            sight_m: SIGHT_AGGRO_M,
            hear_m: HEAR_AGGRO_M,
            leash_m: LEASH_M,
            social_m: SOCIAL_M,
        }
    }
}

#[derive(Clone, Debug)]
pub struct TargetLock {
    pub actor_idx: i32,
    pub cycle: Vec<i32>,
    pub cycle_i: usize,
}

impl TargetLock {
    pub fn clear() -> Option<Self> {
        None
    }
}

/// Nearest hostile in 20 m / 90° front cone, then cycle; off after last.
/// `facing_yaw_rad` is the look yaw (same convention as the walker).
/// `hostiles` is (idx, x, z). Player at origin of the xz pairs already translated.
pub fn tab_candidates(
    player_x: f64,
    player_z: f64,
    facing_x: f64,
    facing_z: f64,
    hostiles: &[(i32, f64, f64)],
) -> Vec<i32> {
    let mut in_cone: Vec<(f64, i32)> = Vec::new();
    let fl = (facing_x * facing_x + facing_z * facing_z).sqrt();
    if fl <= 1e-9 {
        return Vec::new();
    }
    let fx = facing_x / fl;
    let fz = facing_z / fl;
    let half = (TAB_CONE_DEG.to_radians()) * 0.5;
    for &(idx, x, z) in hostiles {
        let dx = x - player_x;
        let dz = z - player_z;
        let dist = (dx * dx + dz * dz).sqrt();
        if dist <= 0.0 || dist > TAB_LOCK_M {
            continue;
        }
        let nx = dx / dist;
        let nz = dz / dist;
        let dot = (fx * nx + fz * nz).clamp(-1.0, 1.0);
        let ang = dot.acos();
        if ang <= half {
            in_cone.push((dist, idx));
        }
    }
    in_cone.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    in_cone.into_iter().map(|(_, i)| i).collect()
}

#[derive(Clone, Debug)]
pub struct LivePlayer {
    pub resources: CombatResources,
    pub lock: Option<i32>,
    pub shaken: Option<Shaken>,
    pub sprinted: bool,
    pub used_pin_or_bind: bool,
    faction: FactionId,
    progression: ActorProgression,
    pub(crate) canonical_actor: Option<Actor>,
}

impl LivePlayer {
    pub fn faction(&self) -> &FactionId {
        self.canonical_actor
            .as_ref()
            .map_or(&self.faction, Actor::effective_faction)
    }

    fn from_profile(profile: &PlayerProfile) -> Self {
        let progression = ActorProgression::from_profile(profile);
        let resources = CombatResources::from_progression(&progression);
        Self {
            resources,
            lock: None,
            shaken: None,
            sprinted: false,
            used_pin_or_bind: false,
            faction: profile.faction().clone(),
            progression,
            canonical_actor: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ActorId {
    canonical: u32,
    runtime_index: i32,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SpawnSeed(u64);
impl SpawnSeed {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
    pub const fn value(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CanonicalHeading {
    x: f64,
    z: f64,
}
impl CanonicalHeading {
    pub fn from_xz(x: f64, z: f64) -> Self {
        let length = x.hypot(z);
        if !length.is_finite() || length <= 1e-9 {
            panic!("canonical heading requires a finite non-zero direction");
        }
        Self {
            x: x / length,
            z: z / length,
        }
    }
    pub fn from_degrees(degrees: f32) -> Self {
        if !degrees.is_finite() {
            panic!("canonical heading degrees must be finite");
        }
        let radians = f64::from(degrees).to_radians();
        Self::from_xz(radians.sin(), radians.cos())
    }
    pub const fn x(self) -> f64 {
        self.x
    }
    pub const fn z(self) -> f64 {
        self.z
    }
}

impl ActorId {
    pub const PLAYER: Self = Self {
        canonical: 0,
        runtime_index: -1,
    };

    pub fn from_runtime_index(index: i32) -> Self {
        let canonical = u32::try_from(index)
            .expect("actor runtime index must be non-negative")
            .checked_add(1)
            .expect("actor id space exhausted");
        Self {
            canonical,
            runtime_index: index,
        }
    }

    pub const fn canonical(self) -> u32 {
        self.canonical
    }

    pub fn runtime_index(self) -> Option<i32> {
        (self != Self::PLAYER).then_some(self.runtime_index)
    }

    fn assigned(canonical: u32, runtime_index: i32) -> Self {
        if canonical == 0 {
            panic!("canonical actor sequence cannot use the player id");
        }
        if runtime_index < 0 {
            panic!("actor runtime index must be non-negative");
        }
        Self {
            canonical,
            runtime_index,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostileState {
    Idle,
    Alerted,
    Pursuing,
    Fleeing,
    Attacking,
    Leashing,
    Dead,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostilePresentationSource {
    /// Actor intentionally has no world presentation (simulation/tests only).
    Headless,
    /// Visible body and all semantic animation/transform cues are owned by FaunaLayer.
    Fauna,
    /// Visible body and all semantic animation/transform cues are owned by CombatLayer.
    CombatLayer,
}

#[derive(Clone, Debug)]
pub struct WorldHostile {
    pub idx: i32,
    actor_id: ActorId,
    pub x: f64,
    pub z: f64,
    previous_x: f64,
    previous_z: f64,
    previous_heading: CanonicalHeading,
    hp: f64,
    max_hp: f64,
    pub armor: i32,
    alive: bool,
    /// Display name for the lock tell. Fixture wolves use "wolf-spider".
    pub name: String,
    /// Sheet / catalog id (orc, tribal, orc_skull, wolf).
    pub mob_id: String,
    /// Presentation ownership is explicit; an entity may only be bound by its owner.
    presentation_source: HostilePresentationSource,
    entity: Option<engine::world::EntityId>,
    pub home_x: f64,
    pub home_z: f64,
    pub aggro: Aggro,
    pub state: HostileState,
    faction: FactionId,
    mode: MobMode,
    target: Option<ActorId>,
    provoked_by: Option<ActorId>,
    flee_threat: Option<ActorId>,
    movement_speed_mps: f64,
    speed_multiplier: f64,
    heading: CanonicalHeading,
    spawn_seed: SpawnSeed,
    detection_check: u64,
    detection_left_s: f64,
    awareness_s: f64,
    endurance_s: f64,
    endurance_max_s: f64,
    progression: ActorProgression,
    actions: Vec<ActionId>,
    pub(crate) canonical_actor: Option<Actor>,
}

#[derive(Clone, Debug)]
pub struct IncomingHit {
    pub dealt: i32,
    pub by: String,
    pub killed: bool,
}

#[derive(Clone, Debug)]
pub struct CombatStep {
    pub resolutions: Vec<Resolution>,
}

#[derive(Clone, Debug)]
pub struct WorldCombat {
    #[allow(dead_code)]
    pub(crate) game_data: std::sync::Arc<crate::gamedata::GameData>,
    player: LivePlayer,
    lock: Option<i32>,
    cycle: Vec<i32>,
    hostiles: Vec<WorldHostile>,
    next_actor_id: u32,
    dead: bool,
    slain_by: Option<String>,
    slain_hold_s: f64,
    last_incoming: Option<IncomingHit>,
    log: CombatLog,
    fail_tell: Option<&'static str>,
    fail_tell_s: f64,
    /// Fixed-step accumulator owned by the simulation, never by presentation.
    fixed_accum_s: f64,
    pub(crate) pending_resolutions: Vec<Resolution>,
}

impl WorldHostile {
    fn from_definition(
        idx: i32,
        x: f64,
        z: f64,
        mob: &MobDefinition,
        movement: &MovementSpec,
        game_data: &crate::gamedata::GameData,
        home_x: f64,
        home_z: f64,
    ) -> Self {
        Self {
            idx,
            actor_id: ActorId::from_runtime_index(idx),
            x,
            z,
            previous_x: x,
            previous_z: z,
            previous_heading: CanonicalHeading::from_xz(1.0, 0.0),
            hp: f64::from(mob.hp()),
            max_hp: f64::from(mob.hp()),
            armor: mob.armor(),
            alive: true,
            name: mob.name().to_owned(),
            mob_id: mob.id().as_str().to_owned(),
            presentation_source: HostilePresentationSource::Headless,
            entity: None,
            home_x,
            home_z,
            aggro: Aggro::default(),
            state: HostileState::Idle,
            faction: mob.faction().clone(),
            mode: mob.mode(),
            target: None,
            provoked_by: None,
            flee_threat: None,
            movement_speed_mps: movement.speed_mps(),
            speed_multiplier: 1.0,
            heading: CanonicalHeading::from_xz(1.0, 0.0),
            spawn_seed: SpawnSeed::new(0),
            detection_check: 0,
            detection_left_s: 0.0,
            awareness_s: 0.0,
            endurance_s: mob.endurance_s().seconds(),
            endurance_max_s: mob.endurance_s().seconds(),
            progression: ActorProgression::from_mob(mob, game_data),
            actions: mob.actions().to_vec(),
            canonical_actor: None,
        }
    }

    pub fn actor_id(&self) -> ActorId {
        self.actor_id
    }

    pub const fn presentation_source(&self) -> HostilePresentationSource {
        self.presentation_source
    }

    pub const fn presentation_entity(&self) -> Option<engine::world::EntityId> {
        self.entity
    }

    pub fn bind_presentation(
        &mut self,
        source: HostilePresentationSource,
        entity: engine::world::EntityId,
    ) {
        if source == HostilePresentationSource::Headless {
            panic!("cannot bind a visible entity as headless");
        }
        if self.presentation_source != HostilePresentationSource::Headless || self.entity.is_some()
        {
            panic!("hostile {:?} presentation is already owned", self.actor_id);
        }
        self.presentation_source = source;
        self.entity = Some(entity);
    }

    pub fn faction(&self) -> &FactionId {
        &self.faction
    }

    pub fn mode(&self) -> MobMode {
        self.mode
    }

    pub fn target(&self) -> Option<ActorId> {
        self.target
    }

    pub fn is_engaged(&self) -> bool {
        matches!(
            self.state,
            HostileState::Alerted
                | HostileState::Pursuing
                | HostileState::Fleeing
                | HostileState::Attacking
                | HostileState::Leashing
        )
    }

    pub fn hp(&self) -> f64 {
        self.hp
    }

    pub fn max_hp(&self) -> f64 {
        self.max_hp
    }

    pub fn is_alive(&self) -> bool {
        self.alive
    }
    pub fn heading(&self) -> CanonicalHeading {
        self.heading
    }
    pub fn movement_speed_mps(&self) -> f64 {
        self.movement_speed_mps * self.speed_multiplier
    }
    pub fn effective_movement_speed_mps(&self) -> f64 {
        movement_speed(self)
    }
    pub fn speed_multiplier(&self) -> f64 {
        self.speed_multiplier
    }
    pub fn statuses(&self) -> impl Iterator<Item = &crate::resolution::TimedStatus> {
        self.canonical_actor.iter().flat_map(Actor::statuses)
    }
    pub fn effective_faction(&self) -> &FactionId {
        self.canonical_actor
            .as_ref()
            .map_or(&self.faction, Actor::effective_faction)
    }
    pub fn can_act(&self) -> bool {
        self.canonical_actor.as_ref().is_none_or(Actor::can_act)
    }
    pub fn can_move(&self) -> bool {
        self.canonical_actor.as_ref().is_none_or(Actor::can_move)
    }
    pub fn endurance_seconds(&self) -> f64 {
        self.endurance_s
    }
    pub fn endurance_max_seconds(&self) -> f64 {
        self.endurance_max_s
    }
    pub fn flee_threat(&self) -> Option<ActorId> {
        self.flee_threat
    }
    pub fn presented_pose(&self, alpha: f64) -> (f64, f64, CanonicalHeading) {
        let alpha = alpha.clamp(0.0, 1.0);
        let x = self.previous_x + (self.x - self.previous_x) * alpha;
        let z = self.previous_z + (self.z - self.previous_z) * alpha;
        let hx = self.previous_heading.x() + (self.heading.x() - self.previous_heading.x()) * alpha;
        let hz = self.previous_heading.z() + (self.heading.z() - self.previous_heading.z()) * alpha;
        let heading = if hx.hypot(hz) <= 1e-9 {
            self.heading
        } else {
            CanonicalHeading::from_xz(hx, hz)
        };
        (x, z, heading)
    }
}

const DETECTION_INTERVAL_S: f64 = 1.0;
const AWARENESS_PERSIST_S: f64 = 3.0;
const EXHAUSTED_SPEED_RATIO: f64 = 0.45;

fn mix64(mut v: u64) -> u64 {
    v ^= v >> 30;
    v = v.wrapping_mul(0xbf58476d1ce4e5b9);
    v ^= v >> 27;
    v = v.wrapping_mul(0x94d049bb133111eb);
    v ^ (v >> 31)
}
fn deterministic_unit(seed: SpawnSeed, draw: u64) -> f64 {
    (mix64(seed.value() ^ draw.wrapping_mul(0x9e3779b97f4a7c15)) >> 11) as f64
        / ((1u64 << 53) as f64)
}
fn deterministic_signed_unit(seed: SpawnSeed, draw: u64) -> f64 {
    deterministic_unit(seed, draw) * 2.0 - 1.0
}
fn stable_spawn_hash(idx: i32, id: &[u8], x: f64, z: f64) -> u64 {
    let mut h = mix64(u64::try_from(idx).expect("spawn index must be non-negative"));
    for b in id {
        h = mix64(h ^ u64::from(*b));
    }
    mix64(h ^ x.to_bits() ^ z.to_bits().rotate_left(17))
}
fn movement_speed(a: &WorldHostile) -> f64 {
    a.movement_speed_mps
        * a.speed_multiplier
        * if a.endurance_s <= 0.0 {
            EXHAUSTED_SPEED_RATIO
        } else {
            1.0
        }
        * a.canonical_actor
            .as_ref()
            .map_or(1.0, Actor::movement_multiplier)
}
fn detection_probability(a: &WorldHostile, x: f64, z: f64) -> f64 {
    let dx = x - a.x;
    let dz = z - a.z;
    let d = dx.hypot(dz);
    let facing = if d <= 1e-9 {
        1.0
    } else {
        let dot = a.heading.x * dx / d + a.heading.z * dz / d;
        if dot >= 0.5 {
            1.0
        } else if dot <= -0.5 {
            0.2
        } else {
            0.55
        }
    };
    let sight = if d < a.aggro.sight_m {
        (1.0 - d / a.aggro.sight_m) * facing
    } else {
        0.0
    };
    let hearing = if d < a.aggro.hear_m {
        1.0 - d / a.aggro.hear_m
    } else {
        0.0
    };
    sight.max(hearing).clamp(0.0, 1.0)
}

fn validate_resource(
    resource: &'static str,
    value: f64,
    maximum: f64,
    reject_zero: bool,
) -> Result<(), PlayerSaveError> {
    if !value.is_finite() {
        return Err(PlayerSaveError::NonFiniteResource { resource, value });
    }
    if reject_zero && value <= 0.0 {
        return Err(PlayerSaveError::DeadPlayer(value));
    }
    if value < 0.0 || value > maximum {
        return Err(PlayerSaveError::ResourceOutOfRange {
            resource,
            value,
            maximum,
        });
    }
    Ok(())
}

impl WorldCombat {
    pub fn player_action_cooldown_s(&self, action_id: &ActionId) -> f64 {
        self.player
            .canonical_actor
            .as_ref()
            .expect("canonical player initialized")
            .actions()
            .cooldown_s(action_id)
    }
    pub fn player_action_roster(&self) -> &[ActionId] {
        self.player
            .canonical_actor
            .as_ref()
            .expect("canonical player initialized")
            .actions()
            .roster()
    }
    pub fn game_data(&self) -> &std::sync::Arc<crate::gamedata::GameData> {
        &self.game_data
    }

    pub fn player(&self) -> &LivePlayer {
        &self.player
    }
    pub fn player_mut(&mut self) -> &mut LivePlayer {
        &mut self.player
    }

    pub fn player_progression(&self) -> &ActorProgression {
        &self.player.progression
    }

    /// Export one internally consistent player snapshot. Dead players are not saveable.
    pub fn export_player_save(&self) -> Result<PlayerSaveSnapshot, PlayerSaveError> {
        validate_resource(
            "HP",
            self.player.resources.hp,
            self.player.progression.hp_max(),
            true,
        )?;
        validate_resource(
            "mana",
            self.player.resources.mana,
            self.player.progression.mana_max(),
            false,
        )?;
        Ok(PlayerSaveSnapshot::new(
            self.player.progression.export_snapshot(),
            self.player.resources.hp,
            self.player.resources.mana,
        ))
    }

    /// Validate progression and resources on temporary state, then commit atomically.
    pub fn restore_player_save(
        &mut self,
        snapshot: &PlayerSaveSnapshot,
    ) -> Result<(), PlayerSaveError> {
        let mut progression = self.player.progression.clone();
        progression.restore_snapshot(snapshot.progression())?;
        let hp_max = progression.hp_max();
        let mana_max = progression.mana_max();
        validate_resource("HP", snapshot.hp(), hp_max, true)?;
        validate_resource("mana", snapshot.mana(), mana_max, false)?;
        self.player.progression = progression;
        self.player.resources.hp_max = hp_max;
        self.player.resources.mana_max = mana_max;
        self.player.resources.hp = snapshot.hp();
        self.player.resources.mana = snapshot.mana();
        self.dead = false;
        self.synchronize_canonical_player();
        Ok(())
    }

    fn create_player_actor(&self, x: f64, z: f64) -> Actor {
        let profile_id = self
            .game_data
            .default_player_profile_id()
            .unwrap_or_else(|err| panic!("invalid default player profile: {err}"));
        let profile = self
            .game_data
            .profile(&profile_id)
            .unwrap_or_else(|| panic!("missing validated player profile {profile_id}"));
        Actor::new(
            crate::resolution::ResolutionActorId::new(ActorId::PLAYER.canonical()),
            "Adventurer".into(),
            self.player.faction.clone(),
            x,
            z,
            0,
            self.player.resources.hp,
            self.player.resources.mana,
            !self.dead,
            self.player.progression.clone(),
            profile.actions().to_vec(),
        )
    }

    fn synchronize_canonical_player(&mut self) {
        let (x, z) = self
            .player
            .canonical_actor
            .as_ref()
            .map_or((0.0, 0.0), Actor::position);
        let action_state = self
            .player
            .canonical_actor
            .as_ref()
            .map(|actor| actor.actions.clone());
        let mut actor = self.create_player_actor(x, z);
        if let Some(action_state) = action_state {
            actor.actions = action_state;
        }
        self.player.canonical_actor = Some(actor);
    }
    fn initialize_canonical_player(&mut self) {
        let data = &self.game_data;
        let profile_id = data
            .default_player_profile_id()
            .unwrap_or_else(|err| panic!("invalid default player profile: {err}"));
        let profile = data
            .profile(&profile_id)
            .unwrap_or_else(|| panic!("missing validated player profile {profile_id}"));
        self.player.faction = profile.faction().clone();
        self.player.progression = ActorProgression::from_profile(profile);
        self.player.resources.hp_max = self.player.progression.hp_max();
        self.player.resources.mana_max = self.player.progression.mana_max();
        self.player.resources.hp = self.player.resources.hp_max;
        self.player.resources.mana = self.player.resources.mana_max;
        self.player.canonical_actor = Some(self.create_player_actor(0.0, 0.0));
    }

    fn hydrate_hostile(&self, hostile: &mut WorldHostile) {
        let data = &self.game_data;
        let mob_id = MobId::new(hostile.mob_id.clone());
        let mob = data
            .mob(&mob_id)
            .unwrap_or_else(|| panic!("live hostile references unknown GameData mob {mob_id}"));
        hostile.name = mob.name().to_string();
        hostile.faction = mob.faction().clone();
        hostile.mode = mob.mode();
        hostile.movement_speed_mps = data
            .movement_by_id(mob.movement_id())
            .expect("validated mob movement")
            .speed_mps();
        let variance = mob.speed_variance_ratio().as_ratio();
        hostile.speed_multiplier =
            1.0 + deterministic_signed_unit(hostile.spawn_seed, 0) * variance;
        hostile.endurance_max_s = mob.endurance_s().seconds();
        hostile.endurance_s = hostile.endurance_max_s;
        hostile.progression = ActorProgression::from_mob(mob, data);
        hostile.actions = mob.actions().to_vec();
        hostile.canonical_actor = Some(Self::create_hostile_actor(hostile));
    }

    fn create_hostile_actor(hostile: &WorldHostile) -> Actor {
        Actor::new(
            crate::resolution::ResolutionActorId::new(hostile.actor_id.canonical),
            hostile.name.clone(),
            hostile.faction.clone(),
            hostile.x,
            hostile.z,
            hostile.armor,
            hostile.hp,
            hostile.progression.mana_max(),
            hostile.alive,
            hostile.progression.clone(),
            hostile.actions.clone(),
        )
    }

    fn hydrate_all_hostiles(&mut self) {
        let data = self.game_data.clone();
        for hostile in &mut self.hostiles {
            if !hostile.actions.is_empty() {
                continue;
            }
            let mob_id = MobId::new(hostile.mob_id.clone());
            let mob = data
                .mob(&mob_id)
                .unwrap_or_else(|| panic!("live hostile references unknown GameData mob {mob_id}"));
            hostile.name = mob.name().to_string();
            hostile.faction = mob.faction().clone();
            hostile.mode = mob.mode();
            hostile.movement_speed_mps = data
                .movement_by_id(mob.movement_id())
                .expect("validated mob movement")
                .speed_mps();
            hostile.progression = ActorProgression::from_mob(mob, &data);
            hostile.actions = mob.actions().to_vec();
            hostile.canonical_actor = Some(Self::create_hostile_actor(hostile));
        }
    }

    pub(crate) fn canonical_actors(&self, player_x: f64, player_z: f64) -> Vec<Actor> {
        let mut actors = Vec::with_capacity(self.hostiles.len() + 1);
        actors.push(
            self.player
                .canonical_actor
                .clone()
                .expect("canonical player initialized"),
        );
        actors[0].set_position(player_x, player_z);
        actors[0].set_hp(self.player.resources.hp);
        actors[0].set_mana(self.player.resources.mana);
        actors.extend(self.hostiles.iter().map(|hostile| {
            let mut actor = hostile
                .canonical_actor
                .clone()
                .unwrap_or_else(|| panic!("canonical actor missing for {}", hostile.mob_id));
            actor.set_position(hostile.x, hostile.z);
            actor.set_hp(hostile.hp);
            actor
        }));
        actors
    }

    pub(crate) fn sync_canonical_actors(&mut self, actors: Vec<Actor>, source: ActorId) {
        let mut iter = actors.into_iter();
        let player = iter.next().expect("canonical actor set contains player");
        self.player.resources.hp = player.hp();
        self.player.resources.hp_max = player.hp_max();
        self.player.resources.mana = player.mana();
        self.player.resources.mana_max = player.mana_max();
        self.player.progression = player.progression().clone();
        self.player.canonical_actor = Some(player.clone());
        if !player.is_alive() && !self.dead {
            self.dead = true;
            self.lock = None;
            self.slain_by.get_or_insert_with(|| "hostile".into());
            self.slain_hold_s = SLAIN_HOLD_S;
        }
        for (hostile, actor) in self.hostiles.iter_mut().zip(iter) {
            let took_damage = actor.hp() < hostile.hp;
            hostile.hp = actor.hp();
            hostile.progression = actor.progression().clone();
            hostile.faction = actor.effective_faction().clone();
            hostile.canonical_actor = Some(actor.clone());
            if took_damage && actor.is_alive() {
                hostile.provoked_by = Some(source);
                hostile.flee_threat = None;
                hostile.target = Some(source);
                hostile.awareness_s = AWARENESS_PERSIST_S;
                hostile.state = HostileState::Alerted;
            }
            if hostile.alive && !actor.is_alive() {
                hostile.alive = false;
                hostile.state = HostileState::Dead;
                if self.lock == Some(hostile.idx) {
                    self.lock = None;
                }
            }
        }
    }

    pub(crate) fn hostile_actor_index(&self, hostile_idx: i32) -> Option<usize> {
        self.hostiles
            .iter()
            .position(|hostile| hostile.idx == hostile_idx && hostile.alive)
            .map(|index| index + 1)
    }

    pub(crate) fn execute_canonical(
        &mut self,
        caster: usize,
        action_id: &ActionId,
        selection: TargetSelection,
        player_x: f64,
        player_z: f64,
    ) -> Result<Resolution, crate::resolution::ResolutionError> {
        self.hydrate_all_hostiles();
        let data = self.game_data.clone();
        let mut actors = self.canonical_actors(player_x, player_z);
        let result = Resolver::new(&data).execute(&mut actors, caster, action_id, selection)?;
        let source = if caster == 0 {
            ActorId::PLAYER
        } else {
            self.hostiles
                .get(caster - 1)
                .expect("resolver caster maps to live actor")
                .actor_id
        };
        self.sync_canonical_actors(actors, source);
        Ok(result)
    }
    pub fn hostiles(&self) -> &[WorldHostile] {
        &self.hostiles
    }
    pub fn hostiles_mut(&mut self) -> &mut [WorldHostile] {
        &mut self.hostiles
    }
    pub fn retain_hostiles(&mut self, mut keep: impl FnMut(&WorldHostile) -> bool) {
        self.hostiles.retain(|actor| keep(actor));
        if self
            .lock
            .is_some_and(|idx| !self.hostiles.iter().any(|actor| actor.idx == idx))
        {
            self.lock = None;
        }
    }
    pub fn lock_id(&self) -> Option<i32> {
        self.lock
    }
    pub fn is_dead(&self) -> bool {
        self.dead
    }
    pub fn slain_by(&self) -> Option<&str> {
        self.slain_by.as_deref()
    }
    pub fn slain_hold_s(&self) -> f64 {
        self.slain_hold_s
    }
    pub fn cast_time(&self) -> f64 {
        self.player
            .canonical_actor
            .as_ref()
            .and_then(|a| a.actions().cast())
            .map_or(0.0, |c| c.remaining_s())
    }
    pub fn casting_action_id(&self) -> Option<&ActionId> {
        self.player
            .canonical_actor
            .as_ref()?
            .actions()
            .cast()
            .map(|c| c.action_id())
    }
    pub(crate) fn log_mut(&mut self) -> &mut CombatLog {
        &mut self.log
    }
    pub(crate) fn fail_tell_timer(&self) -> f64 {
        self.fail_tell_s
    }
    pub(crate) fn fail_tell_value(&self) -> Option<&'static str> {
        self.fail_tell
    }
    pub(crate) fn set_fail_tell(&mut self, value: Option<&'static str>) {
        self.fail_tell = value;
    }
    pub fn log_lines(&self) -> Vec<String> {
        self.log.lines().map(str::to_string).collect()
    }
    pub fn set_lock(&mut self, lock: Option<i32>) {
        self.lock = lock;
    }
    pub fn clear_hostiles(&mut self) {
        self.hostiles.clear();
        self.lock = None;
        self.cycle.clear();
    }
    pub fn deactivate_actors(&mut self, ids: &[ActorId]) {
        if ids.contains(&ActorId::PLAYER) {
            panic!("player cannot be streaming-deactivated");
        }
        let deactivated_runtime_indices: Vec<i32> = self
            .hostiles
            .iter()
            .filter(|actor| ids.contains(&actor.actor_id))
            .map(|actor| actor.idx)
            .collect();
        self.hostiles.retain(|actor| !ids.contains(&actor.actor_id));
        if self
            .lock
            .is_some_and(|idx| deactivated_runtime_indices.contains(&idx))
        {
            self.lock = None;
        }
        self.cycle
            .retain(|idx| !deactivated_runtime_indices.contains(idx));
    }

    pub fn update_idle_actor_position(&mut self, id: ActorId, x: f64, z: f64) {
        let actor = self
            .hostiles
            .iter_mut()
            .find(|actor| actor.actor_id == id)
            .unwrap_or_else(|| panic!("idle fauna actor {id:?} missing from canonical arena"));
        if actor.is_engaged() {
            panic!("fauna attempted to move engaged actor {id:?}");
        }
        actor.x = x;
        actor.z = z;
        actor.previous_x = x;
        actor.previous_z = z;
    }
    pub fn next_actor_runtime_index(&self) -> i32 {
        self.hostiles
            .iter()
            .map(|actor| actor.idx)
            .max()
            .unwrap_or(-1)
            + 1
    }

    pub fn register_hostile(
        &mut self,
        mut hostile: WorldHostile,
        seed: SpawnSeed,
        heading: CanonicalHeading,
    ) -> ActorId {
        if self.hostiles.iter().any(|actor| actor.idx == hostile.idx) {
            panic!("duplicate hostile runtime index {}", hostile.idx);
        }
        if self.next_actor_id == 0 {
            panic!("canonical actor id space exhausted");
        }
        hostile.actor_id = ActorId::assigned(self.next_actor_id, hostile.idx);
        self.next_actor_id = self
            .next_actor_id
            .checked_add(1)
            .expect("canonical actor id space exhausted");
        hostile.spawn_seed = seed;
        hostile.heading = heading;
        hostile.previous_heading = heading;
        hostile.detection_left_s = deterministic_unit(seed, 1);
        self.hydrate_hostile(&mut hostile);
        let id = hostile.actor_id;
        self.hostiles.push(hostile);
        id
    }
    pub fn add_hostile(&mut self, hostile: WorldHostile) {
        let seed = SpawnSeed::new(stable_spawn_hash(
            hostile.idx,
            hostile.mob_id.as_bytes(),
            hostile.x,
            hostile.z,
        ));
        self.register_hostile(hostile, seed, CanonicalHeading::from_xz(1.0, 0.0));
    }

    pub fn canonical_hostile(
        &self,
        mob_id: &MobId,
        runtime_index: i32,
        x: f64,
        z: f64,
        home_x: f64,
        home_z: f64,
    ) -> WorldHostile {
        let mob = self
            .game_data
            .mob(mob_id)
            .unwrap_or_else(|| panic!("unknown combat mob {mob_id}"));
        let movement = self
            .game_data
            .movement_by_id(mob.movement_id())
            .expect("validated mob movement");
        WorldHostile::from_definition(
            runtime_index,
            x,
            z,
            mob,
            movement,
            &self.game_data,
            home_x,
            home_z,
        )
    }

    /// Seat one validated GameData mob in the canonical arena.
    pub fn add_canonical_mob(
        &mut self,
        mob_id: &MobId,
        runtime_index: i32,
        x: f64,
        z: f64,
        home_x: f64,
        home_z: f64,
    ) -> ActorId {
        let hostile = self.canonical_hostile(mob_id, runtime_index, x, z, home_x, home_z);
        let seed = SpawnSeed::new(stable_spawn_hash(
            runtime_index,
            mob_id.as_str().as_bytes(),
            x,
            z,
        ));
        self.register_hostile(hostile, seed, CanonicalHeading::from_xz(1.0, 0.0))
    }
    pub fn reset_for_encounter(&mut self) {
        let player = self.player.clone();
        let hostiles = self.hostiles.clone();
        self.player = player;
        self.hostiles = hostiles;
        self.hydrate_all_hostiles();
    }
    pub fn reset_encounter_state(&mut self) {
        self.lock = None;
        self.cycle.clear();
        self.fixed_accum_s = 0.0;
        self.pending_resolutions.clear();
    }
    /// Restore the player and transient combat state when a seated fixture is armed.
    pub fn reset_for_fixture_start(&mut self) {
        self.reset_encounter_state();
        self.player.resources.hp = self.player.resources.hp_max;
        self.player.resources.mana = self.player.resources.mana_max;
        self.player.shaken = None;
        self.dead = false;
        self.slain_by = None;
        self.slain_hold_s = 0.0;
        self.last_incoming = None;
        self.log = CombatLog::new();
        self.fail_tell = None;
        self.fail_tell_s = 0.0;
    }

    pub fn set_player_hp(&mut self, hp: f64) {
        self.player.resources.hp = hp;
    }
    pub fn set_fail_tell_timer(&mut self, seconds: f64) {
        self.fail_tell_s = seconds;
    }
    /// Consume frame time into authoritative simulation ticks.
    pub fn consume_fixed_steps(&mut self, dt: f64) -> usize {
        if dt <= 0.0 || self.dead {
            return 0;
        }
        self.fixed_accum_s += dt;
        let steps = ((self.fixed_accum_s + 1e-12) / TICK).floor() as usize;
        self.fixed_accum_s -= (steps as f64) * TICK;
        steps
    }

    /// Executes one authoritative simulation step in canonical phase order.
    pub fn step_fixed(
        &mut self,
        player_x: f64,
        player_z: f64,
        _facing_x: f64,
        _facing_z: f64,
    ) -> CombatStep {
        if let Some(shaken) = self.player.shaken.as_mut() {
            shaken.tick(TICK);
            if shaken.remaining_s <= 0.0 {
                self.player.shaken = None;
            }
        }
        self.tick_hostile_ai(player_x, player_z, TICK);
        self.tick_player_actions(player_x, player_z, TICK);
        self.tick_incoming(player_x, player_z, TICK);
        CombatStep {
            resolutions: std::mem::take(&mut self.pending_resolutions),
        }
    }

    pub fn presentation_alpha(&self) -> f64 {
        (self.fixed_accum_s / TICK).clamp(0.0, 1.0)
    }

    pub fn reset_fixed_clock(&mut self) {
        self.fixed_accum_s = 0.0;
    }

    pub fn with_game_data(game_data: std::sync::Arc<crate::gamedata::GameData>) -> Self {
        let profile_id = game_data
            .default_player_profile_id()
            .unwrap_or_else(|err| panic!("invalid default player profile: {err}"));
        let player = LivePlayer::from_profile(
            game_data
                .profile(&profile_id)
                .unwrap_or_else(|| panic!("missing validated player profile {profile_id}")),
        );
        let mut combat = Self {
            game_data,
            lock: None,
            player,
            cycle: Vec::new(),
            hostiles: Vec::new(),
            next_actor_id: 1,
            dead: false,
            slain_by: None,
            slain_hold_s: 0.0,
            last_incoming: None,
            log: CombatLog::new(),
            fail_tell: None,
            fail_tell_s: 0.0,
            fixed_accum_s: 0.0,
            pending_resolutions: Vec::new(),
        };
        combat.initialize_canonical_player();
        combat
    }
    fn hostile_pairs(&self) -> Vec<(i32, f64, f64)> {
        self.hostiles
            .iter()
            .filter(|h| {
                h.alive
                    && self
                        .game_data
                        .factions_are_hostile(&self.player.faction, &h.faction)
            })
            .map(|h| (h.idx, h.x, h.z))
            .collect()
    }

    /// Tab: nearest hostile in 20 m / 90° cone; repeat cycles; off after last.
    pub fn press_tab(&mut self, player_x: f64, player_z: f64, facing_x: f64, facing_z: f64) {
        let ids = tab_candidates(
            player_x,
            player_z,
            facing_x,
            facing_z,
            &self.hostile_pairs(),
        );
        if ids.is_empty() {
            self.lock = None;
            self.cycle.clear();
            return;
        }
        if self.cycle != ids {
            self.cycle = ids;
            self.lock = Some(self.cycle[0]);
            return;
        }
        match self
            .lock
            .and_then(|cur| self.cycle.iter().position(|&id| id == cur))
        {
            Some(i) if i + 1 < self.cycle.len() => self.lock = Some(self.cycle[i + 1]),
            Some(_) => self.lock = None,
            None => self.lock = Some(self.cycle[0]),
        }
    }

    /// Click body: lock nearest in the same 20 m / 90° cone.
    pub fn click_lock(&mut self, player_x: f64, player_z: f64, facing_x: f64, facing_z: f64) {
        let ids = tab_candidates(
            player_x,
            player_z,
            facing_x,
            facing_z,
            &self.hostile_pairs(),
        );
        self.lock = ids.first().copied();
        self.cycle = ids;
    }

    /// After the slain hold, restore resources and apply the visible death debuff.
    pub fn finish_death_respawn(&mut self) {
        self.player.shaken = Some(Shaken::from_death());
        self.player.resources.hp = self.player.resources.hp_max;
        self.player.resources.mana = self.player.resources.mana_max;
        self.dead = false;
        self.synchronize_canonical_player();
        self.lock = None;
        self.slain_hold_s = 0.0;
        self.slain_by = None;
    }

    /// Applies already-mitigated hostile damage and performs the complete hostile transition.
    /// Every live hostile damage source must use this operation.
    /// Applies already-mitigated damage to the player and performs death transition.
    /// Applies already-mitigated damage to the player and performs death transition.
    pub fn apply_damage_to_player(&mut self, dealt: i32, by: String) -> IncomingHit {
        if dealt < 0 {
            panic!("negative player damage from {by}: {dealt}");
        }
        if self.dead {
            panic!("damage reported while player is dead");
        }
        self.player.resources.hp = (self.player.resources.hp - f64::from(dealt)).max(0.0);
        let killed = self.player.resources.hp <= 0.0;
        let hit = IncomingHit {
            dealt,
            by: by.clone(),
            killed,
        };
        self.last_incoming = Some(hit.clone());
        if killed {
            self.dead = true;
            self.lock = None;
            self.slain_by = Some(by);
            self.slain_hold_s = SLAIN_HOLD_S;
        }
        hit
    }
    pub fn apply_damage_to_hostile(&mut self, idx: i32, dealt: i32) -> bool {
        if dealt < 0 {
            panic!("negative hostile damage {dealt} for {idx}");
        }
        let Some(hi) = self.hostiles.iter().position(|h| h.idx == idx && h.alive) else {
            panic!("damage reported for missing or dead hostile {idx}");
        };
        self.hostiles[hi].hp = (self.hostiles[hi].hp - f64::from(dealt)).max(0.0);
        self.set_retaliation(hi, ActorId::PLAYER);
        if self.hostiles[hi].hp <= 0.0 {
            self.defeat_hostile(idx);
            true
        } else {
            false
        }
    }
    pub fn hostile_took_damage(&mut self, idx: i32) {
        let Some(h) = self.hostiles.iter_mut().find(|h| h.idx == idx && h.alive) else {
            panic!("damage reported for missing or dead hostile {idx}");
        };
        h.flee_threat = None;
        h.provoked_by = Some(ActorId::PLAYER);
        h.target = Some(ActorId::PLAYER);
        h.awareness_s = AWARENESS_PERSIST_S;
        h.state = HostileState::Alerted;
    }
    fn set_retaliation(&mut self, index: usize, source: ActorId) {
        let h = &mut self.hostiles[index];
        h.flee_threat = None;
        h.provoked_by = Some(source);
        h.target = Some(source);
        h.awareness_s = AWARENESS_PERSIST_S;
        h.state = HostileState::Alerted;
    }

    pub fn defeat_hostile(&mut self, idx: i32) {
        let Some(hi) = self.hostiles.iter().position(|h| h.idx == idx && h.alive) else {
            panic!("defeat reported for missing or dead hostile {idx}");
        };
        self.hostiles[hi].hp = 0.0;
        self.hostiles[hi].alive = false;
        self.hostiles[hi].state = HostileState::Dead;
        if self.lock == Some(idx) {
            self.lock = None;
        }
    }

    fn usable_hostile_actions<'a>(
        &'a self,
        hostile: &'a WorldHostile,
    ) -> impl Iterator<Item = (ActionId, f64)> + 'a {
        let runtime = hostile.canonical_actor.as_ref();
        hostile.actions.iter().filter_map(move |id| {
            let action = self.game_data.action(id).unwrap_or_else(|| {
                panic!("actor {} references unknown action {id}", hostile.mob_id)
            });
            if !matches!(action.target(), ActionTarget::Hostile | ActionTarget::Any)
                || runtime.is_some_and(|actor| actor.actions().cooldown_s(id) > 0.0)
            {
                return None;
            }
            let range = action
                .effects()
                .iter()
                .map(crate::gamedata::ActionEffect::range_m)
                .max_by(f64::total_cmp)
                .unwrap_or_else(|| panic!("action {id} has no authored geometry"));
            Some((id.clone(), range))
        })
    }

    fn selected_usable_action(
        &self,
        hostile: &WorldHostile,
        target_distance: f64,
    ) -> Option<(ActionId, f64)> {
        self.usable_hostile_actions(hostile)
            .find(|(_, range)| target_distance <= *range)
    }

    fn pursuit_action(&self, hostile: &WorldHostile) -> Option<(ActionId, f64)> {
        self.usable_hostile_actions(hostile)
            .max_by(|left, right| left.1.total_cmp(&right.1))
    }

    /// Advances deterministic perception, flight, pursuit, endurance, and leash reset.
    pub fn tick_hostile_ai(&mut self, player_x: f64, player_z: f64, dt: f64) {
        if self.dead || dt <= 0.0 {
            return;
        }
        self.hydrate_all_hostiles();
        let data = &self.game_data;
        let player = (
            ActorId::PLAYER,
            self.player.faction.clone(),
            player_x,
            player_z,
            true,
        );
        let actors: Vec<(ActorId, FactionId, MobMode, f64, f64, bool)> = self
            .hostiles
            .iter()
            .map(|a| (a.actor_id, a.faction.clone(), a.mode, a.x, a.z, a.alive))
            .collect();

        let pursuit_actions: std::collections::HashMap<ActorId, Option<(ActionId, f64)>> = self
            .hostiles
            .iter()
            .map(|hostile| (hostile.actor_id, self.pursuit_action(hostile)))
            .collect();
        for actor in &mut self.hostiles {
            actor.previous_x = actor.x;
            actor.previous_z = actor.z;
            actor.previous_heading = actor.heading;
            if !actor.is_alive() {
                actor.state = HostileState::Dead;
                actor.target = None;
                actor.flee_threat = None;
                continue;
            }
            let home_distance = (actor.x - actor.home_x).hypot(actor.z - actor.home_z);
            if home_distance > actor.aggro.leash_m {
                actor.state = HostileState::Leashing;
                actor.target = None;
                actor.flee_threat = None;
                actor.awareness_s = 0.0;
            }
            if actor.state == HostileState::Leashing {
                actor.endurance_s = (actor.endurance_s + dt).min(actor.endurance_max_s);
                let dx = actor.home_x - actor.x;
                let dz = actor.home_z - actor.z;
                let distance = dx.hypot(dz);
                if distance <= 0.05 {
                    actor.x = actor.home_x;
                    actor.z = actor.home_z;
                    actor.provoked_by = None;
                    actor.state = HostileState::Idle;
                } else if actor.canonical_actor.as_ref().is_none_or(Actor::can_move) {
                    let step =
                        (actor.movement_speed_mps * actor.speed_multiplier * dt).min(distance);
                    actor.heading = CanonicalHeading::from_xz(dx, dz);
                    actor.x += dx / distance * step;
                    actor.z += dz / distance * step;
                }
                continue;
            }
            let hostile_to = |faction: &FactionId| {
                {
                    !data
                        .faction(&actor.faction)
                        .expect("validated observer faction")
                        .is_neutral()
                        && !data
                            .faction(faction)
                            .expect("validated candidate faction")
                            .is_neutral()
                        && data.factions_are_hostile(&actor.faction, faction)
                }
            };
            let locate = |id: ActorId| -> Option<(f64, f64)> {
                if id == ActorId::PLAYER {
                    return (player.4 && hostile_to(&player.1)).then_some((player.2, player.3));
                }
                actors
                    .iter()
                    .find(|c| c.0 == id && c.5 && hostile_to(&c.1))
                    .map(|c| (c.3, c.4))
            };

            actor.awareness_s = (actor.awareness_s - dt).max(0.0);
            actor.detection_left_s -= dt;
            if actor.detection_left_s <= 0.0 {
                while actor.detection_left_s <= 0.0 {
                    actor.detection_left_s += DETECTION_INTERVAL_S;
                }
                let mut candidates: Vec<(ActorId, MobMode, f64, f64, f64)> = Vec::new();
                if hostile_to(&player.1) {
                    candidates.push((
                        player.0,
                        MobMode::Active,
                        player.2,
                        player.3,
                        (player.2 - actor.x).hypot(player.3 - actor.z),
                    ));
                }
                candidates.extend(
                    actors
                        .iter()
                        .filter(|c| c.0 != actor.actor_id && c.5 && hostile_to(&c.1))
                        .map(|c| (c.0, c.2, c.3, c.4, (c.3 - actor.x).hypot(c.4 - actor.z))),
                );
                candidates.retain(|(_, candidate_mode, _, _, _)| {
                    actor.mode == MobMode::Active || *candidate_mode == MobMode::Active
                });
                candidates.sort_by(|a, b| a.4.total_cmp(&b.4).then_with(|| a.0.cmp(&b.0)));
                for (id, mode, x, z, _) in candidates {
                    let probability = detection_probability(actor, x, z);
                    let draw = deterministic_unit(actor.spawn_seed, 2 + actor.detection_check);
                    actor.detection_check += 1;
                    if draw >= probability {
                        continue;
                    }
                    actor.awareness_s = AWARENESS_PERSIST_S;
                    match actor.mode {
                        MobMode::Active => {
                            actor.target = Some(id);
                            actor.flee_threat = None;
                            actor.state = HostileState::Alerted;
                        }
                        MobMode::Passive if mode == MobMode::Active => {
                            // A damaged passive actor has committed to retaliation.
                            // Ambient prey perception must not overwrite that combat state.
                            if actor.provoked_by.is_none() {
                                actor.flee_threat = Some(id);
                                actor.target = None;
                                actor.state = HostileState::Fleeing;
                            }
                        }
                        MobMode::Passive => {}
                    }
                    break;
                }
            }
            if actor.awareness_s <= 0.0 && actor.provoked_by.is_none() {
                actor.target = None;
                actor.flee_threat = None;
                actor.state = HostileState::Idle;
            }

            let sprinting = match actor.state {
                HostileState::Fleeing => {
                    let Some((tx, tz)) = actor.flee_threat.and_then(locate) else {
                        actor.flee_threat = None;
                        actor.state = HostileState::Idle;
                        continue;
                    };
                    if actor
                        .canonical_actor
                        .as_ref()
                        .is_some_and(|a| !a.can_move())
                    {
                        false
                    } else {
                        let dx = actor.x - tx;
                        let dz = actor.z - tz;
                        let distance = dx.hypot(dz);
                        if distance <= 1e-9 {
                            false
                        } else {
                            let speed = movement_speed(actor);
                            let step = speed * dt;
                            actor.heading = CanonicalHeading::from_xz(dx, dz);
                            actor.x += dx / distance * step;
                            actor.z += dz / distance * step;
                            step > 0.0
                        }
                    }
                }
                HostileState::Alerted | HostileState::Pursuing | HostileState::Attacking => {
                    let Some((tx, tz)) = actor.target.and_then(locate) else {
                        actor.target = None;
                        actor.provoked_by = None;
                        actor.state = HostileState::Idle;
                        continue;
                    };
                    let dx = tx - actor.x;
                    let dz = tz - actor.z;
                    let distance = dx.hypot(dz);
                    let Some((_, action_range)) = pursuit_actions
                        .get(&actor.actor_id)
                        .expect("pursuit action computed for every hostile")
                    else {
                        actor.state = HostileState::Attacking;
                        continue;
                    };
                    if distance <= *action_range {
                        actor.state = HostileState::Attacking;
                        false
                    } else if actor.canonical_actor.as_ref().is_none_or(Actor::can_move) {
                        actor.state = HostileState::Pursuing;
                        let speed = movement_speed(actor);
                        let step = (speed * dt).min((distance - *action_range).max(0.0));
                        if step > 0.0 {
                            actor.heading = CanonicalHeading::from_xz(dx, dz);
                            actor.x += dx / distance * step;
                            actor.z += dz / distance * step;
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                }
                _ => false,
            };
            if sprinting {
                actor.endurance_s = (actor.endurance_s - dt).max(0.0);
            } else {
                actor.endurance_s = (actor.endurance_s + dt).min(actor.endurance_max_s);
            }
        }
    }

    pub fn in_combat(&self) -> bool {
        self.lock.is_some() || self.hostiles.iter().any(|h| h.alive)
    }

    fn tick_canonical_actor_attack(
        &mut self,
        player_x: f64,
        player_z: f64,
        dt: f64,
    ) -> Option<IncomingHit> {
        if self.dead || dt <= 0.0 {
            return None;
        }
        self.hydrate_all_hostiles();
        for hostile in &mut self.hostiles {
            hostile
                .canonical_actor
                .as_mut()
                .unwrap_or_else(|| panic!("canonical actor missing for {}", hostile.mob_id))
                .tick_runtime(dt);
        }
        let selected_actions: std::collections::HashMap<ActorId, Option<ActionId>> = self
            .hostiles
            .iter()
            .map(|hostile| {
                (
                    hostile.actor_id,
                    hostile
                        .target
                        .map(|target| {
                            let (target_x, target_z) = if target == ActorId::PLAYER {
                                (player_x, player_z)
                            } else {
                                self.hostiles
                                    .iter()
                                    .find(|candidate| {
                                        candidate.actor_id == target && candidate.alive
                                    })
                                    .map(|candidate| (candidate.x, candidate.z))
                                    .unwrap_or_else(|| {
                                        panic!(
                                            "actor {:?} targets missing actor {target:?}",
                                            hostile.actor_id
                                        )
                                    })
                            };
                            (target_x - hostile.x).hypot(target_z - hostile.z)
                        })
                        .and_then(|distance| self.selected_usable_action(hostile, distance))
                        .map(|selected| selected.0),
                )
            })
            .collect();
        let mut completed = None;
        for hostile in &mut self.hostiles {
            if !hostile.alive || hostile.state != HostileState::Attacking {
                continue;
            }
            let target_id = hostile
                .target
                .unwrap_or_else(|| panic!("attacking actor {} has no target", hostile.mob_id));
            let actor = hostile
                .canonical_actor
                .as_mut()
                .unwrap_or_else(|| panic!("canonical actor missing for {}", hostile.mob_id));
            if let Some(cast) = actor.actions_mut().take_completed_cast() {
                completed = Some((
                    hostile.actor_id,
                    hostile.name.clone(),
                    target_id,
                    cast.action_id().clone(),
                ));
                break;
            }
            if actor.actions().cast().is_some() || !actor.can_act() {
                continue;
            }
            let Some(action_id) = selected_actions
                .get(&hostile.actor_id)
                .expect("selected action computed for every hostile")
                .clone()
            else {
                continue;
            };
            let action = self.game_data.action(&action_id).unwrap_or_else(|| {
                panic!(
                    "actor {} references unknown action {action_id}",
                    hostile.mob_id
                )
            });
            let target = Some(crate::resolution::ResolutionActorId::new(
                target_id.canonical(),
            ));
            if action.cast_s() > 0.0 {
                actor
                    .actions_mut()
                    .start_cast(action_id, target, action.cast_s());
                continue;
            }
            completed = Some((hostile.actor_id, hostile.name.clone(), target_id, action_id));
            break;
        }
        let (caster_id, name, target_id, action_id) = completed?;
        let caster = self
            .hostiles
            .iter()
            .position(|actor| actor.actor_id == caster_id && actor.alive)
            .map(|index| index + 1)
            .unwrap_or_else(|| panic!("completed action caster {caster_id:?} is missing"));
        let target = if target_id == ActorId::PLAYER {
            0
        } else {
            self.hostiles
                .iter()
                .position(|actor| actor.actor_id == target_id && actor.alive)
                .map(|index| index + 1)
                .unwrap_or_else(|| {
                    panic!("actor {caster_id:?} targets missing actor {target_id:?}")
                })
        };
        let previous_player_hp = self.player.resources.hp;
        match self.execute_canonical(
            caster,
            &action_id,
            TargetSelection::Single(target),
            player_x,
            player_z,
        ) {
            Ok(resolution) => {
                let killed_player = target_id == ActorId::PLAYER && self.dead;
                if killed_player {
                    self.slain_by = Some(name.clone());
                }
                let cooldown = self
                    .game_data
                    .action(&action_id)
                    .expect("validated mob action")
                    .cooldown_s();
                if cooldown > 0.0 {
                    self.hostiles
                        .iter_mut()
                        .find(|hostile| hostile.actor_id == caster_id)
                        .unwrap_or_else(|| {
                            panic!("resolved action caster {caster_id:?} disappeared")
                        })
                        .canonical_actor
                        .as_mut()
                        .expect("canonical hostile actor")
                        .actions_mut()
                        .start_cooldown(action_id.clone(), cooldown);
                }
                self.pending_resolutions.push(resolution);
                (target_id == ActorId::PLAYER).then(|| IncomingHit {
                    dealt: (previous_player_hp - self.player.resources.hp).round() as i32,
                    by: name,
                    killed: killed_player,
                })
            }
            Err(crate::resolution::ResolutionError::OutOfRange(_)) => None,
            Err(err) => panic!("canonical actor action {action_id} failed: {err}"),
        }
    }
    pub fn tick_incoming(&mut self, player_x: f64, player_z: f64, dt: f64) -> Option<IncomingHit> {
        self.tick_canonical_actor_attack(player_x, player_z, dt)
    }
}

#[cfg(test)]
mod live_action_tests {
    use super::*;

    fn canonical_data() -> std::sync::Arc<crate::gamedata::GameData> {
        std::sync::Arc::new(
            crate::gamedata::GameData::load(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../data/OrrunGameData.xml"),
            )
            .expect("canonical GameData"),
        )
    }

    #[test]
    fn fresh_combat_immediately_exposes_player_roster_and_cooldowns() {
        let data = canonical_data();
        let expected_roster = data
            .profile(
                &data
                    .default_player_profile_id()
                    .expect("default player profile id"),
            )
            .expect("default player profile")
            .actions()
            .to_vec();
        let combat = WorldCombat::with_game_data(data);

        assert_eq!(combat.player_action_roster(), expected_roster);
        for action_id in &expected_roster {
            assert_eq!(combat.player_action_cooldown_s(action_id), 0.0);
        }
        let actor = combat
            .player
            .canonical_actor
            .as_ref()
            .expect("fresh canonical player");
        assert_eq!(actor.id().get(), 0);
        assert_eq!(actor.position(), (0.0, 0.0));
    }

    #[test]
    fn restored_save_updates_canonical_player_without_dropping_action_state() {
        let data = canonical_data();
        let mut combat = WorldCombat::with_game_data(data);
        let action_id = combat.player_action_roster()[0].clone();
        combat
            .player
            .canonical_actor
            .as_mut()
            .expect("fresh canonical player")
            .actions_mut()
            .start_cooldown(action_id.clone(), 3.0);

        let initial = combat.player_progression().export_snapshot();
        let progression = crate::progression::ActorProgressionSnapshot::new(
            initial
                .skills()
                .iter()
                .map(|skill| {
                    crate::progression::SkillProgressionSnapshot::new(
                        skill.skill_id(),
                        skill.track(),
                    )
                })
                .collect(),
            crate::progression::ProgressionTrackSnapshot::new(2, 9),
            crate::progression::ProgressionTrackSnapshot::new(2, 7),
        );
        let snapshot = PlayerSaveSnapshot::new(progression, 73.0, 41.0);
        combat
            .restore_player_save(&snapshot)
            .expect("restore player save");

        let actor = combat
            .player
            .canonical_actor
            .as_ref()
            .expect("restored canonical player");
        assert_eq!(actor.hp(), 73.0);
        assert_eq!(actor.mana(), 41.0);
        assert_eq!(actor.progression(), combat.player_progression());
        assert_eq!(combat.player.resources.hp_max(), actor.hp_max());
        assert_eq!(combat.player.resources.mana_max(), actor.mana_max());
        assert_eq!(combat.player_action_cooldown_s(&action_id), 3.0);
    }
    #[test]
    fn seated_wolf_hydrates_before_ai_attack_and_repeats_after_cooldown() {
        let data = canonical_data();
        let mut combat = WorldCombat::with_game_data(data);
        combat.add_canonical_mob(&MobId::new("wolf"), 0, 1.5, 0.0, 1.5, 0.0);

        assert!(combat.hostiles[0].canonical_actor.is_some());
        assert_eq!(combat.hostiles[0].state, HostileState::Idle);

        let hp_before = combat.player.resources.hp();
        let mut hit_ticks = Vec::new();
        for tick in 0..40 {
            let step = combat.step_fixed(0.0, 0.0, 1.0, 0.0);
            if !step.resolutions.is_empty() {
                hit_ticks.push(tick);
            }
        }

        assert_eq!(hit_ticks, vec![15, 26, 37]);
        assert!(combat.player.resources.hp() < hp_before);
        assert_eq!(combat.hostiles[0].state, HostileState::Attacking);
        assert_eq!(
            combat.hostiles[0]
                .canonical_actor
                .as_ref()
                .expect("seated wolf actor")
                .actions()
                .cooldown_s(&ActionId::new("slash")),
            0.8
        );
    }

    #[test]
    fn damaged_passive_deer_retaliates_repeatedly_without_losing_actor_identity() {
        let data = canonical_data();
        let mut combat = WorldCombat::with_game_data(data);
        let deer_id = combat.add_canonical_mob(&MobId::new("deer"), 41, 1.5, 0.0, 1.5, 0.0);
        combat.set_lock(Some(41));
        assert!(combat.press_action(&ActionId::new("arrow"), 0.0, 0.0, 1.0, 0.0));
        for _ in 0..4 {
            combat.step_fixed(0.0, 0.0, 1.0, 0.0);
        }
        let deer = combat
            .hostiles
            .iter()
            .find(|actor| actor.actor_id == deer_id)
            .expect("deer retained");
        assert_eq!(deer.hp(), 47.0);
        assert!(deer.is_alive());
        assert_eq!(deer.target(), Some(ActorId::PLAYER));

        let hp_before = combat.player.resources.hp();
        let mut deer_hits = 0;
        for _ in 0..50 {
            let step = combat.step_fixed(0.0, 0.0, 1.0, 0.0);
            deer_hits += step
                .resolutions
                .iter()
                .filter(|resolution| resolution.caster.get() == deer_id.canonical())
                .count();
        }
        assert!(
            deer_hits >= 2,
            "deer only completed {deer_hits} retaliatory attacks"
        );
        assert!(combat.player.resources.hp() < hp_before);
        assert_eq!(
            combat
                .hostiles
                .iter()
                .find(|actor| actor.actor_id == deer_id)
                .expect("deer retained")
                .idx,
            41
        );
    }

    #[test]
    fn mob_selects_second_in_range_action_and_respects_cast_time() {
        let data = canonical_data();
        let mut combat = WorldCombat::with_game_data(data);
        combat.add_canonical_mob(&MobId::new("skeleton_mage"), 0, 10.0, 0.0, 10.0, 0.0);
        let actors = combat.canonical_actors(0.0, 0.0);
        combat.sync_canonical_actors(actors, ActorId::PLAYER);
        let hostile = &mut combat.hostiles[0];
        hostile.state = HostileState::Attacking;
        hostile.target = Some(ActorId::PLAYER);

        let hp_before = combat.player.resources.hp();
        assert!(combat.tick_incoming(0.0, 0.0, TICK).is_none());
        let cast = combat.hostiles[0]
            .canonical_actor
            .as_ref()
            .expect("canonical hostile")
            .actions()
            .cast()
            .expect("second action should stage a cast");
        assert_eq!(cast.action_id(), &ActionId::new("grave_lance"));
        assert!(cast.remaining_s() > 0.0);
        assert_eq!(combat.player.resources.hp(), hp_before);

        let mut elapsed = 0.0;
        let hit = loop {
            match combat.tick_incoming(0.0, 0.0, TICK) {
                Some(hit) => break hit,
                None => {
                    assert_eq!(combat.player.resources.hp(), hp_before);
                    elapsed += TICK;
                    assert!(elapsed <= 1.2, "authored cast did not complete");
                }
            }
        };
        assert!(
            elapsed + 1e-9 >= 1.0,
            "damage landed before the 1.1 s authored cast"
        );
        assert!(hit.dealt > 0);
        assert!(combat.player.resources.hp() < hp_before);
    }

    #[test]
    fn mob_skips_first_action_while_it_is_on_cooldown() {
        let data = canonical_data();
        let mut combat = WorldCombat::with_game_data(data);
        combat.add_canonical_mob(&MobId::new("blue_demon"), 0, 2.0, 0.0, 2.0, 0.0);
        let actors = combat.canonical_actors(0.0, 0.0);
        combat.sync_canonical_actors(actors, ActorId::PLAYER);
        let hostile = &mut combat.hostiles[0];
        hostile.state = HostileState::Attacking;
        hostile.target = Some(ActorId::PLAYER);
        hostile
            .canonical_actor
            .as_mut()
            .expect("canonical hostile")
            .actions_mut()
            .start_cooldown(ActionId::new("hellbrand"), 2.0);

        assert!(combat.tick_incoming(0.0, 0.0, TICK).is_none());
        let cast = combat.hostiles[0]
            .canonical_actor
            .as_ref()
            .expect("canonical hostile")
            .actions()
            .cast()
            .expect("second action should stage while first is cooling down");
        assert_eq!(cast.action_id(), &ActionId::new("azure_flare"));
    }
}
