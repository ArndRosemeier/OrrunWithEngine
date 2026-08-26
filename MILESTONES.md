# Orrun milestones

This is the living implementation plan for the gameplay rebuild. `game.md` records verified current state; `TRANSITION.md` explains how the experimental combat slice is replaced without losing the parts worth keeping.

## Working rules

- Milestones are ordered by dependency, not by feature excitement.
- GameData is the single source of authored gameplay definitions. Rust owns runtime state and behavior.
- One action model serves players and mobs. Effects are executed through one centralized runtime path.
- There is no global player or mob level, player XP, discipline, attribute allocation, or rank gate.
- Skill levels are the only progression levels. Using an effect trains its skill; taking damage trains HP; spending mana trains mana.
- Invalid data, missing references, unsupported effect kinds, and invalid state transitions fail loudly.
- A milestone closes only when its exit criteria are covered by automated tests and its player-facing behavior is verified in the live app.
- Do not expand the old combat formulas, simulator, controls enum, or legacy sheets except where necessary to remove or isolate them.

## M0 — One truthful GameData contract — DONE

Make the Python authoring model, canonical XML, Rust loader, and runtime accessors describe the same data without dropping fields.

Deliverables:

- Add strongly typed IDs or equivalent validated newtypes for skills, effects, actions, factions, profiles, mobs, and movement specs at the Rust runtime boundary.
- Load and expose `Skill.level_scale` in Rust.
- Load player profile skill assignments and starting skill levels in Rust.
- Load mob action assignments and mob skill levels in Rust.
- Remove mob XP from the canonical schema and from both authoring models.
- Define HP and mana as trainable actor proficiencies/resources in the canonical model; do not encode them as a global level.
- Replace broad/stringly effect categories with a finite typed operation taxonomy that distinguishes executable semantics such as direct damage, heal, root, hold, snare, and charm.
- Add the execution metadata required by the first vertical slice: action costs/timing and effect duration where applicable; keep presentation recipes separate from gameplay semantics.
- Bring Rust validation to parity with Python geometry and cross-reference validation, including profile skills and mob actions.
- Add parity tests that load the canonical XML through both models and assert the same meaningful records and references.
- Correct authoring documentation and commands to use `data/OrrunGameData.xml`.

Exit criteria:

- Saving valid data through the editor and loading it in Rust loses no gameplay field.
- Unknown, duplicate, dangling, geometrically invalid, non-executable, or unsupported data fails before a world session starts.
- No canonical authored record contains player XP, mob XP, discipline, attribute, rank, or global level data.

## M1 — Actor skills and trainable resources — DONE (headless)

Introduce the new runtime progression owner independently of action execution.

Deliverables:

- Create one encapsulated actor progression state keyed by validated skill ID, with integer level and exact per-skill XP/progress.
- Include HP and mana progression under the same authoritative owner while keeping current/max resource values distinct from progression state.
- Define and test one non-linear XP requirement function. It must be strictly increasing and make breadth advance faster than repeatedly pushing one already-high skill.
- Define the initial curve and starting levels explicitly as provisional balance data, not permanent design law.
- Add typed domain operations for `record_effect_use`, `record_damage_taken`, and `record_mana_spent`; callers cannot mutate XP or levels directly.
- Return typed level-up events containing the trained proficiency, old level, new level, and resulting resource/effect changes.
- Initialize player and mob skill states from GameData profiles/definitions.
- Decide and encode the exact mapping from skill level and `level_scale` to effect magnitude.

Exit criteria:

- Unit tests prove deterministic accumulation, multi-level advancement, monotonic non-linear costs, HP training from damage, mana training from actual spend, and loud rejection of unknown skills.
- Training three equal starting skills for the same total number of uses produces more total level gains than training one skill alone.
- No live combat integration is required yet; the progression domain can be exercised headlessly.

## M2 — Canonical action resolution — DONE (headless)

Build the new combat heart as a headless, typed pipeline before connecting controls or presentation.

Deliverables:

- Resolve an action ID from GameData into its assignments, effect definitions, relevant actor skill levels, and typed execution commands.
- Centralize target validation and application geometry for `single_target`, `cone`, `aoe`, and `pbaoe`.
- Implement the first effect kinds through one path: damage and heal.
- Make mitigation, resource mutation, death transition, aggression, and progression events authoritative domain operations.
- Award skill use only when an effect is executed. There is no hit/miss or success roll.
- Train HP from damage actually received and mana from mana actually spent.
- Represent unsupported effect kinds as loud execution errors until their milestone lands.
- Produce typed result events for presentation, combat log, audio, animation, VFX, loot/death handling, and progression UI.

Exit criteria:

- Headless tests execute GameData `strike`, `fire_bolt`, and `mend` without branching on action IDs.
- A multi-effect fixture action resolves every assignment through the same pipeline and trains each referenced skill.
- Player and mob actions use the same resolver.
- No damage or healing formula is duplicated in presentation or actor-specific code.

## M3 — First live vertical replacement — DONE

Replace the smallest complete live combat path while preserving targeting, hostile AI movement, animation hooks, HUD shells, death presentation, and loot.

Verified complete: the combined release report passed `m3_wolf` and `m3_animals` with no failures or skips, alongside full deterministic headless coverage and GameData A/B validation. The original death/respawn exit behavior remains separately verified; this combined report did not add a new death/respawn hook.

Deliverables:

- Bind the player's first hotbar slots to stable GameData action IDs rather than the closed Rust action enum.
- Route player `strike`, `fire_bolt`, and `mend` through canonical action resolution.
- Route one hostile basic attack through the same action system.
- Adapt cooldown/cast state and HUD labels to action data; no action-ID match tables.
- Connect typed result events to existing hit/hurt/death animation, combat log, sound, hit flash, corpse, and loot presentation.
- Remove discipline/rank gating from the migrated path.
- Keep tab/click targeting and existing hostile locomotion/leash behavior unless a concrete bug requires a focused correction.
- Unify ambient animals with canonical mobs: fauna owns deterministic habitat spawning and presentation entities; canonical actors own faction/mode policy, combat movement, retaliation, death, and loot.
- Author predator/active, prey/passive, and citizen/passive animals in GameData and remove fauna-local hunt/catch deletion.
- Add fixed, deterministic, staggered perception rolls: front/side/rear visual likelihood, angle-independent hearing likelihood, and brief refreshed awareness.
- Make active actors pursue detected hostiles and passive actors flee only from detected active, hostile, non-neutral actors; damage must immediately cancel flight and establish retaliation against the attacker.
- Author required per-mob speed variance (maximum ±20%) and endurance; sample one stable deterministic speed multiplier per actor instance, with predators slightly faster and lower-endurance than prey.
- Drain endurance only during actual pursuit/flight movement; keep both behaviors moving at a centralized slower exhausted speed and recover endurance outside sprint movement.
- Keep perception, threat, heading, movement, endurance, and behavior in canonical actor ownership; fauna remains presentation-only while canonical AI is engaged.

Exit criteria:

- In the live app, the player can kill a wolf using GameData actions, take damage from its GameData action, heal, die/respawn, and loot the corpse.
- Headless/live inspection proves the three migrated actions train their effect skills, incoming damage trains HP, and mana spend trains mana. Player-facing progression feedback belongs to M4.
- Editing an action magnitude or assignment in GameData changes live behavior without a Rust action-specific edit.
- The migrated path contains no read of player level, XP, attributes, disciplines, or ranks.
- Deterministic tests cover staggered cadence, front/side/rear vision, hearing, awareness expiry/refresh, active pursuit, passive-flight exclusions, immediate retaliation, and stable bounded per-instance speed variance.
- Tests prove endurance drains only on actual pursuit/flight movement, exhausted actors continue more slowly, recovery occurs outside sprint movement, and existing leash/awareness loss ends chases.
- Live inspection confirms predator/prey approaches from front and rear, correct pursuit/flight/retaliation transitions, heading and locomotion synchronization, and no fauna-side gameplay decisions.

## M4 — Skill progression becomes visible and persistent — NEXT

Turn the invisible domain model into a player reward loop.

Deliverables:

- Add a focused skill view showing known skills, level, and progress to next level.
- Show clear but restrained skill-level-up feedback sourced from typed level-up events.
- Show HP and mana progression using the same language as other proficiencies.
- Introduce a new save format containing skill progression, HP/mana progression, current resources, position, shrine, inventory, and coin.
- Deliberately reject legacy save formats rather than inventing a misleading conversion from global levels/disciplines.
- Save skills by stable ID and reject saves whose required IDs are invalid for the loaded GameData.

Exit criteria:

- Use, damage, and mana spend can each cause a visible level-up.
- Save/reload round-trips every trained skill and resource value exactly.
- Old saves fail with a clear incompatible-format message.
- There is still no global level or global XP display.

## M5 — Complete effect and action migration

Move the rest of useful combat behavior to the canonical pipeline.

Deliverables:

- Implement centralized control/status effects for root, hold, snare, and charm with explicit durations and typed runtime state.
- Implement defense, movement, and utility effect kinds only from concrete authored actions that need them.
- Define authored timing/resource metadata needed by actions (mana cost, cast time, cooldown, and reveal/interrupt behavior) in GameData rather than action-ID branches.
- Migrate worthwhile legacy behaviors as authored actions/effects; do not preserve an old ability merely because it exists.
- Give each combat mob authored skills and actions; remove special attack formula tables from legacy sheets.
- Connect semantic animation, audio, and initial procedural VFX cues to action/effect result events.
- Keep consumables/equipment as typed sources of actions or effects rather than exceptions in the combat executor.
- Unify potion and ammunition ownership with inventory; remove the duplicate `LivePlayer` counters before migrated actions consume either resource.

Exit criteria:

- Every live player and mob combat action is selected from GameData and executed through the same resolver.
- Root, hold, snare, and charm have distinct tested gameplay semantics.
- No runtime match on a concrete action ID determines damage, healing, control, range, or targeting behavior.
- Unsupported authored combinations fail during validation or execution with a specific error.

## M6 — Remove the legacy combat and progression model

Delete the obsolete system once all live paths have crossed the seam.

Deliverables:

- Remove global player level/XP, mob XP, attributes, disciplines, ranks, rank gates, and level-derived stat formulas.
- Remove legacy `player_stats`, `xp_to_next`, dungeon XP/clear-bonus data, old mob XP fields, and obsolete sheets/catalog duplication.
- Remove the closed controls `Action` enum and fixed per-action keybind storage in favor of stable action-ID bindings.
- Remove hardcoded verb execution, action-ID cooldown/cast tables, and legacy special-case combat fields.
- Delete or replace the old Rust/Python combat simulator and generated results; a replacement must consume GameData and the canonical resolver.
- Update `game.md` so migrated combat is no longer classified as POC.

Exit criteria:

- Repository search finds no gameplay use of global level, player XP, discipline, attributes, ranks, rank gates, or mob XP.
- Live combat, tests, playtester flows, settings, and save/load compile and run without compatibility shims for the old model.
- One headless balance harness uses the same GameData and execution code as the live game.

## M7 — Rebalance and broaden the game loop

Only after the replacement is complete, tune and expand content against the real system.

Deliverables:

- Tune starting levels, `level_scale`, resource growth, and the non-linear XP curve from play evidence.
- Establish danger/readability cues without a global level comparison.
- Re-author loot and equipment usefulness around actions, effects, skills, HP, and mana.
- Add content breadth (encounters, actions, rewards, vendors, containers, quests, or persistent completion) one vertical loop at a time.
- Continue the VFX stages in `docs/vfx.md` where they improve gameplay readability.

Exit criteria:

- A new character has multiple viable ways to grow by doing what the player chooses to use.
- Combat difficulty and reward are understandable without global levels.
- New actions and mobs can be added through GameData plus reusable effect/runtime implementation, not action-specific combat branches.

## Deferred until demanded by play

- Anti-grind rules, use throttles, trainers, skill caps, or commercial-retention balancing.
- Global level, classes, disciplines, attribute-point allocation, hit/miss skill checks, and con-color based on level difference.
- Migration of legacy progression saves.
- Large item catalogs, randomized affixes, broad economy, and quest systems before the new combat/progression core is stable.
