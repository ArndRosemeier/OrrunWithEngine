//! Oriented building footprints in world metres.
//!
//! The continent is not terraced. Doors and gates stay at grade. A house caps
//! the whole room; a castle caps only the keep interior so the bailey stays
//! ground. Scatter skips walls and interiors so grass does not grow on the boards.
//!
//! Lattice samples are only lowered *inside* those interiors. Quads that straddle
//! a wall are split in the land mesh so the hillside cannot interpolate through
//! the room, and the yard outside is not excavated.

use std::collections::HashMap;

use engine::collision::{ColliderShape, StaticCollider};
use engine::space::{ChunkCoord, ChunkSpan, GlobalXZ};
use glam::Vec2;

/// How far past the walls scatter still keeps off.
pub const PROP_CLEAR_M: f32 = 0.85;
/// How far past the walls grass, trees, and bushes still stay out of the street.
pub const URBAN_PAD_M: f32 = 8.0;
/// Spatial hash cell for plot queries. Small enough that a sample hits a handful of houses.
const INDEX_CELL_M: f64 = 32.0;
/// Half-width of the door opening that stays at grade.
pub const DOOR_HALF_M: f32 = 1.1;
/// How far into the room from the door wall the sill strip reaches.
pub const DOOR_DEPTH_M: f32 = 0.9;
/// Metres the interior ground sits below the kit floor, so the two do not z-fight.
pub const FLOOR_CLEAR_M: f32 = 0.18;
/// Timber/stone wall thickness for dwelling colliders. Castle curtains use `wall_m`.
const HOUSE_WALL_M: f32 = 0.45;
/// Gate opening on a castle curtain (local −Z). Wider than a house door.
const GATE_HALF_M: f32 = 1.8;

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

    /// Streets and yards next to the house: skip wilderness grass and trees.
    pub fn urban_cover(self, p: GlobalXZ) -> bool {
        self.contains(p, URBAN_PAD_M)
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

    /// Wall boxes with a door on local −Z. The room itself is empty.
    pub fn colliders(self) -> Vec<StaticCollider> {
        let mut out = Vec::new();
        push_wall_ring(
            &mut out,
            self.at,
            self.yaw,
            self.half_x,
            self.half_z,
            HOUSE_WALL_M,
            DOOR_HALF_M,
        );
        out
    }
}

/// Keep-and-curtain: cap the keep interior, leave the bailey as ground.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CastlePlot {
    pub at: GlobalXZ,
    pub half_x: f32,
    pub half_z: f32,
    pub yaw: f32,
    pub floor_y: f32,
    pub wall_m: f32,
    pub keep_offset: Vec2,
    pub keep_half_x: f32,
    pub keep_half_z: f32,
}

impl CastlePlot {
    fn local_xz(self, p: GlobalXZ) -> (f32, f32) {
        let dx = (p.x - self.at.x) as f32;
        let dz = (p.z - self.at.z) as f32;
        let (sin, cos) = (self.yaw.sin(), self.yaw.cos());
        (dx * cos - dz * sin, dx * sin + dz * cos)
    }

    fn in_rect(lx: f32, lz: f32, half_x: f32, half_z: f32, pad: f32) -> bool {
        lx.abs() <= half_x + pad && lz.abs() <= half_z + pad
    }

    fn in_keep(self, lx: f32, lz: f32, pad: f32) -> bool {
        Self::in_rect(
            lx - self.keep_offset.x,
            lz - self.keep_offset.y,
            self.keep_half_x,
            self.keep_half_z,
            pad,
        )
    }

    fn in_curtain(self, lx: f32, lz: f32, pad: f32) -> bool {
        if !Self::in_rect(lx, lz, self.half_x, self.half_z, pad) {
            return false;
        }
        let yard_x = self.half_x - self.wall_m;
        let yard_z = self.half_z - self.wall_m;
        yard_x <= 0.0 || yard_z <= 0.0 || !Self::in_rect(lx, lz, yard_x, yard_z, 0.0)
    }

    pub fn blocks_prop(self, p: GlobalXZ) -> bool {
        let (lx, lz) = self.local_xz(p);
        self.in_curtain(lx, lz, PROP_CLEAR_M) || self.in_keep(lx, lz, PROP_CLEAR_M)
    }

    /// Curtain ring, not the bailey: the courtyard stays a yard.
    pub fn urban_cover(self, p: GlobalXZ) -> bool {
        let (lx, lz) = self.local_xz(p);
        self.in_curtain(lx, lz, URBAN_PAD_M)
    }

    pub fn terrain_cap(self, p: GlobalXZ) -> Option<f32> {
        let (lx, lz) = self.local_xz(p);
        let kx = lx - self.keep_offset.x;
        let kz = lz - self.keep_offset.y;
        let inner_x = self.keep_half_x - self.wall_m;
        let inner_z = self.keep_half_z - self.wall_m;
        if inner_x <= 0.0 || inner_z <= 0.0 {
            return None;
        }
        if !Self::in_rect(kx, kz, inner_x, inner_z, 0.0) {
            return None;
        }
        if kx.abs() <= DOOR_HALF_M && kz <= -inner_z + DOOR_DEPTH_M {
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

    /// Curtain ring with a gate on local −Z, plus keep walls with a door.
    pub fn colliders(self) -> Vec<StaticCollider> {
        let mut out = Vec::new();
        push_wall_ring(
            &mut out,
            self.at,
            self.yaw,
            self.half_x,
            self.half_z,
            self.wall_m,
            GATE_HALF_M,
        );
        let keep_at = local_to_world(self.at, self.yaw, self.keep_offset.x, self.keep_offset.y);
        let keep_wall = self.wall_m.min(self.keep_half_x).min(self.keep_half_z);
        push_wall_ring(
            &mut out,
            keep_at,
            self.yaw,
            self.keep_half_x,
            self.keep_half_z,
            keep_wall,
            DOOR_HALF_M,
        );
        out
    }
}

/// House room or castle ring/keep. Scatter and terrain query this, not a filled castle OBB.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BuildingPlot {
    House(HousePlot),
    Castle(CastlePlot),
}

impl BuildingPlot {
    pub fn blocks_prop(self, p: GlobalXZ) -> bool {
        match self {
            Self::House(h) => h.blocks_prop(p),
            Self::Castle(c) => c.blocks_prop(p),
        }
    }

    pub fn urban_cover(self, p: GlobalXZ) -> bool {
        match self {
            Self::House(h) => h.urban_cover(p),
            Self::Castle(c) => c.urban_cover(p),
        }
    }

    pub fn terrain_cap(self, p: GlobalXZ) -> Option<f32> {
        match self {
            Self::House(h) => h.terrain_cap(p),
            Self::Castle(c) => c.terrain_cap(p),
        }
    }

    fn aabb(self) -> (f64, f64, f64, f64) {
        match self {
            Self::House(h) => h.aabb(),
            Self::Castle(c) => c.aabb(),
        }
    }

    fn colliders(self) -> Vec<StaticCollider> {
        match self {
            Self::House(h) => h.colliders(),
            Self::Castle(c) => c.colliders(),
        }
    }
}

fn index_cell(m: f64) -> i32 {
    (m / INDEX_CELL_M).floor() as i32
}

/// Plots hashed by 32 m cells so terrain, scatter, and fauna do not scan a city.
#[derive(Clone, Debug, Default)]
pub struct BuildingIndex {
    plots: Vec<BuildingPlot>,
    cells: HashMap<(i32, i32), Vec<usize>>,
}

impl PartialEq for BuildingIndex {
    fn eq(&self, other: &Self) -> bool {
        self.plots == other.plots
    }
}

impl BuildingIndex {
    pub fn new(plots: Vec<BuildingPlot>) -> Self {
        let mut cells: HashMap<(i32, i32), Vec<usize>> = HashMap::new();
        for (i, plot) in plots.iter().enumerate() {
            let (min_x, min_z, max_x, max_z) = plot.aabb();
            let x0 = index_cell(min_x);
            let x1 = index_cell(max_x);
            let z0 = index_cell(min_z);
            let z1 = index_cell(max_z);
            for z in z0..=z1 {
                for x in x0..=x1 {
                    cells.entry((x, z)).or_default().push(i);
                }
            }
        }
        Self { plots, cells }
    }

    pub fn plots(&self) -> &[BuildingPlot] {
        &self.plots
    }

    pub fn is_empty(&self) -> bool {
        self.plots.is_empty()
    }

    fn any_nearby(
        &self,
        p: GlobalXZ,
        pad_m: f64,
        mut pred: impl FnMut(BuildingPlot) -> bool,
    ) -> bool {
        let x0 = index_cell(p.x - pad_m);
        let x1 = index_cell(p.x + pad_m);
        let z0 = index_cell(p.z - pad_m);
        let z1 = index_cell(p.z + pad_m);
        for z in z0..=z1 {
            for x in x0..=x1 {
                let Some(ids) = self.cells.get(&(x, z)) else {
                    continue;
                };
                for &i in ids {
                    if pred(self.plots[i]) {
                        return true;
                    }
                }
            }
        }
        false
    }

    fn for_nearby(&self, p: GlobalXZ, pad_m: f64, mut visit: impl FnMut(BuildingPlot)) {
        self.any_nearby(p, pad_m, |plot| {
            visit(plot);
            false
        });
    }

    /// Lowest interior cap under `p`, if any house covers it.
    pub fn terrain_cap(&self, p: GlobalXZ) -> Option<f32> {
        let mut cap: Option<f32> = None;
        self.for_nearby(p, 0.0, |plot| {
            let Some(y) = plot.terrain_cap(p) else {
                return;
            };
            cap = Some(match cap {
                Some(prev) => prev.min(y),
                None => y,
            });
        });
        cap
    }

    pub fn blocks_prop(&self, p: GlobalXZ) -> bool {
        self.any_nearby(p, f64::from(PROP_CLEAR_M), |plot| plot.blocks_prop(p))
    }

    pub fn urban_cover(&self, p: GlobalXZ) -> bool {
        self.any_nearby(p, f64::from(URBAN_PAD_M), |plot| plot.urban_cover(p))
    }

    pub fn overlapping_chunks(&self, span: ChunkSpan) -> Vec<ChunkCoord> {
        overlapping_chunks(&self.plots, span)
    }

    /// Wall and curtain boxes for the engine collision world.
    pub fn colliders(&self) -> Vec<StaticCollider> {
        self.plots
            .iter()
            .flat_map(|plot| plot.colliders())
            .collect()
    }
}

fn local_to_world(at: GlobalXZ, yaw: f32, lx: f32, lz: f32) -> GlobalXZ {
    let (sin, cos) = (yaw.sin(), yaw.cos());
    GlobalXZ::at(
        at.x + f64::from(lx * cos + lz * sin),
        at.z + f64::from(-lx * sin + lz * cos),
    )
}

fn push_box(
    out: &mut Vec<StaticCollider>,
    at: GlobalXZ,
    yaw: f32,
    lx: f32,
    lz: f32,
    half_x: f32,
    half_z: f32,
) {
    if half_x < 0.05 || half_z < 0.05 {
        return;
    }
    out.push(StaticCollider {
        at: local_to_world(at, yaw, lx, lz),
        yaw,
        shape: ColliderShape::Box { half_x, half_z },
    });
}

/// Hollow rectangle: four walls, optional opening on local −Z.
fn push_wall_ring(
    out: &mut Vec<StaticCollider>,
    at: GlobalXZ,
    yaw: f32,
    half_x: f32,
    half_z: f32,
    thickness: f32,
    opening_half: f32,
) {
    let t = thickness.min(half_x).min(half_z);
    if t <= 0.0 {
        panic!("wall thickness must be > 0, got {thickness}");
    }
    let ht = t * 0.5;
    push_box(out, at, yaw, 0.0, half_z - ht, half_x, ht);
    push_box(out, at, yaw, half_x - ht, 0.0, ht, half_z);
    push_box(out, at, yaw, -(half_x - ht), 0.0, ht, half_z);
    let open = opening_half.min((half_x - t).max(0.0));
    if open <= 0.0 {
        push_box(out, at, yaw, 0.0, -(half_z - ht), half_x, ht);
        return;
    }
    let stub = (half_x - open) * 0.5;
    push_box(
        out,
        at,
        yaw,
        -(half_x + open) * 0.5,
        -(half_z - ht),
        stub,
        ht,
    );
    push_box(
        out,
        at,
        yaw,
        (half_x + open) * 0.5,
        -(half_z - ht),
        stub,
        ht,
    );
}

/// Walked-tier chunks whose samples can fall inside a house.
fn overlapping_chunks(plots: &[BuildingPlot], span: ChunkSpan) -> Vec<ChunkCoord> {
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
    use engine::collision::{ActorBody, CollisionWorld};

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

    fn village_castle() -> CastlePlot {
        CastlePlot {
            at: GlobalXZ::at(100.0, 200.0),
            half_x: 16.0,
            half_z: 12.0,
            yaw: 0.0,
            floor_y: 10.0,
            wall_m: 4.0,
            keep_offset: Vec2::new(-4.0, 0.0),
            keep_half_x: 8.0,
            keep_half_z: 8.0,
        }
    }

    #[test]
    fn the_bailey_yard_is_not_excavated() {
        let castle = village_castle();
        let yard = GlobalXZ::at(110.0, 200.0);
        assert!(!castle.blocks_prop(yard));
        assert!(castle.terrain_cap(yard).is_none());
    }

    #[test]
    fn curtain_and_keep_block_props() {
        let castle = village_castle();
        let curtain = GlobalXZ::at(100.0, 200.0 - 12.0);
        let keep = GlobalXZ::at(100.0 - 4.0, 200.0);
        assert!(castle.blocks_prop(curtain));
        assert!(castle.terrain_cap(curtain).is_none());
        assert!(castle.blocks_prop(keep));
        assert_eq!(
            castle.terrain_cap(keep),
            Some(castle.floor_y - FLOOR_CLEAR_M)
        );
    }

    #[test]
    fn the_keep_door_sill_is_not_excavated() {
        let castle = village_castle();
        let sill = GlobalXZ::at(100.0 - 4.0, 200.0 - 4.0);
        assert!(castle.blocks_prop(sill));
        assert!(castle.terrain_cap(sill).is_none());
    }

    #[test]
    fn the_index_finds_a_house_without_scanning_the_rest() {
        let house = plot();
        let mut plots = vec![BuildingPlot::House(house)];
        for i in 0..200 {
            plots.push(BuildingPlot::House(HousePlot {
                at: GlobalXZ::at(5_000.0 + f64::from(i) * 40.0, 5_000.0),
                half_x: 6.0,
                half_z: 4.0,
                yaw: 0.0,
                floor_y: 10.0,
            }));
        }
        let index = BuildingIndex::new(plots);
        assert!(index.blocks_prop(house.at));
        assert!(index.terrain_cap(house.at).is_some());
        assert!(!index.blocks_prop(GlobalXZ::at(130.0, 200.0)));
        assert!(index.urban_cover(GlobalXZ::at(108.0, 200.0)));
        assert!(!index.urban_cover(GlobalXZ::at(130.0, 200.0)));
    }

    #[test]
    fn urban_cover_leaves_the_bailey() {
        let castle = village_castle();
        let index = BuildingIndex::new(vec![BuildingPlot::Castle(castle)]);
        assert!(!index.urban_cover(GlobalXZ::at(110.0, 200.0)));
        assert!(index.urban_cover(GlobalXZ::at(100.0, 200.0 - 12.0)));
    }

    fn collide(plot: BuildingPlot, from: GlobalXZ, dx: f64, dz: f64) -> GlobalXZ {
        let mut world = CollisionWorld::new();
        world
            .replace_layer(1, plot.colliders())
            .expect("building colliders");
        world.move_xz(&ActorBody::player(), from, dx, dz)
    }

    #[test]
    fn a_walker_fits_through_the_door() {
        let house = BuildingPlot::House(plot());
        let from = GlobalXZ::at(100.0, 200.0 - 5.0);
        let to = collide(house, from, 0.0, 4.0);
        assert!(
            to.z > 198.0,
            "door should admit the player into the room, got z={}",
            to.z
        );
    }

    #[test]
    fn a_walker_does_not_pass_the_wall() {
        let house = BuildingPlot::House(plot());
        let from = GlobalXZ::at(103.0, 200.0 - 5.0);
        let to = collide(house, from, 0.0, 4.0);
        assert!(
            to.z < 196.5,
            "the south wall beside the door should stop the player, got z={}",
            to.z
        );
    }

    #[test]
    fn the_room_is_not_a_solid() {
        let house = BuildingPlot::House(plot());
        let from = GlobalXZ::at(100.0, 200.0);
        let to = collide(house, from, 0.5, 0.0);
        assert!(
            (to.x - 100.5).abs() < 0.05,
            "inside the house should be free, got x={}",
            to.x
        );
    }

    #[test]
    fn castle_gate_lets_a_walker_into_the_bailey() {
        let castle = BuildingPlot::Castle(village_castle());
        let from = GlobalXZ::at(100.0, 200.0 - 14.0);
        let to = collide(castle, from, 0.0, 8.0);
        assert!(
            to.z > 190.0,
            "gate should admit the player into the bailey, got z={}",
            to.z
        );
    }

    #[test]
    fn the_curtain_stops_a_walker() {
        let castle = BuildingPlot::Castle(village_castle());
        let from = GlobalXZ::at(108.0, 200.0 - 14.0);
        let to = collide(castle, from, 0.0, 8.0);
        assert!(
            to.z < 189.0,
            "curtain beside the gate should stop the player, got z={}",
            to.z
        );
    }
}
