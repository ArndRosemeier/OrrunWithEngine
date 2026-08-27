//! Live combat state driven by canonical action resolution.

use crate::gamedata::EffectOperation;

use thiserror::Error;

use super::log::CombatLog;
use super::math::*;
use crate::gamedata::{
    ActionId, ActionTarget, FactionId, MobDefinition, MobId, MobMode, MovementSpec, PlayerProfile,
};
use crate::progression::{ActorProgression, ActorProgressionSnapshot, ProgressionError};
use crate::resolution::{Actor, Resolution, Resolver, TargetSelection};
use engine::world::EntityId;

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
    pub lock: Option<i32>,
    pub shaken: Option<Shaken>,
    pub sprinted: bool,
    pub used_pin_or_bind: bool,
}

impl LivePlayer {
    fn from_profile(_profile: &PlayerProfile) -> Self {
        Self {
            lock: None,
            shaken: None,
            sprinted: false,
            used_pin_or_bind: false,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ActorPresentation {
    source: HostilePresentationSource,
    entity: EntityId,
}
impl ActorPresentation {
    pub const fn source(self) -> HostilePresentationSource {
        self.source
    }
    pub const fn entity(self) -> EntityId {
        self.entity
    }
}
#[derive(Clone, Debug, Default)]
pub struct ActorPresentationRegistry {
    bindings: std::collections::BTreeMap<ActorId, ActorPresentation>,
}
impl ActorPresentationRegistry {
    pub fn bind(&mut self, actor: ActorId, source: HostilePresentationSource, entity: EntityId) {
        assert_ne!(
            source,
            HostilePresentationSource::Headless,
            "visible entity cannot be headless"
        );
        assert!(
            self.bindings
                .insert(actor, ActorPresentation { source, entity })
                .is_none(),
            "actor {actor:?} presentation already bound"
        );
    }
    pub fn get(&self, actor: ActorId) -> Option<ActorPresentation> {
        self.bindings.get(&actor).copied()
    }
    pub fn unbind(&mut self, actor: ActorId, source: HostilePresentationSource) -> EntityId {
        let binding = self
            .bindings
            .remove(&actor)
            .unwrap_or_else(|| panic!("actor {actor:?} presentation missing"));
        assert_eq!(
            binding.source, source,
            "actor {actor:?} presentation owner mismatch"
        );
        binding.entity
    }
    pub fn remove_source(&mut self, source: HostilePresentationSource) -> Vec<EntityId> {
        let actors: Vec<ActorId> = self
            .bindings
            .iter()
            .filter(|(_, b)| b.source == source)
            .map(|(id, _)| *id)
            .collect();
        actors
            .into_iter()
            .map(|id| self.bindings.remove(&id).expect("selected binding").entity)
            .collect()
    }
    pub fn clear(&mut self) {
        self.bindings.clear();
    }
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
    pub armor: i32,
    /// Display name for the lock tell. Fixture wolves use "wolf-spider".
    pub name: String,
    /// Sheet / catalog id (orc, tribal, orc_skull, wolf).
    pub mob_id: String,
    pub home_x: f64,
    pub home_z: f64,
    pub aggro: Aggro,
    pub state: HostileState,
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
}

#[derive(Clone, Debug)]
pub struct IncomingHit {
    pub dealt: i32,
    pub by: String,
    pub killed: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CombatHudAction {
    id: ActionId,
    name: String,
    cooldown_fraction: f32,
}
impl CombatHudAction {
    pub fn id(&self) -> &ActionId {
        &self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn cooldown_fraction(&self) -> f32 {
        self.cooldown_fraction
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct CombatHudActor {
    id: ActorId,
    name: String,
    hp: f64,
    hp_max: f64,
    head_x: f64,
    head_y: f64,
    head_z: f64,
    alive: bool,
}
impl CombatHudActor {
    pub fn id(&self) -> ActorId {
        self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn hp(&self) -> f64 {
        self.hp
    }
    pub fn hp_max(&self) -> f64 {
        self.hp_max
    }
    pub fn head_position(&self) -> (f64, f64, f64) {
        (self.head_x, self.head_y, self.head_z)
    }
    pub fn is_alive(&self) -> bool {
        self.alive
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct CombatHudProgression {
    label: String,
    level: i32,
    xp: u64,
    xp_total: u64,
}
impl CombatHudProgression {
    pub fn label(&self) -> &str {
        &self.label
    }
    pub fn level(&self) -> i32 {
        self.level
    }
    pub fn xp(&self) -> u64 {
        self.xp
    }
    pub fn xp_total(&self) -> u64 {
        self.xp_total
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct CombatHudSnapshot {
    hp: f64,
    hp_max: f64,
    mana: f64,
    mana_max: f64,
    dead: bool,
    lock: Option<ActorId>,
    actors: Vec<CombatHudActor>,
    actions: Vec<CombatHudAction>,
    cast_label: Option<String>,
    cast_fraction: Option<f32>,
    log_lines: Vec<String>,
    fail_tell: Option<&'static str>,
    attack_pip: bool,
    hurt_flash: bool,
    hp_ghost_fraction: Option<f32>,
    shaken: bool,
    slain_line: Option<String>,
    progression: Vec<CombatHudProgression>,
}
impl CombatHudSnapshot {
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
    pub fn is_dead(&self) -> bool {
        self.dead
    }
    pub fn lock(&self) -> Option<ActorId> {
        self.lock
    }
    pub fn actors(&self) -> &[CombatHudActor] {
        &self.actors
    }
    pub fn actions(&self) -> &[CombatHudAction] {
        &self.actions
    }
    pub fn locked_actor(&self) -> Option<&CombatHudActor> {
        let id = self.lock?;
        self.actors.iter().find(|a| a.id == id)
    }
    pub fn cast_label(&self) -> Option<&str> {
        self.cast_label.as_deref()
    }
    pub fn cast_fraction(&self) -> Option<f32> {
        self.cast_fraction
    }
    pub fn log_lines(&self) -> &[String] {
        &self.log_lines
    }
    pub fn fail_tell(&self) -> Option<&'static str> {
        self.fail_tell
    }
    pub fn attack_pip(&self) -> bool {
        self.attack_pip
    }
    pub fn hurt_flash(&self) -> bool {
        self.hurt_flash
    }
    pub fn hp_ghost_fraction(&self) -> Option<f32> {
        self.hp_ghost_fraction
    }
    pub fn is_shaken(&self) -> bool {
        self.shaken
    }
    pub fn slain_line(&self) -> Option<&str> {
        self.slain_line.as_deref()
    }
    pub fn progression(&self) -> &[CombatHudProgression] {
        &self.progression
    }
    pub(crate) fn with_head_positions(
        mut self,
        positions: &std::collections::BTreeMap<ActorId, (f64, f64, f64)>,
    ) -> Self {
        assert_eq!(
            positions.len(),
            self.actors.len(),
            "HUD head anchor cardinality differs from actor snapshot"
        );
        for actor in &mut self.actors {
            let position = positions.get(&actor.id).unwrap_or_else(|| {
                panic!(
                    "HUD snapshot missing projected head anchor for actor {:?}",
                    actor.id
                )
            });
            actor.head_x = position.0;
            actor.head_y = position.1;
            actor.head_z = position.2;
        }
        self
    }
    pub(crate) fn with_presentation(
        mut self,
        attack_pip: bool,
        hurt_flash: bool,
        hp_ghost_fraction: Option<f32>,
    ) -> Self {
        self.attack_pip = attack_pip;
        self.hurt_flash = hurt_flash;
        self.hp_ghost_fraction = hp_ghost_fraction;
        self
    }
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
    simulation_tick: u64,
    next_event_sequence: u32,
    actors: std::collections::BTreeMap<ActorId, Actor>,
    presentations: ActorPresentationRegistry,
    pub(crate) pending_resolutions: Vec<Resolution>,
}

impl WorldHostile {
    fn from_definition(
        idx: i32,
        x: f64,
        z: f64,
        mob: &MobDefinition,
        movement: &MovementSpec,
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
            armor: mob.armor(),
            name: mob.name().to_owned(),
            mob_id: mob.id().as_str().to_owned(),
            home_x,
            home_z,
            aggro: Aggro::default(),
            state: HostileState::Idle,
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
        }
    }

    pub fn actor_id(&self) -> ActorId {
        self.actor_id
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

    pub fn heading(&self) -> CanonicalHeading {
        self.heading
    }
    pub fn movement_speed_mps(&self) -> f64 {
        self.movement_speed_mps * self.speed_multiplier
    }
    pub fn effective_movement_speed_mps(&self) -> f64 {
        self.movement_speed_mps * self.speed_multiplier
    }
    pub fn speed_multiplier(&self) -> f64 {
        self.speed_multiplier
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
fn movement_speed(a: &WorldHostile, runtime: &Actor) -> f64 {
    a.movement_speed_mps
        * a.speed_multiplier
        * if a.endurance_s <= 0.0 {
            EXHAUSTED_SPEED_RATIO
        } else {
            1.0
        }
        * runtime.movement_multiplier()
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
    pub(crate) fn actor(&self, id: ActorId) -> &Actor {
        self.actors
            .get(&id)
            .unwrap_or_else(|| panic!("actor {id:?} missing from authoritative arena"))
    }
    pub(crate) fn actor_mut(&mut self, id: ActorId) -> &mut Actor {
        self.actors
            .get_mut(&id)
            .unwrap_or_else(|| panic!("actor {id:?} missing from authoritative arena"))
    }
    pub fn actor_statuses(
        &self,
        id: ActorId,
    ) -> impl Iterator<Item = &crate::resolution::TimedStatus> {
        self.actor(id).statuses()
    }
    pub fn actor_effective_faction(&self, id: ActorId) -> &FactionId {
        self.actor(id).effective_faction()
    }
    pub fn actor_can_act(&self, id: ActorId) -> bool {
        self.actor(id).can_act()
    }
    pub fn actor_can_move(&self, id: ActorId) -> bool {
        self.actor(id).can_move()
    }
    pub fn hostile_hp(&self, id: ActorId) -> f64 {
        assert_ne!(id, ActorId::PLAYER, "hostile query received player");
        self.actor(id).hp()
    }
    pub fn hostile_hp_max(&self, id: ActorId) -> f64 {
        assert_ne!(id, ActorId::PLAYER, "hostile query received player");
        self.actor(id).hp_max()
    }
    pub fn hostile_is_alive(&self, id: ActorId) -> bool {
        assert_ne!(id, ActorId::PLAYER, "hostile query received player");
        self.actor(id).is_alive()
    }
    pub fn player_hp(&self) -> f64 {
        self.actor(ActorId::PLAYER).hp()
    }
    pub fn player_hp_max(&self) -> f64 {
        self.actor(ActorId::PLAYER).hp_max()
    }
    pub fn player_mana(&self) -> f64 {
        self.actor(ActorId::PLAYER).mana()
    }
    pub fn player_mana_max(&self) -> f64 {
        self.actor(ActorId::PLAYER).mana_max()
    }
    pub fn player_faction(&self) -> &FactionId {
        self.actor(ActorId::PLAYER).base_faction()
    }
    pub fn player_action_cooldown_s(&self, action_id: &ActionId) -> f64 {
        self.actor(ActorId::PLAYER).actions().cooldown_s(action_id)
    }
    pub fn player_action_roster(&self) -> &[ActionId] {
        self.actor(ActorId::PLAYER).actions().roster()
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
        self.actor(ActorId::PLAYER).progression()
    }

    /// Export one internally consistent player snapshot. Dead players are not saveable.
    pub fn export_player_save(&self) -> Result<PlayerSaveSnapshot, PlayerSaveError> {
        validate_resource(
            "HP",
            self.actor(ActorId::PLAYER).hp(),
            self.actor(ActorId::PLAYER).hp_max(),
            true,
        )?;
        validate_resource(
            "mana",
            self.actor(ActorId::PLAYER).mana(),
            self.actor(ActorId::PLAYER).mana_max(),
            false,
        )?;
        Ok(PlayerSaveSnapshot::new(
            self.actor(ActorId::PLAYER).progression().export_snapshot(),
            self.actor(ActorId::PLAYER).hp(),
            self.actor(ActorId::PLAYER).mana(),
        ))
    }

    /// Validate progression and resources on temporary state, then commit atomically.
    pub fn restore_player_save(
        &mut self,
        snapshot: &PlayerSaveSnapshot,
    ) -> Result<(), PlayerSaveError> {
        let mut progression = self.actor(ActorId::PLAYER).progression().clone();
        progression.restore_snapshot(snapshot.progression())?;
        validate_resource("HP", snapshot.hp(), progression.hp_max(), true)?;
        validate_resource("mana", snapshot.mana(), progression.mana_max(), false)?;
        let position = self.actor(ActorId::PLAYER).position();
        let profile_id = self
            .game_data
            .default_player_profile_id()
            .expect("player profile");
        let profile = self.game_data.profile(&profile_id).expect("player profile");
        let actions = self.actor(ActorId::PLAYER).actions().clone();
        let mut actor = Actor::new(
            crate::resolution::ResolutionActorId::new(0),
            "Adventurer".into(),
            profile.faction().clone(),
            position.0,
            position.1,
            0,
            snapshot.hp(),
            snapshot.mana(),
            true,
            progression,
            profile.actions().to_vec(),
        );
        actor.actions = actions;
        self.actors.insert(ActorId::PLAYER, actor);
        self.dead = false;
        Ok(())
    }

    fn create_player_actor(&self, x: f64, z: f64) -> Actor {
        let profile_id = self
            .game_data
            .default_player_profile_id()
            .expect("player profile");
        let profile = self.game_data.profile(&profile_id).expect("player profile");
        let progression = ActorProgression::from_profile(profile);
        Actor::new(
            crate::resolution::ResolutionActorId::new(0),
            "Adventurer".into(),
            profile.faction().clone(),
            x,
            z,
            0,
            progression.hp_max(),
            progression.mana_max(),
            true,
            progression,
            profile.actions().to_vec(),
        )
    }

    fn initialize_player_actor(&mut self) {
        let actor = self.create_player_actor(0.0, 0.0);
        assert!(
            self.actors.insert(ActorId::PLAYER, actor).is_none(),
            "player actor initialized twice"
        );
    }

    fn hydrate_hostile_metadata(&self, hostile: &mut WorldHostile) {
        let mob_id = MobId::new(hostile.mob_id.clone());
        let mob = self
            .game_data
            .mob(&mob_id)
            .unwrap_or_else(|| panic!("live hostile references unknown GameData mob {mob_id}"));
        hostile.name = mob.name().to_string();
        hostile.mode = mob.mode();
        hostile.movement_speed_mps = self
            .game_data
            .movement_by_id(mob.movement_id())
            .expect("validated mob movement")
            .speed_mps();
        let variance = mob.speed_variance_ratio().as_ratio();
        hostile.speed_multiplier =
            1.0 + deterministic_signed_unit(hostile.spawn_seed, 0) * variance;
        hostile.endurance_max_s = mob.endurance_s().seconds();
        hostile.endurance_s = hostile.endurance_max_s;
    }

    fn create_hostile_actor(&self, hostile: &WorldHostile) -> Actor {
        let mob_id = MobId::new(hostile.mob_id.clone());
        let mob = self.game_data.mob(&mob_id).expect("validated hostile mob");
        let progression = ActorProgression::from_mob(mob, &self.game_data);
        Actor::new(
            crate::resolution::ResolutionActorId::new(hostile.actor_id.canonical),
            hostile.name.clone(),
            mob.faction().clone(),
            hostile.x,
            hostile.z,
            hostile.armor,
            progression.hp_max(),
            progression.mana_max(),
            true,
            progression,
            mob.actions().to_vec(),
        )
    }

    pub(crate) fn hostile_actor_index(&self, hostile_idx: i32) -> Option<usize> {
        self.hostiles
            .iter()
            .position(|hostile| {
                hostile.idx == hostile_idx && self.actor(hostile.actor_id).is_alive()
            })
            .map(|index| index + 1)
    }

    pub(crate) fn execute_arena_action(
        &mut self,
        caster: usize,
        action_id: &ActionId,
        selection: TargetSelection,
        player_x: f64,
        player_z: f64,
    ) -> Result<Resolution, crate::resolution::ResolutionError> {
        self.actor_mut(ActorId::PLAYER)
            .set_position(player_x, player_z);
        let ordered_ids: Vec<ActorId> = std::iter::once(ActorId::PLAYER)
            .chain(self.hostiles.iter().map(|h| h.actor_id))
            .collect();
        let mut arena = std::mem::take(&mut self.actors);
        assert_eq!(
            arena.len(),
            ordered_ids.len(),
            "authoritative arena cardinality diverged from world metadata"
        );
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut ordered: Vec<Actor> = ordered_ids
                .iter()
                .map(|id| {
                    arena
                        .remove(id)
                        .unwrap_or_else(|| panic!("actor {id:?} missing from authoritative arena"))
                })
                .collect();
            assert!(
                arena.is_empty(),
                "authoritative arena contains actor absent from world metadata"
            );
            let result =
                Resolver::new(&self.game_data).execute(&mut ordered, caster, action_id, selection);
            for (id, actor) in ordered_ids.iter().copied().zip(ordered) {
                assert!(
                    arena.insert(id, actor).is_none(),
                    "actor {id:?} duplicated during resolution"
                );
            }
            result
        }));
        self.actors = arena;
        let resolution = match result {
            Ok(value) => value?,
            Err(payload) => std::panic::resume_unwind(payload),
        };
        self.reduce_actor_state(resolution.caster);
        Ok(resolution)
    }

    fn reduce_actor_state(&mut self, source: crate::resolution::ResolutionActorId) {
        if !self.actor(ActorId::PLAYER).is_alive() && !self.dead {
            self.dead = true;
            self.lock = None;
            self.slain_by.get_or_insert_with(|| "hostile".into());
            self.slain_hold_s = SLAIN_HOLD_S;
        }
        let source = if source.get() == 0 {
            ActorId::PLAYER
        } else {
            self.hostiles
                .iter()
                .find(|h| h.actor_id.canonical() == source.get())
                .expect("resolution source actor")
                .actor_id
        };
        let states: Vec<(ActorId, bool)> = self
            .hostiles
            .iter()
            .map(|h| (h.actor_id, self.actor(h.actor_id).is_alive()))
            .collect();
        for hostile in &mut self.hostiles {
            let alive = states
                .iter()
                .find(|(id, _)| *id == hostile.actor_id)
                .expect("hostile arena state")
                .1;
            if alive && source != hostile.actor_id {
                hostile.provoked_by = Some(source);
                hostile.flee_threat = None;
                hostile.target = Some(source);
                hostile.awareness_s = AWARENESS_PERSIST_S;
                hostile.state = HostileState::Alerted;
            }
            if !alive {
                hostile.state = HostileState::Dead;
                if self.lock == Some(hostile.idx) {
                    self.lock = None;
                }
            }
        }
    }

    /// Release render presentation owned by the combat mesh layer.
    ///
    /// The combat simulation owns these bindings; clearing only the layer's
    /// mesh registry would leave stale entity IDs on hostiles and prevent a
    /// later respawn from installing fresh anchors.
    pub fn release_combat_layer_presentations(&mut self) {
        self.presentations
            .remove_source(HostilePresentationSource::CombatLayer);
    }
    pub fn presentations(&self) -> &ActorPresentationRegistry {
        &self.presentations
    }
    pub fn bind_presentation(
        &mut self,
        actor: ActorId,
        source: HostilePresentationSource,
        entity: EntityId,
    ) {
        assert!(
            actor == ActorId::PLAYER || self.hostiles.iter().any(|h| h.actor_id == actor),
            "cannot bind missing actor {actor:?}"
        );
        self.presentations.bind(actor, source, entity);
    }
    pub fn unbind_presentation(
        &mut self,
        actor: ActorId,
        source: HostilePresentationSource,
    ) -> EntityId {
        self.presentations.unbind(actor, source)
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
        self.actor(ActorId::PLAYER)
            .actions()
            .cast()
            .map_or(0.0, |c| c.remaining_s())
    }
    pub fn casting_action_id(&self) -> Option<&ActionId> {
        self.actor(ActorId::PLAYER)
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
        for hostile in &self.hostiles {
            self.actors
                .remove(&hostile.actor_id)
                .expect("cleared hostile actor in arena");
            if let Some(binding) = self.presentations.get(hostile.actor_id) {
                self.presentations
                    .unbind(hostile.actor_id, binding.source());
            }
        }
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
        for id in ids {
            self.actors
                .remove(id)
                .unwrap_or_else(|| panic!("deactivated actor {id:?} missing from arena"));
            if let Some(binding) = self.presentations.get(*id) {
                self.presentations.unbind(*id, binding.source());
            }
        }
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
        self.hydrate_hostile_metadata(&mut hostile);
        let id = hostile.actor_id;
        let actor = self.create_hostile_actor(&hostile);
        assert!(
            self.actors.insert(id, actor).is_none(),
            "duplicate authoritative actor {id:?}"
        );
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

    pub fn hostile_metadata(
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
        WorldHostile::from_definition(runtime_index, x, z, mob, movement, home_x, home_z)
    }

    /// Seat one validated GameData mob in the canonical arena.
    pub fn add_arena_mob(
        &mut self,
        mob_id: &MobId,
        runtime_index: i32,
        x: f64,
        z: f64,
        home_x: f64,
        home_z: f64,
    ) -> ActorId {
        let hostile = self.hostile_metadata(mob_id, runtime_index, x, z, home_x, home_z);
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
        {
            let hp = self.player_hp_max();
            let mana = self.player_mana_max();
            let player = self.actor_mut(ActorId::PLAYER);
            player.set_hp(hp);
            player.set_mana(mana);
        }
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
        self.actor_mut(ActorId::PLAYER).set_hp(hp);
    }
    pub fn set_fail_tell_timer(&mut self, seconds: f64) {
        self.fail_tell_s = seconds;
    }
    /// Consume frame time into authoritative simulation ticks.
    pub fn consume_fixed_steps(&mut self, dt: f64) -> usize {
        if dt <= 0.0 {
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
        self.simulation_tick = self
            .simulation_tick
            .checked_add(1)
            .expect("simulation tick overflow");
        self.next_event_sequence = 0;
        self.fail_tell_s = (self.fail_tell_s - TICK).max(0.0);
        if self.fail_tell_s == 0.0 {
            self.fail_tell = None;
        }
        if self.dead {
            self.slain_hold_s = (self.slain_hold_s - TICK).max(0.0);
            return CombatStep {
                resolutions: std::mem::take(&mut self.pending_resolutions),
            };
        }
        if let Some(shaken) = self.player.shaken.as_mut() {
            shaken.tick(TICK);
            if shaken.remaining_s <= 0.0 {
                self.player.shaken = None;
            }
        }
        // Fixed phase order: lifecycle timers, AI/movement, player runtime/actions,
        // hostile runtime/actions, then regeneration from the resulting engagement state.
        self.tick_hostile_ai(player_x, player_z, TICK);
        self.tick_player_actions(player_x, player_z, TICK);
        self.tick_incoming(player_x, player_z, TICK);
        let regen = if self.in_combat() {
            MANA_REGEN_COMBAT_PER_S
        } else {
            MANA_REGEN_OOC_PER_S
        };
        self.actor_mut(ActorId::PLAYER)
            .regenerate_mana(regen * TICK);
        CombatStep {
            resolutions: std::mem::take(&mut self.pending_resolutions),
        }
    }

    pub fn hud_snapshot(&self) -> CombatHudSnapshot {
        let player = self.actor(ActorId::PLAYER);
        let cast = player.actions().cast();
        let lock = self
            .lock
            .and_then(|idx| self.hostiles.iter().find(|h| h.idx == idx))
            .map(|h| h.actor_id);
        let actors = self
            .hostiles
            .iter()
            .map(|h| CombatHudActor {
                id: h.actor_id,
                name: h.name.clone(),
                hp: self.actor(h.actor_id).hp(),
                hp_max: self.actor(h.actor_id).hp_max(),
                head_x: h.x,
                head_y: 1.55,
                head_z: h.z,
                alive: self.actor(h.actor_id).is_alive(),
            })
            .collect();
        let actions = player
            .actions()
            .roster()
            .iter()
            .map(|id| {
                let definition = self
                    .game_data
                    .action(id)
                    .unwrap_or_else(|| panic!("HUD actor references unknown action {id}"));
                CombatHudAction {
                    id: id.clone(),
                    name: definition.name().to_owned(),
                    cooldown_fraction: self.action_cd_frac(id),
                }
            })
            .collect();
        let mut progression: Vec<CombatHudProgression> = player
            .progression()
            .skills()
            .map(|id| {
                let skill = self
                    .game_data
                    .skill(id)
                    .unwrap_or_else(|| panic!("player knows unknown skill {id}"));
                let level = player
                    .progression()
                    .skill_level(id)
                    .expect("known skill level");
                let xp = player.progression().skill_xp(id).expect("known skill XP");
                let remaining = player
                    .progression()
                    .skill_xp_to_next(id)
                    .expect("known skill threshold");
                CombatHudProgression {
                    label: skill.name().to_owned(),
                    level,
                    xp,
                    xp_total: xp + remaining,
                }
            })
            .collect();
        let hp_xp = player.progression().hp_xp();
        progression.push(CombatHudProgression {
            label: "HP".into(),
            level: player.progression().hp_level(),
            xp: hp_xp,
            xp_total: hp_xp + player.progression().hp_xp_to_next(),
        });
        let mana_xp = player.progression().mana_xp();
        progression.push(CombatHudProgression {
            label: "Mana".into(),
            level: player.progression().mana_level(),
            xp: mana_xp,
            xp_total: mana_xp + player.progression().mana_xp_to_next(),
        });
        CombatHudSnapshot {
            hp: player.hp(),
            hp_max: player.hp_max(),
            mana: player.mana(),
            mana_max: player.mana_max(),
            dead: self.dead,
            lock,
            actors,
            actions,
            cast_label: cast.map(|c| {
                self.game_data
                    .action(c.action_id())
                    .expect("live cast action")
                    .name()
                    .to_owned()
            }),
            cast_fraction: cast.and_then(|c| {
                (c.total_s() > 0.0 && c.remaining_s() > 0.0)
                    .then(|| (c.remaining_s() / c.total_s()).clamp(0.0, 1.0) as f32)
            }),
            log_lines: self.log_lines(),
            fail_tell: self.fail_tell(),
            attack_pip: false,
            hurt_flash: false,
            hp_ghost_fraction: None,
            shaken: self.player.shaken.is_some(),
            slain_line: self.slain_by.as_ref().map(|by| format!("Slain by {by}")),
            progression,
        }
    }

    pub fn simulation_tick(&self) -> u64 {
        self.simulation_tick
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
            simulation_tick: 0,
            next_event_sequence: 0,
            actors: std::collections::BTreeMap::new(),
            presentations: ActorPresentationRegistry::default(),
            pending_resolutions: Vec::new(),
        };
        combat.initialize_player_actor();
        combat
    }
    fn hostile_pairs(&self) -> Vec<(i32, f64, f64)> {
        let player_faction = self.player_faction();
        self.hostiles
            .iter()
            .filter(|h| {
                self.actor(h.actor_id).is_alive()
                    && self.game_data.factions_are_hostile(
                        player_faction,
                        self.actor(h.actor_id).effective_faction(),
                    )
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
        {
            let hp = self.player_hp_max();
            let mana = self.player_mana_max();
            let player = self.actor_mut(ActorId::PLAYER);
            player.set_hp(hp);
            player.set_mana(mana);
        }
        self.dead = false;
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
        self.actor_mut(ActorId::PLAYER)
            .take_damage(f64::from(dealt));
        let killed = !self.actor(ActorId::PLAYER).is_alive();
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
    pub fn administratively_damage_hostile(&mut self, idx: i32, dealt: i32) -> bool {
        assert!(dealt >= 0, "negative hostile damage {dealt} for {idx}");
        let hi = self
            .hostiles
            .iter()
            .position(|h| h.idx == idx && self.actor(h.actor_id).is_alive())
            .unwrap_or_else(|| panic!("damage reported for missing or dead hostile {idx}"));
        let id = self.hostiles[hi].actor_id;
        self.actor_mut(id).take_damage(f64::from(dealt));
        self.set_retaliation(hi, ActorId::PLAYER);
        if !self.actor(id).is_alive() {
            self.administratively_defeat_hostile(idx);
            true
        } else {
            false
        }
    }
    pub fn hostile_took_damage(&mut self, idx: i32) {
        let hi = self
            .hostiles
            .iter()
            .position(|h| h.idx == idx && self.actor(h.actor_id).is_alive())
            .unwrap_or_else(|| panic!("damage reported for missing or dead hostile {idx}"));
        self.set_retaliation(hi, ActorId::PLAYER);
    }
    fn set_retaliation(&mut self, index: usize, source: ActorId) {
        let h = &mut self.hostiles[index];
        h.flee_threat = None;
        h.provoked_by = Some(source);
        h.target = Some(source);
        h.awareness_s = AWARENESS_PERSIST_S;
        h.state = HostileState::Alerted;
    }

    pub fn administratively_defeat_hostile(&mut self, idx: i32) {
        let hi = self
            .hostiles
            .iter()
            .position(|h| h.idx == idx && self.actor(h.actor_id).is_alive())
            .unwrap_or_else(|| panic!("defeat reported for missing or dead hostile {idx}"));
        let id = self.hostiles[hi].actor_id;
        let hp = self.actor(id).hp();
        self.actor_mut(id).take_damage(hp);
        self.hostiles[hi].state = HostileState::Dead;
        if self.lock == Some(idx) {
            self.lock = None;
        }
    }

    fn usable_hostile_actions<'a>(
        &'a self,
        hostile: &'a WorldHostile,
    ) -> impl Iterator<Item = (ActionId, f64)> + 'a {
        let runtime = self.actors.get(&hostile.actor_id);
        self.actor(hostile.actor_id)
            .actions()
            .roster()
            .iter()
            .filter_map(move |id| {
                let action = self.game_data.action(id).unwrap_or_else(|| {
                    panic!("actor {} references unknown action {id}", hostile.mob_id)
                });
                if !matches!(action.target(), ActionTarget::Hostile | ActionTarget::Any)
                    || runtime.is_some_and(|actor| {
                        actor.actions().cooldown_s(id) > 0.0 || actor.mana() < action.mana_cost()
                    })
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
        let data = &self.game_data;
        let player = (
            ActorId::PLAYER,
            self.player_faction().clone(),
            player_x,
            player_z,
            true,
        );
        let actors: Vec<(ActorId, FactionId, MobMode, f64, f64, bool)> = self
            .hostiles
            .iter()
            .map(|a| {
                (
                    a.actor_id,
                    self.actor(a.actor_id).effective_faction().clone(),
                    a.mode,
                    a.x,
                    a.z,
                    self.actor(a.actor_id).is_alive(),
                )
            })
            .collect();

        let pursuit_actions: std::collections::HashMap<ActorId, Option<(ActionId, f64)>> = self
            .hostiles
            .iter()
            .map(|hostile| (hostile.actor_id, self.pursuit_action(hostile)))
            .collect();
        let runtime_actors = &self.actors;
        for actor in &mut self.hostiles {
            actor.previous_x = actor.x;
            actor.previous_z = actor.z;
            actor.previous_heading = actor.heading;
            if !runtime_actors
                .get(&actor.actor_id)
                .expect("hostile runtime")
                .is_alive()
            {
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
                } else if runtime_actors
                    .get(&actor.actor_id)
                    .expect("hostile runtime")
                    .can_move()
                {
                    let step =
                        (actor.movement_speed_mps * actor.speed_multiplier * dt).min(distance);
                    actor.heading = CanonicalHeading::from_xz(dx, dz);
                    actor.x += dx / distance * step;
                    actor.z += dz / distance * step;
                }
                continue;
            }
            let actor_faction = runtime_actors
                .get(&actor.actor_id)
                .expect("hostile runtime")
                .effective_faction();
            let hostile_to = |faction: &FactionId| {
                !data
                    .faction(actor_faction)
                    .expect("validated observer faction")
                    .is_neutral()
                    && !data
                        .faction(faction)
                        .expect("validated candidate faction")
                        .is_neutral()
                    && data.factions_are_hostile(actor_faction, faction)
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
                    if !runtime_actors
                        .get(&actor.actor_id)
                        .expect("hostile runtime")
                        .can_move()
                    {
                        false
                    } else {
                        let dx = actor.x - tx;
                        let dz = actor.z - tz;
                        let distance = dx.hypot(dz);
                        if distance <= 1e-9 {
                            false
                        } else {
                            let speed = movement_speed(
                                actor,
                                runtime_actors
                                    .get(&actor.actor_id)
                                    .expect("hostile runtime"),
                            );
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
                    } else if runtime_actors
                        .get(&actor.actor_id)
                        .expect("hostile runtime")
                        .can_move()
                    {
                        actor.state = HostileState::Pursuing;
                        let speed = movement_speed(
                            actor,
                            runtime_actors
                                .get(&actor.actor_id)
                                .expect("hostile runtime"),
                        );
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
        let poses: Vec<(ActorId, f64, f64)> = self
            .hostiles
            .iter()
            .map(|h| (h.actor_id, h.x, h.z))
            .collect();
        for (id, x, z) in poses {
            self.actor_mut(id).set_position(x, z);
        }
    }

    pub fn in_combat(&self) -> bool {
        self.hostiles.iter().any(WorldHostile::is_engaged)
    }

    fn record_resolution_log(&mut self, resolution: &Resolution) {
        let caster_name = if resolution.caster.get() == ActorId::PLAYER.canonical() {
            None
        } else {
            Some(
                self.hostiles
                    .iter()
                    .find(|h| h.actor_id.canonical() == resolution.caster.get())
                    .unwrap_or_else(|| {
                        panic!(
                            "resolution log references missing caster {:?}",
                            resolution.caster
                        )
                    })
                    .name
                    .clone(),
            )
        };
        let action_name = self
            .game_data
            .action(&resolution.action_id)
            .unwrap_or_else(|| {
                panic!(
                    "resolution log references unknown action {}",
                    resolution.action_id
                )
            })
            .name()
            .to_owned();
        let mut slain_logged = false;
        for effect in &resolution.effects {
            for (&target, &applied) in effect.targets.iter().zip(&effect.applied) {
                if applied <= 0.0 {
                    continue;
                }
                let target_name = if target.get() == ActorId::PLAYER.canonical() {
                    None
                } else {
                    Some(
                        self.hostiles
                            .iter()
                            .find(|h| h.actor_id.canonical() == target.get())
                            .unwrap_or_else(|| {
                                panic!("resolution log references missing target {target:?}")
                            })
                            .name
                            .clone(),
                    )
                };
                match effect.operation {
                    EffectOperation::DirectDamage => {
                        match (caster_name.as_deref(), target_name.as_deref()) {
                            (None, Some(target)) => self.log.push(format!(
                                "You {action_name} {target} for {}",
                                applied.round() as i32
                            )),
                            (Some(caster), Some(target)) => self.log.push(format!(
                                "{caster} hits {target} for {}",
                                applied.round() as i32
                            )),
                            (Some(caster), None) => {
                                self.log.push(format!(
                                    "{caster} hits you for {}",
                                    applied.round() as i32
                                ));
                                if self.dead && !slain_logged {
                                    self.log.push("You are slain");
                                    slain_logged = true;
                                }
                            }
                            (None, None) => self
                                .log
                                .push(format!("You hit yourself for {}", applied.round() as i32)),
                        }
                    }
                    EffectOperation::Heal => match caster_name.as_deref() {
                        None => self
                            .log
                            .push(format!("You {action_name} for {}", applied.round() as i32)),
                        Some(caster) => self
                            .log
                            .push(format!("{caster} heals for {}", applied.round() as i32)),
                    },
                    _ => {}
                }
            }
        }
    }

    pub(crate) fn record_resolution(&mut self, mut resolution: Resolution) {
        self.record_resolution_log(&resolution);
        let sequence = self.next_event_sequence;
        self.next_event_sequence = self
            .next_event_sequence
            .checked_add(1)
            .expect("combat event sequence overflow");
        resolution.stamp(self.simulation_tick, sequence);
        self.pending_resolutions.push(resolution);
    }

    fn tick_hostile_attacks(
        &mut self,
        player_x: f64,
        player_z: f64,
        dt: f64,
    ) -> Option<IncomingHit> {
        if self.dead || dt <= 0.0 {
            return None;
        }
        let hostile_ids: Vec<ActorId> = self
            .hostiles
            .iter()
            .map(|hostile| hostile.actor_id)
            .collect();
        for id in hostile_ids {
            self.actor_mut(id).tick_runtime(dt);
        }

        #[derive(Debug)]
        struct ReadyAction {
            caster: ActorId,
            name: String,
            target: ActorId,
            action: ActionId,
        }
        let mut ready = Vec::new();
        let mut starts = Vec::new();
        for hostile in &self.hostiles {
            if !self.actor(hostile.actor_id).is_alive() || hostile.state != HostileState::Attacking
            {
                continue;
            }
            let target = hostile
                .target
                .unwrap_or_else(|| panic!("attacking actor {} has no target", hostile.mob_id));
            let actor = self.actor(hostile.actor_id);
            if let Some(cast) = actor.actions().cast() {
                if cast.remaining_s() <= 0.0 {
                    let captured = cast
                        .target()
                        .expect("hostile single-target cast must capture a target");
                    ready.push(ReadyAction {
                        caster: hostile.actor_id,
                        name: hostile.name.clone(),
                        target: self.actor_id_from_resolution(captured),
                        action: cast.action_id().clone(),
                    });
                }
                continue;
            }
            if !actor.can_act() {
                continue;
            }
            let (tx, tz) = self.actor_position(target, player_x, player_z);
            let distance = (tx - hostile.x).hypot(tz - hostile.z);
            if let Some((action, _)) = self.selected_usable_action(hostile, distance) {
                starts.push((hostile.actor_id, hostile.name.clone(), target, action));
            }
        }
        for (caster, name, target, action_id) in starts {
            let cast_s = self
                .game_data
                .action(&action_id)
                .expect("selected action exists")
                .cast_s();
            if cast_s > 0.0 {
                assert!(
                    self.hostiles.iter().any(|h| h.actor_id == caster),
                    "selected caster retained"
                );
                self.actor_mut(caster).actions_mut().start_cast(
                    action_id,
                    Some(crate::resolution::ResolutionActorId::new(
                        target.canonical(),
                    )),
                    cast_s,
                );
            } else {
                ready.push(ReadyAction {
                    caster,
                    name,
                    target,
                    action: action_id,
                });
            }
        }
        ready.sort_by_key(|entry| entry.caster);

        let mut incoming = None;
        for entry in ready {
            let caster = self
                .hostiles
                .iter()
                .position(|h| h.actor_id == entry.caster && self.actor(h.actor_id).is_alive())
                .map(|index| index + 1)
                .unwrap_or_else(|| panic!("ready caster {:?} disappeared", entry.caster));
            let target = if entry.target == ActorId::PLAYER {
                0
            } else {
                self.hostiles
                    .iter()
                    .position(|h| h.actor_id == entry.target && self.actor(h.actor_id).is_alive())
                    .map(|index| index + 1)
                    .unwrap_or_else(|| panic!("ready target {:?} disappeared", entry.target))
            };
            self.actor_mut(entry.caster)
                .actions_mut()
                .take_completed_cast();
            let previous_player_hp = self.actor(ActorId::PLAYER).hp();
            let resolution = self
                .execute_arena_action(
                    caster,
                    &entry.action,
                    TargetSelection::Single(target),
                    player_x,
                    player_z,
                )
                .unwrap_or_else(|err| {
                    panic!("ready canonical action {} failed: {err}", entry.action)
                });
            let cooldown = self
                .game_data
                .action(&entry.action)
                .expect("resolved action exists")
                .cooldown_s();
            if cooldown > 0.0 {
                self.actor_mut(entry.caster)
                    .actions_mut()
                    .start_cooldown(entry.action.clone(), cooldown);
            }
            if entry.target == ActorId::PLAYER {
                incoming = Some(IncomingHit {
                    dealt: (previous_player_hp - self.actor(ActorId::PLAYER).hp()).round() as i32,
                    by: entry.name.clone(),
                    killed: self.dead,
                });
                if self.dead {
                    self.slain_by = Some(entry.name);
                }
            }
            self.record_resolution(resolution);
        }
        incoming
    }

    fn actor_id_from_resolution(&self, id: crate::resolution::ResolutionActorId) -> ActorId {
        if id.get() == ActorId::PLAYER.canonical() {
            return ActorId::PLAYER;
        }
        self.hostiles
            .iter()
            .find(|h| h.actor_id.canonical() == id.get())
            .map(|h| h.actor_id)
            .unwrap_or_else(|| panic!("captured cast target {} is missing", id.get()))
    }

    fn actor_position(&self, id: ActorId, player_x: f64, player_z: f64) -> (f64, f64) {
        if id == ActorId::PLAYER {
            return (player_x, player_z);
        }
        self.hostiles
            .iter()
            .find(|h| h.actor_id == id && self.actor(h.actor_id).is_alive())
            .map(|h| (h.x, h.z))
            .unwrap_or_else(|| panic!("target actor {id:?} is missing or dead"))
    }
    pub fn tick_incoming(&mut self, player_x: f64, player_z: f64, dt: f64) -> Option<IncomingHit> {
        self.tick_hostile_attacks(player_x, player_z, dt)
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
        let actor = combat.actor(ActorId::PLAYER);
        assert_eq!(actor.id().get(), 0);
        assert_eq!(actor.position(), (0.0, 0.0));
    }

    #[test]
    fn hud_head_positions_require_exact_actor_identity_set() {
        let mut combat = WorldCombat::with_game_data(canonical_data());
        combat.add_arena_mob(&MobId::new("wolf"), 0, 1.0, 2.0, 1.0, 2.0);
        let actor_id = combat.hostiles()[0].actor_id();
        let snapshot = combat.hud_snapshot();

        let missing = std::collections::BTreeMap::new();
        assert!(
            std::panic::catch_unwind(|| snapshot.clone().with_head_positions(&missing)).is_err()
        );

        let mut extra = std::collections::BTreeMap::new();
        extra.insert(actor_id, (1.0, 2.0, 3.0));
        extra.insert(ActorId::from_runtime_index(99), (4.0, 5.0, 6.0));
        assert!(std::panic::catch_unwind(|| snapshot.clone().with_head_positions(&extra)).is_err());

        let mut wrong_identity = std::collections::BTreeMap::new();
        wrong_identity.insert(ActorId::from_runtime_index(99), (4.0, 5.0, 6.0));
        assert!(
            std::panic::catch_unwind(|| snapshot.with_head_positions(&wrong_identity)).is_err()
        );
    }
    #[test]
    fn hud_snapshot_is_immutable_until_refreshed_and_contains_progression() {
        let mut combat = WorldCombat::with_game_data(canonical_data());
        let snapshot = combat.hud_snapshot();
        let hp_before = snapshot.hp();
        let progression_before = snapshot.progression().to_vec();
        assert!(!progression_before.is_empty());
        assert!(progression_before.iter().any(|track| track.label() == "HP"));
        assert!(progression_before
            .iter()
            .any(|track| track.label() == "Mana"));
        combat.set_player_hp(hp_before - 1.0);
        assert_eq!(snapshot.hp(), hp_before);
        assert_eq!(snapshot.progression(), progression_before);
        assert_eq!(combat.hud_snapshot().hp(), hp_before - 1.0);
    }
    #[test]
    fn restored_save_updates_canonical_player_without_dropping_action_state() {
        let data = canonical_data();
        let mut combat = WorldCombat::with_game_data(data);
        let action_id = combat.player_action_roster()[0].clone();
        combat
            .actor_mut(ActorId::PLAYER)
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

        let actor = combat.actor(ActorId::PLAYER);
        assert_eq!(actor.hp(), 73.0);
        assert_eq!(actor.mana(), 41.0);
        assert_eq!(actor.progression(), combat.player_progression());
        assert_eq!(combat.player_hp_max(), actor.hp_max());
        assert_eq!(combat.player_mana_max(), actor.mana_max());
        assert_eq!(combat.player_action_cooldown_s(&action_id), 3.0);
    }
    #[test]
    fn seated_wolf_hydrates_before_ai_attack_and_repeats_after_cooldown() {
        let data = canonical_data();
        let mut combat = WorldCombat::with_game_data(data);
        combat.add_arena_mob(&MobId::new("wolf"), 0, 1.5, 0.0, 1.5, 0.0);

        assert!(combat.actors.contains_key(&combat.hostiles[0].actor_id));
        assert_eq!(combat.hostiles[0].state, HostileState::Idle);

        let hp_before = combat.player_hp();
        let mut hit_ticks = Vec::new();
        for tick in 0..40 {
            let step = combat.step_fixed(0.0, 0.0, 1.0, 0.0);
            if !step.resolutions.is_empty() {
                hit_ticks.push(tick);
            }
        }

        assert_eq!(hit_ticks, vec![15, 26, 37]);
        assert!(combat.player_hp() < hp_before);
        assert_eq!(combat.hostiles[0].state, HostileState::Attacking);
        assert_eq!(
            combat
                .actor(combat.hostiles[0].actor_id)
                .actions()
                .cooldown_s(&ActionId::new("slash")),
            0.8
        );
    }

    #[test]
    fn damaged_passive_deer_retaliates_repeatedly_without_losing_actor_identity() {
        let data = canonical_data();
        let mut combat = WorldCombat::with_game_data(data);
        let deer_id = combat.add_arena_mob(&MobId::new("deer"), 41, 1.5, 0.0, 1.5, 0.0);
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
        assert_eq!(combat.hostile_hp(deer_id), 47.0);
        assert!(combat.hostile_is_alive(deer_id));
        assert_eq!(deer.target(), Some(ActorId::PLAYER));

        let hp_before = combat.player_hp();
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
        assert!(combat.player_hp() < hp_before);
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
        combat.add_arena_mob(&MobId::new("skeleton_mage"), 0, 10.0, 0.0, 10.0, 0.0);
        let hostile = &mut combat.hostiles[0];
        hostile.state = HostileState::Attacking;
        hostile.target = Some(ActorId::PLAYER);

        let hp_before = combat.player_hp();
        assert!(combat.tick_incoming(0.0, 0.0, TICK).is_none());
        let cast = combat
            .actor(combat.hostiles[0].actor_id)
            .actions()
            .cast()
            .expect("second action should stage a cast");
        assert_eq!(cast.action_id(), &ActionId::new("grave_lance"));
        assert!(cast.remaining_s() > 0.0);
        assert_eq!(combat.player_hp(), hp_before);

        let mut elapsed = 0.0;
        let hit = loop {
            match combat.tick_incoming(0.0, 0.0, TICK) {
                Some(hit) => break hit,
                None => {
                    assert_eq!(combat.player_hp(), hp_before);
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
        assert!(combat.player_hp() < hp_before);
    }

    #[test]
    fn simultaneous_attackers_all_resolve_in_stable_actor_order() {
        let data = canonical_data();
        let mut combat = WorldCombat::with_game_data(data);
        let first = combat.add_arena_mob(&MobId::new("wolf"), 7, 1.5, 0.0, 1.5, 0.0);
        let second = combat.add_arena_mob(&MobId::new("wolf"), 3, -1.5, 0.0, -1.5, 0.0);
        for hostile in &mut combat.hostiles {
            hostile.state = HostileState::Attacking;
            hostile.target = Some(ActorId::PLAYER);
        }
        combat.simulation_tick = 1;
        combat.tick_incoming(0.0, 0.0, TICK);
        let resolutions = std::mem::take(&mut combat.pending_resolutions);
        assert_eq!(resolutions.len(), 2);
        assert_eq!(
            resolutions
                .iter()
                .map(|r| r.caster.get())
                .collect::<Vec<_>>(),
            vec![first.canonical(), second.canonical()]
        );
        assert_eq!(
            resolutions
                .iter()
                .map(|r| r.event_id().sequence())
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
        assert!(resolutions
            .iter()
            .all(|r| r.event_id().simulation_tick() == combat.simulation_tick()));
    }

    #[test]
    fn completed_cast_uses_captured_target() {
        let data = canonical_data();
        let mut combat = WorldCombat::with_game_data(data);
        combat.add_arena_mob(&MobId::new("skeleton_mage"), 0, 10.0, 0.0, 10.0, 0.0);
        let other = combat.add_arena_mob(&MobId::new("wolf"), 1, 10.0, 1.0, 10.0, 1.0);
        combat.hostiles[0].state = HostileState::Attacking;
        combat.hostiles[0].target = Some(ActorId::PLAYER);
        combat.tick_incoming(0.0, 0.0, TICK);
        assert!(combat
            .actor(combat.hostiles[0].actor_id)
            .actions()
            .cast()
            .is_some());
        combat.hostiles[0].target = Some(other);
        while combat.pending_resolutions.is_empty() {
            combat.tick_incoming(0.0, 0.0, TICK);
        }
        assert_eq!(combat.pending_resolutions.len(), 1);
        assert_eq!(
            combat.pending_resolutions[0].effects[0].targets,
            vec![crate::resolution::ResolutionActorId::new(0)]
        );
        assert!(combat.player_hp() < combat.player_hp_max());
        let id = combat.hostiles[1].actor_id();
        assert_eq!(combat.hostile_hp(id), combat.hostile_hp_max(id));
    }

    #[test]
    fn mana_exhausted_hostile_stages_no_action() {
        let data = canonical_data();
        let mut combat = WorldCombat::with_game_data(data);
        combat.add_arena_mob(&MobId::new("skeleton_mage"), 0, 10.0, 0.0, 10.0, 0.0);
        let actor_id = combat.hostiles[0].actor_id;
        combat.hostiles[0].state = HostileState::Attacking;
        combat.hostiles[0].target = Some(ActorId::PLAYER);
        combat.actor_mut(actor_id).set_mana(0.0);
        assert!(combat.tick_incoming(0.0, 0.0, TICK).is_none());
        assert!(combat.actor(actor_id).actions().cast().is_none());
    }

    #[test]
    fn authoritative_arena_tracks_exact_hostile_identity() {
        let data = canonical_data();
        let mut combat = WorldCombat::with_game_data(data);
        let id = combat.add_arena_mob(&MobId::new("wolf"), 0, 1.0, 0.0, 1.0, 0.0);
        assert_eq!(combat.actor(id).id().get(), id.canonical());
        combat.deactivate_actors(&[id]);
        assert!(!combat.actors.contains_key(&id));
    }

    #[test]
    fn dead_world_ticks_slain_and_failure_timers() {
        let data = canonical_data();
        let mut combat = WorldCombat::with_game_data(data);
        combat.set_fail_tell(Some("failed"));
        combat.set_fail_tell_timer(0.2);
        combat.dead = true;
        combat.slain_hold_s = 0.2;
        assert_eq!(combat.consume_fixed_steps(0.2), 2);
        combat.step_fixed(0.0, 0.0, 1.0, 0.0);
        combat.step_fixed(0.0, 0.0, 1.0, 0.0);
        assert_eq!(combat.slain_hold_s(), 0.0);
        assert_eq!(combat.fail_tell(), None);
    }

    #[test]
    fn mob_skips_first_action_while_it_is_on_cooldown() {
        let data = canonical_data();
        let mut combat = WorldCombat::with_game_data(data);
        combat.add_arena_mob(&MobId::new("blue_demon"), 0, 2.0, 0.0, 2.0, 0.0);
        let hostile = &mut combat.hostiles[0];
        hostile.state = HostileState::Attacking;
        hostile.target = Some(ActorId::PLAYER);
        let actor_id = hostile.actor_id;
        combat
            .actor_mut(actor_id)
            .actions_mut()
            .start_cooldown(ActionId::new("hellbrand"), 2.0);

        assert!(combat.tick_incoming(0.0, 0.0, TICK).is_none());
        let cast = combat
            .actor(combat.hostiles[0].actor_id)
            .actions()
            .cast()
            .expect("second action should stage while first is cooling down");
        assert_eq!(cast.action_id(), &ActionId::new("azure_flare"));
    }
}
