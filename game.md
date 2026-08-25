# Orrun game state

This document records the **verified current state** of gameplay systems. It excludes pure world-building (terrain, hydrology, hamlet layout, settlement planning) and reusable engine capabilities unless they directly support play.

Rules for this file:

- Only current state, no plans. Intent and future work live in `MILESTONES.md`.
- Each limitation is stated once, inline, in the section that owns it.
- Claims are anchored to source files so they can be checked.

Status vocabulary:

- **Shipped**: implemented, connected to the playable loop, and expected to stay.
- **POC / experimental**: runs today but is a prototype; expected to be rebuilt or removed.
- **Data only**: authored data model exists; no runtime uses it yet.
- **Absent**: nothing exists.

## Current playable game loop

The current app supports this loop:

1. Enter a deterministic, procedurally generated world.
2. Travel between a hamlet and staged overland or dungeon encounters.
3. Find hostile mobs, select a target, and lock onto it.
4. Fight with melee auto-attack and abilities while managing range, cooldowns, cast time, mana, health, potions, and arrows.
5. Receive combat feedback through target/HUD state, animation, sound, hit flashes, combat log, and failure tells.
6. Kill a hostile, wait for its death pose, and loot its sparkling corpse pile.
7. Take coin and family-based loot into the bag, then equip compatible items.
8. Die, receive Shaken, and return to the last dungeon hatch-mouth shrine.
9. Save and restore player position, combat state, shrine, inventory, and coin.

This is a combat-and-loot vertical slice. The combat bones that drive it are experimental (see below); the loop is real but its internals are scheduled for a rebuild around the GameData model.

## Progression: rebuild in progress

**The legacy player-level/XP system is abolished.** Everything level/XP-shaped that still exists in code (`player.xp`, `xp_to_next`, `award_hostile_xp`, the L1-L20 curve, mob XP values, dungeon XP, per-level HP/mana/attribute/discipline-rank gains, disciplines themselves) is dead code walking: POC-era material that no future system builds on and that will be removed, not extended.

### The replacement design (decided)

Skills are the only progression axis. There is no player level.

- **Skill levels.** Each skill (see `data/OrrunGameData.xml` `<skills>`) has its own level. Skill level determines how well the skill works. Levels are discrete because visible level-ups feel like achievements; slowly creeping values do not.
- **Use trains.** Using an effect trains its skill. There is no success/failure concept for skill use — skills just work, and level governs how well. The same rule extends to resources: getting hit trains HP; spending mana trains mana.
- **Skill XP with non-linear levels.** Each skill accumulates its own skill XP from use. Per-skill level cost grows steeply, so grinding one skill forever yields far less total advancement than training several in parallel. This curve is the only brake — no caps, no anti-grind mechanics.
- **No disciplines.** The Martial/Hunt/Arcane split is removed. What you can do flows from your known skills (player profile in GameData) and equipment, not from a class-like gate.

### Current implementation status

- **Data only:** the GameData model (`data/OrrunGameData.xml`, `orrun/src/gamedata.rs`) already encodes the direction — skills as effect channels, every effect declaring `progression="skill_level"`, actions as effect bundles, player profiles as faction + known skills. No runtime advancement uses it yet.
- **Absent:** skill XP accrual, skill level-up, the non-linear level cost curve, HP/mana training, skill-level-driven effect magnitudes, any player-facing skill UI.
- **Open decisions:** the concrete level-cost curve; how skill level maps to effect magnitude (`level_scale` semantics); starting skill levels for the player profile; how mob difficulty is communicated without a player level to compare against.

## Shipped gameplay features

### Loot

Implemented in `orrun/src/loot.rs`, connected from `orrun/src/world/session.rs`.

- Corpse ground piles generated after the death pose is active; no-clip mobs retain a visible fallback corpse state.
- Deterministic 2-8 coin rolls and mob-specific family drops (wolf, bandit-by-site, orc, orc skull, skeleton warrior/mage/minion). Tribal, tribal veteran, yeti, demon, and blue demon are coin-only.
- Click-corpse loot modal with one-item pickup, coin pickup, and take-all; pile removal when emptied; corpse sparkle presentation.

Limitations: one deterministic family item per eligible mob, fixed coin range, no quality tiers, affixes, comparison flow, vendors, or container loot. Intentionally loot-v1.

### Inventory and equipment

Implemented in `orrun/src/inventory.rs`.

- Four equipment slots (melee, bow, body, charm), eight bag slots, integer coin.
- Item insertion/removal, stacking for potions and arrows, click-to-equip.
- Starter kit: Worn Blade, Worn Bow, Worn Cloth, one Lesser Mend, 40 Thin Arrows.
- Compact family-based item catalog, not a randomized item database.

### Death, recovery, and persistence

Implemented in `orrun/src/combat/types.rs`, `orrun/src/world/session.rs`, `orrun/src/save.rs`.

- On death: slain hold, Shaken (five minutes, -10% outgoing damage), return to last dungeon hatch-mouth shrine, combat reset.
- Save format 3 persists world seed/size, position and facing, HP/mana, Shaken timer, last shrine, full inventory and coin. It also still persists legacy level/XP/attribute/discipline fields, which will be dropped with the progression rebuild (expect a format bump that rejects old saves loudly).
- The save layer validates format and world identity and reports unreadable/incompatible saves loudly. The continent remains a deterministic function of seed and size.

Limitation: no persistent world state — defeated enemies, cleared sites, dungeon completion, loot piles, and NPC state all reset per session.

### Gameplay HUD, input, and feedback

Implemented in `orrun/src/hud.rs`, `orrun/src/main.rs`, `orrun/src/controls.rs`.

- Target frame and target HP bar, hotbar with labels and keybinds, cooldown radial, cast bar, incoming-HP ghost, Shaken icon.
- Combat log, failure toast, corpse loot modal, inventory window, loot sparkle, hostile nameplate support.
- Default combat bindings on 1-8, R, T, G; bindable actions with serialized custom keybindings; reserved movement/interact keys and duplicate-binding resolution.

Limitation: no player-facing progression display of any kind (no skill levels, no resource growth feedback). This is expected — it waits for the progression rebuild.

### Gameplay audio

Combat SFX wired through `orrun/src/world/combat_layer.rs` and `orrun/src/world/ambience.rs`: `combat/swing.wav`, `combat/hit.wav`, `combat/hurt.wav`. Village, forest, river, and ocean ambience is wired. Title, plains, and dungeon audio files exist but are not wired.

## POC / experimental systems

These run in the current app and make the loop playable, but they predate the GameData model and are considered outdated, buggy, or experimental. They will be rebuilt on GameData, not extended.

### Combat core

`orrun/src/combat/types.rs`, `verbs.rs`, `math.rs`, `sheets.rs`, `orrun/src/world/combat_layer.rs`.

What runs today: target lock and cycling, melee auto-attack, ability activation with global/cast/cooldown timing, range and target validation, damage/armor resolution, incoming hostile attacks, interrupts, mana costs and regen, healing, potions, buffs/debuffs, slow/root/stun, Mark/Ward/Second Wind, player death and respawn, hit/hurt/swing presentation, deterministic hostile AI states (idle/alerted/pursuing/attacking/leashing/dead) with aggro radii, leashing, and range stopping.

Why it is POC: ability definitions, rank gates, damage formulas, and the action set (Strike, Bash, Aimed Shot, Pin, Ember, Bind, Mend, Ward, Potion, Mark, Second Wind) are hardcoded around the abolished level/discipline model. The GameData action/effect pipeline (actions as effect bundles with `skill_level` progression) is the intended replacement; technical support such as spell effects already exists, but nothing in live combat consumes it yet.

### Hostile roster and encounters

`orrun/src/combat/catalog.rs`, `orrun/src/combat/sheets.rs`, `orrun/src/world/sites.rs`, `orrun/src/world/dungeon.rs`, plus the `<mobs>` section of `data/OrrunGameData.xml`.

Wolf packs, dungeon skeleton packs, Taken Cairn and Woods Hut bandit sites, and the higher-tier roster (yeti, demon, blue demon, tribal veteran, orc skull) are placed and fightable. Mob stat authoring is migrating to GameData (`WorldCombat` already requires GameData for mob lookup); the legacy sheet values and their XP fields are POC. Combat hostiles remain distinct from ambient fauna, which never participates in combat, loot, or progression.

### Balance simulator

`orrun/src/combat/sim.rs`, `orrun/src/combat/combat_sim.py`. Built entirely around the abolished level/XP model. Unusable for the skill-based design without a rewrite.

## Absent game systems

No implementation exists for:

- Skill-based progression at runtime (see the progression section — data only).
- Quests, objectives, tracking, rewards, or quest persistence.
- Shops, vendors, buying/selling, pricing, merchant inventories.
- Chests or generic containers.
- Persistent enemy/site/dungeon completion state or persistent NPC state.
- Broader NPC gameplay beyond walkers, villagers, and staged encounter presentation.
- A broad economy or itemization system.

## Canonical source files

- Authored game data: `data/OrrunGameData.xml` (loader: `orrun/src/gamedata.rs`, tooling: `tools/gamedata.py`, `tools/gamedata_ui.py`, `tools/gamedata_viewer.py`)
- Plan: `MILESTONES.md` (migration boundaries: `TRANSITION.md`)
- Combat (POC): `orrun/src/combat/types.rs`, `verbs.rs`, `math.rs`, `sheets.rs`, `sim.rs`
- Hostile presentation/session: `orrun/src/world/combat_layer.rs`, `orrun/src/world/session.rs`
- Loot: `orrun/src/loot.rs`
- Inventory: `orrun/src/inventory.rs`
- Persistence: `orrun/src/save.rs`
- HUD and controls: `orrun/src/hud.rs`, `orrun/src/controls.rs`, `orrun/src/main.rs`
- Audio: `orrun/src/world/ambience.rs`, `orrun/assets/audio/ATTRIBUTION.md`

Last consolidated: 2026-08-25.
