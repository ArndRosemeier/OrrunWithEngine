# Spell authoring

`tools.spell_builder` is an authoring-only Python 3.10 package. It defines strict, versioned spell JSON for future Orrun runtime consumption; it does not modify casting, combat, or runtime VFX.

## CLI

From the repository root:

```powershell
python -m tools.spell_builder list-effects
python -m tools.spell_builder templates tools/spell_builder/data/spells
python -m tools.spell_builder validate tools/spell_builder/data/spells/fire_bolt.json
python -m tools.spell_builder catalog tools/spell_builder/data/spells tools/spell_builder/data/catalog.json
python -m tools.spell_builder ui
```

Each spell has `schema_version`, an identifier, display text, a target mode, a delivery mode, typed effect records, and numeric shaping parameters. Invalid target/delivery pairs, unknown effects, unsupported parameters, and non-positive values are rejected. Prices are deterministic and explainable through `Spell.cost_breakdown`.

The catalog uses atomic replacement and contains only validated spell IDs, names, source filenames, and calculated costs. Future Orrun integration should resolve catalog entries and translate each effect into centralized combat/event commands; the authoring schema is intentionally not a runtime implementation.