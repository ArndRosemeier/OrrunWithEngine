//! Oriented house footprints in world metres.
//!
//! The continent is not terraced. The door stays at grade. Inside the walls the
//! drawn ground is capped at the floor so a hillside cannot fill the room, and
//! scatter skips the same disk so grass does not grow on the boards.
//!
//! Lattice samples are only lowered *inside* the walls. Quads that straddle a
//! wall are split in the land mesh so the hillside cannot interpolate through
//! the room, and the yard outside is not excavated.

use engine::space::{ChunkCoord, ChunkSpan, GlobalXZ};

/// How far past the walls scatter still keeps off.
pub const PROP_CLEAR_M: f32 = 0.85;
/// Half-width of the door opening that stays at grade.
pub const DOOR_HALF_M: f32 = 1.1;
/// How far into the room from the door wall the sill strip reaches.
pub const DOOR_DEPTH_M: f32 = 0.9;
/// Metres the interior ground sits below the kit floor, so the two do not z-fight.
pub const FLOOR_CLEAR_M: f32 = 0.18;

/// One seated dwelling, for scatter and terrain to keep out of.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HousePlot {
    pub at: GlobalXZ,
    pub half_x: f32,
    pub half_z: f32,
    pub yaw: f32,
    pub floor_y: f32,
}

impl HousePlot {
    fn local_xz(self, p: GlobalXZ) -> (f32, f32) {
        let dx = (p.x - self.at.x) as f32;
        let dz = (p.z - self.at.z) as f32;
        let (sin, cos) = (self.yaw.sin(), self.yaw.cos());
        (dx * cos - dz * sin, dx * sin + dz * cos)
    }

    fn contains(self, p: GlobalXZ, pad: f32) -> bool {
        let (lx, lz) = self.local_xz(p);
        lx.abs() <= self.half_x + pad && lz.abs() <= self.half_z + pad
    }

    /// True when a prop would stand inside the house or against the walls.
    pub fn blocks_prop(self, p: GlobalXZ) -> bool {
        self.contains(p, PROP_CLEAR_M)
    }

    /// Interior floor height to cap the drawn ground to, if `p` is inside the walls.
    ///
    /// The door on local −Z stays at grade. Samples just outside the walls are
    /// not lowered: the land mesh splits those quads instead of excavating a yard.
    pub fn terrain_cap(self, p: GlobalXZ) -> Option<f32> {
        let (lx, lz) = self.local_xz(p);
        if lx.abs() > self.half_x || lz.abs() > self.half_z {
            return None;
        }
        if lx.abs() <= DOOR_HALF_M && lz <= -self.half_z + DOOR_DEPTH_M {
            return None;
        }
        Some(self.floor_y - FLOOR_CLEAR_M)
    }

    fn aabb(self) -> (f64, f64, f64, f64) {
        let (sin, cos) = (self.yaw.sin().abs(), self.yaw.cos().abs());
        let ex = f64::from(self.half_x * cos + self.half_z * sin);
        let ez = f64::from(self.half_x * sin + self.half_z * cos);
        (
            self.at.x - ex,
            self.at.z - ez,
            self.at.x + ex,
            self.at.z + ez,
        )
    }
}

/// Lowest interior cap under `p`, if any house covers it.
pub fn terrain_cap(plots: &[HousePlot], p: GlobalXZ) -> Option<f32> {
    let mut cap: Option<f32> = None;
    for plot in plots {
        let Some(y) = plot.terrain_cap(p) else {
            continue;
        };
        cap = Some(match cap {
            Some(prev) => prev.min(y),
            None => y,
        });
    }
    cap
}

pub fn blocks_prop(plots: &[HousePlot], p: GlobalXZ) -> bool {
    plots.iter().any(|plot| plot.blocks_prop(p))
}

/// Walked-tier chunks whose samples can fall inside a house.
pub fn overlapping_chunks(plots: &[HousePlot], span: ChunkSpan) -> Vec<ChunkCoord> {
    let mut out = Vec::new();
    for plot in plots {
        let (min_x, min_z, max_x, max_z) = plot.aabb();
        let a = ChunkCoord::containing(GlobalXZ::at(min_x, min_z), span);
        let b = ChunkCoord::containing(GlobalXZ::at(max_x, max_z), span);
        let x0 = a.x.min(b.x);
        let x1 = a.x.max(b.x);
        let z0 = a.z.min(b.z);
        let z1 = a.z.max(b.z);
        for z in z0..=z1 {
            for x in x0..=x1 {
                let coord = ChunkCoord::new(x, z);
                if !out.contains(&coord) {
                    out.push(coord);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plot() -> HousePlot {
        HousePlot {
            at: GlobalXZ::at(100.0, 200.0),
            half_x: 6.0,
            half_z: 4.0,
            yaw: 0.0,
            floor_y: 10.0,
        }
    }

    #[test]
    fn the_room_is_inside_and_the_yard_is_not() {
        let house = plot();
        assert!(house.blocks_prop(house.at));
        assert!(house.terrain_cap(house.at).is_some());
        assert!(!house.blocks_prop(GlobalXZ::at(130.0, 200.0)));
        assert!(house.terrain_cap(GlobalXZ::at(130.0, 200.0)).is_none());
    }

    #[test]
    fn yaw_turns_the_long_wall() {
        let mut house = plot();
        house.yaw = std::f32::consts::FRAC_PI_2;
        // Local +X is world −Z at yaw 90°.
        assert!(house.blocks_prop(GlobalXZ::at(100.0, 200.0 - 5.0)));
        assert!(!house.blocks_prop(GlobalXZ::at(100.0 + 8.0, 200.0)));
    }

    #[test]
    fn the_door_sill_is_not_excavated() {
        let house = plot();
        // Front wall is −Z at yaw 0; the opening stays at grade.
        let sill = GlobalXZ::at(100.0, 200.0 - 4.0);
        assert!(house.blocks_prop(sill));
        assert!(house.terrain_cap(sill).is_none());
    }

    #[test]
    fn the_back_wall_sample_drops_with_the_room() {
        let house = plot();
        let back = GlobalXZ::at(100.0, 200.0 + 4.0);
        assert_eq!(house.terrain_cap(back), Some(house.floor_y - FLOOR_CLEAR_M));
    }

    #[test]
    fn the_yard_outside_the_corner_is_not_a_pit() {
        let house = plot();
        // 4 m lattice: the house corner is (106, 204); (108, 204) is 2 m outside.
        assert!(house.terrain_cap(GlobalXZ::at(108.0, 204.0)).is_none());
        assert!(house.terrain_cap(GlobalXZ::at(116.0, 200.0)).is_none());
    }
}
