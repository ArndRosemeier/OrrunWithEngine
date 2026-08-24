//! Live combat types. Same numbers as the sim — do not invent a second formula set.

use super::log::CombatLog;
use super::math::*;
use super::sheets::{player_stats, PlayerStats};

#[derive(Clone, Debug)]
pub struct CombatResources {
    pub hp: f64,
    pub hp_max: f64,
    pub mana: f64,
    pub mana_max: f64,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostileState {
    Idle,
    Alerted,
    Pursuing,
    Attacking,
    Leashing,
    Dead,
}

#[derive(Clone, Debug)]
pub struct WorldHostile {
    pub idx: i32,
    pub x: f64,
    pub z: f64,
    pub hp: f64,
    pub max_hp: f64,
    pub armor: i32,
    pub alive: bool,
    pub stun_s: f64,
    pub slow_s: f64,
    pub root_s: f64,
    /// Display name for the lock tell. Fixture wolves use "wolf-spider".
    pub name: String,
    /// Mob level for consideration coloring.
    pub level: i32,
    /// Sheet / catalog id (orc, tribal, orc_skull, crawler_spider_wolf).
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
}

#[derive(Clone, Debug)]
pub struct WorldCombat {
    player: LivePlayer,
    lock: Option<i32>,
    cycle: Vec<i32>,
    auto_cd: f64,
    last_auto_dealt: i32,
    hostiles: Vec<WorldHostile>,
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
}

impl WorldCombat {
    pub fn player(&self) -> &LivePlayer {
        &self.player
    }
    pub fn player_mut(&mut self) -> &mut LivePlayer {
        &mut self.player
    }
    pub fn hostiles(&self) -> &[WorldHostile] {
        &self.hostiles
    }
    pub fn hostiles_mut(&mut self) -> &mut Vec<WorldHostile> {
        &mut self.hostiles
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
        self.cast_t
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
    pub fn add_hostile(&mut self, hostile: WorldHostile) {
        self.hostiles.push(hostile);
    }
    pub fn reset_for_encounter(&mut self) {
        let player = self.player.clone();
        let hostiles = self.hostiles.clone();
        *self = Self::specialist(player.stats.level, player.stats.discipline);
        self.player = player;
        self.hostiles = hostiles;
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
        }
    }

    pub fn reset_fixed_clock(&mut self) {
        self.fixed_accum_s = 0.0;
    }

    pub fn specialist(level: i32, discipline: Discipline) -> Self {
        Self {
            player: LivePlayer::specialist(level, discipline),
            lock: None,
            cycle: Vec::new(),
            auto_cd: MELEE_SWING_S,
            last_auto_dealt: 0,
            hostiles: Vec::new(),
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
        }
    }

    fn hostile_pairs(&self) -> Vec<(i32, f64, f64)> {
        self.hostiles
            .iter()
            .filter(|h| h.alive)
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
        self.hostiles[hi].state = HostileState::Alerted;
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
        h.state = HostileState::Alerted;
    }

    pub fn defeat_hostile(&mut self, idx: i32) {
        let Some(hi) = self.hostiles.iter().position(|h| h.idx == idx && h.alive) else {
            panic!("defeat reported for missing or dead hostile {idx}");
        };
        self.hostiles[hi].hp = 0.0;
        self.hostiles[hi].alive = false;
        self.hostiles[hi].state = HostileState::Dead;
        let defeated = self.hostiles[hi].clone();
        self.award_hostile_xp(&defeated);
        if self.lock == Some(idx) {
            self.lock = None;
        }
    }

    fn award_hostile_xp(&mut self, hostile: &WorldHostile) {
        let sheet = crate::combat::sheets::mob_sheet(&hostile.mob_id, Some(hostile.level))
            .unwrap_or_else(|err| panic!("missing XP sheet for {}: {err}", hostile.mob_id));
        self.player.xp += sheet.xp;
        while let Some(need) = crate::combat::sheets::xp_to_next(self.player.stats.level) {
            if self.player.xp < need {
                break;
            }
            self.player.xp -= need;
            self.player.stats = crate::combat::sheets::player_stats(
                self.player.stats.level + 1,
                self.player.stats.discipline,
            );
            self.player.resources.hp_max = f64::from(self.player.stats.hp);
            self.player.resources.mana_max = f64::from(self.player.stats.mana);
            self.player.resources.hp = self.player.resources.hp_max;
            self.player.resources.mana = self.player.resources.mana_max;
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

    /// Advances deterministic sight/hearing aggro, pursuit, melee range, and leash reset.
    pub fn tick_hostile_ai(&mut self, player_x: f64, player_z: f64, dt: f64) {
        if self.dead || dt <= 0.0 {
            return;
        }
        let social_targets: Vec<(f64, f64)> = self
            .hostiles
            .iter()
            .filter(|h| {
                h.alive
                    && matches!(
                        h.state,
                        HostileState::Alerted | HostileState::Pursuing | HostileState::Attacking
                    )
            })
            .map(|h| (h.x, h.z))
            .collect();
        for h in &mut self.hostiles {
            if !h.alive {
                h.state = HostileState::Dead;
                continue;
            }
            let home_distance = (h.x - h.home_x).hypot(h.z - h.home_z);
            if home_distance > h.aggro.leash_m {
                h.state = HostileState::Leashing;
            }
            if h.state == HostileState::Leashing {
                let dx = h.home_x - h.x;
                let dz = h.home_z - h.z;
                let distance = dx.hypot(dz);
                if distance <= 0.05 {
                    h.x = h.home_x;
                    h.z = h.home_z;
                    h.aggro = Aggro::default();
                    h.state = HostileState::Idle;
                } else if h.root_s <= 0.0 {
                    let speed = crate::combat::WALK_MPS * if h.slow_s > 0.0 { 0.5 } else { 1.0 };
                    let step = (speed * dt).min(distance);
                    h.x += dx / distance * step;
                    h.z += dz / distance * step;
                    if step >= distance - 1e-9 {
                        h.x = h.home_x;
                        h.z = h.home_z;
                        h.aggro = Aggro::default();
                        h.state = HostileState::Idle;
                    }
                }
                continue;
            }
            let player_distance = (player_x - h.x).hypot(player_z - h.z);
            let social = social_targets.iter().any(|&(x, z)| {
                (x - h.x).hypot(z - h.z) > 1e-9 && (x - h.x).hypot(z - h.z) <= h.aggro.social_m
            });
            let can_aggro =
                player_distance <= h.aggro.sight_m || player_distance <= h.aggro.hear_m || social;
            if matches!(h.state, HostileState::Idle) && can_aggro {
                h.state = HostileState::Alerted;
            }
            if matches!(h.state, HostileState::Alerted | HostileState::Pursuing) {
                if player_distance <= h.reach_m {
                    h.state = HostileState::Attacking;
                } else if h.root_s <= 0.0 {
                    h.state = HostileState::Pursuing;
                    let dx = player_x - h.x;
                    let dz = player_z - h.z;
                    let distance = dx.hypot(dz);
                    let speed = crate::combat::WALK_MPS * if h.slow_s > 0.0 { 0.5 } else { 1.0 };
                    let step = (speed * dt).min((distance - h.reach_m).max(0.0));
                    if distance > 1e-9 {
                        h.x += dx / distance * step;
                        h.z += dz / distance * step;
                    }
                    if (player_x - h.x).hypot(player_z - h.z) <= h.reach_m {
                        h.state = HostileState::Attacking;
                    }
                }
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
            let sheet =
                crate::combat::sheets::mob_sheet(&h.mob_id, Some(h.level)).unwrap_or_else(|err| {
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

    /// Mob autos. Same mitigation as the sim. Reach is the sheet reach (1.8 m).
    pub fn tick_incoming(&mut self, player_x: f64, player_z: f64, dt: f64) -> Option<IncomingHit> {
        if self.dead || dt <= 0.0 {
            return None;
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
        WorldHostile {
            idx: 0,
            x: 1.0,
            z: 0.0,
            hp: 70.0,
            max_hp: 70.0,
            armor: 8,
            alive: true,
            stun_s: 0.0,
            slow_s: 0.0,
            root_s: 0.0,
            name: "wolf-spider".into(),
            level: 1,
            mob_id: "crawler_spider_wolf".into(),
            entity: None,
            damage: dmg,
            swing_s: 2.0,
            swing_cd: 0.0,
            reach_m: 1.8,
            home_x: 1.0,
            home_z: 0.0,
            aggro: Aggro::default(),
            state: HostileState::Idle,
        }
    }

    #[test]
    fn hostile_ai_aggros_chases_stops_and_leashes() {
        let mut c = WorldCombat::specialist(1, Discipline::Martial);
        let mut h = dummy_wolf(10);
        h.x = 10.0;
        h.home_x = 10.0;
        h.aggro.sight_m = 12.0;
        h.aggro.leash_m = 9.0;
        c.hostiles.push(h);
        c.tick_hostile_ai(0.0, 0.0, 0.1);
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
        c.hostiles[0].x = 20.0 - WALK_MPS * 2.0;
        c.tick_hostile_ai(0.0, 0.0, 2.0);
        assert_eq!(c.hostiles[0].state, HostileState::Idle);
        assert!((c.hostiles[0].x - 10.0).abs() < 1e-9);
    }

    #[test]
    fn incoming_can_kill_and_holds_before_respawn() {
        let mut c = WorldCombat::specialist(1, Discipline::Martial);
        let mut wolf = dummy_wolf(80);
        wolf.state = HostileState::Attacking;
        c.hostiles.push(wolf);
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
        c.hostiles.push(dummy_wolf(10));
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
}
