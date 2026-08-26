import unittest
from pathlib import Path
from tools.gamedata import GameData

class GameDataTests(unittest.TestCase):
    def test_starter_data_has_all_sections_and_canonical_animals(self):
        data = GameData.load(Path("data/OrrunGameData.xml"))
        self.assertEqual(len(data.mobs), 26)
        self.assertEqual(data.validate(), [])

    def test_canonical_round_trip(self):
        data = GameData.load(Path("data/OrrunGameData.xml"))
        self.assertEqual(GameData.from_xml(data.to_xml()).to_xml(), data.to_xml())

    def test_action_timings_round_trip(self):
        data = GameData.load(Path("data/OrrunGameData.xml"))
        actions = {action.id: action for action in data.actions}
        self.assertEqual(actions["strike"].cast_s, 0.0)
        self.assertEqual(actions["strike"].cooldown_s, 6.0)
        self.assertEqual(actions["fire_bolt"].cast_s, 1.2)
        self.assertEqual(actions["fire_bolt"].cooldown_s, 0.0)
        self.assertEqual(actions["mend"].cast_s, 2.5)
        round_tripped = {action.id: action for action in GameData.from_xml(data.to_xml()).actions}
        self.assertEqual(round_tripped["strike"].cooldown_s, 6.0)
        self.assertEqual(round_tripped["fire_bolt"].cast_s, 1.2)
        self.assertEqual(round_tripped["mend"].cast_s, 2.5)

    def test_negative_action_timing_is_rejected(self):
        xml = GameData.load(Path("data/OrrunGameData.xml")).to_xml().replace('cast_s="1.2"', 'cast_s="-1"', 1)
        with self.assertRaisesRegex(ValueError, "cast_s and cooldown_s must be non-negative"):
            GameData.from_xml(xml)

    def test_mob_motion_bounds_and_round_trip(self):
        original = GameData.load(Path("data/OrrunGameData.xml")).to_xml()
        for ratio in ("0", "0.20"):
            xml = original.replace('speed_variance_ratio="0.08"', f'speed_variance_ratio="{ratio}"', 1)
            data = GameData.from_xml(xml)
            wolf = next(mob for mob in data.mobs if mob.id == "wolf")
            self.assertEqual(wolf.speed_variance_ratio, float(ratio))
            self.assertEqual(wolf.endurance_s, 28.0)
            round_tripped = GameData.from_xml(data.to_xml())
            round_trip_wolf = next(mob for mob in round_tripped.mobs if mob.id == "wolf")
            self.assertEqual(round_trip_wolf.speed_variance_ratio, float(ratio))
            self.assertEqual(round_trip_wolf.endurance_s, 28.0)

    def test_missing_mob_motion_fields_are_rejected(self):
        original = GameData.load(Path("data/OrrunGameData.xml")).to_xml()
        for attribute in (' speed_variance_ratio="0.08"', ' endurance_s="28"'):
            with self.subTest(attribute=attribute):
                xml = original.replace(attribute, "", 1)
                with self.assertRaisesRegex(ValueError, "missing attributes"):
                    GameData.from_xml(xml)

    def test_non_finite_mob_motion_fields_are_rejected(self):
        original = GameData.load(Path("data/OrrunGameData.xml")).to_xml()
        for attribute, current in (("speed_variance_ratio", "0.08"), ("endurance_s", "28")):
            for invalid in ("nan", "inf", "-inf"):
                with self.subTest(attribute=attribute, invalid=invalid):
                    xml = original.replace(f'{attribute}="{current}"', f'{attribute}="{invalid}"', 1)
                    with self.assertRaisesRegex(ValueError, f"{attribute} must be finite"):
                        GameData.from_xml(xml)

    def test_invalid_mob_motion_domains_are_rejected(self):
        original = GameData.load(Path("data/OrrunGameData.xml")).to_xml()
        cases = (
            ("speed_variance_ratio", "0.08", "-0.01", "0..=0.20"),
            ("speed_variance_ratio", "0.08", "0.200001", "0..=0.20"),
            ("endurance_s", "28", "0", "finite and positive"),
            ("endurance_s", "28", "-1", "finite and positive"),
        )
        for attribute, current, invalid, message in cases:
            with self.subTest(attribute=attribute, invalid=invalid):
                xml = original.replace(f'{attribute}="{current}"', f'{attribute}="{invalid}"', 1)
                with self.assertRaisesRegex(ValueError, message):
                    GameData.from_xml(xml)

    def test_unknown_references_are_rejected(self):
        xml = GameData.load(Path("data/OrrunGameData.xml")).to_xml().replace('skill_id="slashing_damage"', 'skill_id="missing"', 1)
        with self.assertRaisesRegex(ValueError, "unknown skill"):
            GameData.from_xml(xml)

    def test_profile_skills_and_mob_actions_are_loaded(self):
        data = GameData.load(Path("data/OrrunGameData.xml"))
        profile = next(p for p in data.player_profiles if p.id == "default_player")
        self.assertGreater(len(profile.skills), 0)
        wolf = next(m for m in data.mobs if m.id == "wolf")
        self.assertIn("strike", wolf.actions)

    def test_no_mob_xp_field(self):
        data = GameData.load(Path("data/OrrunGameData.xml"))
        self.assertFalse(hasattr(data.mobs[0], "xp"))

    def test_unknown_mob_action_rejected(self):
        xml = GameData.load(Path("data/OrrunGameData.xml")).to_xml().replace('<action id="strike" />', '<action id="missing" />', 1)
        with self.assertRaisesRegex(ValueError, "unknown action"):
            GameData.from_xml(xml)

if __name__ == "__main__": unittest.main()
