//! Live combat types. Same numbers as the sim — do not invent a second formula set.

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
    in_cone.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal).then(a.1.cmp(&b.1)));
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
    /// Visible body entity, if the fixture mesh has been spawned.
    pub entity: Option<engine::world::EntityId>,
    pub damage: i32,
    pub swing_s: f64,
    pub swing_cd: f64,
    pub reach_m: f64,
}

#[derive(Clone, Debug)]
pub struct IncomingHit {
    pub dealt: i32,
    pub by: String,
    pub killed: bool,
}

#[derive(Clone, Debug)]
pub struct WorldCombat {
    pub player: LivePlayer,
    pub lock: Option<i32>,
    pub cycle: Vec<i32>,
    pub auto_cd: f64,
    pub last_auto_dealt: i32,
    pub hostiles: Vec<WorldHostile>,
    pub strike_armed: bool,
    pub ember_started: bool,
    pub last_potion_heal: i32,
    pub busy: f64,
    pub gcd: f64,
    pub cds: std::collections::BTreeMap<&'static str, f64>,
    pub cast_kind: Option<&'static str>,
    pub cast_t: f64,
    pub cast_target: Option<i32>,
    pub ward: f64,
    pub ward_t: f64,
    pub mark_t: f64,
    pub second_wind_used: bool,
    pub last_rank_gate: Option<crate::controls::RankGate>,
    pub dead: bool,
    pub slain_by: Option<String>,
    pub slain_hold_s: f64,
    pub last_incoming: Option<IncomingHit>,
}

impl WorldCombat {
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
        let ids = tab_candidates(player_x, player_z, facing_x, facing_z, &self.hostile_pairs());
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
        match self.lock.and_then(|cur| self.cycle.iter().position(|&id| id == cur)) {
            Some(i) if i + 1 < self.cycle.len() => self.lock = Some(self.cycle[i + 1]),
            Some(_) => self.lock = None,
            None => self.lock = Some(self.cycle[0]),
        }
    }

    /// Click body: lock nearest in the same 20 m / 90° cone.
    pub fn click_lock(&mut self, player_x: f64, player_z: f64, facing_x: f64, facing_z: f64) {
        let ids = tab_candidates(player_x, player_z, facing_x, facing_z, &self.hostile_pairs());
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
        let Some(lock) = self.lock else {
            return None;
        };
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
        if let Some(shaken) = &self.player.shaken {
            raw = crate::combat::math::trunc(f64::from(raw) * shaken.outgoing_mult());
        }
        if strike {
            self.strike_armed = false;
        }
        if self.mark_t > 0.0 {
            raw = crate::combat::math::trunc(f64::from(raw) * crate::combat::math::MARK_MULT);
        }
        let dealt = mitigation(f64::from(raw), self.hostiles[hi].armor);
        self.last_auto_dealt = dealt;
        self.hostiles[hi].hp -= f64::from(dealt);
        if self.hostiles[hi].hp <= 0.0 {
            self.hostiles[hi].hp = 0.0;
            self.hostiles[hi].alive = false;
            self.lock = None;
        }
        self.auto_cd += MELEE_SWING_S;
        Some(dealt)
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
        for h in &mut self.hostiles {
            if !h.alive || h.stun_s > 0.0 {
                continue;
            }
            let dx = player_x - h.x;
            let dz = player_z - h.z;
            if (dx * dx + dz * dz).sqrt() > h.reach_m {
                continue;
            }
            h.swing_cd -= dt;
            if h.swing_cd > 0.0 {
                continue;
            }
            let dealt = mitigation(f64::from(h.damage), grit);
            self.player.resources.hp = (self.player.resources.hp - f64::from(dealt)).max(0.0);
            h.swing_cd += h.swing_s;
            let killed = self.player.resources.hp <= 0.0;
            let hit = IncomingHit {
                dealt,
                by: h.name.clone(),
                killed,
            };
            self.last_incoming = Some(hit.clone());
            last = Some(hit);
            if killed {
                self.dead = true;
                self.lock = None;
                self.auto_cd = 999.0;
                self.slain_by = Some(h.name.clone());
                self.slain_hold_s = SLAIN_HOLD_S;
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
            entity: None,
            damage: dmg,
            swing_s: 2.0,
            swing_cd: 0.0,
            reach_m: 1.8,
        }
    }

    #[test]
    fn incoming_can_kill_and_holds_before_respawn() {
        let mut c = WorldCombat::specialist(1, Discipline::Martial);
        c.hostiles.push(dummy_wolf(80));
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
}
