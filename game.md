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

This is a combat-and-loot vertical slice. Every live player and mob combat action is selected from GameData and executes through the canonical resolver. Root, hold, snare, and charm use distinct typed runtime status semantics.

## Skill progression runtime

**The legacy player-level/XP system is abolished and removed from live combat.** Skills plus HP and mana proficiencies are the only progression axes.

### The replacement design (decided)

Skills are the only progression axis. There is no player level.

- **Skill levels.** Each skill (see `data/OrrunGameData.xml` `<skills>`) has its own level. Skill level determines how well the skill works. Levels are discrete because visible level-ups feel like achievements; slowly creeping values do not.
- **Use trains.** Using an effect trains its skill. There is no success/failure concept for skill use — skills just work, and level governs how well. The same rule extends to resources: getting hit trains HP; spending mana trains mana.
- **Skill XP with non-linear levels.** Each skill accumulates its own skill XP from use. Per-skill level cost grows steeply, so grinding one skill forever yields far less total advancement than training several in parallel. This curve is the only brake — no caps, no anti-grind mechanics.
- **No disciplines.** The Martial/Hunt/Arcane split is removed. What you can do flows from your known skills (player profile in GameData) and equipment, not from a class-like gate.

### Current implementation status

- **Canonical runtime data:** `data/OrrunGameData.xml` and `orrun/src/gamedata.rs` define skills, effect bundles, action assignments, factions, player profiles, mobs, movement, and starting skill levels. Python/Rust A/B loading and validation pass against the same canonical data.
- **Progression owner:** `orrun/src/progression.rs` holds per-skill integer level + exact XP, HP and mana as trainable proficiencies, typed training operations (`record_effect_use`, `record_damage_taken`, `record_mana_spent`), typed level-up events, and a strictly increasing non-linear level-cost curve. Provisional balance values remain centralized in `progression::balance`.
- **Live training:** canonical effect execution trains the referenced action skill; incoming applied damage trains HP; actual mana spend trains mana. The verified wolf slice exercises all three paths through Strike, Fire Bolt, Mend, and the wolf attack.
- **Visible and persistent progression:** the Skills window reports every known skill plus HP and mana with exact level progress. Typed level-up events drive restrained notices. Save format 4 round-trips skill/resource progression and current resources through the canonical model; formats 1-3 are rejected as incompatible.
- **Open decisions:** how mob difficulty is communicated without a player level to compare against.

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

### Death and recovery

Implemented in `orrun/src/combat/types.rs` and `orrun/src/world/session.rs`.

- On death: slain hold, Shaken (five minutes, -10% outgoing damage), return to last dungeon hatch-mouth shrine, combat reset. This behavior remains separately verified; the combined M3 wolf/animal report did not add a new death/respawn hook.

Limitation: no persistent world state — defeated enemies, cleared sites, dungeon completion, loot piles, and NPC state all reset per session.

### Gameplay HUD, input, and feedback

Implemented in `orrun/src/hud.rs`, `orrun/src/main.rs`, `orrun/src/controls.rs`.

- Target frame and target HP bar, hotbar with labels and keybinds, cooldown radial, cast bar, incoming-HP ghost, Shaken icon.
- Combat log, failure toast, corpse loot modal, inventory window, loot sparkle, hostile nameplate support.
- Default combat bindings on 1-8, R, T, G; bindable actions with serialized custom keybindings; reserved movement/interact keys and duplicate-binding resolution.

The Skills window shows known skills, HP, and mana with level and exact progress to the next level. Typed level-up events produce player-facing notices without inferring progression in the HUD.

### Gameplay audio

Combat SFX wired through `orrun/src/world/combat_layer.rs` and `orrun/src/world/ambience.rs`: `combat/swing.wav`, `combat/hit.wav`, `combat/hurt.wav`. Village, forest, river, and ocean ambience is wired. Title, plains, and dungeon audio files exist but are not wired.

### Canonical action vertical slice

`orrun/src/resolution.rs`, `orrun/src/combat/verbs.rs`, `orrun/src/world/session.rs`, and `orrun/src/world/combat_layer.rs` execute the first live GameData-backed slice.

- Stable action IDs resolve authored assignments and damage/heal effects through one typed player/mob path; target geometry, mitigation, resource mutation, aggression, death, and progression events are centralized.
- The live player and complete authored mob roster use the same resolver. Cooldown/cast state, targeting, range, and HUD labels consume action data without action-specific behavior branches.
- Combined release verification passed `m3_wolf` with no failure or skip: incoming damage 8, Mend healing 8, Strike damage 8, Fire Bolt damage 14, skill/HP/mana training, death presentation, and one-time corpse transfer containing coin and an item.
- Full deterministic headless resolver coverage and canonical GameData Python/Rust A/B coverage pass.

### Canonical animal actors

Fauna spawning and animated entities remain in `orrun/src/world/fauna.rs`; once engaged, canonical actor state in `orrun/src/combat/types.rs` owns gameplay decisions and synchronizes presentation.

- GameData authors linked animal species as predator/active, prey/passive, or citizen/passive actors with factions, movement specs, bounded deterministic speed variance, and endurance.
- Fixed staggered perception covers front/side/rear vision, angle-independent hearing, and refreshed awareness. Active hostile actors pursue; passive actors flee only detected active hostile non-neutral threats; damage immediately cancels flight and establishes retaliation.
- Endurance drains only during actual pursuit/flight, exhausted actors continue at the centralized slower speed, and endurance recovers outside sprint movement. Canonical ownership includes threat, heading, locomotion, leash/awareness chase termination, death, and loot; fauna performs no catch/kill decision.
- Combined release verification passed `m3_animals` with no failure or skip, including front/rear heading and locomotion synchronization plus immediate retaliation. Deterministic headless coverage exercises perception, pursuit/flight exclusions, retaliation, speed variance, endurance, and chase termination.

## POC / experimental systems

These run in the current app and make the loop playable, but they predate the GameData model and are considered outdated, buggy, or experimental. They will be rebuilt on GameData, not extended.

### Combat core

`orrun/src/combat/types.rs`, `verbs.rs`, `math.rs`, `sheets.rs`, `orrun/src/world/combat_layer.rs`.

What runs today: target lock and cycling, melee auto-attack, ability activation with global/cast/cooldown timing, range and target validation, damage/armor resolution, incoming hostile attacks, interrupts, mana costs and regen, healing, potions, buffs/debuffs, slow/root/stun, Mark/Ward/Second Wind, player death and respawn, hit/hurt/swing presentation, deterministic hostile AI states (idle/alerted/pursuing/attacking/leashing/dead) with aggro radii, leashing, and range stopping.

Why it is POC: non-migrated ability definitions, rank gates, formulas, and paths for Bash, Aimed Shot, Pin, Bind, Ward, Potion, Mark, Second Wind, and other legacy behavior remain hardcoded around the abolished level/discipline model. Strike, Fire Bolt, Mend, and one hostile basic attack have crossed to GameData; the remaining legacy paths are retained only until M5/M6 migration and removal.

### Hostile roster and encounters

`orrun/src/combat/catalog.rs`, `orrun/src/world/sites.rs`, `orrun/src/world/dungeon.rs`, plus the `<mobs>` section of `data/OrrunGameData.xml`.

Wolf packs, dungeon skeleton packs, Taken Cairn and Woods Hut bandit sites, and the authored higher-tier roster are placed through canonical GameData mob definitions. Faction/mode policy, perception, movement, retaliation, action selection, death, and loot are authoritative in the shared combat arena.

### Headless combat harness

`orrun/src/bin/playtester.rs` executes the production `WorldCombat` runtime from GameData. The `m5_actions` hook verifies canonical action/status semantics, cooldowns, and full authored mob-roster selection and writes a JSON report.

## Absent game systems

No implementation exists for:

- Quests, objectives, tracking, rewards, or quest persistence.
- Shops, vendors, buying/selling, pricing, merchant inventories.
- Chests or generic containers.
- Persistent enemy/site/dungeon completion state or persistent NPC state.
- Broader NPC gameplay beyond walkers, villagers, and staged encounter presentation.
- A broad economy or itemization system.

## Canonical source files

- Authored game data: `data/OrrunGameData.xml` (loader: `orrun/src/gamedata.rs`, tooling: `tools/gamedata.py`, `tools/gamedata_ui.py`, `tools/gamedata_viewer.py`)
- Progression: `orrun/src/progression.rs`
- Canonical action resolution: `orrun/src/resolution.rs`, `orrun/src/combat/verbs.rs`
- Canonical animal ownership/presentation: `orrun/src/combat/types.rs`, `orrun/src/world/fauna.rs`
- Plan: `MILESTONES.md` (migration boundaries: `TRANSITION.md`)
- Combat runtime: `orrun/src/combat/types.rs`, `orrun/src/combat/actions.rs`, `orrun/src/resolution.rs`
- Hostile presentation/session: `orrun/src/world/combat_layer.rs`, `orrun/src/world/session.rs`
- Loot: `orrun/src/loot.rs`
- Inventory: `orrun/src/inventory.rs`
- Persistence: `orrun/src/save.rs`
- HUD and controls: `orrun/src/hud.rs`, `orrun/src/controls.rs`, `orrun/src/main.rs`
- Audio: `orrun/src/world/ambience.rs`, `orrun/assets/audio/ATTRIBUTION.md`

Last consolidated: 2026-08-27.
