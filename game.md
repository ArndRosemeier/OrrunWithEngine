# Orrun game state

This document tracks actual game features and game-loop work. It deliberately excludes pure world-building systems such as terrain generation, hydrology, hamlet layout, procedural settlement planning, and reusable engine capabilities unless they directly support play.

Status vocabulary:

- **Shipped**: implemented in the current app and connected to the playable loop.
- **Partial**: meaningful runtime or data support exists, but the feature is incomplete or not fully player-facing.
- **Planned / held**: recorded in the living plan, but intentionally deferred or blocked.
- **Absent**: no gameplay system is currently present.

The source of truth for milestone intent is `MILESTONES.md`. This file is the consolidated gameplay view of that plan and the current code state.

## Current playable game loop

The current app supports this loop:

1. Enter a deterministic, procedurally generated world.
2. Travel between a hamlet and staged overland or dungeon encounters.
3. Find hostile mobs, select a target, and lock onto it.
4. Fight with melee auto-attack and rank-gated abilities while managing range, cooldowns, cast time, mana, health, potions, and arrows.
5. Receive combat feedback through target/HUD state, animation, sound effects, hit flashes, combat log, and failure tells.
6. Kill a hostile, wait for its death pose, and open its sparkling corpse pile.
7. Take coin and family-based loot into the bag, then equip compatible items.
8. Die, receive Shaken, and return to the last dungeon hatch-mouth shrine.
9. Save and restore the player position, combat state, progression data, shrine, inventory, and coin.

This is a real combat-and-loot vertical slice, not yet a complete RPG progression or economy loop.

## Shipped gameplay features

### Hostile mobs and encounters

Implemented in `orrun/src/combat/catalog.rs`, `orrun/src/combat/sheets.rs`, `orrun/src/world/combat_layer.rs`, `orrun/src/world/sites.rs`, and `orrun/src/world/dungeon.rs`.

The authored combat roster currently includes:

- Wolf / wolf-spider
- Orc
- Tribal
- Bandit
- Orc skull
- Skeleton warrior
- Skeleton minion
- Skeleton mage
- Yeti
- Demon
- Blue demon
- Tribal veteran

`WorldHostile` carries live state including HP, armor, damage, attack interval, reach, level, model entity, alive/dead state, stun, slow, and root effects.

Encounter placement currently includes:

- Readable level-1 wolf packs.
- T2+ dungeon skeleton packs, including warrior/minion groups and a mage pack.
- Taken Cairn bandit sites.
- Woods Hut bandit sites.
- Higher-level overland/fixture combat through the current L10 roster.

Combat hostiles are distinct from ambient fauna. Fauna is world life for show and does not participate in combat, loot, or progression.

### Combat and fights

Implemented primarily in `orrun/src/combat/types.rs`, `orrun/src/combat/verbs.rs`, `orrun/src/combat/math.rs`, `orrun/src/combat/log.rs`, and `orrun/src/world/combat_layer.rs`.

Shipped capabilities:

- Target selection, click-to-lock, target cycling, and lock ring.
- Melee auto-attack.
- Ability activation with global/cast/cooldown timing.
- Rank gating and explicit failure tells.
- Range and target validation.
- Damage and armor resolution.
- Incoming hostile attacks.
- Cast times, interrupts, mana costs, mana regeneration, and health regeneration.
- Healing, potions, buffs/debuffs, slow/root/stun behavior, and Mark/Ward/Second Wind state.
- Player death, a slain hold, respawn at the last shrine, and combat reset.
- Hit, hurt, and swing presentation; death poses and combat sound events.
- Combat event log and headless simulator for balance testing.

Current action set:

- Strike
- Bash
- Aimed Shot
- Pin
- Ember
- Bind
- Mend
- Ward
- Potion
- Mark
- Second Wind

The combat bible in `MILESTONES.md` fixes key baseline values including trash baseline 5.2, intentionally starved in-combat mana, Ember at 3 mana with a 1.2-second cast, and potion healing of 40.

### Progression data and player stats

Implemented data and formulas live in `orrun/src/combat/mod.rs`, `orrun/src/combat/sheets.rs`, `orrun/src/combat/math.rs`, and `orrun/src/combat/sim.rs`.

The current model contains:

- Player level and XP fields.
- Martial, hunt, and arcane disciplines/ranks.
- Attributes and focused attribute points.
- HP and mana scaling by level.
- Discipline points per level.
- Skill XP and skill caps.
- Ability damage/healing formulas.
- Mob XP values.
- L1-L20 XP curve data and leveling-path reporting.
- Dungeon XP and clear-bonus data.
- A numeric combat simulator that models progression-related values.

Current intended progression numbers include +12 HP and +6 mana per level, +2 focused attribute points per level, +1 discipline rank per level, and skill caps of level x 5.

Important current-state limitation: the data/model layer is present, but the live application does not clearly expose a complete player-facing level-up flow. In particular, the current source review did not identify a confirmed runtime path that awards mob XP on death and applies level-up/stat changes. Treat live XP gain and level advancement as **not verified / partial**, even though XP fields, curves, and simulator support exist.

### Loot

Implemented in `orrun/src/loot.rs` and connected from `orrun/src/world/session.rs`.

Shipped loot-v1 behavior:

- Corpse ground piles generated after the death pose is active.
- Deterministic 2-8 coin rolls.
- Mob-specific family drops.
- Site-specific bandit drops.
- Click-corpse loot modal.
- One-item pickup, coin pickup, and take-all.
- Loot pile removal after it is emptied.
- Corpse sparkle presentation.

Current family drops include:

- Wolf -> Hide Wrap
- Bandit at a cairn -> Cairn Blade
- Bandit at a hut -> Hut Wrap
- Orc -> Orc Club
- Orc skull -> Ember Chip
- Skeleton warrior -> Bone Plate
- Skeleton mage -> Staff Splinter
- Skeleton minion -> Lesser Mend

Tribal, tribal veteran, yeti, demon, and blue demon currently produce coin-only drops unless a playtester forces a visible family item.

### Inventory and equipment

Implemented in `orrun/src/inventory.rs`.

The current inventory has:

- Four equipment slots: melee, bow, body, charm.
- Eight bag slots.
- Integer coin storage.
- Item insertion and removal.
- Stacking for potions and arrows.
- Click-to-equip interaction.
- Starter kit: Worn Blade, Worn Bow, Worn Cloth, one Lesser Mend, and 40 Thin Arrows.
- Compact family-based item catalog rather than a large randomized item database.

Current item families include weapons, clothing/materials, potions, arrows, coin, and combat resources. The intended scope is still loot-v1, not a full itemization system.

### Death, recovery, and persistence

Implemented in `orrun/src/combat/types.rs`, `orrun/src/world/session.rs`, and `orrun/src/save.rs`.

On player death:

- The player enters a slain state for a short hold.
- Shaken is applied for five minutes and reduces outgoing damage by 10%.
- The player returns to the last dungeon hatch-mouth shrine.
- Combat is cleared/rearmed.

Save format 3 persists:

- World seed and size.
- Player X/Z position and facing.
- Level and XP fields.
- HP and mana.
- Attributes and discipline ranks.
- Shaken timer.
- Last dungeon shrine.
- Full inventory and coin.

The save layer validates format/world identity, migrates older supported formats, and reports unreadable/incompatible saves loudly. The continent itself remains a deterministic function of seed and size.

### Gameplay HUD, input, and feedback

Implemented in `orrun/src/hud.rs`, `orrun/src/main.rs`, and `orrun/src/controls.rs`.

Shipped HUD and controls:

- Target frame and target HP bar.
- Hotbar with action labels and keybinds.
- Cooldown radial sweep.
- Cast bar.
- Incoming-HP ghost.
- Shaken status icon.
- Combat log and failure toast.
- Corpse loot modal.
- Inventory/bag window.
- Loot sparkle visualization.
- Hostile nameplate drawing support.
- Default combat bindings on 1-8, R, T, and G.
- Bindable combat actions with serialized custom keybindings.
- Reserved movement/interact keys and duplicate-binding resolution.

### Gameplay audio

Combat SFX are wired through `orrun/src/world/combat_layer.rs` and `orrun/src/world/ambience.rs`:

- `combat/swing.wav`
- `combat/hit.wav`
- `combat/hurt.wav`

Village, forest, river, and ocean ambience is also wired. The title, plains, and dungeon audio files exist but are not currently wired into gameplay.

## Partial systems and known limitations

### Progression is data-rich but not yet a complete player loop

The game has levels, XP fields, stats, ranks, skill caps, curves, mob XP, and simulator support. The following remain unresolved or unverified in the live app:

- XP award on hostile death.
- Automatic threshold handling and level-up.
- Applying new HP/mana/stat/rank values after level-up.
- Player-facing level-up feedback.
- Attribute and discipline allocation UI.
- A complete L1-L20 runtime progression path.

### Ability and animation coverage

The basic ability framework is live, but content breadth is limited. `MILESTONES.md` holds Slam, Shout, caster-specific abilities, and Pin-related content where required clips/API support is missing. Bandits and villagers lack Death clips; bandits/villagers therefore skip the standard death-pose path.

### Loot and itemization depth

Loot works end-to-end but is intentionally narrow:

- Mostly one deterministic family item per eligible mob.
- Fixed 2-8 coin range.
- No quality tiers or randomized affixes.
- No equipment comparison flow.
- No broad item catalog.
- No vendor economy.
- No chest/container loot.

### Save scope

The save stores player state, but not a persistent world-state model. It does not persist:

- Defeated enemy state.
- Cleared site state.
- Quest progress.
- Dungeon completion state.
- Loot piles across sessions.
- Persistent NPC state.
- Other world changes.

## Planned and held gameplay work

These items come from the living plan and should remain visible as future game work:

### M6: village life and first reward loop

The milestone heading remains `NEXT`, although several individual features are already shipped:

- Death pose: shipped for mobs with clips; bandits/villagers remain excluded.
- First loot slice: shipped, including corpse sparkle, modal, and bag.
- Hamlet yard tent and campfire ring: shipped.
- Atlas travel cheat buttons to hamlet yard, Taken Cairn, and Woods Hut: shipped.

Remaining or adjacent content should not be assumed complete merely because the milestone heading exists.

### M7: ability HUD and readability

Already shipped:

- Cooldown radial.
- Cast bar.
- Incoming HP ghost.
- Shaken icon.

Still later:

- Con-color readability.
- Finalized nameplate behavior.
- Auto-attack pip.

Explicitly off the board unless reintroduced:

- Floating combat text.
- First-person weapon presentation.

### Additional encounter/content stamps

Planned later overland site types:

- Berry theft.
- Ford.
- Silk yard.
- Cut landing.
- Ash Hold.
- Sour Well.

### Held combat content

- Slam.
- Shout.
- Caster-specific abilities.
- Pin-related content where clips/API support is missing.
- Additional combat clips and held combat abilities.

## Absent game systems

No implementation was found for:

- Quests, objectives, quest tracker, quest rewards, or quest persistence.
- Shops, vendors, buying, selling, pricing, or merchant inventories.
- Chests or generic containers.
- Persistent enemy ecology or persistent predator/prey state.
- Persistent enemy/site/dungeon completion state.
- Broader NPC gameplay beyond walkers, villagers, and staged encounter presentation.
- A full player-facing level-up/allocation flow.
- A broad economy or itemization system.

## Game-state priorities

For the next genuinely game-focused work, the clearest priorities are:

1. Verify and wire live XP awards from hostile deaths.
2. Implement level thresholds, stat/rank application, and player-facing level-up feedback.
3. Decide whether the L1-L20 curve is the active runtime target and make dungeon/encounter rewards follow it.
4. Expand loot rewards and equipment usefulness without prematurely creating a large item catalog.
5. Add the next chosen non-world game loop: quests, vendors/economy, containers, or persistent encounter completion.
6. Finish or explicitly retire held ability/content work based on available animation/API support.
7. Complete the remaining M7 combat readability features if they improve moment-to-moment play.

## Canonical source files

- Plan: `MILESTONES.md`
- Combat model: `orrun/src/combat/types.rs`, `verbs.rs`, `math.rs`, `sheets.rs`, `sim.rs`
- Hostile presentation/session: `orrun/src/world/combat_layer.rs`, `orrun/src/world/session.rs`
- Loot: `orrun/src/loot.rs`
- Inventory: `orrun/src/inventory.rs`
- Persistence: `orrun/src/save.rs`
- HUD and controls: `orrun/src/hud.rs`, `orrun/src/controls.rs`, `orrun/src/main.rs`
- Audio: `orrun/src/world/ambience.rs`, `orrun/assets/audio/ATTRIBUTION.md`

Last consolidated: 2026-08-24.

## Repair pass status

- Death presentation and loot no longer depend on a death animation clip; no-clip mobs retain a visible fallback corpse state.
- Overland site accounting includes dead bandits, preventing immediate whole-site reseating.
- Hostiles now have deterministic idle/alerted/pursuing/attacking/leashing/dead state, aggro radii, home positions, range stopping, slow/root handling, and leash reset in the live combat tick.
- Remaining alpha limitation: XP threshold crossing and player-facing level-up feedback still require explicit runtime wiring and tests.
