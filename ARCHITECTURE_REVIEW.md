# Combat Architecture Review

Date: 2026-08-24
Scope: `orrun/src/combat`, `orrun/src/world/combat_layer.rs`, `orrun/src/world/session.rs`, `orrun/src/world/sites.rs`, `orrun/src/bin/playtester.rs`, combat sheets/catalogs, simulator, and related tests.

## Executive verdict

The current combat implementation is not yet architecturally safe for the intended EverQuest-like procedural game. The main problem is not that the formulas are wrong; it is that ownership is split. `WorldSession`, `CombatLayer`, `WorldCombat`, site seating, playtester helpers, and `combat::sim` can all influence combat state. That makes it possible for a mob to be logically alive but visually unbound, for damage to bypass death/XP, and for live combat to diverge from the simulator.

The required target is a single authoritative combat simulation with private state transitions. Rendering, world streaming, site composition, persistence, and headless balance tests must be adapters around that authority.

## Current authority map

- `WorldSession::step` owns frame orchestration, player movement, travel/death lifecycle, input dispatch, roster repair, loot synchronization, and presentation sequencing.
- `CombatLayer::tick` owns the fixed-step accumulator and tick order, while also resolving skull/mage damage directly. It is therefore both presentation and simulation.
- `WorldCombat` owns much of the live state and some damage, AI, death, and XP rules, but its public fields allow bypasses.
- `sites.rs` directly inserts/removes hostiles in the live combat vector.
- `playtester.rs` directly mutates hostile death fields in at least two paths.
- `combat::sim` is a second combat engine, not merely a runner over live rules.

The effective flow is:

```text
input -> WorldSession -> CombatLayer::tick
                         |-> WorldCombat AI / verbs / melee / incoming
                         |-> CombatLayer skull + mage damage
                         |-> presentation and entity mutation
sites.rs ----------------^                         
combat::sim = separate rules engine
```

## Critical findings

### P0: damage and death are duplicated

Hostile damage is repeated in `WorldCombat::tick_melee_auto` and the ranged branches in `combat/verbs.rs`. Player damage is applied in `WorldCombat::tick_incoming` and separately in `CombatLayer::tick_skull_bolts` and `tick_mage_bolts`. The latter paths directly mutate HP, death flags, lock, slain metadata, cooldown, and log state.

There must be one hostile damage operation and one player damage operation. Every melee hit, bow action, Ember, bolt, slam, poison tick, and future ability must call those operations. The operation must clamp HP, apply mitigation/ward rules, update threat/aggro, transition death, and emit the result atomically.

### P0: the visual layer is a gameplay authority

`CombatLayer` currently owns the combat clock and special attack rules. It also reads and writes authoritative combat fields. Changing render cadence, mesh state, or presentation reset state can therefore change gameplay. This is the direct architectural cause of bugs such as stale combat state surviving travel while render anchors are destroyed.

`CombatLayer` must consume combat events and produce animations/audio/effects. It must not resolve damage or decide death.

### P0: the simulator is a parallel implementation

`combat/sim.rs` has independent player/mob state and implementations for damage, interrupts, statuses, AI, special attacks, and progression. Shared constants do not guarantee behavioral parity. A simulator test can pass while the live game behaves differently.

The simulator should become a scenario runner over the same resolver. If a simplified balance model is deliberately retained, it must be named and tested as an approximation, not treated as the combat oracle.

### P0: mutable public state permits invariant bypass

`WorldCombat`, `LivePlayer`, and `WorldHostile` expose sensitive fields publicly. Playtester code can set hostile HP/alive directly, bypassing `defeat_hostile`, XP, state transitions, logs, and future loot events. `WorldHostile` also embeds a render `EntityId`, coupling deterministic state to a transient engine identity.

Make mutation-sensitive fields private. Expose commands and read-only views. Keep render bindings in a presentation-side map keyed by a stable combatant ID.

### P1: AI is centralized today, but not extensible enough

`tick_hostile_ai` is generic, which is the correct direction, but behavior is still limited to one hardcoded policy and uses the global `WALK_MPS` rather than each sheet's speed. Special behaviors are selected by mob-id branches in `CombatLayer`; `MobSheet::specials` is free-form strings. This will not scale to a roster of complex mobs.

Use typed, declarative behavior profiles. Runtime code should evaluate capabilities and action profiles, never ask whether `mob_id == "orc_skull"`.

### P1: encounter ownership is distributed

Fixtures, overland sites, dungeons, session reset logic, and site seating all mutate the same hostile vector. IDs are locally allocated using max-plus-one and can be reused after clearing. Roster flags such as `fixture`, `roster_pins_seated`, and `skip_roster_pins` are implicit lifecycle state.

Introduce one `EncounterDirector`/`CombatRoster` that owns encounter creation, stable IDs, reset, and source ownership. Sites should return encounter specifications, not insert combatants.

### P1: life/death state is redundant

Hostiles use `hp`, `alive`, and `HostileState::Dead`. The player uses HP, `dead`, `slain_by`, and `slain_hold_s`. These representations can disagree. Player death is detected in combat and resolved later by session; projectile paths duplicate parts of the transition.

Use explicit lifecycle state and enforce invariants. A hostile's dead state should be entered only by the resolver. Player death should be a modeled phase (`Active`, `SlainHold`, `Respawning`) rather than a collection of flags.

### P1: render lookup relies on unstable indices

Presentation falls back from `WorldHostile.entity` to `mesh_ids[h.idx as usize]`. Hostile IDs are not vector indices, and roster insertion/removal/travel can invalidate that assumption. This is an immediate source of wrong or missing animation targets.

Use `CombatantId -> RenderBinding` only. Missing bindings should be explicit presentation errors or a clearly documented not-yet-bound state, never an index guess.

### P2: typed boundaries are missing

Cast kinds are stringly typed (`"aimed"`, `"pin"`, `"ember"`, etc.). Save state uses a different lock integer width from runtime. Targeting has a second apparently unused abstraction in `combat/lock.rs`. These are signals of parallel models.

Use typed `CastKind`, one `CombatantId`, and one canonical targeting API. Remove or adapt dead abstractions.

## Target architecture

```text
WorldSession
  | gathers input and world facts; owns travel/streaming
  v
CombatRuntime
  | owns fixed-step scheduling and encounter lifecycle
  v
CombatResolver
  | owns all simulation state and transitions
  |-- commands: UseAction, SetTarget, ForceTestState
  |-- generic behavior profiles and action evaluator
  |-- apply_damage_to_hostile / apply_damage_to_player
  |-- death, XP, status, threat, cooldowns
  `-- emits CombatEvent
       |-> CombatPresentationAdapter (meshes, animation, audio, VFX)
       |-> HUD/log adapter
       `-> headless scenario runner and tests

EncounterDirector -> CombatRuntime: encounter specifications
World query adapter -> CombatResolver: navigation/perception facts
Save adapter <-> canonical combat persistence DTO
```

### Required core contracts

```rust
pub struct CombatRuntime {
    resolver: CombatResolver,
    accumulator_s: f64,
    encounters: EncounterDirector,
}

pub fn step(&mut self, frame: CombatFrame, commands: &[CombatCommand]) -> Vec<CombatEvent>;

fn apply_damage_to_hostile(&mut self, packet: DamagePacket) -> DamageResult;
fn apply_damage_to_player(&mut self, packet: DamagePacket) -> DamageResult;
```

Those two damage operations are the only code allowed to mutate combat HP. `DamageResult` must state dealt amount, absorbed amount, status effects, and whether a death transition occurred. Death/XP must be part of the same transition, not a follow-up convention callers can forget.

### Generic behavior roster

Represent behavior as typed data:

```rust
pub struct BehaviorProfile {
    pub perception: PerceptionProfile,
    pub locomotion: LocomotionPolicy,
    pub target_policy: TargetPolicy,
    pub actions: Vec<ActionProfile>,
}
```

`ActionProfile` should describe conditions, priority, cooldown, range, telegraph, interruptibility, effects, and presentation cue. Wolves, bandits, orcs, skulls, mages, yetis, scorpions, and future mobs should differ by profile data, not branches in the resolver. Mob-specific content belongs in sheets/catalogs/behavior data; mob-specific code is prohibited.

## Migration plan

1. Add stable `CombatantId`, private mutation-sensitive fields, and read-only snapshots.
2. Add `CombatCommand`, `CombatEvent`, `DamagePacket`, and `DamageResult`.
3. Implement the two centralized damage operations; route melee, bow, Ember, and `defeat_hostile` through them.
4. Move skull/mage/other special attack resolution from `CombatLayer` into the resolver; leave only telegraph and animation consumption in presentation.
5. Move fixed-step accumulation and the complete simulation order out of `CombatLayer` into `CombatRuntime`.
6. Replace direct playtester death mutation with an explicit test-only command.
7. Introduce typed `CastKind` and typed behavior/action profiles.
8. Make `CombatLayer` a pure event/render adapter; remove `EntityId` from simulation records and index fallbacks.
9. Introduce encounter ownership; make sites and fixtures produce specs consumed by one roster owner.
10. Convert `combat/sim.rs` into a runner over the resolver and add parity tests for canonical encounters.
11. Unify persistence DTOs with runtime state and collapse travel/death/reset into atomic lifecycle operations.

## Acceptance criteria

- A repository-wide search finds exactly two authoritative HP mutation functions: one player, one hostile.
- No `CombatLayer` code writes HP, death, XP, threat, or cooldown state.
- No runtime combat code branches on mob IDs to select behavior.
- No caller sets `hp`, `alive`, or dead flags directly outside controlled constructors/test commands.
- One fixed-step function defines phase order and is independent of rendering cadence.
- Hostile AI receives behavior data and world/navigation facts; it does not contain per-mob branches.
- Live and headless tests execute the same resolver.
- Encounter reset/travel invalidates bindings and rosters through one lifecycle method.
- Every mob definition passes a roster contract: sheet, behavior, mesh, animation, and loot references resolve.

## Test requirements

Add unit and integration coverage for: every damage source through the same operation; lethal/nonlethal damage; ward and mitigation; death and XP exactly once; status/interrupt semantics; sight/hearing/social aggro; leash and per-profile speed; special attack telegraph/interruption/completion; travel reset; fixture/site seating; render binding invalidation; live/simulator parity; and full roster contracts.

## Bottom line

The recent fixes addressed symptoms, but the current shape still allows the same class of bugs to return. Do not add more damage or AI branches to the existing layout. Refactor toward a resolver/event boundary first, then grow the EverQuest-style behavior roster as data interpreted by generic systems.
