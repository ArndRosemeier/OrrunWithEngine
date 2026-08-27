//! Live combat types. Same numbers as the sim — do not invent a second formula set.

use std::collections::BTreeMap;

use thiserror::Error;

use super::log::CombatLog;
use super::math::*;
use super::sheets::{player_stats, PlayerStats};
use crate::gamedata::{ActionId, FactionId, MobId, MobMode};
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
    pub fn from_stats(stats: &PlayerStats) -> Self {
        Self {
            hp: f64::from(stats.hp),
            hp_max: f64::from(stats.hp),
            mana: f64::from(stats.mana),
            mana_max: f64::from(stats.mana),
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

    pub fn outgoing_mult(&self) -> f64 {
        if self.remaining_s > 0.0 {
            SHAKEN_DMG
        } else {
            1.0
        }
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
    in_cone.sort_by(|a, b| {
        a.0.partial_cmp(&b.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.cmp(&b.1))
    });
    in_cone.into_iter().map(|(_, i)| i).collect()
}

/// Melee auto is legal only if lock is in 2.8 m and a 120° facing cone.
pub fn melee_auto_legal(
    player_x: f64,
    player_z: f64,
    facing_x: f64,
    facing_z: f64,
    target_x: f64,
    target_z: f64,
) -> bool {
    let dx = target_x - player_x;
    let dz = target_z - player_z;
    let dist = (dx * dx + dz * dz).sqrt();
    if dist > MELEE_REACH_M {
        return false;
    }
    if dist <= 1e-9 {
        return true;
    }
    let fl = (facing_x * facing_x + facing_z * facing_z).sqrt();
    if fl <= 1e-9 {
        return false;
    }
    let fx = facing_x / fl;
    let fz = facing_z / fl;
    let nx = dx / dist;
    let nz = dz / dist;
    let dot = (fx * nx + fz * nz).clamp(-1.0, 1.0);
    let ang = dot.acos();
    ang <= (MELEE_CONE_DEG.to_radians()) * 0.5
}

#[derive(Clone, Debug)]
pub struct LivePlayer {
    pub stats: PlayerStats,
    pub resources: CombatResources,
    pub lock: Option<i32>,
    pub potions: i32,
    pub arrows: i32,
    pub shaken: Option<Shaken>,
    pub sprinted: bool,
    pub used_pin_or_bind: bool,
    pub xp: i32,
    faction: FactionId,
    progression: ActorProgression,
}

impl LivePlayer {
    pub fn specialist(level: i32, discipline: Discipline) -> Self {
        let stats = player_stats(level, discipline);
        let resources = CombatResources::from_stats(&stats);
        Self {
            stats,
            resources,
            lock: None,
            potions: START_POTIONS,
            arrows: START_ARROWS,
            shaken: None,
            sprinted: false,
            used_pin_or_bind: false,
            xp: 0,
            faction: FactionId::new("citizen"),
            progression: ActorProgression::empty(),
        }
    }

    pub fn drink_potion(&mut self) -> bool {
        if self.potions <= 0 {
            return false;
        }
        self.resources.hp = self
            .resources
            .hp_max
            .min(self.resources.hp + f64::from(POTION_HEAL));
        self.potions -= 1;
        true
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
    pub stun_s: f64,
    pub slow_s: f64,
    pub root_s: f64,
    /// Display name for the lock tell. Fixture wolves use "wolf-spider".
    pub name: String,
    /// Sheet / catalog id (orc, tribal, orc_skull, wolf).
    pub mob_id: String,
    /// Visible body entity, if the fixture mesh has been spawned.
    pub entity: Option<engine::world::EntityId>,
    pub damage: i32,
    pub swing_s: f64,
    pub swing_cd: f64,
    pub reach_m: f64,
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
}

#[derive(Clone, Debug)]
pub struct IncomingHit {
    pub dealt: i32,
    pub by: String,
    pub killed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpecialAttackCue {
    Weapon,
    SpellcastShoot,
}

#[derive(Clone, Debug)]
pub struct SpecialAttackEvent {
    pub attacker_idx: i32,
    pub cue: SpecialAttackCue,
    pub hit: Option<IncomingHit>,
    pub previous_player_hp: f64,
}

#[derive(Clone, Debug)]
pub struct CombatStep {
    pub outgoing: Option<(i32, i32, bool)>,
    pub incoming: Option<(f64, IncomingHit)>,
    pub specials: Vec<SpecialAttackEvent>,
    pub resolutions: Vec<Resolution>,
}

#[derive(Clone, Debug)]
pub(crate) struct CanonicalCast {
    pub(crate) action_id: ActionId,
    pub(crate) target: Option<i32>,
    pub(crate) remaining_s: f64,
    pub(crate) total_s: f64,
}

#[derive(Clone, Debug)]
pub struct WorldCombat {
    #[allow(dead_code)]
    pub(crate) game_data: Option<std::sync::Arc<crate::gamedata::GameData>>,
    player: LivePlayer,
    lock: Option<i32>,
    cycle: Vec<i32>,
    auto_cd: f64,
    last_auto_dealt: i32,
    hostiles: Vec<WorldHostile>,
    next_actor_id: u32,
    strike_armed: bool,
    ember_started: bool,
    last_potion_heal: i32,
    busy: f64,
    gcd: f64,
    cds: std::collections::BTreeMap<&'static str, f64>,
    cast_kind: Option<&'static str>,
    cast_t: f64,
    cast_target: Option<i32>,
    ward: f64,
    ward_t: f64,
    mark_t: f64,
    second_wind_used: bool,
    last_rank_gate: Option<crate::controls::RankGate>,
    dead: bool,
    slain_by: Option<String>,
    slain_hold_s: f64,
    last_incoming: Option<IncomingHit>,
    log: CombatLog,
    fail_tell: Option<&'static str>,
    fail_tell_s: f64,
    special_tele: std::collections::BTreeMap<i32, f64>,
    /// Fixed-step accumulator owned by the simulation, never by presentation.
    fixed_accum_s: f64,
    pub(crate) canonical_cds: BTreeMap<ActionId, f64>,
    pub(crate) canonical_cast: Option<CanonicalCast>,
    pub(crate) pending_resolutions: Vec<Resolution>,
}

impl WorldHostile {
    pub(crate) fn from_sheet(
        idx: i32,
        x: f64,
        z: f64,
        sheet: &crate::combat::sheets::MobSheet,
        mob_id: impl Into<String>,
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
            hp: f64::from(sheet.hp),
            max_hp: f64::from(sheet.hp),
            armor: sheet.armor,
            alive: true,
            stun_s: 0.0,
            slow_s: 0.0,
            root_s: 0.0,
            name: sheet.name.clone(),
            mob_id: mob_id.into(),
            entity: None,
            damage: sheet.damage,
            swing_s: sheet.swing_s,
            swing_cd: sheet.swing_s,
            reach_m: sheet.reach_m,
            home_x,
            home_z,
            aggro: Aggro {
                sight_m: sheet.sight_m,
                hear_m: sheet.hear_m,
                leash_m: sheet.leash_m,
                social_m: sheet.social_m,
            },
            state: HostileState::Idle,
            faction: FactionId::new("wild"),
            mode: MobMode::Active,
            target: None,
            provoked_by: None,
            flee_threat: None,
            movement_speed_mps: sheet.speed_mps,
            speed_multiplier: 1.0,
            heading: CanonicalHeading::from_xz(1.0, 0.0),
            spawn_seed: SpawnSeed::new(0),
            detection_check: 0,
            detection_left_s: 0.0,
            awareness_s: 0.0,
            endurance_s: 1.0,
            endurance_max_s: 1.0,
            progression: ActorProgression::empty(),
            actions: Vec::new(),
        }
    }

    pub fn actor_id(&self) -> ActorId {
        self.actor_id
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
        * if a.slow_s > 0.0 { 0.5 } else { 1.0 }
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
    pub fn game_data(&self) -> Option<&std::sync::Arc<crate::gamedata::GameData>> {
        self.game_data.as_ref()
    }
    pub(crate) fn mob_sheet(&self, id: &str) -> crate::combat::sheets::MobSheet {
        let data = self
            .game_data
            .as_ref()
            .expect("WorldCombat requires GameData for mob lookup");
        data.mob_sheet(id)
            .unwrap_or_else(|err| panic!("invalid combat mob {id:?}: {err}"))
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
        Ok(())
    }

    fn initialize_canonical_player(&mut self) {
        let data = self
            .game_data
            .as_ref()
            .expect("canonical player initialization requires GameData");
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
    }

    fn hydrate_hostile(&self, hostile: &mut WorldHostile) {
        let Some(data) = self.game_data.as_ref() else {
            return;
        };
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
    }

    fn hydrate_all_hostiles(&mut self) {
        let Some(data) = self.game_data.clone() else {
            return;
        };
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
        }
    }

    fn canonical_actors(&self, player_x: f64, player_z: f64) -> Vec<Actor> {
        let mut actors = Vec::with_capacity(self.hostiles.len() + 1);
        actors.push(Actor {
            id: 0,
            name: "Adventurer".into(),
            faction: self.player.faction.clone(),
            x: player_x,
            z: player_z,
            armor: 0,
            hp: self.player.resources.hp,
            mana: self.player.resources.mana,
            alive: !self.dead,
            progression: self.player.progression.clone(),
        });
        actors.extend(self.hostiles.iter().map(|hostile| Actor {
            id: hostile.actor_id.canonical,
            name: hostile.name.clone(),
            faction: hostile.faction.clone(),
            x: hostile.x,
            z: hostile.z,
            armor: hostile.armor,
            hp: hostile.hp,
            mana: hostile.progression.mana_max(),
            alive: hostile.alive,
            progression: hostile.progression.clone(),
        }));
        actors
    }

    fn sync_canonical_actors(&mut self, actors: Vec<Actor>, source: ActorId) {
        let mut iter = actors.into_iter();
        let player = iter.next().expect("canonical actor set contains player");
        self.player.resources.hp = player.hp;
        self.player.resources.hp_max = player.hp_max();
        self.player.resources.mana = player.mana;
        self.player.resources.mana_max = player.mana_max();
        self.player.progression = player.progression;
        if !player.alive && !self.dead {
            self.dead = true;
            self.lock = None;
            self.auto_cd = 999.0;
            self.slain_by.get_or_insert_with(|| "hostile".into());
            self.slain_hold_s = SLAIN_HOLD_S;
        }
        for (hostile, actor) in self.hostiles.iter_mut().zip(iter) {
            let took_damage = actor.hp < hostile.hp;
            hostile.hp = actor.hp;
            hostile.progression = actor.progression;
            if took_damage && actor.alive {
                hostile.provoked_by = Some(source);
                hostile.flee_threat = None;
                hostile.target = Some(source);
                hostile.awareness_s = AWARENESS_PERSIST_S;
                hostile.state = HostileState::Alerted;
            }
            if hostile.alive && !actor.alive {
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
        let data = self
            .game_data
            .clone()
            .expect("canonical action execution requires GameData");
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
    pub fn last_potion_heal(&self) -> i32 {
        self.last_potion_heal
    }
    pub fn ward_value(&self) -> f64 {
        self.ward
    }
    pub fn cast_time(&self) -> f64 {
        self.canonical_cast
            .as_ref()
            .map(|cast| cast.remaining_s)
            .unwrap_or(self.cast_t)
    }
    pub fn casting_action_id(&self) -> Option<&ActionId> {
        self.canonical_cast.as_ref().map(|cast| &cast.action_id)
    }
    pub fn strike_is_armed(&self) -> bool {
        self.strike_armed
    }
    pub fn ember_is_started(&self) -> bool {
        self.ember_started
    }
    pub fn last_rank_gate(&self) -> Option<crate::controls::RankGate> {
        self.last_rank_gate
    }
    pub(crate) fn cds(&self) -> &std::collections::BTreeMap<&'static str, f64> {
        &self.cds
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
    pub(crate) fn set_last_rank_gate(&mut self, value: Option<crate::controls::RankGate>) {
        self.last_rank_gate = value;
    }
    pub(crate) fn gcd_value(&self) -> f64 {
        self.gcd
    }
    pub(crate) fn set_gcd(&mut self, value: f64) {
        self.gcd = value;
    }
    pub(crate) fn set_strike_armed(&mut self, value: bool) {
        self.strike_armed = value;
    }
    pub(crate) fn set_ember_started(&mut self, value: bool) {
        self.ember_started = value;
    }
    pub(crate) fn ward_time(&self) -> f64 {
        self.ward_t
    }
    pub(crate) fn set_ward(&mut self, value: f64) {
        self.ward = value;
    }
    pub(crate) fn set_ward_time(&mut self, value: f64) {
        self.ward_t = value;
    }
    pub(crate) fn mark_time(&self) -> f64 {
        self.mark_t
    }
    pub(crate) fn mark_time_mut(&mut self) -> &mut f64 {
        &mut self.mark_t
    }
    pub(crate) fn set_mark_time(&mut self, value: f64) {
        self.mark_t = value;
    }
    pub(crate) fn second_wind_used(&self) -> bool {
        self.second_wind_used
    }
    pub(crate) fn set_second_wind_used(&mut self, value: bool) {
        self.second_wind_used = value;
    }
    pub(crate) fn set_last_potion_heal(&mut self, value: i32) {
        self.last_potion_heal = value;
    }
    pub(crate) fn busy_time(&self) -> f64 {
        self.busy
    }
    pub(crate) fn set_busy_time(&mut self, value: f64) {
        self.busy = value;
    }
    pub(crate) fn cast_kind_mut(&mut self) -> &mut Option<&'static str> {
        &mut self.cast_kind
    }
    pub(crate) fn cast_target_mut(&mut self) -> &mut Option<i32> {
        &mut self.cast_target
    }
    pub(crate) fn set_cast_kind(&mut self, value: Option<&'static str>) {
        self.cast_kind = value;
    }
    pub(crate) fn set_cast_time(&mut self, value: f64) {
        self.cast_t = value;
    }
    pub(crate) fn set_cast_target(&mut self, value: Option<i32>) {
        self.cast_target = value;
    }
    pub(crate) fn slain_hold_s_mut(&mut self) -> &mut f64 {
        &mut self.slain_hold_s
    }
    pub(crate) fn cds_mut(&mut self) -> &mut std::collections::BTreeMap<&'static str, f64> {
        &mut self.cds
    }
    pub(crate) fn log_mut(&mut self) -> &mut CombatLog {
        &mut self.log
    }
    pub(crate) fn fail_tell_timer_mut(&mut self) -> &mut f64 {
        &mut self.fail_tell_s
    }

    pub(crate) fn ward_t_mut(&mut self) -> &mut f64 {
        &mut self.ward_t
    }
    pub(crate) fn cast_time_mut(&mut self) -> &mut f64 {
        &mut self.cast_t
    }
    pub fn cast_kind(&self) -> Option<&'static str> {
        self.cast_kind
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
        let sheet = self.mob_sheet(mob_id.as_str());
        let hostile =
            WorldHostile::from_sheet(runtime_index, x, z, &sheet, mob_id.as_str(), home_x, home_z);
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
        let game_data = self.game_data.clone();
        let next_actor_id = self.next_actor_id;
        *self = Self::specialist_with_data(game_data, player.stats.level, player.stats.discipline);
        self.player = player;
        self.hostiles = hostiles;
        self.next_actor_id = next_actor_id;
        self.hydrate_all_hostiles();
    }
    pub fn reset_encounter_state(&mut self) {
        self.lock = None;
        self.cycle.clear();
        self.auto_cd = MELEE_SWING_S;
        self.strike_armed = false;
        self.ember_started = false;
        self.last_potion_heal = 0;
        self.busy = 0.0;
        self.gcd = 0.0;
        self.cds = super::verbs::empty_cds();
        self.cast_kind = None;
        self.cast_t = 0.0;
        self.cast_target = None;
        self.ward = 0.0;
        self.ward_t = 0.0;
        self.mark_t = 0.0;
        self.second_wind_used = false;
        self.last_rank_gate = None;
        self.special_tele.clear();
        self.fixed_accum_s = 0.0;
        self.canonical_cds.clear();
        self.canonical_cast = None;
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
        facing_x: f64,
        facing_z: f64,
    ) -> CombatStep {
        let strike = self.strike_armed;
        let target = self.lock.unwrap_or(-1);
        self.tick_hostile_ai(player_x, player_z, TICK);
        self.tick_verbs(player_x, player_z, TICK);
        let outgoing = self
            .tick_melee_auto(player_x, player_z, facing_x, facing_z, TICK)
            .map(|dealt| (dealt, target, strike));
        let previous_hp = self.player.resources.hp;
        let incoming = self
            .tick_incoming(player_x, player_z, TICK)
            .map(|hit| (previous_hp, hit));
        let specials = self.tick_special_attacks(player_x, player_z, TICK);
        CombatStep {
            outgoing,
            incoming,
            specials,
            resolutions: std::mem::take(&mut self.pending_resolutions),
        }
    }

    pub fn presentation_alpha(&self) -> f64 {
        (self.fixed_accum_s / TICK).clamp(0.0, 1.0)
    }

    pub fn reset_fixed_clock(&mut self) {
        self.fixed_accum_s = 0.0;
    }

    pub fn specialist(level: i32, discipline: Discipline) -> Self {
        Self::specialist_with_data(None, level, discipline)
    }

    pub fn specialist_with_game_data(
        game_data: std::sync::Arc<crate::gamedata::GameData>,
        level: i32,
        discipline: Discipline,
    ) -> Self {
        let mut combat = Self::specialist_with_data(Some(game_data), level, discipline);
        combat.initialize_canonical_player();
        combat
    }

    fn specialist_with_data(
        #[allow(dead_code)] game_data: Option<std::sync::Arc<crate::gamedata::GameData>>,
        level: i32,
        discipline: Discipline,
    ) -> Self {
        Self {
            game_data,
            lock: None,
            player: LivePlayer::specialist(level, discipline),
            cycle: Vec::new(),
            auto_cd: MELEE_SWING_S,
            last_auto_dealt: 0,
            hostiles: Vec::new(),
            next_actor_id: 1,
            strike_armed: false,
            ember_started: false,
            last_potion_heal: 0,
            busy: 0.0,
            gcd: 0.0,
            cds: super::verbs::empty_cds(),
            cast_kind: None,
            cast_t: 0.0,
            cast_target: None,
            ward: 0.0,
            ward_t: 0.0,
            mark_t: 0.0,
            second_wind_used: false,
            last_rank_gate: None,
            dead: false,
            slain_by: None,
            slain_hold_s: 0.0,
            last_incoming: None,
            log: CombatLog::new(),
            fail_tell: None,
            fail_tell_s: 0.0,
            special_tele: std::collections::BTreeMap::new(),
            fixed_accum_s: 0.0,
            canonical_cds: BTreeMap::new(),
            canonical_cast: None,
            pending_resolutions: Vec::new(),
        }
    }

    fn hostile_pairs(&self) -> Vec<(i32, f64, f64)> {
        self.hostiles
            .iter()
            .filter(|h| {
                h.alive
                    && self.game_data.as_ref().is_none_or(|data| {
                        data.factions_are_hostile(&self.player.faction, &h.faction)
                    })
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

    /// After the slain hold: full resources, Shaken live, swings can start again.
    pub fn finish_death_respawn(&mut self) {
        self.player.shaken = Some(Shaken::from_death());
        self.player.resources.hp = self.player.resources.hp_max;
        self.player.resources.mana = self.player.resources.mana_max;
        self.lock = None;
        self.auto_cd = MELEE_SWING_S;
        self.dead = false;
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
            self.auto_cd = 999.0;
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

    pub fn outgoing_raw(&self, raw: i32) -> i32 {
        let mut raw = raw;
        if let Some(shaken) = &self.player.shaken {
            raw = crate::combat::math::trunc(f64::from(raw) * shaken.outgoing_mult());
        }
        if self.mark_t > 0.0 {
            raw = crate::combat::math::trunc(f64::from(raw) * crate::combat::math::MARK_MULT);
        }
        raw
    }

    /// Melee auto only if lock is in 2.8 m and 120° cone. Uses sim melee_raw.
    pub fn tick_melee_auto(
        &mut self,
        player_x: f64,
        player_z: f64,
        facing_x: f64,
        facing_z: f64,
        dt: f64,
    ) -> Option<i32> {
        if self.dead {
            return None;
        }
        if self.game_data.is_some() {
            return None;
        }
        if self.player.stats.discipline != Discipline::Martial {
            return None;
        }
        let lock = self.lock?;
        let Some(hi) = self.hostiles.iter().position(|h| h.idx == lock && h.alive) else {
            self.lock = None;
            return None;
        };
        let h = &self.hostiles[hi];
        if !melee_auto_legal(player_x, player_z, facing_x, facing_z, h.x, h.z) {
            return None;
        }
        if self.cast_kind.is_some() || self.busy > 0.0 {
            return None;
        }
        self.auto_cd -= dt;
        if self.auto_cd > 0.0 {
            return None;
        }
        let strike = self.strike_armed;
        let mut raw = self.player.stats.melee_hit(strike);
        if strike {
            self.strike_armed = false;
        }
        raw = self.outgoing_raw(raw);
        let dealt = mitigation(f64::from(raw), self.hostiles[hi].armor);
        self.last_auto_dealt = dealt;
        self.apply_damage_to_hostile(lock, dealt);
        self.auto_cd += MELEE_SWING_S;
        Some(dealt)
    }

    /// Advances deterministic perception, flight, pursuit, endurance, and leash reset.
    pub fn tick_hostile_ai(&mut self, player_x: f64, player_z: f64, dt: f64) {
        if self.dead || dt <= 0.0 {
            return;
        }
        self.hydrate_all_hostiles();
        let data = self.game_data.as_ref();
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

        for actor in &mut self.hostiles {
            actor.previous_x = actor.x;
            actor.previous_z = actor.z;
            actor.previous_heading = actor.heading;
            if !actor.alive {
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
                } else if actor.root_s <= 0.0 {
                    let step =
                        (actor.movement_speed_mps * actor.speed_multiplier * dt).min(distance);
                    actor.heading = CanonicalHeading::from_xz(dx, dz);
                    actor.x += dx / distance * step;
                    actor.z += dz / distance * step;
                }
                continue;
            }
            let hostile_to = |faction: &FactionId| {
                data.map_or(
                    actor.faction != *faction
                        && actor.faction.as_str() != "neutral"
                        && faction.as_str() != "neutral",
                    |d| {
                        !d.faction(&actor.faction)
                            .expect("validated observer faction")
                            .is_neutral()
                            && !d
                                .faction(faction)
                                .expect("validated candidate faction")
                                .is_neutral()
                            && d.factions_are_hostile(&actor.faction, faction)
                    },
                )
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
                            actor.flee_threat = Some(id);
                            actor.target = None;
                            actor.state = HostileState::Fleeing;
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
                    if actor.root_s > 0.0 {
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
                    if distance <= actor.reach_m {
                        actor.state = HostileState::Attacking;
                        false
                    } else if actor.root_s <= 0.0 {
                        actor.state = HostileState::Pursuing;
                        let speed = movement_speed(actor);
                        let step = (speed * dt).min((distance - actor.reach_m).max(0.0));
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

    /// Advances all hostile special attacks and resolves their effects.
    /// Presentation receives the result; it never owns special-attack gameplay state.
    pub fn tick_special_attacks(
        &mut self,
        player_x: f64,
        player_z: f64,
        dt: f64,
    ) -> Vec<SpecialAttackEvent> {
        if self.dead || dt <= 0.0 {
            self.special_tele.clear();
            return Vec::new();
        }
        let grit = self.player.stats.attrs.grit;
        let mut events = Vec::new();
        let ids: Vec<i32> = self
            .hostiles
            .iter()
            .filter(|h| h.alive)
            .map(|h| h.idx)
            .collect();
        for idx in ids {
            let Some(slot) = self.hostiles.iter().position(|h| h.idx == idx && h.alive) else {
                self.special_tele.remove(&idx);
                continue;
            };
            let h = &self.hostiles[slot];
            if self
                .game_data
                .as_ref()
                .and_then(|data| data.mob(&MobId::new(h.mob_id.clone())))
                .is_some()
            {
                self.special_tele.remove(&idx);
                continue;
            }
            let sheet = crate::combat::sheets::mob_sheet(&h.mob_id).unwrap_or_else(|err| {
                panic!("missing special-attack sheet for {}: {err}", h.mob_id)
            });
            let (Some(raw), Some(telegraph)) = (sheet.slam_damage, sheet.telegraph_s) else {
                self.special_tele.remove(&idx);
                continue;
            };
            if h.stun_s > 0.0 {
                self.special_tele.remove(&idx);
                continue;
            }
            let distance = (player_x - h.x).hypot(player_z - h.z);
            let range = if h.mob_id == "orc_skull" {
                SKULL_BOLT_RANGE_M
            } else {
                MAGE_BOLT_RANGE_M
            };
            if distance > range {
                continue;
            }
            let cue = if h.mob_id == "orc_skull" {
                SpecialAttackCue::Weapon
            } else {
                SpecialAttackCue::SpellcastShoot
            };
            if let Some(remaining) = self.special_tele.get(&idx).copied() {
                let next = remaining - dt;
                if next > 1e-12 {
                    self.special_tele.insert(idx, next);
                    continue;
                }
                self.special_tele.remove(&idx);
                let previous_player_hp = self.player.resources.hp;
                let dealt = mitigation(f64::from(raw), grit);
                let hit = self.apply_damage_to_player(dealt, h.name.clone());
                events.push(SpecialAttackEvent {
                    attacker_idx: idx,
                    cue,
                    hit: Some(hit),
                    previous_player_hp,
                });
            } else {
                self.special_tele.insert(idx, telegraph);
                events.push(SpecialAttackEvent {
                    attacker_idx: idx,
                    cue,
                    hit: None,
                    previous_player_hp: self.player.resources.hp,
                });
            }
        }
        events
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
        let mut ready = None;
        for (index, actor) in self.hostiles.iter_mut().enumerate() {
            if !actor.alive || actor.stun_s > 0.0 || actor.state != HostileState::Attacking {
                continue;
            }
            let Some(target_id) = actor.target else {
                panic!("attacking actor {} has no target", actor.mob_id);
            };
            actor.swing_cd -= dt;
            if actor.swing_cd > 0.0 {
                continue;
            }
            actor.swing_cd += actor.swing_s;
            let action_id = actor
                .actions
                .first()
                .cloned()
                .unwrap_or_else(|| panic!("actor {} has no authored actions", actor.mob_id));
            ready = Some((
                index + 1,
                actor.actor_id,
                actor.name.clone(),
                target_id,
                action_id,
            ));
            break;
        }
        let (caster, caster_id, name, target_id, action_id) = ready?;
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

    /// Mob autos. Same mitigation as the sim. Reach is the sheet reach (1.8 m).
    pub fn tick_incoming(&mut self, player_x: f64, player_z: f64, dt: f64) -> Option<IncomingHit> {
        if self.dead || dt <= 0.0 {
            return None;
        }
        if self.game_data.is_some() {
            return self.tick_canonical_actor_attack(player_x, player_z, dt);
        }
        let grit = self.player.stats.attrs.grit;
        let mut last = None;
        for i in 0..self.hostiles.len() {
            let Some((dealt, name)) = ({
                let h = &mut self.hostiles[i];
                if !h.alive || h.stun_s > 0.0 || h.state != HostileState::Attacking {
                    None
                } else {
                    let distance = (player_x - h.x).hypot(player_z - h.z);
                    if distance > h.reach_m {
                        None
                    } else {
                        h.swing_cd -= dt;
                        if h.swing_cd > 0.0 {
                            None
                        } else {
                            let dealt = mitigation(f64::from(h.damage), grit);
                            h.swing_cd += h.swing_s;
                            Some((dealt, h.name.clone()))
                        }
                    }
                }
            }) else {
                continue;
            };
            let hit = self.apply_damage_to_player(dealt, name);
            let killed = hit.killed;
            last = Some(hit);
            if killed {
                break;
            }
        }
        last
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_wolf(dmg: i32) -> WorldHostile {
        let mut sheet = crate::combat::sheets::wolf_sheet();
        sheet.damage = dmg;
        let mut wolf = WorldHostile::from_sheet(0, 1.0, 0.0, &sheet, "wolf", 1.0, 0.0);
        wolf.swing_cd = 0.0;
        wolf
    }

    fn canonical_combat() -> WorldCombat {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../data/OrrunGameData.xml");
        let data =
            std::sync::Arc::new(crate::gamedata::GameData::load(path).expect("canonical GameData"));
        let sheet = data.mob_sheet("wolf").expect("canonical wolf sheet");
        let mut combat = WorldCombat::specialist_with_game_data(data, 1, Discipline::Martial);
        let mut wolf = WorldHostile::from_sheet(0, 1.5, 0.0, &sheet, "wolf", 1.5, 0.0);
        wolf.swing_cd = 0.0;
        combat.add_hostile(wolf);
        combat.set_lock(Some(0));
        combat
    }

    #[test]
    fn deterministic_spawn_draws_and_heading_are_stable() {
        let seed = SpawnSeed::new(42);
        assert_eq!(deterministic_unit(seed, 7), deterministic_unit(seed, 7));
        assert_ne!(deterministic_unit(seed, 7), deterministic_unit(seed, 8));
        let heading = CanonicalHeading::from_xz(3.0, 4.0);
        assert!((heading.x() - 0.6).abs() < 1e-12);
        assert!((heading.z() - 0.8).abs() < 1e-12);
    }

    #[test]
    fn observer_facing_weights_sight_but_not_hearing() {
        let mut wolf = dummy_wolf(1);
        wolf.x = 0.0;
        wolf.z = 0.0;
        wolf.aggro.sight_m = 20.0;
        wolf.aggro.hear_m = 0.0;
        wolf.heading = CanonicalHeading::from_xz(1.0, 0.0);
        assert!(detection_probability(&wolf, 10.0, 0.0) > detection_probability(&wolf, -10.0, 0.0));
        wolf.aggro.sight_m = 0.0;
        wolf.aggro.hear_m = 20.0;
        assert_eq!(
            detection_probability(&wolf, 10.0, 0.0),
            detection_probability(&wolf, -10.0, 0.0)
        );
    }

    #[test]
    fn fleeing_damage_immediately_retaliates() {
        let mut c = WorldCombat::specialist(1, Discipline::Martial);
        let mut wolf = dummy_wolf(1);
        wolf.state = HostileState::Fleeing;
        wolf.flee_threat = Some(ActorId::from_runtime_index(2));
        c.add_hostile(wolf);
        c.apply_damage_to_hostile(0, 1);
        assert_eq!(c.hostiles[0].state, HostileState::Alerted);
        assert_eq!(c.hostiles[0].flee_threat, None);
        assert_eq!(c.hostiles[0].target, Some(ActorId::PLAYER));
    }

    #[test]
    fn exhausted_pursuit_keeps_moving_at_reduced_speed() {
        let mut c = WorldCombat::specialist(1, Discipline::Martial);
        let mut wolf = dummy_wolf(1);
        wolf.x = 10.0;
        wolf.home_x = 10.0;
        wolf.aggro.leash_m = 100.0;
        wolf.target = Some(ActorId::PLAYER);
        wolf.awareness_s = 10.0;
        wolf.state = HostileState::Pursuing;
        wolf.endurance_s = 0.0;
        wolf.endurance_max_s = 5.0;
        wolf.slow_s = 1.0;
        wolf.speed_multiplier = 1.2;
        assert_eq!(
            wolf.effective_movement_speed_mps(),
            wolf.movement_speed_mps * 1.2 * EXHAUSTED_SPEED_RATIO * 0.5
        );
        wolf.detection_left_s = 100.0;
        c.add_hostile(wolf);
        let before = c.hostiles[0].x;
        c.tick_hostile_ai(0.0, 0.0, 1.0);
        let moved = before - c.hostiles[0].x;
        assert!(moved > 0.0);
        assert!((moved - c.hostiles[0].effective_movement_speed_mps()).abs() < 1e-12);
        assert_eq!(c.hostiles[0].endurance_seconds(), 0.0);
    }

    #[test]
    fn canonical_live_actions_train_and_apply_from_game_data() {
        let mut combat = canonical_combat();
        assert!(combat.press_action(&ActionId::new("strike"), 0.0, 0.0, 1.0, 0.0,));
        assert_eq!(combat.hostiles[0].hp(), 62.0);
        assert_eq!(
            combat
                .player_progression()
                .skill_xp(&crate::gamedata::SkillId::new("slashing_damage")),
            Some(10)
        );
        assert_eq!(combat.pending_resolutions.len(), 1);

        let mana_before = combat.player.resources.mana();
        assert!(combat.press_action(&ActionId::new("fire_bolt"), 0.0, 0.0, 1.0, 0.0,));
        combat.tick_verbs(0.0, 0.0, 1.2);
        assert_eq!(combat.hostiles[0].hp(), 48.0);
        assert!(combat.player.resources.mana() < mana_before);
        assert_eq!(
            combat
                .player_progression()
                .skill_xp(&crate::gamedata::SkillId::new("fire_damage")),
            Some(10)
        );
        assert_eq!(combat.player_progression().mana_xp(), 3);

        combat.player.resources.hp = 50.0;
        assert!(combat.press_action(&ActionId::new("mend"), 0.0, 0.0, 1.0, 0.0,));
        combat.tick_verbs(0.0, 0.0, 2.5);
        assert_eq!(combat.player.resources.hp(), 75.0);
        assert_eq!(
            combat
                .player_progression()
                .skill_xp(&crate::gamedata::SkillId::new("healing")),
            Some(10)
        );
    }

    #[test]
    fn canonical_damage_alerts_idle_hostile_and_uses_game_data_name() {
        let mut combat = canonical_combat();
        assert_eq!(combat.hostiles[0].name, "Wolf");
        assert_eq!(combat.hostiles[0].state, HostileState::Idle);
        assert!(combat.press_action(&ActionId::new("fire_bolt"), 0.0, 0.0, 1.0, 0.0,));
        combat.tick_verbs(0.0, 0.0, 1.2);
        assert_eq!(combat.hostiles[0].state, HostileState::Alerted);
    }

    #[test]
    fn canonical_wolf_attack_uses_same_resolver_and_trains_hp() {
        let mut combat = canonical_combat();
        combat.hostiles[0].state = HostileState::Attacking;
        combat.hostiles[0].target = Some(ActorId::PLAYER);
        let before = combat.player.resources.hp();
        let hp_xp_before = combat.player_progression().hp_xp();
        let hit = combat
            .tick_incoming(0.0, 0.0, 0.1)
            .expect("canonical incoming hit");
        assert_eq!(hit.dealt, 8);
        assert_eq!(combat.player.resources.hp(), before - 8.0);
        assert!(combat.player_progression().hp_xp() > hp_xp_before);
        assert_eq!(combat.pending_resolutions.len(), 1);
    }

    #[test]
    fn active_actor_acquires_outside_faction_and_leashes() {
        let mut c = canonical_combat();
        c.hostiles[0].x = 10.0;
        c.hostiles[0].home_x = 10.0;
        c.hostiles[0].heading = CanonicalHeading::from_xz(-1.0, 0.0);
        c.hostiles[0].aggro.sight_m = 100.0;
        c.hostiles[0].aggro.leash_m = 9.0;
        c.hostiles[0].spawn_seed = SpawnSeed::new(0);
        c.hostiles[0].detection_left_s = 0.0;
        c.tick_hostile_ai(0.0, 0.0, 0.1);
        assert_eq!(c.hostiles[0].target, Some(ActorId::PLAYER));
        assert_eq!(c.hostiles[0].state, HostileState::Pursuing);
        let before = c.hostiles[0].x;
        c.tick_hostile_ai(0.0, 0.0, 1.0);
        assert!(c.hostiles[0].x < before);
        c.hostiles[0].x = 1.7;
        c.tick_hostile_ai(0.0, 0.0, 0.1);
        assert_eq!(c.hostiles[0].state, HostileState::Attacking);
        c.hostiles[0].x = 20.0;
        c.tick_hostile_ai(0.0, 0.0, 0.1);
        assert_eq!(c.hostiles[0].state, HostileState::Leashing);
    }

    #[test]
    fn passive_prey_flees_active_player_but_not_passive_or_neutral_actor() {
        fn perception_fixture(candidate_mode: MobMode, candidate_faction: &str) -> WorldCombat {
            let mut c = canonical_combat();
            let deer_sheet = c.mob_sheet("deer");
            c.clear_hostiles();
            c.add_hostile(WorldHostile::from_sheet(
                0,
                0.0,
                0.0,
                &deer_sheet,
                "deer",
                0.0,
                0.0,
            ));
            let wolf_sheet = c.mob_sheet("wolf");
            c.add_hostile(WorldHostile::from_sheet(
                1,
                3.0,
                0.0,
                &wolf_sheet,
                "wolf",
                3.0,
                0.0,
            ));
            c.player.faction = FactionId::new("prey");
            c.hostiles[0].heading = CanonicalHeading::from_xz(1.0, 0.0);
            c.hostiles[0].aggro.sight_m = 100.0;
            c.hostiles[0].spawn_seed = SpawnSeed::new(0);
            c.hostiles[0].detection_left_s = 0.0;
            c.hostiles[1].mode = candidate_mode;
            c.hostiles[1].faction = FactionId::new(candidate_faction);
            c
        }

        let mut player_threat = perception_fixture(MobMode::Passive, "prey");
        player_threat.player.faction = FactionId::new("citizen");
        player_threat.hostiles[1].faction = FactionId::new("prey");
        player_threat.tick_hostile_ai(3.0, 0.0, 0.1);
        assert_eq!(player_threat.hostiles[0].state, HostileState::Fleeing);
        assert_eq!(player_threat.hostiles[0].flee_threat, Some(ActorId::PLAYER));
        assert_eq!(player_threat.hostiles[0].target, None);

        let mut passive_threat = perception_fixture(MobMode::Passive, "predator");
        passive_threat.tick_hostile_ai(1000.0, 0.0, 0.1);
        assert_eq!(passive_threat.hostiles[0].state, HostileState::Idle);
        assert_eq!(passive_threat.hostiles[0].flee_threat, None);

        let mut neutral_threat = perception_fixture(MobMode::Active, "neutral");
        neutral_threat.tick_hostile_ai(1000.0, 0.0, 0.1);
        assert_eq!(neutral_threat.hostiles[0].state, HostileState::Idle);
        assert_eq!(neutral_threat.hostiles[0].flee_threat, None);
    }
    #[test]
    fn passive_actor_retaliates_through_authoritative_damage_api() {
        let mut c = canonical_combat();
        c.hostiles[0].mode = MobMode::Passive;
        c.hostiles[0].state = HostileState::Fleeing;
        c.hostiles[0].flee_threat = Some(ActorId::from_runtime_index(2));
        c.apply_damage_to_hostile(0, 1);
        assert_eq!(c.hostiles[0].provoked_by, Some(ActorId::PLAYER));
        assert_eq!(c.hostiles[0].target, Some(ActorId::PLAYER));
        assert_eq!(c.hostiles[0].flee_threat, None);
        assert_eq!(c.hostiles[0].state, HostileState::Alerted);
    }
    #[test]
    fn animal_faction_and_mode_matrix_is_authored() {
        let c = canonical_combat();
        let data = c.game_data.as_ref().expect("canonical data");
        for (id, faction, mode) in [
            ("wolf", "predator", MobMode::Active),
            ("fox", "predator", MobMode::Active),
            ("deer", "prey", MobMode::Passive),
            ("stag", "prey", MobMode::Passive),
            ("horse", "prey", MobMode::Passive),
            ("horse_white", "prey", MobMode::Passive),
            ("cow", "citizen", MobMode::Passive),
            ("bull", "citizen", MobMode::Passive),
            ("donkey", "citizen", MobMode::Passive),
            ("alpaca", "citizen", MobMode::Passive),
            ("husky", "citizen", MobMode::Passive),
            ("shiba", "citizen", MobMode::Passive),
        ] {
            let mob = data
                .mob(&MobId::new(id))
                .unwrap_or_else(|| panic!("missing {id}"));
            assert_eq!(mob.faction().as_str(), faction, "{id} faction");
            assert_eq!(mob.mode(), mode, "{id} mode");
        }
        assert!(
            !data.factions_are_hostile(&FactionId::new("predator"), &FactionId::new("predator"))
        );
        assert!(data.factions_are_hostile(&FactionId::new("predator"), &FactionId::new("prey")));
        assert!(data.factions_are_hostile(&FactionId::new("predator"), &FactionId::new("citizen")));
        assert!(!data.factions_are_hostile(&FactionId::new("citizen"), &FactionId::new("citizen")));
    }

    #[test]
    fn predator_selects_nearest_outside_faction_actor() {
        let mut c = canonical_combat();
        c.hostiles[0].x = 0.0;
        c.hostiles[0].z = 0.0;
        c.hostiles[0].heading = CanonicalHeading::from_xz(1.0, 0.0);
        c.hostiles[0].aggro.sight_m = 100.0;
        c.hostiles[0].spawn_seed = SpawnSeed::new(0);
        c.hostiles[0].detection_left_s = 0.0;
        let deer_sheet = c.mob_sheet("deer");
        c.add_hostile(WorldHostile::from_sheet(
            1,
            3.0,
            0.0,
            &deer_sheet,
            "deer",
            3.0,
            0.0,
        ));
        c.tick_hostile_ai(8.0, 0.0, 0.1);
        assert_eq!(c.hostiles[0].target, Some(c.hostiles[1].actor_id));
        assert_eq!(c.hostiles[1].target, None);
    }

    #[test]
    fn mob_on_mob_resolution_damages_selected_actor() {
        let mut c = canonical_combat();
        let deer_sheet = c.mob_sheet("deer");
        c.add_hostile(WorldHostile::from_sheet(
            1,
            1.0,
            0.0,
            &deer_sheet,
            "deer",
            1.0,
            0.0,
        ));
        let before = c.hostiles[1].hp();
        let resolution = c
            .execute_canonical(
                1,
                &ActionId::new("strike"),
                TargetSelection::Single(2),
                20.0,
                0.0,
            )
            .expect("wolf attacks deer");
        assert!(c.hostiles[1].hp() < before);
        assert_eq!(resolution.effects[0].targets, vec![2]);
        assert_eq!(c.hostiles[1].provoked_by, Some(c.hostiles[0].actor_id));
    }

    #[test]
    fn canonical_mob_api_uses_gamedata_and_monotonic_actor_identity() {
        let mut c = canonical_combat();
        let first = c.hostiles[0].actor_id();
        c.clear_hostiles();
        let second = c.add_canonical_mob(&MobId::new("orc"), 0, 1.5, 0.0, 1.5, 0.0);
        assert_eq!(c.hostiles[0].mob_id, "orc");
        assert_eq!(c.hostiles[0].name, "Orc");
        assert!(second > first);
    }

    #[test]
    fn actor_identity_is_not_reused_with_runtime_slot() {
        let mut c = canonical_combat();
        let stale_id = c.hostiles[0].actor_id();
        c.clear_hostiles();
        c.add_hostile(dummy_wolf(1));
        let replacement_id = c.hostiles[0].actor_id();

        assert_eq!(c.hostiles[0].idx, 0);
        assert_ne!(replacement_id, stale_id);
    }

    #[test]
    fn stale_deactivation_cannot_remove_reused_runtime_slot() {
        let mut c = canonical_combat();
        let stale_id = c.hostiles[0].actor_id();
        c.clear_hostiles();
        c.add_hostile(dummy_wolf(1));
        let replacement_id = c.hostiles[0].actor_id();
        c.set_lock(Some(0));

        c.deactivate_actors(&[stale_id]);

        assert_eq!(c.hostiles.len(), 1);
        assert_eq!(c.hostiles[0].actor_id(), replacement_id);
        assert_eq!(c.lock_id(), Some(0));
    }

    #[test]
    fn streaming_deactivation_is_not_death() {
        let mut c = canonical_combat();
        let actor_id = c.hostiles[0].actor_id;
        c.deactivate_actors(&[actor_id]);
        assert!(c.hostiles.is_empty());
        assert!(c.pending_resolutions.is_empty());
        assert!(!c.dead);
    }

    #[test]
    fn perception_checks_use_staggered_one_second_cadence() {
        let mut c = WorldCombat::specialist(1, Discipline::Martial);
        c.add_hostile(dummy_wolf(1));
        let mut second = dummy_wolf(1);
        second.idx = 1;
        c.add_hostile(second);
        c.hostiles[0].detection_left_s = 0.25;
        c.hostiles[1].detection_left_s = 0.75;
        c.hostiles[0].aggro.sight_m = 0.0;
        c.hostiles[0].aggro.hear_m = 0.0;
        c.hostiles[1].aggro.sight_m = 0.0;
        c.hostiles[1].aggro.hear_m = 0.0;

        c.tick_hostile_ai(100.0, 100.0, 0.24);
        assert_eq!(
            (c.hostiles[0].detection_check, c.hostiles[1].detection_check),
            (0, 0)
        );
        c.tick_hostile_ai(100.0, 100.0, 0.02);
        assert_eq!(
            (c.hostiles[0].detection_check, c.hostiles[1].detection_check),
            (1, 0)
        );
        c.tick_hostile_ai(100.0, 100.0, 0.50);
        assert_eq!(
            (c.hostiles[0].detection_check, c.hostiles[1].detection_check),
            (1, 1)
        );
        c.tick_hostile_ai(100.0, 100.0, 0.49);
        assert_eq!(
            (c.hostiles[0].detection_check, c.hostiles[1].detection_check),
            (2, 1)
        );
    }

    #[test]
    fn sight_probability_has_exact_front_side_and_rear_weights() {
        let mut wolf = dummy_wolf(1);
        wolf.x = 0.0;
        wolf.z = 0.0;
        wolf.heading = CanonicalHeading::from_xz(1.0, 0.0);
        wolf.aggro.sight_m = 20.0;
        wolf.aggro.hear_m = 0.0;
        assert_eq!(detection_probability(&wolf, 10.0, 0.0), 0.5);
        assert_eq!(detection_probability(&wolf, 0.0, 10.0), 0.275);
        assert_eq!(detection_probability(&wolf, -10.0, 0.0), 0.1);
    }

    #[test]
    fn provoked_actor_keeps_target_after_awareness_expires() {
        let mut c = WorldCombat::specialist(1, Discipline::Martial);
        let mut wolf = dummy_wolf(1);
        wolf.x = 10.0;
        wolf.home_x = 10.0;
        wolf.aggro.leash_m = 1_000.0;
        c.add_hostile(wolf);
        c.apply_damage_to_hostile(0, 1);
        c.hostiles[0].detection_left_s = 100.0;

        c.tick_hostile_ai(0.0, 0.0, AWARENESS_PERSIST_S + 0.1);

        assert_eq!(c.hostiles[0].target, Some(ActorId::PLAYER));
        assert_eq!(c.hostiles[0].provoked_by, Some(ActorId::PLAYER));
        assert!(matches!(
            c.hostiles[0].state,
            HostileState::Pursuing | HostileState::Attacking
        ));
    }

    #[test]
    fn successful_perception_refreshes_awareness_then_expiry_ends_chase() {
        let mut c = WorldCombat::specialist(1, Discipline::Martial);
        let mut wolf = dummy_wolf(1);
        wolf.x = 10.0;
        wolf.home_x = 10.0;
        wolf.aggro.leash_m = 1_000.0;
        c.add_hostile(wolf);
        c.hostiles[0].heading = CanonicalHeading::from_xz(-1.0, 0.0);
        c.hostiles[0].aggro.sight_m = 100.0;
        c.hostiles[0].aggro.hear_m = 100.0;
        c.hostiles[0].detection_left_s = 0.0;
        c.tick_hostile_ai(10.0, 0.0, 0.1);
        assert_eq!(c.hostiles[0].awareness_s, AWARENESS_PERSIST_S);
        assert_eq!(c.hostiles[0].target, Some(ActorId::PLAYER));

        c.hostiles[0].detection_left_s = 100.0;
        c.tick_hostile_ai(100.0, 0.0, AWARENESS_PERSIST_S - 0.1);
        assert_ne!(c.hostiles[0].state, HostileState::Idle);
        c.tick_hostile_ai(100.0, 0.0, 0.11);
        assert_eq!(c.hostiles[0].state, HostileState::Idle);
        assert_eq!(c.hostiles[0].target, None);
    }

    #[test]
    fn presentation_pose_interpolates_without_mutating_canonical_position() {
        let mut wolf = dummy_wolf(1);
        wolf.previous_x = 0.0;
        wolf.previous_z = 0.0;
        wolf.x = 1.0;
        wolf.z = 2.0;
        wolf.previous_heading = CanonicalHeading::from_xz(1.0, 0.0);
        wolf.heading = CanonicalHeading::from_xz(0.0, 1.0);

        let (x, z, heading) = wolf.presented_pose(0.5);

        assert!((x - 0.5).abs() < 1e-12);
        assert!((z - 1.0).abs() < 1e-12);
        assert!((heading.x() - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-12);
        assert!((heading.z() - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-12);
        assert_eq!((wolf.x, wolf.z), (1.0, 2.0));
    }

    #[test]
    fn speed_multiplier_is_same_seed_stable_different_seed_varied_and_bounded() {
        fn multiplier(seed: u64) -> (f64, f64) {
            let mut c = canonical_combat();
            c.clear_hostiles();
            let sheet = c.mob_sheet("wolf");
            c.register_hostile(
                WorldHostile::from_sheet(0, 0.0, 0.0, &sheet, "wolf", 0.0, 0.0),
                SpawnSeed::new(seed),
                CanonicalHeading::from_xz(1.0, 0.0),
            );
            let variance = c
                .game_data
                .as_ref()
                .unwrap()
                .mob(&MobId::new("wolf"))
                .unwrap()
                .speed_variance_ratio()
                .as_ratio();
            (c.hostiles[0].speed_multiplier(), variance)
        }
        let (same_a, variance) = multiplier(42);
        let (same_b, _) = multiplier(42);
        assert_eq!(same_a, same_b);
        assert!((1.0 - variance..=1.0 + variance).contains(&same_a));
        let different: Vec<f64> = (43..51).map(|seed| multiplier(seed).0).collect();
        assert!(different
            .iter()
            .all(|value| (1.0 - variance..=1.0 + variance).contains(value)));
        assert!(different.iter().any(|value| *value != same_a));
    }

    #[test]
    fn endurance_drains_only_for_actual_pursuit_or_flight_movement_and_recovers_otherwise() {
        fn configured(state: HostileState, passive: bool) -> WorldCombat {
            let mut c = WorldCombat::specialist(1, Discipline::Martial);
            let mut wolf = dummy_wolf(1);
            wolf.x = 10.0;
            wolf.home_x = 10.0;
            wolf.aggro.leash_m = 1_000.0;
            wolf.state = state;
            wolf.awareness_s = 10.0;
            wolf.endurance_s = 4.0;
            wolf.endurance_max_s = 5.0;
            wolf.target = (!passive).then_some(ActorId::PLAYER);
            wolf.flee_threat = passive.then_some(ActorId::PLAYER);
            c.add_hostile(wolf);
            c.hostiles[0].detection_left_s = 100.0;
            c
        }
        let mut pursuing = configured(HostileState::Pursuing, false);
        pursuing.tick_hostile_ai(0.0, 0.0, 1.0);
        assert_eq!(pursuing.hostiles[0].endurance_s, 3.0);

        let mut fleeing = configured(HostileState::Fleeing, true);
        fleeing.tick_hostile_ai(0.0, 0.0, 1.0);
        assert_eq!(fleeing.hostiles[0].endurance_s, 3.0);

        let mut rooted = configured(HostileState::Pursuing, false);
        rooted.hostiles[0].root_s = 2.0;
        rooted.tick_hostile_ai(0.0, 0.0, 1.0);
        assert_eq!(rooted.hostiles[0].endurance_s, 5.0);

        let mut attacking = configured(HostileState::Attacking, false);
        attacking.hostiles[0].x = 1.0;
        attacking.tick_hostile_ai(0.0, 0.0, 1.0);
        assert_eq!(attacking.hostiles[0].endurance_s, 5.0);
    }

    #[test]
    fn exhausted_flight_continues_at_reduced_speed() {
        let mut c = WorldCombat::specialist(1, Discipline::Martial);
        let mut wolf = dummy_wolf(1);
        wolf.x = 1.0;
        wolf.home_x = 1.0;
        wolf.aggro.leash_m = 1_000.0;
        wolf.state = HostileState::Fleeing;
        wolf.flee_threat = Some(ActorId::PLAYER);
        wolf.awareness_s = 10.0;
        wolf.endurance_s = 0.0;
        wolf.endurance_max_s = 5.0;
        c.add_hostile(wolf);
        c.hostiles[0].detection_left_s = 100.0;
        let before = c.hostiles[0].x;
        let expected = c.hostiles[0].movement_speed_mps * EXHAUSTED_SPEED_RATIO;
        c.tick_hostile_ai(0.0, 0.0, 1.0);
        assert!((c.hostiles[0].x - before - expected).abs() < 1e-12);
        assert_eq!(c.hostiles[0].endurance_s, 0.0);
    }

    #[test]
    fn passive_flight_excludes_passive_same_faction_neutral_and_dead_candidates() {
        let mut c = canonical_combat();
        c.clear_hostiles();
        let deer_sheet = c.mob_sheet("deer");
        c.add_hostile(WorldHostile::from_sheet(
            0,
            0.0,
            0.0,
            &deer_sheet,
            "deer",
            0.0,
            0.0,
        ));
        let wolf_sheet = c.mob_sheet("wolf");
        for (idx, x) in [(1, 1.0), (2, 2.0), (3, 3.0), (4, 4.0)] {
            c.add_hostile(WorldHostile::from_sheet(
                idx,
                x,
                0.0,
                &wolf_sheet,
                "wolf",
                x,
                0.0,
            ));
        }
        c.player.faction = FactionId::new("prey");
        c.hostiles[0].heading = CanonicalHeading::from_xz(1.0, 0.0);
        c.hostiles[0].aggro.sight_m = 100.0;
        c.hostiles[0].aggro.hear_m = 100.0;
        c.hostiles[0].detection_left_s = 0.0;
        c.hostiles[1].mode = MobMode::Passive;
        c.hostiles[1].faction = FactionId::new("predator");
        c.hostiles[2].mode = MobMode::Active;
        c.hostiles[2].faction = FactionId::new("prey");
        c.hostiles[3].mode = MobMode::Active;
        c.hostiles[3].faction = FactionId::new("neutral");
        c.hostiles[4].mode = MobMode::Active;
        c.hostiles[4].faction = FactionId::new("predator");
        c.hostiles[4].alive = false;
        c.tick_hostile_ai(1_000.0, 0.0, 0.1);
        assert_eq!(c.hostiles[0].state, HostileState::Idle);
        assert_eq!(c.hostiles[0].flee_threat, None);

        c.hostiles[4].alive = true;
        c.hostiles[0].detection_left_s = 0.0;
        c.tick_hostile_ai(1_000.0, 0.0, 0.1);
        assert_eq!(c.hostiles[0].state, HostileState::Fleeing);
        assert_eq!(c.hostiles[0].flee_threat, Some(c.hostiles[4].actor_id));
    }

    #[test]
    fn incoming_can_kill_and_holds_before_respawn() {
        let mut c = WorldCombat::specialist(1, Discipline::Martial);
        let mut wolf = dummy_wolf(80);
        wolf.state = HostileState::Attacking;
        c.add_hostile(wolf);
        c.player.resources.hp = 10.0;
        let hit = c.tick_incoming(0.0, 0.0, 0.1).expect("hit");
        assert!(hit.killed);
        assert!(c.dead);
        assert_eq!(c.slain_by.as_deref(), Some("wolf-spider"));
        assert!((c.slain_hold_s - SLAIN_HOLD_S).abs() < 1e-9);
        assert!(c.tick_melee_auto(0.0, 0.0, 1.0, 0.0, 2.0).is_none());
        assert!(c.tick_incoming(0.0, 0.0, 0.1).is_none());
    }

    #[test]
    fn shaken_ticks_and_multiplies_outgoing_after_clear_dead() {
        let mut c = WorldCombat::specialist(1, Discipline::Martial);
        c.add_hostile(dummy_wolf(10));
        c.finish_death_respawn();
        c.lock = Some(0);
        assert!(!c.dead);
        assert!(c.player.shaken.is_some());
        let dealt = c.tick_melee_auto(0.0, 0.0, 1.0, 0.0, 2.0).expect("swing");
        let raw = c.player.stats.melee_hit(false);
        let shaken_raw = crate::combat::math::trunc(f64::from(raw) * SHAKEN_DMG);
        let want = mitigation(f64::from(shaken_raw), c.hostiles[0].armor);
        let unshaken = mitigation(f64::from(raw), c.hostiles[0].armor);
        assert_eq!(dealt, want);
        assert_ne!(dealt, unshaken);
        c.tick_verbs(0.0, 0.0, 10.0);
        let left = c.player.shaken.as_ref().unwrap().remaining_s;
        assert!((left - (SHAKEN_S - 10.0)).abs() < 1e-6);
    }

    #[test]
    fn slain_by_clears_after_respawn() {
        let mut c = WorldCombat::specialist(1, Discipline::Martial);
        c.slain_by = Some("wolf-spider".into());
        c.dead = true;
        c.finish_death_respawn();
        assert!(c.slain_by.is_none());
        assert!(!c.dead);
    }

    #[test]
    fn player_save_restore_is_exact_and_atomic() {
        let mut combat = canonical_combat();
        let before = combat.export_player_save().unwrap();
        let progression = ActorProgressionSnapshot::new(
            before.progression().skills().to_vec(),
            crate::progression::ProgressionTrackSnapshot::new(2, 9),
            crate::progression::ProgressionTrackSnapshot::new(3, 11),
        );
        let snapshot = PlayerSaveSnapshot::new(progression, 81.0, 42.0);
        combat.restore_player_save(&snapshot).unwrap();
        assert_eq!(combat.export_player_save().unwrap(), snapshot);

        let invalid = PlayerSaveSnapshot::new(snapshot.progression().clone(), 10_000.0, 1.0);
        assert!(matches!(
            combat.restore_player_save(&invalid),
            Err(PlayerSaveError::ResourceOutOfRange { .. })
        ));
        assert_eq!(combat.export_player_save().unwrap(), snapshot);
    }

    #[test]
    fn invalid_player_snapshots_are_rejected_without_mutation() {
        let mut combat = canonical_combat();
        let before = combat.export_player_save().unwrap();
        let tracks = before.progression().skills();
        let cases = vec![
            ActorProgressionSnapshot::new(
                tracks[..tracks.len() - 1].to_vec(),
                before.progression().hp(),
                before.progression().mana(),
            ),
            ActorProgressionSnapshot::new(
                [tracks.to_vec(), vec![tracks[0].clone()]].concat(),
                before.progression().hp(),
                before.progression().mana(),
            ),
            ActorProgressionSnapshot::new(
                vec![crate::progression::SkillProgressionSnapshot::new(
                    "unknown",
                    crate::progression::ProgressionTrackSnapshot::new(1, 0),
                )],
                before.progression().hp(),
                before.progression().mana(),
            ),
            ActorProgressionSnapshot::new(
                tracks.to_vec(),
                crate::progression::ProgressionTrackSnapshot::new(0, 0),
                before.progression().mana(),
            ),
            ActorProgressionSnapshot::new(
                tracks.to_vec(),
                crate::progression::ProgressionTrackSnapshot::new(
                    1,
                    crate::progression::balance::xp_to_next(1),
                ),
                before.progression().mana(),
            ),
        ];
        for progression in cases {
            assert!(combat
                .restore_player_save(&PlayerSaveSnapshot::new(
                    progression,
                    before.hp(),
                    before.mana()
                ))
                .is_err());
            assert_eq!(combat.export_player_save().unwrap(), before);
        }
    }

    #[test]
    fn dead_and_non_finite_resources_are_not_saveable() {
        let mut combat = canonical_combat();
        combat.player.resources.hp = 0.0;
        assert!(matches!(
            combat.export_player_save(),
            Err(PlayerSaveError::DeadPlayer(0.0))
        ));
        combat.player.resources.hp = 1.0;
        combat.player.resources.mana = f64::NAN;
        assert!(matches!(
            combat.export_player_save(),
            Err(PlayerSaveError::NonFiniteResource {
                resource: "mana",
                ..
            })
        ));
    }
}
