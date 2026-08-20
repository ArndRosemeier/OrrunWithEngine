//! Player specialist spend, mob sheets, XP tables. Oracle: combat_sim.py.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{json, Value};

use crate::combat::math::*;
use crate::combat::{Attrs, Discipline, Ranks};

#[derive(Debug, Clone, Serialize)]
pub struct PlayerStats {
    pub level: i32,
    pub discipline: Discipline,
    pub hp: i32,
    pub mana: i32,
    pub attrs: Attrs,
    pub ranks: Ranks,
    pub weapon_skill: i32,
    pub skill_cap: i32,
    pub skill_xp: i32,
}

impl PlayerStats {
    pub fn melee_hit(&self, strike: bool) -> i32 {
        melee_hit(self.attrs.might, self.weapon_skill, self.ranks.martial, strike)
    }

    pub fn bow_hit(&self, aimed: bool, distance: f64) -> i32 {
        bow_hit(
            self.attrs.swift,
            self.weapon_skill,
            self.ranks.hunt,
            aimed,
            distance,
        )
    }

    pub fn ember(&self) -> i32 {
        ember(self.attrs.will, self.weapon_skill, self.ranks.arcane)
    }

    pub fn mend(&self) -> i32 {
        mend(self.attrs.will, self.ranks.arcane)
    }

    pub fn ward(&self) -> i32 {
        ward(self.attrs.will, self.ranks.arcane)
    }
}

pub fn melee_raw(p: &PlayerStats, strike: bool) -> i32 {
    p.melee_hit(strike)
}

pub fn bow_raw(p: &PlayerStats, aimed: bool, distance: f64) -> i32 {
    p.bow_hit(aimed, distance)
}

pub fn ember_raw(p: &PlayerStats) -> i32 {
    p.ember()
}

pub fn mend_raw(p: &PlayerStats) -> i32 {
    p.mend()
}

pub fn ward_raw(p: &PlayerStats) -> i32 {
    p.ward()
}

pub fn player_stats(level: i32, discipline: Discipline) -> PlayerStats {
    let gained = level - 1;
    let points = gained * ATTR_PER_LEVEL;
    let mut attrs = Attrs {
        might: ATTR_BASE,
        swift: ATTR_BASE,
        will: ATTR_BASE,
        grit: ATTR_BASE,
    };
    let ranks = match discipline {
        Discipline::Martial => {
            attrs.might += points;
            Ranks {
                martial: level,
                hunt: 0,
                arcane: 0,
            }
        }
        Discipline::Hunt => {
            attrs.swift += points;
            Ranks {
                martial: 0,
                hunt: level,
                arcane: 0,
            }
        }
        Discipline::Arcane => {
            attrs.will += points;
            Ranks {
                martial: 0,
                hunt: 0,
                arcane: level,
            }
        }
    };
    let skill_cap = level * SKILL_CAP_PER_LEVEL;
    PlayerStats {
        level,
        discipline,
        hp: HP_L1 + HP_PER_LEVEL * gained,
        mana: MANA_L1 + MANA_PER_LEVEL * gained,
        attrs,
        ranks,
        weapon_skill: skill_cap,
        skill_cap,
        skill_xp: 0,
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct MobSheet {
    pub id: String,
    pub name: String,
    pub level: i32,
    pub hp: i32,
    pub armor: i32,
    pub damage: i32,
    pub swing_s: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slam_damage: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slam_every_s: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub telegraph_s: Option<f64>,
    pub reach_m: f64,
    pub speed_mps: f64,
    pub sight_m: f64,
    pub hear_m: f64,
    pub leash_m: f64,
    pub social_m: f64,
    pub xp: i32,
    pub token_brood: i32,
    pub specials: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale_hp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale_dmg: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale_xp: Option<String>,
}

fn aggro_fields() -> (f64, f64, f64, f64) {
    (SIGHT_AGGRO_M, HEAR_AGGRO_M, LEASH_M, SOCIAL_M)
}

pub fn wolf_sheet(level: i32) -> MobSheet {
    let (sight, hear, leash, social) = aggro_fields();
    MobSheet {
        id: "crawler_spider_wolf".into(),
        name: "wolf-spider".into(),
        level,
        hp: WOLF_HP_BASE + WOLF_HP_PER * (level - 1),
        armor: WOLF_ARMOR,
        damage: WOLF_DMG_BASE + WOLF_DMG_PER * (level - 1),
        swing_s: WOLF_SWING_S,
        slam_damage: None,
        slam_every_s: None,
        telegraph_s: None,
        reach_m: WOLF_REACH_M,
        speed_mps: WOLF_SPEED,
        sight_m: sight,
        hear_m: hear,
        leash_m: leash,
        social_m: social,
        xp: WOLF_XP_BASE + WOLF_XP_PER * (level - 1),
        token_brood: 1,
        specials: vec![],
        scale_hp: Some("70 + 18*(lvl-1)".into()),
        scale_dmg: Some("10 + 2*(lvl-1)".into()),
        scale_xp: Some("35 + 12*(lvl-1)".into()),
    }
}

pub fn mother_sheet() -> MobSheet {
    let (sight, hear, leash, social) = aggro_fields();
    MobSheet {
        id: "line_mother".into(),
        name: "Line-Mother".into(),
        level: MOTHER_LEVEL,
        hp: MOTHER_HP,
        armor: MOTHER_ARMOR,
        damage: MOTHER_DMG,
        swing_s: MOTHER_SWING_S,
        slam_damage: Some(MOTHER_SLAM),
        slam_every_s: Some(MOTHER_SLAM_EVERY_S),
        telegraph_s: Some(MOTHER_TELE_S),
        reach_m: MOTHER_REACH_M,
        speed_mps: MOTHER_SPEED,
        sight_m: sight,
        hear_m: hear,
        leash_m: leash,
        social_m: social,
        xp: MOTHER_XP,
        token_brood: MOTHER_TOKEN,
        specials: vec![
            "slam_every_8s_1.2s_telegraph_interruptible".into(),
            "enrage_30pct_dmg*1.3_swing*0.77".into(),
        ],
        scale_hp: None,
        scale_dmg: None,
        scale_xp: None,
    }
}

pub fn scorpion_sheet() -> MobSheet {
    let w = wolf_sheet(SCORP_LEVEL);
    MobSheet {
        id: "crawler_scorpion".into(),
        name: "scorpion".into(),
        specials: vec!["poison_3hps_4s_refresh_not_stack".into()],
        scale_hp: None,
        scale_dmg: None,
        scale_xp: None,
        ..w
    }
}

pub fn blob_sheet() -> MobSheet {
    let (sight, hear, leash, social) = aggro_fields();
    MobSheet {
        id: "green_blob".into(),
        name: "GreenBlob".into(),
        level: BLOB_LEVEL,
        hp: BLOB_HP,
        armor: BLOB_ARMOR,
        damage: BLOB_DMG,
        swing_s: BLOB_SWING_S,
        slam_damage: None,
        slam_every_s: None,
        telegraph_s: None,
        reach_m: BLOB_REACH_M,
        speed_mps: BLOB_SPEED,
        sight_m: sight,
        hear_m: hear,
        leash_m: leash,
        social_m: social,
        xp: BLOB_XP,
        token_brood: 0,
        specials: vec!["slow".into(), "no_poison".into()],
        scale_hp: None,
        scale_dmg: None,
        scale_xp: None,
    }
}

pub fn orc_sheet() -> MobSheet {
    let (sight, hear, leash, social) = aggro_fields();
    MobSheet {
        id: "orc".into(),
        name: "orc".into(),
        level: ORC_LEVEL,
        hp: ORC_HP,
        armor: ORC_ARMOR,
        damage: ORC_DMG,
        swing_s: ORC_SWING_S,
        slam_damage: None,
        slam_every_s: None,
        telegraph_s: None,
        reach_m: ORC_REACH_M,
        speed_mps: ORC_SPEED,
        sight_m: sight,
        hear_m: hear,
        leash_m: leash,
        social_m: social,
        xp: ORC_XP,
        token_brood: 0,
        specials: vec![],
        scale_hp: None,
        scale_dmg: None,
        scale_xp: None,
    }
}

pub fn tribal_sheet() -> MobSheet {
    let (sight, hear, leash, social) = aggro_fields();
    MobSheet {
        id: "tribal".into(),
        name: "tribal".into(),
        level: TRIBAL_LEVEL,
        hp: TRIBAL_HP,
        armor: TRIBAL_ARMOR,
        damage: TRIBAL_DMG,
        swing_s: TRIBAL_SWING_S,
        slam_damage: None,
        slam_every_s: None,
        telegraph_s: None,
        reach_m: TRIBAL_REACH_M,
        speed_mps: TRIBAL_SPEED,
        sight_m: sight,
        hear_m: hear,
        leash_m: leash,
        social_m: social,
        xp: TRIBAL_XP,
        token_brood: 0,
        specials: vec!["punch".into()],
        scale_hp: None,
        scale_dmg: None,
        scale_xp: None,
    }
}

pub fn bandit_sheet() -> MobSheet {
    let (sight, hear, leash, social) = aggro_fields();
    MobSheet {
        id: "bandit".into(),
        name: "bandit".into(),
        level: BANDIT_LEVEL,
        hp: BANDIT_HP,
        armor: BANDIT_ARMOR,
        damage: BANDIT_DMG,
        swing_s: BANDIT_SWING_S,
        slam_damage: None,
        slam_every_s: None,
        telegraph_s: None,
        reach_m: BANDIT_REACH_M,
        speed_mps: BANDIT_SPEED,
        sight_m: sight,
        hear_m: hear,
        leash_m: leash,
        social_m: social,
        xp: BANDIT_XP,
        token_brood: 0,
        specials: vec![],
        scale_hp: None,
        scale_dmg: None,
        scale_xp: None,
    }
}

pub fn orc_skull_sheet() -> MobSheet {
    let (sight, hear, leash, social) = aggro_fields();
    MobSheet {
        id: "orc_skull".into(),
        name: "orc-skull".into(),
        level: SKULL_LEVEL,
        hp: SKULL_HP,
        armor: SKULL_ARMOR,
        damage: SKULL_DMG,
        swing_s: SKULL_SWING_S,
        slam_damage: Some(SKULL_BOLT_DMG),
        slam_every_s: Some(0.0),
        telegraph_s: Some(SKULL_TELE_S),
        reach_m: SKULL_REACH_M,
        speed_mps: SKULL_SPEED,
        sight_m: sight,
        hear_m: hear,
        leash_m: leash,
        social_m: social,
        xp: SKULL_XP,
        token_brood: 0,
        specials: vec![
            "punch".into(),
            "weapon_bolt_24m_1.2s_telegraph_interruptible".into(),
        ],
        scale_hp: None,
        scale_dmg: None,
        scale_xp: None,
    }
}

pub fn skeleton_warrior_sheet() -> MobSheet {
    let (sight, hear, leash, social) = aggro_fields();
    MobSheet {
        id: "skeleton_warrior".into(),
        name: "Warrior".into(),
        level: BONE_LEVEL,
        hp: WARRIOR_HP,
        armor: WARRIOR_ARMOR,
        damage: WARRIOR_DMG,
        swing_s: WARRIOR_SWING_S,
        slam_damage: None,
        slam_every_s: None,
        telegraph_s: None,
        reach_m: WARRIOR_REACH_M,
        speed_mps: WARRIOR_SPEED,
        sight_m: sight,
        hear_m: hear,
        leash_m: leash,
        social_m: social,
        xp: WARRIOR_XP,
        token_brood: 0,
        specials: vec![],
        scale_hp: None,
        scale_dmg: None,
        scale_xp: None,
    }
}

pub fn skeleton_minion_sheet() -> MobSheet {
    let mut sheet = skeleton_warrior_sheet();
    sheet.id = "skeleton_minion".into();
    sheet.name = "Minion".into();
    sheet.hp = MINION_HP;
    sheet
}

pub fn skeleton_mage_sheet() -> MobSheet {
    let (sight, hear, leash, social) = aggro_fields();
    MobSheet {
        id: "skeleton_mage".into(),
        name: "Mage".into(),
        level: BONE_LEVEL,
        hp: MAGE_HP,
        armor: WARRIOR_ARMOR,
        damage: WARRIOR_DMG,
        swing_s: WARRIOR_SWING_S,
        slam_damage: Some(MAGE_BOLT_DMG),
        slam_every_s: Some(0.0),
        telegraph_s: Some(MAGE_TELE_S),
        reach_m: WARRIOR_REACH_M,
        speed_mps: WARRIOR_SPEED,
        sight_m: sight,
        hear_m: hear,
        leash_m: leash,
        social_m: social,
        xp: WARRIOR_XP,
        token_brood: 0,
        specials: vec![
            "punch_a".into(),
            "staff_bolt_24m_1.2s_telegraph_interruptible".into(),
        ],
        scale_hp: None,
        scale_dmg: None,
        scale_xp: None,
    }
}

pub fn demon_sheet() -> MobSheet {
    let (sight, hear, leash, social) = aggro_fields();
    MobSheet {
        id: "demon".into(),
        name: "demon".into(),
        level: DEMON_LEVEL,
        hp: DEMON_HP,
        armor: DEMON_ARMOR,
        damage: DEMON_DMG,
        swing_s: DEMON_SWING_S,
        slam_damage: None,
        slam_every_s: None,
        telegraph_s: None,
        reach_m: DEMON_REACH_M,
        speed_mps: DEMON_SPEED,
        sight_m: sight,
        hear_m: hear,
        leash_m: leash,
        social_m: social,
        xp: DEMON_XP,
        token_brood: 0,
        specials: vec![],
        scale_hp: None,
        scale_dmg: None,
        scale_xp: None,
    }
}

pub fn blue_demon_sheet() -> MobSheet {
    let (sight, hear, leash, social) = aggro_fields();
    MobSheet {
        id: "blue_demon".into(),
        name: "blue_demon".into(),
        level: BLUE_DEMON_LEVEL,
        hp: BLUE_DEMON_HP,
        armor: BLUE_DEMON_ARMOR,
        damage: BLUE_DEMON_DMG,
        swing_s: BLUE_DEMON_SWING_S,
        slam_damage: None,
        slam_every_s: None,
        telegraph_s: None,
        reach_m: BLUE_DEMON_REACH_M,
        speed_mps: BLUE_DEMON_SPEED,
        sight_m: sight,
        hear_m: hear,
        leash_m: leash,
        social_m: social,
        xp: BLUE_DEMON_XP,
        token_brood: 0,
        specials: vec![],
        scale_hp: None,
        scale_dmg: None,
        scale_xp: None,
    }
}

pub fn yeti_sheet() -> MobSheet {
    let (sight, hear, leash, social) = aggro_fields();
    MobSheet {
        id: "yeti".into(),
        name: "yeti".into(),
        level: YETI_LEVEL,
        hp: YETI_HP,
        armor: YETI_ARMOR,
        damage: YETI_DMG,
        swing_s: YETI_SWING_S,
        slam_damage: Some(YETI_SLAM),
        slam_every_s: Some(YETI_SLAM_EVERY_S),
        telegraph_s: Some(YETI_TELE_S),
        reach_m: YETI_REACH_M,
        speed_mps: YETI_SPEED,
        sight_m: sight,
        hear_m: hear,
        leash_m: leash,
        social_m: social,
        xp: YETI_XP,
        token_brood: 0,
        specials: vec!["slam_every_8s_1.0s_telegraph_interruptible".into()],
        scale_hp: None,
        scale_dmg: None,
        scale_xp: None,
    }
}

pub fn resolve_mob_id(name: &str) -> Result<String, String> {
    match name {
        "crawler_spider_wolf" | "wolf" | "wolf-spider" | "wolf_spider" => {
            Ok("crawler_spider_wolf".into())
        }
        "crawler_scorpion" | "scorpion" => Ok("crawler_scorpion".into()),
        "line_mother" | "line-mother" | "Line-Mother" => Ok("line_mother".into()),
        "green_blob" | "GreenBlob" | "greenblob" => Ok("green_blob".into()),
        "orc" => Ok("orc".into()),
        "tribal" => Ok("tribal".into()),
        "orc_skull" | "orc-skull" | "skull" => Ok("orc_skull".into()),
        "skeleton_warrior" | "Warrior" | "warrior" => Ok("skeleton_warrior".into()),
        "skeleton_minion" | "Minion" | "minion" => Ok("skeleton_minion".into()),
        "skeleton_mage" | "Mage" | "mage" => Ok("skeleton_mage".into()),
        "yeti" => Ok("yeti".into()),
        "demon" => Ok("demon".into()),
        "blue_demon" | "BlueDemon" => Ok("blue_demon".into()),
        "bandit" | "male_bandit" => Ok("bandit".into()),
        other => Err(format!("unknown mob: {other}")),
    }
}

pub fn mob_sheet(id: &str, level: Option<i32>) -> Result<MobSheet, String> {
    let id = resolve_mob_id(id)?;
    Ok(match id.as_str() {
        "crawler_spider_wolf" => wolf_sheet(level.unwrap_or(1)),
        "crawler_scorpion" => scorpion_sheet(),
        "line_mother" => mother_sheet(),
        "green_blob" => blob_sheet(),
        "orc" => orc_sheet(),
        "tribal" => tribal_sheet(),
        "orc_skull" => orc_skull_sheet(),
        "skeleton_warrior" => skeleton_warrior_sheet(),
        "skeleton_minion" => skeleton_minion_sheet(),
        "skeleton_mage" => skeleton_mage_sheet(),
        "yeti" => yeti_sheet(),
        "demon" => demon_sheet(),
        "blue_demon" => blue_demon_sheet(),
        "bandit" => bandit_sheet(),
        other => return Err(format!("unknown mob: {other}")),
    })
}

pub fn xp_to_next(from_level: i32) -> Option<i32> {
    match from_level {
        1 => Some(200),
        2 => Some(250),
        3 => Some(300),
        4 => Some(550),
        5 => Some(700),
        6 => Some(850),
        7 => Some(1000),
        8 => Some(1200),
        9 => Some(1500),
        _ => None,
    }
}

pub fn heart_bonus(tier: i32) -> i32 {
    match tier {
        0 => 40,
        1 => 70,
        2 => 120,
        _ => 0,
    }
}

pub fn first_clear(tier: i32) -> i32 {
    match tier {
        0 => 80,
        1 => 140,
        2 => 200,
        _ => 0,
    }
}

pub fn min_per_dungeon(tier: i32) -> f64 {
    match tier {
        0 => 4.0,
        1 => 7.0,
        2 => 12.0,
        _ => 0.0,
    }
}

pub fn dungeon_wolves(tier: i32) -> i32 {
    match tier {
        0 => 2,
        1 => 4,
        2 => 6,
        _ => 0,
    }
}

pub fn dungeon_wolf_level(tier: i32) -> i32 {
    match tier {
        0 => 1,
        1 => 4,
        2 => 6,
        _ => 1,
    }
}

pub fn dungeon_xp(tier: i32, first: bool) -> Value {
    let n = dungeon_wolves(tier);
    let lvl = dungeon_wolf_level(tier);
    let trash = n * wolf_sheet(lvl).xp;
    let elite = if tier == 2 { MOTHER_XP } else { 0 };
    let heart = heart_bonus(tier);
    let first_b = if first { first_clear(tier) } else { 0 };
    json!({
        "tier": tier,
        "first": first,
        "trash_xp": trash,
        "elite_xp": elite,
        "heart_bonus": heart,
        "first_clear": first_b,
        "total": trash + elite + heart + first_b,
        "minutes": min_per_dungeon(tier),
    })
}

pub fn xp_curve() -> Value {
    let mut cum = 0;
    let mut rows = Vec::new();
    let mut xp_map = BTreeMap::new();
    for n in 1..10 {
        let need = xp_to_next(n).unwrap_or(0);
        cum += need;
        xp_map.insert(n.to_string(), need);
        rows.push(json!({
            "from": n,
            "to": n + 1,
            "xp": need,
            "cumulative_to": cum,
        }));
    }
    json!({
        "xp_to_next": xp_map,
        "rows": rows,
        "cumulative_to_10": cum,
        "heart_bonus": {"0": 40, "1": 70, "2": 120},
        "first_clear_bonus": {"0": 80, "1": 140, "2": 200},
        "minutes_per_dungeon": {"0": 4, "1": 7, "2": 12},
        "dungeon_wolf_counts": {"0": 2, "1": 4, "2": 6},
        "dungeon_wolf_level": {"0": 1, "1": 4, "2": 6},
    })
}


fn grant_xp(
    need: &BTreeMap<i32, i32>,
    level: &mut i32,
    xp_into: &mut i32,
    minutes: &mut f64,
    events: &mut Vec<Value>,
    amount: i32,
    label: &str,
    mins: f64,
) {
    *minutes += mins;
    let mut left = amount;
    let mut leveled = Vec::new();
    while *level < 10 && left > 0 {
        let room = need[level] - *xp_into;
        let take = room.min(left);
        *xp_into += take;
        left -= take;
        if *xp_into >= need[level] {
            *level += 1;
            *xp_into = 0;
            leveled.push(*level);
        }
    }
    events.push(json!({
        "label": label,
        "xp": amount,
        "minutes": mins,
        "level_after": *level,
        "xp_into_level": *xp_into,
        "leveled_to": leveled,
        "total_minutes": *minutes,
    }));
}

pub fn leveling_path() -> Value {
    let mut need = BTreeMap::new();
    for n in 1..10 {
        need.insert(n, xp_to_next(n).unwrap_or(0));
    }
    let mut level = 1i32;
    let mut xp_into = 0i32;
    let mut minutes = 0.0f64;
    let mut events = Vec::new();


    let d1f = dungeon_xp(0, true);
    grant_xp(&need, &mut level, &mut xp_into, &mut minutes, &mut events,
        d1f["total"].as_i64().unwrap() as i32,
        "D1 first-clear (2 L1 wolves + heart + first)",
        d1f["minutes"].as_f64().unwrap(),
    );
    while level < 4 {
        let d1 = dungeon_xp(0, false);
        grant_xp(&need, &mut level, &mut xp_into, &mut minutes, &mut events,
            d1["total"].as_i64().unwrap() as i32,
            "D1 repeat (2 L1 wolves + heart)",
            d1["minutes"].as_f64().unwrap(),
        );
    }
    let d2f = dungeon_xp(1, true);
    grant_xp(&need, &mut level, &mut xp_into, &mut minutes, &mut events,
        d2f["total"].as_i64().unwrap() as i32,
        "D2 first-clear Pale Hall (4 L4 wolves + heart + first)",
        d2f["minutes"].as_f64().unwrap(),
    );
    while level < 8 {
        let d2 = dungeon_xp(1, false);
        grant_xp(&need, &mut level, &mut xp_into, &mut minutes, &mut events,
            d2["total"].as_i64().unwrap() as i32,
            "D2 repeat (4 L4 wolves + heart)",
            d2["minutes"].as_f64().unwrap(),
        );
    }
    let t2f = dungeon_xp(2, true);
    grant_xp(&need, &mut level, &mut xp_into, &mut minutes, &mut events,
        t2f["total"].as_i64().unwrap() as i32,
        "T2 first-clear (6 L6 wolves + Line-Mother + heart + first)",
        t2f["minutes"].as_f64().unwrap(),
    );
    while level < 10 {
        let t2 = dungeon_xp(2, false);
        grant_xp(&need, &mut level, &mut xp_into, &mut minutes, &mut events,
            t2["total"].as_i64().unwrap() as i32,
            "T2 repeat (6 L6 wolves + Line-Mother + heart)",
            t2["minutes"].as_f64().unwrap(),
        );
    }

    let d1_count = events.iter().filter(|e| e["label"].as_str().unwrap().starts_with("D1")).count();
    let d2_count = events.iter().filter(|e| e["label"].as_str().unwrap().starts_with("D2")).count();
    let t2_count = events.iter().filter(|e| e["label"].as_str().unwrap().starts_with("T2")).count();

    json!({
        "events": events,
        "final_level": level,
        "total_minutes": minutes,
        "in_90_150": (90.0..=150.0).contains(&minutes),
        "d1_first_xp": d1f["total"],
        "d1_repeat_xp": dungeon_xp(0, false)["total"],
        "d2_first_xp": d2f["total"],
        "d2_repeat_xp": dungeon_xp(1, false)["total"],
        "t2_first_xp": t2f["total"],
        "t2_repeat_xp": dungeon_xp(2, false)["total"],
        "clears": {"d1": d1_count, "d2": d2_count, "t2": t2_count},
    })
}

pub fn formulas() -> Value {
    let mut m = serde_json::Map::new();
    {
    let mut put = |k: &str, v: Value| {
        m.insert(k.to_string(), v);
    };
    put("tick_s", json!(TICK));
    put("hard_cap_s", json!(HARD_CAP_S));
    put("always_hit", json!(true));
    put("no_crit_v1", json!(true));
    put("seed_unused_v1", json!(true));
    put("mitigation", json!("floor(incoming * 100 / (100 + Grit_or_Armor))"));
    put("skill_rider", json!("1 + skill * 0.004"));
    put("skill_xp", json!("+1 per connecting damaging hit; cost = 8 * current_skill; cap = level * 5"));
    put("melee_hit", json!("floor((8 + Might*0.4) * skill_rider * strike_mult * martial_5_bonus)"));
    put("melee_swing_s", json!(MELEE_SWING_S));
    put("melee_reach_m", json!(MELEE_REACH_M));
    put("melee_cone_deg", json!(MELEE_CONE_DEG));
    put("bow_hit", json!("floor((7 + Swift*0.4) * skill_rider * aimed_mult * hunt_5_bonus * range_mult)"));
    put("bow_draw_s", json!(BOW_DRAW_S));
    put("bow_full_m", json!(BOW_FULL_M));
    put("bow_falloff_end_m", json!(BOW_FALLOFF_END_M));
    put("ember", json!("floor((14 + Will*0.5) * ember_rank * arcane_10_spell * skill_rider)"));
    put("ember_rank", json!("1.30 if Arcane>=10 else 1.15 if Arcane>=1 else 1.00"));
    put("arcane_10_spell", json!("1.15 if Arcane>=10 else 1.00"));
    put("mend", json!("floor((25 + Will*0.4) * arcane_10_spell)"));
    put("ward_absorb", json!("floor((20 + Will*0.3) * arcane_10_spell)"));
    put("bind", json!("root 4.0s; damage after 1.0s breaks; 0 damage"));
    put("ember_mana", json!(EMBER_MANA));
    put("ember_cast_s", json!(EMBER_CAST_S));
    put("ember_cd_s", json!(EMBER_CD_S));
    put("ember_range_m", json!(EMBER_RANGE_M));
    put("mend_mana", json!(MEND_MANA));
    put("mend_cast_s", json!(MEND_CAST_S));
    put("mend_cd_s", json!(MEND_CD_S));
    put("bind_mana", json!(BIND_MANA));
    put("bind_cast_s", json!(BIND_CAST_S));
    put("bind_cd_s", json!(BIND_CD_S));
    put("bind_range_m", json!(BIND_RANGE_M));
    put("ward_mana", json!(WARD_MANA));
    put("ward_gcd_s", json!(WARD_GCD_S));
    put("ward_dur_s", json!(WARD_DUR_S));
    put("ward_cd_s", json!(WARD_CD_S));
    put("strike", json!("next swing * 1.5, 6.0s CD, 0 mana"));
    put("bash", json!("0.4s anim, interrupt + 1.5s stun, 8.0s CD, 0 dmg"));
    put("aimed_shot", json!("2.0s draw, *1.8, 10s CD"));
    put("pin", json!("bow hit + 40% slow 4.0s, 12s CD"));
    put("cleave", json!("second target within 2.2m at 40% (Martial>=7)"));
    put("second_wind", json!("once/combat, 20% max HP (Martial>=10)"));
    put("mark", json!("+15% damage 12s (Hunt>=10)"));
    put("mana_regen_per_s", json!(MANA_REGEN_PER_S));
    put("mana_regen_combat_per_s", json!(MANA_REGEN_COMBAT_PER_S));
    put("mana_regen_ooc_per_s", json!(MANA_REGEN_OOC_PER_S));
    put("mana_regen_note", json!("sim is in-combat only; OOC regen 2.0/s specified for the bible but unused in these fights"));
    put("trash_speed_mps", json!(WOLF_SPEED));
    put("threat_damage", json!("damage * 1.0"));
    put("threat_heal", json!("heal * 0.5 applied to current lock"));
    put("first_hit_establishes_threat", json!(true));
    put("no_taunt_v1", json!(true));
    put("gcd", json!("spell cast time is the wait; Ward instant + 1.0s GCD"));
    put("auto_continues_while_casting", json!(true));
    put("interrupt", json!("any HP damage on caster interrupts current cast; Bash interrupts"));
    put("potion", json!(format!("Lesser Mend +{POTION_HEAL} HP, 60s CD, not a spell, no mana")));
    put("potion_heal", json!(POTION_HEAL));
    put("potion_at_create", json!(1));
    put("arrows_at_create", json!(START_ARROWS));
    put("hp_l1", json!(HP_L1));
    put("hp_per_level", json!(HP_PER_LEVEL));
    put("mana_l1", json!(MANA_L1));
    put("mana_per_level", json!(MANA_PER_LEVEL));
    put("attr_start", json!(ATTR_BASE));
    put("attr_points_per_level", json!(ATTR_PER_LEVEL));
    put("discipline_points_per_level", json!(DISC_PER_LEVEL));
    put("walk_mps", json!(WALK_MPS));
    put("sprint_mps", json!(SPRINT_MPS));
    put("aggro_sight_m", json!(SIGHT_AGGRO_M));
    put("aggro_hear_m", json!(HEAR_AGGRO_M));
    put("leash_m", json!(LEASH_M));
    put("social_m", json!(SOCIAL_M));
    put("wolf_hp", json!("70 + 18*(lvl-1)"));
    put("wolf_dmg", json!("10 + 2*(lvl-1)"));
    put("wolf_xp", json!("35 + 12*(lvl-1)"));
    put("dungeon_wolf_counts", json!({"0": 2, "1": 4, "2": 6}));
    put("death", json!("last shrine + 5 min Shaken (-10% dmg); no corpse; no XP debt"));
    let mut spend = serde_json::Map::new();
    spend.insert("Martial".into(), json!("all attr -> Might; all discipline -> Martial"));
    spend.insert("Hunt".into(), json!("all attr -> Swift; all discipline -> Hunt"));
    spend.insert("Arcane".into(), json!("all attr -> Will; all discipline -> Arcane"));
    put("specialist_spend", Value::Object(spend));
    }
    Value::Object(m)
}

pub fn mob_sheets_json() -> Value {
    json!({
        "crawler_spider_wolf_l1": wolf_sheet(1),
        "crawler_spider_wolf_l4": wolf_sheet(4),
        "crawler_spider_wolf_l6": wolf_sheet(6),
        "line_mother": mother_sheet(),
        "crawler_scorpion": scorpion_sheet(),
        "green_blob": blob_sheet(),
        "orc": orc_sheet(),
        "tribal": tribal_sheet(),
        "orc_skull": orc_skull_sheet(),
        "skeleton_warrior": skeleton_warrior_sheet(),
        "skeleton_minion": skeleton_minion_sheet(),
        "skeleton_mage": skeleton_mage_sheet(),
        "yeti": yeti_sheet(),
        "demon": demon_sheet(),
        "blue_demon": blue_demon_sheet(),
        "bandit": bandit_sheet(),
    })
}

pub fn player_specialists_json() -> Value {
    let mut map = serde_json::Map::new();
    for lv in [1, 2, 3, 5, 9, 10] {
        for d in [Discipline::Martial, Discipline::Hunt, Discipline::Arcane] {
            let key = format!("L{lv}_{}", d.as_str());
            map.insert(key, serde_json::to_value(player_stats(lv, d)).unwrap());
        }
    }
    Value::Object(map)
}

