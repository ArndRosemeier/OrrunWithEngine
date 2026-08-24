//! Live combat verbs. Same numbers as the sim ÔÇö do not invent a second formula set.

use std::collections::BTreeMap;

use crate::controls::RankGate;

use super::math::*;
use super::types::WorldCombat;

pub use crate::controls::Action as CombatVerb;

pub fn empty_cds() -> BTreeMap<&'static str, f64> {
    let mut cds = BTreeMap::new();
    for v in CombatVerb::ALL {
        cds.insert(v.id(), 0.0);
    }
    cds
}

impl WorldCombat {
    fn cd(&self, k: &str) -> f64 {
        *self.cds().get(k).unwrap_or(&0.0)
    }

    pub fn verb_cd_frac(&self, verb: CombatVerb) -> f32 {
        let left = self.cd(verb.id());
        if left <= 0.0 {
            return 0.0;
        }
        let max = cd_for(verb.id());
        if max <= 0.0 {
            return 0.0;
        }
        (left / max).clamp(0.0, 1.0) as f32
    }

    /// Remaining-time fraction of the live cast. `1.0` just started, `0.0` gone.
    /// Same remaining-visible convention as [`Self::verb_cd_frac`].
    pub fn cast_frac(&self) -> Option<f32> {
        let kind = self.cast_kind()?;
        let max = cast_duration_s(kind);
        if max <= 0.0 || self.cast_time() <= 0.0 {
            return None;
        }
        Some((self.cast_time() / max).clamp(0.0, 1.0) as f32)
    }

    pub fn cast_label(&self) -> Option<&'static str> {
        Some(match self.cast_kind()? {
            "ember" => "Ember",
            "mend" => "Mend",
            "bind" => "Bind",
            "bash" => "Bash",
            "aimed" => "Aimed Shot",
            "pin" => "Pin",
            other => other,
        })
    }

    fn set_cd(&mut self, k: &'static str, v: f64) {
        self.cds_mut().insert(k, v);
    }

    fn note_fail(&mut self, line: &'static str) {
        self.log_mut().push(line);
        self.set_fail_tell(Some(line));
        self.set_fail_tell_timer(1.2);
    }

    pub fn fail_tell(&self) -> Option<&'static str> {
        if self.fail_tell_timer() > 0.0 {
            self.fail_tell_value()
        } else {
            None
        }
    }

    fn lock_in_range(
        &mut self,
        player_x: f64,
        player_z: f64,
        facing_x: f64,
        facing_z: f64,
        range: f64,
        melee: bool,
    ) -> Option<i32> {
        let Some(lock) = self.lock_id() else {
            self.note_fail("No target");
            return None;
        };
        let Some(h) = self.hostiles().iter().find(|h| h.idx == lock && h.alive) else {
            self.note_fail("No target");
            return None;
        };
        if melee {
            if !super::types::melee_auto_legal(player_x, player_z, facing_x, facing_z, h.x, h.z) {
                self.note_fail("Out of range");
                return None;
            }
        } else if !self.target_in_range(player_x, player_z, lock, range) {
            self.note_fail("Out of range");
            return None;
        }
        Some(lock)
    }

    /// Apply a live verb. No-lock / out-of-range push a combat-log fail tell.
    /// Returns whether the verb started (armed, cast begun, or potion drunk).
    pub fn press_verb(
        &mut self,
        verb: CombatVerb,
        player_x: f64,
        player_z: f64,
        facing_x: f64,
        facing_z: f64,
    ) -> bool {
        let _ = (facing_x, facing_z);
        let ranks = self.player().stats.ranks;
        if !verb.rank_ok(ranks.martial, ranks.hunt, ranks.arcane) {
            self.set_last_rank_gate(Some(RankGate {
                action: verb,
                blocked: true,
                have: verb.rank_have(ranks.martial, ranks.hunt, ranks.arcane),
                need: verb.rank_need(),
            }));
            return false;
        }
        if verb != CombatVerb::Potion && (self.cast_kind().is_some() || self.gcd_value() > 0.0) {
            return false;
        }
        if self.cd(verb.id()) > 0.0 {
            return false;
        }
        match verb {
            CombatVerb::Strike => {
                if self
                    .lock_in_range(player_x, player_z, facing_x, facing_z, MELEE_REACH_M, true)
                    .is_none()
                {
                    return false;
                }
                self.set_strike_armed(true);
                self.set_cd("strike", STRIKE_CD_S);
                true
            }
            CombatVerb::Bash => {
                let Some(lock) =
                    self.lock_in_range(player_x, player_z, facing_x, facing_z, MELEE_REACH_M, true)
                else {
                    return false;
                };
                self.start_cast("bash", BASH_ANIM_S, 0, Some(lock))
            }
            CombatVerb::AimedShot => {
                if self.player().arrows <= 0 {
                    return false;
                }
                let Some(lock) = self.lock_in_range(
                    player_x,
                    player_z,
                    facing_x,
                    facing_z,
                    BOW_FALLOFF_END_M,
                    false,
                ) else {
                    return false;
                };
                self.start_cast("aimed", AIMED_DRAW_S, 0, Some(lock))
            }
            CombatVerb::Pin => {
                if self.player().arrows <= 0 {
                    return false;
                }
                let Some(lock) = self.lock_in_range(
                    player_x,
                    player_z,
                    facing_x,
                    facing_z,
                    BOW_FALLOFF_END_M,
                    false,
                ) else {
                    return false;
                };
                self.start_cast("pin", BOW_DRAW_S, 0, Some(lock))
            }
            CombatVerb::Ember => {
                if self.player().resources.mana < f64::from(EMBER_MANA) {
                    return false;
                }
                let Some(lock) = self.lock_in_range(
                    player_x,
                    player_z,
                    facing_x,
                    facing_z,
                    EMBER_RANGE_M,
                    false,
                ) else {
                    return false;
                };
                let started = self.start_cast("ember", EMBER_CAST_S, EMBER_MANA, Some(lock));
                if started {
                    self.set_ember_started(true);
                }
                started
            }
            CombatVerb::Bind => {
                if self.player().resources.mana < f64::from(BIND_MANA) {
                    return false;
                }
                let Some(lock) =
                    self.lock_in_range(player_x, player_z, facing_x, facing_z, BIND_RANGE_M, false)
                else {
                    return false;
                };
                self.start_cast("bind", BIND_CAST_S, BIND_MANA, Some(lock))
            }
            CombatVerb::Mend => {
                if self.player().resources.mana < f64::from(MEND_MANA) {
                    return false;
                }
                self.start_cast("mend", MEND_CAST_S, MEND_MANA, None)
            }
            CombatVerb::Ward => {
                if self.player().resources.mana < f64::from(WARD_MANA) {
                    return false;
                }
                if !self.spend_mana(WARD_MANA) {
                    return false;
                }
                self.set_ward(f64::from(self.player().stats.ward()));
                self.set_ward_time(WARD_DUR_S);
                self.set_gcd(WARD_GCD_S);
                self.set_cd("ward", WARD_CD_S);
                true
            }
            CombatVerb::Mark => {
                self.set_mark_time(MARK_DUR_S);
                self.set_cd("mark", MARK_CD_S);
                true
            }
            CombatVerb::SecondWind => {
                if self.second_wind_used() {
                    return false;
                }
                let heal = SECOND_WIND_PCT * self.player().resources.hp_max;
                self.player_mut().resources.hp = self
                    .player()
                    .resources
                    .hp_max
                    .min(self.player().resources.hp + heal);
                self.set_second_wind_used(true);
                true
            }
            CombatVerb::Potion => {
                if self.player().potions <= 0 {
                    return false;
                }
                let before = self.player().resources.hp;
                if !self.player_mut().drink_potion() {
                    return false;
                }
                self.set_last_potion_heal(trunc(self.player().resources.hp - before));
                self.set_cd("potion", POTION_CD_S);
                true
            }
        }
    }

    fn start_cast(
        &mut self,
        kind: &'static str,
        time: f64,
        mana: i32,
        target: Option<i32>,
    ) -> bool {
        if time < 0.0 {
            return false;
        }
        if mana > 0 && !self.spend_mana(mana) {
            return false;
        }
        self.set_cast_kind(Some(kind));
        self.set_cast_time(time);
        self.set_cast_target(target);
        self.set_busy_time(time);
        self.set_cd(kind, cd_for(kind));
        true
    }

    fn spend_mana(&mut self, amount: i32) -> bool {
        let need = f64::from(amount);
        if self.player().resources.mana < need {
            return false;
        }
        self.player_mut().resources.mana -= need;
        true
    }

    fn target_in_range(&self, player_x: f64, player_z: f64, idx: i32, range: f64) -> bool {
        self.hostiles()
            .iter()
            .any(|h| h.idx == idx && h.alive && dist(player_x, player_z, h.x, h.z) <= range)
    }

    /// Tick CDs, casts, and CC. Same 0.1 s clock as auto.
    pub fn tick_verbs(&mut self, player_x: f64, player_z: f64, dt: f64) {
        if dt <= 0.0 {
            return;
        }
        if self.slain_hold_s() > 0.0 {
            *self.slain_hold_s_mut() = (self.slain_hold_s() - dt).max(0.0);
        }
        if self.fail_tell_timer() > 0.0 {
            *self.fail_tell_timer_mut() = (self.fail_tell_timer() - dt).max(0.0);
            if self.fail_tell_timer() <= 0.0 {
                self.set_fail_tell(None);
            }
        }
        if let Some(shaken) = &mut self.player_mut().shaken {
            shaken.remaining_s = (shaken.remaining_s - dt).max(0.0);
        }
        if self
            .player()
            .shaken
            .as_ref()
            .is_some_and(|s| s.remaining_s <= 0.0)
        {
            self.player_mut().shaken = None;
        }
        for v in self.cds_mut().values_mut() {
            *v = (*v - dt).max(0.0);
        }
        self.set_gcd((self.gcd_value() - dt).max(0.0));
        self.set_busy_time((self.busy_time() - dt).max(0.0));
        if self.ward_time() > 0.0 {
            *self.ward_t_mut() = (self.ward_time() - dt).max(0.0);
            if self.ward_time() <= 0.0 {
                self.set_ward(0.0);
            }
        }
        if self.mark_time() > 0.0 {
            *self.mark_time_mut() = (self.mark_time() - dt).max(0.0);
        }
        for h in self.hostiles_mut() {
            h.stun_s = (h.stun_s - dt).max(0.0);
            h.slow_s = (h.slow_s - dt).max(0.0);
            h.root_s = (h.root_s - dt).max(0.0);
        }
        if self.cast_kind().is_some() {
            *self.cast_time_mut() -= dt;
            if self.cast_time() <= 0.0 {
                self.finish_cast(player_x, player_z);
            }
        }
        if self.in_combat() {
            self.player_mut().resources.regen_combat(dt);
        } else {
            self.player_mut().resources.regen_ooc(dt);
        }
    }

    fn finish_cast(&mut self, player_x: f64, player_z: f64) {
        let kind = self.cast_kind_mut().take().unwrap_or("");
        let target = self.cast_target_mut().take();
        self.set_cast_time(0.0);
        self.set_busy_time(0.0);
        match kind {
            "bash" => {
                if let Some(idx) = target.or(self.lock_id()) {
                    if let Some(h) = self
                        .hostiles_mut()
                        .iter_mut()
                        .find(|h| h.idx == idx && h.alive)
                    {
                        h.stun_s = h.stun_s.max(BASH_STUN_S);
                    }
                }
            }
            "mend" => {
                let heal = f64::from(self.player().stats.mend());
                self.player_mut().resources.hp = self
                    .player()
                    .resources
                    .hp_max
                    .min(self.player().resources.hp + heal);
            }
            "bind" => {
                if let Some(idx) = target.or(self.lock_id()) {
                    if self.target_in_range(player_x, player_z, idx, BIND_RANGE_M) {
                        if let Some(h) = self
                            .hostiles_mut()
                            .iter_mut()
                            .find(|h| h.idx == idx && h.alive)
                        {
                            h.root_s = BIND_ROOT_S;
                            self.player_mut().used_pin_or_bind = true;
                        }
                    }
                }
            }
            "aimed" | "pin" => {
                let Some(idx) = target.or(self.lock_id()) else {
                    return;
                };
                let Some(hi) = self.hostiles().iter().position(|h| h.idx == idx && h.alive) else {
                    return;
                };
                if self.player().arrows <= 0 {
                    return;
                }
                self.player_mut().arrows -= 1;
                let d = dist(
                    player_x,
                    player_z,
                    self.hostiles_mut()[hi].x,
                    self.hostiles_mut()[hi].z,
                );
                let raw = self.outgoing_raw(self.player().stats.bow_hit(kind == "aimed", d));
                let dealt = mitigation(f64::from(raw), self.hostiles_mut()[hi].armor);
                self.hostiles_mut()[hi].hp -= f64::from(dealt);
                if kind == "pin" {
                    self.hostiles_mut()[hi].slow_s = PIN_DUR_S;
                    self.player_mut().used_pin_or_bind = true;
                }
                if self.hostiles_mut()[hi].hp <= 0.0 {
                    self.hostiles_mut()[hi].hp = 0.0;
                    self.hostiles_mut()[hi].alive = false;
                    if self.lock_id() == Some(idx) {
                        self.set_lock(None);
                    }
                }
            }
            "ember" => {
                let Some(idx) = target.or(self.lock_id()) else {
                    return;
                };
                let Some(hi) = self.hostiles().iter().position(|h| h.idx == idx && h.alive) else {
                    return;
                };
                let d = dist(
                    player_x,
                    player_z,
                    self.hostiles_mut()[hi].x,
                    self.hostiles_mut()[hi].z,
                );
                if d > EMBER_RANGE_M {
                    return;
                }
                let raw = self.outgoing_raw(self.player().stats.ember());
                let dealt = mitigation(f64::from(raw), self.hostiles_mut()[hi].armor);
                self.hostiles_mut()[hi].hp -= f64::from(dealt);
                if self.hostiles_mut()[hi].hp <= 0.0 {
                    self.hostiles_mut()[hi].hp = 0.0;
                    self.hostiles_mut()[hi].alive = false;
                    if self.lock_id() == Some(idx) {
                        self.set_lock(None);
                    }
                }
            }
            _ => {}
        }
    }
}

pub fn cd_max(kind: &str) -> f64 {
    cd_for(kind)
}

fn cast_duration_s(kind: &str) -> f64 {
    match kind {
        "bash" => BASH_ANIM_S,
        "aimed" => AIMED_DRAW_S,
        "pin" => BOW_DRAW_S,
        "ember" => EMBER_CAST_S,
        "bind" => BIND_CAST_S,
        "mend" => MEND_CAST_S,
        _ => 0.0,
    }
}

fn cd_for(kind: &str) -> f64 {
    match kind {
        "strike" => STRIKE_CD_S,
        "bash" => BASH_CD_S,
        "aimed" => AIMED_CD_S,
        "pin" => PIN_CD_S,
        "ember" => EMBER_CD_S,
        "bind" => BIND_CD_S,
        "mend" => MEND_CD_S,
        "ward" => WARD_CD_S,
        "potion" => POTION_CD_S,
        "mark" => MARK_CD_S,
        _ => 0.0,
    }
}

fn dist(ax: f64, az: f64, bx: f64, bz: f64) -> f64 {
    let dx = bx - ax;
    let dz = bz - az;
    (dx * dx + dz * dz).sqrt()
}
