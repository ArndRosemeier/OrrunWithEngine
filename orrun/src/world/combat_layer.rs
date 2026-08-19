//! Live combat layer. Not FaunaLayer — fixture hostiles, combat clock, lock+auto.
//!
//! Uses `orrun::combat` types and the same melee_raw / mitigation as the sim.
//! Frame dt is not 0.1; this accumulates to [`crate::combat::TICK`] before a
//! live auto tick.

use crate::combat::math::{TICK, WALK_MPS};
use crate::combat::sheets::wolf_sheet;
use crate::combat::types::{WorldCombat, WorldHostile};
use crate::combat::Discipline;

/// Live hostiles + 0.1 s combat clock in front of the player.
pub struct CombatLayer {
    accum_s: f64,
    fixture: bool,
    first_auto: Option<i32>,
}

impl CombatLayer {
    pub fn install() -> Self {
        Self {
            accum_s: 0.0,
            fixture: false,
            first_auto: None,
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
    out
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
    fn ember_is_noop_without_arcane_rank() {
        let mut combat = WorldCombat::specialist(1, Discipline::Martial);
        let mut layer = CombatLayer::install();
        layer.install_l1_wolf_line(&mut combat, 0.0, 0.0, 1.0, 0.0);
        combat.lock = Some(0);
        assert!(!combat.press_verb(crate::combat::CombatVerb::Ember, 0.0, 0.0, 1.0, 0.0));
        assert!(!combat.ember_started);
    }

    #[test]
    fn ember_starts_when_arcane_known() {
        let mut combat = WorldCombat::specialist(1, Discipline::Martial);
        let mut layer = CombatLayer::install();
        layer.install_l1_wolf_line(&mut combat, 0.0, 0.0, 1.0, 0.0);
        combat.lock = Some(0);
        combat.player.stats.ranks.arcane = 1;
        let mana_before = combat.player.resources.mana;
        assert!(combat.press_verb(crate::combat::CombatVerb::Ember, 0.0, 0.0, 1.0, 0.0));
        assert!(combat.ember_started);
        assert!(combat.player.resources.mana < mana_before);
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
}
