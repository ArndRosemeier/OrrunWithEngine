//! Door-sill seating: the floor is the ground at the door, not the lowest corner.
//!
//! Godot sat every house on its lowest footprint sample so downhill corners
//! never floated. That buried the uphill door. Flattening the heightfield to
//! make a pad was the other trap — it fought the continent and was easy to get
//! wrong at the pad edge.
//!
//! The continent stays as it is. The door sits at grade. Downhill air under the
//! floor is a foundation skirt. Uphill, the plinth may bite into the bank; past
//! that the plot is refused and the planner tries another candidate.

use glam::{Vec2, Vec3};

use super::castle::CastleLayout;

/// Sample inset vs catalog size so eaves do not veto a plot the walls would sit on.
pub const SEAT_INSET: f32 = 0.82;
/// Dry margin a door keeps from standing water, in metres of the signed field.
pub const WATERLINE_MARGIN: f32 = 0.35;
/// ~28° — natural continental slopes; we do not terrace towns.
pub const MIN_UPNESS: f32 = 0.88;
/// How far the skirt is driven into the downhill ground so it does not z-fight.
pub const SKIRT_BITE_M: f32 = 0.15;
/// Door threshold slightly into grade so it is not a lip.
pub const DOOR_SINK_M: f32 = 0.04;
/// How far past the plinth the back wall may sit in the bank.
pub const BANK_EXTRA_M: f32 = 0.35;
/// Tallest foundation skirt we will draw. Steeper than this is a cliff, not a plot.
pub const MAX_SKIRT_M: f32 = 3.6;

/// Height and wetness in the planner's local XZ (plaza at the origin).
pub trait Plot {
    fn height(&self, p: Vec2) -> f32;
    /// Signed wetness; `>= 0` is standing water.
    fn wetness(&self, p: Vec2) -> f32;
}

/// Ground under one oriented footprint, sampled at the door and the inset corners.
#[derive(Clone, Copy, Debug)]
pub struct FootprintSample {
    pub door: Vec2,
    pub door_z: f32,
    pub min_z: f32,
    pub max_z: f32,
    pub upness: f32,
    pub wettest: f32,
}

/// Where a building sits once the door is at grade.
#[derive(Clone, Copy, Debug)]
pub struct Seat {
    /// Door sill height — the floor plane.
    pub floor_z: f32,
    /// Mesh origin (plinth base) in the same vertical frame as [`Self::floor_z`].
    pub origin_y: f32,
    /// How far below the floor the skirt must reach, including the bite into grade.
    pub skirt_height: f32,
}

/// Door on the front wall: plan `facing = (sin yaw, cos yaw)`, door toward −facing.
pub fn door_point(center: Vec2, half_z: f32, yaw: f32) -> Vec2 {
    let facing = Vec2::new(yaw.sin(), yaw.cos());
    center - facing * half_z
}

/// Oriented inset corners of the body, matching the planner's yaw convention.
pub fn inset_corners(center: Vec2, half_x: f32, half_z: f32, yaw: f32) -> [Vec2; 4] {
    let axis_x = Vec2::new(yaw.cos(), -yaw.sin());
    let axis_z = Vec2::new(yaw.sin(), yaw.cos());
    let hx = half_x * SEAT_INSET;
    let hz = half_z * SEAT_INSET;
    [
        center + axis_x * hx + axis_z * hz,
        center + axis_x * hx - axis_z * hz,
        center - axis_x * hx + axis_z * hz,
        center - axis_x * hx - axis_z * hz,
    ]
}

fn upness_at(plot: &dyn Plot, p: Vec2) -> f32 {
    const D: f32 = 1.0;
    let dx = plot.height(p + Vec2::X * D) - plot.height(p - Vec2::X * D);
    let dz = plot.height(p + Vec2::Y * D) - plot.height(p - Vec2::Y * D);
    Vec3::new(-dx / (2.0 * D), 1.0, -dz / (2.0 * D))
        .normalize_or_zero()
        .y
}

/// Probe the door, the body centre, and the inset corners.
pub fn sample_footprint(
    plot: &dyn Plot,
    center: Vec2,
    half_x: f32,
    half_z: f32,
    yaw: f32,
) -> FootprintSample {
    let door = door_point(center, half_z, yaw);
    let mut points = vec![door, center];
    points.extend(inset_corners(center, half_x, half_z, yaw));

    let mut min_z = f32::INFINITY;
    let mut max_z = f32::NEG_INFINITY;
    let mut wettest = f32::NEG_INFINITY;
    for p in &points {
        let z = plot.height(*p);
        min_z = min_z.min(z);
        max_z = max_z.max(z);
        wettest = wettest.max(plot.wetness(*p));
    }
    FootprintSample {
        door,
        door_z: plot.height(door),
        min_z,
        max_z,
        upness: upness_at(plot, door),
        wettest,
    }
}

/// Wall-ring and keep samples. The bailey courtyard is not a house floor.
pub fn sample_castle_footprint(
    plot: &dyn Plot,
    center: Vec2,
    yaw: f32,
    layout: CastleLayout,
) -> FootprintSample {
    let half_x = layout.size_x() * 0.5;
    let half_z = layout.size_z() * 0.5;
    let door = door_point(center, half_z, yaw);
    let mut points = vec![door];
    let wall_mid_x = (half_x - layout.wall_m * 0.5).max(0.0);
    let wall_mid_z = (half_z - layout.wall_m * 0.5).max(0.0);
    points.extend(rect_samples(center, wall_mid_x, wall_mid_z, yaw));
    let keep_c = layout.keep_center(center, yaw);
    let keep_mid_x = (layout.keep_half_x - layout.wall_m * 0.5).max(0.0);
    let keep_mid_z = (layout.keep_half_z - layout.wall_m * 0.5).max(0.0);
    points.extend(rect_samples(keep_c, keep_mid_x, keep_mid_z, yaw));

    let mut min_z = f32::INFINITY;
    let mut max_z = f32::NEG_INFINITY;
    let mut wettest = f32::NEG_INFINITY;
    for p in &points {
        let z = plot.height(*p);
        min_z = min_z.min(z);
        max_z = max_z.max(z);
        wettest = wettest.max(plot.wetness(*p));
    }
    FootprintSample {
        door,
        door_z: plot.height(door),
        min_z,
        max_z,
        upness: upness_at(plot, door),
        wettest,
    }
}

fn rect_samples(center: Vec2, half_x: f32, half_z: f32, yaw: f32) -> [Vec2; 8] {
    let axis_x = Vec2::new(yaw.cos(), -yaw.sin());
    let axis_z = Vec2::new(yaw.sin(), yaw.cos());
    [
        center + axis_x * half_x + axis_z * half_z,
        center + axis_x * half_x - axis_z * half_z,
        center - axis_x * half_x + axis_z * half_z,
        center - axis_x * half_x - axis_z * half_z,
        center + axis_x * half_x,
        center - axis_x * half_x,
        center + axis_z * half_z,
        center - axis_z * half_z,
    ]
}

/// Whether this footprint can take a door-sill seat on `foundation_m` of plinth.
pub fn accept(sample: &FootprintSample, foundation_m: f32) -> bool {
    if sample.wettest >= -WATERLINE_MARGIN {
        return false;
    }
    if sample.upness < MIN_UPNESS {
        return false;
    }
    let uphill = sample.max_z - sample.door_z;
    if uphill > foundation_m + BANK_EXTRA_M {
        return false;
    }
    let downhill = sample.door_z - sample.min_z;
    if downhill > MAX_SKIRT_M {
        return false;
    }
    true
}

/// Flatter plots score closer to 1. Used by the planner when a [`Plot`] is present.
pub fn ground_score(sample: &FootprintSample) -> f32 {
    let relief = (sample.max_z - sample.min_z).max(0.0);
    (-relief / 1.6).exp()
}

/// Door at grade, skirt into the downhill air. `None` if the plot is refused.
pub fn seat_building(sample: &FootprintSample, foundation_m: f32) -> Option<Seat> {
    if !accept(sample, foundation_m) {
        return None;
    }
    let floor_z = sample.door_z;
    Some(Seat {
        floor_z,
        origin_y: floor_z - foundation_m - DOOR_SINK_M,
        skirt_height: (floor_z - sample.min_z + SKIRT_BITE_M).max(SKIRT_BITE_M),
    })
}
