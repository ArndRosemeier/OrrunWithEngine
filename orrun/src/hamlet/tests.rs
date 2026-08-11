use glam::Vec2;

use super::catalog::{self, BuildingRole};
use super::config::HamletLabConfig;
use super::planner::plan;
use super::{ShapeKind, Shape};

fn house_obb_overlap(a: &Shape, b: &Shape) -> bool {
    if a.kind != ShapeKind::House || b.kind != ShapeKind::House {
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
        let ra = a.half_size.x * axis.dot(a_axes[0]).abs()
            + a.half_size.y * axis.dot(a_axes[1]).abs();
        let rb = b.half_size.x * axis.dot(b_axes[0]).abs()
            + b.half_size.y * axis.dot(b_axes[1]).abs();
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
    assert!(plan.underfill_message.is_empty(), "{}", plan.underfill_message);

    let mut saw_well = false;
    for s in &plan.shapes {
        if s.kind != ShapeKind::House {
            continue;
        }
        let spec = catalog::spec_for(&s.catalog_id).expect("catalog");
        assert!((s.half_size.x * 2.0 - spec.size_x).abs() < 1e-4);
        assert!((s.half_size.y * 2.0 - spec.size_z).abs() < 1e-4);
        assert!(spec.is_dwelling() || spec.is_civic());
        if s.catalog_id == "Well" {
            saw_well = true;
        }
    }
    assert!(saw_well);
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
    assert!(
        plan.shapes
            .iter()
            .any(|s| s.kind == ShapeKind::House && s.catalog_id == "Bell_Tower")
    );
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
            assert!(
                !house_obb_overlap(houses[i], houses[j]),
                "overlap {} vs {}",
                houses[i].catalog_id,
                houses[j].catalog_id
            );
        }
    }
}

#[test]
fn higher_tier_has_more_market_sides() {
    assert!(super::tier_market_sides(0) < super::tier_market_sides(1));
    assert!(super::tier_market_sides(1) < super::tier_market_sides(2));
    assert_eq!(super::tier_market_sides(3), 24);
    assert!(!catalog::ids_with_role(BuildingRole::Dwelling, 0).is_empty());
}
