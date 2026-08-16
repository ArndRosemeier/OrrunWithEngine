# Human assets

Source: Asset Lab Character Studio export
(`C:\Projekte\AssetGenerator\character_studio\assets\humans`)

Pipeline: MPFB on the MakeHuman basemesh, `game_engine` skeleton (53 bones,
anatomy proxy `pelvis`), 28 face morphs (`face_*__pos` / `face_*__neg`), fitted
low-poly eyes. Clothes, hair, eyebrows and shoes are separate pieces bound to
the same rest pose.

Rebuild there with `python tools/sync_character_studio_assets.py`, then pull
into this repo with `python tools/sync_humans.py`.

Do not edit these meshes here.

## License

MakeHuman / MPFB core assets, system clothes, hair, eyebrows, shoes, and these
exports: **CC0 / Public Domain**.

- https://static.makehumancommunity.org/about/license.html
- https://static.makehumancommunity.org/mpfb/faq/use_in_closed_source.html

Vendored glbs live under `orrun/assets/humans/` (`{sex}_base.glb`, dressed
bodies, `pieces/`). The runtime catalogue is `wardrobe.json`.

The Asset Lab wardrobe also lists monk / viking / germanic / medieval-dress
outfits. Those glbs were not in the Character Studio export at copy time
(community mhclo packs live under `makehuman_extra_assets/` after
`fetch_medieval_clothes.py`). Re-export there, then re-run `sync_humans.py`.
