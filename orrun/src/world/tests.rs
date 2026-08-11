use engine::surface::WATER_CLEARANCE;
use glam::Vec2;

use super::{AtlasFields, ContinentalSurface};
use crate::atlas::hydro::HydroSink;
use crate::atlas::ContinentAtlas;
use crate::atlas::CELL_METRES;
use super::hydro_geom::{nearest_river, signed_distance_ring};

fn small_surface(seed: i32) -> (ContinentAtlas, ContinentalSurface) {
    let atlas = ContinentAtlas::generate(seed, 64);
    let fields = AtlasFields::build(&atlas);
    let surface = ContinentalSurface::new(&atlas, fields);
    (atlas, surface)
}

#[test]
fn sample_is_deterministic() {
    let (_a, surface) = small_surface(7);
    let s0 = surface.sample_at(12_500.0, 18_000.0);
    let s1 = surface.sample_at(12_500.0, 18_000.0);
    assert_eq!(s0.ground, s1.ground);
    assert_eq!(s0.water_top, s1.water_top);
}

#[test]
fn hydro_vectors_bake_with_sinks() {
    let atlas = ContinentAtlas::generate(5, 64);
    assert_eq!(atlas.schema_version, 5);
    assert!(!atlas.hydro.coasts.is_empty(), "expected coast rings");
    for r in &atlas.hydro.rivers {
        assert!(matches!(
            r.sink,
            HydroSink::Ocean | HydroSink::Lake { .. }
        ));
        assert!(r.points.len() >= 2);
        assert!(r.half_width_m > 0.0);
    }
    for l in &atlas.hydro.lakes {
        assert!(l.ring.len() >= 4);
    }
}

#[test]
fn ocean_wet_columns_obey_clearance() {
    let (atlas, surface) = small_surface(3);
    let mut wet = 0;
    let mut max_err = 0.0_f32;
    let size = atlas.size as i32;
    for az in 0..size {
        for ax in 0..size {
            let wx = (ax as f32 + 0.5) * CELL_METRES;
            let wz = (az as f32 + 0.5) * CELL_METRES;
            let s = surface.sample_at(wx, wz);
            max_err = max_err.max(s.contract_error());
            if s.is_wet() {
                wet += 1;
                assert!(
                    s.ground <= s.water_top - WATER_CLEARANCE + 1e-4,
                    "bed above clearance floor"
                );
            }
        }
    }
    assert!(wet > 0, "expected ocean/lake wet columns");
    assert!(max_err < 1e-4, "max_contract_error={max_err}");
}

#[test]
fn inland_wet_columns_obey_clearance() {
    let (_atlas, surface) = small_surface(5);
    let mut max_err = 0.0_f32;
    for z in 0..80 {
        for x in 0..80 {
            let wx = 8_000.0 + x as f32 * 40.0;
            let wz = 8_000.0 + z as f32 * 40.0;
            let s = surface.sample_at(wx, wz);
            max_err = max_err.max(s.contract_error());
        }
    }
    assert!(max_err < 1e-3, "max_contract_error={max_err}");
}

#[test]
fn river_ribbon_has_fluent_width() {
    let (atlas, surface) = small_surface(5);
    let hydro = surface.hydro();
    let Some(river) = hydro.rivers.first() else {
        return;
    };
    let a = river.points[0];
    let b = river.points.get(1).copied().unwrap_or(a);
    let dir = (b - a).normalize_or_zero();
    if dir.length_squared() < 1e-6 {
        return;
    }
    let perp = Vec2::new(-dir.y, dir.x);
    let mid = (a + b) * 0.5;
    // Along a diagonal-ish cross-cut, wetness should track distance to centreline
    // (not axis-aligned stairs).
    let mut last_wet: Option<bool> = None;
    let mut flips = 0;
    for i in -40..=40 {
        let p = mid + perp * (i as f32 * 2.0);
        let s = surface.sample_at(p.x, p.y);
        let hit = nearest_river(hydro, atlas.size, p);
        let expect_near = hit.map(|h| h.dist < h.half_width).unwrap_or(false);
        if expect_near {
            assert!(
                s.is_wet() || s.water_top.is_finite(),
                "near ribbon should engage water sheet"
            );
        }
        if let Some(prev) = last_wet {
            if prev != s.is_wet() {
                flips += 1;
            }
        }
        last_wet = Some(s.is_wet());
    }
    // A clean ribbon cross-section flips at most twice (enter + leave).
    assert!(flips <= 2, "ribbon cross-cut flips={flips} (stair mask?)");
}

#[test]
fn lake_ring_interior_is_wet() {
    let (atlas, surface) = small_surface(5);
    let Some(lake) = atlas.hydro.lakes.first() else {
        return;
    };
    let mut c = Vec2::ZERO;
    for p in &lake.ring {
        c += *p;
    }
    c /= lake.ring.len() as f32;
    let sd = signed_distance_ring(c, &lake.ring);
    if sd > 10.0 {
        let s = surface.sample_at(c.x, c.y);
        assert!(s.is_wet(), "lake centroid should be wet");
        assert!((s.water_top - lake.surface_z).abs() < 1.0);
    }
}

#[test]
fn wet_sheets_stay_near_ground() {
    let (_atlas, surface) = small_surface(1);
    let mut max_float = 0.0_f32;
    for z in 0..100 {
        for x in 0..100 {
            let wx = 10_000.0 + x as f32 * 50.0;
            let wz = 10_000.0 + z as f32 * 50.0;
            let s = surface.sample_at(wx, wz);
            if s.is_wet() {
                // Water must not hover as a sky wall above the bed.
                let float = s.water_top - s.ground;
                max_float = max_float.max(float);
                assert!(
                    float < 80.0,
                    "sheet floats {float}m above bed at ({wx},{wz}) — sky water"
                );
            }
        }
    }
    assert!(max_float >= WATER_CLEARANCE * 0.5 || max_float == 0.0);
}

#[test]
fn coast_transect_rim_matches_wetness() {
    let (atlas, surface) = small_surface(11);
    let Some(coast) = atlas.hydro.coasts.first() else {
        panic!("expected coast ring");
    };
    let mut c = Vec2::ZERO;
    for p in &coast.ring {
        c += *p;
    }
    c /= coast.ring.len() as f32;
    let p0 = coast.ring[0];
    let dir = (p0 - c).normalize_or_zero();
    let mut saw_dry = false;
    let mut saw_wet = false;
    for i in 0..60 {
        let t = (i as f32 - 20.0) * 25.0;
        let p = p0 + dir * t;
        let s = surface.sample_at(p.x, p.y);
        if s.is_wet() {
            saw_wet = true;
            assert!((s.water_top - s.ground) >= WATER_CLEARANCE - 1e-4);
        } else {
            saw_dry = true;
        }
    }
    assert!(saw_dry && saw_wet, "expected coast transect to cross the rim");
}
