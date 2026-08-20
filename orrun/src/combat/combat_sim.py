#!/usr/bin/env python3
"""Orrun v1 headless numeric combat simulator.

All TTK/TTD in combat-bible.md must come from this file. Deterministic.
`seed` is accepted and written to JSON but unused in v1 math (reserved).

Tick = 0.1s. Hard cap 180s.
"""
from __future__ import annotations

import argparse
import json
import math
import sys
from copy import deepcopy
from typing import Any, Dict, List, Optional, Tuple

TICK = 0.1
HARD_CAP_S = 180.0

# ---------------------------------------------------------------------------
# Locked feel / math (change a NUMBER if a band fails, not a rule)
# ---------------------------------------------------------------------------
WALK_MPS = 4.5
SPRINT_MPS = 7.0
MELEE_SWING_S = 1.8
MELEE_REACH_M = 2.8
MELEE_CONE_DEG = 120.0
BOW_DRAW_S = 1.2
BOW_FULL_M = 20.0
BOW_FALLOFF_END_M = 35.0
TAB_LOCK_M = 20.0
SIGHT_AGGRO_M = 12.0
HEAR_AGGRO_M = 6.0
LEASH_M = 40.0
SOCIAL_M = 15.0

HP_L1 = 100
HP_PER_LEVEL = 12
MANA_L1 = 50
MANA_PER_LEVEL = 6
ATTR_BASE = 10
ATTR_PER_LEVEL = 2  # points to spend
DISC_PER_LEVEL = 1
SKILL_RIDER_PER = 0.004  # +20% at 50
SKILL_UP_COST_MULT = 8
SKILL_CAP_PER_LEVEL = 5

MELEE_BASE = 8.0
MELEE_MIGHT = 0.4
BOW_BASE = 7.0
BOW_SWIFT = 0.4

# Ember — first-slice 12/1.0s, then 6/1.2s. Cost 3 / 1.2s after combat
# regen locked at 0.4/s (20x6 cannot finish Pale Hall inside 50s).
EMBER_BASE = 14.0
EMBER_WILL = 0.5
EMBER_MANA = 3  # was 6; 0.4 combat regen cannot fund 20x6 in a 50s room
EMBER_CAST_S = 1.2
EMBER_CD_S = 0.0
EMBER_RANGE_M = 28.0
EMBER_RANK1 = 1.15
EMBER_RANK10 = 1.30

MEND_BASE = 25.0
MEND_WILL = 0.4
MEND_MANA = 20
MEND_CAST_S = 2.5
MEND_CD_S = 0.0
MEND_RANGE_M = 28.0  # self ok

BIND_MANA = 16
BIND_CAST_S = 1.5
BIND_CD_S = 12.0
BIND_RANGE_M = 24.0
BIND_ROOT_S = 4.0
BIND_GRACE_S = 1.0  # damage after 1s breaks

WARD_BASE = 20.0
WARD_WILL = 0.3
WARD_MANA = 18
WARD_GCD_S = 1.0
WARD_DUR_S = 8.0
WARD_CD_S = 16.0

STRIKE_MULT = 1.5
STRIKE_CD_S = 6.0
BASH_ANIM_S = 0.4
BASH_STUN_S = 1.5
BASH_CD_S = 8.0
AIMED_DRAW_S = 2.0
AIMED_MULT = 1.8
AIMED_CD_S = 10.0
PIN_SLOW_PCT = 0.40
PIN_DUR_S = 4.0
PIN_CD_S = 12.0
CLEAVE_PCT = 0.40
CLEAVE_RANGE_M = 2.2
MARK_MULT = 1.15
MARK_DUR_S = 12.0
SECOND_WIND_PCT = 0.20
POTION_HEAL = 40  # was 30; trash 5.2 stacks the 2-pull so both wolves connect
POTION_CD_S = 60.0
START_ARROWS = 40
START_POTIONS = 1
MANA_REGEN_COMBAT_PER_S = 0.4  # locked start; 0.0 if Pale Hall never OOMs
MANA_REGEN_OOC_PER_S = 2.0  # bible spec; sim fights are in-combat only (unused here)
MANA_REGEN_PER_S = MANA_REGEN_COMBAT_PER_S
THREAT_DMG = 1.0
THREAT_HEAL = 0.5
SHAKEN_DMG = 0.90
SHAKEN_S = 300.0
RANK5_WEAPON = 1.10
RANK10_SPELL = 1.15

# Wolf-spider scale (one block, three concrete rows)
WOLF_HP_BASE = 70
WOLF_HP_PER = 18
WOLF_DMG_BASE = 10
WOLF_DMG_PER = 2
WOLF_SWING_S = 2.0
WOLF_REACH_M = 1.8
WOLF_SPEED = 5.2  # locked: faster than walk 4.5; sprint 7.0 or Pin/Bind to take 0
WOLF_ARMOR = 8
WOLF_XP_BASE = 35
WOLF_XP_PER = 12

# Line-Mother first try HP/armor/swing/xp kept; dmg 18 one-shot the
# L9 tank window (player died before 420 HP). Dropped auto to 12 so
# heart ends 20–50% HP. Slam 24 (= 2× auto). Speed/telegraph locked.
MOTHER_LEVEL = 9
MOTHER_HP = 420
MOTHER_DMG = 12
MOTHER_SWING_S = 2.2
MOTHER_SLAM = 24
MOTHER_SLAM_EVERY_S = 8.0
MOTHER_TELE_S = 1.2
MOTHER_ARMOR = 14
MOTHER_XP = 280
MOTHER_TOKEN = 1
MOTHER_SPEED = 3.2
MOTHER_REACH_M = 2.2
MOTHER_ENRAGE_HP = 0.30
MOTHER_ENRAGE_DMG = 1.30
MOTHER_ENRAGE_SWING = 0.77

# Scorpion = L3 even trash, wolf-shaped + poison
SCORP_LEVEL = 3
SCORP_POISON_DPS = 3.0
SCORP_POISON_S = 4.0

# GreenBlob — L2 even trash, slow, no poison, no token
BLOB_LEVEL = 2
BLOB_HP = 82
BLOB_DMG = 11
BLOB_SWING_S = 2.4
BLOB_ARMOR = 6
BLOB_SPEED = 5.2  # trash lock; melee 1v1 starts in range so TTK unchanged
BLOB_REACH_M = 1.6
BLOB_XP = 47

# Orc — L4 grunt (Quaternius big). No special.
ORC_LEVEL = 4
ORC_HP = 130
ORC_DMG = 15
ORC_SWING_S = 2.0
ORC_ARMOR = 10
ORC_SPEED = 3.5
ORC_REACH_M = 2.0
ORC_XP = 80

# Yeti — L6 brute, weaker Line-Mother slam
YETI_LEVEL = 6
YETI_HP = 240
YETI_DMG = 14
YETI_SWING_S = 2.3
YETI_SLAM = 26
YETI_SLAM_EVERY_S = 8.0
YETI_TELE_S = 1.0
YETI_ARMOR = 12
YETI_SPEED = 3.0
YETI_REACH_M = 2.2
YETI_XP = 160

# Demon — L8 melee, Punch only. Shout tell HOLD (no clip).
DEMON_LEVEL = 8
DEMON_HP = 220
DEMON_ARMOR = 12
DEMON_DMG = 16
DEMON_SWING_S = 2.0
DEMON_REACH_M = 2.2
DEMON_SPEED = 3.2
DEMON_XP = 180

# XP to go from N → N+1 (N=1..9). Tuned so the locked dungeon path
# D1→L4, D2→L8, T2/Mother→L10 lands in 90–150 minutes.
XP_TO_NEXT = {
    1: 200,
    2: 250,
    3: 300,
    4: 550,
    5: 700,
    6: 850,
    7: 1000,
    8: 1200,
    9: 1500,
}
HEART_BONUS = {0: 40, 1: 70, 2: 120}
FIRST_CLEAR = {0: 80, 1: 140, 2: 200}
MIN_PER_DUNGEON = {0: 4, 1: 7, 2: 12}

DUNGEON_WOLVES = {0: 2, 1: 4, 2: 6}  # LOCKED
DUNGEON_WOLF_LEVEL = {0: 1, 1: 4, 2: 6}


def trunc(x: float) -> int:
    return int(x)  # toward zero for the non-negative combat path


def mitigation(incoming: float, grit_or_armor: int) -> int:
    if incoming <= 0:
        return 0
    return trunc(incoming * 100.0 / (100.0 + grit_or_armor))


def skill_rider(skill: int) -> float:
    return 1.0 + skill * SKILL_RIDER_PER


def bow_range_mult(distance: float) -> float:
    if distance <= BOW_FULL_M:
        return 1.0
    if distance >= BOW_FALLOFF_END_M:
        return 0.0
    return (BOW_FALLOFF_END_M - distance) / (BOW_FALLOFF_END_M - BOW_FULL_M)


def wolf_sheet(level: int) -> Dict[str, Any]:
    return {
        "id": "crawler_spider_wolf",
        "name": "wolf-spider",
        "level": level,
        "hp": WOLF_HP_BASE + WOLF_HP_PER * (level - 1),
        "armor": WOLF_ARMOR,
        "damage": WOLF_DMG_BASE + WOLF_DMG_PER * (level - 1),
        "swing_s": WOLF_SWING_S,
        "reach_m": WOLF_REACH_M,
        "speed_mps": WOLF_SPEED,
        "sight_m": SIGHT_AGGRO_M,
        "hear_m": HEAR_AGGRO_M,
        "leash_m": LEASH_M,
        "social_m": SOCIAL_M,
        "xp": WOLF_XP_BASE + WOLF_XP_PER * (level - 1),
        "token_brood": 1,
        "specials": [],
        "scale_hp": "70 + 18*(lvl-1)",
        "scale_dmg": "10 + 2*(lvl-1)",
        "scale_xp": "35 + 12*(lvl-1)",
    }


def mother_sheet() -> Dict[str, Any]:
    return {
        "id": "line_mother",
        "name": "Line-Mother",
        "level": MOTHER_LEVEL,
        "hp": MOTHER_HP,
        "armor": MOTHER_ARMOR,
        "damage": MOTHER_DMG,
        "swing_s": MOTHER_SWING_S,
        "slam_damage": MOTHER_SLAM,
        "slam_every_s": MOTHER_SLAM_EVERY_S,
        "telegraph_s": MOTHER_TELE_S,
        "reach_m": MOTHER_REACH_M,
        "speed_mps": MOTHER_SPEED,
        "sight_m": SIGHT_AGGRO_M,
        "hear_m": HEAR_AGGRO_M,
        "leash_m": LEASH_M,
        "social_m": SOCIAL_M,
        "xp": MOTHER_XP,
        "token_brood": MOTHER_TOKEN,
        "specials": [
            "slam_every_8s_1.2s_telegraph_interruptible",
            "enrage_30pct_dmg*1.3_swing*0.77",
        ],
    }


def scorpion_sheet() -> Dict[str, Any]:
    w = wolf_sheet(SCORP_LEVEL)
    return {
        "id": "crawler_scorpion",
        "name": "scorpion",
        "level": SCORP_LEVEL,
        "hp": w["hp"],
        "armor": w["armor"],
        "damage": w["damage"],
        "swing_s": w["swing_s"],
        "reach_m": w["reach_m"],
        "speed_mps": w["speed_mps"],
        "sight_m": SIGHT_AGGRO_M,
        "hear_m": HEAR_AGGRO_M,
        "leash_m": LEASH_M,
        "social_m": SOCIAL_M,
        "xp": w["xp"],
        "token_brood": 1,
        "specials": ["poison_3hps_4s_refresh_not_stack"],
    }


def blob_sheet() -> Dict[str, Any]:
    return {
        "id": "green_blob",
        "name": "GreenBlob",
        "level": BLOB_LEVEL,
        "hp": BLOB_HP,
        "armor": BLOB_ARMOR,
        "damage": BLOB_DMG,
        "swing_s": BLOB_SWING_S,
        "reach_m": BLOB_REACH_M,
        "speed_mps": BLOB_SPEED,
        "sight_m": SIGHT_AGGRO_M,
        "hear_m": HEAR_AGGRO_M,
        "leash_m": LEASH_M,
        "social_m": SOCIAL_M,
        "xp": BLOB_XP,
        "token_brood": 0,
        "specials": ["slow", "no_poison"],
    }


def orc_sheet() -> Dict[str, Any]:
    return {
        "id": "orc",
        "name": "orc",
        "level": ORC_LEVEL,
        "hp": ORC_HP,
        "armor": ORC_ARMOR,
        "damage": ORC_DMG,
        "swing_s": ORC_SWING_S,
        "reach_m": ORC_REACH_M,
        "speed_mps": ORC_SPEED,
        "sight_m": SIGHT_AGGRO_M,
        "hear_m": HEAR_AGGRO_M,
        "leash_m": LEASH_M,
        "social_m": SOCIAL_M,
        "xp": ORC_XP,
        "token_brood": 0,
        "specials": [],
    }


def demon_sheet() -> Dict[str, Any]:
    return {
        "id": "demon",
        "name": "demon",
        "level": DEMON_LEVEL,
        "hp": DEMON_HP,
        "armor": DEMON_ARMOR,
        "damage": DEMON_DMG,
        "swing_s": DEMON_SWING_S,
        "reach_m": DEMON_REACH_M,
        "speed_mps": DEMON_SPEED,
        "sight_m": SIGHT_AGGRO_M,
        "hear_m": HEAR_AGGRO_M,
        "leash_m": LEASH_M,
        "social_m": SOCIAL_M,
        "xp": DEMON_XP,
        "token_brood": 0,
        "specials": [],
    }


def yeti_sheet() -> Dict[str, Any]:
    return {
        "id": "yeti",
        "name": "yeti",
        "level": YETI_LEVEL,
        "hp": YETI_HP,
        "armor": YETI_ARMOR,
        "damage": YETI_DMG,
        "swing_s": YETI_SWING_S,
        "slam_damage": YETI_SLAM,
        "slam_every_s": YETI_SLAM_EVERY_S,
        "telegraph_s": YETI_TELE_S,
        "reach_m": YETI_REACH_M,
        "speed_mps": YETI_SPEED,
        "sight_m": SIGHT_AGGRO_M,
        "hear_m": HEAR_AGGRO_M,
        "leash_m": LEASH_M,
        "social_m": SOCIAL_M,
        "xp": YETI_XP,
        "token_brood": 0,
        "specials": ["slam_every_8s_1.0s_telegraph_interruptible"],
    }


MOB_BUILDERS = {
    "crawler_spider_wolf": lambda lvl=1: wolf_sheet(lvl),
    "crawler_scorpion": lambda lvl=None: scorpion_sheet(),
    "line_mother": lambda lvl=None: mother_sheet(),
    "green_blob": lambda lvl=None: blob_sheet(),
    "orc": lambda lvl=None: orc_sheet(),
    "yeti": lambda lvl=None: yeti_sheet(),
    "demon": lambda lvl=None: demon_sheet(),
}

MOB_ALIASES = {
    "wolf": "crawler_spider_wolf",
    "wolf-spider": "crawler_spider_wolf",
    "wolf_spider": "crawler_spider_wolf",
    "scorpion": "crawler_scorpion",
    "line-mother": "line_mother",
    "Line-Mother": "line_mother",
    "GreenBlob": "green_blob",
    "greenblob": "green_blob",
}


def resolve_mob_id(name: str) -> str:
    if name in MOB_BUILDERS:
        return name
    if name in MOB_ALIASES:
        return MOB_ALIASES[name]
    raise SystemExit(f"unknown mob: {name}")


def formulas() -> Dict[str, Any]:
    return {
        "tick_s": TICK,
        "hard_cap_s": HARD_CAP_S,
        "always_hit": True,
        "no_crit_v1": True,
        "seed_unused_v1": True,
        "mitigation": "floor(incoming * 100 / (100 + Grit_or_Armor))",
        "skill_rider": "1 + skill * 0.004",
        "skill_xp": "+1 per connecting damaging hit; cost = 8 * current_skill; cap = level * 5",
        "melee_hit": "floor((8 + Might*0.4) * skill_rider * strike_mult * martial_5_bonus)",
        "melee_swing_s": MELEE_SWING_S,
        "melee_reach_m": MELEE_REACH_M,
        "melee_cone_deg": MELEE_CONE_DEG,
        "bow_hit": "floor((7 + Swift*0.4) * skill_rider * aimed_mult * hunt_5_bonus * range_mult)",
        "bow_draw_s": BOW_DRAW_S,
        "bow_full_m": BOW_FULL_M,
        "bow_falloff_end_m": BOW_FALLOFF_END_M,
        "ember": "floor((14 + Will*0.5) * ember_rank * arcane_10_spell * skill_rider)",
        "ember_rank": "1.30 if Arcane>=10 else 1.15 if Arcane>=1 else 1.00",
        "arcane_10_spell": "1.15 if Arcane>=10 else 1.00",
        "mend": "floor((25 + Will*0.4) * arcane_10_spell)",
        "ward_absorb": "floor((20 + Will*0.3) * arcane_10_spell)",
        "bind": "root 4.0s; damage after 1.0s breaks; 0 damage",
        "ember_mana": EMBER_MANA,
        "ember_cast_s": EMBER_CAST_S,
        "ember_cd_s": EMBER_CD_S,
        "ember_range_m": EMBER_RANGE_M,
        "mend_mana": MEND_MANA,
        "mend_cast_s": MEND_CAST_S,
        "mend_cd_s": MEND_CD_S,
        "bind_mana": BIND_MANA,
        "bind_cast_s": BIND_CAST_S,
        "bind_cd_s": BIND_CD_S,
        "bind_range_m": BIND_RANGE_M,
        "ward_mana": WARD_MANA,
        "ward_gcd_s": WARD_GCD_S,
        "ward_dur_s": WARD_DUR_S,
        "ward_cd_s": WARD_CD_S,
        "strike": "next swing * 1.5, 6.0s CD, 0 mana",
        "bash": "0.4s anim, interrupt + 1.5s stun, 8.0s CD, 0 dmg",
        "aimed_shot": "2.0s draw, *1.8, 10s CD",
        "pin": "bow hit + 40% slow 4.0s, 12s CD",
        "cleave": "second target within 2.2m at 40% (Martial>=7)",
        "second_wind": "once/combat, 20% max HP (Martial>=10)",
        "mark": "+15% damage 12s (Hunt>=10)",
        "mana_regen_per_s": MANA_REGEN_PER_S,
        "mana_regen_combat_per_s": MANA_REGEN_COMBAT_PER_S,
        "mana_regen_ooc_per_s": MANA_REGEN_OOC_PER_S,
        "mana_regen_note": "sim is in-combat only; OOC regen 2.0/s specified for the bible but unused in these fights",
        "trash_speed_mps": WOLF_SPEED,
        "threat_damage": "damage * 1.0",
        "threat_heal": "heal * 0.5 applied to current lock",
        "first_hit_establishes_threat": True,
        "no_taunt_v1": True,
        "gcd": "spell cast time is the wait; Ward instant + 1.0s GCD",
        "auto_continues_while_casting": True,
        "interrupt": "any HP damage on caster interrupts current cast; Bash interrupts",
        "potion": f"Lesser Mend +{POTION_HEAL} HP, 60s CD, not a spell, no mana",
        "potion_heal": POTION_HEAL,
        "potion_at_create": 1,
        "arrows_at_create": START_ARROWS,
        "hp_l1": HP_L1,
        "hp_per_level": HP_PER_LEVEL,
        "mana_l1": MANA_L1,
        "mana_per_level": MANA_PER_LEVEL,
        "attr_start": ATTR_BASE,
        "attr_points_per_level": ATTR_PER_LEVEL,
        "discipline_points_per_level": DISC_PER_LEVEL,
        "walk_mps": WALK_MPS,
        "sprint_mps": SPRINT_MPS,
        "aggro_sight_m": SIGHT_AGGRO_M,
        "aggro_hear_m": HEAR_AGGRO_M,
        "leash_m": LEASH_M,
        "social_m": SOCIAL_M,
        "wolf_hp": "70 + 18*(lvl-1)",
        "wolf_dmg": "10 + 2*(lvl-1)",
        "wolf_xp": "35 + 12*(lvl-1)",
        "dungeon_wolf_counts": DUNGEON_WOLVES,
        "death": "last shrine + 5 min Shaken (-10% dmg); no corpse; no XP debt",
        "specialist_spend": {
            "Martial": "all attr → Might; all discipline → Martial",
            "Hunt": "all attr → Swift; all discipline → Hunt",
            "Arcane": "all attr → Will; all discipline → Arcane",
        },
    }


def player_stats(level: int, discipline: str) -> Dict[str, Any]:
    discipline = discipline.capitalize()
    if discipline not in ("Martial", "Hunt", "Arcane"):
        raise SystemExit(f"unknown discipline: {discipline}")
    gained = level - 1
    attrs = {"Might": ATTR_BASE, "Swift": ATTR_BASE, "Will": ATTR_BASE, "Grit": ATTR_BASE}
    points = gained * ATTR_PER_LEVEL
    if discipline == "Martial":
        attrs["Might"] += points
        ranks = {"Martial": level, "Hunt": 0, "Arcane": 0}
    elif discipline == "Hunt":
        attrs["Swift"] += points
        ranks = {"Martial": 0, "Hunt": level, "Arcane": 0}
    else:
        attrs["Will"] += points
        ranks = {"Martial": 0, "Hunt": 0, "Arcane": level}
    skill_cap = level * SKILL_CAP_PER_LEVEL
    return {
        "level": level,
        "discipline": discipline,
        "hp": HP_L1 + HP_PER_LEVEL * gained,
        "mana": MANA_L1 + MANA_PER_LEVEL * gained,
        "attrs": attrs,
        "ranks": ranks,
        "weapon_skill": skill_cap,
        "skill_cap": skill_cap,
        "skill_xp": 0,
    }


def melee_raw(p: Dict[str, Any], strike: bool = False) -> int:
    might = p["attrs"]["Might"]
    rider = skill_rider(p["weapon_skill"])
    strike_m = STRIKE_MULT if strike else 1.0
    m5 = RANK5_WEAPON if p["ranks"]["Martial"] >= 5 else 1.0
    return trunc((MELEE_BASE + might * MELEE_MIGHT) * rider * strike_m * m5)


def bow_raw(p: Dict[str, Any], aimed: bool = False, distance: float = 18.0) -> int:
    swift = p["attrs"]["Swift"]
    rider = skill_rider(p["weapon_skill"])
    aimed_m = AIMED_MULT if aimed else 1.0
    h5 = RANK5_WEAPON if p["ranks"]["Hunt"] >= 5 else 1.0
    return trunc((BOW_BASE + swift * BOW_SWIFT) * rider * aimed_m * h5 * bow_range_mult(distance))


def ember_rank_mult(arcane: int) -> float:
    if arcane >= 10:
        return EMBER_RANK10
    if arcane >= 1:
        return EMBER_RANK1
    return 1.0


def spell_rank_mult(arcane: int) -> float:
    return RANK10_SPELL if arcane >= 10 else 1.0


def ember_raw(p: Dict[str, Any]) -> int:
    will = p["attrs"]["Will"]
    arc = p["ranks"]["Arcane"]
    rider = skill_rider(p["weapon_skill"])
    return trunc((EMBER_BASE + will * EMBER_WILL) * ember_rank_mult(arc) * spell_rank_mult(arc) * rider)


def mend_raw(p: Dict[str, Any]) -> int:
    will = p["attrs"]["Will"]
    return trunc((MEND_BASE + will * MEND_WILL) * spell_rank_mult(p["ranks"]["Arcane"]))


def ward_raw(p: Dict[str, Any]) -> int:
    will = p["attrs"]["Will"]
    return trunc((WARD_BASE + will * WARD_WILL) * spell_rank_mult(p["ranks"]["Arcane"]))


# ---------------------------------------------------------------------------
# Fight
# ---------------------------------------------------------------------------
class Mob:
    def __init__(self, sheet: Dict[str, Any], x: float, idx: int):
        self.sheet = sheet
        self.id = sheet["id"]
        self.idx = idx
        self.x = x
        self.hp = float(sheet["hp"])
        self.max_hp = float(sheet["hp"])
        self.armor = int(sheet["armor"])
        self.base_dmg = float(sheet["damage"])
        self.base_swing = float(sheet["swing_s"])
        self.swing = self.base_swing
        self.reach = float(sheet["reach_m"])
        self.speed = float(sheet["speed_mps"])
        self.auto_cd = self.swing  # first swing at end of first period
        self.stun = 0.0
        self.root = 0.0
        self.root_grace = 0.0
        self.slow = 0.0
        self.tele = 0.0
        self.slam_cd = float(sheet.get("slam_every_s") or 0.0)
        self.slam_dmg = float(sheet.get("slam_damage") or 0.0)
        self.tele_len = float(sheet.get("telegraph_s") or 0.0)
        self.has_slam = self.slam_cd > 0 and self.slam_dmg > 0
        self.enraged = False
        self.alive = True
        self.threat = 0.0
        self.dmg_taken = 0

    def dmg_mult(self) -> float:
        return MOTHER_ENRAGE_DMG if self.enraged else 1.0


class Player:
    def __init__(self, stats: Dict[str, Any], potions: int, arrows: int):
        self.stats = stats
        self.x = 0.0
        self.hp = float(stats["hp"])
        self.max_hp = float(stats["hp"])
        self.mana = float(stats["mana"])
        self.max_mana = float(stats["mana"])
        self.potions = potions
        self.arrows = arrows
        self.alive = True
        self.auto_cd = 0.0  # martial starts swing immediately (lands at 1.8)
        self.busy = 0.0  # remaining cast/draw/bash anim (does not stop auto)
        self.cast: Optional[Dict[str, Any]] = None
        self.gcd = 0.0
        self.cds: Dict[str, float] = {
            "strike": 0.0,
            "bash": 0.0,
            "aimed": 0.0,
            "pin": 0.0,
            "ember": 0.0,
            "mend": 0.0,
            "bind": 0.0,
            "ward": 0.0,
            "potion": 0.0,
            "mark": 0.0,
        }
        self.strike_armed = False
        self.ward = 0.0
        self.ward_t = 0.0
        self.mark_t = 0.0
        self.second_wind_used = False
        self.poison_t = 0.0
        self.poison_acc = 0.0
        self.lock: Optional[int] = None
        self.mana_spent = 0
        self.swings = 0
        self.spells: Dict[str, int] = {}
        self.max_hit_taken = 0
        self.shaken = False
        self.sprinted = False
        self.used_pin_or_bind = False
        self.damage_taken = 0
        self.mana_hit_zero = False
        self.skipped_spell_for_mana = False
        self.min_mana = float(stats["mana"])

    def rank(self, d: str) -> int:
        return self.stats["ranks"][d]

    def bump_skill(self) -> None:
        st = self.stats
        if st["weapon_skill"] >= st["skill_cap"]:
            return
        st["skill_xp"] += 1
        cost = SKILL_UP_COST_MULT * st["weapon_skill"]
        if st["skill_xp"] >= cost:
            st["weapon_skill"] += 1
            st["skill_xp"] = 0


def dist(p: Player, m: Mob) -> float:
    return abs(m.x - p.x)


def living(mobs: List[Mob]) -> List[Mob]:
    return [m for m in mobs if m.alive]


def apply_player_damage(p: Player, amount: int, interrupt: bool = True) -> int:
    """Apply incoming (already mitigated) damage through Ward. Returns HP lost."""
    if amount <= 0 or not p.alive:
        return 0
    p.max_hit_taken = max(p.max_hit_taken, amount)
    left = amount
    if p.ward > 0:
        absorb = min(p.ward, left)
        p.ward -= absorb
        left -= absorb
        if p.ward <= 0:
            p.ward = 0.0
            p.ward_t = 0.0
    if left <= 0:
        return 0
    p.hp -= left
    p.damage_taken += int(left)
    if interrupt and p.cast is not None:
        p.cast = None
        p.busy = 0.0
    if p.hp <= 0:
        p.hp = 0.0
        p.alive = False
    return int(left)


def hit_mob(p: Player, m: Mob, raw: int, threat_mult: float = THREAT_DMG) -> int:
    if p.shaken:
        raw = trunc(raw * SHAKEN_DMG)
    if p.mark_t > 0:
        raw = trunc(raw * MARK_MULT)
    dealt = mitigation(raw, m.armor)
    m.hp -= dealt
    m.dmg_taken += dealt
    m.threat += dealt * threat_mult
    if m.root > 0 and m.root_grace <= 0:
        m.root = 0.0
    if m.hp <= 0:
        m.hp = 0.0
        m.alive = False
    p.bump_skill()
    return dealt


def start_cast(p: Player, name: str, duration: float, mana: int, extra: Dict[str, Any]) -> bool:
    if p.busy > 0 or p.gcd > 0:
        return False
    if mana > 0 and p.mana < mana:
        return False
    if mana > 0:
        p.mana -= mana
        p.mana_spent += mana
        if p.mana <= 1e-9:
            p.mana = 0.0
            p.mana_hit_zero = True
    if name in ("Pin", "Bind") or extra.get("kind") in ("pin", "bind"):
        p.used_pin_or_bind = True
    p.cast = {"name": name, **extra}
    p.busy = duration
    p.spells[name] = p.spells.get(name, 0) + 1
    return True


def player_ai_start(p: Player, mobs: List[Mob], kite: bool) -> None:
    if not p.alive or p.busy > 0 or p.gcd > 0:
        return
    disc = p.stats["discipline"]
    live = living(mobs)
    if not live:
        return

    # potion
    pot_line = 0.30 if disc == "Arcane" else 0.35
    if p.potions > 0 and p.cds["potion"] <= 0 and p.hp < pot_line * p.max_hp:
        p.hp = min(p.max_hp, p.hp + POTION_HEAL)
        p.potions -= 1
        p.cds["potion"] = POTION_CD_S
        p.spells["potion"] = p.spells.get("potion", 0) + 1
        return

    # Second Wind
    if disc == "Martial" and p.rank("Martial") >= 10 and not p.second_wind_used:
        if p.hp < 0.25 * p.max_hp:
            p.hp = min(p.max_hp, p.hp + SECOND_WIND_PCT * p.max_hp)
            p.second_wind_used = True
            p.spells["SecondWind"] = p.spells.get("SecondWind", 0) + 1
            return

    if disc == "Martial":
        # Bash every slam telegraph
        if p.rank("Martial") >= 3 and p.cds["bash"] <= 0:
            slamming = [m for m in live if m.tele > 0]
            if slamming:
                start_cast(p, "Bash", BASH_ANIM_S, 0, {"kind": "bash", "target": slamming[0].idx})
                p.cds["bash"] = BASH_CD_S
                return
        # Strike on CD (arm next swing)
        if p.rank("Martial") >= 1 and p.cds["strike"] <= 0 and not p.strike_armed:
            p.strike_armed = True
            p.cds["strike"] = STRIKE_CD_S
            p.spells["Strike"] = p.spells.get("Strike", 0) + 1
        return

    if disc == "Hunt":
        if p.arrows <= 0:
            return
        if p.rank("Hunt") >= 10 and p.cds["mark"] <= 0:
            p.mark_t = MARK_DUR_S
            p.cds["mark"] = 20.0
            p.spells["Mark"] = p.spells.get("Mark", 0) + 1
        if p.rank("Hunt") >= 1 and p.cds["aimed"] <= 0:
            tgt = lowest_hp(live).idx
            p.lock = tgt
            start_cast(p, "AimedShot", AIMED_DRAW_S, 0, {"kind": "aimed", "target": tgt})
            p.cds["aimed"] = AIMED_CD_S
            return
        if p.rank("Hunt") >= 3 and p.cds["pin"] <= 0:
            tgt = lowest_hp(live).idx
            p.lock = tgt
            start_cast(p, "Pin", BOW_DRAW_S, 0, {"kind": "pin", "target": tgt})
            p.cds["pin"] = PIN_CD_S
            return
        tgt = lowest_hp(live).idx
        p.lock = tgt
        start_cast(p, "Fire", BOW_DRAW_S, 0, {"kind": "fire", "target": tgt})
        return

    # Arcane
    if p.rank("Arcane") >= 3 and p.hp < 0.50 * p.max_hp and p.cds["mend"] <= 0:
        if p.mana >= MEND_MANA:
            start_cast(p, "Mend", MEND_CAST_S, MEND_MANA, {"kind": "mend"})
            p.cds["mend"] = MEND_CD_S
            return
        p.skipped_spell_for_mana = True
    # Ward if known and (incoming slam or HP<60%)
    if p.rank("Arcane") >= 7 and p.cds["ward"] <= 0:
        slamming = any(m.tele > 0 for m in live)
        if slamming or p.hp < 0.60 * p.max_hp:
            if p.mana >= WARD_MANA:
                start_cast(p, "Ward", 0.0, WARD_MANA, {"kind": "ward"})
                p.cds["ward"] = WARD_CD_S
                p.gcd = WARD_GCD_S
                p.ward = float(ward_raw(p.stats))
                p.ward_t = WARD_DUR_S
                p.cast = None
                p.busy = 0.0
                return
            p.skipped_spell_for_mana = True
    # Ember the lowest-HP target IN RANGE. Bind can root an extra; the
    # kite then leaves that extra behind (~30 m). Casting Ember at the
    # stranded one wastes mana (range 28 m). Kill the chasing pack first.
    in_ember = [m for m in live if dist(p, m) <= EMBER_RANGE_M]
    focus_pool = in_ember if in_ember else live
    focus = lowest_hp(focus_pool)
    # Bind on a second if Arcane>=5 (only if that add is in Bind range)
    if p.rank("Arcane") >= 5 and p.cds["bind"] <= 0:
        others = [
            m for m in live
            if m.idx != focus.idx and m.root <= 0 and dist(p, m) <= BIND_RANGE_M
        ]
        if others:
            if p.mana >= BIND_MANA:
                tgt = others[0]
                p.lock = tgt.idx
                start_cast(p, "Bind", BIND_CAST_S, BIND_MANA, {"kind": "bind", "target": tgt.idx})
                p.cds["bind"] = BIND_CD_S
                return
            p.skipped_spell_for_mana = True
    if in_ember and p.cds["ember"] <= 0:
        if p.mana >= EMBER_MANA:
            p.lock = focus.idx
            start_cast(p, "Ember", EMBER_CAST_S, EMBER_MANA, {"kind": "ember", "target": focus.idx})
            p.cds["ember"] = EMBER_CD_S
            return
        p.skipped_spell_for_mana = True


def lowest_hp(mobs: List[Mob]) -> Mob:
    return min(mobs, key=lambda m: (m.hp, m.idx))


def finish_cast(p: Player, mobs: List[Mob]) -> None:
    if p.cast is None:
        return
    kind = p.cast.get("kind")
    tgt_idx = p.cast.get("target")
    tgt = None
    if tgt_idx is not None:
        for m in mobs:
            if m.idx == tgt_idx:
                tgt = m
                break
    if kind == "bash":
        if tgt and tgt.alive:
            tgt.tele = 0.0  # interrupt slam
            tgt.stun = max(tgt.stun, BASH_STUN_S)
        p.cast = None
        return
    if kind == "mend":
        heal = mend_raw(p.stats)
        p.hp = min(p.max_hp, p.hp + heal)
        if p.lock is not None:
            for m in mobs:
                if m.idx == p.lock and m.alive:
                    m.threat += heal * THREAT_HEAL
        p.cast = None
        return
    if kind == "bind":
        if tgt and tgt.alive and dist(p, tgt) <= BIND_RANGE_M:
            tgt.root = BIND_ROOT_S
            tgt.root_grace = BIND_GRACE_S
        p.cast = None
        return
    if kind in ("aimed", "pin", "fire"):
        if p.arrows <= 0:
            p.cast = None
            return
        p.arrows -= 1
        if tgt and tgt.alive:
            d = dist(p, tgt)
            raw = bow_raw(p.stats, aimed=(kind == "aimed"), distance=d)
            if raw > 0:
                hit_mob(p, tgt, raw)
                p.swings += 1
            if kind == "pin" and tgt.alive:
                tgt.slow = PIN_DUR_S
        p.cast = None
        return
    if kind == "ember":
        if tgt and tgt.alive and dist(p, tgt) <= EMBER_RANGE_M:
            raw = ember_raw(p.stats)
            hit_mob(p, tgt, raw)
        p.cast = None
        return
    p.cast = None


def martial_auto(p: Player, mobs: List[Mob]) -> None:
    if p.stats["discipline"] != "Martial" or not p.alive:
        return
    live = living(mobs)
    if not live:
        return
    # lock lowest-idx living (focus current until dead — we keep lock if still alive)
    if p.lock is None or not any(m.idx == p.lock and m.alive for m in live):
        p.lock = live[0].idx
    tgt = next(m for m in live if m.idx == p.lock)
    d = dist(p, tgt)
    if d > MELEE_REACH_M:
        return
    # in-cone assumed unless kiting (Martial is not kiting)
    strike = p.strike_armed
    raw = melee_raw(p.stats, strike=strike)
    if strike:
        p.strike_armed = False
    hit_mob(p, tgt, raw)
    p.swings += 1
    # Cleave
    if p.rank("Martial") >= 7:
        for m in live:
            if m.idx == tgt.idx or not m.alive:
                continue
            if dist(p, m) <= CLEAVE_RANGE_M:
                hit_mob(p, m, trunc(raw * CLEAVE_PCT))
                break


def move_entities(p: Player, mobs: List[Mob], desired: float) -> None:
    live = living(mobs)
    if not live:
        return
    # Hold range vs the chasing (unrooted) pack. If Bind left someone
    # behind and the pack is dead, walk back to the stranded mob.
    chasing = [m for m in live if m.root <= 0]
    ref = chasing if chasing else live
    nearest = min(ref, key=lambda m: dist(p, m))
    d0 = dist(p, nearest)
    if d0 < desired - 0.05:
        # hold range: Hunt/Arcane sprint (free, no stamina). Walk cannot
        # outrun trash 5.2. Martial stays at walk (melee, not kiting).
        disc = p.stats["discipline"]
        if disc in ("Hunt", "Arcane"):
            p.x -= SPRINT_MPS * TICK
            p.sprinted = True
        else:
            p.x -= WALK_MPS * TICK
    elif d0 > desired + 0.05:
        # close a gap (stranded Bind target) — walk, do not sprint in
        p.x += WALK_MPS * TICK
    # mobs chase (or stand if rooted/stunned)
    for m in live:
        if m.stun > 0 or m.root > 0:
            continue
        spd = m.speed * (1.0 - PIN_SLOW_PCT if m.slow > 0 else 1.0)
        if m.x > p.x:
            m.x = max(p.x, m.x - spd * TICK)
        else:
            m.x = min(p.x, m.x + spd * TICK)


def tick_fight(
    p: Player,
    mobs: List[Mob],
    desired_range: float,
    kite: bool,
) -> None:
    # 1. time auras / cds
    for k in list(p.cds):
        if p.cds[k] > 0:
            p.cds[k] = max(0.0, p.cds[k] - TICK)
    if p.gcd > 0:
        p.gcd = max(0.0, p.gcd - TICK)
    if p.ward_t > 0:
        p.ward_t = max(0.0, p.ward_t - TICK)
        if p.ward_t <= 0:
            p.ward = 0.0
    if p.mark_t > 0:
        p.mark_t = max(0.0, p.mark_t - TICK)
    if p.poison_t > 0:
        p.poison_acc += SCORP_POISON_DPS * TICK
        p.poison_t = max(0.0, p.poison_t - TICK)
        if p.poison_acc >= 1.0:
            ticks = int(p.poison_acc)
            p.poison_acc -= ticks
            apply_player_damage(p, ticks, interrupt=True)
    p.mana = min(p.max_mana, p.mana + MANA_REGEN_PER_S * TICK)
    if p.mana < p.min_mana:
        p.min_mana = p.mana
    if p.mana <= 1e-9:
        p.mana_hit_zero = True

    for m in mobs:
        if not m.alive:
            continue
        if m.stun > 0:
            m.stun = max(0.0, m.stun - TICK)
        if m.root_grace > 0:
            m.root_grace = max(0.0, m.root_grace - TICK)
        if m.root > 0:
            m.root = max(0.0, m.root - TICK)
        if m.slow > 0:
            m.slow = max(0.0, m.slow - TICK)
        if (not m.enraged) and m.max_hp > 0 and m.hp <= MOTHER_ENRAGE_HP * m.max_hp:
            if m.id == "line_mother":
                m.enraged = True
                m.swing = m.base_swing * MOTHER_ENRAGE_SWING

    if not p.alive:
        return

    # 2. player AI may start something
    player_ai_start(p, mobs, kite)

    # 3. progress player cast
    if p.busy > 0:
        p.busy = max(0.0, p.busy - TICK)
        if p.busy <= 0 and p.cast is not None:
            finish_cast(p, mobs)

    # 4. martial auto continues while casting
    if p.stats["discipline"] == "Martial":
        in_range = any(m.alive and dist(p, m) <= MELEE_REACH_M for m in mobs)
        if in_range:
            p.auto_cd -= TICK
            if p.auto_cd <= 0:
                martial_auto(p, mobs)
                p.auto_cd += MELEE_SWING_S
        else:
            # out of range: timer does not advance (not swinging at air)
            pass
    else:
        # Hunt/Arcane: their "auto" is the draw/cast started by AI
        pass

    if not p.alive:
        return

    # 5. mobs: slam telegraph + auto (paused while stunned)
    for m in living(mobs):
        if m.stun > 0:
            continue
        if m.has_slam:
            if m.tele > 0:
                m.tele = max(0.0, m.tele - TICK)
                if m.tele <= 0:
                    # slam lands if still in reach
                    if p.alive and dist(p, m) <= m.reach:
                        raw = trunc(m.slam_dmg * m.dmg_mult())
                        dealt = mitigation(raw, p.stats["attrs"]["Grit"])
                        apply_player_damage(p, dealt, interrupt=True)
            else:
                m.slam_cd -= TICK
                if m.slam_cd <= 0:
                    m.tele = m.tele_len
                    m.slam_cd += float(m.sheet.get("slam_every_s") or 8.0)
        # auto
        if dist(p, m) <= m.reach:
            m.auto_cd -= TICK
            if m.auto_cd <= 0:
                raw = trunc(m.base_dmg * m.dmg_mult())
                dealt = mitigation(raw, p.stats["attrs"]["Grit"])
                apply_player_damage(p, dealt, interrupt=True)
                if m.sheet["id"] == "crawler_scorpion" and p.alive:
                    p.poison_t = SCORP_POISON_S  # refresh
                m.auto_cd += m.swing
        # else timer holds (not swinging at air)

    # 6. move last so this tick's swings used pre-move positions
    move_entities(p, mobs, desired_range)


def desired_range_for(disc: str, kite: bool) -> Tuple[float, float]:
    """Return (start_distance, hold_distance)."""
    if disc == "Martial":
        # both connects: inside wolf 1.8 and player 2.8
        return 1.5, 1.5
    if disc == "Hunt":
        return 18.0, 18.0
    return 16.0, 16.0


def simulate_fight(
    level: int,
    discipline: str,
    mob_id: str,
    count: int,
    seed: int = 1,
    potions: Optional[int] = None,
    arrows: Optional[int] = None,
    mob_level: Optional[int] = None,
    kite: Optional[bool] = None,
    notes: str = "",
) -> Dict[str, Any]:
    mob_id = resolve_mob_id(mob_id)
    stats = player_stats(level, discipline)
    if potions is None:
        potions = START_POTIONS
    if arrows is None:
        arrows = START_ARROWS
    p = Player(stats, potions=potions, arrows=arrows)
    disc = stats["discipline"]
    if kite is None:
        kite = disc != "Martial"
    start_d, hold_d = desired_range_for(disc, kite)
    if not kite:
        start_d, hold_d = 1.5, 1.5

    builder = MOB_BUILDERS[mob_id]
    sheets: List[Dict[str, Any]] = []
    mobs: List[Mob] = []
    for i in range(count):
        if mob_id == "crawler_spider_wolf":
            sh = wolf_sheet(mob_level if mob_level is not None else level)
        else:
            sh = builder()
        sheets.append(sh)
        # pack stacked on the line, all social-in (0.4 m apart, well inside 15 m)
        mobs.append(Mob(sh, x=start_d + i * 0.4, idx=i))

    # first swing timing: player martial auto lands at 1.8; mobs land at their swing
    p.auto_cd = MELEE_SWING_S

    t = 0.0
    winner = "timeout"
    while t < HARD_CAP_S - 1e-9:
        tick_fight(p, mobs, hold_d, kite)
        t += TICK
        t = round(t, 4)  # keep 0.1 grid clean
        if not p.alive:
            winner = "mobs"
            break
        if not living(mobs):
            winner = "player"
            break

    ttk = t if winner == "player" else None
    ttd = t if winner == "mobs" else None
    if winner == "timeout":
        ttd = t

    result = {
        "time_to_kill_s": ttk,
        "time_to_die_s": ttd,
        "mana_spent": p.mana_spent,
        "swings": p.swings,
        "spells_used": dict(p.spells),
        "winner": winner,
        "hp_remaining": int(round(p.hp)),
        "hp_max": int(p.max_hp),
        "hp_pct": round(100.0 * p.hp / p.max_hp, 1) if p.max_hp else 0.0,
        "player_level": level,
        "discipline": disc,
        "mob_id": mob_id,
        "mob_level": sheets[0]["level"] if sheets else None,
        "count": count,
        "seed": seed,
        "potions_used": p.spells.get("potion", 0),
        "max_hit_on_player": p.max_hit_taken,
        "oneshot": p.max_hit_taken >= int(p.max_hp),
        "player_sprinted": bool(p.sprinted),
        "used_pin_or_bind": bool(p.used_pin_or_bind),
        "damage_taken": int(p.damage_taken),
        "mana_hit_zero": bool(p.mana_hit_zero),
        "skipped_spell_for_mana": bool(p.skipped_spell_for_mana),
        "mana_decision": bool(p.mana_hit_zero or p.skipped_spell_for_mana),
        "min_mana": round(p.min_mana, 2),
        "notes": notes,
        "player": {
            "hp": int(p.max_hp),
            "mana": int(p.max_mana),
            "attrs": stats["attrs"],
            "ranks": stats["ranks"],
            "weapon_skill": stats["weapon_skill"],
            "melee_hit": melee_raw(stats, False),
            "melee_strike": melee_raw(stats, True),
            "bow_hit": bow_raw(stats, False),
            "bow_aimed": bow_raw(stats, True),
            "ember": ember_raw(stats),
            "mend": mend_raw(stats),
            "ward": ward_raw(stats),
        },
        "mob_sheet": sheets[0] if sheets else None,
    }
    return result


# ---------------------------------------------------------------------------
# Bands
# ---------------------------------------------------------------------------
def kite_walk_fail(r: Dict[str, Any]) -> bool:
    """Hunt/Arcane 0 damage while only walking, no Pin/Bind = trash too slow."""
    if r.get("discipline") not in ("Hunt", "Arcane"):
        return False
    return (
        int(r.get("damage_taken") or 0) == 0
        and not r.get("player_sprinted")
        and not r.get("used_pin_or_bind")
    )


def even_1v1_pass(r: Dict[str, Any]) -> Tuple[bool, str]:
    if r["winner"] != "player":
        return False, "player must win"
    ttk = r["time_to_kill_s"]
    if ttk is None or ttk < 8.0 or ttk > 14.0:
        return False, f"TTK {ttk} not in 8.0–14.0"
    if r["hp_remaining"] < 0.40 * r["hp_max"]:
        return False, f"end HP {r['hp_pct']}% < 40%"
    if r["oneshot"]:
        return False, "one-shot"
    if kite_walk_fail(r):
        return False, "0 damage walking, no Pin/Bind — raise trash speed"
    return True, "even 1v1"


def two_pull_nopot_pass(r: Dict[str, Any]) -> Tuple[bool, str]:
    if r["winner"] == "mobs":
        return True, "lose (expected risky)"
    if r["winner"] == "player" and r["hp_remaining"] < 0.20 * r["hp_max"]:
        return True, "win but <20% HP"
    return False, f"too safe ({r['winner']}, {r['hp_pct']}% HP)"


def two_pull_pot_pass(r: Dict[str, Any]) -> Tuple[bool, str]:
    if r["winner"] == "player":
        return True, "win with potion"
    return False, f"lost TTD={r['time_to_die_s']}"


def pale_hall_pass(r: Dict[str, Any]) -> Tuple[bool, str]:
    if r["winner"] != "player":
        return False, "must WIN"
    ttk = r["time_to_kill_s"]
    if ttk is None or ttk < 20.0 or ttk > 50.0:
        return False, f"TTK {ttk} not in 20–50 (room)"
    if r["hp_remaining"] <= 0:
        return False, "end HP not >0"
    if r["oneshot"]:
        return False, "one-shot"
    if kite_walk_fail(r):
        return False, "0 damage walking, no Pin/Bind — raise trash speed"
    return True, "Pale Hall room"


def heart_pass(r: Dict[str, Any]) -> Tuple[bool, str]:
    if r["winner"] != "player":
        return False, "must WIN"
    ttk = r["time_to_kill_s"]
    if ttk is None or ttk < 25.0 or ttk > 45.0:
        return False, f"TTK {ttk} not in 25–45 (heart)"
    lo, hi = 0.20 * r["hp_max"], 0.50 * r["hp_max"]
    if r["hp_remaining"] < lo or r["hp_remaining"] > hi:
        return False, f"end HP {r['hp_pct']}% not in 20–50%"
    if r["oneshot"]:
        return False, "one-shot"
    return True, "heart"


def win_pass(r: Dict[str, Any]) -> Tuple[bool, str]:
    if r["winner"] == "player" and not r["oneshot"]:
        return True, "must WIN"
    return False, f"{r['winner']} oneshot={r['oneshot']}"


SCENARIOS = [
    {
        "id": "1_l1_martial_1wolf",
        "title": "L1 Martial vs 1 wolf-spider",
        "level": 1,
        "discipline": "Martial",
        "mob": "crawler_spider_wolf",
        "count": 1,
        "mob_level": 1,
        "potions": 1,
        "kite": False,
        "band": "even_1v1",
        "check": even_1v1_pass,
    },
    {
        "id": "2_l1_martial_2wolf_nopot",
        "title": "L1 Martial vs 2 wolf-spiders, no potion",
        "level": 1,
        "discipline": "Martial",
        "mob": "crawler_spider_wolf",
        "count": 2,
        "mob_level": 1,
        "potions": 0,
        "kite": False,
        "band": "2pull_nopot",
        "check": two_pull_nopot_pass,
    },
    {
        "id": "3_l1_martial_2wolf_pot",
        "title": "L1 Martial vs 2 wolf-spiders, 1 potion",
        "level": 1,
        "discipline": "Martial",
        "mob": "crawler_spider_wolf",
        "count": 2,
        "mob_level": 1,
        "potions": 1,
        "kite": False,
        "band": "2pull_pot",
        "check": two_pull_pot_pass,
    },
    {
        "id": "4_l3_hunt_scorpion",
        "title": "L3 Hunt vs 1 scorpion",
        "level": 3,
        "discipline": "Hunt",
        "mob": "crawler_scorpion",
        "count": 1,
        "mob_level": 3,
        "potions": 1,
        "kite": True,
        "band": "even_1v1",
        "check": even_1v1_pass,
    },
    {
        "id": "5_l5_arcane_pale_hall",
        "title": "L5 Arcane vs Pale Hall (4 L4 wolves)",
        "level": 5,
        "discipline": "Arcane",
        "mob": "crawler_spider_wolf",
        "count": 4,
        "mob_level": 4,
        "potions": 1,
        "kite": True,
        "band": "pale_hall",
        "check": pale_hall_pass,
    },
    {
        "id": "6_l9_martial_line_mother",
        "title": "L9 Martial vs Line-Mother",
        "level": 9,
        "discipline": "Martial",
        "mob": "line_mother",
        "count": 1,
        "mob_level": 9,
        "potions": 1,
        "kite": False,
        "band": "heart",
        "check": heart_pass,
    },
    {
        "id": "7_l2_martial_blob",
        "title": "L2 Martial vs 1 GreenBlob",
        "level": 2,
        "discipline": "Martial",
        "mob": "green_blob",
        "count": 1,
        "mob_level": 2,
        "potions": 1,
        "kite": False,
        "band": "even_1v1",
        "check": even_1v1_pass,
    },
    {
        "id": "s_l2_martial_d1_pot",
        "title": "SANITY L2 Martial vs D1 (2 L1 wolves) + potion",
        "level": 2,
        "discipline": "Martial",
        "mob": "crawler_spider_wolf",
        "count": 2,
        "mob_level": 1,
        "potions": 1,
        "kite": False,
        "band": "d1_clear",
        "check": win_pass,
    },
]


def run_scenario(sc: Dict[str, Any], seed: int = 1) -> Dict[str, Any]:
    r = simulate_fight(
        level=sc["level"],
        discipline=sc["discipline"],
        mob_id=sc["mob"],
        count=sc["count"],
        seed=seed,
        potions=sc["potions"],
        mob_level=sc.get("mob_level"),
        kite=sc.get("kite"),
        notes=sc["title"],
    )
    ok, why = sc["check"](r)
    r["band"] = sc["band"]
    r["band_pass"] = bool(ok)
    r["band_reason"] = why
    r["scenario_id"] = sc["id"]
    r["title"] = sc["title"]
    return r


def oneshot_sanity() -> Dict[str, Any]:
    """L5 Arcane must not be one-shot by 4 L4 wolves in the first swing."""
    p = player_stats(5, "Arcane")
    w = wolf_sheet(4)
    hit = mitigation(w["damage"], p["attrs"]["Grit"])
    volley = hit * 4
    return {
        "title": "SANITY L5 Arcane not one-shot by 4 L4 wolves first swing",
        "scenario_id": "s_l5_arcane_oneshot",
        "player_level": 5,
        "discipline": "Arcane",
        "mob_id": "crawler_spider_wolf",
        "count": 4,
        "seed": 1,
        "winner": "n/a",
        "time_to_kill_s": None,
        "time_to_die_s": None,
        "mana_spent": 0,
        "swings": 0,
        "spells_used": {},
        "hp_remaining": p["hp"],
        "hp_max": p["hp"],
        "hp_pct": 100.0,
        "one_wolf_hit": hit,
        "four_wolf_volley": volley,
        "player_max_hp": p["hp"],
        "oneshot": volley >= p["hp"],
        "band": "no_oneshot",
        "band_pass": volley < p["hp"],
        "band_reason": f"4×{hit}={volley} vs {p['hp']} HP",
        "notes": "first-swing volley vs L5 Arcane max HP (kiting aside)",
        "max_hit_on_player": volley,
        "player_sprinted": False,
        "used_pin_or_bind": False,
        "damage_taken": 0,
        "mana_hit_zero": False,
        "skipped_spell_for_mana": False,
        "mana_decision": False,
        "min_mana": None,
    }


def xp_curve() -> Dict[str, Any]:
    cum = 0
    rows = []
    for n in range(1, 10):
        need = XP_TO_NEXT[n]
        cum += need
        rows.append({"from": n, "to": n + 1, "xp": need, "cumulative_to": cum})
    return {
        "xp_to_next": XP_TO_NEXT,
        "rows": rows,
        "cumulative_to_10": cum,
        "heart_bonus": HEART_BONUS,
        "first_clear_bonus": FIRST_CLEAR,
        "minutes_per_dungeon": MIN_PER_DUNGEON,
        "dungeon_wolf_counts": DUNGEON_WOLVES,
        "dungeon_wolf_level": DUNGEON_WOLF_LEVEL,
    }


def dungeon_xp(tier: int, first: bool) -> Dict[str, Any]:
    n = DUNGEON_WOLVES[tier]
    lvl = DUNGEON_WOLF_LEVEL[tier]
    trash = n * wolf_sheet(lvl)["xp"]
    elite = MOTHER_XP if tier == 2 else 0
    heart = HEART_BONUS[tier]  # named dies
    first_b = FIRST_CLEAR[tier] if first else 0
    total = trash + elite + heart + first_b
    return {
        "tier": tier,
        "first": first,
        "trash_xp": trash,
        "elite_xp": elite,
        "heart_bonus": heart,
        "first_clear": first_b,
        "total": total,
        "minutes": MIN_PER_DUNGEON[tier],
    }


def leveling_path() -> Dict[str, Any]:
    """D1 first, D1 repeats to L4, D2 first + repeats to L8, T2/Mother to L10."""
    need = {n: XP_TO_NEXT[n] for n in range(1, 10)}
    level = 1
    xp_into = 0
    minutes = 0.0
    events = []

    def grant(amount: int, label: str, mins: float) -> None:
        nonlocal level, xp_into, minutes
        minutes += mins
        left = amount
        leveled = []
        while level < 10 and left > 0:
            room = need[level] - xp_into
            take = min(room, left)
            xp_into += take
            left -= take
            if xp_into >= need[level]:
                level += 1
                xp_into = 0
                leveled.append(level)
        events.append(
            {
                "label": label,
                "xp": amount,
                "minutes": mins,
                "level_after": level,
                "xp_into_level": xp_into,
                "leveled_to": leveled,
                "total_minutes": minutes,
            }
        )

    d1f = dungeon_xp(0, True)
    grant(d1f["total"], "D1 first-clear (2 L1 wolves + heart + first)", d1f["minutes"])
    while level < 4:
        d1 = dungeon_xp(0, False)
        grant(d1["total"], "D1 repeat (2 L1 wolves + heart)", d1["minutes"])
    d2f = dungeon_xp(1, True)
    grant(d2f["total"], "D2 first-clear Pale Hall (4 L4 wolves + heart + first)", d2f["minutes"])
    while level < 8:
        d2 = dungeon_xp(1, False)
        grant(d2["total"], "D2 repeat (4 L4 wolves + heart)", d2["minutes"])
    t2f = dungeon_xp(2, True)
    grant(t2f["total"], "T2 first-clear (6 L6 wolves + Line-Mother + heart + first)", t2f["minutes"])
    while level < 10:
        t2 = dungeon_xp(2, False)
        grant(t2["total"], "T2 repeat (6 L6 wolves + Line-Mother + heart)", t2["minutes"])

    return {
        "events": events,
        "final_level": level,
        "total_minutes": minutes,
        "in_90_150": 90.0 <= minutes <= 150.0,
        "d1_first_xp": d1f["total"],
        "d1_repeat_xp": dungeon_xp(0, False)["total"],
        "d2_first_xp": d2f["total"],
        "d2_repeat_xp": dungeon_xp(1, False)["total"],
        "t2_first_xp": t2f["total"],
        "t2_repeat_xp": dungeon_xp(2, False)["total"],
        "clears": {
            "d1": sum(1 for e in events if e["label"].startswith("D1")),
            "d2": sum(1 for e in events if e["label"].startswith("D2")),
            "t2": sum(1 for e in events if e["label"].startswith("T2")),
        },
    }


def print_table(rows: List[Dict[str, Any]]) -> None:
    hdr = (
        f"{'SCENARIO':<46} {'WIN':<8} {'TTK':>6} {'TTD':>6} {'HP%':>6} "
        f"{'HP':>5} {'MANA':>5} {'SPR':>3} {'PIN':>3} {'DMG':>4} {'MDEC':>4} "
        f"{'BAND':<12} {'PASS':<6}"
    )
    print(hdr)
    print("-" * len(hdr))
    for r in rows:
        ttk = f"{r['time_to_kill_s']:.1f}" if r.get("time_to_kill_s") is not None else "—"
        ttd = f"{r['time_to_die_s']:.1f}" if r.get("time_to_die_s") is not None else "—"
        title = r.get("title") or r.get("notes") or r.get("scenario_id") or ""
        bp = r.get("band_pass")
        mark = "PASS" if bp is True else ("FAIL" if bp is False else "—")
        spr = "Y" if r.get("player_sprinted") else "n"
        pin = "Y" if r.get("used_pin_or_bind") else "n"
        mdec = "Y" if r.get("mana_decision") else "n"
        print(
            f"{title[:46]:<46} {str(r.get('winner')):<8} {ttk:>6} {ttd:>6} "
            f"{r.get('hp_pct', 0):>5.1f}% {r.get('hp_remaining', 0):>5} "
            f"{r.get('mana_spent', 0):>5} {spr:>3} {pin:>3} {r.get('damage_taken', 0):>4} "
            f"{mdec:>4} {str(r.get('band') or ''):<12} "
            f"{mark:<6}"
        )


def run_all(seed: int = 1) -> Dict[str, Any]:
    results = [run_scenario(sc, seed=seed) for sc in SCENARIOS]
    results.append(oneshot_sanity())
    path = leveling_path()
    all_pass = all(r["band_pass"] for r in results) and path["in_90_150"]
    payload = {
        "formulas": formulas(),
        "xp_curve": xp_curve(),
        "leveling_path": path,
        "mob_sheets": {
            "crawler_spider_wolf_l1": wolf_sheet(1),
            "crawler_spider_wolf_l4": wolf_sheet(4),
            "crawler_spider_wolf_l6": wolf_sheet(6),
            "line_mother": mother_sheet(),
            "crawler_scorpion": scorpion_sheet(),
            "green_blob": blob_sheet(),
            "orc": orc_sheet(),
            "yeti": yeti_sheet(),
            "demon": demon_sheet(),
        },
        "player_specialists": {
            f"L{lv}_{d}": player_stats(lv, d)
            for lv in (1, 2, 3, 5, 9, 10)
            for d in ("Martial", "Hunt", "Arcane")
        },
        "scenarios": results,
        "all_pass": all_pass,
        "seed": seed,
    }
    return payload


def main(argv: Optional[List[str]] = None) -> int:
    ap = argparse.ArgumentParser(description="Orrun v1 combat simulator")
    ap.add_argument("--scenario", default=None, help="all | scenario id")
    ap.add_argument("--out", default=None, help="write JSON here")
    ap.add_argument("--player-level", type=int, default=None)
    ap.add_argument("--discipline", default="Martial")
    ap.add_argument("--mob", default="crawler_spider_wolf")
    ap.add_argument("--count", type=int, default=1)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--potions", type=int, default=None)
    ap.add_argument("--mob-level", type=int, default=None)
    args = ap.parse_args(argv)

    if args.scenario in (None,) and args.player_level is not None:
        r = simulate_fight(
            level=args.player_level,
            discipline=args.discipline,
            mob_id=args.mob,
            count=args.count,
            seed=args.seed,
            potions=args.potions,
            mob_level=args.mob_level,
        )
        r["band_pass"] = None
        r["title"] = f"L{args.player_level} {args.discipline} vs {args.count} {args.mob}"
        print_table([r])
        text = json.dumps({"formulas": formulas(), "fight": r}, indent=2)
        if args.out:
            with open(args.out, "w", encoding="utf-8") as f:
                f.write(text + "\n")
        else:
            print(text)
        return 0

    if args.scenario in (None, "all"):
        payload = run_all(seed=args.seed)
        print_table(payload["scenarios"])
        print()
        print(
            f"XP path: {payload['leveling_path']['total_minutes']:.0f} min to L10 "
            f"({payload['leveling_path']['clears']}) "
            f"{'PASS' if payload['leveling_path']['in_90_150'] else 'FAIL 90–150'}"
        )
        print(f"ALL BANDS: {'PASS' if payload['all_pass'] else 'FAIL'}")
        if args.out:
            with open(args.out, "w", encoding="utf-8") as f:
                json.dump(payload, f, indent=2)
                f.write("\n")
            print(f"wrote {args.out}")
        return 0 if payload["all_pass"] else 1

    sc = next((s for s in SCENARIOS if s["id"] == args.scenario or s["id"].startswith(args.scenario)), None)
    if sc is None:
        print("unknown scenario. ids:", [s["id"] for s in SCENARIOS], file=sys.stderr)
        return 2
    r = run_scenario(sc, seed=args.seed)
    print_table([r])
    print(json.dumps(r, indent=2))
    if args.out:
        with open(args.out, "w", encoding="utf-8") as f:
            json.dump({"formulas": formulas(), "fight": r}, f, indent=2)
            f.write("\n")
    return 0 if r["band_pass"] else 1


if __name__ == "__main__":
    sys.exit(main())
