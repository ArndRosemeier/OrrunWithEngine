//! Live combat layer. Not FaunaLayer — fixture hostiles, combat clock, lock+auto.
//!
//! Uses `orrun::combat` types and the same melee_raw / mitigation as the sim.
//! Frame dt is not 0.1; this accumulates to [`crate::combat::TICK`] before a
//! live auto tick.
//!
//! Visible fixture bodies use catalog id `wolf` (`assets/fauna/wolf/wolf.gltf`).
//! There is no spider in the fauna catalog; do not route these through FaunaLayer.

use crate::combat::math::{TICK, WALK_MPS};
use crate::combat::sheets::wolf_sheet;
use crate::combat::types::{WorldCombat, WorldHostile};
use crate::combat::Discipline;
use crate::world::fauna::FaunaCatalog;
use engine::anim::AnimatedModel;
use engine::error::{EngineError, EngineResult};
use engine::place::Place;
use engine::space::GlobalPosition;
use engine::world::{EntityId, World};
use std::path::PathBuf;
use std::sync::Arc;

/// Live hostiles + 0.1 s combat clock in front of the player.
pub struct CombatLayer {
    accum_s: f64,
    fixture: bool,
    first_auto: Option<i32>,
    mesh_ids: Vec<EntityId>,
    wolf_model: Option<Arc<AnimatedModel>>,
}

impl CombatLayer {
    pub fn install() -> Self {
        Self {
            accum_s: 0.0,
            fixture: false,
            first_auto: None,
            mesh_ids: Vec::new(),
            wolf_model: None,
        }
    }

    pub fn fixture_ready(&self) -> bool {
        self.fixture
    }

    pub fn first_auto(&self) -> Option<i32> {
        self.first_auto
    }

    pub fn rearm(&mut self) {
        self.fixture = false;
        self.first_auto = None;
        self.accum_s = 0.0;
    }

    pub fn despawn_meshes(&mut self, world: &mut World) {
        for id in self.mesh_ids.drain(..) {
            world.despawn(id);
        }
    }

    fn wolf_model(&mut self) -> EngineResult<Arc<AnimatedModel>> {
        if let Some(model) = &self.wolf_model {
            return Ok(model.clone());
        }
        let model = load_wolf_model()?;
        self.wolf_model = Some(model.clone());
        Ok(model)
    }

    /// Visible fixture meshes on the live hostiles. Faces the player.
    ///
    /// Mesh is catalog id `wolf` (`assets/fauna/wolf/wolf.gltf`). There is no
    /// spider in the fauna catalog. Spawned like other glTF, not through FaunaLayer.
    pub fn spawn_wolf_meshes(
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
        let model = self.wolf_model()?;
        let yaw = player_yaw_deg + 180.0;
        for (i, h) in combat.hostiles.iter_mut().enumerate() {
            let y = feet_y.get(i).copied().unwrap_or(0.0);
            let pos = GlobalPosition::at(h.x, y, h.z);
            let render = world.to_render(pos)?;
            let place = Place::at(render.x, render.y, render.z)?
                .yaw_deg(yaw)?
                .scale(1.0)?;
            let id = world.spawn_animated_shared(model.clone(), place)?;
            world.play_animation(id, "Idle")?;
            world.set_animation_speed(id, 0.65)?;
            h.entity = Some(id);
            self.mesh_ids.push(id);
        }
        Ok(())
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
        // Line along facing: first at 2.0 m (inside 2.8 melee), then +0.4 m.
        for i in 0..3 {
            let dist = 2.0 + f64::from(i) * 0.4;
            // Combat sheet id is wolf-spider; catalog has wolf, not spider.
            combat.hostiles.push(WorldHostile {
                idx: i,
                x: player_x + fx * dist,
                z: player_z + fz * dist,
                hp: f64::from(sheet.hp),
                max_hp: f64::from(sheet.hp),
                armor: sheet.armor,
                alive: true,
                stun_s: 0.0,
                slow_s: 0.0,
                root_s: 0.0,
                name: "Wolf".into(),
                entity: None,
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
    ) {
        if dt <= 0.0 {
            return;
        }
        self.accum_s += dt;
        while self.accum_s + 1e-12 >= TICK {
            self.accum_s -= TICK;
            combat.tick_verbs(player_x, player_z, TICK);
            if let Some(dealt) =
                combat.tick_melee_auto(player_x, player_z, facing_x, facing_z, TICK)
            {
                if self.first_auto.is_none() {
                    self.first_auto = Some(dealt);
                }
            }
        }
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
    out
}

fn fauna_dir() -> EngineResult<PathBuf> {
    let mut tried = Vec::new();
    if let Some(dir) = std::env::var_os("ORRUN_ASSETS") {
        tried.push(PathBuf::from(dir).join("fauna"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            tried.push(dir.join("assets").join("fauna"));
        }
    }
    tried.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join("fauna"),
    );
    for root in &tried {
        if root.is_dir() {
            return Ok(root.clone());
        }
    }
    Err(EngineError::Model(format!(
        "no fauna assets under {}",
        tried.first().map(|p| p.display().to_string()).unwrap_or_default()
    )))
}

/// Catalog id `wolf` / `wolf/wolf.gltf` via AnimatedModel, same as FaunaLayer.
fn load_wolf_model() -> EngineResult<Arc<AnimatedModel>> {
    let catalog = FaunaCatalog::load().map_err(|e| EngineError::Model(e.to_string()))?;
    let spec = catalog
        .specs()
        .iter()
        .find(|s| s.id == "wolf")
        .ok_or_else(|| EngineError::Model("fauna catalog missing id 'wolf'".into()))?;
    if spec.source != "wolf/wolf.gltf" {
        return Err(EngineError::Model(format!(
            "wolf catalog source must be wolf/wolf.gltf, got {}",
            spec.source
        )));
    }
    let root = fauna_dir()?;
    let path = root.join(&spec.source);
    if !path.is_file() {
        return Err(EngineError::Model(format!(
            "wolf mesh missing at {}",
            path.display()
        )));
    }
    let model = AnimatedModel::load_with(&path, &root, &engine::EngineLimits::default())?;
    if model.find_clip(&spec.anim_idle).is_none() {
        return Err(EngineError::Model(format!(
            "wolf clip '{}' is not in {}",
            spec.anim_idle,
            path.display()
        )));
    }
    Ok(Arc::new(model))
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

    #[test]
    fn first_mitigated_auto_on_l1_wolf_is_11() {
        assert_eq!(first_fixture_auto_hit(), 11);
    }

    #[test]
    fn combat_walk_is_4_5_not_10() {
        assert!((CombatLayer::install().walk_mps() - 4.5).abs() < 1e-6);
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
    fn fixture_lock_name_is_wolf_and_mesh_is_catalog_wolf() {
        let mut combat = WorldCombat::specialist(1, Discipline::Martial);
        let mut layer = CombatLayer::install();
        layer.install_l1_wolf_line(&mut combat, 0.0, 0.0, 1.0, 0.0);
        assert_eq!(combat.hostiles.len(), 3);
        assert!(combat.hostiles.iter().all(|h| h.name == "Wolf"));
        let catalog = FaunaCatalog::load().expect("fauna catalog");
        let spec = catalog.spec("wolf");
        assert_eq!(spec.source, "wolf/wolf.gltf");
        let model = load_wolf_model().expect("wolf.gltf via AnimatedModel");
        assert!(model.find_clip(&spec.anim_idle).is_some());
    }
}
