from pathlib import Path
import json
from .model import Spell
from .catalogue import EFFECTS

def validate_file(path: Path) -> list[str]:
    try: spell = Spell.load(path)
    except (OSError, ValueError, KeyError, TypeError, json.JSONDecodeError) as exc: return [str(exc)]
    return spell.validate(EFFECTS)

def write_catalog(spells_dir: Path, catalog_path: Path) -> None:
    entries=[]; seen_ids=set()
    for path in sorted(spells_dir.glob("*.json")):
        spell=Spell.load(path)
        if spell.id in seen_ids: raise ValueError(f"{path}: duplicate spell id {spell.id!r}")
        seen_ids.add(spell.id)
        errors=spell.validate(EFFECTS)
        if errors: raise ValueError(f"{path}: " + "; ".join(errors))
        entries.append({"id": spell.id, "name": spell.name, "path": path.name, "cost": spell.cost(EFFECTS)})
    temporary=catalog_path.with_suffix(catalog_path.suffix+".tmp"); catalog_path.parent.mkdir(parents=True, exist_ok=True); temporary.write_text(json.dumps({"schema_version":1,"spells":entries},indent=2)+"\n",encoding="utf-8"); temporary.replace(catalog_path)