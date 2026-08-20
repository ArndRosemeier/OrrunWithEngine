//! Live combat layer. Not FaunaLayer ? fixture hostiles, combat clock, lock+auto.
//!
//! Uses `orrun::combat` types and the same melee_raw / mitigation as the sim.
//! Frame dt is not 0.1; this accumulates to [`crate::combat::TICK`] before a
//! live auto tick.
//!
//! Visible fixture bodies use catalog id `wolf` (`assets/fauna/wolf/wolf.gltf`).
//! There is no spider in the fauna catalog; do not route these through FaunaLayer.

use crate::combat::catalog::mesh_spec;
use crate::combat::math::{
    mitigation, MAGE_BOLT_DMG, MAGE_BOLT_RANGE_M, MAGE_TELE_S, SKULL_BOLT_DMG,
    SKULL_BOLT_RANGE_M, SKULL_TELE_S, TICK, WALK_MPS,
};
use crate::combat::sheets::{
    orc_sheet, orc_skull_sheet, skeleton_mage_sheet, skeleton_minion_sheet,
    skeleton_warrior_sheet, wolf_sheet, MobSheet,
};
use crate::combat::types::{WorldCombat, WorldHostile};
use crate::combat::Discipline;
use engine::anim::AnimatedModel;
use engine::color::Color;
use engine::error::{EngineError, EngineResult};
use engine::mesh::Mesh;
use engine::place::{GlobalPlace, Place};
use engine::space::{GlobalPosition, GlobalXZ};
use engine::world::{EntityId, World};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Attack pip is a short tell, not the 1.8 s swing clock.
const ATTACK_PIP_S: f64 = 0.15;
const STRAFE_M: f64 = 1.8;
const FLINCH_S: f32 = 4.0;
const FLINCH_PEAK: f32 = 1.32;
const RING_LIFT_M: f64 = 0.14;
const RING_MAJOR_M: f32 = 2.55;
const RING_MINOR_M: f32 = 0.22;
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

/// One-shot combat voices queued on a live auto that deals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CombatSfx {
    Swing,
    Hit,
    Hurt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FixtureKind {
    WolfLine,
    Orc,
    Bones,
    Mage,
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
    accum_s: f64,
    fixture: bool,
    skip_roster_pins: bool,
    first_auto: Option<i32>,
    mesh_ids: Vec<EntityId>,
    mesh_anchors: Vec<MeshAnchor>,
    wolf_model: Option<Arc<AnimatedModel>>,
    models: HashMap<String, Arc<AnimatedModel>>,
    fixture_kind: FixtureKind,
    pending_melee: Option<(Option<EntityId>, &'static str)>,
    skull_tele: HashMap<i32, f64>,
    mage_tele: HashMap<i32, f64>,
    lock_ring: Option<EntityId>,
    ring_on: Option<i32>,
    attack_pip_s: f64,
    swing_whoosh: bool,
    hit_flash: bool,
    pending_sfx: Vec<CombatSfx>,
    pending_flinch: bool,
    flinch: Option<Flinch>,
    flash: Option<EntityId>,
    flash_t: f32,
    incoming_hit: bool,
    hurt_flash_s: f64,
}

impl CombatLayer {
    pub fn install() -> Self {
        Self {
            accum_s: 0.0,
            fixture: false,
            skip_roster_pins: false,
            first_auto: None,
            mesh_ids: Vec::new(),
            mesh_anchors: Vec::new(),
            wolf_model: None,
            models: HashMap::new(),
            fixture_kind: FixtureKind::WolfLine,
            pending_melee: None,
            skull_tele: HashMap::new(),
            mage_tele: HashMap::new(),
            lock_ring: None,
            ring_on: None,
            attack_pip_s: 0.0,
            swing_whoosh: false,
            hit_flash: false,
            pending_sfx: Vec::new(),
            pending_flinch: false,
            flinch: None,
            flash: None,
            flash_t: 0.0,
            incoming_hit: false,
            hurt_flash_s: 0.0,
        }
    }

    pub fn fixture_ready(&self) -> bool {
        self.fixture
    }

    pub fn request_orc_fixture(&mut self) {
        self.fixture_kind = FixtureKind::Orc;
    }

    pub fn request_wolf_fixture(&mut self) {
        self.fixture_kind = FixtureKind::WolfLine;
    }

    pub fn request_bones_fixture(&mut self) {
        self.fixture_kind = FixtureKind::Bones;
    }

    pub fn request_mage_fixture(&mut self) {
        self.fixture_kind = FixtureKind::Mage;
    }

    /// Playtester HOLD rearms stay fixture-only: no overland sites, no roster pins.
    pub fn skip_roster_pins(&mut self) {
        self.skip_roster_pins = true;
    }

    /// Keep current hostiles. Next world tick will not reseat the wolf line.
    pub fn hold_fixture(&mut self) {
        self.fixture = true;
    }

    /// Stream reset already dropped the bodies. Forget ids so a later
    /// despawn does not poke a recycled entity.
    pub fn forget_meshes(&mut self) {
        self.mesh_ids.clear();
        self.mesh_anchors.clear();
        self.lock_ring = None;
        self.ring_on = None;
        self.flash = None;
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

    pub fn hit_flash_latched(&self) -> bool {
        self.hit_flash
    }

    pub fn lock_ring_visible(&self, world: &World) -> bool {
        self.lock_ring
            .map(|id| world.entity(id).is_ok())
            .unwrap_or(false)
    }

    pub fn take_combat_sfx(&mut self) -> Vec<CombatSfx> {
        std::mem::take(&mut self.pending_sfx)
    }

    pub fn log_potion(&self, combat: &mut WorldCombat) {
        let heal = combat.last_potion_heal;
        combat.log.push(format!("You drink a potion for {heal}"));
    }

    pub fn log_ward(&self, combat: &mut WorldCombat) {
        combat.log.push("You Ward");
    }

    pub fn rearm(&mut self) {
        self.fixture = false;
        self.first_auto = None;
        self.accum_s = 0.0;
        self.attack_pip_s = 0.0;
        self.swing_whoosh = false;
        self.hit_flash = false;
        self.pending_sfx.clear();
        self.pending_flinch = false;
        self.flinch = None;
        self.incoming_hit = false;
        self.hurt_flash_s = 0.0;
        self.pending_melee = None;
        self.skull_tele.clear();
        self.mage_tele.clear();
        self.flash = None;
        self.flash_t = 0.0;
        self.ring_on = None;
    }

    pub fn despawn_meshes(&mut self, world: &mut World) {
        for id in self.mesh_ids.drain(..) {
            world.despawn(id);
        }
        self.mesh_anchors.clear();
        self.despawn_ring(world);
        self.despawn_flash(world);
        self.flinch = None;
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
        let spec = mesh_spec(mob_id).ok_or_else(|| {
            EngineError::Model(format!("no combat mesh for '{mob_id}'"))
        })?;
        if let Some(model) = self.models.get(spec.id) {
            return Ok(model.clone());
        }
        let model = load_combat_model(mob_id)?;
        if spec.id == "crawler_spider_wolf" {
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
        for h in &mut combat.hostiles {
            h.entity = None;
        }
        let yaw = player_yaw_deg + 180.0;
        // wolf.gltf is ~5.5 m long. Origin at the combat point puts the
        // camera inside the snout. Keep hit XZ; sit the mesh behind it.
        let away = player_yaw_deg.to_radians();
        let ax = away.sin() as f64;
        let az = away.cos() as f64;
        const WOLF_MESH_BEHIND_M: f64 = 2.55;
        const ORC_MESH_BEHIND_M: f64 = 1.6;
        for (i, h) in combat.hostiles.iter_mut().enumerate() {
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
            if model.find_clip(spec.anim_idle).is_some() {
                world.play_animation(id, spec.anim_idle)?;
                if is_wolf_mesh(&h.mob_id) {
                    world.set_animation_speed(id, 0.65)?;
                }
            }
            h.entity = Some(id);
            self.mesh_ids.push(id);
            self.mesh_anchors.push(MeshAnchor { id, pos, yaw });
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
        let animated: Vec<engine::world::EntityId> = world
            .animated_entities()
            .map(|(id, _)| *id)
            .collect();
        self.mesh_ids.iter().all(|id| {
            animated.iter().any(|a| a == id) || world.entity(*id).is_ok()
        })
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
        let sheet = wolf_sheet(1);
        combat.hostiles.clear();
        combat.lock = None;
        combat.cycle.clear();
        combat.auto_cd = crate::combat::MELEE_SWING_S;
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
            combat.hostiles.push(WorldHostile {
                idx: i,
                x: player_x + fx * dist + sx * strafe,
                z: player_z + fz * dist + sz * strafe,
                hp: f64::from(sheet.hp),
                max_hp: f64::from(sheet.hp),
                armor: sheet.armor,
                alive: true,
                stun_s: 0.0,
                slow_s: 0.0,
                root_s: 0.0,
                name: sheet.name.clone(),
                mob_id: sheet.id.clone(),
                entity: None,
                damage: sheet.damage,
                swing_s: sheet.swing_s,
                swing_cd: sheet.swing_s,
                reach_m: sheet.reach_m,
            });
        }
        *combat = keep_player(combat);
        combat.strike_armed = false;
        combat.ember_started = false;
        combat.last_potion_heal = 0;
        combat.busy = 0.0;
        combat.gcd = 0.0;
        combat.cds = crate::combat::verbs::empty_cds();
        combat.cast_kind = None;
        combat.cast_t = 0.0;
        combat.cast_target = None;
        combat.ward = 0.0;
        combat.ward_t = 0.0;
        combat.mark_t = 0.0;
        combat.second_wind_used = false;
        combat.last_rank_gate = None;
        self.fixture = true;
        self.first_auto = None;
        self.accum_s = 0.0;
        self.attack_pip_s = 0.0;
        self.swing_whoosh = false;
        self.hit_flash = false;
        self.pending_sfx.clear();
        self.pending_flinch = false;
        self.flinch = None;
        self.flash = None;
        self.flash_t = 0.0;
        self.ring_on = None;
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
        combat.hostiles.clear();
        combat.lock = None;
        combat.cycle.clear();
        combat.auto_cd = crate::combat::MELEE_SWING_S;
        combat.hostiles.push(WorldHostile {
            idx: 0,
            x: player_x + fx * 1.5,
            z: player_z + fz * 1.5,
            hp: f64::from(sheet.hp),
            max_hp: f64::from(sheet.hp),
            armor: sheet.armor,
            alive: true,
            stun_s: 0.0,
            slow_s: 0.0,
            root_s: 0.0,
            name: sheet.name.clone(),
            mob_id: "orc".into(),
            entity: None,
            damage: sheet.damage,
            swing_s: sheet.swing_s,
            swing_cd: sheet.swing_s,
            reach_m: sheet.reach_m,
        });
        *combat = keep_player(combat);
        combat.strike_armed = false;
        combat.ember_started = false;
        combat.last_potion_heal = 0;
        combat.busy = 0.0;
        combat.gcd = 0.0;
        combat.cds = crate::combat::verbs::empty_cds();
        combat.cast_kind = None;
        combat.cast_t = 0.0;
        combat.cast_target = None;
        combat.ward = 0.0;
        combat.ward_t = 0.0;
        combat.mark_t = 0.0;
        combat.second_wind_used = false;
        combat.last_rank_gate = None;
        self.fixture_kind = FixtureKind::Orc;
        self.fixture = true;
        self.first_auto = None;
        self.accum_s = 0.0;
        self.attack_pip_s = 0.0;
        self.swing_whoosh = false;
        self.hit_flash = false;
        self.pending_sfx.clear();
        self.pending_flinch = false;
        self.flinch = None;
        self.flash = None;
        self.flash_t = 0.0;
        self.ring_on = None;
        self.pending_melee = None;
        self.skull_tele.clear();
        self.incoming_hit = false;
        self.hurt_flash_s = 0.0;
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
        combat.hostiles.clear();
        combat.lock = None;
        combat.cycle.clear();
        combat.auto_cd = crate::combat::MELEE_SWING_S;
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
            combat.hostiles.push(hostile_from_sheet(
                i,
                player_x + fx * dist + sx * strafe,
                player_z + fz * dist + sz * strafe,
                sheet,
                mob_id,
            ));
        }
        *combat = keep_player(combat);
        combat.strike_armed = false;
        combat.ember_started = false;
        combat.last_potion_heal = 0;
        combat.busy = 0.0;
        combat.gcd = 0.0;
        combat.cds = crate::combat::verbs::empty_cds();
        combat.cast_kind = None;
        combat.cast_t = 0.0;
        combat.cast_target = None;
        combat.ward = 0.0;
        combat.ward_t = 0.0;
        combat.mark_t = 0.0;
        combat.second_wind_used = false;
        combat.last_rank_gate = None;
        self.fixture_kind = FixtureKind::Bones;
        self.fixture = true;
        self.first_auto = None;
        self.accum_s = 0.0;
        self.attack_pip_s = 0.0;
        self.swing_whoosh = false;
        self.hit_flash = false;
        self.pending_sfx.clear();
        self.pending_flinch = false;
        self.flinch = None;
        self.flash = None;
        self.flash_t = 0.0;
        self.ring_on = None;
        self.pending_melee = None;
        self.skull_tele.clear();
        self.mage_tele.clear();
        self.incoming_hit = false;
        self.hurt_flash_s = 0.0;
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
        combat.hostiles.clear();
        combat.lock = None;
        combat.cycle.clear();
        combat.auto_cd = crate::combat::MELEE_SWING_S;
        combat.hostiles.push(hostile_from_sheet(
            0,
            player_x + fx * 1.5,
            player_z + fz * 1.5,
            &sheet,
            "skeleton_mage",
        ));
        *combat = keep_player(combat);
        combat.strike_armed = false;
        combat.ember_started = false;
        combat.last_potion_heal = 0;
        combat.busy = 0.0;
        combat.gcd = 0.0;
        combat.cds = crate::combat::verbs::empty_cds();
        combat.cast_kind = None;
        combat.cast_t = 0.0;
        combat.cast_target = None;
        combat.ward = 0.0;
        combat.ward_t = 0.0;
        combat.mark_t = 0.0;
        combat.second_wind_used = false;
        combat.last_rank_gate = None;
        self.fixture_kind = FixtureKind::Mage;
        self.fixture = true;
        self.first_auto = None;
        self.accum_s = 0.0;
        self.attack_pip_s = 0.0;
        self.swing_whoosh = false;
        self.hit_flash = false;
        self.pending_sfx.clear();
        self.pending_flinch = false;
        self.flinch = None;
        self.flash = None;
        self.flash_t = 0.0;
        self.ring_on = None;
        self.pending_melee = None;
        self.skull_tele.clear();
        self.mage_tele.clear();
        self.incoming_hit = false;
        self.hurt_flash_s = 0.0;
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
        if self.attack_pip_s > 0.0 {
            self.attack_pip_s = (self.attack_pip_s - dt).max(0.0);
        }
        if self.hurt_flash_s > 0.0 {
            self.hurt_flash_s = (self.hurt_flash_s - dt).max(0.0);
        }
        let mut just = None;
        self.accum_s += dt;
        while self.accum_s + 1e-12 >= TICK {
            self.accum_s -= TICK;
            let pending_cast = combat.cast_kind;
            let lock_id = combat.lock;
            let lock_hp = lock_id.and_then(|id| {
                combat
                    .hostiles
                    .iter()
                    .find(|h| h.idx == id)
                    .map(|h| (h.name.clone(), h.hp, h.root_s, h.slow_s))
            });
            let player_hp = combat.player.resources.hp;
            let ward = combat.ward;
            let skull_hp: HashMap<i32, f64> = combat
                .hostiles
                .iter()
                .filter(|h| h.mob_id == "orc_skull")
                .map(|h| (h.idx, h.hp))
                .collect();
            let mage_hp: HashMap<i32, f64> = combat
                .hostiles
                .iter()
                .filter(|h| h.mob_id == "skeleton_mage")
                .map(|h| (h.idx, h.hp))
                .collect();
            combat.tick_verbs(player_x, player_z, TICK);
            log_finished_cast(
                combat,
                pending_cast,
                lock_id,
                lock_hp,
                player_hp,
                ward,
            );
            let strike = combat.strike_armed;
            if let Some(dealt) =
                combat.tick_melee_auto(player_x, player_z, facing_x, facing_z, TICK)
            {
                if self.first_auto.is_none() {
                    self.first_auto = Some(dealt);
                }
                self.attack_pip_s = ATTACK_PIP_S;
                self.swing_whoosh = true;
                self.hit_flash = true;
                self.pending_sfx.push(CombatSfx::Swing);
                self.pending_sfx.push(CombatSfx::Hit);
                self.pending_flinch = true;
                just = Some(dealt);
                let name = combat
                    .lock
                    .and_then(|id| {
                        combat
                            .hostiles
                            .iter()
                            .find(|h| h.idx == id)
                            .map(|h| h.name.clone())
                    })
                    .or_else(|| combat.hostiles.first().map(|h| h.name.clone()))
                    .unwrap_or_else(|| "wolf-spider".into());
                if strike {
                    combat.log.push(format!("You Strike {name} for {dealt}"));
                } else {
                    combat.log.push(format!("You hit {name} for {dealt}"));
                }
            }
            if let Some(hit) = combat.tick_incoming(player_x, player_z, TICK) {
                self.incoming_hit = true;
                self.hurt_flash_s = 0.15;
                self.pending_sfx.push(CombatSfx::Hurt);
                combat
                    .log
                    .push(format!("{} hits you for {}", hit.by, hit.dealt));
                if hit.killed {
                    combat.log.push("You are slain");
                }
                if let Some(h) = combat
                    .hostiles
                    .iter()
                    .find(|h| h.name == hit.by || h.mob_id == hit.by)
                {
                    queue_connecting_melee(&mut self.pending_melee, &self.models, h);
                }
            }
            self.tick_skull_bolts(combat, player_x, player_z, &skull_hp);
            self.tick_mage_bolts(combat, player_x, player_z, &mage_hp);
        }
        just
    }

    fn tick_skull_bolts(
        &mut self,
        combat: &mut WorldCombat,
        player_x: f64,
        player_z: f64,
        skull_hp: &HashMap<i32, f64>,
    ) {
        if combat.dead {
            self.skull_tele.clear();
            return;
        }
        let grit = combat.player.stats.attrs.grit;
        let ids: Vec<i32> = combat
            .hostiles
            .iter()
            .filter(|h| h.mob_id == "orc_skull")
            .map(|h| h.idx)
            .collect();
        for idx in ids {
            let Some(slot) = combat.hostiles.iter().position(|h| h.idx == idx) else {
                self.skull_tele.remove(&idx);
                continue;
            };
            let took_hp = skull_hp
                .get(&idx)
                .is_some_and(|hp| combat.hostiles[slot].hp < *hp);
            let alive = combat.hostiles[slot].alive;
            let stun_s = combat.hostiles[slot].stun_s;
            let hx = combat.hostiles[slot].x;
            let hz = combat.hostiles[slot].z;
            if !alive || stun_s > 0.0 || took_hp {
                self.skull_tele.remove(&idx);
                continue;
            }
            let dx = player_x - hx;
            let dz = player_z - hz;
            if (dx * dx + dz * dz).sqrt() > SKULL_BOLT_RANGE_M {
                continue;
            }
            if let Some(rem) = self.skull_tele.get(&idx).copied() {
                let next = rem - TICK;
                if next > 1e-12 {
                    self.skull_tele.insert(idx, next);
                    continue;
                }
                self.skull_tele.remove(&idx);
                let dealt = mitigation(f64::from(SKULL_BOLT_DMG), grit);
                combat.player.resources.hp =
                    (combat.player.resources.hp - f64::from(dealt)).max(0.0);
                let killed = combat.player.resources.hp <= 0.0;
                let name = combat.hostiles[slot].name.clone();
                combat.log.push(format!("{name} hits you for {dealt}"));
                self.incoming_hit = true;
                self.hurt_flash_s = 0.15;
                self.pending_sfx.push(CombatSfx::Hurt);
                if killed {
                    combat.dead = true;
                    combat.lock = None;
                    combat.auto_cd = 999.0;
                    combat.slain_by = Some(name);
                    combat.slain_hold_s = crate::combat::math::SLAIN_HOLD_S;
                    combat.log.push("You are slain");
                    break;
                }
            } else {
                self.skull_tele.insert(idx, SKULL_TELE_S);
                queue_weapon_anim(
                    &mut self.pending_melee,
                    &self.models,
                    &combat.hostiles[slot],
                );
            }
        }
    }

    fn tick_mage_bolts(
        &mut self,
        combat: &mut WorldCombat,
        player_x: f64,
        player_z: f64,
        mage_hp: &HashMap<i32, f64>,
    ) {
        if combat.dead {
            self.mage_tele.clear();
            return;
        }
        let grit = combat.player.stats.attrs.grit;
        let ids: Vec<i32> = combat
            .hostiles
            .iter()
            .filter(|h| h.mob_id == "skeleton_mage")
            .map(|h| h.idx)
            .collect();
        for idx in ids {
            let Some(slot) = combat.hostiles.iter().position(|h| h.idx == idx) else {
                self.mage_tele.remove(&idx);
                continue;
            };
            let took_hp = mage_hp
                .get(&idx)
                .is_some_and(|hp| combat.hostiles[slot].hp < *hp);
            let alive = combat.hostiles[slot].alive;
            let stun_s = combat.hostiles[slot].stun_s;
            let hx = combat.hostiles[slot].x;
            let hz = combat.hostiles[slot].z;
            if !alive || stun_s > 0.0 || took_hp {
                self.mage_tele.remove(&idx);
                continue;
            }
            let dx = player_x - hx;
            let dz = player_z - hz;
            if (dx * dx + dz * dz).sqrt() > MAGE_BOLT_RANGE_M {
                continue;
            }
            if let Some(rem) = self.mage_tele.get(&idx).copied() {
                let next = rem - TICK;
                if next > 1e-12 {
                    self.mage_tele.insert(idx, next);
                    continue;
                }
                self.mage_tele.remove(&idx);
                let dealt = mitigation(f64::from(MAGE_BOLT_DMG), grit);
                combat.player.resources.hp =
                    (combat.player.resources.hp - f64::from(dealt)).max(0.0);
                let killed = combat.player.resources.hp <= 0.0;
                let name = combat.hostiles[slot].name.clone();
                combat.log.push(format!("{name} hits you for {dealt}"));
                self.incoming_hit = true;
                self.hurt_flash_s = 0.15;
                self.pending_sfx.push(CombatSfx::Hurt);
                if killed {
                    combat.dead = true;
                    combat.lock = None;
                    combat.auto_cd = 999.0;
                    combat.slain_by = Some(name);
                    combat.slain_hold_s = crate::combat::math::SLAIN_HOLD_S;
                    combat.log.push("You are slain");
                    break;
                }
            } else {
                self.mage_tele.insert(idx, MAGE_TELE_S);
                let h = &combat.hostiles[slot];
                let spec = mesh_spec(&h.mob_id).unwrap_or_else(|| {
                    panic!(
                        "{}",
                        EngineError::Model(format!(
                            "tick_mage_bolts: no mesh spec for '{}'",
                            h.mob_id
                        ))
                    )
                });
                let clip = spec.anim_weapon.unwrap_or_else(|| {
                    panic!(
                        "{}",
                        EngineError::Model(format!(
                            "tick_mage_bolts: '{}' has no anim_weapon",
                            h.mob_id
                        ))
                    )
                });
                if clip != "Spellcast_Shoot" {
                    panic!(
                        "{}",
                        EngineError::Model(format!(
                            "tick_mage_bolts: want Spellcast_Shoot, got '{clip}'"
                        ))
                    );
                }
                self.pending_melee = Some((h.entity, clip));
            }
        }
    }

    /// Play catalog anim_melee on the locked mesh only.
    pub fn replay_melee(&mut self, world: &mut World, combat: &WorldCombat) {
        let Some(lock) = combat.lock else {
            panic!("{}", EngineError::Model("replay_melee: no lock".into()));
        };
        let Some(h) = combat.hostiles.iter().find(|h| h.idx == lock) else {
            panic!("{}", EngineError::Model("replay_melee: lock not in hostiles".into()));
        };
        let spec = mesh_spec(&h.mob_id).unwrap_or_else(|| {
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
        if let Err(err) = world.play_animation(id, spec.anim_melee) {
            panic!(
                "{}",
                EngineError::Model(format!("melee clip '{}' failed: {err}", spec.anim_melee))
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
        let Some(lock) = combat.lock else {
            panic!("{}", EngineError::Model("replay_weapon: no lock".into()));
        };
        let Some(h) = combat.hostiles.iter().find(|h| h.idx == lock) else {
            panic!("{}", EngineError::Model("replay_weapon: lock not in hostiles".into()));
        };
        let spec = mesh_spec(&h.mob_id).unwrap_or_else(|| {
            panic!(
                "{}",
                EngineError::Model(format!("replay_weapon: no mesh spec for '{}'", h.mob_id))
            )
        });
        let clip = spec.anim_weapon.unwrap_or_else(|| {
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
        if let Err(err) = world.play_animation(id, clip) {
            panic!(
                "{}",
                EngineError::Model(format!("weapon clip '{clip}' failed: {err}"))
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
        self.present(world, combat, |_, _| player_y, dt as f32)
    }

    /// Lock ring + hit flinch. Call after [`Self::tick`] with the live world.
    pub fn present(
        &mut self,
        world: &mut World,
        combat: &WorldCombat,
        mut ground_y: impl FnMut(f64, f64) -> f64,
        dt: f32,
    ) -> EngineResult<()> {
        if let Some((id, clip)) = self.pending_melee.take() {
            if let Some(id) = id {
                world.play_animation(id, clip).map_err(|err| {
                    EngineError::Model(format!("melee clip '{clip}' failed: {err}"))
                })?;
            }
        }
        self.sync_lock_ring(world, combat, &mut ground_y)?;
        if self.pending_flinch {
            self.pending_flinch = false;
            self.start_flinch(world, combat)?;
            self.spawn_flash(world, combat, &mut ground_y)?;
        }
        self.tick_flinch(world, dt)?;
        self.tick_flash(world, dt);
        Ok(())
    }

    fn sync_lock_ring(
        &mut self,
        world: &mut World,
        combat: &WorldCombat,
        ground_y: &mut impl FnMut(f64, f64) -> f64,
    ) -> EngineResult<()> {
        let want = combat.lock.filter(|_| self.fixture);
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
        let Some(h) = combat.hostiles.iter().find(|h| h.idx == lock && h.alive) else {
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


    fn spawn_flash(
        &mut self,
        world: &mut World,
        combat: &WorldCombat,
        ground_y: &mut impl FnMut(f64, f64) -> f64,
    ) -> EngineResult<()> {
        self.despawn_flash(world);
        let Some(lock) = combat.lock else {
            return Ok(());
        };
        let Some(h) = combat.hostiles.iter().find(|h| h.idx == lock) else {
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

    fn start_flinch(&mut self, world: &mut World, combat: &WorldCombat) -> EngineResult<()> {
        let Some(lock) = combat.lock else {
            return Ok(());
        };
        let Some(h) = combat.hostiles.iter().find(|h| h.idx == lock) else {
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
                    .hostiles
                    .iter()
                    .find(|h| h.idx == id)
                    .map(|h| h.name.clone())
            })
        })
        .unwrap_or_else(|| "wolf-spider".into());
    match kind {
        "aimed" | "pin" | "ember" => {
            if let Some((_, hp0, _, _)) = before {
                if let Some(h) = lock_id.and_then(|id| combat.hostiles.iter().find(|h| h.idx == id))
                {
                    let dealt = (hp0 - h.hp).round() as i32;
                    if dealt > 0 {
                        let verb = match kind {
                            "aimed" => "Aimed Shot",
                            "pin" => "Pin",
                            "ember" => "Ember",
                            _ => kind,
                        };
                        combat.log.push(format!("You {verb} {name} for {dealt}"));
                    }
                }
            }
        }
        "bind" => {
            if let Some((_, _, root0, _)) = before {
                if let Some(h) = lock_id.and_then(|id| combat.hostiles.iter().find(|h| h.idx == id))
                {
                    if h.root_s > root0 {
                        combat.log.push(format!("You Bind {name}"));
                    }
                }
            }
        }
        "mend" => {
            let heal = (combat.player.resources.hp - player_hp).round() as i32;
            if heal > 0 {
                combat.log.push(format!("You Mend for {heal}"));
            }
        }
        "ward" => {
            if combat.ward > ward {
                combat.log.push("You Ward");
            }
        }
        _ => {}
    }
}

fn keep_player(combat: &WorldCombat) -> WorldCombat {
    let mut out = WorldCombat::specialist(combat.player.stats.level, combat.player.stats.discipline);
    out.player = combat.player.clone();
    out.hostiles = combat.hostiles.clone();
    out.lock = combat.lock;
    out.cycle = combat.cycle.clone();
    out.auto_cd = combat.auto_cd;
    out.last_auto_dealt = combat.last_auto_dealt;
    out.strike_armed = combat.strike_armed;
    out.ember_started = combat.ember_started;
    out.last_potion_heal = combat.last_potion_heal;
    out.busy = combat.busy;
    out.gcd = combat.gcd;
    out.cds = combat.cds.clone();
    out.cast_kind = combat.cast_kind;
    out.cast_t = combat.cast_t;
    out.cast_target = combat.cast_target;
    out.ward = combat.ward;
    out.ward_t = combat.ward_t;
    out.mark_t = combat.mark_t;
    out.second_wind_used = combat.second_wind_used;
    out.last_rank_gate = combat.last_rank_gate;
    out.dead = combat.dead;
    out.slain_by = combat.slain_by.clone();
    out.slain_hold_s = combat.slain_hold_s;
    out.last_incoming = combat.last_incoming.clone();
    out.log = combat.log.clone();
    out.fail_tell = combat.fail_tell;
    out.fail_tell_s = combat.fail_tell_s;
    out
}


fn is_wolf_mesh(mob_id: &str) -> bool {
    matches!(mob_id, "crawler_spider_wolf" | "wolf" | "wolf-spider")
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

fn queue_weapon_anim(
    pending: &mut Option<(Option<EntityId>, &'static str)>,
    models: &HashMap<String, Arc<AnimatedModel>>,
    h: &WorldHostile,
) {
    let Some(spec) = mesh_spec(&h.mob_id) else {
        return;
    };
    let Some(clip) = spec.anim_weapon else {
        return;
    };
    queue_clip(pending, models, h, clip);
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
        tried.first().map(|p| p.display().to_string()).unwrap_or_default()
    )))
}

fn load_combat_model(mob_id: &str) -> EngineResult<Arc<AnimatedModel>> {
    let spec = mesh_spec(mob_id).ok_or_else(|| {
        EngineError::Model(format!("no combat mesh for '{mob_id}'"))
    })?;
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
    WorldHostile {
        idx,
        x,
        z,
        hp: f64::from(sheet.hp),
        max_hp: f64::from(sheet.hp),
        armor: sheet.armor,
        alive: true,
        stun_s: 0.0,
        slow_s: 0.0,
        root_s: 0.0,
        name: sheet.name.clone(),
        mob_id: mob_id.into(),
        entity: None,
        damage: sheet.damage,
        swing_s: sheet.swing_s,
        swing_cd: sheet.swing_s,
        reach_m: sheet.reach_m,
    }
}

pub fn seat_dungeon_skulls(combat: &mut WorldCombat, spots: &[GlobalXZ]) {
    let mut idx = combat.hostiles.iter().map(|h| h.idx).max().unwrap_or(-1) + 1;
    let sheet = orc_skull_sheet();
    for p in spots {
        combat
            .hostiles
            .push(hostile_from_sheet(idx, p.x, p.z, &sheet, "orc_skull"));
        idx += 1;
    }
}

pub fn clear_dungeon_skulls(combat: &mut WorldCombat) {
    combat.hostiles.retain(|h| h.mob_id != "orc_skull");
    if let Some(lock) = combat.lock {
        if !combat.hostiles.iter().any(|h| h.idx == lock) {
            combat.lock = None;
        }
    }
}

pub fn seat_dungeon_bones(
    combat: &mut WorldCombat,
    spots: &[GlobalXZ],
    heart: Option<GlobalXZ>,
) {
    let mut idx = combat.hostiles.iter().map(|h| h.idx).max().unwrap_or(-1) + 1;
    let warrior = skeleton_warrior_sheet();
    let minion = skeleton_minion_sheet();
    let mage = skeleton_mage_sheet();
    let (fx, fz) = (1.0, 0.0);
    let (sx, sz) = (-fz, fx);
    for p in spots {
        combat.hostiles.push(hostile_from_sheet(
            idx,
            p.x,
            p.z,
            &warrior,
            "skeleton_warrior",
        ));
        idx += 1;
        combat.hostiles.push(hostile_from_sheet(
            idx,
            p.x + sx * -STRAFE_M,
            p.z + sz * -STRAFE_M,
            &minion,
            "skeleton_minion",
        ));
        idx += 1;
        combat.hostiles.push(hostile_from_sheet(
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
        combat.hostiles.push(hostile_from_sheet(
            idx,
            p.x + fx * STRAFE_M,
            p.z + fz * STRAFE_M,
            &mage,
            "skeleton_mage",
        ));
    }
}

pub fn clear_dungeon_bones(combat: &mut WorldCombat) {
    combat.hostiles.retain(|h| !is_bone_id(&h.mob_id));
    if let Some(lock) = combat.lock {
        if !combat.hostiles.iter().any(|h| h.idx == lock) {
            combat.lock = None;
        }
    }
}

/// Headless live lock+auto of the L1 Martial fixture wolf. First mitigated hit is 11.
pub fn first_fixture_auto_hit() -> i32 {
    let mut combat = WorldCombat::specialist(1, Discipline::Martial);
    let mut layer = CombatLayer::install();
    layer.install_l1_wolf_line(&mut combat, 0.0, 0.0, 1.0, 0.0);
    combat.lock = Some(0);
    layer.tick(&mut combat, 0.0, 0.0, 1.0, 0.0, 2.0);
    layer
        .first_auto()
        .expect("fixture L1 Martial auto must land within 2 s")
}

#[cfg(test)]
mod tests {
    use super::*;
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
        assert_eq!(combat.hostiles.len(), 1);
        let h = &combat.hostiles[0];
        assert_eq!(h.mob_id, "orc");
        assert_eq!(h.name, "orc");
        assert!((h.x - 1.5).abs() < 1e-9);
        assert!((h.swing_cd - h.swing_s).abs() < 1e-9);
        assert!((h.reach_m - 2.0).abs() < 1e-9);
        assert_eq!(h.max_hp, 130.0);
        assert!(layer.wants_orc());
        assert!(layer.fixture_ready());
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
        combat.lock = Some(0);
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
        combat.lock = Some(0);
        assert!(combat.press_verb(crate::combat::CombatVerb::Strike, 0.0, 0.0, 1.0, 0.0));
        layer.tick(&mut combat, 0.0, 0.0, 1.0, 0.0, 2.0);
        assert_eq!(layer.first_auto(), Some(16));
    }

    #[test]
    fn ember_is_create_and_bind_is_rank_gated() {
        let mut combat = WorldCombat::specialist(1, Discipline::Martial);
        let mut layer = CombatLayer::install();
        layer.install_l1_wolf_line(&mut combat, 0.0, 0.0, 1.0, 0.0);
        combat.lock = Some(0);
        assert!(combat.press_verb(crate::combat::CombatVerb::Ember, 0.0, 0.0, 1.0, 0.0));
        assert!(combat.ember_started);
        combat.cast_kind = None;
        combat.gcd = 0.0;
        combat.busy = 0.0;
        assert!(!combat.press_verb(crate::combat::CombatVerb::Bind, 0.0, 0.0, 1.0, 0.0));
        let gate = combat.last_rank_gate.expect("bind rank miss is fail-loud");
        assert!(gate.blocked);
        assert_eq!(gate.action, crate::combat::CombatVerb::Bind);
    }

    #[test]
    fn ember_starts_on_l1_martial_without_arcane_rank() {
        let mut combat = WorldCombat::specialist(1, Discipline::Martial);
        let mut layer = CombatLayer::install();
        layer.install_l1_wolf_line(&mut combat, 0.0, 0.0, 1.0, 0.0);
        combat.lock = Some(0);
        assert_eq!(combat.player.stats.ranks.arcane, 0);
        let mana_before = combat.player.resources.mana;
        assert!(combat.press_verb(crate::combat::CombatVerb::Ember, 0.0, 0.0, 1.0, 0.0));
        assert!(combat.ember_started);
        assert_eq!(combat.player.stats.ranks.arcane, 0);
        assert!(combat.player.resources.mana < mana_before);
    }

    #[test]
    fn bash_is_blocked_on_l1_martial() {
        let mut combat = WorldCombat::specialist(1, Discipline::Martial);
        let mut layer = CombatLayer::install();
        layer.install_l1_wolf_line(&mut combat, 0.0, 0.0, 1.0, 0.0);
        combat.lock = Some(0);
        assert!(!combat.press_verb(crate::combat::CombatVerb::Bash, 0.0, 0.0, 1.0, 0.0));
        let gate = combat.last_rank_gate.expect("rank miss is fail-loud");
        assert!(gate.blocked);
        assert_eq!(gate.action, crate::combat::CombatVerb::Bash);
        assert_ne!(combat.cast_kind, Some("bash"));
    }

    #[test]
    fn potion_heals_forty() {
        let mut combat = WorldCombat::specialist(1, Discipline::Martial);
        combat.player.resources.hp = 50.0;
        assert!(combat.press_verb(crate::combat::CombatVerb::Potion, 0.0, 0.0, 1.0, 0.0));
        assert_eq!(combat.player.resources.hp, 90.0);
        assert_eq!(combat.player.potions, 0);
        assert_eq!(combat.last_potion_heal, 40);
    }

    #[test]
    fn fixture_lock_name_is_wolf_spider_and_mesh_is_catalog_wolf() {
        let mut combat = WorldCombat::specialist(1, Discipline::Martial);
        let mut layer = CombatLayer::install();
        layer.install_l1_wolf_line(&mut combat, 0.0, 0.0, 1.0, 0.0);
        assert_eq!(combat.hostiles.len(), 3);
        assert!(combat.hostiles.iter().all(|h| h.name == "wolf-spider"));
        assert!((combat.hostiles[0].x - 1.5).abs() < 1e-9);
        assert!(combat.hostiles[0].z.abs() < 1e-9);
        assert!((combat.hostiles[1].z + 1.8).abs() < 1e-9);
        assert!((combat.hostiles[2].z - 1.8).abs() < 1e-9);
        let catalog = FaunaCatalog::load().expect("fauna catalog");
        let spec = catalog.spec("wolf");
        assert_eq!(spec.source, "wolf/wolf.gltf");
        let model = load_combat_model("crawler_spider_wolf").expect("wolf.gltf via AnimatedModel");
        assert!(model.find_clip(&spec.anim_idle).is_some());
    }

    #[test]
    fn lock_ring_mesh_is_gold_and_nonempty() {
        let mesh = lock_ring_mesh().expect("ring mesh");
        assert!(mesh.point_count() > 32);
        assert!(mesh.face_count() > 32);
    }

    #[test]
    fn tick_pushes_outgoing_and_incoming_log_lines() {
        let mut combat = WorldCombat::specialist(1, Discipline::Martial);
        let mut layer = CombatLayer::install();
        layer.install_l1_wolf_line(&mut combat, 0.0, 0.0, 1.0, 0.0);
        combat.lock = Some(0);
        layer.tick(&mut combat, 0.0, 0.0, 1.0, 0.0, 2.0);
        let lines: Vec<_> = combat.log.lines().map(str::to_string).collect();
        assert!(
            lines.iter().any(|l| l.starts_with("You hit wolf-spider for ")),
            "{lines:?}"
        );
        combat.player.resources.hp = 5.0;
        for h in &mut combat.hostiles {
            h.swing_cd = 0.0;
        }
        layer.tick(&mut combat, 0.0, 0.0, 1.0, 0.0, 0.2);
        let lines: Vec<_> = combat.log.lines().map(str::to_string).collect();
        assert!(
            lines.iter().any(|l| l.contains(" hits you for ")),
            "{lines:?}"
        );
        layer.log_potion(&mut combat);
        assert!(combat.log.lines().any(|l| l.starts_with("You drink a potion for ")));
    }

    #[test]
    fn ember_cast_pushes_log_line() {
        let mut combat = WorldCombat::specialist(1, Discipline::Martial);
        let mut layer = CombatLayer::install();
        layer.install_l1_wolf_line(&mut combat, 0.0, 0.0, 1.0, 0.0);
        combat.lock = Some(0);
        assert!(combat.press_verb(crate::combat::CombatVerb::Ember, 0.0, 0.0, 1.0, 0.0));
        layer.tick(&mut combat, 0.0, 0.0, 1.0, 0.0, 2.5);
        let lines: Vec<_> = combat.log.lines().map(str::to_string).collect();
        assert!(
            lines.iter().any(|l| l.starts_with("You Ember wolf-spider for ")),
            "{lines:?}"
        );
    }

    #[test]
    fn mage_bolt_telegraphs_spellcast_shoot_then_deals_mitigated_15() {
        let mut combat = WorldCombat::specialist(1, Discipline::Martial);
        combat.hostiles.clear();
        let sheet = skeleton_mage_sheet();
        combat
            .hostiles
            .push(hostile_from_sheet(0, 10.0, 0.0, &sheet, "skeleton_mage"));
        let hp0 = combat.player.resources.hp;
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
        assert_eq!(combat.player.resources.hp, hp0);
        layer.tick(&mut combat, 0.0, 0.0, 1.0, 0.0, 1.2);
        let want = mitigation(f64::from(MAGE_BOLT_DMG), combat.player.stats.attrs.grit);
        let skull = mitigation(f64::from(SKULL_BOLT_DMG), combat.player.stats.attrs.grit);
        assert_ne!(want, skull, "mage bolt 15 is not skull bolt 14");
        assert_eq!(combat.player.resources.hp, hp0 - f64::from(want));
    }

    #[test]
    fn seat_dungeon_bones_is_warrior_two_minions_and_one_non_heart_mage() {
        let mut combat = WorldCombat::specialist(1, Discipline::Martial);
        combat.hostiles.clear();
        let spots = [
            GlobalXZ::at(0.0, 0.0),
            GlobalXZ::at(10.0, 0.0),
        ];
        let heart = Some(GlobalXZ::at(10.0, 0.0));
        seat_dungeon_bones(&mut combat, &spots, heart);
        let warriors: Vec<_> = combat
            .hostiles
            .iter()
            .filter(|h| h.mob_id == "skeleton_warrior")
            .collect();
        let minions: Vec<_> = combat
            .hostiles
            .iter()
            .filter(|h| h.mob_id == "skeleton_minion")
            .collect();
        let mages: Vec<_> = combat
            .hostiles
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
        combat.hostiles.clear();
        let sheet = orc_skull_sheet();
        combat
            .hostiles
            .push(hostile_from_sheet(0, 10.0, 0.0, &sheet, "orc_skull"));
        let hp0 = combat.player.resources.hp;
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
        assert_eq!(combat.player.resources.hp, hp0);
        layer.tick(&mut combat, 0.0, 0.0, 1.0, 0.0, 1.2);
        let want = mitigation(f64::from(SKULL_BOLT_DMG), combat.player.stats.attrs.grit);
        assert_eq!(combat.player.resources.hp, hp0 - f64::from(want));
        let lines: Vec<_> = combat.log.lines().map(str::to_string).collect();
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
        combat.lock = None;
        assert!(!combat.press_verb(crate::combat::CombatVerb::Ember, 0.0, 0.0, 1.0, 0.0));
        assert!(!combat.ember_started);
        assert_eq!(combat.fail_tell(), Some("No target"));
        let lines: Vec<_> = combat.log.lines().map(str::to_string).collect();
        assert!(lines.iter().any(|l| l == "No target"), "{lines:?}");
    }

    #[test]
    fn strike_past_melee_tells_out_of_range() {
        let mut combat = WorldCombat::specialist(1, Discipline::Martial);
        let mut layer = CombatLayer::install();
        layer.install_l1_wolf_line(&mut combat, 0.0, 0.0, 1.0, 0.0);
        combat.lock = Some(0);
        assert!(!combat.press_verb(crate::combat::CombatVerb::Strike, -4.0, 0.0, 1.0, 0.0));
        assert!(!combat.strike_armed);
        assert_eq!(combat.fail_tell(), Some("Out of range"));
        let lines: Vec<_> = combat.log.lines().map(str::to_string).collect();
        assert!(lines.iter().any(|l| l == "Out of range"), "{lines:?}");
    }

    #[test]
    fn ward_success_pushes_you_ward() {
        let mut combat = WorldCombat::specialist(1, Discipline::Martial);
        combat.player.stats.ranks.arcane = 7;
        let layer = CombatLayer::install();
        assert!(combat.press_verb(crate::combat::CombatVerb::Ward, 0.0, 0.0, 1.0, 0.0));
        assert!(combat.ward > 0.0);
        layer.log_ward(&mut combat);
        let lines: Vec<_> = combat.log.lines().map(str::to_string).collect();
        assert!(lines.iter().any(|l| l == "You Ward"), "{lines:?}");
    }
}
