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

Finite domain values are selected from dropdowns rather than typed: faction neutrality, effect kind, effect progression, action target, mob mode, hamlet enabled state, and effect application mode. Numeric values and genuinely authored text remain editable, then strict validation checks them before saving. Required fields cannot be saved empty, and invalid existing values remain visible instead of being silently repaired.

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

## Actions, effect catalog, and assignments

There is one action model. Attacks, spells, abilities, mob attacks, and utility abilities are all `Action`s. A classification may be retained for editor or presentation purposes, but it must not create separate execution systems.

The top-level effect catalog defines what an effect is. Each catalog effect has a stable `id`, display `name`, `kind`, exactly one `skill_id`, and a `progression` rule. The catalog effect is authored once and reused.

An action contains effect assignments. An assignment references an authored effect with `effect_id` and defines how that effect behaves for this action, including `magnitude` and application geometry. The assignment does not redefine the effect’s kind, skill, or progression.

Every authored effect references exactly one skill, and the level of that skill modifies the assignment’s effect. Levels are never stored on effects or action assignments.

Conceptually:

```text
Action
  -> ActionEffect assignment -> Effect definition -> exactly one Skill
  -> ActionEffect assignment -> Effect definition -> exactly one Skill
```

This permits one action to combine, for example, slashing damage and fire damage. Effect execution, validation, progression, and application are centralized in Rust.

The starter catalog includes several damage types (`slashing_damage`, `fire_damage`, `frost_damage`, `piercing_damage`, `bludgeoning_damage`, `poison_damage`, and `lightning_damage`) plus control effects (`root`, `hold`, `snare`, and `charm`). These are authored effect identities, not arbitrary editor-created kinds. `root` prevents movement, `hold` prevents movement and actions, and `snare` reduces movement according to the eventual centralized control implementation. `charm` temporarily changes the affected actor's effective faction to the initiator's faction; it does not change the actor's permanent base faction and expires through runtime effect state.

### Application geometry

An action-effect assignment has one of these application modes:

- `single_target`: applies to the selected tab target; requires positive `range_m`.
- `cone`: originates from the caster, uses the selected target for direction, and requires positive `range_m` and an `angle_deg` between 0 and 360.
- `aoe`: is centered on the selected tab target; requires positive `range_m` and `radius_m`.
- `pbaoe`: is centered on the caster and requires no target; requires positive `radius_m` and `range_m="0"`.

Tab targeting remains the only targeting mechanism. A target-centered AOE does not introduce free-area targeting. `range_m` is the targeting or reach distance, `radius_m` is required for area modes, and `angle_deg` is required for cones. Melee actions use a small positive range.

## Actors and movement

Players and mobs share these authored concepts:

- faction
- movement specification
- skill levels
- assigned actions

Movement specifications contain stable parameters needed by the movement controller, such as movement speed, acceleration, deceleration, turn speed, and preferred distance where applicable. Current velocity, destinations, paths, and fleeing targets are runtime state.

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

## Passive mob fleeing

Passive mobs flee when they see an active, non-neutral threatening mob. They do not flee from passive or neutral actors.

Fleeing is deliberately finite to avoid permanently fleeing or exploitable mobs:

```text
Calm
  -> sees active, non-neutral threat: Fleeing
Fleeing
  -> takes damage: Retaliating
Retaliating
  -> combat ends: Calm
```

After taking damage, the passive mob stops fleeing and can retaliate. The exact movement path and timing are runtime concerns; authored data selects the passive behavior and movement specification.

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

The following is representative of the current schema. It shows the distinction between the effect catalog and action assignments; it is not a replacement for typed validation.

```xml
<OrrunGameData schema_version="1">
  <skills>
    <skill id="fire_damage" name="Fire Damage" level_scale="1" />
  </skills>
  <factions>
    <faction id="neutral" name="Neutral" neutral="true" />
    <faction id="citizen" name="Citizen" neutral="false" />
  </factions>
  <effects>
    <effect id="fire_bolt_damage" name="Fire Bolt Damage" kind="damage"
            skill_id="fire_damage" progression="skill_level" />
  </effects>
  <actions>
    <action id="firebolt" name="Fire Bolt" target="hostile">
      <effects>
        <effect effect_id="fire_bolt_damage" magnitude="1"
                application="single_target" range_m="18" />
      </effects>
    </action>
  </actions>
  <players />
  <mobs />
  <movement />
  <hamlet enabled="true" width="32" depth="32"
           kit_catalog="catalogs/medieval.json" />
  <defaults />
</OrrunGameData>
```

The typed Python authoring model, the typed Rust runtime model, and their strict validation rules are authoritative for implementation details.
