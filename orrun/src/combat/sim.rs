//! Headless 0.1 s combat tick. AI + 1D movement from combat_sim.py.
//! Live uses 0.1 s accumulators or calls [`Fight::tick`].

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::combat::math::*;
use crate::combat::sheets::{
    formulas, leveling_path, mob_sheet, mob_sheets_json, player_specialists_json, player_stats,
    resolve_mob_id, wolf_sheet, xp_curve, MobSheet, PlayerStats,
};
use crate::combat::Discipline;

#[derive(Debug, Clone)]
struct Cast {
    kind: &'static str,
    target: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct Mob {
    pub sheet: MobSheet,
    pub id: String,
    pub idx: i32,
    pub x: f64,
    pub hp: f64,
    pub max_hp: f64,
    pub armor: i32,
    pub base_dmg: f64,
    pub base_swing: f64,
    pub swing: f64,
    pub reach: f64,
    pub speed: f64,
    pub auto_cd: f64,
    pub stun: f64,
    pub root: f64,
    pub root_grace: f64,
    pub slow: f64,
    pub tele: f64,
    pub slam_cd: f64,
    pub slam_dmg: f64,
    pub tele_len: f64,
    pub has_slam: bool,
    pub enraged: bool,
    pub alive: bool,
    pub threat: f64,
    pub dmg_taken: i32,
}

impl Mob {
    pub fn from_sheet(sheet: MobSheet, x: f64, idx: i32) -> Self {
        let slam_cd = sheet.slam_every_s.unwrap_or(0.0);
        let slam_dmg = f64::from(sheet.slam_damage.unwrap_or(0));
        let tele_len = sheet.telegraph_s.unwrap_or(0.0);
        let has_slam = slam_cd > 0.0 && slam_dmg > 0.0;
        let base_swing = sheet.swing_s;
        Self {
            id: sheet.id.clone(),
            idx,
            x,
            hp: f64::from(sheet.hp),
            max_hp: f64::from(sheet.hp),
            armor: sheet.armor,
            base_dmg: f64::from(sheet.damage),
            base_swing,
            swing: base_swing,
            reach: sheet.reach_m,
            speed: sheet.speed_mps,
            auto_cd: base_swing,
            stun: 0.0,
            root: 0.0,
            root_grace: 0.0,
            slow: 0.0,
            tele: 0.0,
            slam_cd,
            slam_dmg,
            tele_len,
            has_slam,
            enraged: false,
            alive: true,
            threat: 0.0,
            dmg_taken: 0,
            sheet,
        }
    }

    fn dmg_mult(&self) -> f64 {
        if self.enraged {
            MOTHER_ENRAGE_DMG
        } else {
            1.0
        }
    }
}

#[derive(Debug, Clone)]
pub struct Player {
    pub stats: PlayerStats,
    pub x: f64,
    pub hp: f64,
    pub max_hp: f64,
    pub mana: f64,
    pub max_mana: f64,
    pub potions: i32,
    pub arrows: i32,
    pub alive: bool,
    pub auto_cd: f64,
    pub busy: f64,
    cast: Option<Cast>,
    pub gcd: f64,
    pub cds: BTreeMap<&'static str, f64>,
    pub strike_armed: bool,
    pub ward: f64,
    pub ward_t: f64,
    pub mark_t: f64,
    pub second_wind_used: bool,
    pub poison_t: f64,
    pub poison_acc: f64,
    pub lock: Option<i32>,
    pub mana_spent: i32,
    pub swings: i32,
    pub spells: BTreeMap<String, i32>,
    pub max_hit_taken: i32,
    pub shaken: bool,
    pub sprinted: bool,
    pub used_pin_or_bind: bool,
    pub damage_taken: i32,
    pub mana_hit_zero: bool,
    pub skipped_spell_for_mana: bool,
    pub min_mana: f64,
}

impl Player {
    pub fn new(stats: PlayerStats, potions: i32, arrows: i32) -> Self {
        let hp = f64::from(stats.hp);
        let mana = f64::from(stats.mana);
        let mut cds = BTreeMap::new();
        for k in [
            "strike", "bash", "aimed", "pin", "ember", "mend", "bind", "ward", "potion", "mark",
        ] {
            cds.insert(k, 0.0);
        }
        Self {
            stats,
            x: 0.0,
            hp,
            max_hp: hp,
            mana,
            max_mana: mana,
            potions,
            arrows,
            alive: true,
            auto_cd: 0.0,
            busy: 0.0,
            cast: None,
            gcd: 0.0,
            cds,
            strike_armed: false,
            ward: 0.0,
            ward_t: 0.0,
            mark_t: 0.0,
            second_wind_used: false,
            poison_t: 0.0,
            poison_acc: 0.0,
            lock: None,
            mana_spent: 0,
            swings: 0,
            spells: BTreeMap::new(),
            max_hit_taken: 0,
            shaken: false,
            sprinted: false,
            used_pin_or_bind: false,
            damage_taken: 0,
            mana_hit_zero: false,
            skipped_spell_for_mana: false,
            min_mana: mana,
        }
    }

    fn rank(&self, d: Discipline) -> i32 {
        match d {
            Discipline::Martial => self.stats.ranks.martial,
            Discipline::Hunt => self.stats.ranks.hunt,
            Discipline::Arcane => self.stats.ranks.arcane,
        }
    }

    fn bump_skill(&mut self) {
        if self.stats.weapon_skill >= self.stats.skill_cap {
            return;
        }
        self.stats.skill_xp += 1;
        let cost = SKILL_UP_COST_MULT * self.stats.weapon_skill;
        if self.stats.skill_xp >= cost {
            self.stats.weapon_skill += 1;
            self.stats.skill_xp = 0;
        }
    }

    fn bump_spell(&mut self, name: &str) {
        *self.spells.entry(name.to_string()).or_insert(0) += 1;
    }

    fn cd(&self, k: &str) -> f64 {
        *self.cds.get(k).unwrap_or(&0.0)
    }

    fn set_cd(&mut self, k: &'static str, v: f64) {
        self.cds.insert(k, v);
    }
}

fn dist(p: &Player, m: &Mob) -> f64 {
    (m.x - p.x).abs()
}

fn living(mobs: &[Mob]) -> Vec<usize> {
    mobs.iter()
        .enumerate()
        .filter(|(_, m)| m.alive)
        .map(|(i, _)| i)
        .collect()
}

fn apply_player_damage(p: &mut Player, amount: i32, interrupt: bool) -> i32 {
    if amount <= 0 || !p.alive {
        return 0;
    }
    p.max_hit_taken = p.max_hit_taken.max(amount);
    let mut left = amount;
    if p.ward > 0.0 {
        let absorb = p.ward.min(f64::from(left));
        p.ward -= absorb;
        left -= trunc(absorb);
        if p.ward <= 0.0 {
            p.ward = 0.0;
            p.ward_t = 0.0;
        }
    }
    if left <= 0 {
        return 0;
    }
    p.hp -= f64::from(left);
    p.damage_taken += left;
    if interrupt && p.cast.is_some() {
        p.cast = None;
        p.busy = 0.0;
    }
    if p.hp <= 0.0 {
        p.hp = 0.0;
        p.alive = false;
    }
    left
}

fn hit_mob(p: &mut Player, m: &mut Mob, mut raw: i32) -> i32 {
    if p.shaken {
        raw = trunc(f64::from(raw) * SHAKEN_DMG);
    }
    if p.mark_t > 0.0 {
        raw = trunc(f64::from(raw) * MARK_MULT);
    }
    let dealt = mitigation(f64::from(raw), m.armor);
    m.hp -= f64::from(dealt);
    m.dmg_taken += dealt;
    m.threat += f64::from(dealt) * THREAT_DMG;
    if m.root > 0.0 && m.root_grace <= 0.0 {
        m.root = 0.0;
    }
    if m.hp <= 0.0 {
        m.hp = 0.0;
        m.alive = false;
    }
    p.bump_skill();
    dealt
}

fn start_cast(p: &mut Player, name: &'static str, duration: f64, mana: i32, kind: &'static str, target: Option<i32>) -> bool {
    if p.busy > 0.0 || p.gcd > 0.0 {
        return false;
    }
    if mana > 0 && p.mana < f64::from(mana) {
        return false;
    }
    if mana > 0 {
        p.mana -= f64::from(mana);
        p.mana_spent += mana;
        if p.mana <= 1e-9 {
            p.mana = 0.0;
            p.mana_hit_zero = true;
        }
    }
    if name == "Pin" || name == "Bind" || kind == "pin" || kind == "bind" {
        p.used_pin_or_bind = true;
    }
    p.cast = Some(Cast { kind, target });
    p.busy = duration;
    p.bump_spell(name);
    true
}

fn lowest_hp(mobs: &[Mob], idxs: &[usize]) -> usize {
    *idxs
        .iter()
        .min_by(|a, b| {
            mobs[**a]
                .hp
                .partial_cmp(&mobs[**b].hp)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(mobs[**a].idx.cmp(&mobs[**b].idx))
        })
        .expect("live mob")
}

fn player_ai_start(p: &mut Player, mobs: &mut [Mob], _kite: bool) {
    if !p.alive || p.busy > 0.0 || p.gcd > 0.0 {
        return;
    }
    let disc = p.stats.discipline;
    let live = living(mobs);
    if live.is_empty() {
        return;
    }

    let pot_line = if disc == Discipline::Arcane { 0.30 } else { 0.35 };
    if p.potions > 0 && p.cd("potion") <= 0.0 && p.hp < pot_line * p.max_hp {
        p.hp = (p.hp + f64::from(POTION_HEAL)).min(p.max_hp);
        p.potions -= 1;
        p.set_cd("potion", POTION_CD_S);
        p.bump_spell("potion");
        return;
    }

    if disc == Discipline::Martial
        && p.rank(Discipline::Martial) >= 10
        && !p.second_wind_used
        && p.hp < 0.25 * p.max_hp
    {
        p.hp = (p.hp + SECOND_WIND_PCT * p.max_hp).min(p.max_hp);
        p.second_wind_used = true;
        p.bump_spell("SecondWind");
        return;
    }

    if disc == Discipline::Martial {
        if p.rank(Discipline::Martial) >= 3 && p.cd("bash") <= 0.0 {
            if let Some(&i) = live.iter().find(|&&i| mobs[i].tele > 0.0) {
                start_cast(p, "Bash", BASH_ANIM_S, 0, "bash", Some(mobs[i].idx));
                p.set_cd("bash", BASH_CD_S);
                return;
            }
        }
        if p.rank(Discipline::Martial) >= 1 && p.cd("strike") <= 0.0 && !p.strike_armed {
            p.strike_armed = true;
            p.set_cd("strike", STRIKE_CD_S);
            p.bump_spell("Strike");
        }
        return;
    }

    if disc == Discipline::Hunt {
        if p.arrows <= 0 {
            return;
        }
        if p.rank(Discipline::Hunt) >= 10 && p.cd("mark") <= 0.0 {
            p.mark_t = MARK_DUR_S;
            p.set_cd("mark", MARK_CD_S);
            p.bump_spell("Mark");
        }
        if p.rank(Discipline::Hunt) >= 1 && p.cd("aimed") <= 0.0 {
            let i = lowest_hp(mobs, &live);
            let tgt = mobs[i].idx;
            p.lock = Some(tgt);
            start_cast(p, "AimedShot", AIMED_DRAW_S, 0, "aimed", Some(tgt));
            p.set_cd("aimed", AIMED_CD_S);
            return;
        }
        if p.rank(Discipline::Hunt) >= 3 && p.cd("pin") <= 0.0 {
            let i = lowest_hp(mobs, &live);
            let tgt = mobs[i].idx;
            p.lock = Some(tgt);
            start_cast(p, "Pin", BOW_DRAW_S, 0, "pin", Some(tgt));
            p.set_cd("pin", PIN_CD_S);
            return;
        }
        let i = lowest_hp(mobs, &live);
        let tgt = mobs[i].idx;
        p.lock = Some(tgt);
        start_cast(p, "Fire", BOW_DRAW_S, 0, "fire", Some(tgt));
        return;
    }

    if p.rank(Discipline::Arcane) >= 3 && p.hp < 0.50 * p.max_hp && p.cd("mend") <= 0.0 {
        if p.mana >= f64::from(MEND_MANA) {
            start_cast(p, "Mend", MEND_CAST_S, MEND_MANA, "mend", None);
            p.set_cd("mend", MEND_CD_S);
            return;
        }
        p.skipped_spell_for_mana = true;
    }
    if p.rank(Discipline::Arcane) >= 7 && p.cd("ward") <= 0.0 {
        let slamming = live.iter().any(|&i| mobs[i].tele > 0.0);
        if slamming || p.hp < 0.60 * p.max_hp {
            if p.mana >= f64::from(WARD_MANA) {
                start_cast(p, "Ward", 0.0, WARD_MANA, "ward", None);
                p.set_cd("ward", WARD_CD_S);
                p.gcd = WARD_GCD_S;
                p.ward = f64::from(p.stats.ward());
                p.ward_t = WARD_DUR_S;
                p.cast = None;
                p.busy = 0.0;
                return;
            }
            p.skipped_spell_for_mana = true;
        }
    }
    let in_ember: Vec<usize> = live
        .iter()
        .copied()
        .filter(|&i| dist(p, &mobs[i]) <= EMBER_RANGE_M)
        .collect();
    let focus_pool = if in_ember.is_empty() { live.clone() } else { in_ember.clone() };
    let focus_i = lowest_hp(mobs, &focus_pool);
    let focus_idx = mobs[focus_i].idx;
    if p.rank(Discipline::Arcane) >= 5 && p.cd("bind") <= 0.0 {
        let others: Vec<usize> = live
            .iter()
            .copied()
            .filter(|&i| {
                mobs[i].idx != focus_idx && mobs[i].root <= 0.0 && dist(p, &mobs[i]) <= BIND_RANGE_M
            })
            .collect();
        if !others.is_empty() {
            if p.mana >= f64::from(BIND_MANA) {
                let tgt = mobs[others[0]].idx;
                p.lock = Some(tgt);
                start_cast(p, "Bind", BIND_CAST_S, BIND_MANA, "bind", Some(tgt));
                p.set_cd("bind", BIND_CD_S);
                return;
            }
            p.skipped_spell_for_mana = true;
        }
    }
    if !in_ember.is_empty() && p.cd("ember") <= 0.0 {
        if p.mana >= f64::from(EMBER_MANA) {
            p.lock = Some(focus_idx);
            start_cast(p, "Ember", EMBER_CAST_S, EMBER_MANA, "ember", Some(focus_idx));
            p.set_cd("ember", EMBER_CD_S);
        } else {
            p.skipped_spell_for_mana = true;
        }
    }
}

fn finish_cast(p: &mut Player, mobs: &mut [Mob]) {
    let Some(cast) = p.cast.take() else {
        return;
    };
    let find = |mobs: &mut [Mob]| -> Option<usize> {
        cast.target.and_then(|idx| mobs.iter().position(|m| m.idx == idx))
    };
    match cast.kind {
        "bash" => {
            if let Some(i) = find(mobs) {
                if mobs[i].alive {
                    mobs[i].tele = 0.0;
                    mobs[i].stun = mobs[i].stun.max(BASH_STUN_S);
                }
            }
        }
        "mend" => {
            let heal = p.stats.mend();
            p.hp = (p.hp + f64::from(heal)).min(p.max_hp);
            if let Some(lock) = p.lock {
                if let Some(m) = mobs.iter_mut().find(|m| m.idx == lock && m.alive) {
                    m.threat += f64::from(heal) * THREAT_HEAL;
                }
            }
        }
        "bind" => {
            if let Some(i) = find(mobs) {
                let d = (mobs[i].x - p.x).abs();
                if mobs[i].alive && d <= BIND_RANGE_M {
                    mobs[i].root = BIND_ROOT_S;
                    mobs[i].root_grace = BIND_GRACE_S;
                }
            }
        }
        "aimed" | "pin" | "fire" => {
            if p.arrows <= 0 {
                return;
            }
            p.arrows -= 1;
            if let Some(i) = find(mobs) {
                if mobs[i].alive {
                    let d = (mobs[i].x - p.x).abs();
                    let raw = p.stats.bow_hit(cast.kind == "aimed", d);
                    if raw > 0 {
                        hit_mob(p, &mut mobs[i], raw);
                        p.swings += 1;
                    }
                    if cast.kind == "pin" && mobs[i].alive {
                        mobs[i].slow = PIN_DUR_S;
                    }
                }
            }
        }
        "ember" => {
            if let Some(i) = find(mobs) {
                let d = (mobs[i].x - p.x).abs();
                if mobs[i].alive && d <= EMBER_RANGE_M {
                    let raw = p.stats.ember();
                    hit_mob(p, &mut mobs[i], raw);
                }
            }
        }
        _ => {}
    }
}

fn martial_auto(p: &mut Player, mobs: &mut [Mob]) {
    if p.stats.discipline != Discipline::Martial || !p.alive {
        return;
    }
    let live = living(mobs);
    if live.is_empty() {
        return;
    }
    if p.lock.map(|id| !mobs.iter().any(|m| m.idx == id && m.alive)).unwrap_or(true) {
        p.lock = Some(mobs[live[0]].idx);
    }
    let lock = p.lock.unwrap();
    let Some(ti) = mobs.iter().position(|m| m.idx == lock && m.alive) else {
        return;
    };
    let d = dist(p, &mobs[ti]);
    if d > MELEE_REACH_M {
        return;
    }
    let strike = p.strike_armed;
    let raw = p.stats.melee_hit(strike);
    if strike {
        p.strike_armed = false;
    }
    hit_mob(p, &mut mobs[ti], raw);
    p.swings += 1;
    if p.rank(Discipline::Martial) >= 7 {
        let tgt_idx = mobs[ti].idx;
        if let Some(oi) = living(mobs).into_iter().find(|&i| {
            mobs[i].idx != tgt_idx && mobs[i].alive && dist(p, &mobs[i]) <= CLEAVE_RANGE_M
        }) {
            let cleave = trunc(f64::from(raw) * CLEAVE_PCT);
            hit_mob(p, &mut mobs[oi], cleave);
        }
    }
}

fn move_entities(p: &mut Player, mobs: &mut [Mob], desired: f64) {
    let live = living(mobs);
    if live.is_empty() {
        return;
    }
    let chasing: Vec<usize> = live.iter().copied().filter(|&i| mobs[i].root <= 0.0).collect();
    let ref_set = if chasing.is_empty() { live } else { chasing };
    let nearest = *ref_set
        .iter()
        .min_by(|a, b| {
            dist(p, &mobs[**a])
                .partial_cmp(&dist(p, &mobs[**b]))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap();
    let d0 = dist(p, &mobs[nearest]);
    if d0 < desired - 0.05 {
        if matches!(p.stats.discipline, Discipline::Hunt | Discipline::Arcane) {
            p.x -= SPRINT_MPS * TICK;
            p.sprinted = true;
        } else {
            p.x -= WALK_MPS * TICK;
        }
    } else if d0 > desired + 0.05 {
        p.x += WALK_MPS * TICK;
    }
    let px = p.x;
    for m in mobs.iter_mut().filter(|m| m.alive) {
        if m.stun > 0.0 || m.root > 0.0 {
            continue;
        }
        let spd = m.speed * if m.slow > 0.0 { 1.0 - PIN_SLOW_PCT } else { 1.0 };
        if m.x > px {
            m.x = px.max(m.x - spd * TICK);
        } else {
            m.x = px.min(m.x + spd * TICK);
        }
    }
}

/// One 0.1 s combat step. Live clock accumulates to this or calls it directly.
pub fn tick(p: &mut Player, mobs: &mut [Mob], desired_range: f64, kite: bool) {
    for v in p.cds.values_mut() {
        if *v > 0.0 {
            *v = (*v - TICK).max(0.0);
        }
    }
    if p.gcd > 0.0 {
        p.gcd = (p.gcd - TICK).max(0.0);
    }
    if p.ward_t > 0.0 {
        p.ward_t = (p.ward_t - TICK).max(0.0);
        if p.ward_t <= 0.0 {
            p.ward = 0.0;
        }
    }
    if p.mark_t > 0.0 {
        p.mark_t = (p.mark_t - TICK).max(0.0);
    }
    if p.poison_t > 0.0 {
        p.poison_acc += SCORP_POISON_DPS * TICK;
        p.poison_t = (p.poison_t - TICK).max(0.0);
        if p.poison_acc >= 1.0 {
            let ticks = p.poison_acc as i32;
            p.poison_acc -= f64::from(ticks);
            apply_player_damage(p, ticks, true);
        }
    }
    p.mana = (p.mana + MANA_REGEN_PER_S * TICK).min(p.max_mana);
    if p.mana < p.min_mana {
        p.min_mana = p.mana;
    }
    if p.mana <= 1e-9 {
        p.mana_hit_zero = true;
    }

    for m in mobs.iter_mut() {
        if !m.alive {
            continue;
        }
        if m.stun > 0.0 {
            m.stun = (m.stun - TICK).max(0.0);
        }
        if m.root_grace > 0.0 {
            m.root_grace = (m.root_grace - TICK).max(0.0);
        }
        if m.root > 0.0 {
            m.root = (m.root - TICK).max(0.0);
        }
        if m.slow > 0.0 {
            m.slow = (m.slow - TICK).max(0.0);
        }
        if !m.enraged && m.max_hp > 0.0 && m.hp <= MOTHER_ENRAGE_HP * m.max_hp && m.id == "line_mother" {
            m.enraged = true;
            m.swing = m.base_swing * MOTHER_ENRAGE_SWING;
        }
    }

    if !p.alive {
        return;
    }

    player_ai_start(p, mobs, kite);

    if p.busy > 0.0 {
        p.busy = (p.busy - TICK).max(0.0);
        if p.busy <= 0.0 && p.cast.is_some() {
            finish_cast(p, mobs);
        }
    }

    if p.stats.discipline == Discipline::Martial {
        let in_range = mobs.iter().any(|m| m.alive && dist(p, m) <= MELEE_REACH_M);
        if in_range {
            p.auto_cd -= TICK;
            if p.auto_cd <= 0.0 {
                martial_auto(p, mobs);
                p.auto_cd += MELEE_SWING_S;
            }
        }
    }

    if !p.alive {
        return;
    }

    let grit = p.stats.attrs.grit;
    for mob in mobs.iter_mut() {
        if !mob.alive || mob.stun > 0.0 {
            continue;
        }
        if mob.has_slam {
            if mob.tele > 0.0 {
                mob.tele = (mob.tele - TICK).max(0.0);
                if mob.tele <= 0.0 && p.alive && dist(p, mob) <= mob.reach {
                    let raw = trunc(mob.slam_dmg * mob.dmg_mult());
                    let dealt = mitigation(f64::from(raw), grit);
                    apply_player_damage(p, dealt, true);
                }
            } else {
                mob.slam_cd -= TICK;
                if mob.slam_cd <= 0.0 {
                    mob.tele = mob.tele_len;
                    let every = mob.sheet.slam_every_s.unwrap_or(8.0);
                    mob.slam_cd += every;
                }
            }
        }
        if dist(p, mob) <= mob.reach {
            mob.auto_cd -= TICK;
            if mob.auto_cd <= 0.0 {
                let raw = trunc(mob.base_dmg * mob.dmg_mult());
                let dealt = mitigation(f64::from(raw), grit);
                apply_player_damage(p, dealt, true);
                if mob.sheet.id == "crawler_scorpion" && p.alive {
                    p.poison_t = SCORP_POISON_S;
                }
                mob.auto_cd += mob.swing;
            }
        }
    }

    move_entities(p, mobs, desired_range);
}

pub fn desired_range_for(disc: Discipline, kite: bool) -> (f64, f64) {
    if !kite || disc == Discipline::Martial {
        return (1.5, 1.5);
    }
    match disc {
        Discipline::Martial => (1.5, 1.5),
        Discipline::Hunt => (18.0, 18.0),
        Discipline::Arcane => (16.0, 16.0),
    }
}

#[derive(Debug, Clone)]
pub struct Fight {
    pub player: Player,
    pub mobs: Vec<Mob>,
    pub hold_d: f64,
    pub kite: bool,
    pub t: f64,
    pub seed: i32,
    pub notes: String,
}

impl Fight {
    pub fn tick(&mut self) {
        tick(&mut self.player, &mut self.mobs, self.hold_d, self.kite);
        self.t += TICK;
        self.t = (self.t * 10000.0).round() / 10000.0;
    }
}

pub fn simulate_fight(
    level: i32,
    discipline: Discipline,
    mob_id: &str,
    count: i32,
    seed: i32,
    potions: Option<i32>,
    arrows: Option<i32>,
    mob_level: Option<i32>,
    kite: Option<bool>,
    notes: &str,
) -> Result<Value, String> {
    let mob_id = resolve_mob_id(mob_id)?;
    let stats = player_stats(level, discipline);
    let potions = potions.unwrap_or(START_POTIONS);
    let arrows = arrows.unwrap_or(START_ARROWS);
    let mut p = Player::new(stats.clone(), potions, arrows);
    let disc = stats.discipline;
    let kite = kite.unwrap_or(disc != Discipline::Martial);
    let (start_d, hold_d) = desired_range_for(disc, kite);

    let mut sheets = Vec::new();
    let mut mobs = Vec::new();
    for i in 0..count {
        let sh = if mob_id == "crawler_spider_wolf" {
            wolf_sheet(mob_level.unwrap_or(level))
        } else {
            mob_sheet(&mob_id, mob_level)?
        };
        sheets.push(sh.clone());
        mobs.push(Mob::from_sheet(sh, start_d + f64::from(i) * 0.4, i));
    }

    p.auto_cd = MELEE_SWING_S;

    let mut t = 0.0;
    let mut winner = "timeout";
    while t < HARD_CAP_S - 1e-9 {
        tick(&mut p, &mut mobs, hold_d, kite);
        t += TICK;
        t = (t * 10000.0).round() / 10000.0;
        if !p.alive {
            winner = "mobs";
            break;
        }
        if !mobs.iter().any(|m| m.alive) {
            winner = "player";
            break;
        }
    }

    let ttk = if winner == "player" { Some(t) } else { None };
    let ttd = if winner == "mobs" || winner == "timeout" {
        Some(t)
    } else {
        None
    };

    let hp_pct = if p.max_hp > 0.0 {
        (100.0 * p.hp / p.max_hp * 10.0).round() / 10.0
    } else {
        0.0
    };

    Ok(json!({
        "time_to_kill_s": ttk,
        "time_to_die_s": ttd,
        "mana_spent": p.mana_spent,
        "swings": p.swings,
        "spells_used": p.spells,
        "winner": winner,
        "hp_remaining": p.hp.round() as i32,
        "hp_max": p.max_hp as i32,
        "hp_pct": hp_pct,
        "player_level": level,
        "discipline": disc.as_str(),
        "mob_id": mob_id,
        "mob_level": sheets.first().map(|s| s.level),
        "count": count,
        "seed": seed,
        "potions_used": p.spells.get("potion").copied().unwrap_or(0),
        "max_hit_on_player": p.max_hit_taken,
        "oneshot": p.max_hit_taken >= p.max_hp as i32,
        "player_sprinted": p.sprinted,
        "used_pin_or_bind": p.used_pin_or_bind,
        "damage_taken": p.damage_taken,
        "mana_hit_zero": p.mana_hit_zero,
        "skipped_spell_for_mana": p.skipped_spell_for_mana,
        "mana_decision": p.mana_hit_zero || p.skipped_spell_for_mana,
        "min_mana": (p.min_mana * 100.0).round() / 100.0,
        "mana_remaining": (p.mana * 100.0).round() / 100.0,
        "lock": p.lock,
        "notes": notes,
        "player": {
            "hp": p.max_hp as i32,
            "mana": p.max_mana as i32,
            "attrs": stats.attrs,
            "ranks": stats.ranks,
            "weapon_skill": stats.weapon_skill,
            "melee_hit": stats.melee_hit(false),
            "melee_strike": stats.melee_hit(true),
            "bow_hit": stats.bow_hit(false, 18.0),
            "bow_aimed": stats.bow_hit(true, 18.0),
            "ember": stats.ember(),
            "mend": stats.mend(),
            "ward": stats.ward(),
        },
        "mob_sheet": sheets.first(),
    }))
}

fn kite_walk_fail(r: &Value) -> bool {
    let disc = r["discipline"].as_str().unwrap_or("");
    if disc != "Hunt" && disc != "Arcane" {
        return false;
    }
    r["damage_taken"].as_i64().unwrap_or(0) == 0
        && !r["player_sprinted"].as_bool().unwrap_or(false)
        && !r["used_pin_or_bind"].as_bool().unwrap_or(false)
}

fn even_1v1_pass(r: &Value) -> (bool, &'static str) {
    if r["winner"].as_str() != Some("player") {
        return (false, "player must win");
    }
    let ttk = r["time_to_kill_s"].as_f64();
    if ttk.map(|t| !(8.0..=14.0).contains(&t)).unwrap_or(true) {
        return (false, "TTK not in 8.0-14.0");
    }
    let hp = r["hp_remaining"].as_f64().unwrap_or(0.0);
    let hp_max = r["hp_max"].as_f64().unwrap_or(1.0);
    if hp < 0.40 * hp_max {
        return (false, "end HP < 40%");
    }
    if r["oneshot"].as_bool().unwrap_or(false) {
        return (false, "one-shot");
    }
    if kite_walk_fail(r) {
        return (false, "0 damage walking, no Pin/Bind");
    }
    (true, "even 1v1")
}

fn two_pull_nopot_pass(r: &Value) -> (bool, &'static str) {
    if r["winner"].as_str() == Some("mobs") {
        return (true, "lose (expected risky)");
    }
    let hp = r["hp_remaining"].as_f64().unwrap_or(0.0);
    let hp_max = r["hp_max"].as_f64().unwrap_or(1.0);
    if r["winner"].as_str() == Some("player") && hp < 0.20 * hp_max {
        return (true, "win but <20% HP");
    }
    (false, "too safe")
}

fn two_pull_pot_pass(r: &Value) -> (bool, &'static str) {
    if r["winner"].as_str() == Some("player") {
        (true, "win with potion")
    } else {
        (false, "lost")
    }
}

fn pale_hall_pass(r: &Value) -> (bool, &'static str) {
    if r["winner"].as_str() != Some("player") {
        return (false, "must WIN");
    }
    let ttk = r["time_to_kill_s"].as_f64();
    if ttk.map(|t| !(20.0..=50.0).contains(&t)).unwrap_or(true) {
        return (false, "TTK not in 20-50 (room)");
    }
    if r["hp_remaining"].as_i64().unwrap_or(0) <= 0 {
        return (false, "end HP not >0");
    }
    if r["oneshot"].as_bool().unwrap_or(false) {
        return (false, "one-shot");
    }
    if kite_walk_fail(r) {
        return (false, "0 damage walking, no Pin/Bind");
    }
    (true, "Pale Hall room")
}

fn heart_pass(r: &Value) -> (bool, &'static str) {
    if r["winner"].as_str() != Some("player") {
        return (false, "must WIN");
    }
    let ttk = r["time_to_kill_s"].as_f64();
    if ttk.map(|t| !(25.0..=45.0).contains(&t)).unwrap_or(true) {
        return (false, "TTK not in 25-45 (heart)");
    }
    let hp = r["hp_remaining"].as_f64().unwrap_or(0.0);
    let hp_max = r["hp_max"].as_f64().unwrap_or(1.0);
    if hp < 0.20 * hp_max || hp > 0.50 * hp_max {
        return (false, "end HP not in 20-50%");
    }
    if r["oneshot"].as_bool().unwrap_or(false) {
        return (false, "one-shot");
    }
    (true, "heart")
}

fn win_pass(r: &Value) -> (bool, &'static str) {
    if r["winner"].as_str() == Some("player") && !r["oneshot"].as_bool().unwrap_or(false) {
        (true, "must WIN")
    } else {
        (false, "lost or oneshot")
    }
}

struct Scenario {
    id: &'static str,
    title: &'static str,
    level: i32,
    discipline: Discipline,
    mob: &'static str,
    count: i32,
    mob_level: i32,
    potions: i32,
    kite: bool,
    band: &'static str,
    check: fn(&Value) -> (bool, &'static str),
}

const SCENARIOS: &[Scenario] = &[
    Scenario { id: "1_l1_martial_1wolf", title: "L1 Martial vs 1 wolf-spider", level: 1, discipline: Discipline::Martial, mob: "crawler_spider_wolf", count: 1, mob_level: 1, potions: 1, kite: false, band: "even_1v1", check: even_1v1_pass },
    Scenario { id: "2_l1_martial_2wolf_nopot", title: "L1 Martial vs 2 wolf-spiders, no potion", level: 1, discipline: Discipline::Martial, mob: "crawler_spider_wolf", count: 2, mob_level: 1, potions: 0, kite: false, band: "2pull_nopot", check: two_pull_nopot_pass },
    Scenario { id: "3_l1_martial_2wolf_pot", title: "L1 Martial vs 2 wolf-spiders, 1 potion", level: 1, discipline: Discipline::Martial, mob: "crawler_spider_wolf", count: 2, mob_level: 1, potions: 1, kite: false, band: "2pull_pot", check: two_pull_pot_pass },
    Scenario { id: "4_l3_hunt_scorpion", title: "L3 Hunt vs 1 scorpion", level: 3, discipline: Discipline::Hunt, mob: "crawler_scorpion", count: 1, mob_level: 3, potions: 1, kite: true, band: "even_1v1", check: even_1v1_pass },
    Scenario { id: "5_l5_arcane_pale_hall", title: "L5 Arcane vs Pale Hall (4 L4 wolves)", level: 5, discipline: Discipline::Arcane, mob: "crawler_spider_wolf", count: 4, mob_level: 4, potions: 1, kite: true, band: "pale_hall", check: pale_hall_pass },
    Scenario { id: "6_l9_martial_line_mother", title: "L9 Martial vs Line-Mother", level: 9, discipline: Discipline::Martial, mob: "line_mother", count: 1, mob_level: 9, potions: 1, kite: false, band: "heart", check: heart_pass },
    Scenario { id: "7_l2_martial_blob", title: "L2 Martial vs 1 GreenBlob", level: 2, discipline: Discipline::Martial, mob: "green_blob", count: 1, mob_level: 2, potions: 1, kite: false, band: "even_1v1", check: even_1v1_pass },
    Scenario { id: "s_l2_martial_d1_pot", title: "SANITY L2 Martial vs D1 (2 L1 wolves) + potion", level: 2, discipline: Discipline::Martial, mob: "crawler_spider_wolf", count: 2, mob_level: 1, potions: 1, kite: false, band: "d1_clear", check: win_pass },
];

pub fn run_scenario(id: &str, seed: i32) -> Result<Value, String> {
    let sc = SCENARIOS
        .iter()
        .find(|s| s.id == id || s.id.starts_with(id))
        .ok_or_else(|| format!("unknown scenario: {id}"))?;
    run_scenario_sc(sc, seed)
}

fn run_scenario_sc(sc: &Scenario, seed: i32) -> Result<Value, String> {
    let mut r = simulate_fight(
        sc.level, sc.discipline, sc.mob, sc.count, seed, Some(sc.potions), None, Some(sc.mob_level), Some(sc.kite), sc.title,
    )?;
    let (ok, why) = (sc.check)(&r);
    r["band"] = json!(sc.band);
    r["band_pass"] = json!(ok);
    r["band_reason"] = json!(why);
    r["scenario_id"] = json!(sc.id);
    r["title"] = json!(sc.title);
    Ok(r)
}

pub fn oneshot_sanity() -> Value {
    let p = player_stats(5, Discipline::Arcane);
    let w = wolf_sheet(4);
    let hit = mitigation(f64::from(w.damage), p.attrs.grit);
    let volley = hit * 4;
    json!({
        "title": "SANITY L5 Arcane not one-shot by 4 L4 wolves first swing",
        "scenario_id": "s_l5_arcane_oneshot",
        "player_level": 5,
        "discipline": "Arcane",
        "mob_id": "crawler_spider_wolf",
        "count": 4,
        "seed": 1,
        "winner": "n/a",
        "time_to_kill_s": null,
        "time_to_die_s": null,
        "mana_spent": 0,
        "swings": 0,
        "spells_used": {},
        "hp_remaining": p.hp,
        "hp_max": p.hp,
        "hp_pct": 100.0,
        "one_wolf_hit": hit,
        "four_wolf_volley": volley,
        "player_max_hp": p.hp,
        "oneshot": volley >= p.hp,
        "band": "no_oneshot",
        "band_pass": volley < p.hp,
        "band_reason": format!("4x{hit}={volley} vs {} HP", p.hp),
        "notes": "first-swing volley vs L5 Arcane max HP (kiting aside)",
        "max_hit_on_player": volley,
        "player_sprinted": false,
        "used_pin_or_bind": false,
        "damage_taken": 0,
        "mana_hit_zero": false,
        "skipped_spell_for_mana": false,
        "mana_decision": false,
        "min_mana": null,
    })
}

pub fn run_all(seed: i32) -> Result<Value, String> {
    let mut results = Vec::new();
    for sc in SCENARIOS {
        results.push(run_scenario_sc(sc, seed)?);
    }
    results.push(oneshot_sanity());
    let path = leveling_path();
    let all_pass = results.iter().all(|r| r["band_pass"].as_bool() == Some(true))
        && path["in_90_150"].as_bool() == Some(true);
    Ok(json!({
        "formulas": formulas(),
        "xp_curve": xp_curve(),
        "leveling_path": path,
        "mob_sheets": mob_sheets_json(),
        "player_specialists": player_specialists_json(),
        "scenarios": results,
        "all_pass": all_pass,
        "seed": seed,
    }))
}

pub fn print_table(rows: &[Value]) {
    println!(
        "{:<46} {:<8} {:>6} {:>6} {:>6} {:>5} {:>5} {:>3} {:>3} {:>4} {:>4} {:<12} {:<6}",
        "SCENARIO", "WIN", "TTK", "TTD", "HP%", "HP", "MANA", "SPR", "PIN", "DMG", "MDEC", "BAND", "PASS"
    );
    println!("{}", "-".repeat(130));
    for r in rows {
        let ttk = r["time_to_kill_s"].as_f64().map(|t| format!("{t:.1}")).unwrap_or_else(|| "-".into());
        let ttd = r["time_to_die_s"].as_f64().map(|t| format!("{t:.1}")).unwrap_or_else(|| "-".into());
        let title = r["title"].as_str().or_else(|| r["notes"].as_str()).or_else(|| r["scenario_id"].as_str()).unwrap_or("");
        let title: String = title.chars().take(46).collect();
        let bp = r.get("band_pass").and_then(|v| v.as_bool());
        let mark = match bp {
            Some(true) => "PASS",
            Some(false) => "FAIL",
            None => "-",
        };
        let spr = if r["player_sprinted"].as_bool() == Some(true) { "Y" } else { "n" };
        let pin = if r["used_pin_or_bind"].as_bool() == Some(true) { "Y" } else { "n" };
        let mdec = if r["mana_decision"].as_bool() == Some(true) { "Y" } else { "n" };
        println!(
            "{:<46} {:<8} {:>6} {:>6} {:>5.1}% {:>5} {:>5} {:>3} {:>3} {:>4} {:>4} {:<12} {:<6}",
            title,
            r["winner"].as_str().unwrap_or(""),
            ttk,
            ttd,
            r["hp_pct"].as_f64().unwrap_or(0.0),
            r["hp_remaining"].as_i64().unwrap_or(0),
            r["mana_spent"].as_i64().unwrap_or(0),
            spr,
            pin,
            r["damage_taken"].as_i64().unwrap_or(0),
            mdec,
            r["band"].as_str().unwrap_or(""),
            mark
        );
    }
}

pub fn scenario_ids() -> Vec<&'static str> {
    SCENARIOS.iter().map(|s| s.id).collect()
}

pub fn fixture_scenario_1() -> Result<Value, String> {
    run_scenario("1_l1_martial_1wolf", 1)
}

/// Exact published TTK table. If Rust disagrees, Rust is wrong — do not widen a band.
pub fn match_published_rows(payload: &Value) -> Result<(), String> {
    let rows = payload
        .get("scenarios")
        .and_then(|v| v.as_array())
        .ok_or("missing scenarios")?;
    let expect = [
        ("1_l1_martial_1wolf", "player", Some(10.8), None, 55, 100),
        ("2_l1_martial_2wolf_nopot", "mobs", None, Some(14.1), 0, 100),
        ("3_l1_martial_2wolf_pot", "player", Some(21.6), None, 5, 100),
        ("4_l3_hunt_scorpion", "player", Some(11.1), None, 124, 124),
        ("5_l5_arcane_pale_hall", "player", Some(46.4), None, 148, 148),
        ("6_l9_martial_line_mother", "player", Some(32.4), None, 94, 196),
        ("7_l2_martial_blob", "player", Some(10.8), None, 72, 112),
        ("s_l2_martial_d1_pot", "player", Some(19.8), None, 35, 112),
    ];
    let mut errs = Vec::new();
    for (id, winner, ttk, ttd, hp, hp_max) in expect {
        let Some(r) = rows
            .iter()
            .find(|r| r.get("scenario_id").and_then(|v| v.as_str()) == Some(id))
        else {
            errs.push(format!("{id}: missing"));
            continue;
        };
        let got_w = r.get("winner").and_then(|v| v.as_str()).unwrap_or("");
        if got_w != winner {
            errs.push(format!("{id}: winner {got_w} != {winner}"));
        }
        let got_ttk = r.get("time_to_kill_s").and_then(|v| v.as_f64());
        if !close_opt(got_ttk, ttk) {
            errs.push(format!("{id}: TTK {got_ttk:?} != {ttk:?}"));
        }
        let got_ttd = r.get("time_to_die_s").and_then(|v| v.as_f64());
        if !close_opt(got_ttd, ttd) {
            errs.push(format!("{id}: TTD {got_ttd:?} != {ttd:?}"));
        }
        let got_hp = r.get("hp_remaining").and_then(|v| v.as_i64()).unwrap_or(-1);
        let got_max = r.get("hp_max").and_then(|v| v.as_i64()).unwrap_or(-1);
        if got_hp != hp || got_max != hp_max {
            errs.push(format!("{id}: HP {got_hp}/{got_max} != {hp}/{hp_max}"));
        }
    }
    if errs.is_empty() {
        Ok(())
    } else {
        Err(errs.join("; "))
    }
}

fn close_opt(a: Option<f64>, b: Option<f64>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) => (x - y).abs() <= 0.05,
        _ => false,
    }
}

