# Orrun visual effects design

## Purpose

This document is the working design reference for Orrun's visual effects (VFX). It describes the visual language, delivery-specific treatments, the boundary between authored data and runtime implementation, and a staged implementation path. It is intentionally iterative: decisions can be refined here before they become engine code or Blender assets.

The first scope is spell and ability effects. Weapon effects are explicitly out of scope for now.

## Core direction

Use a hybrid system:

- GameData identifies the authored visual concept and exposes tunable values.
- Rust owns effect construction, timing, animation, targeting, and lifecycle.
- The Engine provides reusable procedural primitives, materials, particles, decals, beams, and volumes.
- Blender supplies distinctive reusable hero components where procedural geometry is insufficient.

Avoid creating a unique asset for every combination of effect type and delivery mode. A fire effect should reuse one visual identity while adapting its delivery treatment to single target, target-centered multi-target, or cone.

## Visual grammar

An effect's visual identity should define:

- palette and material character;
- particle and motion language;
- impact treatment;
- persistent status indicator;
- eventual sound family;
- optional reusable asset components.

An action-effect assignment defines delivery data:

- `application`: `single_target`, `aoe`, `pbaoe`, or `cone`;
- `range_m`, `radius_m`, and `angle_deg` where applicable;
- magnitude and, later, duration.

The effect definition must not encode one fixed range, radius, or target. The same identity must scale with the assignment.

## Delivery families

### Single target

Use a narrow origin-to-target path and a compact actor-focused impact. Scale primarily with authored magnitude and actor size. The effect should clearly communicate the caster, the selected target, and the hit moment.

### Target-centered multi-target

For `aoe`, center the effect on the selected tab target. Use a larger impact, expanding ring or volume, and distinct secondary impacts on each affected actor. This remains compatible with tab targeting and does not introduce free-area targeting.

For `pbaoe`, center the effect on the caster and omit the target path entirely. The visual should make the caster-centered origin obvious.

### Cone

Cones need a dedicated treatment rather than a stretched projectile. Show the caster origin, direction, wedge or fan geometry, travel through the area, and a readable boundary. Actors should visibly be reached as the wave passes through the cone.

## Effect concepts

### Fire

- Single target: compact ember or fire bolt, orange-red trail, impact burst, rising sparks.
- Multi-target: larger target-centered explosion, scorch or hot-ground ring, expanding heat wave.
- Cone: fan of flame from the caster, translucent heat wedge, parallel flame tongues, bright boundary edge.

Start procedurally with particles, emissive meshes, rings, and a cone wedge. No Blender asset is required initially.

### Frost

- Single target: pale blue shard or projectile, crystalline flash, outward frost motes.
- Multi-target: target-centered ice bloom, radial frost ring, short-lived ice crystals.
- Cone: frozen breath or mist fan, dense wedge near the caster, ice shards along the boundary.

Procedural particles, translucent volumes, rings, and crystal-like primitives should be sufficient for the first pass.

### Lightning

- Single target: fast bright arc, contact flash, short branching impact arcs.
- Multi-target: primary strike plus secondary jumps, central flash, circular electric field.
- Cone: electrical fan or chain curtain with branching arcs and a charged caster origin.

Lightning should be generated at runtime. Static Blender meshes are unlikely to provide enough variation.

### Poison

- Single target: dark green projectile or glob, splatter-like impact, bubbles and motes.
- Multi-target: lingering cloud centered on the selected target, low fog ring, rising bubbles.
- Cone: low rolling poison breath, ground-hugging green wedge, flow directed away from the caster.

Use transparent particles, billboards, decals, and procedural fog-like volumes first.

### Control effects

Control visuals must distinguish their gameplay meanings at a glance:

- `root`: movement is prevented; emphasize feet, roots, and ground attachment.
- `hold`: movement and actions are prevented; use strong bands, chains, or a restraint cage.
- `snare`: movement is reduced; use drag, tethers, sticky ground, or trailing particles with lower intensity.
- `charm`: effective faction temporarily follows the initiator; use a soft allegiance or influence indicator, not a hostile restraint.

#### Root

- Single target: roots or tendrils around the feet and a small ground ring.
- Multi-target: spreading root patch with separate attachments to affected actors.
- Cone: travelling wedge of ground tendrils that erupts as it reaches actors.

#### Hold

- Single target: translucent bands, chains, or crystalline braces around torso and limbs.
- Multi-target: individual restraint cages with a shared containment pulse.
- Cone: binding-energy fan that forms restraints as it passes through actors.

#### Snare

- Single target: foot tether, weighted trailing particles, or slow rotating ring.
- Multi-target: sticky ground patch and separate subtle tethers.
- Cone: web, mud, frost, or drag-field wedge on the ground.

Snare should be visibly less intense than root or hold because the actor remains mobile.

#### Charm

- Single target: soft rose, violet, or gold halo, brief caster-to-target influence thread, faction-colored outline.
- Multi-target: expanding influence pulse with links to affected actors.
- Cone: fan-shaped influence wave with brief caster-facing ribbons.

Charm changes only the target's runtime effective faction for its duration. It does not rewrite the permanent base faction. The persistent indicator should be subtle, such as a halo or nameplate accent.

## Procedural primitives

The first reusable VFX library should provide:

- projectile and trail;
- beam and tether;
- lightning arc;
- expanding ring and shockwave;
- ground decal and area patch;
- cone wedge and travelling fan;
- fog or translucent volume;
- actor halo and outline;
- restraint bands;
- impact burst and secondary hit marker.

Primitives should accept typed runtime parameters for origin, target, direction, range, radius, angle, duration, scale, palette, and intensity. They must not embed effect-specific game rules.

## Blender asset policy

Prefer runtime authoring for projectiles, trails, rings, wedges, arcs, fog, halos, tethers, restraints, and impact flashes. These naturally need to scale with authored geometry and delivery mode.

Use Blender later for reusable components with a strong silhouette or close-up importance:

- crystal hold cages;
- distinctive root clusters;
- complex rune circles;
- boss-scale spell shapes;
- iconic charm crowns or masks;
- summoned objects;
- persistent environmental poison growth.

A Blender asset should be a component spawned by a procedural recipe, not a complete fixed-range spell. It should not encode whether the parent effect is single target, area, or cone.

## GameData boundary

When the runtime visual system is ready, effects may reference a visual profile through a stable ID:

```xml
<effect id="fire_damage" name="Fire Damage" kind="damage"
        skill_id="fire_damage" progression="skill_level"
        visual_id="fire" />
```

The visual profile should select executable recipes for each delivery family:

```text
fire:
  single_target -> projectile + impact
  aoe           -> target burst + ground ring
  pbaoe         -> caster burst + ground ring
  cone          -> flame fan
```

Keep implementation details such as raw shader names, particle implementation names, and mesh paths behind the visual profile catalog. The structured editor should offer visual profiles as references or dropdowns, not arbitrary implementation text.

Visual data may tune palette, timing, scale, and intensity, but Rust remains responsible for validation, target resolution, lifecycle, and effect execution.

## Iteration checklist

For each visual, verify:

- Can the player identify the caster and affected area immediately?
- Is the selected target or target-centered area obvious?
- Is a cone visibly different from a projectile or area burst?
- Does the scale follow range, radius, angle, magnitude, and actor size correctly?
- Can the player distinguish root, hold, snare, and charm without reading text?
- Does the visual remain readable with multiple actors and terrain clutter?
- Does the visual clean up reliably when the effect expires or is interrupted?
- Does it work from gameplay camera distance as well as close range?
- Is it performant with several concurrent effects?
- Does it avoid implying permanent faction changes for charm?

## Implementation stages

1. Prototype procedural primitives in the Engine: projectile, beam, arc, ring, decal, cone wedge, fog, tether, halo, restraint, and impact burst.
2. Implement fire and frost in all delivery families to validate scaling and readability.
3. Implement root and charm in all delivery families to validate persistent control and faction indicators.
4. Add lightning, poison, hold, and snare by reusing the primitive library.
5. Add a visual-profile catalog and structured GameData editor support.
6. Add selected Blender hero components only where procedural visuals remain insufficient.
7. Integrate VFX lifecycle with centralized Rust effect execution and runtime status state.

## Open decisions

- Should `aoe` and `pbaoe` have distinct authored visual profile IDs, or only distinct delivery recipes?
- Should control duration and intensity be added to action-effect assignments or to a separate status specification?
- Which palettes and silhouettes remain readable in the final terrain and lighting conditions?
- Should charm use a universal influence color or inherit the initiator's faction color?
- Which primitives belong in the Engine API, and which are Orrun-specific recipes?
- Which first-pass visuals need real transparency sorting or volumetric rendering support?
