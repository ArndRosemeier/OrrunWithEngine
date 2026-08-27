//! Live combat layer. Not FaunaLayer ? fixture hostiles, combat clock, lock+auto.
//!
//! Uses `orrun::combat` types and the same melee_raw / mitigation as the sim.
//! Frame dt is not 0.1; this accumulates to [`crate::combat::TICK`] before a
//! live auto tick.
//!
//! Visible fixture bodies use catalog id `wolf` (`assets/fauna/wolf/wolf.gltf`).
//! There is no spider in the fauna catalog; do not route these through FaunaLayer.

use crate::combat::catalog::mesh_spec;
use crate::combat::math::{MAGE_TELE_S, SKULL_TELE_S, WALK_MPS};
use crate::combat::sheets::{
    blue_demon_sheet, demon_sheet, orc_sheet, orc_skull_sheet, skeleton_mage_sheet,
    skeleton_minion_sheet, skeleton_warrior_sheet, tribal_veteran_sheet, wolf_sheet, yeti_sheet,
    MobSheet,
};
use crate::combat::types::{SpecialAttackCue, SpecialAttackEvent, WorldCombat, WorldHostile};
use crate::combat::Discipline;
use crate::gamedata::EffectKind;
use crate::progression::LevelUpEvent;
use crate::resolution::Resolution;
use engine::anim::{AnimatedModel, AnimationAction, AnimationProfile, Locomotion};
use engine::color::Color;
use engine::error::{EngineError, EngineResult};
use engine::mesh::Mesh;
use engine::place::{GlobalPlace, Place};
use engine::space::{GlobalPosition, GlobalXZ};
use engine::vfx::{Delivery, EffectSpec, VfxSystem, VisualKind};
use engine::world::{EntityId, World};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

/// Attack pip is a short tell, not the 1.8 s swing clock.
const ATTACK_PIP_S: f64 = 0.15;
/// Previous-HP leftover on the bar. Longer than hurt_flash (0.15 s) so a PNG can catch it.
const HP_CHUNK_S: f64 = 0.80;
const STRAFE_M: f64 = 1.8;
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
const SPARKLE_LIFT_M: f64 = 0.50;
const GOLD: Color = Color {
    r: 1.0,
    g: 0.90,
    b: 0.12,
    a: 1.0,
};
const FLASH_S: f32 = 6.0;
const FLASH: Color = Color {
    r: 1.0,
    g: 0.96,
    b: 0.55,
    a: 1.0,
};
const SPARKLE: Color = Color {
    r: 1.0,
    g: 0.86,
    b: 0.22,
    a: 1.0,
};

/// One-shot combat voices queued on a live auto that deals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CombatSfx {
    Swing,
    Hit,
    Hurt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FixtureKind {
    /// No published pack; overland sites may seat.
    None,
    CanonicalMob,
    WolfLine,
    HeldMobs,
    Orc,
    Bones,
    Mage,
    Yeti,
    Demon,
    BlueDemon,
    TribalVeteran,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FixtureActivation {
    NormalWorld,
    Held,
    Active,
}

#[derive(Clone, Debug)]
pub struct HeldMobFixture {
    mob_id: crate::gamedata::MobId,
    forward_m: f64,
    right_m: f64,
}

impl HeldMobFixture {
    pub fn new(mob_id: crate::gamedata::MobId, forward_m: f64, right_m: f64) -> Self {
        if !forward_m.is_finite() || !right_m.is_finite() {
            panic!("held mob fixture offsets must be finite");
        }
        Self {
            mob_id,
            forward_m,
            right_m,
        }
    }
}

struct MeshAnchor {
    id: EntityId,
    pos: GlobalPosition,
    yaw: f32,
}

struct Flinch {
    id: EntityId,
    pos: GlobalPosition,
    yaw: f32,
    t: f32,
}

/// Live hostiles + 0.1 s combat clock in front of the player.
pub struct CombatLayer {
    fixture: bool,
    skip_roster_pins: bool,
    first_auto: Option<i32>,
    mesh_ids: Vec<EntityId>,
    mesh_anchors: Vec<MeshAnchor>,
    wolf_model: Option<Arc<AnimatedModel>>,
    models: HashMap<String, Arc<AnimatedModel>>,
    fixture_kind: FixtureKind,
    fixture_activation: FixtureActivation,
    held_mobs: Vec<HeldMobFixture>,
    pending_melee: Option<(Option<EntityId>, &'static str)>,
    skull_tele: HashMap<i32, f64>,
    mage_tele: HashMap<i32, f64>,
    lock_ring: Option<EntityId>,
    ring_on: Option<i32>,
    attack_pip_s: f64,
    swing_whoosh: bool,
    hit_flash: bool,
    pending_sfx: Vec<CombatSfx>,
    pending_level_ups: Vec<LevelUpEvent>,
    pending_flinch: Option<i32>,
    pending_fire_bolts: Vec<i32>,
    vfx: VfxSystem,
    vfx_seed: u32,
    flinch: Option<Flinch>,
    flash: Option<EntityId>,
    flash_t: f32,
    incoming_hit: bool,
    hurt_flash_s: f64,
    hp_ghost_frac: f32,
    hp_chunk_s: f64,
    death_posed: HashSet<EntityId>,
    sparkles: HashMap<i32, EntityId>,
}

impl CombatLayer {
    pub fn install() -> Self {
        Self {
            fixture: false,
            skip_roster_pins: false,
            first_auto: None,
            mesh_ids: Vec::new(),
            mesh_anchors: Vec::new(),
            wolf_model: None,
            models: HashMap::new(),
            fixture_kind: FixtureKind::None,
            fixture_activation: FixtureActivation::NormalWorld,
            held_mobs: Vec::new(),
            pending_melee: None,
            skull_tele: HashMap::new(),
            mage_tele: HashMap::new(),
            lock_ring: None,
            ring_on: None,
            attack_pip_s: 0.0,
            swing_whoosh: false,
            hit_flash: false,
            pending_sfx: Vec::new(),
            pending_level_ups: Vec::new(),
            pending_flinch: None,
            pending_fire_bolts: Vec::new(),
            vfx: VfxSystem::new(),
            vfx_seed: 1,
            flinch: None,
            flash: None,
            flash_t: 0.0,
            incoming_hit: false,
            hurt_flash_s: 0.0,
            hp_ghost_frac: 0.0,
            hp_chunk_s: 0.0,
            death_posed: HashSet::new(),
            sparkles: HashMap::new(),
        }
    }

    /// Drain player-facing typed level-up events produced while presenting resolutions.
    pub fn take_player_level_ups(&mut self) -> Vec<LevelUpEvent> {
        std::mem::take(&mut self.pending_level_ups)
    }

    pub fn fixture_ready(&self) -> bool {
        self.fixture
    }

    pub fn fixture_encounter_held(&self) -> bool {
        self.fixture_activation == FixtureActivation::Held
    }

    pub fn canonical_mob_fixture(&self) -> bool {
        self.fixture_kind == FixtureKind::CanonicalMob
    }

    /// Release a fully seated presentation fixture into authoritative combat.
    pub fn start_fixture_encounter(&mut self, combat: &mut WorldCombat) {
        if !self.fixture {
            panic!("fixture encounter cannot start before hostiles are seated");
        }
        if self.fixture_activation != FixtureActivation::Held {
            panic!("fixture encounter start requires a held fixture");
        }
        combat.reset_for_fixture_start();
        self.first_auto = None;
        self.incoming_hit = false;
        self.fixture_activation = FixtureActivation::Active;
    }

    /// Arm the existing hold gate for a typed canonical-mob fixture.
    pub fn request_canonical_mob_fixture(&mut self) {
        self.fixture_kind = FixtureKind::CanonicalMob;
        self.fixture_activation = FixtureActivation::Held;
    }

    /// Seat exactly one validated GameData mob without resetting ActorId allocation.
    pub fn install_canonical_mob_fixture(
        &mut self,
        combat: &mut WorldCombat,
        mob_id: &crate::gamedata::MobId,
        player_x: f64,
        player_z: f64,
        facing_x: f64,
        facing_z: f64,
    ) -> crate::combat::ActorId {
        if self.fixture_activation != FixtureActivation::Held
            || self.fixture_kind != FixtureKind::CanonicalMob
        {
            panic!("canonical mob fixture must be requested before seating");
        }
        let length = facing_x.hypot(facing_z);
        if !length.is_finite() || length <= 1e-9 {
            panic!("canonical mob fixture requires a finite non-zero facing");
        }
        let fx = facing_x / length;
        let fz = facing_z / length;
        let x = player_x + fx * 1.5;
        let z = player_z + fz * 1.5;
        combat.clear_hostiles();
        combat.reset_encounter_state();
        let actor_id =
            combat.add_canonical_mob(mob_id, combat.next_actor_runtime_index(), x, z, x, z);
        self.fixture = true;
        self.first_auto = None;
        self.attack_pip_s = 0.0;
        self.swing_whoosh = false;
        self.hit_flash = false;
        self.pending_sfx.clear();
        self.pending_flinch = None;
        self.pending_fire_bolts.clear();
        self.pending_melee = None;
        self.skull_tele.clear();
        self.mage_tele.clear();
        self.incoming_hit = false;
        self.hurt_flash_s = 0.0;
        self.hp_ghost_frac = 0.0;
        self.hp_chunk_s = 0.0;
        actor_id
    }
    pub fn request_orc_fixture(&mut self) {
        self.fixture_kind = FixtureKind::Orc;
        self.fixture_activation = FixtureActivation::Active;
    }

    pub fn request_wolf_fixture(&mut self) {
        self.fixture_kind = FixtureKind::WolfLine;
        self.fixture_activation = FixtureActivation::Held;
        self.held_mobs.clear();
    }

    pub fn request_held_mobs(&mut self, mobs: Vec<HeldMobFixture>) {
        if mobs.is_empty() {
            panic!("held mob fixture requires at least one mob");
        }
        self.fixture_kind = FixtureKind::HeldMobs;
        self.fixture_activation = FixtureActivation::Held;
        self.held_mobs = mobs;
    }

    pub fn wants_held_mobs(&self) -> bool {
        self.fixture_kind == FixtureKind::HeldMobs
    }

    pub fn request_bones_fixture(&mut self) {
        self.fixture_kind = FixtureKind::Bones;
        self.fixture_activation = FixtureActivation::Active;
    }

    pub fn request_mage_fixture(&mut self) {
        self.fixture_kind = FixtureKind::Mage;
        self.fixture_activation = FixtureActivation::Active;
    }

    pub fn request_yeti_fixture(&mut self) {
        self.fixture_kind = FixtureKind::Yeti;
        self.fixture_activation = FixtureActivation::Active;
    }

    pub fn request_demon_fixture(&mut self) {
        self.fixture_kind = FixtureKind::Demon;
        self.fixture_activation = FixtureActivation::Active;
    }

    pub fn request_bluedemon_fixture(&mut self) {
        self.fixture_kind = FixtureKind::BlueDemon;
        self.fixture_activation = FixtureActivation::Active;
    }

    pub fn request_tribal_veteran_fixture(&mut self) {
        self.fixture_kind = FixtureKind::TribalVeteran;
        self.fixture_activation = FixtureActivation::Active;
    }

    /// Playtester HOLD rearms stay fixture-only: no overland sites, no roster pins.
    pub fn skip_roster_pins(&mut self) {
        self.skip_roster_pins = true;
    }

    /// Leave fixture-only mode so overland sites can seat again.
    pub fn allow_roster_pins(&mut self) {
        self.skip_roster_pins = false;
        self.fixture_kind = FixtureKind::None;
        self.fixture_activation = FixtureActivation::NormalWorld;
    }

    /// Keep current hostiles. Next world tick will not reseat the wolf line.
    pub fn hold_fixture(&mut self) {
        self.fixture = true;
    }

    /// Stream reset already dropped the bodies. Forget ids so a later
    /// despawn does not poke a recycled entity.
    pub fn forget_meshes(&mut self) {
        self.vfx = VfxSystem::new();
        self.pending_fire_bolts.clear();
        self.mesh_ids.clear();
        self.mesh_anchors.clear();
        self.lock_ring = None;
        self.ring_on = None;
        self.flash = None;
        self.death_posed.clear();
        self.sparkles.clear();
    }

    pub fn roster_pins_skipped(&self) -> bool {
        self.skip_roster_pins
    }

    pub fn wants_orc(&self) -> bool {
        self.fixture_kind == FixtureKind::Orc
    }

    pub fn wants_bones(&self) -> bool {
        self.fixture_kind == FixtureKind::Bones
    }

    pub fn wants_mage(&self) -> bool {
        self.fixture_kind == FixtureKind::Mage
    }

    pub fn wants_yeti(&self) -> bool {
        self.fixture_kind == FixtureKind::Yeti
    }

    pub fn wants_demon(&self) -> bool {
        self.fixture_kind == FixtureKind::Demon
    }

    pub fn wants_bluedemon(&self) -> bool {
        self.fixture_kind == FixtureKind::BlueDemon
    }

    pub fn wants_tribal_veteran(&self) -> bool {
        self.fixture_kind == FixtureKind::TribalVeteran
    }

    pub fn first_auto(&self) -> Option<i32> {
        self.first_auto
    }

    pub fn attack_pip(&self) -> bool {
        self.attack_pip_s > 0.0
    }

    pub fn swing_whoosh(&self) -> bool {
        self.swing_whoosh
    }

    pub fn hit_flash(&self) -> bool {
        self.hit_flash
    }

    pub fn incoming_hit(&self) -> bool {
        self.incoming_hit
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
        self.incoming_hit = true;
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

    pub fn hit_flash_latched(&self) -> bool {
        self.hit_flash
    }

    pub fn lock_ring_visible(&self, world: &World) -> bool {
        self.lock_ring
            .map(|id| world.entity(id).is_ok())
            .unwrap_or(false)
    }

    pub fn is_death_posed(&self, id: EntityId) -> bool {
        self.death_posed.contains(&id)
    }

    pub fn has_sparkle(&self, idx: i32) -> bool {
        self.sparkles.contains_key(&idx)
    }

    pub fn sparkle_visible(&self, world: &World) -> bool {
        self.sparkles.values().any(|&id| world.entity(id).is_ok())
    }

    pub fn spawn_sparkle(
        &mut self,
        world: &mut World,
        idx: i32,
        entity: EntityId,
        fallback_x: f64,
        fallback_y: f64,
        fallback_z: f64,
    ) -> EngineResult<()> {
        if let Some(id) = self.sparkles.get(&idx).copied() {
            if world.entity(id).is_ok() {
                return Ok(());
            }
            self.sparkles.remove(&idx);
        }
        // Same XZ as lock-ring / hit-flash: Death-posed mesh origin, not combat h.x/h.z
        // (orc mesh sits ORC_MESH_BEHIND_M behind the hostile point).
        let (x, y, z) = self
            .mesh_anchors
            .iter()
            .find(|a| a.id == entity)
            .map(|a| (a.pos.x, a.pos.y, a.pos.z))
            .unwrap_or((fallback_x, fallback_y, fallback_z));
        let id = world.spawn_anchored(
            sparkle_mesh()?,
            GlobalPlace::at(GlobalPosition::at(x, y + SPARKLE_LIFT_M, z)),
        )?;
        let _ = world.set_casts_shadow(id, false);
        self.sparkles.insert(idx, id);
        Ok(())
    }

    pub fn strip_sparkle(&mut self, world: &mut World, idx: i32) {
        if let Some(id) = self.sparkles.remove(&idx) {
            world.despawn(id);
        }
    }

    pub fn strip_all_sparkles(&mut self, world: &mut World) {
        for id in self.sparkles.drain().map(|(_, id)| id) {
            world.despawn(id);
        }
    }

    pub fn take_combat_sfx(&mut self) -> Vec<CombatSfx> {
        std::mem::take(&mut self.pending_sfx)
    }

    pub fn log_potion(&self, combat: &mut WorldCombat) {
        let heal = combat.last_potion_heal();
        combat
            .log_mut()
            .push(format!("You drink a potion for {heal}"));
    }

    pub fn log_ward(&self, combat: &mut WorldCombat) {
        combat.log_mut().push("You Ward");
    }

    pub fn rearm(&mut self) {
        self.fixture = false;
        self.first_auto = None;
        self.attack_pip_s = 0.0;
        self.swing_whoosh = false;
        self.hit_flash = false;
        self.pending_sfx.clear();
        self.pending_flinch = None;
        self.pending_fire_bolts.clear();
        self.flinch = None;
        self.incoming_hit = false;
        self.hurt_flash_s = 0.0;
        self.hp_ghost_frac = 0.0;
        self.hp_chunk_s = 0.0;
        self.pending_melee = None;
        self.skull_tele.clear();
        self.mage_tele.clear();
        self.flash = None;
        self.flash_t = 0.0;
        self.ring_on = None;
        self.death_posed.clear();
        self.sparkles.clear();
    }

    pub fn reset_vfx(&mut self, world: &mut World) -> EngineResult<()> {
        self.pending_fire_bolts.clear();
        self.vfx.reset(world)
    }

    pub fn despawn_meshes(&mut self, world: &mut World) {
        self.vfx
            .reset(world)
            .unwrap_or_else(|err| panic!("combat VFX reset failed: {err}"));
        self.pending_fire_bolts.clear();
        for id in self.mesh_ids.drain(..) {
            world.despawn(id);
        }
        self.mesh_anchors.clear();
        self.despawn_ring(world);
        self.despawn_flash(world);
        self.strip_all_sparkles(world);
        self.flinch = None;
        self.death_posed.clear();
    }

    fn despawn_ring(&mut self, world: &mut World) {
        if let Some(id) = self.lock_ring.take() {
            world.despawn(id);
        }
        self.ring_on = None;
    }

    fn despawn_flash(&mut self, world: &mut World) {
        if let Some(id) = self.flash.take() {
            world.despawn(id);
        }
        self.flash_t = 0.0;
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
    /// inside the volume during the melee step. Not FaunaLayer.
    pub fn spawn_hostile_meshes(
        &mut self,
        world: &mut World,
        combat: &mut WorldCombat,
        feet_y: &[f64],
        player_yaw_deg: f32,
    ) -> EngineResult<()> {
        self.despawn_meshes(world);
        let yaw = player_yaw_deg + 180.0;
        // wolf.gltf is ~5.5 m long. Origin at the combat point puts the
        // camera inside the snout. Keep hit XZ; sit the mesh behind it.
        let away = player_yaw_deg.to_radians();
        let ax = away.sin() as f64;
        let az = away.cos() as f64;
        const WOLF_MESH_BEHIND_M: f64 = 2.55;
        const ORC_MESH_BEHIND_M: f64 = 1.6;
        for (i, h) in combat.hostiles_mut().iter_mut().enumerate() {
            if h.entity.is_some() {
                continue;
            }
            let result = (|| -> EngineResult<()> {
                let spec = mesh_spec(&h.mob_id).ok_or_else(|| {
                    EngineError::Model(format!("no combat mesh for '{}'", h.mob_id))
                })?;
                let model = self.model_for(&h.mob_id)?;
                let y = feet_y.get(i).copied().unwrap_or(0.0);
                let behind = if is_wolf_mesh(&h.mob_id) {
                    WOLF_MESH_BEHIND_M
                } else {
                    ORC_MESH_BEHIND_M
                };
                let pos = GlobalPosition::at(h.x + ax * behind, y, h.z + az * behind);
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
                if is_wolf_mesh(&h.mob_id) {
                    world.set_animation_speed(id, 0.65)?;
                }
                h.entity = Some(id);
                self.mesh_ids.push(id);
                self.mesh_anchors.push(MeshAnchor { id, pos, yaw });
                Ok(())
            })();
            result?;
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

    /// L1 wolves on a line in front of the player. First is in melee reach.
    pub fn install_l1_wolf_line(
        &mut self,
        combat: &mut WorldCombat,
        player_x: f64,
        player_z: f64,
        facing_x: f64,
        facing_z: f64,
    ) {
        let fl = (facing_x * facing_x + facing_z * facing_z).sqrt();
        let (fx, fz) = if fl > 1e-9 {
            (facing_x / fl, facing_z / fl)
        } else {
            (1.0, 0.0)
        };
        let sheet = combat
            .game_data()
            .map(|_| combat.mob_sheet("wolf"))
            .unwrap_or_else(wolf_sheet);
        combat.clear_hostiles();
        combat.reset_encounter_state();
        // First at 1.5 m so wolf reach (1.8 m) connects. The other two sit
        // beside it (strafe, metres not centimetres) so Tab can pick one mesh.
        let (sx, sz) = (-fz, fx);
        for i in 0..3 {
            let dist = 1.5;
            let strafe = match i {
                1 => -STRAFE_M,
                2 => STRAFE_M,
                _ => 0.0,
            };
            combat.add_hostile(WorldHostile::from_sheet(
                i,
                player_x + fx * dist + sx * strafe,
                player_z + fz * dist + sz * strafe,
                &sheet,
                "wolf",
                player_x + fx * 1.5,
                player_z + fz * 1.5,
            ));
        }
        *combat = keep_player(combat);
        combat.reset_encounter_state();
        self.fixture = true;
        self.first_auto = None;
        self.attack_pip_s = 0.0;
        self.swing_whoosh = false;
        self.hit_flash = false;
        self.pending_sfx.clear();
        self.pending_flinch = None;
        self.flinch = None;
        self.flash = None;
        self.flash_t = 0.0;
        self.ring_on = None;
    }

    pub fn install_held_mobs(
        &mut self,
        combat: &mut WorldCombat,
        player_x: f64,
        player_z: f64,
        facing_x: f64,
        facing_z: f64,
    ) {
        let length = facing_x.hypot(facing_z);
        if length <= 1e-9 {
            panic!("held mob fixture requires a non-zero player facing");
        }
        let (fx, fz) = (facing_x / length, facing_z / length);
        let (rx, rz) = (-fz, fx);
        combat.clear_hostiles();
        combat.reset_encounter_state();
        for (idx, fixture) in self.held_mobs.iter().enumerate() {
            let x = player_x + fx * fixture.forward_m + rx * fixture.right_m;
            let z = player_z + fz * fixture.forward_m + rz * fixture.right_m;
            let sheet = combat.mob_sheet(fixture.mob_id.as_str());
            combat.add_hostile(WorldHostile::from_sheet(
                i32::try_from(idx).expect("held fixture mob index"),
                x,
                z,
                &sheet,
                fixture.mob_id.as_str(),
                x,
                z,
            ));
        }
        self.fixture = true;
        self.first_auto = None;
        self.incoming_hit = false;
    }

    /// One published orc 1.5 m in front of the player. First Punch after swing_s.
    pub fn install_orc_fixture(
        &mut self,
        combat: &mut WorldCombat,
        player_x: f64,
        player_z: f64,
        facing_x: f64,
        facing_z: f64,
    ) {
        let fl = (facing_x * facing_x + facing_z * facing_z).sqrt();
        let (fx, fz) = if fl > 1e-9 {
            (facing_x / fl, facing_z / fl)
        } else {
            (1.0, 0.0)
        };
        let sheet = orc_sheet();
        combat.clear_hostiles();
        combat.reset_encounter_state();
        combat.add_hostile(WorldHostile::from_sheet(
            0,
            player_x + fx * 1.5,
            player_z + fz * 1.5,
            &sheet,
            "orc",
            player_x + fx * 1.5,
            player_z + fz * 1.5,
        ));
        *combat = keep_player(combat);
        combat.reset_encounter_state();
        self.fixture_kind = FixtureKind::Orc;
        self.fixture = true;
        self.first_auto = None;
        self.attack_pip_s = 0.0;
        self.swing_whoosh = false;
        self.hit_flash = false;
        self.pending_sfx.clear();
        self.pending_flinch = None;
        self.flinch = None;
        self.flash = None;
        self.flash_t = 0.0;
        self.ring_on = None;
        self.pending_melee = None;
        self.skull_tele.clear();
        self.incoming_hit = false;
        self.hurt_flash_s = 0.0;
        self.hp_ghost_frac = 0.0;
        self.hp_chunk_s = 0.0;
    }

    /// One published yeti 1.5 m in front of the player. First Punch after swing_s.
    /// Live slam tell is HOLD. Melee clip is Punch.
    pub fn install_yeti_fixture(
        &mut self,
        combat: &mut WorldCombat,
        player_x: f64,
        player_z: f64,
        facing_x: f64,
        facing_z: f64,
    ) {
        let fl = (facing_x * facing_x + facing_z * facing_z).sqrt();
        let (fx, fz) = if fl > 1e-9 {
            (facing_x / fl, facing_z / fl)
        } else {
            (1.0, 0.0)
        };
        let sheet = yeti_sheet();
        combat.clear_hostiles();
        combat.reset_encounter_state();
        combat.add_hostile(WorldHostile::from_sheet(
            0,
            player_x + fx * 1.5,
            player_z + fz * 1.5,
            &sheet,
            "yeti",
            player_x + fx * 1.5,
            player_z + fz * 1.5,
        ));
        *combat = keep_player(combat);
        combat.reset_encounter_state();
        self.fixture_kind = FixtureKind::Yeti;
        self.fixture = true;
        self.first_auto = None;
        self.attack_pip_s = 0.0;
        self.swing_whoosh = false;
        self.hit_flash = false;
        self.pending_sfx.clear();
        self.pending_flinch = None;
        self.flinch = None;
        self.flash = None;
        self.flash_t = 0.0;
        self.ring_on = None;
        self.pending_melee = None;
        self.skull_tele.clear();
        self.incoming_hit = false;
        self.hurt_flash_s = 0.0;
        self.hp_ghost_frac = 0.0;
        self.hp_chunk_s = 0.0;
    }

    /// One published demon 1.5 m in front of the player. First Punch after swing_s.
    /// Live shout tell is HOLD (no clip, no API). Melee clip is Punch.
    pub fn install_demon_fixture(
        &mut self,
        combat: &mut WorldCombat,
        player_x: f64,
        player_z: f64,
        facing_x: f64,
        facing_z: f64,
    ) {
        let fl = (facing_x * facing_x + facing_z * facing_z).sqrt();
        let (fx, fz) = if fl > 1e-9 {
            (facing_x / fl, facing_z / fl)
        } else {
            (1.0, 0.0)
        };
        let sheet = demon_sheet();
        combat.clear_hostiles();
        combat.reset_encounter_state();
        combat.add_hostile(WorldHostile::from_sheet(
            0,
            player_x + fx * 1.5,
            player_z + fz * 1.5,
            &sheet,
            "demon",
            player_x + fx * 1.5,
            player_z + fz * 1.5,
        ));
        *combat = keep_player(combat);
        combat.reset_encounter_state();
        self.fixture_kind = FixtureKind::Demon;
        self.fixture = true;
        self.first_auto = None;
        self.attack_pip_s = 0.0;
        self.swing_whoosh = false;
        self.hit_flash = false;
        self.pending_sfx.clear();
        self.pending_flinch = None;
        self.flinch = None;
        self.flash = None;
        self.flash_t = 0.0;
        self.ring_on = None;
        self.pending_melee = None;
        self.skull_tele.clear();
        self.incoming_hit = false;
        self.hurt_flash_s = 0.0;
        self.hp_ghost_frac = 0.0;
        self.hp_chunk_s = 0.0;
    }

    /// One published blue_demon 1.5 m in front of the player. First Punch after swing_s.
    /// Live bolt/self-Mend/Ward tells are HOLD (no mob caster API). Melee clip is Punch.
    pub fn install_bluedemon_fixture(
        &mut self,
        combat: &mut WorldCombat,
        player_x: f64,
        player_z: f64,
        facing_x: f64,
        facing_z: f64,
    ) {
        let fl = (facing_x * facing_x + facing_z * facing_z).sqrt();
        let (fx, fz) = if fl > 1e-9 {
            (facing_x / fl, facing_z / fl)
        } else {
            (1.0, 0.0)
        };
        let sheet = blue_demon_sheet();
        combat.clear_hostiles();
        combat.reset_encounter_state();
        combat.add_hostile(WorldHostile::from_sheet(
            0,
            player_x + fx * 1.5,
            player_z + fz * 1.5,
            &sheet,
            "blue_demon",
            player_x + fx * 1.5,
            player_z + fz * 1.5,
        ));
        *combat = keep_player(combat);
        combat.reset_encounter_state();
        self.fixture_kind = FixtureKind::BlueDemon;
        self.fixture = true;
        self.first_auto = None;
        self.attack_pip_s = 0.0;
        self.swing_whoosh = false;
        self.hit_flash = false;
        self.pending_sfx.clear();
        self.pending_flinch = None;
        self.flinch = None;
        self.flash = None;
        self.flash_t = 0.0;
        self.ring_on = None;
        self.pending_melee = None;
        self.skull_tele.clear();
        self.incoming_hit = false;
        self.hurt_flash_s = 0.0;
        self.hp_ghost_frac = 0.0;
        self.hp_chunk_s = 0.0;
    }

    /// One published tribal_veteran 1.5 m in front of the player. First Punch after swing_s.
    /// Pin/slow tells are HOLD (no clip). Melee clip is Punch. Empty hands.
    pub fn install_tribal_veteran_fixture(
        &mut self,
        combat: &mut WorldCombat,
        player_x: f64,
        player_z: f64,
        facing_x: f64,
        facing_z: f64,
    ) {
        let fl = (facing_x * facing_x + facing_z * facing_z).sqrt();
        let (fx, fz) = if fl > 1e-9 {
            (facing_x / fl, facing_z / fl)
        } else {
            (1.0, 0.0)
        };
        let sheet = tribal_veteran_sheet();
        combat.clear_hostiles();
        combat.reset_encounter_state();
        combat.add_hostile(WorldHostile::from_sheet(
            0,
            player_x + fx * 1.5,
            player_z + fz * 1.5,
            &sheet,
            "tribal_veteran",
            player_x + fx * 1.5,
            player_z + fz * 1.5,
        ));
        *combat = keep_player(combat);
        combat.reset_encounter_state();
        self.fixture_kind = FixtureKind::TribalVeteran;
        self.fixture = true;
        self.first_auto = None;
        self.attack_pip_s = 0.0;
        self.swing_whoosh = false;
        self.hit_flash = false;
        self.pending_sfx.clear();
        self.pending_flinch = None;
        self.flinch = None;
        self.flash = None;
        self.flash_t = 0.0;
        self.ring_on = None;
        self.pending_melee = None;
        self.skull_tele.clear();
        self.incoming_hit = false;
        self.hurt_flash_s = 0.0;
        self.hp_ghost_frac = 0.0;
        self.hp_chunk_s = 0.0;
    }

    /// One Warrior + two Minions, wolf-line seating (1.5 m + strafe +/-1.8 m).
    /// Does not call install_l1_wolf_line (that still clears).
    pub fn install_bones_fixture(
        &mut self,
        combat: &mut WorldCombat,
        player_x: f64,
        player_z: f64,
        facing_x: f64,
        facing_z: f64,
    ) {
        let fl = (facing_x * facing_x + facing_z * facing_z).sqrt();
        let (fx, fz) = if fl > 1e-9 {
            (facing_x / fl, facing_z / fl)
        } else {
            (1.0, 0.0)
        };
        let (sx, sz) = (-fz, fx);
        let warrior = skeleton_warrior_sheet();
        let minion = skeleton_minion_sheet();
        combat.clear_hostiles();
        combat.reset_encounter_state();
        for i in 0..3 {
            let dist = 1.5;
            let strafe = match i {
                1 => -STRAFE_M,
                2 => STRAFE_M,
                _ => 0.0,
            };
            let sheet = if i == 0 { &warrior } else { &minion };
            let mob_id = if i == 0 {
                "skeleton_warrior"
            } else {
                "skeleton_minion"
            };
            combat.add_hostile(hostile_from_sheet(
                i,
                player_x + fx * dist + sx * strafe,
                player_z + fz * dist + sz * strafe,
                sheet,
                mob_id,
            ));
        }
        *combat = keep_player(combat);
        combat.reset_encounter_state();
        self.fixture_kind = FixtureKind::Bones;
        self.fixture = true;
        self.first_auto = None;
        self.attack_pip_s = 0.0;
        self.swing_whoosh = false;
        self.hit_flash = false;
        self.pending_sfx.clear();
        self.pending_flinch = None;
        self.flinch = None;
        self.flash = None;
        self.flash_t = 0.0;
        self.ring_on = None;
        self.pending_melee = None;
        self.skull_tele.clear();
        self.mage_tele.clear();
        self.incoming_hit = false;
        self.hurt_flash_s = 0.0;
        self.hp_ghost_frac = 0.0;
        self.hp_chunk_s = 0.0;
    }

    /// One Mage_Staff 1.5 m in front of the player.
    pub fn install_mage_fixture(
        &mut self,
        combat: &mut WorldCombat,
        player_x: f64,
        player_z: f64,
        facing_x: f64,
        facing_z: f64,
    ) {
        let fl = (facing_x * facing_x + facing_z * facing_z).sqrt();
        let (fx, fz) = if fl > 1e-9 {
            (facing_x / fl, facing_z / fl)
        } else {
            (1.0, 0.0)
        };
        let sheet = skeleton_mage_sheet();
        combat.clear_hostiles();
        combat.reset_encounter_state();
        combat.add_hostile(hostile_from_sheet(
            0,
            player_x + fx * 1.5,
            player_z + fz * 1.5,
            &sheet,
            "skeleton_mage",
        ));
        *combat = keep_player(combat);
        combat.reset_encounter_state();
        self.fixture_kind = FixtureKind::Mage;
        self.fixture = true;
        self.first_auto = None;
        self.attack_pip_s = 0.0;
        self.swing_whoosh = false;
        self.hit_flash = false;
        self.pending_sfx.clear();
        self.pending_flinch = None;
        self.flinch = None;
        self.flash = None;
        self.flash_t = 0.0;
        self.ring_on = None;
        self.pending_melee = None;
        self.skull_tele.clear();
        self.mage_tele.clear();
        self.incoming_hit = false;
        self.hurt_flash_s = 0.0;
        self.hp_ghost_frac = 0.0;
        self.hp_chunk_s = 0.0;
    }

    /// Soft lock + auto. Does not touch camera, Esc, or E.
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
    ) -> Option<i32> {
        if dt <= 0.0 {
            return None;
        }
        if self.fixture_activation == FixtureActivation::Held {
            return None;
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
        let mut just = None;
        let fixed_steps = combat.consume_fixed_steps(dt);
        for _ in 0..fixed_steps {
            let pending_cast = combat.cast_kind();
            let lock_id = combat.lock_id();
            let lock_hp = lock_id.and_then(|id| {
                combat
                    .hostiles()
                    .iter()
                    .find(|h| h.idx == id)
                    .map(|h| (h.name.clone(), h.hp(), h.root_s, h.slow_s))
            });
            let player_hp = combat.player().resources.hp();
            let ward = combat.ward_value();
            let step = combat.step_fixed(player_x, player_z, facing_x, facing_z);
            log_finished_cast(combat, pending_cast, lock_id, lock_hp, player_hp, ward);
            if let Some((dealt, target_id, strike)) = step.outgoing {
                if self.first_auto.is_none() {
                    self.first_auto = Some(dealt);
                }
                self.attack_pip_s = ATTACK_PIP_S;
                self.swing_whoosh = true;
                self.hit_flash = true;
                self.pending_sfx.push(CombatSfx::Swing);
                self.pending_sfx.push(CombatSfx::Hit);
                self.pending_flinch = Some(target_id);
                just = Some(dealt);
                let name = combat
                    .hostiles()
                    .iter()
                    .find(|h| h.idx == target_id)
                    .map(|h| h.name.clone())
                    .unwrap_or_else(|| "Wolf".into());
                combat.log_mut().push(format!(
                    "You {} {name} for {dealt}",
                    if strike { "Strike" } else { "hit" }
                ));
            }
            if let Some((prev_hp, hit)) = step.incoming {
                self.latch_incoming_chunk(prev_hp, combat.player().resources.hp_max());
                self.pending_sfx.push(CombatSfx::Hurt);
                combat
                    .log_mut()
                    .push(format!("{} hits you for {}", hit.by, hit.dealt));
                if hit.killed {
                    combat.log_mut().push("You are slain");
                }
                if let Some(h) = combat
                    .hostiles()
                    .iter()
                    .find(|h| h.name == hit.by || h.mob_id == hit.by)
                {
                    queue_connecting_melee(&mut self.pending_melee, &self.models, h);
                }
            }
            self.present_resolutions(combat, step.resolutions);
            self.present_special_events(combat, step.specials);
        }
        just
    }

    fn present_resolutions(&mut self, combat: &mut WorldCombat, resolutions: Vec<Resolution>) {
        for mut resolution in resolutions {
            let caster_is_player = resolution.caster == 0;
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
                    match effect.kind {
                        EffectKind::Damage if caster_is_player => {
                            let hostile_index = target
                                .checked_sub(1)
                                .expect("player damage target must be a hostile actor");
                            let hostile_name = combat
                                .hostiles()
                                .get(hostile_index)
                                .unwrap_or_else(|| {
                                    panic!("resolution references missing hostile actor {target}")
                                })
                                .name
                                .clone();
                            let dealt = applied.round() as i32;
                            if self.first_auto.is_none() {
                                self.first_auto = Some(dealt);
                            }
                            self.attack_pip_s = ATTACK_PIP_S;
                            self.swing_whoosh = true;
                            self.hit_flash = true;
                            self.pending_sfx.push(CombatSfx::Swing);
                            self.pending_sfx.push(CombatSfx::Hit);
                            let target_id = combat.hostiles()[hostile_index].idx;
                            self.pending_flinch = Some(target_id);
                            if resolution.action_id.as_str() == "fire_bolt" {
                                self.pending_fire_bolts.push(target_id);
                            }
                            combat.log_mut().push(format!(
                                "You {} {} for {dealt}",
                                resolution.action_id, hostile_name
                            ));
                        }
                        EffectKind::Damage => {
                            let attacker = combat
                                .hostiles()
                                .get(resolution.caster - 1)
                                .map(|hostile| hostile.name.clone())
                                .unwrap_or_else(|| "Hostile".into());
                            self.latch_incoming_chunk(
                                combat.player().resources.hp() + applied,
                                combat.player().resources.hp_max(),
                            );
                            self.pending_sfx.push(CombatSfx::Hurt);
                            combat.log_mut().push(format!(
                                "{attacker} hits you for {}",
                                applied.round() as i32
                            ));
                            if combat.is_dead() {
                                combat.log_mut().push("You are slain");
                            }
                            if let Some(hostile) = combat.hostiles().get(resolution.caster - 1) {
                                queue_connecting_melee(
                                    &mut self.pending_melee,
                                    &self.models,
                                    hostile,
                                );
                            }
                        }
                        EffectKind::Heal if caster_is_player => {
                            combat.log_mut().push(format!(
                                "You {} for {}",
                                resolution.action_id,
                                applied.round() as i32
                            ));
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    fn present_special_events(
        &mut self,
        combat: &mut WorldCombat,
        events: Vec<SpecialAttackEvent>,
    ) {
        for event in events {
            let Some(h) = combat
                .hostiles()
                .iter()
                .find(|h| h.idx == event.attacker_idx)
            else {
                panic!(
                    "special attack event references missing hostile {}",
                    event.attacker_idx
                );
            };
            let tele = match event.cue {
                SpecialAttackCue::Weapon => &mut self.skull_tele,
                SpecialAttackCue::SpellcastShoot => &mut self.mage_tele,
            };
            if event.hit.is_none() {
                let duration = match event.cue {
                    SpecialAttackCue::Weapon => SKULL_TELE_S,
                    SpecialAttackCue::SpellcastShoot => MAGE_TELE_S,
                };
                tele.insert(event.attacker_idx, duration);
                let clip = match event.cue {
                    SpecialAttackCue::Weapon => "Weapon",
                    SpecialAttackCue::SpellcastShoot => "Spellcast_Shoot",
                };
                self.pending_melee = Some((h.entity, clip));
                continue;
            }
            tele.remove(&event.attacker_idx);
            let hit = event.hit.expect("special hit present after is_none check");
            self.latch_incoming_chunk(event.previous_player_hp, combat.player().resources.hp_max());
            self.pending_sfx.push(CombatSfx::Hurt);
            combat
                .log_mut()
                .push(format!("{} hits you for {}", hit.by, hit.dealt));
            if hit.killed {
                combat.log_mut().push("You are slain");
            }
        }
    }

    /// Play catalog anim_melee on the locked mesh only.
    pub fn replay_melee(&mut self, world: &mut World, combat: &WorldCombat) {
        let Some(lock) = combat.lock_id() else {
            panic!("{}", EngineError::Model("replay_melee: no lock".into()));
        };
        let Some(h) = combat.hostiles().iter().find(|h| h.idx == lock) else {
            panic!(
                "{}",
                EngineError::Model("replay_melee: lock not in hostiles".into())
            );
        };
        let _spec = mesh_spec(&h.mob_id).unwrap_or_else(|| {
            panic!(
                "{}",
                EngineError::Model(format!("replay_melee: no mesh spec for '{}'", h.mob_id))
            )
        });
        let id = h
            .entity
            .or_else(|| self.mesh_ids.get(h.idx as usize).copied())
            .unwrap_or_else(|| {
                panic!(
                    "{}",
                    EngineError::Model("replay_melee: locked mesh has no entity".into())
                )
            });
        if let Err(err) = world.play_animation_action(id, AnimationAction::Attack) {
            panic!(
                "{}",
                EngineError::Model(format!("melee action failed for '{}': {err}", h.mob_id))
            );
        }
        if let Err(err) = world.set_animation_speed(id, 1.0) {
            panic!(
                "{}",
                EngineError::Model(format!("melee clip speed failed: {err}"))
            );
        }
    }

    /// Play catalog anim_weapon (Spellcast_Shoot) on the locked mesh. Fail-loud.
    pub fn replay_weapon(&mut self, world: &mut World, combat: &WorldCombat) {
        let Some(lock) = combat.lock_id() else {
            panic!("{}", EngineError::Model("replay_weapon: no lock".into()));
        };
        let Some(h) = combat.hostiles().iter().find(|h| h.idx == lock) else {
            panic!(
                "{}",
                EngineError::Model("replay_weapon: lock not in hostiles".into())
            );
        };
        let spec = mesh_spec(&h.mob_id).unwrap_or_else(|| {
            panic!(
                "{}",
                EngineError::Model(format!("replay_weapon: no mesh spec for '{}'", h.mob_id))
            )
        });
        let _clip = spec.anim_weapon.unwrap_or_else(|| {
            panic!(
                "{}",
                EngineError::Model(format!("replay_weapon: '{}' has no anim_weapon", h.mob_id))
            )
        });
        let id = h
            .entity
            .or_else(|| self.mesh_ids.get(h.idx as usize).copied())
            .unwrap_or_else(|| {
                panic!(
                    "{}",
                    EngineError::Model("replay_weapon: locked mesh has no entity".into())
                )
            });
        if let Err(err) = world.play_animation_action(id, AnimationAction::Cast) {
            panic!(
                "{}",
                EngineError::Model(format!("weapon action failed for '{}': {err}", h.mob_id))
            );
        }
        if let Err(err) = world.set_animation_speed(id, 1.0) {
            panic!(
                "{}",
                EngineError::Model(format!("weapon clip speed failed: {err}"))
            );
        }
    }

    /// Session hook: ring + flinch after the combat clock. `player_y` is the
    /// fallback ground when the caller has no column yet.
    pub fn sync_tells(
        &mut self,
        world: &mut World,
        combat: &WorldCombat,
        player_y: f64,
        dt: f64,
        _just: Option<i32>,
    ) -> EngineResult<()> {
        self.present(
            world,
            combat,
            GlobalPosition::at(0.0, player_y, 0.0),
            |_, _| player_y,
            dt as f32,
        )
    }

    /// Lock ring + hit flinch. Call after [`Self::tick`] with the live world.
    pub fn present(
        &mut self,
        world: &mut World,
        combat: &WorldCombat,
        player_position: GlobalPosition,
        mut ground_y: impl FnMut(f64, f64) -> f64,
        dt: f32,
    ) -> EngineResult<()> {
        if let Some((Some(id), clip)) = self.pending_melee.take() {
            world
                .play_animation(id, clip)
                .map_err(|err| EngineError::Model(format!("melee clip '{clip}' failed: {err}")))?;
        }
        self.play_death_poses(world, combat)?;
        self.sync_lock_ring(world, combat, &mut ground_y)?;
        let fire_targets = std::mem::take(&mut self.pending_fire_bolts);
        for target_id in &fire_targets {
            self.spawn_fire_bolt(world, combat, player_position, *target_id, &mut ground_y)?;
        }
        if let Some(target_id) = self.pending_flinch.take() {
            self.start_flinch(world, combat, target_id)?;
            if !fire_targets.contains(&target_id) {
                self.spawn_flash(world, combat, target_id, &mut ground_y)?;
            }
        }
        self.tick_flinch(world, dt)?;
        self.tick_flash(world, dt);
        self.vfx.update(world, dt)?;
        Ok(())
    }

    /// Synchronize render-space hostile transforms after deterministic combat AI moves them.
    pub fn sync_hostile_transforms(
        &mut self,
        world: &mut World,
        combat: &WorldCombat,
    ) -> EngineResult<()> {
        for h in combat.hostiles() {
            let Some(id) = h.entity else {
                continue;
            };
            let Some(anchor) = self.mesh_anchors.iter().find(|a| a.id == id) else {
                continue;
            };
            let render = world.to_render(GlobalPosition::at(h.x, anchor.pos.y, h.z))?;
            let heading = h.heading();
            let yaw = heading.x().atan2(heading.z()).to_degrees() as f32;
            let place = Place::at(render.x, render.y, render.z)?.yaw_deg(yaw)?;
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
            if h.is_alive() {
                continue;
            }
            let Some(id) = h
                .entity
                .or_else(|| self.mesh_ids.get(h.idx as usize).copied())
            else {
                continue;
            };
            if !self.mesh_anchors.iter().any(|anchor| anchor.id == id) {
                continue;
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
        ground_y: &mut impl FnMut(f64, f64) -> f64,
    ) -> EngineResult<()> {
        let want = combat.lock_id().filter(|_| self.fixture);
        let still = want == self.ring_on
            && self
                .lock_ring
                .map(|id| world.entity(id).is_ok())
                .unwrap_or(false);
        if still {
            return Ok(());
        }
        self.despawn_ring(world);
        let Some(lock) = want else {
            return Ok(());
        };
        let Some(h) = combat
            .hostiles()
            .iter()
            .find(|h| h.idx == lock && h.is_alive())
        else {
            return Ok(());
        };
        let (x, z) = self
            .mesh_anchors
            .iter()
            .find(|a| Some(a.id) == h.entity)
            .map(|a| (a.pos.x, a.pos.z))
            .unwrap_or((h.x, h.z));
        let y = ground_y(x, z) + RING_LIFT_M;
        let mesh = lock_ring_mesh()?;
        let place = GlobalPlace::at(GlobalPosition::at(x, y, z));
        let id = world.spawn_anchored(mesh, place)?;
        let _ = world.set_casts_shadow(id, false);
        self.lock_ring = Some(id);
        self.ring_on = Some(lock);
        Ok(())
    }

    fn spawn_fire_bolt(
        &mut self,
        world: &mut World,
        combat: &WorldCombat,
        player_position: GlobalPosition,
        target_id: i32,
        ground_y: &mut impl FnMut(f64, f64) -> f64,
    ) -> EngineResult<()> {
        let hostile = combat
            .hostiles()
            .iter()
            .find(|hostile| hostile.idx == target_id)
            .unwrap_or_else(|| panic!("fire bolt references missing hostile {target_id}"));
        let target_y = ground_y(hostile.x, hostile.z);
        let target = world.to_render(GlobalPosition::at(hostile.x, target_y + 0.85, hostile.z))?;
        let origin = world.to_render(GlobalPosition::at(
            player_position.x,
            player_position.y + 1.35,
            player_position.z,
        ))?;
        let distance = origin.distance(target).max(0.1);
        self.vfx.spawn(
            world,
            EffectSpec {
                kind: VisualKind::Fire,
                delivery: Delivery::SingleTarget,
                origin,
                target,
                range_m: distance,
                radius_m: 1.0,
                angle_deg: 45.0,
                duration_s: 1.15,
                scale: 0.75,
                intensity: 1.0,
                seed: self.vfx_seed,
            },
        )?;
        self.vfx_seed = self.vfx_seed.wrapping_add(1);
        Ok(())
    }

    fn spawn_flash(
        &mut self,
        world: &mut World,
        combat: &WorldCombat,
        target_id: i32,
        ground_y: &mut impl FnMut(f64, f64) -> f64,
    ) -> EngineResult<()> {
        self.despawn_flash(world);
        let Some(h) = combat.hostiles().iter().find(|h| h.idx == target_id) else {
            return Ok(());
        };
        let (x, z) = self
            .mesh_anchors
            .iter()
            .find(|a| Some(a.id) == h.entity)
            .map(|a| (a.pos.x, a.pos.z))
            .unwrap_or((h.x, h.z));
        let y = ground_y(x, z) + 0.22;
        let id = world.spawn_anchored(
            hit_flash_mesh()?,
            GlobalPlace::at(GlobalPosition::at(x, y, z)).with_scale(FLINCH_PEAK),
        )?;
        let _ = world.set_casts_shadow(id, false);
        self.flash = Some(id);
        self.flash_t = 0.0;
        Ok(())
    }

    fn tick_flash(&mut self, world: &mut World, dt: f32) {
        let Some(id) = self.flash else {
            return;
        };
        self.flash_t += dt;
        if self.flash_t >= FLASH_S {
            world.despawn(id);
            self.flash = None;
            self.flash_t = 0.0;
        }
    }

    fn start_flinch(
        &mut self,
        world: &mut World,
        combat: &WorldCombat,
        target_id: i32,
    ) -> EngineResult<()> {
        let Some(h) = combat.hostiles().iter().find(|h| h.idx == target_id) else {
            return Ok(());
        };
        let Some(entity) = h.entity else {
            return Ok(());
        };
        let Some(anchor) = self.mesh_anchors.iter().find(|a| a.id == entity) else {
            return Ok(());
        };
        apply_mesh_scale(world, anchor, FLINCH_PEAK)?;
        self.flinch = Some(Flinch {
            id: anchor.id,
            pos: anchor.pos,
            yaw: anchor.yaw,
            t: 0.0,
        });
        Ok(())
    }

    fn tick_flinch(&mut self, world: &mut World, dt: f32) -> EngineResult<()> {
        let Some(mut flinch) = self.flinch.take() else {
            return Ok(());
        };
        flinch.t += dt;
        if flinch.t >= FLINCH_S {
            let anchor = MeshAnchor {
                id: flinch.id,
                pos: flinch.pos,
                yaw: flinch.yaw,
            };
            apply_mesh_scale(world, &anchor, 1.0)?;
            return Ok(());
        }
        let fade = (1.0 - flinch.t / FLINCH_S).clamp(0.0, 1.0);
        let scale = 1.0 + (FLINCH_PEAK - 1.0) * fade;
        let anchor = MeshAnchor {
            id: flinch.id,
            pos: flinch.pos,
            yaw: flinch.yaw,
        };
        apply_mesh_scale(world, &anchor, scale)?;
        self.flinch = Some(flinch);
        Ok(())
    }
}

fn hit_flash_mesh() -> EngineResult<Mesh> {
    let mut mesh = Mesh::new();
    let s = 3.40;
    let a = mesh.add_point((-s, 0.0, -s))?;
    let b = mesh.add_point((s, 0.0, -s))?;
    let c = mesh.add_point((s, 0.0, s))?;
    let d = mesh.add_point((-s, 0.0, s))?;
    mesh.add_quad(a, b, c, d)?;
    mesh.add_quad(a, d, c, b)?;
    mesh.paint_all(FLASH);
    Ok(mesh)
}

fn sparkle_mesh() -> EngineResult<Mesh> {
    // Clone of the hit-flash unlit quad, lock-ring-tube scale. Stays until taken or reset.
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

fn apply_mesh_scale(world: &mut World, anchor: &MeshAnchor, scale: f32) -> EngineResult<()> {
    let render = world.to_render(anchor.pos)?;
    let place = Place::at(render.x, render.y, render.z)?
        .yaw_deg(anchor.yaw)?
        .scale(scale)?;
    // Wolf may have been despawned on rearm; a missing body is not a hard fail.
    let _ = world.set_place(anchor.id, place);
    Ok(())
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

fn log_finished_cast(
    combat: &mut WorldCombat,
    kind: Option<&str>,
    lock_id: Option<i32>,
    before: Option<(String, f64, f64, f64)>,
    player_hp: f64,
    ward: f64,
) {
    let Some(kind) = kind else {
        return;
    };
    let name = before
        .as_ref()
        .map(|(n, _, _, _)| n.clone())
        .or_else(|| {
            lock_id.and_then(|id| {
                combat
                    .hostiles()
                    .iter()
                    .find(|h| h.idx == id)
                    .map(|h| h.name.clone())
            })
        })
        .unwrap_or_else(|| "Wolf".into());
    match kind {
        "aimed" | "pin" | "ember" => {
            if let Some((_, hp0, _, _)) = before {
                if let Some(h) =
                    lock_id.and_then(|id| combat.hostiles().iter().find(|h| h.idx == id))
                {
                    let dealt = (hp0 - h.hp()).round() as i32;
                    if dealt > 0 {
                        let verb = match kind {
                            "aimed" => "Aimed Shot",
                            "pin" => "Pin",
                            "ember" => "Ember",
                            _ => kind,
                        };
                        combat
                            .log_mut()
                            .push(format!("You {verb} {name} for {dealt}"));
                    }
                }
            }
        }
        "bind" => {
            if let Some((_, _, root0, _)) = before {
                if let Some(h) =
                    lock_id.and_then(|id| combat.hostiles().iter().find(|h| h.idx == id))
                {
                    if h.root_s > root0 {
                        combat.log_mut().push(format!("You Bind {name}"));
                    }
                }
            }
        }
        "mend" => {
            let heal = (combat.player().resources.hp() - player_hp).round() as i32;
            if heal > 0 {
                combat.log_mut().push(format!("You Mend for {heal}"));
            }
        }
        "ward" if combat.ward_value() > ward => {
            combat.log_mut().push("You Ward");
        }
        _ => {}
    }
}

fn keep_player(combat: &WorldCombat) -> WorldCombat {
    let mut out = combat.clone();
    out.reset_for_encounter();
    out
}

fn is_wolf_mesh(mob_id: &str) -> bool {
    matches!(mob_id, "wolf")
}

pub fn is_bone_id(mob_id: &str) -> bool {
    matches!(
        mob_id,
        "skeleton_warrior" | "skeleton_minion" | "skeleton_mage"
    )
}

fn queue_clip(
    pending: &mut Option<(Option<EntityId>, &'static str)>,
    models: &HashMap<String, Arc<AnimatedModel>>,
    h: &WorldHostile,
    clip: &'static str,
) {
    let Some(spec) = mesh_spec(&h.mob_id) else {
        return;
    };
    let has_clip = models
        .get(spec.id)
        .map(|m| m.find_clip(clip).is_some())
        .unwrap_or(true);
    if has_clip {
        *pending = Some((h.entity, clip));
    }
}

fn queue_connecting_melee(
    pending: &mut Option<(Option<EntityId>, &'static str)>,
    models: &HashMap<String, Arc<AnimatedModel>>,
    h: &WorldHostile,
) {
    let Some(spec) = mesh_spec(&h.mob_id) else {
        return;
    };
    if spec.anim_melee == spec.anim_idle {
        return;
    }
    queue_clip(pending, models, h, spec.anim_melee);
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
    Err(EngineError::Model(format!(
        "no assets under {}",
        tried
            .first()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    )))
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

fn hostile_from_sheet(idx: i32, x: f64, z: f64, sheet: &MobSheet, mob_id: &str) -> WorldHostile {
    WorldHostile::from_sheet(idx, x, z, sheet, mob_id, x, z)
}

pub fn seat_dungeon_skulls(combat: &mut WorldCombat, spots: &[GlobalXZ]) {
    let sheet = orc_skull_sheet();
    let next = combat.hostiles().iter().map(|h| h.idx).max().unwrap_or(-1) + 1;
    for (idx, p) in (next..).zip(spots.iter()) {
        combat.add_hostile(hostile_from_sheet(idx, p.x, p.z, &sheet, "orc_skull"));
    }
}

pub fn clear_dungeon_skulls(combat: &mut WorldCombat) {
    combat.retain_hostiles(|h| h.mob_id != "orc_skull");
    if let Some(lock) = combat.lock_id() {
        if !combat.hostiles().iter().any(|h| h.idx == lock) {
            combat.set_lock(None);
        }
    }
}

pub fn seat_dungeon_bones(combat: &mut WorldCombat, spots: &[GlobalXZ], heart: Option<GlobalXZ>) {
    let mut idx = combat.hostiles().iter().map(|h| h.idx).max().unwrap_or(-1) + 1;
    let warrior = skeleton_warrior_sheet();
    let minion = skeleton_minion_sheet();
    let mage = skeleton_mage_sheet();
    let (fx, fz) = (1.0, 0.0);
    let (sx, sz) = (-fz, fx);
    for p in spots {
        combat.add_hostile(hostile_from_sheet(
            idx,
            p.x,
            p.z,
            &warrior,
            "skeleton_warrior",
        ));
        idx += 1;
        combat.add_hostile(hostile_from_sheet(
            idx,
            p.x + sx * -STRAFE_M,
            p.z + sz * -STRAFE_M,
            &minion,
            "skeleton_minion",
        ));
        idx += 1;
        combat.add_hostile(hostile_from_sheet(
            idx,
            p.x + sx * STRAFE_M,
            p.z + sz * STRAFE_M,
            &minion,
            "skeleton_minion",
        ));
        idx += 1;
    }
    let mage_at = spots.iter().find(|p| {
        heart
            .map(|h| (p.x - h.x).hypot(p.z - h.z) > 0.05)
            .unwrap_or(spots.len() > 1)
    });
    if let Some(p) = mage_at {
        combat.add_hostile(hostile_from_sheet(
            idx,
            p.x + fx * STRAFE_M,
            p.z + fz * STRAFE_M,
            &mage,
            "skeleton_mage",
        ));
    }
}

pub fn clear_dungeon_bones(combat: &mut WorldCombat) {
    combat.retain_hostiles(|h| !is_bone_id(&h.mob_id));
    if let Some(lock) = combat.lock_id() {
        if !combat.hostiles().iter().any(|h| h.idx == lock) {
            combat.set_lock(None);
        }
    }
}

/// Headless live lock+auto of the L1 Martial fixture wolf. First mitigated hit is 11.
pub fn first_fixture_auto_hit() -> i32 {
    let mut combat = WorldCombat::specialist(1, Discipline::Martial);
    let mut layer = CombatLayer::install();
    layer.request_wolf_fixture();
    layer.install_l1_wolf_line(&mut combat, 0.0, 0.0, 1.0, 0.0);
    layer.start_fixture_encounter(&mut combat);
    combat.press_tab(0.0, 0.0, 1.0, 0.0);
    layer.tick(&mut combat, 0.0, 0.0, 1.0, 0.0, 2.0);
    layer
        .first_auto()
        .expect("fixture L1 Martial auto must land within 2 s")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::math::{mitigation, MAGE_BOLT_DMG, SKULL_BOLT_DMG};
    use crate::world::fauna::FaunaCatalog;

    #[test]
    fn first_mitigated_auto_on_l1_wolf_is_11() {
        assert_eq!(first_fixture_auto_hit(), 11);
    }

    #[test]
    fn orc_sheet_fixture_is_one_orc_with_swing_cd_armed() {
        let mut combat = WorldCombat::specialist(1, Discipline::Martial);
        let mut layer = CombatLayer::install();
        layer.install_orc_fixture(&mut combat, 0.0, 0.0, 1.0, 0.0);
        assert_eq!(combat.hostiles().len(), 1);
        let h = &combat.hostiles()[0];
        assert_eq!(h.mob_id, "orc");
        assert_eq!(h.name, "orc");
        assert!((h.x - 1.5).abs() < 1e-9);
        assert!((h.swing_cd - h.swing_s).abs() < 1e-9);
        assert!((h.reach_m - 2.0).abs() < 1e-9);
        assert_eq!(h.max_hp(), 130.0);
        assert!(layer.wants_orc());
        assert!(layer.fixture_ready());
    }

    #[test]
    fn demon_sheet_fixture_is_one_demon_with_swing_cd_armed() {
        let mut combat = WorldCombat::specialist(1, Discipline::Martial);
        let mut layer = CombatLayer::install();
        layer.install_demon_fixture(&mut combat, 0.0, 0.0, 1.0, 0.0);
        assert_eq!(combat.hostiles().len(), 1);
        let h = &combat.hostiles()[0];
        assert_eq!(h.mob_id, "demon");
        assert_eq!(h.name, "demon");
        assert!((h.x - 1.5).abs() < 1e-9);
        assert!((h.swing_cd - h.swing_s).abs() < 1e-9);
        assert!((h.reach_m - 2.2).abs() < 1e-9);
        assert_eq!(h.max_hp(), 220.0);
        assert_eq!(h.damage, 16);
        assert!(layer.wants_demon());
        assert!(layer.fixture_ready());
    }

    #[test]
    fn blue_demon_sheet_fixture_is_one_blue_demon_with_swing_cd_armed() {
        let mut combat = WorldCombat::specialist(1, Discipline::Martial);
        let mut layer = CombatLayer::install();
        layer.install_bluedemon_fixture(&mut combat, 0.0, 0.0, 1.0, 0.0);
        assert_eq!(combat.hostiles().len(), 1);
        let h = &combat.hostiles()[0];
        assert_eq!(h.mob_id, "blue_demon");
        assert_eq!(h.name, "blue_demon");
        assert!((h.x - 1.5).abs() < 1e-9);
        assert!((h.swing_cd - h.swing_s).abs() < 1e-9);
        assert!((h.reach_m - 2.0).abs() < 1e-9);
        assert_eq!(h.max_hp(), 155.0);
        assert_eq!(h.damage, 12);
        assert!(layer.wants_bluedemon());
        assert!(layer.fixture_ready());
        assert!(!layer.wants_demon());
    }

    #[test]
    fn tribal_veteran_sheet_fixture_is_one_tribal_veteran_with_swing_cd_armed() {
        let mut combat = WorldCombat::specialist(1, Discipline::Martial);
        let mut layer = CombatLayer::install();
        layer.install_tribal_veteran_fixture(&mut combat, 0.0, 0.0, 1.0, 0.0);
        assert_eq!(combat.hostiles().len(), 1);
        let h = &combat.hostiles()[0];
        assert_eq!(h.mob_id, "tribal_veteran");
        assert_eq!(h.name, "tribal_veteran");
        assert!((h.x - 1.5).abs() < 1e-9);
        assert!((h.swing_cd - h.swing_s).abs() < 1e-9);
        assert!((h.reach_m - 1.6).abs() < 1e-9);
        assert_eq!(h.max_hp(), 210.0);
        assert_eq!(h.damage, 22);
        assert!(layer.wants_tribal_veteran());
        assert!(layer.fixture_ready());
        assert!(!layer.wants_bluedemon());
    }

    #[test]
    fn yeti_sheet_fixture_is_one_yeti_with_swing_cd_armed() {
        let mut combat = WorldCombat::specialist(1, Discipline::Martial);
        let mut layer = CombatLayer::install();
        layer.install_yeti_fixture(&mut combat, 0.0, 0.0, 1.0, 0.0);
        assert_eq!(combat.hostiles().len(), 1);
        let h = &combat.hostiles()[0];
        assert_eq!(h.mob_id, "yeti");
        assert_eq!(h.name, "yeti");
        assert!((h.x - 1.5).abs() < 1e-9);
        assert!((h.swing_cd - h.swing_s).abs() < 1e-9);
        assert!((h.reach_m - 2.2).abs() < 1e-9);
        assert_eq!(h.max_hp(), 240.0);
        assert!(layer.wants_yeti());
        assert!(layer.fixture_ready());
    }

    #[test]
    fn held_canonical_mob_freezes_then_starts_once_and_preserves_actor_ids() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../data/OrrunGameData.xml");
        let data = Arc::new(crate::gamedata::GameData::load(path).expect("canonical GameData"));
        let mut combat = WorldCombat::specialist_with_game_data(data, 1, Discipline::Martial);
        let prior = combat.add_canonical_mob(
            &crate::gamedata::MobId::new("wolf"),
            0,
            30.0,
            0.0,
            30.0,
            0.0,
        );
        let mut layer = CombatLayer::install();
        layer.request_canonical_mob_fixture();
        let seated = layer.install_canonical_mob_fixture(
            &mut combat,
            &crate::gamedata::MobId::new("orc"),
            0.0,
            0.0,
            1.0,
            0.0,
        );

        assert_eq!(combat.hostiles().len(), 1);
        assert_eq!(combat.hostiles()[0].mob_id, "orc");
        assert!(seated > prior, "canonical ActorId must remain monotonic");
        let hp = combat.player().resources.hp();
        combat.hostiles_mut()[0].swing_cd = 0.0;
        assert!(layer.tick(&mut combat, 0.0, 0.0, 1.0, 0.0, 20.0).is_none());
        assert_eq!(combat.player().resources.hp(), hp);
        assert!(layer.fixture_encounter_held());

        layer.start_fixture_encounter(&mut combat);
        combat.hostiles_mut()[0].swing_cd = 0.0;
        layer.tick(&mut combat, 0.0, 0.0, 1.0, 0.0, 1.1);
        assert!(combat.player().resources.hp() < hp);
    }

    #[test]
    #[should_panic(expected = "fixture encounter start requires a held fixture")]
    fn held_canonical_mob_cannot_start_twice() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../data/OrrunGameData.xml");
        let data = Arc::new(crate::gamedata::GameData::load(path).expect("canonical GameData"));
        let mut combat = WorldCombat::specialist_with_game_data(data, 1, Discipline::Martial);
        let mut layer = CombatLayer::install();
        layer.request_canonical_mob_fixture();
        layer.install_canonical_mob_fixture(
            &mut combat,
            &crate::gamedata::MobId::new("orc"),
            0.0,
            0.0,
            1.0,
            0.0,
        );
        layer.start_fixture_encounter(&mut combat);
        layer.start_fixture_encounter(&mut combat);
    }

    #[test]
    fn held_wolf_fixture_waits_for_explicit_start_and_tab() {
        let mut combat = WorldCombat::specialist(1, Discipline::Martial);
        let mut layer = CombatLayer::install();
        layer.request_wolf_fixture();
        layer.install_l1_wolf_line(&mut combat, 0.0, 0.0, 1.0, 0.0);
        combat.apply_damage_to_player(500, "fixture setup".into());

        layer.tick(&mut combat, 0.0, 0.0, 1.0, 0.0, 10.0);
        assert!(combat.is_dead());
        assert_eq!(combat.player().resources.hp(), 0.0);
        assert!(combat.lock_id().is_none());
        assert!(layer.first_auto().is_none());

        layer.start_fixture_encounter(&mut combat);
        assert!(!combat.is_dead());
        assert_eq!(
            combat.player().resources.hp(),
            combat.player().resources.hp_max()
        );
        assert!(combat.lock_id().is_none());
        combat.press_tab(0.0, 0.0, 1.0, 0.0);
        assert_eq!(combat.lock_id(), Some(0));
    }

    #[test]
    fn combat_walk_is_4_5_not_10() {
        assert!((CombatLayer::install().walk_mps() - 4.5).abs() < 1e-6);
    }

    #[test]
    fn first_auto_latches_swing_hit_and_opens_pip() {
        let mut combat = WorldCombat::specialist(1, Discipline::Martial);
        let mut layer = CombatLayer::install();
        layer.install_l1_wolf_line(&mut combat, 0.0, 0.0, 1.0, 0.0);
        combat.set_lock(Some(0));
        layer.tick(&mut combat, 0.0, 0.0, 1.0, 0.0, 1.7);
        assert!(layer.first_auto().is_none());
        assert!(!layer.swing_whoosh());
        assert!(!layer.hit_flash());
        assert!(!layer.attack_pip());
        layer.tick(&mut combat, 0.0, 0.0, 1.0, 0.0, 0.2);
        assert_eq!(layer.first_auto(), Some(11));
        assert!(layer.swing_whoosh());
        assert!(layer.hit_flash());
        assert!(layer.attack_pip());
        let sfx = layer.take_combat_sfx();
        assert!(sfx.contains(&CombatSfx::Swing));
        assert!(sfx.contains(&CombatSfx::Hit));
        layer.tick(&mut combat, 0.0, 0.0, 1.0, 0.0, 0.2);
        assert!(!layer.attack_pip());
        assert!(layer.swing_whoosh());
        assert!(layer.hit_flash());
    }

    #[test]
    fn strike_next_swing_is_one_point_five() {
        let mut combat = WorldCombat::specialist(1, Discipline::Martial);
        let mut layer = CombatLayer::install();
        layer.install_l1_wolf_line(&mut combat, 0.0, 0.0, 1.0, 0.0);
        combat.set_lock(Some(0));
        assert!(combat.press_verb(crate::combat::CombatVerb::Strike, 0.0, 0.0, 1.0, 0.0));
        layer.tick(&mut combat, 0.0, 0.0, 1.0, 0.0, 2.0);
        assert_eq!(layer.first_auto(), Some(16));
    }

    #[test]
    fn ember_is_create_and_bind_is_rank_gated() {
        let mut combat = WorldCombat::specialist(1, Discipline::Martial);
        let mut layer = CombatLayer::install();
        layer.install_l1_wolf_line(&mut combat, 0.0, 0.0, 1.0, 0.0);
        combat.set_lock(Some(0));
        assert!(combat.press_verb(crate::combat::CombatVerb::Ember, 0.0, 0.0, 1.0, 0.0));
        assert!(combat.ember_is_started());
        combat.set_cast_kind(None);
        combat.set_gcd(0.0);
        combat.set_busy_time(0.0);
        assert!(!combat.press_verb(crate::combat::CombatVerb::Bind, 0.0, 0.0, 1.0, 0.0));
        let gate = combat
            .last_rank_gate()
            .expect("bind rank miss is fail-loud");
        assert!(gate.blocked);
        assert_eq!(gate.action, crate::combat::CombatVerb::Bind);
    }

    #[test]
    fn ember_starts_on_l1_martial_without_arcane_rank() {
        let mut combat = WorldCombat::specialist(1, Discipline::Martial);
        let mut layer = CombatLayer::install();
        layer.install_l1_wolf_line(&mut combat, 0.0, 0.0, 1.0, 0.0);
        combat.set_lock(Some(0));
        assert_eq!(combat.player().stats.ranks.arcane, 0);
        let mana_before = combat.player().resources.mana();
        assert!(combat.press_verb(crate::combat::CombatVerb::Ember, 0.0, 0.0, 1.0, 0.0));
        assert!(combat.ember_is_started());
        assert_eq!(combat.player().stats.ranks.arcane, 0);
        assert!(combat.player().resources.mana() < mana_before);
    }

    #[test]
    fn bash_is_blocked_on_l1_martial() {
        let mut combat = WorldCombat::specialist(1, Discipline::Martial);
        let mut layer = CombatLayer::install();
        layer.install_l1_wolf_line(&mut combat, 0.0, 0.0, 1.0, 0.0);
        combat.set_lock(Some(0));
        assert!(!combat.press_verb(crate::combat::CombatVerb::Bash, 0.0, 0.0, 1.0, 0.0));
        let gate = combat.last_rank_gate().expect("rank miss is fail-loud");
        assert!(gate.blocked);
        assert_eq!(gate.action, crate::combat::CombatVerb::Bash);
        assert_ne!(combat.cast_kind(), Some("bash"));
    }

    #[test]
    fn potion_heals_forty() {
        let mut combat = WorldCombat::specialist(1, Discipline::Martial);
        combat.set_player_hp(50.0);
        assert!(combat.press_verb(crate::combat::CombatVerb::Potion, 0.0, 0.0, 1.0, 0.0));
        assert_eq!(combat.player().resources.hp(), 90.0);
        assert_eq!(combat.player().potions, 0);
        assert_eq!(combat.last_potion_heal(), 40);
    }

    #[test]
    fn fixture_lock_name_and_mesh_are_canonical_wolf() {
        let mut combat = WorldCombat::specialist(1, Discipline::Martial);
        let mut layer = CombatLayer::install();
        layer.install_l1_wolf_line(&mut combat, 0.0, 0.0, 1.0, 0.0);
        assert_eq!(combat.hostiles().len(), 3);
        let expected = if combat.game_data().is_some() {
            "Wolf"
        } else {
            "wolf-spider"
        };
        assert!(combat.hostiles().iter().all(|h| h.name == expected));
        assert!((combat.hostiles()[0].x - 1.5).abs() < 1e-9);
        assert!(combat.hostiles()[0].z.abs() < 1e-9);
        assert!((combat.hostiles()[1].z + 1.8).abs() < 1e-9);
        assert!((combat.hostiles()[2].z - 1.8).abs() < 1e-9);
        let catalog = FaunaCatalog::load().expect("fauna catalog");
        let spec = catalog.spec("wolf");
        assert_eq!(spec.source, "wolf/wolf.gltf");
        let model = load_combat_model("wolf").expect("wolf.gltf via AnimatedModel");
        assert!(model.find_clip(&spec.anim_idle).is_some());
    }

    #[test]
    fn lock_ring_mesh_is_gold_and_nonempty() {
        let mesh = lock_ring_mesh().expect("ring mesh");
        assert!(mesh.point_count() > 32);
        assert!(mesh.face_count() > 32);
    }

    #[test]
    fn lock_ring_major_stays_inside_one_strafe_gap() {
        assert!(
            RING_MAJOR_M < STRAFE_M as f32,
            "ring major {RING_MAJOR_M} must stay under strafe {STRAFE_M} so one lock reads as one body"
        );
    }

    #[test]
    fn tick_pushes_outgoing_and_incoming_log_lines() {
        let mut combat = WorldCombat::specialist(1, Discipline::Martial);
        let mut layer = CombatLayer::install();
        layer.install_l1_wolf_line(&mut combat, 0.0, 0.0, 1.0, 0.0);
        combat.set_lock(Some(0));
        layer.tick(&mut combat, 0.0, 0.0, 1.0, 0.0, 2.0);
        let lines: Vec<_> = combat.log_lines();
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("You hit wolf-spider for ")),
            "{lines:?}"
        );
        combat.set_player_hp(5.0);
        for h in combat.hostiles_mut() {
            h.swing_cd = 0.0;
        }
        layer.tick(&mut combat, 0.0, 0.0, 1.0, 0.0, 0.2);
        let lines: Vec<_> = combat.log_lines();
        assert!(
            lines.iter().any(|l| l.contains(" hits you for ")),
            "{lines:?}"
        );
        layer.log_potion(&mut combat);
        assert!(combat
            .log_lines()
            .iter()
            .any(|l| l.starts_with("You drink a potion for ")));
    }

    #[test]
    fn ember_cast_pushes_log_line() {
        let mut combat = WorldCombat::specialist(1, Discipline::Martial);
        let mut layer = CombatLayer::install();
        layer.install_l1_wolf_line(&mut combat, 0.0, 0.0, 1.0, 0.0);
        combat.set_lock(Some(0));
        assert!(combat.press_verb(crate::combat::CombatVerb::Ember, 0.0, 0.0, 1.0, 0.0));
        layer.tick(&mut combat, 0.0, 0.0, 1.0, 0.0, 2.5);
        let lines: Vec<_> = combat.log_lines();
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("You Ember wolf-spider for ")),
            "{lines:?}"
        );
    }

    #[test]
    fn mage_bolt_telegraphs_spellcast_shoot_then_deals_mitigated_15() {
        let mut combat = WorldCombat::specialist(1, Discipline::Martial);
        combat.clear_hostiles();
        let sheet = skeleton_mage_sheet();
        combat.add_hostile(hostile_from_sheet(0, 10.0, 0.0, &sheet, "skeleton_mage"));
        let hp0 = combat.player().resources.hp();
        let mut layer = CombatLayer::install();
        layer.tick(&mut combat, 0.0, 0.0, 1.0, 0.0, 0.1);
        assert!(
            layer.mage_tele.contains_key(&0),
            "mage telegraph should start on first tick"
        );
        assert_eq!(
            layer.pending_melee.as_ref().map(|(_, clip)| *clip),
            Some("Spellcast_Shoot"),
            "Spellcast_Shoot queued"
        );
        assert_eq!(combat.player().resources.hp(), hp0);
        layer.tick(&mut combat, 0.0, 0.0, 1.0, 0.0, 1.2);
        let want = mitigation(f64::from(MAGE_BOLT_DMG), combat.player().stats.attrs.grit);
        let skull = mitigation(f64::from(SKULL_BOLT_DMG), combat.player().stats.attrs.grit);
        assert_ne!(want, skull, "mage bolt 15 is not skull bolt 14");
        assert_eq!(combat.player().resources.hp(), hp0 - f64::from(want));
    }

    #[test]
    fn seat_dungeon_bones_is_warrior_two_minions_and_one_non_heart_mage() {
        let mut combat = WorldCombat::specialist(1, Discipline::Martial);
        combat.clear_hostiles();
        let spots = [GlobalXZ::at(0.0, 0.0), GlobalXZ::at(10.0, 0.0)];
        let heart = Some(GlobalXZ::at(10.0, 0.0));
        seat_dungeon_bones(&mut combat, &spots, heart);
        let warriors: Vec<_> = combat
            .hostiles()
            .iter()
            .filter(|h| h.mob_id == "skeleton_warrior")
            .collect();
        let minions: Vec<_> = combat
            .hostiles()
            .iter()
            .filter(|h| h.mob_id == "skeleton_minion")
            .collect();
        let mages: Vec<_> = combat
            .hostiles()
            .iter()
            .filter(|h| h.mob_id == "skeleton_mage")
            .collect();
        assert_eq!(warriors.len(), 2);
        assert_eq!(minions.len(), 4);
        assert_eq!(mages.len(), 1);
        assert!((mages[0].x - STRAFE_M).abs() < 1e-9);
        assert!((mages[0].z).abs() < 1e-9);
        assert_eq!(mages[0].name, "Mage");
        assert_eq!(warriors[0].name, "Warrior");
        assert_eq!(minions[0].name, "Minion");
        assert!((minions[0].z + STRAFE_M).abs() < 1e-9 || (minions[0].z - STRAFE_M).abs() < 1e-9);
    }

    #[test]
    fn skull_bolt_telegraphs_weapon_then_deals_mitigated_14() {
        let mut combat = WorldCombat::specialist(1, Discipline::Martial);
        combat.clear_hostiles();
        let sheet = orc_skull_sheet();
        combat.add_hostile(hostile_from_sheet(0, 10.0, 0.0, &sheet, "orc_skull"));
        let hp0 = combat.player().resources.hp();
        let mut layer = CombatLayer::install();
        layer.tick(&mut combat, 0.0, 0.0, 1.0, 0.0, 0.1);
        assert!(
            layer.skull_tele.contains_key(&0),
            "telegraph should start on first tick"
        );
        assert_eq!(
            layer.pending_melee.as_ref().map(|(_, clip)| *clip),
            Some("Weapon"),
            "Weapon queued"
        );
        assert_eq!(combat.player().resources.hp(), hp0);
        layer.tick(&mut combat, 0.0, 0.0, 1.0, 0.0, 1.2);
        let want = mitigation(f64::from(SKULL_BOLT_DMG), combat.player().stats.attrs.grit);
        assert_eq!(combat.player().resources.hp(), hp0 - f64::from(want));
        let lines: Vec<_> = combat.log_lines();
        assert!(
            lines.iter().any(|l| l.contains(" hits you for ")),
            "{lines:?}"
        );
    }

    #[test]
    fn ember_without_lock_tells_no_target() {
        let mut combat = WorldCombat::specialist(1, Discipline::Martial);
        let mut layer = CombatLayer::install();
        layer.install_l1_wolf_line(&mut combat, 0.0, 0.0, 1.0, 0.0);
        combat.set_lock(None);
        assert!(!combat.press_verb(crate::combat::CombatVerb::Ember, 0.0, 0.0, 1.0, 0.0));
        assert!(!combat.ember_is_started());
        assert_eq!(combat.fail_tell(), Some("No target"));
        let lines: Vec<_> = combat.log_lines();
        assert!(lines.iter().any(|l| l == "No target"), "{lines:?}");
    }

    #[test]
    fn strike_past_melee_tells_out_of_range() {
        let mut combat = WorldCombat::specialist(1, Discipline::Martial);
        let mut layer = CombatLayer::install();
        layer.install_l1_wolf_line(&mut combat, 0.0, 0.0, 1.0, 0.0);
        combat.set_lock(Some(0));
        assert!(!combat.press_verb(crate::combat::CombatVerb::Strike, -4.0, 0.0, 1.0, 0.0));
        assert!(!combat.strike_is_armed());
        assert_eq!(combat.fail_tell(), Some("Out of range"));
        let lines: Vec<_> = combat.log_lines();
        assert!(lines.iter().any(|l| l == "Out of range"), "{lines:?}");
    }

    #[test]
    fn ward_success_pushes_you_ward() {
        let mut combat = WorldCombat::specialist(1, Discipline::Martial);
        combat.player_mut().stats.ranks.arcane = 7;
        let layer = CombatLayer::install();
        assert!(combat.press_verb(crate::combat::CombatVerb::Ward, 0.0, 0.0, 1.0, 0.0));
        assert!(combat.ward_value() > 0.0);
        layer.log_ward(&mut combat);
        let lines: Vec<_> = combat.log_lines();
        assert!(lines.iter().any(|l| l == "You Ward"), "{lines:?}");
    }
}
