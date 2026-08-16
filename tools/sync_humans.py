"""Copy Character Studio humans from the Asset Lab into this repo.

The Asset Lab (`C:\\Projekte\\AssetGenerator`) gitignores the exported glbs and
rebuilds them with `python tools/sync_character_studio_assets.py`. This game
vendors the meshes so they are here when we wire villagers later.

    python tools/sync_humans.py [path-to-asset-lab]

Copies every `.glb` under `character_studio/assets/humans` (bases, dressed
bodies, wardrobe pieces). Textures stay embedded. Godot `.import` sidecars and
extracted PNGs are dropped. `wardrobe.json` is rewritten to relative paths and
only keeps entries whose files actually exist — skipped ids are printed.
"""

from __future__ import annotations

import json
import shutil
import sys
from pathlib import Path

DEFAULT_LAB = Path("C:/Projekte/AssetGenerator")
GODOT_PREFIX = "res://assets/humans/"


def _rel_path(raw: str) -> str:
    if raw.startswith(GODOT_PREFIX):
        return raw[len(GODOT_PREFIX) :]
    if raw.startswith("res://"):
        raise ValueError(f"unexpected Godot path {raw!r}")
    return raw.replace("\\", "/")


def _rewrite_wardrobe(doc: dict, source: Path) -> tuple[dict, list[str]]:
    skipped: list[str] = []
    bodies = []
    for body in doc["bodies"]:
        sex = body["sex"]
        nude = _rel_path(body["nude"])
        if not (source / nude).is_file():
            raise FileNotFoundError(f"nude body missing: {source / nude}")
        dressed: dict[str, str] = {}
        for suit_id, raw in body.get("dressed", {}).items():
            rel = _rel_path(raw)
            if (source / rel).is_file():
                dressed[suit_id] = rel
            else:
                skipped.append(f"{sex}_dressed_{suit_id}")
        bodies.append({"sex": sex, "nude": nude, "dressed": dressed})

    items = []
    for item in doc.get("items", []):
        rel = _rel_path(item["path"])
        if (source / rel).is_file():
            entry = dict(item)
            entry["path"] = rel
            items.append(entry)
        else:
            skipped.append(f"{item['sex']}_{item['id']}")

    return (
        {"slots": list(doc["slots"]), "bodies": bodies, "items": items},
        skipped,
    )


def main() -> int:
    lab = Path(sys.argv[1]) if len(sys.argv) > 1 else DEFAULT_LAB
    source = lab / "character_studio" / "assets" / "humans"
    wardrobe_src = source / "wardrobe.json"
    if not wardrobe_src.is_file():
        print(f"no Character Studio wardrobe at {wardrobe_src}", file=sys.stderr)
        return 2

    target = Path(__file__).resolve().parents[1] / "orrun" / "assets" / "humans"
    target.mkdir(parents=True, exist_ok=True)

    wanted: set[Path] = set()
    copied = 0
    for glb in sorted(source.rglob("*.glb")):
        rel = glb.relative_to(source)
        dest = target / rel
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(glb, dest)
        wanted.add(dest.resolve())
        copied += 1

    for stale in target.rglob("*.glb"):
        if stale.resolve() not in wanted:
            stale.unlink()

    if copied == 0:
        print(f"no human glbs under {source}", file=sys.stderr)
        return 1

    doc = json.loads(wardrobe_src.read_text(encoding="utf-8"))
    wardrobe, skipped = _rewrite_wardrobe(doc, source)
    (target / "wardrobe.json").write_text(
        json.dumps(wardrobe, indent=2) + "\n",
        encoding="utf-8",
    )

    print(f"copied {copied} meshes into {target}")
    if skipped:
        print(
            "wardrobe skipped (not among exported glbs): " + ", ".join(skipped),
            file=sys.stderr,
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
