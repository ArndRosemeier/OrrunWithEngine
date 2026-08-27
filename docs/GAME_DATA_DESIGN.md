# Orrun game data and actor rules

## Purpose

This document is the reference for Orrun’s authored game-data model and the rules shared by players and mobs. The current combat implementation is proof-of-concept code and may be replaced; this document describes the data contract and intended runtime behavior, not legacy compatibility.

## Single source of truth

All changeable game metadata belongs in `data/OrrunGameData.xml`. It is loaded once when the game starts into an immutable, strongly typed `GameData` object.

The current top-level sections are:

- `skills`: skills and their level modifiers
- `factions`: faction identities
- `effects`: the catalog of authored effect definitions
- `actions`: reusable actions and their effect assignments
- `players`: player profiles
- `mobs`: mob definitions
- `movement`: movement specifications
- `hamlet`: hamlet generation data and layers
- `defaults`: named authored defaults

`GameData` owns private fields and exposes read-only accessors. Orrun systems must use those accessors exclusively; they must not parse XML independently, load parallel catalogs, or duplicate authored definitions in Rust. Invalid XML, duplicate IDs, missing references, invalid enum values, and invalid numeric or geometric combinations fail loudly during loading or validation.

`OrrunGameData.xml` contains authored definitions only. Runtime state such as health, current velocity, navigation paths, cooldown timers, faction overrides, hostility records, and AI state does not belong in the file. Executable behavior and procedural algorithms remain implemented in Rust.

The shared Modular kit catalog remains owned by the Modular crate. Orrun hamlet data may reference Modular IDs, but must not duplicate the kit catalog.

## Editor contract and stable fields

The GameData editor is a structured editor. XML is an implementation detail and is never presented as the user-facing editing surface.

IDs are stable keys, not ordinary display text. They are used by runtime lookups and by references from other records, so existing record IDs are read-only in the editor. This applies to IDs for skills, factions, effects, actions, player profiles, mobs, movement specifications, and defaults. IDs on nested references are also not free text:

- an action effect assignment selects an existing effect from the effect catalog;
- a mob action selects an existing action;
- a player skill selects an existing skill;
- mob and profile faction fields select an existing faction;
- a mob movement field selects an existing movement specification.

New records receive a generated unique ID. The editor may allow that provisional ID to be chosen during creation, but once the record exists it is treated as stable. Renaming a referenced record is therefore an explicit migration operation, not normal field editing.

Finite domain values are selected from dropdowns rather than typed: faction neutrality, effect operation, effect progression, action target, mob mode, hamlet enabled state, and effect application mode. Numeric values and genuinely authored text remain editable, then strict validation checks them before saving. Required fields cannot be saved empty, and invalid existing values remain visible instead of being silently repaired.

Deleting a referenced record is allowed as an editing operation only if the resulting data is repaired before saving. Validation must reject dangling references.

## Skills and levels

There is no global player level and no global mob level. Levels describe proficiency in individual skills.

Examples of skills include:

- `slashing_damage`
- `fire_damage`
- `healing`
- `illusion`

An actor may have different levels in each skill. A player can therefore have fire damage level 3 and slashing damage level 1. Mobs use the same model.

A skill definition owns the level-modifier behavior. An actor owns its current skill levels. The modifier calculation is centralized in Rust; XML supplies the skill definition and authored parameters.

## Schema 3: actions and executable operations

Schema version 3 is a clean-slate contract. Each effect definition selects exactly one finite, typed executable `operation`: `direct_damage`, `heal`, `root`, `hold`, `snare`, or `charm`. Runtime execution matches this enum; action and effect IDs never select behavior.

Every action explicitly authors non-negative finite `mana_cost`, `cast_s`, and `cooldown_s`, plus required booleans `interruptible` and `reveals`. Every assignment explicitly authors `magnitude`, application geometry, `duration_s`, and `movement_multiplier`; all numeric values must be finite.

Operation-specific rules are strict:

- `direct_damage` and `heal`: positive magnitude, zero duration, movement multiplier exactly 1.
- `root`, `hold`, and `charm`: magnitude exactly 1, positive duration, movement multiplier exactly 1.
- `snare`: magnitude exactly 1, positive duration, and movement multiplier strictly between 0 and 1.

`root` prevents movement; `hold` prevents movement and actions; `snare` multiplies movement speed; `charm` temporarily uses the caster faction. Assignment geometry remains `single_target`, `cone`, `aoe`, or `pbaoe`, with the existing positive range/radius and cone angle invariants.

Player profiles and mobs both contain validated skill-level and action rosters. Rosters are non-empty, contain no duplicates, reference existing records, and assign every skill required by every assigned action. Mob progression starts at its authored skill levels, not inferred level 1.

Rust and `tools/gamedata.py` reject unknown XML attributes/elements, missing required contract fields, non-finite numbers, invalid domains, dangling references, and incoherent rosters. They do not default or repair schema-3 combat data.

## Actors and movement

Players and mobs share these authored concepts:

- faction
- movement specification
- skill levels
- assigned actions

Each mob also explicitly authors a symmetric `speed_variance_ratio` and an `endurance_s` value. The speed variance is finite and constrained to `0.0..=0.20`; endurance is a finite positive duration. Missing or invalid values are data errors, not requests for runtime defaults. Movement specifications remain the source of base speed and controller parameters such as acceleration, deceleration, turn speed, and preferred distance. Predators are authored slightly faster with lower endurance than prey.

At actor registration, the canonical runtime uses the actor's stable spawn seed to sample one movement multiplier inside the authored symmetric range. That multiplier remains stable for the actor instance's lifetime. Current endurance, velocity, destinations, paths, awareness, and flee threats are runtime state.

Mob `mode` is either `active` or `passive`. It is a mob behavior setting, not a faction property. Mobs have no combat level; all progression is skill-specific.

## Factions

There is exactly one special neutral faction. The neutral faction is ignored by default: ordinary actors do not treat neutral actors as hostile, and passive mobs do not flee from them.

The player normally belongs to the `citizen` faction. Other human groups may use factions such as `robber`. Mob factions are authored separately and are not implied by mob type.

Faction identity and behavior are separate:

- faction says who the actor belongs to;
- mob activity mode says whether it may initiate combat;
- runtime hostility records aggression that has already occurred;
- temporary faction overrides change effective faction without changing base faction.

The neutral faction is globally unique and must not be redeclared. Actor references to unknown factions are invalid.

## Default hostility

An active mob attacks by default when the target:

- is not neutral; and
- belongs to a different faction.

This includes active neutral mobs: neutrality does not prevent an active mob from initiating combat. An active mob does not automatically attack members of its own faction or neutral actors.

A passive mob never initiates an attack. It may retaliate after taking damage. Passive is a mob-specific behavior mode; neutral active mobs are valid.

The hostility decision is one centralized domain operation used by AI targeting, attack validation, retaliation, and presentation. It must not be reimplemented separately in each subsystem.

## Deterministic perception, pursuit, and flight

Perception is a canonical fixed-step operation. Every mob rolls on a deterministic, staggered one-second cadence so results do not depend on rendering frame rate or actor iteration order. Visual likelihood is based on distance and canonical facing: strongest in front, weaker at the sides, and substantially weaker behind. Close-range hearing contributes a separate angle-independent likelihood; it does not guarantee detection. Successful rolls establish brief awareness, and later successful rolls refresh it.

Active mobs pursue actors they have detected and that the centralized hostility policy considers hostile. Passive mobs never initiate an attack. They flee only after detecting an actor that is simultaneously active, hostile to them, and non-neutral; passive or neutral actors cannot trigger passive flight. Flight threat is stored separately from an attack target.

```text
Calm
  -> detects active, hostile, non-neutral threat: Fleeing
Fleeing
  -> awareness/leash ends: Calm
  -> takes damage: Retaliating against attacker
Retaliating
  -> combat ends: Calm
```

Positive damage immediately cancels flight and establishes personal retaliation against the attacker without a detection roll. Perception cadence, awareness, flee threats, retaliation, movement, and transitions are private canonical actor state. Fauna entities only present canonical snapshots and never make these decisions.

Pursuit and flight drain endurance only while the actor actually moves. At zero endurance, neither state stops: movement continues at one centralized slower exhausted multiplier. Endurance recovers outside sprint movement, while awareness loss and existing leash rules keep chases finite.

## Aggression and neutrality

Neutrality is a default disposition, not an immunity. An explicit attack creates an aggression event and can establish runtime hostility toward the target.

The base faction remains unchanged. Runtime hostility must be tracked per actor or relationship so that one neutral actor becoming hostile does not change the meaning of neutrality for every neutral actor.

Neutral active mobs are useful for ambushers and stealth attackers. They may remain ignored until they perform an opening attack, at which point the same aggression rules apply.

## Invisibility

Invisibility does not overwrite the player’s permanent faction. It temporarily changes the effective faction:

```text
base faction: citizen
effective faction while invisible: neutral
```

When an invisible player attacks, the attack reveals the player and must:

1. remove the temporary neutral faction override;
2. restore the effective `citizen` faction;
3. register the explicit aggression event; and
4. allow normal hostility rules to resolve subsequent combat.

The same concept can support invisible or stealth mob attackers. Whether an action reveals its user should be represented by authored effect or behavior data and executed by centralized Rust logic.

## Hamlet data

The hamlet section stores authored configuration and generation rules, including layers, tiers, recipes, weights, dimensions, and constraints. It does not store generated settlement instances.

The hamlet lab may be started on demand, but it must load the same `GameData` as the main game. It must not maintain a separate set of hamlet defaults. Modular kit IDs remain references to the Modular catalog.

## XML shape

The canonical `data/OrrunGameData.xml` is the schema-3 example. Its minimal player roster covers melee, ranged, healing, root, hold, snare, and charm; mobs author coherent subsets and explicit levels. The typed Python authoring model and typed Rust runtime loader enforce the same contract.
