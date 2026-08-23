//! Soft target-lock. Tab nearest hostile in 20 m / 90° front cone; cycle; click-body.
//! Does not steal camera, Esc, or E.

use crate::combat::math::{TAB_CONE_DEG, TAB_LOCK_M};

#[derive(Debug, Clone, Copy)]
pub struct Hostile {
    pub id: u64,
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

fn wrap_deg(d: f64) -> f64 {
    let mut a = d % 360.0;
    if a > 180.0 {
        a -= 360.0;
    }
    if a < -180.0 {
        a += 360.0;
    }
    a
}

/// Yaw 0 faces +X, degrees increase toward +Z (CCW on XZ). Matches first-person body yaw.
pub fn yaw_to_target(origin_x: f64, origin_z: f64, tx: f64, tz: f64) -> f64 {
    let dx = tx - origin_x;
    let dz = tz - origin_z;
    dz.atan2(dx).to_degrees()
}

pub fn in_front_cone(
    origin_x: f64,
    origin_z: f64,
    facing_yaw_deg: f64,
    tx: f64,
    tz: f64,
    cone_deg: f64,
) -> bool {
    let to = yaw_to_target(origin_x, origin_z, tx, tz);
    wrap_deg(to - facing_yaw_deg).abs() <= cone_deg * 0.5
}

fn horiz_dist(ox: f64, oz: f64, tx: f64, tz: f64) -> f64 {
    let dx = tx - ox;
    let dz = tz - oz;
    (dx * dx + dz * dz).sqrt()
}

/// Hostiles in the Tab cone, nearest first, then id.
pub fn cone_candidates(
    origin_x: f64,
    origin_z: f64,
    facing_yaw_deg: f64,
    hostiles: &[Hostile],
) -> Vec<u64> {
    let mut rows: Vec<(f64, u64)> = hostiles
        .iter()
        .filter_map(|h| {
            let d = horiz_dist(origin_x, origin_z, h.x, h.z);
            if d > TAB_LOCK_M || d <= 0.0 {
                return None;
            }
            if !in_front_cone(origin_x, origin_z, facing_yaw_deg, h.x, h.z, TAB_CONE_DEG) {
                return None;
            }
            Some((d, h.id))
        })
        .collect();
    rows.sort_by(|a, b| {
        a.0.partial_cmp(&b.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.cmp(&b.1))
    });
    rows.into_iter().map(|(_, id)| id).collect()
}

/// Tab: lock nearest in cone; repeat cycles; cycle off after last.
pub fn tab_cycle(
    origin_x: f64,
    origin_z: f64,
    facing_yaw_deg: f64,
    current: Option<u64>,
    hostiles: &[Hostile],
) -> Option<u64> {
    let ids = cone_candidates(origin_x, origin_z, facing_yaw_deg, hostiles);
    if ids.is_empty() {
        return None;
    }
    match current {
        None => Some(ids[0]),
        Some(cur) => match ids.iter().position(|&id| id == cur) {
            None => Some(ids[0]),
            Some(i) if i + 1 < ids.len() => Some(ids[i + 1]),
            Some(_) => None,
        },
    }
}

/// Click-body lock. No cone test — the body was the pick.
pub fn click_body_lock(id: u64) -> u64 {
    id
}
