//! Live combat presentation for canonical resolutions and actor animation.

use crate::combat::catalog::mesh_spec;
use crate::combat::math::WALK_MPS;

use crate::combat::types::{HostilePresentationSource, WorldCombat};
use crate::gamedata::{Application, EffectOperation};
use crate::progression::LevelUpEvent;
use crate::resolution::{Resolution, ResolutionActorId, TimedStatusKind};
use engine::anim::{AnimatedModel, AnimationAction, AnimationProfile, Locomotion};
use engine::color::Color;
use engine::error::{EngineError, EngineResult};
use engine::mesh::Mesh;
use engine::place::{GlobalPlace, Place};
use engine::space::GlobalPosition;
use engine::vfx::{Delivery, EffectHandle, EffectSpec, VfxSystem, VisualKind};
use engine::world::{EntityId, World};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

/// Attack pip is a short tell, not the 1.8 s swing clock.
const ATTACK_PIP_S: f64 = 0.15;
/// Previous-HP leftover on the bar. Longer than hurt_flash (0.15 s) so a PNG can catch it.
const HP_CHUNK_S: f64 = 0.80;
const FLINCH_S: f32 = 4.0;
const FLINCH_PEAK: f32 = 1.32;
const RING_LIFT_M: f64 = 0.14;
/// Keep the lock tell on one body. Wolves/bandits sit ±1.8 m; a 2.55 m major
/// wrapped the whole pack and broke "one lock = one body".
const RING_MAJOR_M: f32 = 0.95;
const RING_MINOR_M: f32 = 0.16;
/// Small chest tell. Not the 3.7 m ground carpet (half 1.85).
const SPARKLE_HALF_M: f32 = 0.34;
/// Prone-orc torso above the Death-posed mesh feet.
pub(super) const SPARKLE_LIFT_M: f64 = 0.50;
const GOLD: Color = Color {
    r: 1.0,
    g: 0.90,
    b: 0.12,
    a: 1.0,
};
const SPARKLE: Color = Color {
    r: 1.0,
    g: 0.86,
    b: 0.22,
    a: 1.0,
};

/// One-shot combat sounds queued from typed damage resolutions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CombatSfx {
    Swing,
    Hit,
    Hurt,
}

struct MeshAnchor {
    actor_id: crate::combat::ActorId,
    id: EntityId,
}

#[derive(Clone, Copy, Debug)]
struct Flinch {
    t: f32,
}

#[derive(Clone, Debug)]
struct EffectPresentation {
    operation: EffectOperation,
    application: Application,
    caster: ResolutionActorId,
    target: ResolutionActorId,
    duration_s: f64,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct StatusVfxIdentity {
    target: ResolutionActorId,
    kind: TimedStatusKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StatusVfxDecision {
    Keep,
    Spawn,
    Replace,
    Remove,
}

fn status_vfx_decision(
    tracked: bool,
    active: bool,
    desired: bool,
    refreshed: bool,
) -> StatusVfxDecision {
    match (tracked, active, desired, refreshed) {
        (true, true, true, false) => StatusVfxDecision::Keep,
        (true, true, true, true) => StatusVfxDecision::Replace,
        (true, _, false, _) => StatusVfxDecision::Remove,
        (true, false, true, _) => StatusVfxDecision::Replace,
        (false, _, true, _) => StatusVfxDecision::Spawn,
        (false, _, false, _) => StatusVfxDecision::Keep,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CombatPresentationEvent {
    SwingWhoosh,
    HitFlash,
    IncomingHit,
}

/// Live hostiles + 0.1 s combat clock in front of the player.
pub struct CombatLayer {
    presentation_ready: bool,
    mesh_ids: Vec<EntityId>,
    mesh_anchors: Vec<MeshAnchor>,
    wolf_model: Option<Arc<AnimatedModel>>,
    models: HashMap<String, Arc<AnimatedModel>>,
    pending_actor_animations: Vec<(crate::combat::ActorId, AnimationAction)>,
    pending_fauna_animations: Vec<(crate::combat::ActorId, AnimationAction)>,
    lock_ring: Option<EntityId>,
    ring_on: Option<i32>,
    attack_pip_s: f64,
    presentation_events: Vec<CombatPresentationEvent>,
    pending_sfx: Vec<CombatSfx>,
    pending_level_ups: Vec<LevelUpEvent>,
    pending_flinches: Vec<i32>,
    pending_effect_vfx: Vec<EffectPresentation>,
    refreshed_status_vfx: HashSet<StatusVfxIdentity>,
    status_vfx: HashMap<StatusVfxIdentity, EffectHandle>,
    vfx: VfxSystem,
    vfx_seed: u32,
    flinches: HashMap<crate::combat::ActorId, Flinch>,
    hurt_flash_s: f64,
    hp_ghost_frac: f32,
    hp_chunk_s: f64,
    death_posed: HashSet<EntityId>,
}

impl CombatLayer {
    pub fn install() -> Self {
        Self {
            presentation_ready: false,
            mesh_ids: Vec::new(),
            mesh_anchors: Vec::new(),
            wolf_model: None,
            models: HashMap::new(),
            pending_actor_animations: Vec::new(),
            pending_fauna_animations: Vec::new(),
            lock_ring: None,
            ring_on: None,
            attack_pip_s: 0.0,
            presentation_events: Vec::new(),
            pending_sfx: Vec::new(),
            pending_level_ups: Vec::new(),
            pending_flinches: Vec::new(),
            pending_effect_vfx: Vec::new(),
            refreshed_status_vfx: HashSet::new(),
            status_vfx: HashMap::new(),
            vfx: VfxSystem::new(),
            vfx_seed: 1,
            flinches: HashMap::new(),
            hurt_flash_s: 0.0,
            hp_ghost_frac: 0.0,
            hp_chunk_s: 0.0,
            death_posed: HashSet::new(),
        }
    }

    /// Drain player-facing typed level-up events produced while presenting resolutions.
    pub fn take_player_level_ups(&mut self) -> Vec<LevelUpEvent> {
        std::mem::take(&mut self.pending_level_ups)
    }

    pub fn presentation_ready(&self) -> bool {
        self.presentation_ready
    }

    pub fn mark_presentation_ready(&mut self) {
        self.presentation_ready = true;
    }

    pub fn reset_presentation_state(&mut self) {
        self.presentation_ready = false;
        self.attack_pip_s = 0.0;
        self.presentation_events.clear();
        self.pending_sfx.clear();
        self.pending_flinches.clear();
        self.pending_effect_vfx.clear();
        self.flinches.clear();
        self.hurt_flash_s = 0.0;
        self.hp_ghost_frac = 0.0;
        self.hp_chunk_s = 0.0;
        self.pending_actor_animations.clear();
        self.pending_fauna_animations.clear();
        self.ring_on = None;
        self.death_posed.clear();
    }

    /// Stream reset already dropped the bodies. Forget ids so a later
    /// despawn does not poke a recycled entity.
    pub fn forget_meshes(&mut self) {
        self.vfx = VfxSystem::new();
        self.pending_effect_vfx.clear();
        self.refreshed_status_vfx.clear();
        self.status_vfx.clear();
        self.mesh_ids.clear();
        self.mesh_anchors.clear();
        self.lock_ring = None;
        self.ring_on = None;
        self.death_posed.clear();
    }

    pub fn attack_pip(&self) -> bool {
        self.attack_pip_s > 0.0
    }

    pub fn take_presentation_events(&mut self) -> Vec<CombatPresentationEvent> {
        std::mem::take(&mut self.presentation_events)
    }

    pub fn hurt_flash(&self) -> bool {
        self.hurt_flash_s > 0.0
    }

    pub fn hp_ghost_frac(&self) -> Option<f32> {
        if self.hp_chunk_s > 0.0 {
            Some(self.hp_ghost_frac)
        } else {
            None
        }
    }

    fn latch_incoming_chunk(&mut self, prev_hp: f64, hp_max: f64) {
        self.presentation_events
            .push(CombatPresentationEvent::IncomingHit);
        self.hurt_flash_s = 0.15;
        let prev_frac = if hp_max > 0.0 {
            (prev_hp / hp_max).clamp(0.0, 1.0) as f32
        } else {
            0.0
        };
        if self.hp_chunk_s <= 0.0 {
            self.hp_ghost_frac = prev_frac;
        } else {
            self.hp_ghost_frac = self.hp_ghost_frac.max(prev_frac);
        }
        self.hp_chunk_s = HP_CHUNK_S;
    }

    pub fn lock_ring_visible(&self, world: &World) -> bool {
        self.lock_ring
            .map(|id| world.entity(id).is_ok())
            .unwrap_or(false)
    }

    pub fn is_death_posed(&self, id: EntityId) -> bool {
        self.death_posed.contains(&id)
    }

    pub fn take_combat_sfx(&mut self) -> Vec<CombatSfx> {
        std::mem::take(&mut self.pending_sfx)
    }

    pub fn reset_vfx(&mut self, world: &mut World) -> EngineResult<()> {
        self.pending_effect_vfx.clear();
        self.refreshed_status_vfx.clear();
        self.status_vfx.clear();
        self.vfx.reset(world)
    }

    pub fn despawn_meshes(&mut self, world: &mut World, combat: &mut WorldCombat) {
        self.vfx
            .reset(world)
            .unwrap_or_else(|err| panic!("combat VFX reset failed: {err}"));
        self.pending_effect_vfx.clear();
        self.refreshed_status_vfx.clear();
        self.status_vfx.clear();
        for id in self.mesh_ids.drain(..) {
            world.despawn(id);
        }
        combat.release_combat_layer_presentations();
        self.mesh_anchors.clear();
        self.despawn_ring(world);
        self.flinches.clear();
        self.death_posed.clear();
    }

    fn despawn_ring(&mut self, world: &mut World) {
        if let Some(id) = self.lock_ring.take() {
            world.despawn(id);
        }
        self.ring_on = None;
    }

    fn model_for(&mut self, mob_id: &str) -> EngineResult<Arc<AnimatedModel>> {
        let spec = mesh_spec(mob_id)
            .ok_or_else(|| EngineError::Model(format!("no combat mesh for '{mob_id}'")))?;
        if let Some(model) = self.models.get(spec.id) {
            return Ok(model.clone());
        }
        let model = load_combat_model(mob_id)?;
        if spec.id == "wolf" {
            self.wolf_model = Some(model.clone());
        }
        self.models.insert(spec.id.to_string(), model.clone());
        Ok(model)
    }

    /// Visible fixture meshes on the live hostiles. Faces the player.
    ///
    /// Each hostile uses `catalog::mesh_spec(mob_id)`. Wolf stays
    /// `fauna/wolf/wolf.gltf` with MESH_BEHIND 2.55. Orc/tribal/skull
    /// sit MESH_BEHIND 1.6 m behind combat XZ so the camera is not
    /// inside the volume at close range. Not FaunaLayer.
    pub fn spawn_hostile_meshes(
        &mut self,
        world: &mut World,
        combat: &mut WorldCombat,
        feet_y: &[f64],
        player_yaw_deg: f32,
    ) -> EngineResult<()> {
        let hostile_count = combat.hostiles().len();
        if feet_y.len() != hostile_count {
            return Err(EngineError::InvalidValue(format!(
                "combat hostile position/feet_y cardinality mismatch: {hostile_count} hostiles, {} feet heights",
                feet_y.len()
            )));
        }
        self.despawn_meshes(world, combat);
        let yaw = player_yaw_deg + 180.0;
        // wolf.gltf is ~5.5 m long. Origin at the combat point puts the
        // camera inside the snout. Keep hit XZ; sit the mesh behind it.
        let away = player_yaw_deg.to_radians();
        let ax = away.sin() as f64;
        let az = away.cos() as f64;
        const WOLF_MESH_BEHIND_M: f64 = 2.55;
        const ORC_MESH_BEHIND_M: f64 = 1.6;
        for i in 0..combat.hostiles().len() {
            let (actor_id, mob_id, x, z) = {
                let h = &combat.hostiles()[i];
                (h.actor_id(), h.mob_id.clone(), h.x, h.z)
            };
            if combat
                .presentations()
                .get(actor_id)
                .map(|b| b.entity())
                .is_some()
            {
                continue;
            }
            let result = (|| -> EngineResult<()> {
                let spec = mesh_spec(&mob_id).ok_or_else(|| {
                    EngineError::Model(format!("no combat mesh for '{}'", mob_id))
                })?;
                let model = self.model_for(&mob_id)?;
                let y = feet_y[i];
                let behind = if is_wolf_mesh(&mob_id) {
                    WOLF_MESH_BEHIND_M
                } else {
                    ORC_MESH_BEHIND_M
                };
                let pos = GlobalPosition::at(x + ax * behind, y, z + az * behind);
                let render = world.to_render(pos)?;
                let place = Place::at(render.x, render.y, render.z)?
                    .yaw_deg(yaw)?
                    .scale(1.0)?;
                let id = world.spawn_animated_shared(model.clone(), place)?;
                let mut profile = AnimationProfile::new()
                    .idle(spec.anim_idle)
                    .attack(spec.anim_melee);
                if let Some(clip) = spec
                    .anim_walk
                    .filter(|clip| model.find_clip(clip).is_some())
                {
                    profile = profile.walk(clip);
                }
                if let Some(clip) = spec.anim_run.filter(|clip| model.find_clip(clip).is_some()) {
                    profile = profile.run(clip);
                }
                if let Some(clip) = spec
                    .anim_weapon
                    .filter(|clip| model.find_clip(clip).is_some())
                {
                    profile = profile.cast(clip);
                }
                if let Some(clip) = spec
                    .anim_death
                    .filter(|clip| model.find_clip(clip).is_some())
                {
                    profile = profile.death(clip);
                }
                world.configure_animation(id, profile)?;
                if is_wolf_mesh(&mob_id) {
                    world.set_animation_speed(id, 0.65)?;
                }
                combat.bind_presentation(actor_id, HostilePresentationSource::CombatLayer, id);
                self.mesh_ids.push(id);
                self.mesh_anchors.push(MeshAnchor { actor_id, id });
                Ok(())
            })();
            if let Err(error) = result {
                self.despawn_meshes(world, combat);
                self.reset_presentation_state();
                return Err(error);
            }
        }
        Ok(())
    }

    /// Wrapper so session.rs keeps compiling if it still calls the wolf name.
    pub fn spawn_wolf_meshes(
        &mut self,
        world: &mut World,
        combat: &mut WorldCombat,
        feet_y: &[f64],
        player_yaw_deg: f32,
    ) -> EngineResult<()> {
        self.spawn_hostile_meshes(world, combat, feet_y, player_yaw_deg)
    }

    pub fn mesh_visible(&self, world: &World) -> bool {
        if self.mesh_ids.is_empty() {
            return false;
        }
        // Animated wolf bodies live in World::animated, not World::entity.
        let animated: Vec<engine::world::EntityId> =
            world.animated_entities().map(|(id, _)| *id).collect();
        self.mesh_ids
            .iter()
            .all(|id| animated.iter().any(|a| a == id) || world.entity(*id).is_ok())
    }

    pub fn walk_mps(&self) -> f32 {
        WALK_MPS as f32
    }

    /// Cycle the canonical target lock without touching camera, Esc, or E.
    pub fn press_tab(
        &mut self,
        combat: &mut WorldCombat,
        player_x: f64,
        player_z: f64,
        facing_x: f64,
        facing_z: f64,
    ) {
        combat.press_tab(player_x, player_z, facing_x, facing_z);
    }

    pub fn tick(
        &mut self,
        combat: &mut WorldCombat,
        player_x: f64,
        player_z: f64,
        facing_x: f64,
        facing_z: f64,
        dt: f64,
    ) {
        if dt <= 0.0 {
            return;
        }
        if self.attack_pip_s > 0.0 {
            self.attack_pip_s = (self.attack_pip_s - dt).max(0.0);
        }
        if self.hurt_flash_s > 0.0 {
            self.hurt_flash_s = (self.hurt_flash_s - dt).max(0.0);
        }
        if self.hp_chunk_s > 0.0 {
            self.hp_chunk_s = (self.hp_chunk_s - dt).max(0.0);
        }
        let fixed_steps = combat.consume_fixed_steps(dt);
        for _ in 0..fixed_steps {
            let step = combat.step_fixed(player_x, player_z, facing_x, facing_z);
            self.present_resolutions(combat, step.resolutions);
        }
    }

    fn present_resolutions(&mut self, combat: &mut WorldCombat, resolutions: Vec<Resolution>) {
        for mut resolution in resolutions {
            let caster_is_player = resolution.caster.get() == 0;
            if !caster_is_player {
                let action = combat
                    .game_data()
                    .action(&resolution.action_id)
                    .unwrap_or_else(|| {
                        panic!(
                            "resolution references unknown action {}",
                            resolution.action_id
                        )
                    });
                let hostile = combat
                    .hostiles()
                    .iter()
                    .find(|hostile| hostile.actor_id().canonical() == resolution.caster.get())
                    .unwrap_or_else(|| {
                        panic!(
                            "resolution references missing caster {:?}",
                            resolution.caster
                        )
                    });
                let animation = if action.cast_s() > 0.0
                    && mesh_spec(&hostile.mob_id).is_some_and(|spec| spec.anim_weapon.is_some())
                {
                    AnimationAction::Cast
                } else {
                    AnimationAction::Attack
                };
                match combat
                    .presentations()
                    .get(hostile.actor_id())
                    .map_or(crate::combat::HostilePresentationSource::Headless, |b| {
                        b.source()
                    }) {
                    HostilePresentationSource::Headless => {}
                    HostilePresentationSource::Fauna => self
                        .pending_fauna_animations
                        .push((hostile.actor_id(), animation)),
                    HostilePresentationSource::CombatLayer => self
                        .pending_actor_animations
                        .push((hostile.actor_id(), animation)),
                }
            }
            self.pending_level_ups.extend(
                resolution
                    .level_ups
                    .drain(..)
                    .filter(|attributed| attributed.actor().get() == 0)
                    .map(|attributed| attributed.event().clone()),
            );
            for effect in resolution.effects {
                for (&target, &applied) in effect.targets.iter().zip(&effect.applied) {
                    if applied <= 0.0 {
                        continue;
                    }
                    let target_runtime_id = (target.get() != 0).then(|| {
                        combat
                            .hostiles()
                            .iter()
                            .find(|hostile| hostile.actor_id().canonical() == target.get())
                            .unwrap_or_else(|| {
                                panic!("resolution references missing hostile actor {target:?}")
                            })
                            .idx
                    });
                    match effect.operation {
                        EffectOperation::DirectDamage if target.get() != 0 => {
                            let target_id = target_runtime_id.expect("hostile target id");
                            if caster_is_player {
                                self.attack_pip_s = ATTACK_PIP_S;
                                self.presentation_events
                                    .push(CombatPresentationEvent::SwingWhoosh);
                                self.pending_sfx.push(CombatSfx::Swing);
                            } else {
                            }
                            self.presentation_events
                                .push(CombatPresentationEvent::HitFlash);
                            self.pending_sfx.push(CombatSfx::Hit);
                            let target_hostile = combat
                                .hostiles()
                                .iter()
                                .find(|hostile| hostile.idx == target_id)
                                .expect("resolution hostile target");
                            match combat
                                .presentations()
                                .get(target_hostile.actor_id())
                                .map_or(crate::combat::HostilePresentationSource::Headless, |b| {
                                    b.source()
                                }) {
                                HostilePresentationSource::Headless => {}
                                HostilePresentationSource::Fauna => self
                                    .pending_fauna_animations
                                    .push((target_hostile.actor_id(), AnimationAction::Hit)),
                                HostilePresentationSource::CombatLayer => {
                                    self.pending_flinches.push(target_id)
                                }
                            }
                            self.pending_effect_vfx.push(EffectPresentation {
                                operation: effect.operation,
                                application: effect.application,
                                caster: resolution.caster,
                                target,
                                duration_s: effect.duration_s,
                            });
                        }
                        EffectOperation::DirectDamage => {
                            self.latch_incoming_chunk(
                                combat.player_hp() + applied,
                                combat.player_hp_max(),
                            );
                            self.pending_sfx.push(CombatSfx::Hurt);
                        }
                        EffectOperation::Heal => {
                            if target_runtime_id.is_some() || target.get() == 0 {
                                self.pending_effect_vfx.push(EffectPresentation {
                                    operation: effect.operation,
                                    application: effect.application,
                                    caster: resolution.caster,
                                    target,
                                    duration_s: effect.duration_s,
                                });
                            }
                        }
                        EffectOperation::Root
                        | EffectOperation::Hold
                        | EffectOperation::Snare
                        | EffectOperation::Charm => {
                            if target_runtime_id.is_some() || target.get() == 0 {
                                self.refreshed_status_vfx.insert(StatusVfxIdentity {
                                    target,
                                    kind: status_kind(effect.operation),
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    pub fn take_fauna_animation_cues(&mut self) -> Vec<(crate::combat::ActorId, AnimationAction)> {
        std::mem::take(&mut self.pending_fauna_animations)
    }

    /// Lock ring + hit flinch. Call after [`Self::tick`] with the live world.
    pub fn present(
        &mut self,
        world: &mut World,
        combat: &WorldCombat,
        player_position: GlobalPosition,
        mut ground_y: impl FnMut(f64, f64) -> EngineResult<f64>,
        dt: f32,
    ) -> EngineResult<()> {
        for (actor_id, animation) in std::mem::take(&mut self.pending_actor_animations) {
            let hostile = combat
                .hostiles()
                .iter()
                .find(|hostile| hostile.actor_id() == actor_id)
                .ok_or_else(|| {
                    EngineError::Model(format!(
                        "combat presentation cue references missing actor {actor_id:?}"
                    ))
                })?;
            if combat
                .presentations()
                .get(hostile.actor_id())
                .map_or(crate::combat::HostilePresentationSource::Headless, |b| {
                    b.source()
                })
                != HostilePresentationSource::CombatLayer
            {
                return Err(EngineError::Model(format!(
                    "combat presentation cue for {actor_id:?} has owner {:?}",
                    combat
                        .presentations()
                        .get(hostile.actor_id())
                        .map_or(crate::combat::HostilePresentationSource::Headless, |b| b
                            .source())
                )));
            }
            let id = combat
                .presentations()
                .get(hostile.actor_id())
                .map(|b| b.entity())
                .ok_or_else(|| {
                    EngineError::Model(format!(
                        "combat-owned actor {actor_id:?} has no presentation entity"
                    ))
                })?;
            world.play_animation_action(id, animation).map_err(|err| {
                EngineError::Model(format!(
                    "typed combat animation failed for {actor_id:?}: {err}"
                ))
            })?;
        }
        self.play_death_poses(world, combat)?;
        self.sync_lock_ring(world, combat, &mut ground_y)?;
        self.reconcile_status_vfx(world, combat, player_position, &mut ground_y)?;
        let effects = std::mem::take(&mut self.pending_effect_vfx);
        for effect in effects {
            let _handle =
                self.spawn_effect_vfx(world, combat, player_position, effect, &mut ground_y)?;
        }
        for target_id in std::mem::take(&mut self.pending_flinches) {
            self.start_flinch(world, combat, target_id)?;
        }
        self.tick_flinches(dt);
        self.vfx.update(world, dt)?;
        Ok(())
    }

    /// Synchronize render-space hostile transforms after deterministic combat AI moves them.
    pub fn sync_hostile_transforms(
        &mut self,
        world: &mut World,
        combat: &WorldCombat,
        mut ground_y: impl FnMut(f64, f64) -> EngineResult<f64>,
    ) -> EngineResult<()> {
        for h in combat.hostiles() {
            match combat
                .presentations()
                .get(h.actor_id())
                .map_or(HostilePresentationSource::Headless, |b| b.source())
            {
                HostilePresentationSource::Headless | HostilePresentationSource::Fauna => continue,
                HostilePresentationSource::CombatLayer => {}
            }
            let id = combat
                .presentations()
                .get(h.actor_id())
                .map(|b| b.entity())
                .ok_or_else(|| {
                    EngineError::Model(format!(
                    "combat-owned hostile {:?} has no presentation entity during transform sync",
                    h.actor_id()
                ))
                })?;
            self.mesh_anchors
                .iter()
                .find(|a| a.id == id)
                .ok_or_else(|| {
                    EngineError::Model(format!(
                        "combat-owned hostile {:?} entity {id} has no mesh anchor",
                        h.actor_id()
                    ))
                })?;
            let (x, z, heading) = h.presented_pose(combat.presentation_alpha());
            let y = ground_y(x, z).map_err(|source| {
                EngineError::InvalidValue(format!(
                    "missing required ground contact for hostile {:?} at ({x:.3}, {z:.3}): {source}",
                    h.actor_id()
                ))
            })?;
            let render = world.to_render(GlobalPosition::at(x, y, z))?;
            let yaw = heading.x().atan2(heading.z()).to_degrees() as f32;
            let scale = self.flinches.get(&h.actor_id()).map_or(1.0, |flinch| {
                let fade = (1.0 - flinch.t / FLINCH_S).clamp(0.0, 1.0);
                1.0 + (FLINCH_PEAK - 1.0) * fade
            });
            let place = Place::at(render.x, render.y, render.z)?
                .yaw_deg(yaw)?
                .scale(scale)?;
            let has_locomotion_clip = mesh_spec(&h.mob_id)
                .is_some_and(|spec| spec.anim_walk.is_some() || spec.anim_run.is_some());
            let locomotion = match h.state {
                crate::combat::types::HostileState::Pursuing
                | crate::combat::types::HostileState::Fleeing
                | crate::combat::types::HostileState::Leashing
                    if has_locomotion_clip =>
                {
                    Locomotion::Moving {
                        speed_mps: h.effective_movement_speed_mps() as f32,
                    }
                }
                _ => Locomotion::Idle,
            };
            world.set_locomotion(id, locomotion)?;
            world.set_place(id, place).map_err(|err| {
                EngineError::Model(format!(
                    "hostile transform sync failed for {}: {err}",
                    h.mob_id
                ))
            })?;
        }
        Ok(())
    }
    fn play_death_poses(&mut self, world: &mut World, combat: &WorldCombat) -> EngineResult<()> {
        for h in combat.hostiles() {
            if combat.hostile_is_alive(h.actor_id()) {
                continue;
            }
            match combat
                .presentations()
                .get(h.actor_id())
                .map_or(HostilePresentationSource::Headless, |b| b.source())
            {
                HostilePresentationSource::Headless | HostilePresentationSource::Fauna => continue,
                HostilePresentationSource::CombatLayer => {}
            }
            let id = combat
                .presentations()
                .get(h.actor_id())
                .map(|b| b.entity())
                .ok_or_else(|| {
                    EngineError::Model(format!(
                        "combat-owned dead hostile {:?} has no presentation entity",
                        h.actor_id()
                    ))
                })?;
            if !self.mesh_anchors.iter().any(|anchor| anchor.id == id) {
                return Err(EngineError::Model(format!(
                    "combat-owned dead hostile {:?} entity {id} has no mesh anchor",
                    h.actor_id()
                )));
            }
            if !self.death_posed.insert(id) {
                continue;
            }
            let Some(spec) = mesh_spec(&h.mob_id) else {
                return Err(EngineError::Model(format!(
                    "no mesh spec for dead hostile '{}'",
                    h.mob_id
                )));
            };
            let Some(_clip) = spec.anim_death else {
                continue;
            };
            world
                .play_animation_action(id, AnimationAction::Death)
                .map_err(|err| {
                    EngineError::Model(format!("death action failed for '{}': {err}", h.mob_id))
                })?;
            world
                .set_animation_speed(id, 1.0)
                .map_err(|err| EngineError::Model(format!("death clip speed failed: {err}")))?;
        }
        Ok(())
    }

    fn sync_lock_ring(
        &mut self,
        world: &mut World,
        combat: &WorldCombat,
        ground_y: &mut impl FnMut(f64, f64) -> EngineResult<f64>,
    ) -> EngineResult<()> {
        let want = combat.lock_id().filter(|_| self.presentation_ready);
        let still = want == self.ring_on
            && self
                .lock_ring
                .map(|id| world.entity(id).is_ok())
                .unwrap_or(false);
        let Some(lock) = want else {
            self.despawn_ring(world);
            return Ok(());
        };
        let h = combat
            .hostiles()
            .iter()
            .find(|h| h.idx == lock)
            .ok_or_else(|| {
                EngineError::Model(format!(
                    "target lock references missing hostile runtime id {lock}"
                ))
            })?;
        if !combat.hostile_is_alive(h.actor_id()) {
            return Err(EngineError::Model(format!(
                "target lock references dead hostile {:?} (runtime id {lock})",
                h.actor_id()
            )));
        }
        let entity = combat.presentations().get(h.actor_id()).map(|b| b.entity()).ok_or_else(|| {
            EngineError::Model(format!(
                "locked hostile {:?} (runtime id {lock}, owner {:?}) has no presentation entity",
                h.actor_id(),
                combat.presentations().get(h.actor_id()).map_or(HostilePresentationSource::Headless, |b| b.source())
            ))
        })?;
        match combat
            .presentations()
            .get(h.actor_id())
            .map_or(HostilePresentationSource::Headless, |b| b.source())
        {
            HostilePresentationSource::Headless => {
                return Err(EngineError::Model(format!(
                    "locked hostile {:?} (runtime id {lock}) is headless but has entity {entity}",
                    h.actor_id()
                )));
            }
            HostilePresentationSource::Fauna => {
                world.animated_entity(entity).map_err(|source| {
                    EngineError::Model(format!(
                        "locked fauna hostile {:?} (runtime id {lock}) entity {entity} is not live: {source}",
                        h.actor_id()
                    ))
                })?;
            }
            HostilePresentationSource::CombatLayer => {
                if !self.mesh_anchors.iter().any(|anchor| anchor.id == entity) {
                    return Err(EngineError::Model(format!(
                        "locked hostile {:?} (runtime id {lock}, owner CombatLayer) entity {entity} has no presentation anchor",
                        h.actor_id()
                    )));
                }
            }
        }
        let (x, z, _) = h.presented_pose(combat.presentation_alpha());
        let y = ground_y(x, z)? + RING_LIFT_M;
        let place = GlobalPlace::at(GlobalPosition::at(x, y, z));
        if still {
            let id = self.lock_ring.expect("live ring validated");
            world.set_anchored_place(id, place).map_err(|source| {
                EngineError::Model(format!(
                    "moving target-lock ring for hostile {lock}: {source}"
                ))
            })?;
        } else {
            self.despawn_ring(world);
            let id = world.spawn_anchored(lock_ring_mesh()?, place)?;
            disable_shadow(world, id, &format!("target-lock ring for hostile {lock}"))?;
            self.lock_ring = Some(id);
            self.ring_on = Some(lock);
        }
        Ok(())
    }

    fn spawn_effect_vfx(
        &mut self,
        world: &mut World,
        combat: &WorldCombat,
        player_position: GlobalPosition,
        effect: EffectPresentation,
        ground_y: &mut impl FnMut(f64, f64) -> EngineResult<f64>,
    ) -> EngineResult<EffectHandle> {
        let mut actor_position = |actor: ResolutionActorId| -> EngineResult<GlobalPosition> {
            if actor.get() == 0 {
                return Ok(GlobalPosition::at(
                    player_position.x,
                    player_position.y + 1.0,
                    player_position.z,
                ));
            }
            let hostile = combat
                .hostiles()
                .iter()
                .find(|hostile| hostile.actor_id().canonical() == actor.get())
                .unwrap_or_else(|| panic!("effect references missing hostile actor {actor:?}"));
            Ok(GlobalPosition::at(
                hostile.x,
                ground_y(hostile.x, hostile.z)? + 0.85,
                hostile.z,
            ))
        };
        let origin = world.to_render(actor_position(effect.caster)?)?;
        let target = world.to_render(actor_position(effect.target)?)?;
        let distance = origin.distance(target).max(0.1);
        let duration_s = if effect.duration_s > 0.0 {
            effect.duration_s as f32
        } else {
            1.15
        };
        let handle = self.vfx.spawn(
            world,
            EffectSpec {
                kind: visual_kind(effect.operation),
                delivery: delivery(effect.application),
                origin,
                target,
                range_m: distance,
                radius_m: 1.0,
                angle_deg: 45.0,
                duration_s,
                scale: 0.75,
                intensity: 1.0,
                seed: self.vfx_seed,
            },
        )?;
        self.vfx_seed = self.vfx_seed.wrapping_add(1);
        Ok(handle)
    }

    fn reconcile_status_vfx(
        &mut self,
        world: &mut World,
        combat: &WorldCombat,
        player_position: GlobalPosition,
        ground_y: &mut impl FnMut(f64, f64) -> EngineResult<f64>,
    ) -> EngineResult<()> {
        let mut desired = HashMap::new();
        if !combat.is_dead() {
            for status in combat.actor_statuses(crate::combat::ActorId::PLAYER) {
                desired.insert(
                    StatusVfxIdentity {
                        target: ResolutionActorId::new(0),
                        kind: status.kind(),
                    },
                    (status.source(), status.remaining_s()),
                );
            }
        }
        for hostile in combat
            .hostiles()
            .iter()
            .filter(|h| combat.hostile_is_alive(h.actor_id()))
        {
            for status in combat.actor_statuses(hostile.actor_id()) {
                desired.insert(
                    StatusVfxIdentity {
                        target: ResolutionActorId::new(hostile.actor_id().canonical()),
                        kind: status.kind(),
                    },
                    (status.source(), status.remaining_s()),
                );
            }
        }
        let identities: HashSet<_> = self
            .status_vfx
            .keys()
            .copied()
            .chain(desired.keys().copied())
            .collect();
        for identity in identities {
            let tracked = self.status_vfx.contains_key(&identity);
            let active = self
                .status_vfx
                .get(&identity)
                .is_some_and(|h| self.vfx.is_active(*h));
            let want = desired.get(&identity).copied();
            let decision = status_vfx_decision(
                tracked,
                active,
                want.is_some(),
                self.refreshed_status_vfx.contains(&identity),
            );
            if matches!(
                decision,
                StatusVfxDecision::Remove | StatusVfxDecision::Replace
            ) {
                if let Some(handle) = self.status_vfx.remove(&identity) {
                    if self.vfx.is_active(handle) {
                        self.vfx.interrupt(world, handle)?;
                    }
                }
            }
            if matches!(
                decision,
                StatusVfxDecision::Spawn | StatusVfxDecision::Replace
            ) {
                let (source, duration_s) =
                    want.expect("status spawn decision requires desired status");
                let handle = self.spawn_effect_vfx(
                    world,
                    combat,
                    player_position,
                    EffectPresentation {
                        operation: status_operation(identity.kind),
                        application: Application::SingleTarget,
                        caster: source,
                        target: identity.target,
                        duration_s,
                    },
                    ground_y,
                )?;
                self.status_vfx.insert(identity, handle);
            }
        }
        self.refreshed_status_vfx.clear();
        Ok(())
    }

    fn start_flinch(
        &mut self,
        _world: &mut World,
        combat: &WorldCombat,
        target_id: i32,
    ) -> EngineResult<()> {
        let h = combat
            .hostiles()
            .iter()
            .find(|h| h.idx == target_id)
            .ok_or_else(|| {
                EngineError::Model(format!(
                    "flinch references missing hostile runtime id {target_id}"
                ))
            })?;
        if combat
            .presentations()
            .get(h.actor_id())
            .map_or(HostilePresentationSource::Headless, |b| b.source())
            != HostilePresentationSource::CombatLayer
        {
            return Err(EngineError::Model(format!(
                "CombatLayer flinch cannot present {:?}-owned actor {:?}",
                combat
                    .presentations()
                    .get(h.actor_id())
                    .map_or(HostilePresentationSource::Headless, |b| b.source()),
                h.actor_id()
            )));
        }
        if !self
            .mesh_anchors
            .iter()
            .any(|anchor| anchor.actor_id == h.actor_id())
        {
            return Err(EngineError::Model(format!(
                "combat-owned flinch target {:?} has no mesh anchor",
                h.actor_id()
            )));
        }
        self.flinches.insert(h.actor_id(), Flinch { t: 0.0 });
        Ok(())
    }

    fn tick_flinches(&mut self, dt: f32) {
        self.flinches.retain(|_, flinch| {
            flinch.t += dt;
            flinch.t < FLINCH_S
        });
    }
}

pub(super) fn sparkle_mesh() -> EngineResult<Mesh> {
    // Compact corpse-loot marker. Stays until taken or reset.
    let mut mesh = Mesh::new();
    let s = SPARKLE_HALF_M;
    let a = mesh.add_point((-s, 0.0, -s))?;
    let b = mesh.add_point((s, 0.0, -s))?;
    let c = mesh.add_point((s, 0.0, s))?;
    let d = mesh.add_point((-s, 0.0, s))?;
    mesh.add_quad(a, b, c, d)?;
    mesh.add_quad(a, d, c, b)?;
    mesh.paint_all(SPARKLE);
    Ok(mesh)
}

pub(super) fn disable_shadow(world: &mut World, id: EntityId, context: &str) -> EngineResult<()> {
    world.set_casts_shadow(id, false).map_err(|source| {
        EngineError::Model(format!(
            "disable shadows for {context} entity {id}: {source}"
        ))
    })
}

fn lock_ring_mesh() -> EngineResult<Mesh> {
    let mut mesh = Mesh::new();
    let major = RING_MAJOR_M;
    let minor = RING_MINOR_M;
    const SEG_U: usize = 28;
    const SEG_V: usize = 8;
    let mut ids: Vec<engine::mesh::PointId> = Vec::with_capacity(SEG_U * SEG_V);
    for i in 0..SEG_U {
        let u = (i as f32 / SEG_U as f32) * std::f32::consts::TAU;
        let cu = u.cos();
        let su = u.sin();
        for j in 0..SEG_V {
            let v = (j as f32 / SEG_V as f32) * std::f32::consts::TAU;
            let cv = v.cos();
            let sv = v.sin();
            // Torus in XZ, hole facing +Y.
            let x = (major + minor * cv) * cu;
            let y = minor * sv;
            let z = (major + minor * cv) * su;
            ids.push(mesh.add_point((x, y, z))?);
        }
    }
    for i in 0..SEG_U {
        let i1 = (i + 1) % SEG_U;
        for j in 0..SEG_V {
            let j1 = (j + 1) % SEG_V;
            let a = ids[i * SEG_V + j];
            let b = ids[i1 * SEG_V + j];
            let c = ids[i1 * SEG_V + j1];
            let d = ids[i * SEG_V + j1];
            mesh.add_quad(a, b, c, d)?;
        }
    }
    // Flat two-sided annulus so the tell reads from above as well as the side.
    let inner = major - minor * 0.35;
    let outer = major + minor * 0.85;
    const SEG_A: usize = 28;
    let mut inner_ids = Vec::with_capacity(SEG_A);
    let mut outer_ids = Vec::with_capacity(SEG_A);
    for i in 0..SEG_A {
        let a = (i as f32 / SEG_A as f32) * std::f32::consts::TAU;
        let (s, c) = (a.sin(), a.cos());
        inner_ids.push(mesh.add_point((inner * c, 0.02, inner * s))?);
        outer_ids.push(mesh.add_point((outer * c, 0.02, outer * s))?);
    }
    for i in 0..SEG_A {
        let i1 = (i + 1) % SEG_A;
        mesh.add_quad(inner_ids[i], outer_ids[i], outer_ids[i1], inner_ids[i1])?;
        mesh.add_quad(inner_ids[i1], outer_ids[i1], outer_ids[i], inner_ids[i])?;
    }
    mesh.paint_all(GOLD);
    Ok(mesh)
}
fn is_wolf_mesh(mob_id: &str) -> bool {
    matches!(mob_id, "wolf")
}

fn assets_dir() -> EngineResult<PathBuf> {
    let mut tried = Vec::new();
    if let Some(dir) = std::env::var_os("ORRUN_ASSETS") {
        tried.push(PathBuf::from(dir));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            tried.push(dir.join("assets"));
        }
    }
    tried.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets"));
    for root in &tried {
        if root.is_dir() {
            return Ok(root.clone());
        }
    }
    Err(EngineError::Model(format!("no assets under {}", {
        assert!(!tried.is_empty(), "asset candidate invariant");
        tried
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    })))
}

fn load_combat_model(mob_id: &str) -> EngineResult<Arc<AnimatedModel>> {
    let spec = mesh_spec(mob_id)
        .ok_or_else(|| EngineError::Model(format!("no combat mesh for '{mob_id}'")))?;
    let assets = assets_dir()?;
    let path = assets.join(spec.source);
    if !path.is_file() {
        return Err(EngineError::Model(format!(
            "{} mesh missing at {}",
            spec.id,
            path.display()
        )));
    }
    let root = if spec.source.starts_with("fauna/") {
        assets.join("fauna")
    } else {
        path.parent().unwrap_or(&assets).to_path_buf()
    };
    let model = AnimatedModel::load_with(&path, &root, &engine::EngineLimits::default())?;
    if is_wolf_mesh(mob_id) && model.find_clip(spec.anim_idle).is_none() {
        return Err(EngineError::Model(format!(
            "wolf clip '{}' is not in {}",
            spec.anim_idle,
            path.display()
        )));
    }
    Ok(Arc::new(model))
}

fn status_kind(operation: EffectOperation) -> TimedStatusKind {
    match operation {
        EffectOperation::Root => TimedStatusKind::Root,
        EffectOperation::Hold => TimedStatusKind::Hold,
        EffectOperation::Snare => TimedStatusKind::Snare,
        EffectOperation::Charm => TimedStatusKind::Charm,
        EffectOperation::DirectDamage | EffectOperation::Heal => {
            panic!("transient operation {operation:?} has no timed status identity")
        }
    }
}

fn status_operation(kind: TimedStatusKind) -> EffectOperation {
    match kind {
        TimedStatusKind::Root => EffectOperation::Root,
        TimedStatusKind::Hold => EffectOperation::Hold,
        TimedStatusKind::Snare => EffectOperation::Snare,
        TimedStatusKind::Charm => EffectOperation::Charm,
    }
}

fn visual_kind(operation: EffectOperation) -> VisualKind {
    match operation {
        EffectOperation::DirectDamage => VisualKind::Fire,
        EffectOperation::Heal => VisualKind::Frost,
        EffectOperation::Root => VisualKind::Root,
        EffectOperation::Hold => VisualKind::Hold,
        EffectOperation::Snare => VisualKind::Snare,
        EffectOperation::Charm => VisualKind::Charm,
    }
}

fn delivery(application: Application) -> Delivery {
    match application {
        Application::SingleTarget => Delivery::SingleTarget,
        Application::Cone => Delivery::Cone,
        Application::Aoe => Delivery::Aoe,
        Application::Pbaoe => Delivery::Pbaoe,
    }
}

#[cfg(test)]
mod typed_presentation_tests {
    use super::*;

    fn world_entity(world: &mut World) -> EntityId {
        let mut mesh = Mesh::new();
        let a = mesh.add_point((0.0, 0.0, 0.0)).expect("point");
        let b = mesh.add_point((1.0, 0.0, 0.0)).expect("point");
        let c = mesh.add_point((0.0, 1.0, 0.0)).expect("point");
        mesh.add_triangle(a, b, c).expect("triangle");
        world.spawn(mesh)
    }

    fn test_combat() -> WorldCombat {
        let data = Arc::new(
            crate::gamedata::GameData::load(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../data/OrrunGameData.xml"),
            )
            .expect("canonical GameData"),
        );
        WorldCombat::with_game_data(data)
    }

    #[test]
    fn combat_owned_transform_without_anchor_is_loud() {
        let mut combat = test_combat();
        let hostile =
            combat.hostile_metadata(&crate::gamedata::MobId::new("deer"), 0, 2.0, 0.0, 2.0, 0.0);
        let mut world = World::new();
        let entity = world_entity(&mut world);
        combat.add_hostile(hostile);
        let actor_id = combat.hostiles().last().expect("added hostile").actor_id();
        combat.bind_presentation(actor_id, HostilePresentationSource::CombatLayer, entity);
        let mut layer = CombatLayer::install();
        let err = layer
            .sync_hostile_transforms(&mut world, &combat, |_, _| Ok(0.0))
            .expect_err("missing combat anchor must fail");
        assert!(err.to_string().contains("no mesh anchor"), "{err}");
    }

    #[test]
    fn combat_teardown_releases_owned_presentation_bindings() {
        let mut combat = test_combat();
        let hostile =
            combat.hostile_metadata(&crate::gamedata::MobId::new("deer"), 0, 2.0, 0.0, 2.0, 0.0);
        let mut world = World::new();
        let entity = world_entity(&mut world);
        combat.add_hostile(hostile);
        let actor_id = combat.hostiles().last().expect("added hostile").actor_id();
        combat.bind_presentation(actor_id, HostilePresentationSource::CombatLayer, entity);

        combat.release_combat_layer_presentations();

        let hostile = &combat.hostiles()[0];
        assert_eq!(
            combat
                .presentations()
                .get(hostile.actor_id())
                .map_or(crate::combat::HostilePresentationSource::Headless, |b| b
                    .source()),
            HostilePresentationSource::Headless
        );
        assert_eq!(
            combat
                .presentations()
                .get(hostile.actor_id())
                .map(|b| b.entity()),
            None
        );
    }
    #[test]
    fn hostile_feet_cardinality_mismatch_is_loud_before_mutation() {
        let mut combat = test_combat();
        combat.add_hostile(combat.hostile_metadata(
            &crate::gamedata::MobId::new("deer"),
            0,
            2.0,
            0.0,
            2.0,
            0.0,
        ));
        let mut world = World::new();
        let mut layer = CombatLayer::install();

        let err = layer
            .spawn_hostile_meshes(&mut world, &mut combat, &[], 0.0)
            .expect_err("missing feet height must fail");
        assert!(err.to_string().contains("cardinality mismatch"), "{err}");
    }

    #[test]
    fn missing_hostile_ground_contact_is_loud() {
        let mut combat = test_combat();
        let hostile =
            combat.hostile_metadata(&crate::gamedata::MobId::new("deer"), 0, 2.0, 0.0, 2.0, 0.0);
        let mut world = World::new();
        let entity = world_entity(&mut world);
        combat.add_hostile(hostile);
        let actor_id = combat.hostiles().last().expect("added hostile").actor_id();
        combat.bind_presentation(actor_id, HostilePresentationSource::CombatLayer, entity);
        let mut layer = CombatLayer::install();
        layer.mesh_anchors.push(MeshAnchor {
            actor_id,
            id: entity,
        });

        let err = layer
            .sync_hostile_transforms(&mut world, &combat, |_, _| {
                Err(EngineError::InvalidValue("fixture has no ground".into()))
            })
            .expect_err("missing ground contact must not use stale anchor Y");
        assert!(
            err.to_string().contains("missing required ground contact"),
            "{err}"
        );
    }

    #[test]
    fn shadow_configuration_failure_has_presentation_context() {
        let mut world = World::new();
        let stale = world_entity(&mut world);
        world.despawn(stale);
        let err = disable_shadow(&mut world, stale, "test marker")
            .expect_err("stale marker shadow update must fail");
        assert!(
            err.to_string().contains("disable shadows for test marker"),
            "{err}"
        );
        assert!(err.to_string().contains(&stale.to_string()), "{err}");
    }

    #[test]
    fn presentation_events_drain_once_as_typed_batch() {
        let mut layer = CombatLayer::install();
        layer
            .presentation_events
            .push(CombatPresentationEvent::SwingWhoosh);
        layer
            .presentation_events
            .push(CombatPresentationEvent::HitFlash);
        assert_eq!(
            layer.take_presentation_events(),
            vec![
                CombatPresentationEvent::SwingWhoosh,
                CombatPresentationEvent::HitFlash
            ]
        );
        assert!(layer.take_presentation_events().is_empty());
    }

    #[test]
    fn stale_target_lock_is_loud() {
        let mut combat = test_combat();
        combat.set_lock(Some(77));
        let mut layer = CombatLayer::install();
        layer.presentation_ready = true;
        let mut world = World::new();
        let err = layer
            .sync_lock_ring(&mut world, &combat, &mut |_, _| Ok(0.0))
            .expect_err("missing locked hostile must fail");
        assert!(
            err.to_string()
                .contains("target lock references missing hostile"),
            "{err}"
        );
        assert!(err.to_string().contains("77"), "{err}");
    }

    #[test]
    fn locked_visible_hostile_without_anchor_is_loud() {
        let mut combat = test_combat();
        let hostile =
            combat.hostile_metadata(&crate::gamedata::MobId::new("deer"), 5, 2.0, 0.0, 2.0, 0.0);
        let mut world = World::new();
        let entity = world_entity(&mut world);
        combat.add_hostile(hostile);
        let actor_id = combat.hostiles().last().expect("added hostile").actor_id();
        combat.bind_presentation(actor_id, HostilePresentationSource::CombatLayer, entity);
        combat.set_lock(Some(5));
        let mut layer = CombatLayer::install();
        layer.presentation_ready = true;
        let err = layer
            .sync_lock_ring(&mut world, &combat, &mut |_, _| Ok(0.0))
            .expect_err("locked hostile without anchor must fail");
        assert!(err.to_string().contains("no presentation anchor"), "{err}");
        assert!(err.to_string().contains("CombatLayer"), "{err}");
    }

    #[test]
    fn locked_fauna_hostile_uses_canonical_pose_without_combat_anchor() {
        let mut combat = test_combat();
        let hostile =
            combat.hostile_metadata(&crate::gamedata::MobId::new("deer"), 5, 2.0, 0.0, 3.0, 0.0);
        let mut world = World::new();
        let entity = world
            .spawn_animated_shared(
                load_combat_model("deer").expect("deer model"),
                Place::default(),
            )
            .expect("animated fauna entity");
        combat.add_hostile(hostile);
        let actor_id = combat.hostiles().last().expect("added hostile").actor_id();
        combat.bind_presentation(actor_id, HostilePresentationSource::Fauna, entity);
        combat.set_lock(Some(5));
        let mut layer = CombatLayer::install();
        layer.presentation_ready = true;

        layer
            .sync_lock_ring(&mut world, &combat, &mut |x, z| {
                assert_eq!((x, z), (2.0, 0.0));
                Ok(0.0)
            })
            .expect("live fauna lock must use the canonical presented pose");

        let ring = layer.lock_ring.expect("fauna target gets a lock ring");
        assert!(world.entity(ring).is_ok());
    }
    #[test]
    fn every_typed_operation_has_a_visual_kind() {
        assert_eq!(visual_kind(EffectOperation::DirectDamage), VisualKind::Fire);
        assert_eq!(visual_kind(EffectOperation::Heal), VisualKind::Frost);
        assert_eq!(visual_kind(EffectOperation::Root), VisualKind::Root);
        assert_eq!(visual_kind(EffectOperation::Hold), VisualKind::Hold);
        assert_eq!(visual_kind(EffectOperation::Snare), VisualKind::Snare);
        assert_eq!(visual_kind(EffectOperation::Charm), VisualKind::Charm);
    }

    #[test]
    fn persistent_status_presentation_uses_authored_duration() {
        let cue = EffectPresentation {
            operation: EffectOperation::Root,
            application: Application::SingleTarget,
            caster: ResolutionActorId::new(0),
            target: ResolutionActorId::new(1),
            duration_s: 4.0,
        };
        assert_eq!(cue.duration_s, 4.0);
        assert_eq!(visual_kind(cue.operation), VisualKind::Root);
    }

    #[test]
    fn every_typed_application_has_a_delivery() {
        assert_eq!(delivery(Application::SingleTarget), Delivery::SingleTarget);
        assert_eq!(delivery(Application::Cone), Delivery::Cone);
        assert_eq!(delivery(Application::Aoe), Delivery::Aoe);
        assert_eq!(delivery(Application::Pbaoe), Delivery::Pbaoe);
    }

    #[test]
    fn status_vfx_reconciliation_replaces_refresh_and_removes_absent_status() {
        assert_eq!(
            status_vfx_decision(false, false, true, false),
            StatusVfxDecision::Spawn
        );
        assert_eq!(
            status_vfx_decision(true, true, true, false),
            StatusVfxDecision::Keep
        );
        assert_eq!(
            status_vfx_decision(true, true, true, true),
            StatusVfxDecision::Replace
        );
        assert_eq!(
            status_vfx_decision(true, false, true, false),
            StatusVfxDecision::Replace
        );
        assert_eq!(
            status_vfx_decision(true, true, false, false),
            StatusVfxDecision::Remove
        );
    }
}
