# GameData authoring

`OrrunGameData.xml` (canonical path `data/OrrunGameData.xml`) is the single authored-data document for the Python authoring foundation. The stdlib-only `tools.gamedata` module provides typed records, strict cross-reference validation, atomic saves, and deterministic XML formatting.

Validate the starter file:

```powershell
python -m tools.gamedata_viewer --validate data/OrrunGameData.xml
```

Open the compact all-section Tkinter viewer/editor:

```powershell
python -m tools.gamedata_viewer data/OrrunGameData.xml
```

Or omit the path; both commands default to `data/OrrunGameData.xml`.

The editor currently presents all sections and saves canonical XML. Domain changes are made through the typed model rather than parsing XML in individual tools. `tools.spell_builder` remains available with its existing JSON API and tests; its package now also exports the GameData types for gradual migration.

## Mob movement and endurance

Every mob record must explicitly author:

- `speed_variance_ratio`: a finite symmetric per-instance speed range from `0.0` through `0.20`;
- `endurance_s`: a finite positive number of seconds available for pursuit or flight.

The runtime samples one multiplier from the authored speed range using the actor's stable spawn seed and keeps that value for the actor instance's lifetime. It does not reroll per frame, engagement, or streaming activation. Base movement speed remains owned by the referenced movement specification. Predators should generally be authored slightly faster than prey but with lower endurance.

Endurance drains only while an actor actually moves in canonical `Pursuing` or `Fleeing` state. Reaching zero does not stop either behavior: the actor continues at the centralized exhausted speed and recovers endurance while outside sprint movement. These transitions and current values are runtime state and must not be authored.

Missing fields, ratios outside `0.0..=0.20`, non-finite numbers, and non-positive endurance are validation errors. The editor, Python model, XML round trip, and Rust loader must preserve and reject the same values without silent defaults.
