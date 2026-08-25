import unittest
from pathlib import Path
from tools.gamedata import GameData

class GameDataTests(unittest.TestCase):
    def test_starter_data_has_all_sections_and_fifteen_mobs(self):
        data = GameData.load(Path("data/OrrunGameData.xml"))
        self.assertEqual(len(data.mobs), 15)
        self.assertEqual(data.validate(), [])

    def test_canonical_round_trip(self):
        data = GameData.load(Path("data/OrrunGameData.xml"))
        self.assertEqual(GameData.from_xml(data.to_xml()).to_xml(), data.to_xml())

    def test_unknown_references_are_rejected(self):
        xml = GameData.load(Path("data/OrrunGameData.xml")).to_xml().replace('skill_id="slashing_damage"', 'skill_id="missing"', 1)
        with self.assertRaisesRegex(ValueError, "unknown skill"):
            GameData.from_xml(xml)

    def test_profile_skills_and_mob_actions_are_loaded(self):
        data = GameData.load(Path("data/OrrunGameData.xml"))
        profile = next(p for p in data.player_profiles if p.id == "default_player")
        self.assertGreater(len(profile.skills), 0)
        wolf = next(m for m in data.mobs if m.id == "crawler_spider_wolf")
        self.assertIn("strike", wolf.actions)

    def test_no_mob_xp_field(self):
        data = GameData.load(Path("data/OrrunGameData.xml"))
        self.assertFalse(hasattr(data.mobs[0], "xp"))

    def test_unknown_mob_action_rejected(self):
        xml = GameData.load(Path("data/OrrunGameData.xml")).to_xml().replace('<action id="strike" />', '<action id="missing" />', 1)
        with self.assertRaisesRegex(ValueError, "unknown action"):
            GameData.from_xml(xml)

if __name__ == "__main__": unittest.main()
