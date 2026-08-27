# Gameplay transition

This document is the bridge from Orrun's current combat-and-loot proof of concept to the GameData-driven action, effect, and skill system.

It is not a second roadmap. `MILESTONES.md` owns ordering and completion criteria. `game.md` owns verified current state. This file owns the migration boundaries: what survives, what is replaced, how the new runtime is introduced, and when old code may be deleted.

## Why this transition exists

The app already has a playable loop, but its combat core answers the wrong design questions:

- one global player level and XP total;
- Martial, Hunt, and Arcane disciplines;
- attributes and rank gates;
- hardcoded actions and formulas selected by Rust enums and string matches;
- player and hostile attacks following different special-case paths;
- a simulator and save format built around that model.

The intended game has none of those progression concepts. Actors have individual skill levels. Actions contain one or more effects. Each effect references exactly one skill, and that skill level determines how well the effect works. Effects always execute when their targeting and resource requirements are valid; there is no hit/miss or skill-success roll.

Progress comes from use:

- executing an effect trains the effect's skill;
- receiving damage trains HP;
- spending mana trains mana.

Each proficiency has its own non-linear level cost. Pushing one high skill becomes progressively expensive, so using several skills produces more total level gains than specializing forever. No anti-grind system, cap, or commercial-retention mechanic is planned.

## The seam

The migration is a **vertical replacement**, not an in-place refactor of legacy formulas.

```text
Authored GameData
  -> validated typed definitions
  -> actor runtime state
  -> action request
  -> target + geometry resolution
  -> effect commands
  -> centralized state mutations
  -> typed result/progression events
  -> HUD, animation, audio, VFX, death, loot
```

New work moves down this path. The legacy path remains only long enough to keep the current loop playable while slices cross the seam. No new feature should deepen the legacy model.

## Current-to-target map

### Authored data

Current:

- `data/OrrunGameData.xml` already defines skills, factions, effects, actions, player profiles, mobs, movement, hamlet data, and defaults.
- `tools/gamedata.py` reads and writes player skill assignments, mob action assignments, and `Skill.level_scale`.
- `orrun/src/gamedata.rs` loads the document once and supplies mob stats to live combat.

Gap:

- The Rust model currently drops `Skill.level_scale`, player profile skills, and mob action assignments.
- The Python and Rust validators do not yet enforce exactly the same contract.
- Mob XP still exists in both models even though the design abolishes it.
- XML IDs cross much of the runtime as untyped strings.

Target:

- Python, XML, and Rust preserve one identical strict contract.
- Stable, validated IDs connect authored records to runtime state.
- GameData contains definitions only; mutable actor state stays in Rust and saves.

### Actor state and progression

Current:

- `LivePlayer` owns `PlayerStats`, resources, inventory-adjacent counters, discipline-era flags, and one global XP value.
- `PlayerStats` derives HP, mana, attributes, ranks, and action output from player level and discipline.
- `WorldHostile` owns direct combat values and temporary control timers but no shared actor skill state.
- Hostile death awards global player XP and can apply global level-ups.

Target:

- Players and mobs share an actor model for faction, skills, resources, statuses, known actions, and hostility.
- One private progression owner records per-skill level and XP and owns every level transition.
- HP/mana progression and current/max resources are related but not conflated.
- Hostile death triggers death/loot events, never global XP.

### Actions and effects

Current:

- `controls::Action` is a closed enum of eleven player verbs.
- `combat/verbs.rs` matches every verb and hardcodes range, mana, timing, cooldown, formulas, and status behavior.
- Hostile basic and special attacks run through separate code.
- GameData actions/effects are validated data but not live executable definitions.
- The standalone spell builder is authoring-only and has no runtime execution path.

Target:

- Controls and AI request stable action IDs.
- One resolver reads action assignments and effect definitions from GameData.
- Targeting geometry, resource costs, timings, and effect application are data-driven within a finite typed schema.
- Players and mobs use the same resolver.
- Damage, healing, control, defense, movement, and utility mutate state through centralized domain operations.
- Result events drive presentation without presentation owning game rules.

### Presentation and world loop

Current assets worth preserving:

- tab/click target selection and lock presentation;
- hostile navigation, aggro state, range stopping, leash, and home reset;
- hotbar layout, cooldown radial, cast bar, target frame, combat log, failure toast, and hit ghost;
- semantic animation calls, combat SFX, hit flashes, death pose/fallback corpse state;
- corpse sparkle, loot modal, inventory, equipment slots, Shaken/respawn, shrine, encounter placement.

Transition rule:

- Preserve these as presentation or world-loop adapters where practical.
- Replace their inputs with typed action/effect/result events.
- Do not preserve a UI or animation assumption that requires a concrete legacy action enum or discipline/rank state.

### Persistence and tools

Current:

- Save format 3 persists global level, XP, attributes, ranks, HP/mana, Shaken, shrine, inventory, and coin.
- Keybind settings serialize one field per closed legacy action.
- The Rust/Python combat simulators reproduce legacy formulas and progression.
- Playtester checks assert concrete legacy verbs and rank gates.

Target:

- A new save format persists stable skill IDs with level/progress, trainable HP/mana state, current resources, and the model-independent world/inventory state.
- Legacy saves are rejected clearly; there is no meaningful automatic conversion.
- Keybinds map physical keys or slots to stable action IDs and are validated against the current player profile.
- Balance and playtest tools consume the same GameData and action resolver as the live game.

## What survives, what adapts, what is deleted

### Preserve

These are useful independently of the old progression model:

- deterministic world/session ownership;
- encounter placement and hostile body lifecycle;
- target selection geometry and lock presentation;
- hostile locomotion/leashing behavior, subject to focused bug fixes;
- inventory, corpse loot, death pose, respawn/shrine, save location/world identity;
- semantic animation and audio/VFX presentation hooks;
- strict startup loading and fail-loud error policy.

### Adapt behind typed interfaces

- HUD/hotbar/cast/cooldown state;
- controls and settings;
- combat log and failure messages;
- hostile AI attack selection;
- mitigation and resource state;
- death/loot trigger plumbing;
- consumables and equipment;
- playtester and headless balance tooling.

### Delete after replacement

- global player level and XP;
- mob XP and dungeon XP rewards;
- disciplines, attributes, ranks, and rank gates;
- `PlayerStats` level/discipline formulas;
- hardcoded action enum execution and per-action string tables;
- separate player/mob formula paths;
- legacy skill caps and L1-L20 curve;
- old combat simulator outputs and compatibility tests tied to the abolished model.

## Runtime ownership rules

### GameData

Immutable after startup. Owns definitions and validated references. It does not own current HP/mana, accumulated skill XP, cooldowns, statuses, hostility, targets, or positions.

### Actor state

Owns private mutable runtime state: skill progression, current/max resources, effective faction, statuses, known actions, cooldown/cast state, and life/death state. Mutation happens through named domain operations.

The final shape may separate combat, progression, and movement into focused sub-owners, but callers must not coordinate their invariants manually.

### Action resolver

Owns action validation and execution order:

1. resolve actor and action;
2. validate actor state, target requirement, range, and resources;
3. resolve application geometry;
4. spend resources and start/finish timing as defined;
5. produce typed effect commands;
6. apply effects through authoritative state operations;
7. record progression from actual effect execution, damage received, and mana spent;
8. emit typed results.

It never branches on `slash`, `arrow`, `restore`, or another concrete action ID to select game rules.

### Presentation adapters

Consume results and inspect immutable state. They may choose animations, sounds, labels, colors, and VFX recipes, but cannot calculate damage, award progression, alter faction, or perform death transitions.

## Progression contract

These decisions are fixed for the transition:

- There is no global level.
- Every trainable proficiency has an integer level and its own XP/progress.
- Effect use does not roll for success. Valid effects execute.
- An executed effect trains exactly the skill referenced by its effect definition. A multi-effect action may train multiple skills.
- Taking positive post-mitigation damage trains HP.
- Spending positive mana trains mana. Failed requests and zero-cost actions do not train mana.
- Level cost is deterministic, non-linear, and strictly increasing.
- The system has no cap or anti-grind rule.
- Level changes are emitted as events; UI does not infer them by polling values.

M1 established the first values (centralized in `orrun/src/progression.rs` `balance`, replaceable):

- level cost is quadratic (`100 * level²`) so cumulative cost is cubic and breadth beats depth;
- `XP_PER_EFFECT_USE` 10, `XP_PER_HP_DAMAGE` 1 (rounded up), `XP_PER_MANA_SPENT` 1 (rounded up);
- HP capacity `100 + 12 * (level - 1)`; mana capacity `50 + 6 * (level - 1)`;
- `level_scale` maps `skill_level` to `base * (1 + level_scale * (level - 1))`; `flat` returns `base`;
- player profile skills start at their authored level; mob skills start at 1 until the schema carries per-mob skill levels.

These are balance parameters, not design law; M7 retunes them from play evidence.

## Cutover sequence

1. **Make data lossless.** Do not build runtime progression on fields the Rust loader discards.
2. **Build progression headlessly.** Establish private state, formulas, typed training commands, and events without touching live combat.
3. **Build action resolution headlessly.** Prove action/effect execution for player and mob actors from GameData.
4. **Cross one playable slice.** Slash, arrow, restore, and one hostile attack provide enough coverage for damage, healing, mana, HP training, skill training, death, and loot.
5. **Expose and save progression.** Only persist the new model after its live behavior is proven.
6. **Migrate effect breadth.** Add control/status and remaining useful action semantics through the same path.
7. **Delete legacy code.** Removal is a milestone, not background cleanup; compatibility shims must not become permanent architecture.
8. **Rebalance and expand.** New content waits until it exercises the real model.

## Temporary coexistence rules

During M0-M5, old and new paths may coexist. To prevent permanent duplication:

- every migrated action has exactly one execution path;
- no result is applied by both systems;
- new progression state is never mirrored into global XP, disciplines, attributes, or ranks;
- legacy and new saves are not merged;
- new GameData fields are not backported into old formula tables;
- compatibility adapters point from old presentation/control callers toward the new domain, never from the new domain back into legacy rules;
- temporary adapters carry an owning milestone and are deleted in M6.

## Verification strategy

### Contract tests

- canonical XML round-trip in Python;
- canonical XML load in Rust;
- parity fixtures for IDs, references, profile skills, mob actions, levels, geometry, and rejected data.

### Domain tests

- progression accumulation and multi-level transitions;
- breadth advantage under the non-linear curve;
- action target/range/geometry validation;
- each effect kind and status transition;
- HP and mana training triggers;
- death, hostility, faction override, and resource invariants;
- player/mob equivalence through the shared resolver.

### Integration tests

- GameData edit changes headless action result;
- live wolf fight through the new path;
- progression event reaches HUD;
- save/reload exact round-trip;
- corpse/loot spawns once after canonical death;
- old save fails loudly.

### Visual/play verification

Automated correctness is not enough for targeting, timing, feedback, or VFX. Each live milestone verifies readable target selection, cast/cooldown feedback, hit/death presentation, level-up feedback, and absence of duplicate effects.

## Known risks

- **Schema drift:** Python is currently richer than Rust. M0 closes this before runtime work.
- **Stringly typed IDs:** stable strings are necessary at file boundaries, but runtime APIs need validated typed identities.
- **Dual execution:** coexistence can apply damage or rewards twice. Migrated actions must switch atomically.
- **God-object combat state:** current `WorldCombat` owns simulation, timing, AI-adjacent state, progression, and presentation-facing fields. New owners should be focused, with one authoritative mutation path per invariant.
- **Presentation coupling:** HUD and animation code know concrete legacy actions. Result events and data lookups must replace match tables.
- **Premature balancing:** exact curves are uncertain. Build deterministic centralized formulas first; tune only after live use is observable.
- **Overmigration:** not every POC ability deserves preservation. Migrate gameplay intent, not historical code coverage.

## Definition of transition complete

The transition is complete when:

- all live player and mob actions originate in GameData;
- all effects execute through one centralized typed resolver;
- actors progress only through individual skills, HP, and mana;
- use/damage/spend progression is visible and persisted;
- no gameplay runtime depends on global level, XP, disciplines, attributes, ranks, or mob XP;
- controls, HUD, animation, audio, VFX, death, and loot consume the new domain state/results;
- the legacy combat simulator and formula tables are gone or replaced by tooling using the live resolver;
- adding a normal action or assigning it to a mob/player requires authored data, not a new concrete action branch in Rust.

## Canonical animal integration

Ambient fauna and combat mobs now share one runtime identity. The fauna catalog owns habitat, density, model paths, dimensions, and animation clips; every species links exactly once to a GameData mob. Canonical actor state owns faction, mode, targets, personal provocation, resources, actions, position while engaged, death, and stable corpse/loot identity.

Animal policy has no per-species hostility scripting:

- fixed-step canonical AI performs deterministic detection rolls on a staggered one-second cadence;
- visual likelihood is strongest in front, weaker at the sides, and weakest at the rear, while close-range hearing contributes an angle-independent likelihood rather than automatic detection;
- awareness persists briefly and is refreshed by later successful rolls so behavior does not flicker between ticks;
- `active` actors pursue detected hostile actors; `passive` actors flee only from detected actors that are active, hostile, and non-neutral;
- flight keeps a separate threat from attack targeting; taking damage immediately cancels flight and establishes personal retaliation against the attacker without a detection roll;
- each actor instance receives one stable deterministic movement multiplier from its spawn seed, bounded by its mob's authored symmetric variance of at most ±20%; predators are authored slightly faster but with lower endurance;
- authored endurance drains only during actual movement in pursuit or flight; at zero, both behaviors continue at a centralized slower exhausted speed, and endurance recovers outside sprint movement;
- streaming deactivation unregisters an actor without producing death or loot;
- idle movement remains deterministic fauna roaming, while canonical AI exclusively owns engaged position, heading, awareness, movement, endurance, and behavior state;
- the existing fauna entity is presentation-only for locomotion, attack, death, targeting, VFX, corpse, and loot and never decides gameplay behavior.

The old fauna role, flee, hunt, catch, and direct prey-despawn path is deleted. `wolf` is the canonical mob ID; combat-facing aliases no longer create a second wolf actor.
