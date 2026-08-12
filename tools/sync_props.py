"""Copy generated prop meshes from the Asset Lab into this repo.

The Asset Lab (`C:\\Projekte\\AssetGenerator`) treats its specs as the source of
truth and gitignores its output, so the game vendors the meshes it actually
scatters. Run this after regenerating an asset there.

    python tools/sync_props.py [path-to-asset-lab]

Only the ids listed below are copied: everything under `assets/props` is loaded
at startup, so an unused mesh would cost upload time for nothing.

Baked texture maps are dropped on the way in. The prop pipeline shades from
vertex colour alone and names stone itself, so an embedded albedo/normal set is
several hundred kilobytes of repository and load time that nothing can read.
"""

from __future__ import annotations

import json
import struct
import sys
from pathlib import Path

DEFAULT_LAB = Path("C:/Projekte/AssetGenerator")

WANTED: dict[str, tuple[str, ...]] = {
    "grass": (
        "grass_tuft_lush",
        "grass_tuft_dry",
        "grass_tuft_sparse",
    ),
    "rocks": (
        "rock_boulder_round",
        "rock_chunk_angular",
        "rock_cobble_worn",
        "rock_dark_basalt",
        "rock_fieldstone_low",
        "rock_riverstone_flat",
        "rock_slab_veined",
        "rock_talus_shard",
    ),
    "trees": (
        "pine_alpine_short",
        "pine_ponderosa_stylized",
        "pine_spruce_narrow",
        "pine_young_sapling",
    ),
    "reeds": (
        "reed_tall",
        "reed_clump",
    ),
    "bushes": (
        "bush_round_lush",
        "bush_berries",
        "bush_dry",
        "bush_broad_lush",
        "bush_tall_open",
        "bush_alpine_low",
        "bush_sparse",
    ),
}


_TEXTURE_KEYS = (
    "baseColorTexture",
    "metallicRoughnessTexture",
    "normalTexture",
    "occlusionTexture",
    "emissiveTexture",
)


def _read_glb(path: Path) -> tuple[dict, bytes]:
    blob = path.read_bytes()
    magic, _version, _length = struct.unpack("<4sII", blob[:12])
    if magic != b"glTF":
        raise ValueError(f"{path} is not a binary glTF")
    offset = 12
    doc: dict | None = None
    binary = b""
    while offset < len(blob):
        size, kind = struct.unpack("<I4s", blob[offset : offset + 8])
        chunk = blob[offset + 8 : offset + 8 + size]
        if kind == b"JSON":
            doc = json.loads(chunk)
        elif kind == b"BIN\x00":
            binary = chunk
        offset += 8 + size + (-size % 4)
    if doc is None:
        raise ValueError(f"{path} has no JSON chunk")
    return doc, binary


def _write_glb(path: Path, doc: dict, binary: bytes) -> None:
    json_chunk = json.dumps(doc, separators=(",", ":")).encode("utf-8")
    json_chunk += b" " * (-len(json_chunk) % 4)
    binary += b"\x00" * (-len(binary) % 4)
    total = 12 + 8 + len(json_chunk) + (8 + len(binary) if binary else 0)
    out = bytearray(struct.pack("<4sII", b"glTF", 2, total))
    out += struct.pack("<I4s", len(json_chunk), b"JSON") + json_chunk
    if binary:
        out += struct.pack("<I4s", len(binary), b"BIN\x00") + binary
    path.write_bytes(bytes(out))


def strip_textures(source: Path, target: Path) -> None:
    """Copy a glb, dropping images and any buffer data only they used."""
    doc, binary = _read_glb(source)
    for material in doc.get("materials", []):
        pbr = material.get("pbrMetallicRoughness", {})
        for key in _TEXTURE_KEYS:
            material.pop(key, None)
            pbr.pop(key, None)
    for key in ("images", "textures", "samplers"):
        doc.pop(key, None)

    views = doc.get("bufferViews", [])
    kept: dict[int, int] = {}
    packed = bytearray()
    for accessor in doc.get("accessors", []):
        old = accessor.get("bufferView")
        if old is None:
            continue
        if old not in kept:
            view = dict(views[old])
            start = view.get("byteOffset", 0)
            length = view["byteLength"]
            packed += b"\x00" * (-len(packed) % 4)
            view["byteOffset"] = len(packed)
            packed += binary[start : start + length]
            kept[old] = len(kept)
            doc.setdefault("_kept", []).append(view)
        accessor["bufferView"] = kept[old]

    doc["bufferViews"] = doc.pop("_kept", [])
    doc["buffers"] = [{"byteLength": len(packed)}]
    _write_glb(target, doc, bytes(packed))


def main() -> int:
    lab = Path(sys.argv[1]) if len(sys.argv) > 1 else DEFAULT_LAB
    source = lab / "assets" / "out"
    if not source.is_dir():
        print(f"no Asset Lab output at {source}; run its regenerate first", file=sys.stderr)
        return 2

    target_root = Path(__file__).resolve().parents[1] / "orrun" / "assets" / "props"
    copied = 0
    missing: list[str] = []
    for folder, ids in WANTED.items():
        target = target_root / folder
        target.mkdir(parents=True, exist_ok=True)
        for asset_id in ids:
            glb = source / f"{asset_id}.glb"
            if not glb.is_file():
                missing.append(asset_id)
                continue
            strip_textures(glb, target / glb.name)
            copied += 1

    print(f"copied {copied} meshes into {target_root}")
    if missing:
        print("missing from the Asset Lab: " + ", ".join(missing), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
