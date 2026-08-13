"""Copy generated prop meshes from the Asset Lab into this repo.

The Asset Lab (`C:\\Projekte\\AssetGenerator`) treats its specs as the source of
truth and gitignores its output, so the game vendors the meshes it actually
scatters and the medieval kit pieces hamlets assemble. Run this after
regenerating an asset there.

    python tools/sync_props.py [path-to-asset-lab]

Only the ids listed below are copied: everything under `assets/props` is loaded
at startup, so an unused mesh would cost upload time for nothing. Kit pieces
land under `assets/kit/medieval` and `assets/kit/castle` and are loaded by name.

Most scatter props shade from vertex colour / material factor, so their maps
are dropped on the way in. Rocks bake veins and grit into albedo — the engine
samples that map — so those copies keep `baseColorTexture`. Kit cells do the
same. Unused normal/roughness maps are dropped either way.
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

KIT: dict[str, tuple[str, ...]] = {
    "medieval": (
        "med_wall",
        "med_wall_b",
        "med_window",
        "med_window_b",
        "med_window_c",
        "med_door",
        "med_door_b",
        "med_corner",
        "med_wall_jetty",
        "med_wall_b_jetty",
        "med_window_jetty",
        "med_window_b_jetty",
        "med_window_c_jetty",
        "med_corner_jetty",
        "med_roof",
        "med_roof_b",
        "med_chimney",
        "med_floor",
        "med_plinth",
        "med_plinth_b",
    ),
    "castle": (
        "castle_curtain",
        "castle_loop",
        "castle_gate",
        "castle_tower",
        "castle_turret",
        "castle_battlement",
        "castle_plinth",
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


def _keep_albedo_only(doc: dict) -> None:
    """Drop every map the engine cannot sample. Keep baseColorTexture."""
    old_textures = doc.get("textures", [])
    old_images = doc.get("images", [])
    old_samplers = doc.get("samplers", [])
    used_tex: dict[int, int] = {}
    used_img: dict[int, int] = {}
    used_samp: dict[int, int] = {}

    for material in doc.get("materials", []):
        pbr = material.get("pbrMetallicRoughness", {})
        for key in _TEXTURE_KEYS:
            if key == "baseColorTexture":
                continue
            material.pop(key, None)
            pbr.pop(key, None)
        info = pbr.get("baseColorTexture")
        if info is None:
            continue
        old_tex = info["index"]
        if old_tex not in used_tex:
            used_tex[old_tex] = len(used_tex)
        info["index"] = used_tex[old_tex]

    new_textures = []
    for old_tex, _new_tex in sorted(used_tex.items(), key=lambda item: item[1]):
        tex = dict(old_textures[old_tex])
        if "source" in tex:
            old_img = tex["source"]
            if old_img not in used_img:
                used_img[old_img] = len(used_img)
            tex["source"] = used_img[old_img]
        if "sampler" in tex:
            old_samp = tex["sampler"]
            if old_samp not in used_samp:
                used_samp[old_samp] = len(used_samp)
            tex["sampler"] = used_samp[old_samp]
        new_textures.append(tex)

    new_images = [
        dict(old_images[old])
        for old, _ in sorted(used_img.items(), key=lambda item: item[1])
    ]
    new_samplers = [
        old_samplers[old]
        for old, _ in sorted(used_samp.items(), key=lambda item: item[1])
    ]

    if new_images:
        doc["images"] = new_images
        doc["textures"] = new_textures
        if new_samplers:
            doc["samplers"] = new_samplers
        else:
            doc.pop("samplers", None)
    else:
        for key in ("images", "textures", "samplers"):
            doc.pop(key, None)


def copy_glb(source: Path, target: Path, *, keep_albedo: bool) -> None:
    """Copy a glb. Rocks and kit cells keep albedo; other scatter props drop maps."""
    doc, binary = _read_glb(source)
    if keep_albedo:
        _keep_albedo_only(doc)
    else:
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

    def keep_view(old: int) -> int:
        if old not in kept:
            view = dict(views[old])
            start = view.get("byteOffset", 0)
            length = view["byteLength"]
            packed.extend(b"\x00" * (-len(packed) % 4))
            view["byteOffset"] = len(packed)
            packed.extend(binary[start : start + length])
            kept[old] = len(kept)
            doc.setdefault("_kept", []).append(view)
        return kept[old]

    for accessor in doc.get("accessors", []):
        old = accessor.get("bufferView")
        if old is None:
            continue
        accessor["bufferView"] = keep_view(old)
    for image in doc.get("images", []):
        old = image.get("bufferView")
        if old is None:
            continue
        image["bufferView"] = keep_view(old)

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
    kit_root = Path(__file__).resolve().parents[1] / "orrun" / "assets" / "kit"
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
            copy_glb(glb, target / glb.name, keep_albedo=folder == "rocks")
            copied += 1
    for folder, ids in KIT.items():
        target = kit_root / folder
        target.mkdir(parents=True, exist_ok=True)
        for asset_id in ids:
            glb = source / f"{asset_id}.glb"
            if not glb.is_file():
                missing.append(asset_id)
                continue
            copy_glb(glb, target / glb.name, keep_albedo=True)
            copied += 1

    print(f"copied {copied} meshes into {target_root} and {kit_root}")
    if missing:
        print("missing from the Asset Lab: " + ", ".join(missing), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
