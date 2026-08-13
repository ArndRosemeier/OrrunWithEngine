//! Evolutionary marketplace hamlet (2D lab).

use glam::Vec2;
use rand::Rng;
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;

use super::castle;
use super::catalog::{self, BuildingRole};
use super::config::{self, HamletLabConfig};
use super::occupancy::{point_in_polygon, Occupancy};
use super::seat::{self, Plot};
use super::{HamletError, PlacedBuilding, Plan2D, Shape, ShapeKind};

#[derive(Clone, Debug)]
struct Candidate {
    score: f32,
    center: Vec2,
    yaw: f32,
    half_x: f32,
    half_z: f32,
}

pub fn plan(config: &HamletLabConfig) -> Result<Plan2D, HamletError> {
    plan_on(config, None)
}

/// Pack a hamlet. When `plot` is set, wet / steep / high-relief candidates lose
/// and flatter ground is preferred. The 2D lab passes `None` and is unchanged.
pub fn plan_on(config: &HamletLabConfig, plot: Option<&dyn Plot>) -> Result<Plan2D, HamletError> {
    let mut rng = ChaCha8Rng::seed_from_u64(config.seed);
    let mut out = Plan2D {
        plaza: Vec2::ZERO,
        market_radius: config.market_radius,
        market_sides: config.market_side_count(),
        ..Plan2D::default()
    };

    let primary = build_market_polygon(
        config,
        out.plaza,
        out.market_sides,
        config.market_radius,
        &mut rng,
    )?;
    out.market_polygon = primary.clone();
    out.markets.push(primary.clone());
    out.market_centers.push(out.plaza);
    out.shapes.push(Shape {
        kind: ShapeKind::Market,
        center: out.plaza,
        half_size: Vec2::ZERO,
        yaw: 0.0,
        radius: config.market_radius,
        catalog_id: String::new(),
        polygon: primary,
    });

    if config.tier >= 2 {
        add_secondary_markets(config, &mut out, &mut rng)?;
    }

    let world_half = config.max_settle_radius + 16.0;
    let mut occ = Occupancy::setup(world_half, config.occupancy_cell);
    for poly in &out.markets {
        occ.stamp_polygon(poly);
    }

    let dwelling_ids = catalog::ids_with_role(BuildingRole::Dwelling, config.tier);
    if dwelling_ids.is_empty() {
        return Err(HamletError::NoDwellings { tier: config.tier });
    }
    let max_depth = dwelling_ids
        .iter()
        .map(|id| catalog::spec_for(id).expect("catalog id").size_z)
        .fold(0.0_f32, f32::max);

    let want_lo = config.dwelling_min.min(config.dwelling_max);
    let want_hi = config.dwelling_min.max(config.dwelling_max);
    let want = rng.gen_range(want_lo..=want_hi);
    out.want_count = want;

    let mut houses: Vec<PlacedBuilding> = Vec::new();
    let mut frontier_r = 0.0_f32;
    for i in 0..out.markets.len() {
        let extent = polygon_extent_from(out.market_centers[i], &out.markets[i]);
        frontier_r = frontier_r.max(
            out.market_centers[i].length() + extent + config.market_front_gap + max_depth + 4.0,
        );
    }
    let expand_step = max_depth * 0.65 + config.alley;

    if config.place_castle {
        place_castle(
            config,
            &mut out,
            &mut occ,
            &mut houses,
            &mut rng,
            max_depth,
            plot,
        )?;
    }

    for civic_id in civic_ids_for_tier(config.tier)? {
        let placed = place_building(
            config,
            &mut out,
            &mut occ,
            &mut houses,
            &mut rng,
            civic_id,
            frontier_r,
            expand_step,
            true,
            plot,
        )?
        .expect("civic placement required");
        frontier_r = frontier_r.max(placed);
        out.civic_count += 1;
    }

    for _ in 0..want {
        let catalog_id = dwelling_ids[rng.gen_range(0..dwelling_ids.len())];
        if let Some(new_r) = place_building(
            config,
            &mut out,
            &mut occ,
            &mut houses,
            &mut rng,
            catalog_id,
            frontier_r,
            expand_step,
            false,
            plot,
        )? {
            frontier_r = frontier_r.max(new_r);
        }
    }

    out.house_count = houses
        .iter()
        .filter(|h| {
            catalog::spec_for(&h.catalog_id)
                .map(|s| s.is_dwelling())
                .unwrap_or(false)
        })
        .count() as u32;

    if out.house_count < out.want_count {
        out.underfill_message = format!(
            "UNDERFILL: placed {} / {} dwellings (tier {}, markets {}, settle_r {:.0})",
            out.house_count,
            out.want_count,
            config.tier,
            out.markets.len(),
            config.max_settle_radius
        );
        eprintln!("HamletLabPlanner: {}", out.underfill_message);
    }

    let mut max_d = 0.0_f32;
    for i in 0..out.markets.len() {
        max_d = max_d.max(
            out.market_centers[i].length()
                + polygon_extent_from(out.market_centers[i], &out.markets[i]),
        );
    }
    for h in &houses {
        max_d = max_d.max(h.center.length() + h.half_z);
    }
    out.built_envelope = max_d + 3.0;

    if config.show_occupancy {
        out.occupancy_dots = occ.occupied_dots(3);
    }
    Ok(out)
}

fn place_castle(
    config: &HamletLabConfig,
    plan: &mut Plan2D,
    occ: &mut Occupancy,
    houses: &mut Vec<PlacedBuilding>,
    rng: &mut ChaCha8Rng,
    house_depth: f32,
    plot: Option<&dyn Plot>,
) -> Result<(), HamletError> {
    let Some(catalog_id) = castle::id_for_tier(config.tier) else {
        return Ok(());
    };
    let spec = catalog::spec_for(catalog_id).ok_or_else(|| HamletError::UnknownCatalogId {
        id: catalog_id.to_string(),
    })?;
    if spec.min_tier > config.tier {
        return Err(HamletError::CivicTier {
            id: catalog_id.to_string(),
            min_tier: spec.min_tier,
            settlement_tier: config.tier,
        });
    }
    let half_x = spec.half_x();
    let half_z = spec.half_z();
    let layout = castle::layout_for(catalog_id)
        .unwrap_or_else(|| panic!("{catalog_id} is not a castle layout"));
    let reach = (half_x * half_x + half_z * half_z).sqrt();

    let mut min_r = 0.0_f32;
    for i in 0..plan.markets.len() {
        let extent = polygon_extent_from(plan.market_centers[i], &plan.markets[i]);
        min_r = min_r.max(
            plan.market_centers[i].length()
                + extent
                + config.market_front_gap
                + house_depth
                + config.alley
                + half_z,
        );
    }
    let max_r = (config.max_settle_radius - reach).max(0.0);
    if min_r > max_r {
        return Err(HamletError::PlaceFailed {
            catalog_id: catalog_id.to_string(),
        });
    }

    let mut pick: Option<Candidate> = None;
    let mut local_max = (min_r + house_depth * 2.0 + 12.0).min(max_r);
    let mut attempts = 0;
    while pick.is_none() && attempts < 8 {
        attempts += 1;
        let mut tries = config.candidates_per_settler as usize;
        if attempts >= 7 {
            tries *= 3;
        }
        let mut scored = Vec::new();
        for _ in 0..tries {
            let ang = rng.gen::<f32>() * std::f32::consts::TAU;
            let dir = Vec2::new(ang.cos(), ang.sin());
            let span = (local_max - min_r).max(0.0);
            let dist = min_r + span * rng.gen::<f32>().powf(1.6);
            let center = plan.plaza + dir * dist;
            if center.length() + reach > config.max_settle_radius {
                continue;
            }
            let away = center - plan.plaza;
            if away.length_squared() < 1e-8 {
                continue;
            }
            let facing = away.normalize();
            let yaw = facing.x.atan2(facing.y);
            if !occ.fits_obb(center, half_x, half_z, yaw, config.alley * 0.5) {
                continue;
            }
            if houses_obb_overlap(houses, center, half_x, half_z, yaw) {
                continue;
            }
            let gate = center - facing * half_z;
            if signed_distance_to_markets(gate, &plan.markets) < 0.4 {
                continue;
            }
            if signed_distance_to_markets(center, &plan.markets) < half_z {
                continue;
            }
            let Some(score) = scored_castle(
                config,
                &plan.markets,
                gate,
                center,
                yaw,
                layout,
                spec.foundation_m,
                plot,
                rng,
            ) else {
                continue;
            };
            scored.push(Candidate {
                score,
                center,
                yaw,
                half_x,
                half_z,
            });
        }
        pick = best_scored(&scored);
        if pick.is_none() {
            let next_max = (local_max + house_depth * 0.65 + config.alley).min(max_r);
            if next_max <= local_max + 1e-4 {
                break;
            }
            local_max = next_max;
        }
    }

    let Some(pick) = pick else {
        return Err(HamletError::PlaceFailed {
            catalog_id: catalog_id.to_string(),
        });
    };

    plan.shapes.push(Shape {
        kind: ShapeKind::Castle,
        center: pick.center,
        half_size: Vec2::new(pick.half_x, pick.half_z),
        yaw: pick.yaw,
        radius: 0.0,
        catalog_id: catalog_id.to_string(),
        polygon: Vec::new(),
    });
    occ.stamp_obb(pick.center, pick.half_x, pick.half_z, pick.yaw, 0.0);
    houses.push(PlacedBuilding {
        center: pick.center,
        half_x: pick.half_x,
        half_z: pick.half_z,
        yaw: pick.yaw,
        catalog_id: catalog_id.to_string(),
    });
    plan.castle_count = 1;
    Ok(())
}

fn civic_ids_for_tier(tier: u8) -> Result<Vec<&'static str>, HamletError> {
    let raw = config::CIVIC_BY_TIER[tier.min(3) as usize];
    let mut out = Vec::with_capacity(raw.len());
    for id in raw {
        let spec = catalog::spec_for(id).ok_or_else(|| HamletError::UnknownCatalogId {
            id: (*id).to_string(),
        })?;
        if spec.min_tier > tier {
            return Err(HamletError::CivicTier {
                id: (*id).to_string(),
                min_tier: spec.min_tier,
                settlement_tier: tier,
            });
        }
        out.push(*id);
    }
    Ok(out)
}

fn place_building(
    config: &HamletLabConfig,
    plan: &mut Plan2D,
    occ: &mut Occupancy,
    houses: &mut Vec<PlacedBuilding>,
    rng: &mut ChaCha8Rng,
    catalog_id: &str,
    frontier_r: f32,
    expand_step: f32,
    require_place: bool,
    plot: Option<&dyn Plot>,
) -> Result<Option<f32>, HamletError> {
    let spec = catalog::spec_for(catalog_id).ok_or_else(|| HamletError::UnknownCatalogId {
        id: catalog_id.to_string(),
    })?;
    let half_x = spec.half_x();
    let half_z = spec.half_z();
    let yaw_offset = spec.yaw_offset;

    let mut pick: Option<Candidate> = None;
    let mut local_max = frontier_r.min(config.max_settle_radius);
    let mut attempts = 0;
    while pick.is_none() && attempts < 8 {
        attempts += 1;
        let mut tries = config.candidates_per_settler as usize;
        if attempts >= 7 {
            tries *= 3;
        }
        let hug_scored = sample_wall_share_candidates(
            config,
            plan,
            occ,
            houses,
            rng,
            half_x,
            half_z,
            spec.foundation_m,
            local_max,
            tries,
            plot,
        );
        let free_scored = sample_settler_candidates(
            config,
            plan,
            occ,
            houses,
            rng,
            half_x,
            half_z,
            yaw_offset,
            spec.foundation_m,
            local_max,
            tries,
            plot,
        );
        let hug_pick = best_scored(&hug_scored);
        let free_pick = if free_scored.is_empty() {
            None
        } else {
            Some(softmax_pick(&free_scored, config.select_temperature, rng))
        };
        pick = match (hug_pick, free_pick) {
            (Some(h), Some(f)) => {
                if h.score >= f.score {
                    Some(h)
                } else {
                    Some(f)
                }
            }
            (Some(h), None) => Some(h),
            (None, Some(f)) => Some(f),
            (None, None) => {
                let next_max = (local_max + expand_step).min(config.max_settle_radius);
                if next_max <= local_max + 1e-4 {
                    break;
                }
                local_max = next_max;
                None
            }
        };
    }

    let Some(pick) = pick else {
        if require_place {
            return Err(HamletError::PlaceFailed {
                catalog_id: catalog_id.to_string(),
            });
        }
        return Ok(None);
    };

    plan.shapes.push(Shape {
        kind: ShapeKind::House,
        center: pick.center,
        half_size: Vec2::new(pick.half_x, pick.half_z),
        yaw: pick.yaw,
        radius: 0.0,
        catalog_id: catalog_id.to_string(),
        polygon: Vec::new(),
    });
    occ.stamp_obb(pick.center, pick.half_x, pick.half_z, pick.yaw, 0.0);
    houses.push(PlacedBuilding {
        center: pick.center,
        half_x: pick.half_x,
        half_z: pick.half_z,
        yaw: pick.yaw,
        catalog_id: catalog_id.to_string(),
    });
    Ok(Some(frontier_r.max(local_max)))
}

fn add_secondary_markets(
    config: &HamletLabConfig,
    out: &mut Plan2D,
    rng: &mut ChaCha8Rng,
) -> Result<(), HamletError> {
    let child_tier = config.tier.saturating_sub(2);
    let child_r = config::tier_market_radius(child_tier);
    let child_sides = config::tier_market_sides(child_tier);
    let preferred_orbit = config.market_radius * 3.0;
    let primary_extent = polygon_extent_from(out.plaza, &out.market_polygon);
    let min_dist = primary_extent + child_r * 1.9 + config.market_front_gap + 2.0;
    let orbit = preferred_orbit.max(min_dist);
    let base_ang = rng.gen::<f32>() * std::f32::consts::TAU;

    for i in 0..2 {
        let mut placed = false;
        for _ in 0..64 {
            let ang = base_ang + i as f32 * std::f32::consts::PI + rng.gen_range(-0.4..=0.4);
            let mut dist = rng.gen_range(min_dist.max(orbit * 0.7)..=orbit);
            dist = dist.max(min_dist);
            let center = out.plaza + Vec2::new(ang.cos(), ang.sin()) * dist;
            let mut clear = true;
            for j in 1..out.market_centers.len() {
                if center.distance(out.market_centers[j]) < child_r * 3.2 {
                    clear = false;
                    break;
                }
            }
            if !clear {
                continue;
            }
            let poly = build_market_polygon(config, center, child_sides, child_r, rng)?;
            if markets_overlap(&out.market_polygon, &poly) {
                continue;
            }
            let mut overlaps_other = false;
            for j in 1..out.markets.len() {
                if markets_overlap(&out.markets[j], &poly) {
                    overlaps_other = true;
                    break;
                }
            }
            if overlaps_other {
                continue;
            }

            out.markets.push(poly.clone());
            out.market_centers.push(center);
            out.shapes.push(Shape {
                kind: ShapeKind::Market,
                center,
                half_size: Vec2::ZERO,
                yaw: 0.0,
                radius: child_r,
                catalog_id: String::new(),
                polygon: poly,
            });
            placed = true;
            break;
        }
        if !placed {
            return Err(HamletError::SecondaryMarket { index: i });
        }
    }
    Ok(())
}

fn polygon_extent_from(center: Vec2, poly: &[Vec2]) -> f32 {
    poly.iter()
        .map(|p| p.distance(center))
        .fold(0.0_f32, f32::max)
}

fn markets_overlap(a: &[Vec2], b: &[Vec2]) -> bool {
    a.iter().any(|p| point_in_polygon(*p, b)) || b.iter().any(|p| point_in_polygon(*p, a))
}

fn build_market_polygon(
    config: &HamletLabConfig,
    plaza: Vec2,
    sides: usize,
    mean_radius: f32,
    rng: &mut ChaCha8Rng,
) -> Result<Vec<Vec2>, HamletError> {
    if sides < 3 {
        return Err(HamletError::MarketSides { sides });
    }
    let n = sides;
    let mean_r = mean_radius.max(1.0);
    let aspect_lo = config.market_aspect_min.min(config.market_aspect_max);
    let aspect_hi = config.market_aspect_min.max(config.market_aspect_max);
    let aspect = rng.gen_range(aspect_lo..=aspect_hi).max(1.05);
    let semi_major = mean_r * aspect.sqrt();
    let semi_minor = mean_r / aspect.sqrt();
    let ellipse_yaw = rng.gen::<f32>() * std::f32::consts::TAU;

    let sector = std::f32::consts::TAU / n as f32;
    let max_ang_jit = sector * 0.5 * config.market_angle_jitter.clamp(0.0, 0.95);
    let r_jit = config.market_radius_jitter.clamp(0.0, 0.9);
    let sample_rot = rng.gen::<f32>() * std::f32::consts::TAU;

    let mut verts = Vec::with_capacity(n);
    for i in 0..n {
        let ang = sample_rot + i as f32 * sector + rng.gen_range(-max_ang_jit..=max_ang_jit);
        let r_ell = ellipse_polar_radius(ang, semi_major, semi_minor, ellipse_yaw)?;
        let mut scale = rng.gen_range((1.0 - r_jit)..=(1.0 + r_jit));
        if rng.gen::<f32>() < 0.28 {
            scale *= rng.gen_range(0.55..=0.85);
        } else if rng.gen::<f32>() < 0.22 {
            scale *= rng.gen_range(1.15..=1.45);
        }
        let r = (r_ell * scale).max(mean_r * 0.22);
        verts.push(plaza + Vec2::new(ang.cos(), ang.sin()) * r);
    }

    if !point_in_polygon(plaza, &verts) {
        return Err(HamletError::PlazaOutsideMarket);
    }
    Ok(verts)
}

fn ellipse_polar_radius(
    world_angle: f32,
    semi_a: f32,
    semi_b: f32,
    ellipse_yaw: f32,
) -> Result<f32, HamletError> {
    let local = world_angle - ellipse_yaw;
    let c = local.cos();
    let s = local.sin();
    let denom = (semi_b * c) * (semi_b * c) + (semi_a * s) * (semi_a * s);
    if denom < 1e-10 {
        return Err(HamletError::DegenerateEllipse);
    }
    Ok((semi_a * semi_b) / denom.sqrt())
}

fn ray_hit_polygon_rim(origin: Vec2, dir: Vec2, poly: &[Vec2]) -> Option<(Vec2, Vec2, f32)> {
    let d = dir.normalize_or_zero();
    if d.length_squared() < 1e-12 {
        return None;
    }
    let mut best_t = f32::INFINITY;
    let mut best_point = Vec2::ZERO;
    let mut best_out = Vec2::ZERO;
    let n = poly.len();
    for i in 0..n {
        let a = poly[i];
        let b = poly[(i + 1) % n];
        let Some(t) = ray_segment_intersect(origin, d, a, b) else {
            continue;
        };
        if t < 1e-4 || t >= best_t {
            continue;
        }
        let point = origin + d * t;
        let edge = b - a;
        if edge.length_squared() < 1e-8 {
            continue;
        }
        let mut nrm = Vec2::new(-edge.y, edge.x).normalize();
        if nrm.dot(point - origin) < 0.0 {
            nrm = -nrm;
        }
        best_t = t;
        best_point = point;
        best_out = nrm;
    }
    if best_t.is_finite() {
        Some((best_point, best_out, best_t))
    } else {
        None
    }
}

fn ray_segment_intersect(origin: Vec2, dir: Vec2, a: Vec2, b: Vec2) -> Option<f32> {
    let v = b - a;
    let cross_dv = dir.x * v.y - dir.y * v.x;
    if cross_dv.abs() < 1e-8 {
        return None;
    }
    let ao = a - origin;
    let t = (ao.x * v.y - ao.y * v.x) / cross_dv;
    let u = (ao.x * dir.y - ao.y * dir.x) / cross_dv;
    if t > 0.0 && (0.0..=1.0).contains(&u) {
        Some(t)
    } else {
        None
    }
}

fn signed_distance_to_polygon(p: Vec2, poly: &[Vec2]) -> f32 {
    let mut best = f32::INFINITY;
    let n = poly.len();
    for i in 0..n {
        best = best.min(point_segment_distance(p, poly[i], poly[(i + 1) % n]));
    }
    if point_in_polygon(p, poly) {
        -best
    } else {
        best
    }
}

fn point_segment_distance(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    p.distance(closest_point_on_segment(p, a, b))
}

fn closest_point_on_segment(p: Vec2, a: Vec2, b: Vec2) -> Vec2 {
    let ab = b - a;
    let len2 = ab.length_squared();
    if len2 < 1e-10 {
        return a;
    }
    let t = ((p - a).dot(ab) / len2).clamp(0.0, 1.0);
    a + ab * t
}

fn closest_point_on_polygon(p: Vec2, poly: &[Vec2]) -> Vec2 {
    let mut best = poly[0];
    let mut best_d2 = f32::INFINITY;
    let n = poly.len();
    for i in 0..n {
        let q = closest_point_on_segment(p, poly[i], poly[(i + 1) % n]);
        let d2 = p.distance_squared(q);
        if d2 < best_d2 {
            best_d2 = d2;
            best = q;
        }
    }
    best
}

fn max_ray_in_disk(origin: Vec2, dir: Vec2, disk_center: Vec2, disk_r: f32) -> f32 {
    let d = dir.normalize_or_zero();
    let o = origin - disk_center;
    let b = 2.0 * o.dot(d);
    let c = o.length_squared() - disk_r * disk_r;
    let disc = b * b - 4.0 * c;
    if disc < 0.0 {
        return -1.0;
    }
    let t = (-b + disc.sqrt()) * 0.5;
    if t <= 0.0 {
        -1.0
    } else {
        t
    }
}

fn pick_attractor_index(plan: &Plan2D, rng: &mut ChaCha8Rng) -> usize {
    if plan.markets.len() <= 1 {
        return 0;
    }
    if rng.gen::<f32>() < 0.55 {
        0
    } else {
        1 + rng.gen_range(0..plan.markets.len() - 1)
    }
}

fn closest_point_on_markets(p: Vec2, markets: &[Vec<Vec2>]) -> Vec2 {
    let mut best = markets[0][0];
    let mut best_d2 = f32::INFINITY;
    for poly in markets {
        let q = closest_point_on_polygon(p, poly);
        let d2 = p.distance_squared(q);
        if d2 < best_d2 {
            best_d2 = d2;
            best = q;
        }
    }
    best
}

fn signed_distance_to_markets(p: Vec2, markets: &[Vec<Vec2>]) -> f32 {
    let mut best_out = f32::INFINITY;
    let mut best_in = 0.0_f32;
    let mut inside = false;
    for poly in markets {
        let d = signed_distance_to_polygon(p, poly);
        if d < 0.0 {
            inside = true;
            best_in = best_in.min(d);
        } else {
            best_out = best_out.min(d);
        }
    }
    if inside {
        best_in
    } else {
        best_out
    }
}

fn sample_wall_share_candidates(
    config: &HamletLabConfig,
    plan: &Plan2D,
    occ: &Occupancy,
    houses: &[PlacedBuilding],
    rng: &mut ChaCha8Rng,
    half_x: f32,
    half_z: f32,
    foundation_m: f32,
    max_center_r: f32,
    candidate_tries: usize,
    plot: Option<&dyn Plot>,
) -> Vec<Candidate> {
    let mut scored = Vec::new();
    if houses.is_empty() {
        return scored;
    }
    let tries = candidate_tries.max(1);
    for _ in 0..tries {
        let h = &houses[rng.gen_range(0..houses.len())];
        let side = rng.gen_range(0..=2);
        let Some((center, yaw)) = wall_share_pose(h, half_x, half_z, side, rng) else {
            continue;
        };
        if center.distance(plan.plaza) + half_z > max_center_r {
            continue;
        }
        if !occ.fits_obb(center, half_x, half_z, yaw, 0.0) {
            continue;
        }
        if houses_obb_overlap(houses, center, half_x, half_z, yaw) {
            continue;
        }
        let facing = Vec2::new(yaw.sin(), yaw.cos());
        let door = center - facing * half_z;
        if signed_distance_to_markets(door, &plan.markets) < 0.4 {
            continue;
        }
        if signed_distance_to_markets(center, &plan.markets) < half_z {
            continue;
        }
        let Some(score) = scored_plot(
            config,
            &plan.markets,
            door,
            center,
            half_x,
            half_z,
            yaw,
            foundation_m,
            plot,
            rng,
            config.wall_share_boost,
        ) else {
            continue;
        };
        scored.push(Candidate {
            score,
            center,
            yaw,
            half_x,
            half_z,
        });
    }
    scored
}

fn wall_share_pose(
    h: &PlacedBuilding,
    half_x: f32,
    half_z: f32,
    side: i32,
    rng: &mut ChaCha8Rng,
) -> Option<(Vec2, f32)> {
    let yaw_h = h.yaw;
    let x_axis = axis(yaw_h, true);
    let z_axis = axis(yaw_h, false);
    let hx = h.half_x;
    let hz = h.half_z;
    let (yaw, mut center, tangent, half_t_h, half_t_n) = match side {
        0 => (yaw_h, h.center + x_axis * (hx + half_x), z_axis, hz, half_z),
        1 => (yaw_h, h.center - x_axis * (hx + half_x), z_axis, hz, half_z),
        2 => (
            yaw_h + std::f32::consts::PI,
            h.center + z_axis * (hz + half_z),
            x_axis,
            hx,
            half_x,
        ),
        _ => return None,
    };
    let max_slide = half_t_h + half_t_n - 0.2;
    if max_slide <= 0.0 {
        return None;
    }
    center += tangent * rng.gen_range(-max_slide..=max_slide);
    Some((center, yaw))
}

fn sample_settler_candidates(
    config: &HamletLabConfig,
    plan: &Plan2D,
    occ: &Occupancy,
    houses: &[PlacedBuilding],
    rng: &mut ChaCha8Rng,
    half_x: f32,
    half_z: f32,
    yaw_offset: f32,
    foundation_m: f32,
    max_center_r: f32,
    candidate_tries: usize,
    plot: Option<&dyn Plot>,
) -> Vec<Candidate> {
    let mut scored = Vec::new();
    let tries = candidate_tries.max(1);
    for _ in 0..tries {
        let attr_i = pick_attractor_index(plan, rng);
        let attr_center = plan.market_centers[attr_i];
        let attr_poly = &plan.markets[attr_i];

        let ang = rng.gen::<f32>() * std::f32::consts::TAU;
        let dir = Vec2::new(ang.cos(), ang.sin());
        let Some((_point, _out, rim_dist)) = ray_hit_polygon_rim(attr_center, dir, attr_poly)
        else {
            continue;
        };
        let front_gap = config.market_front_gap + rng.gen_range(0.0..=1.2);
        let min_r = rim_dist + front_gap + half_z;
        let max_r = max_ray_in_disk(
            attr_center,
            dir,
            plan.plaza,
            (max_center_r - half_z).max(0.0),
        );
        if max_r < min_r {
            continue;
        }
        let center_dist = min_r + (max_r - min_r) * rng.gen::<f32>().powf(2.4);
        let center = attr_center + dir * center_dist;
        if center.distance(plan.plaza) + half_z > max_center_r {
            continue;
        }

        let nearest = closest_point_on_markets(center, &plan.markets);
        let mut away = center - nearest;
        if away.length_squared() < 1e-8 {
            away = dir;
        }
        let outward = away.normalize();
        let mut yaw = outward.x.atan2(outward.y) + yaw_offset;
        yaw += rng.gen_range(-0.18..=0.18);
        let facing = Vec2::new(yaw.sin(), yaw.cos());

        if !occ.fits_obb(center, half_x, half_z, yaw, config.alley * 0.5) {
            continue;
        }
        if houses_obb_overlap(houses, center, half_x, half_z, yaw) {
            continue;
        }
        let door = center - facing * half_z;
        if signed_distance_to_markets(door, &plan.markets) < 0.4 {
            continue;
        }
        if signed_distance_to_markets(center, &plan.markets) < half_z {
            continue;
        }

        let Some(score) = scored_plot(
            config,
            &plan.markets,
            door,
            center,
            half_x,
            half_z,
            yaw,
            foundation_m,
            plot,
            rng,
            0.0,
        ) else {
            continue;
        };
        scored.push(Candidate {
            score,
            center,
            yaw,
            half_x,
            half_z,
        });
    }
    scored
}

fn fitness(
    config: &HamletLabConfig,
    markets: &[Vec<Vec2>],
    door: Vec2,
    rng: &mut ChaCha8Rng,
) -> f32 {
    let rim_dist = signed_distance_to_markets(door, markets).max(0.0);
    let market_score = (-rim_dist / (config.market_front_gap * 1.1).max(1.8)).exp();
    let noise = rng.gen_range(-config.fitness_noise..=config.fitness_noise);
    config.weight_market * market_score + noise
}

fn scored_plot(
    config: &HamletLabConfig,
    markets: &[Vec<Vec2>],
    door: Vec2,
    center: Vec2,
    half_x: f32,
    half_z: f32,
    yaw: f32,
    foundation_m: f32,
    plot: Option<&dyn Plot>,
    rng: &mut ChaCha8Rng,
    extra: f32,
) -> Option<f32> {
    let mut score = fitness(config, markets, door, rng) + extra;
    if let Some(plot) = plot {
        let sample = seat::sample_footprint(plot, center, half_x, half_z, yaw);
        if !seat::accept(&sample, foundation_m) {
            return None;
        }
        score += config.weight_ground * seat::ground_score(&sample);
    }
    Some(score)
}

fn scored_castle(
    config: &HamletLabConfig,
    markets: &[Vec<Vec2>],
    gate: Vec2,
    center: Vec2,
    yaw: f32,
    layout: castle::CastleLayout,
    foundation_m: f32,
    plot: Option<&dyn Plot>,
    rng: &mut ChaCha8Rng,
) -> Option<f32> {
    let mut score = fitness(config, markets, gate, rng);
    if let Some(plot) = plot {
        let sample = seat::sample_castle_footprint(plot, center, yaw, layout);
        if !seat::accept(&sample, foundation_m) {
            return None;
        }
        score += config.weight_ground * seat::ground_score(&sample);
    }
    Some(score)
}

fn best_scored(scored: &[Candidate]) -> Option<Candidate> {
    scored
        .iter()
        .max_by(|a, b| a.score.total_cmp(&b.score))
        .cloned()
}

fn softmax_pick(scored: &[Candidate], temperature: f32, rng: &mut ChaCha8Rng) -> Candidate {
    let temp = temperature.max(0.05);
    let max_s = scored
        .iter()
        .map(|c| c.score)
        .fold(f32::NEG_INFINITY, f32::max);
    let weights: Vec<f32> = scored
        .iter()
        .map(|c| ((c.score - max_s) / temp).exp())
        .collect();
    let total: f32 = weights.iter().sum();
    let roll = rng.gen::<f32>() * total;
    let mut acc = 0.0;
    for (i, w) in weights.iter().enumerate() {
        acc += *w;
        if roll <= acc {
            return scored[i].clone();
        }
    }
    scored[scored.len() - 1].clone()
}

fn houses_obb_overlap(
    houses: &[PlacedBuilding],
    center: Vec2,
    half_x: f32,
    half_z: f32,
    yaw: f32,
) -> bool {
    houses.iter().any(|h| {
        obb_overlap(
            center, half_x, half_z, yaw, h.center, h.half_x, h.half_z, h.yaw,
        )
    })
}

fn obb_overlap(
    a_c: Vec2,
    a_hx: f32,
    a_hz: f32,
    a_yaw: f32,
    b_c: Vec2,
    b_hx: f32,
    b_hz: f32,
    b_yaw: f32,
) -> bool {
    let axes = [
        axis(a_yaw, true),
        axis(a_yaw, false),
        axis(b_yaw, true),
        axis(b_yaw, false),
    ];
    let delta = b_c - a_c;
    const TOUCH_EPS: f32 = 0.05;
    for axis_v in axes {
        let ra = a_hx * axis_v.dot(axis(a_yaw, true)).abs()
            + a_hz * axis_v.dot(axis(a_yaw, false)).abs();
        let rb = b_hx * axis_v.dot(axis(b_yaw, true)).abs()
            + b_hz * axis_v.dot(axis(b_yaw, false)).abs();
        if delta.dot(axis_v).abs() >= ra + rb - TOUCH_EPS {
            return false;
        }
    }
    true
}

fn axis(yaw: f32, along_x: bool) -> Vec2 {
    let c = yaw.cos();
    let s = yaw.sin();
    if along_x {
        Vec2::new(c, -s).normalize()
    } else {
        Vec2::new(s, c).normalize()
    }
}
