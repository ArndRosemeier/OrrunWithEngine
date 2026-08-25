from .model import Spell, SpellEffect, TargetMode

def starter_templates() -> dict[str, Spell]:
    return {
      "fire_bolt": Spell("fire_bolt", "Fire Bolt", "A direct burst of fire.", TargetMode.HOSTILE, "projectile", [SpellEffect("fire_damage", {"magnitude": 1.0, "range": 12})]),
      "frost_nova": Spell("frost_nova", "Frost Nova", "Frost damage around the caster.", TargetMode.AREA, "area", [SpellEffect("frost_damage", {"magnitude": 1.0, "radius": 5}), SpellEffect("slow", {"duration": 4})]),
      "searing_root": Spell("searing_root", "Searing Root", "Damage and hold a foe.", TargetMode.HOSTILE, "direct", [SpellEffect("fire_damage", {"magnitude": 1.0}), SpellEffect("root", {"duration": 3})]),
      "renew": Spell("renew", "Renew", "Healing over time.", TargetMode.FRIENDLY, "direct", [SpellEffect("regeneration", {"magnitude": 1.0, "duration": 8})]),
      "ward_cleanse": Spell("ward_cleanse", "Ward and Cleanse", "A clean protective ward.", TargetMode.FRIENDLY, "direct", [SpellEffect("shield", {"magnitude": 1.0, "duration": 10}), SpellEffect("cleanse")]),
      "beguiling_word": Spell("beguiling_word", "Beguiling Word", "Charm one hostile target.", TargetMode.HOSTILE, "direct", [SpellEffect("charm", {"duration": 6})]),
      "call_companion": Spell("call_companion", "Call Companion", "Summon a temporary pet.", TargetMode.SELF, "ground_targeted", [SpellEffect("summon", {"duration": 30})]),
      "blink": Spell("blink", "Blink", "Teleport a short distance.", TargetMode.SELF, "ground_targeted", [SpellEffect("teleport", {"range": 10})]),
      "sight_unweaving": Spell("sight_unweaving", "Sight Unweaving", "Reveal and dispel an area.", TargetMode.AREA, "area", [SpellEffect("reveal", {"duration": 8, "radius": 5}), SpellEffect("dispel", {"radius": 5})]),
      "battle_focus": Spell("battle_focus", "Battle Focus", "A friendly haste and stat buff.", TargetMode.FRIENDLY, "direct", [SpellEffect("haste", {"duration": 12}), SpellEffect("stat_buff", {"duration": 12})]),
    }