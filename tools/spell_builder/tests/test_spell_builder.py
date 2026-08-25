import unittest
from pathlib import Path
from tools.spell_builder.catalogue import EFFECTS
from tools.spell_builder.io import write_catalog
from tools.spell_builder.model import Spell, SpellEffect, TargetMode
from tools.spell_builder.templates import starter_templates
class SpellBuilderTests(unittest.TestCase):
 def test_registry_covers_initial_families(self): self.assertEqual({d.family.value for d in EFFECTS.values()},{"damage","delivery","control","restoration","utility"})
 def test_cost_is_deterministic(self):
  spell=Spell("x","X",target=TargetMode.HOSTILE,effects=[SpellEffect("fire_damage",{"magnitude":2,"range":10})]); self.assertEqual(spell.cost(EFFECTS),spell.cost(EFFECTS))
 def test_invalid_target_and_delivery_are_loud(self):
  errors=Spell("x","X",target=TargetMode.FRIENDLY,delivery="projectile",effects=[SpellEffect("fire_damage")]).validate(EFFECTS); self.assertTrue(any("cannot target friendly" in e for e in errors))
 def test_round_trip(self):
  spell=starter_templates()["fire_bolt"]; self.assertEqual(Spell.from_dict(spell.to_dict()).to_dict(),spell.to_dict())
 def test_catalog_duplicate_detection(self):
  import tempfile
  with tempfile.TemporaryDirectory() as d:
   p=Path(d); spell=starter_templates()["fire_bolt"]; spell.save(p/"one.json"); spell.save(p/"two.json")
   with self.assertRaisesRegex(ValueError,"duplicate"): write_catalog(p,p/"catalog.json")
 def test_templates_are_valid(self):
  for spell in starter_templates().values(): self.assertEqual(spell.validate(EFFECTS),[],spell.id)
if __name__ == "__main__": unittest.main()