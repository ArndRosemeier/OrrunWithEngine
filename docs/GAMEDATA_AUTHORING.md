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
