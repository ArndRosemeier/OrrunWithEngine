import unittest
from pathlib import Path

from tools.gamedata import GameData


class GameDataTests(unittest.TestCase):
    def setUp(self):
        self.data = GameData.load(Path("data/OrrunGameData.xml"))
        self.xml = self.data.to_xml()

    def test_canonical_contract_and_round_trip(self):
        self.assertEqual(self.data.validate(), [])
        self.assertEqual(GameData.from_xml(self.xml).to_xml(), self.xml)
        default_player = next(
            profile
            for profile in self.data.player_profiles
            if profile.id == "default_player"
        )
        self.assertEqual(
            default_player.actions,
            ("slash", "arrow", "restore", "entangle", "stasis", "hobble", "befriend"),
        )
        self.assertEqual(
            {effect.operation for effect in self.data.effects},
            {"direct_damage", "heal", "root", "hold", "snare", "charm"},
        )

    def test_action_contract(self):
        restore = next(action for action in self.data.actions if action.id == "restore")
        self.assertTrue(restore.interruptible)
        self.assertFalse(restore.reveals)
        snare = next(action for action in self.data.actions if action.id == "hobble").effects[0]
        self.assertEqual((snare.duration_s, snare.movement_multiplier), (6.0, 0.5))

    def test_unknown_attributes_are_rejected(self):
        with self.assertRaisesRegex(ValueError, "unknown attributes"):
            GameData.from_xml(
                self.xml.replace('id="slash"', 'id="slash" surprise="x"', 1)
            )

    def test_numeric_parity_rejects_non_finite(self):
        for field, current in (
            ("mana_cost", "0"),
            ("duration_s", "4"),
            ("movement_multiplier", "0.5"),
            ("speed_mps", "2.5"),
        ):
            with self.subTest(field=field):
                with self.assertRaisesRegex(ValueError, "must be finite"):
                    GameData.from_xml(
                        self.xml.replace(
                            f'{field}="{current}"', f'{field}="nan"', 1
                        )
                    )

    def test_operation_specific_validation(self):
        cases = (
            (
                'duration_s="0" movement_multiplier="1"',
                'duration_s="2" movement_multiplier="1"',
                "zero duration_s",
            ),
            (
                'duration_s="6" movement_multiplier="0.5"',
                'duration_s="6" movement_multiplier="1"',
                "movement_multiplier in 0..1",
            ),
        )
        for old, new, message in cases:
            with self.subTest(message=message):
                with self.assertRaisesRegex(ValueError, message):
                    GameData.from_xml(self.xml.replace(old, new, 1))

    def test_rosters_are_strict(self):
        with self.assertRaisesRegex(ValueError, "action roster must not be empty"):
            start = self.xml.index("<profile")
            end = self.xml.index("</profile>")
            profile = self.xml[start:end]
            for action in (
                "slash",
                "arrow",
                "restore",
                "entangle",
                "stasis",
                "hobble",
                "befriend",
            ):
                profile = profile.replace(f'<action id="{action}" />', "")
            GameData.from_xml(self.xml[:start] + profile + self.xml[end:])
        with self.assertRaisesRegex(ValueError, "requires unassigned skill"):
            GameData.from_xml(
                self.xml.replace('<skill id="ranged" level="1" />', "", 1)
            )

    def test_mob_skills_and_actions_load(self):
        hexer = next(mob for mob in self.data.mobs if mob.id == "hexer")
        self.assertIn(("charm", 2), hexer.skills)
        self.assertIn("befriend", hexer.actions)


if __name__ == "__main__":
    unittest.main()
