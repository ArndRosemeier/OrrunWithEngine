use glam::Vec2;

use super::catalog::{self, BuildingRole};
use super::config::HamletLabConfig;
use super::planner::plan;
use super::{Plot, Shape, ShapeKind};

fn house_obb_overlap(a: &Shape, b: &Shape) -> bool {
    if a.kind == ShapeKind::Market || b.kind == ShapeKind::Market {
        return false;
    }
    const TOUCH_EPS: f32 = 0.05;
    let axes = |yaw: f32| -> [Vec2; 2] {
        let c = yaw.cos();
        let s = yaw.sin();
        [Vec2::new(c, -s).normalize(), Vec2::new(s, c).normalize()]
    };
    let a_axes = axes(a.yaw);
    let b_axes = axes(b.yaw);
    let all = [a_axes[0], a_axes[1], b_axes[0], b_axes[1]];
    let delta = b.center - a.center;
    for axis in all {
        let ra =
            a.half_size.x * axis.dot(a_axes[0]).abs() + a.half_size.y * axis.dot(a_axes[1]).abs();
        let rb =
            b.half_size.x * axis.dot(b_axes[0]).abs() + b.half_size.y * axis.dot(b_axes[1]).abs();
        if delta.dot(axis).abs() >= ra + rb - TOUCH_EPS {
            return false;
        }
    }
    true
}

#[test]
fn hamlet_tier0_places_well_and_dwellings() {
    let mut cfg = HamletLabConfig::default();
    cfg.apply_tier_defaults(0);
    cfg.seed = 1;
    let plan = plan(&cfg).expect("plan");
    assert!(plan.house_count >= 3, "expected several dwellings");
    assert!(plan.civic_count >= 1);
    assert_eq!(plan.market_sides, 6);
    assert!(
        plan.underfill_message.is_empty(),
        "{}",
        plan.underfill_message
    );

    let mut saw_well = false;
    let mut footprints = std::collections::HashSet::new();
    for s in &plan.shapes {
        if s.kind != ShapeKind::House {
            continue;
        }
        if let Some(brief) = s.dwelling {
            assert!((s.half_size.x * 2.0 - brief.size_x()).abs() < 1e-4);
            assert!((s.half_size.y * 2.0 - brief.size_z()).abs() < 1e-4);
            footprints.insert((brief.cells_x, brief.cells_z));
            continue;
        }
        let spec = catalog::spec_for(&s.catalog_id).expect("catalog");
        assert!((s.half_size.x * 2.0 - spec.size_x).abs() < 1e-4);
        assert!((s.half_size.y * 2.0 - spec.size_z).abs() < 1e-4);
        assert!(spec.is_civic());
        if s.catalog_id == "Well" {
            saw_well = true;
        }
    }
    assert!(saw_well);
    assert!(
        footprints.len() >= 2,
        "expected mixed footprints, got {footprints:?}"
    );
}

#[test]
fn village_fills_fixed_dwelling_count() {
    let mut cfg = HamletLabConfig::default();
    cfg.apply_tier_defaults(1);
    cfg.seed = 7;
    cfg.dwelling_min = 20;
    cfg.dwelling_max = 20;
    let plan = plan(&cfg).expect("plan");
    assert_eq!(plan.markets.len(), 1);
    assert_eq!(plan.house_count, 20, "{}", plan.underfill_message);
}

#[test]
fn town_has_secondary_markets() {
    let mut cfg = HamletLabConfig::default();
    cfg.apply_tier_defaults(2);
    cfg.seed = 3;
    cfg.dwelling_min = 30;
    cfg.dwelling_max = 30;
    let plan = plan(&cfg).expect("plan");
    assert_eq!(plan.markets.len(), 3);
    assert_eq!(plan.house_count, 30, "{}", plan.underfill_message);
    assert!(plan.civic_count >= 4);
}

#[test]
fn port_places_bell_tower() {
    let mut cfg = HamletLabConfig::default();
    cfg.apply_tier_defaults(3);
    cfg.seed = 5;
    cfg.dwelling_min = 40;
    cfg.dwelling_max = 40;
    let plan = plan(&cfg).expect("plan");
    assert!(plan
        .shapes
        .iter()
        .any(|s| s.kind == ShapeKind::House && s.catalog_id == "Bell_Tower"));
    assert_eq!(plan.house_count, 40, "{}", plan.underfill_message);
}

#[test]
fn same_seed_is_deterministic() {
    let mut cfg = HamletLabConfig::default();
    cfg.apply_tier_defaults(0);
    cfg.seed = 42;
    let a = plan(&cfg).expect("a");
    let b = plan(&cfg).expect("b");
    assert_eq!(a.house_count, b.house_count);
    assert_eq!(a.civic_count, b.civic_count);
    assert_eq!(a.shapes.len(), b.shapes.len());
    assert!((a.built_envelope - b.built_envelope).abs() < 1e-5);
    for (sa, sb) in a.shapes.iter().zip(b.shapes.iter()) {
        assert_eq!(sa.kind, sb.kind);
        assert_eq!(sa.catalog_id, sb.catalog_id);
        assert_eq!(sa.dwelling, sb.dwelling);
        assert!((sa.center - sb.center).length() < 1e-5);
        assert!((sa.yaw - sb.yaw).abs() < 1e-5);
    }
}

#[test]
fn houses_do_not_overlap() {
    let mut cfg = HamletLabConfig::default();
    cfg.apply_tier_defaults(0);
    cfg.seed = 11;
    let plan = plan(&cfg).expect("plan");
    let houses: Vec<_> = plan
        .shapes
        .iter()
        .filter(|s| s.kind == ShapeKind::House)
        .collect();
    for i in 0..houses.len() {
        for j in (i + 1)..houses.len() {
            let label_i = houses[i]
                .dwelling
                .map(|d| d.label())
                .unwrap_or_else(|| houses[i].catalog_id.clone());
            let label_j = houses[j]
                .dwelling
                .map(|d| d.label())
                .unwrap_or_else(|| houses[j].catalog_id.clone());
            assert!(
                !house_obb_overlap(houses[i], houses[j]),
                "overlap {label_i} vs {label_j}"
            );
        }
    }
}

#[test]
fn higher_tier_has_more_market_sides() {
    assert!(super::tier_market_sides(0) < super::tier_market_sides(1));
    assert!(super::tier_market_sides(1) < super::tier_market_sides(2));
    assert_eq!(super::tier_market_sides(3), 24);
    assert!(!catalog::ids_with_role(BuildingRole::Civic, 0).is_empty());
}

fn castle_cfg(tier: u8, seed: u64) -> HamletLabConfig {
    let mut cfg = HamletLabConfig::default();
    cfg.apply_tier_defaults(tier);
    cfg.seed = seed;
    cfg.place_castle = true;
    cfg
}

#[test]
fn castle_placement_is_opt_in() {
    let mut cfg = HamletLabConfig::default();
    cfg.apply_tier_defaults(2);
    cfg.seed = 3;
    cfg.dwelling_min = 30;
    cfg.dwelling_max = 30;
    let plan = plan(&cfg).expect("plan");
    assert_eq!(plan.castle_count, 0);
    assert!(plan.shapes.iter().all(|s| s.kind != ShapeKind::Castle));
}

#[test]
fn hamlet_lab_skips_castle_on_tier_0() {
    let mut cfg = castle_cfg(0, 1);
    cfg.dwelling_min = 6;
    cfg.dwelling_max = 6;
    let plan = plan(&cfg).expect("plan");
    assert_eq!(plan.castle_count, 0);
    assert!(plan.shapes.iter().all(|s| s.kind != ShapeKind::Castle));
}

fn castle_of(plan: &super::Plan2D) -> &super::Shape {
    plan.shapes
        .iter()
        .find(|s| s.kind == ShapeKind::Castle)
        .expect("castle")
}

#[test]
fn castle_grows_with_settlement_tier() {
    let village = plan(&castle_cfg(1, 7)).expect("village");
    let town = {
        let mut cfg = castle_cfg(2, 7);
        cfg.dwelling_min = 30;
        cfg.dwelling_max = 30;
        plan(&cfg).expect("town")
    };
    let port = {
        let mut cfg = castle_cfg(3, 7);
        cfg.dwelling_min = 40;
        cfg.dwelling_max = 40;
        plan(&cfg).expect("port")
    };
    let v = castle_of(&village);
    let t = castle_of(&town);
    let p = castle_of(&port);
    assert_eq!(v.catalog_id, "castle_keep_8x6");
    assert_eq!(t.catalog_id, "castle_keep_12x10");
    assert_eq!(p.catalog_id, "castle_keep_16x14");
    assert!(t.half_size.x > v.half_size.x && t.half_size.y > v.half_size.y);
    assert!(p.half_size.x > t.half_size.x && p.half_size.y > t.half_size.y);
    assert_eq!(town.house_count, 30, "{}", town.underfill_message);
}

#[test]
fn castle_gate_faces_the_plaza() {
    let plan = plan(&castle_cfg(1, 11)).expect("plan");
    let castle = plan
        .shapes
        .iter()
        .find(|s| s.kind == ShapeKind::Castle)
        .expect("castle");
    let facing = Vec2::new(castle.yaw.sin(), castle.yaw.cos());
    let gate = castle.center - facing * castle.half_size.y;
    assert!(
        gate.distance(plan.plaza) < castle.center.distance(plan.plaza),
        "gate {} should sit closer to the plaza than the keep centre {}",
        gate,
        castle.center
    );
}

#[test]
fn houses_do_not_overlap_the_castle() {
    let mut cfg = castle_cfg(1, 11);
    cfg.dwelling_min = 20;
    cfg.dwelling_max = 20;
    let plan = plan(&cfg).expect("plan");
    let buildings: Vec<_> = plan
        .shapes
        .iter()
        .filter(|s| s.kind == ShapeKind::House || s.kind == ShapeKind::Castle)
        .collect();
    assert!(buildings.iter().any(|s| s.kind == ShapeKind::Castle));
    for i in 0..buildings.len() {
        for j in (i + 1)..buildings.len() {
            let label_i = buildings[i]
                .dwelling
                .map(|d| d.label())
                .unwrap_or_else(|| buildings[i].catalog_id.clone());
            let label_j = buildings[j]
                .dwelling
                .map(|d| d.label())
                .unwrap_or_else(|| buildings[j].catalog_id.clone());
            assert!(
                !house_obb_overlap(buildings[i], buildings[j]),
                "overlap {label_i} vs {label_j}"
            );
        }
    }
}

struct Flat;
impl Plot for Flat {
    fn height(&self, _: Vec2) -> f32 {
        10.0
    }
    fn wetness(&self, _: Vec2) -> f32 {
        -5.0
    }
}

/// Height rises with +Z. Yaw 0 puts the door at −Z, so the door is downhill.
struct DownhillDoor {
    grade: f32,
}
impl Plot for DownhillDoor {
    fn height(&self, p: Vec2) -> f32 {
        p.y * self.grade
    }
    fn wetness(&self, _: Vec2) -> f32 {
        -5.0
    }
}

/// Height falls with +Z. Yaw 0 puts the door uphill — the Godot min-corner trap.
struct UphillDoor {
    grade: f32,
}
impl Plot for UphillDoor {
    fn height(&self, p: Vec2) -> f32 {
        -p.y * self.grade
    }
    fn wetness(&self, _: Vec2) -> f32 {
        -5.0
    }
}

struct Wet;
impl Plot for Wet {
    fn height(&self, _: Vec2) -> f32 {
        4.0
    }
    fn wetness(&self, _: Vec2) -> f32 {
        1.0
    }
}

#[test]
fn door_sill_matches_the_ground_at_the_door() {
    let plot = UphillDoor { grade: 0.22 };
    let center = Vec2::ZERO;
    let half_x = 2.1;
    let half_z = 2.5;
    let yaw = 0.0;
    let sample = super::sample_footprint(&plot, center, half_x, half_z, yaw);
    let door = super::door_point(center, half_z, yaw);
    assert!((sample.door_z - plot.height(door)).abs() < 1e-4);
    let seat = super::seat_building(&sample, 0.7).expect("gentle uphill door is a plot");
    assert!(
        (seat.floor_z - sample.door_z).abs() < 1e-4,
        "floor must be the door, not the lowest corner"
    );
    let min_corner_sit = sample.min_z;
    assert!(
        sample.door_z - min_corner_sit > 0.6,
        "this slope would bury the door if we sat on the lowest corner"
    );
    assert!(
        seat.skirt_height > sample.door_z - sample.min_z,
        "skirt must cover the downhill air under the floor"
    );
}

#[test]
fn downhill_door_does_not_need_a_tall_skirt() {
    let plot = DownhillDoor { grade: 0.22 };
    let sample = super::sample_footprint(&plot, Vec2::ZERO, 2.1, 2.5, 0.0);
    let seat = super::seat_building(&sample, 0.7).expect("gentle downhill door");
    assert!((seat.floor_z - sample.door_z).abs() < 1e-4);
    assert!(seat.skirt_height < 0.5, "door is already the low corner");
}

#[test]
fn wet_and_cliff_plots_are_refused() {
    let wet = super::sample_footprint(&Wet, Vec2::ZERO, 2.0, 2.5, 0.0);
    assert!(super::seat_building(&wet, 0.7).is_none());

    let cliff = super::sample_footprint(&UphillDoor { grade: 0.8 }, Vec2::ZERO, 2.1, 2.5, 0.0);
    assert!(super::seat_building(&cliff, 0.7).is_none());
}

#[test]
fn castle_seat_ignores_a_pit_in_the_bailey() {
    struct BaileyPit;
    impl Plot for BaileyPit {
        fn height(&self, p: Vec2) -> f32 {
            if p.length() < 1.5 {
                -8.0
            } else {
                0.0
            }
        }
        fn wetness(&self, _: Vec2) -> f32 {
            -5.0
        }
    }
    let layout = super::castle_layout("castle_keep_8x6").expect("village");
    let castle = super::sample_castle_footprint(&BaileyPit, Vec2::ZERO, 0.0, layout);
    assert!(
        super::seat_building(&castle, 2.7).is_some(),
        "the bailey is not the castle floor"
    );
    let filled = super::sample_footprint(
        &BaileyPit,
        Vec2::ZERO,
        layout.size_x() * 0.5,
        layout.size_z() * 0.5,
        0.0,
    );
    assert!(
        super::seat_building(&filled, 2.7).is_none(),
        "a house plot would treat the yard as the room"
    );
}

#[test]
fn plan_on_flat_ground_still_places_a_hamlet() {
    let mut cfg = HamletLabConfig::default();
    cfg.apply_tier_defaults(0);
    cfg.seed = 1;
    let plan = super::plan_on(&cfg, Some(&Flat)).expect("plan");
    assert!(plan.house_count >= 3);
    assert!(plan.civic_count >= 1);
}

#[test]
fn plan_on_water_cannot_place_the_well() {
    let mut cfg = HamletLabConfig::default();
    cfg.apply_tier_defaults(0);
    cfg.seed = 1;
    let err = super::plan_on(&cfg, Some(&Wet)).expect_err("well cannot sit in water");
    assert!(err.to_string().contains("Well"), "{err}");
}

#[test]
fn dwelling_storeys_include_both_heights() {
    let mut cfg = HamletLabConfig::default();
    cfg.apply_tier_defaults(1);
    cfg.seed = 21;
    cfg.dwelling_min = 40;
    cfg.dwelling_max = 40;
    let plan = plan(&cfg).expect("plan");
    let mut one = 0u32;
    let mut two = 0u32;
    for s in &plan.shapes {
        match s.dwelling.map(|d| d.storeys) {
            Some(1) => one += 1,
            Some(2) => two += 1,
            _ => {}
        }
    }
    assert!(one > 0, "expected some 1-storey houses");
    assert!(two > 0, "expected some 2-storey houses");
}
