//! Castle footprints for the 2D lab. Same 4 m pitch as `catalogs/castle.json`.
//!
//! Recipes match Modular `enclosed_ward` sizes. The keep sits in cell space;
//! the planner uses the outer AABB centre, gate on the local −Z wall.

use glam::Vec2;

/// Pitch shared with `catalogs/castle.json` and the medieval house kit.
pub const PITCH_M: f32 = 4.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CastleLayout {
    pub id: &'static str,
    pub cells_x: i32,
    pub cells_z: i32,
    pub wall_m: f32,
    /// Inner keep / ward half-extents. Zero = the outer ring is the keep.
    pub keep_half_x: f32,
    pub keep_half_z: f32,
    /// Keep centre relative to the outer AABB centre, local yaw 0 (+X east, +Y north).
    pub keep_offset: Vec2,
    /// Concentric inner curtain (hollow) rather than a solid keep.
    pub keep_is_ward: bool,
}

impl CastleLayout {
    pub fn size_x(self) -> f32 {
        self.cells_x as f32 * PITCH_M
    }

    pub fn size_z(self) -> f32 {
        self.cells_z as f32 * PITCH_M
    }

    pub fn keep_center(self, outer_center: Vec2, yaw: f32) -> Vec2 {
        let x_axis = Vec2::new(yaw.cos(), -yaw.sin());
        let z_axis = Vec2::new(yaw.sin(), yaw.cos());
        outer_center + x_axis * self.keep_offset.x + z_axis * self.keep_offset.y
    }
}

fn layout(
    id: &'static str,
    cells_x: i32,
    cells_z: i32,
    keep_cells_x: i32,
    keep_cells_z: i32,
    keep_origin: (i32, i32),
    keep_is_ward: bool,
) -> CastleLayout {
    let outer = Vec2::new(cells_x as f32 * 0.5, cells_z as f32 * 0.5);
    let keep = if keep_cells_x == 0 || keep_cells_z == 0 {
        Vec2::ZERO
    } else {
        Vec2::new(
            keep_origin.0 as f32 + keep_cells_x as f32 * 0.5,
            keep_origin.1 as f32 + keep_cells_z as f32 * 0.5,
        )
    };
    CastleLayout {
        id,
        cells_x,
        cells_z,
        wall_m: PITCH_M,
        keep_half_x: keep_cells_x as f32 * PITCH_M * 0.5,
        keep_half_z: keep_cells_z as f32 * PITCH_M * 0.5,
        keep_offset: (keep - outer) * PITCH_M,
        keep_is_ward,
    }
}

/// 4×4 keep ring — peels / tower houses.
const TOWER_HOUSE: CastleLayout = CastleLayout {
    id: "castle_tower_house",
    cells_x: 4,
    cells_z: 4,
    wall_m: PITCH_M,
    keep_half_x: 0.0,
    keep_half_z: 0.0,
    keep_offset: Vec2::ZERO,
    keep_is_ward: false,
};

fn small_bailey() -> CastleLayout {
    layout("castle_small_bailey", 8, 6, 4, 4, (1, 1), false)
}

fn keep_and_curtain() -> CastleLayout {
    layout("castle_keep_and_curtain", 12, 10, 4, 4, (1, 1), false)
}

fn concentric() -> CastleLayout {
    layout("castle_concentric", 16, 12, 10, 6, (3, 3), true)
}

pub fn layout_for(id: &str) -> Option<CastleLayout> {
    match id {
        "castle_tower_house" => Some(TOWER_HOUSE),
        "castle_small_bailey" => Some(small_bailey()),
        "castle_keep_and_curtain" => Some(keep_and_curtain()),
        "castle_concentric" => Some(concentric()),
        _ => None,
    }
}

/// One castle per settlement, scaled to atlas tier.
pub fn id_for_tier(tier: u8) -> &'static str {
    match tier.min(3) {
        0 => "castle_tower_house",
        1 => "castle_small_bailey",
        2 => "castle_keep_and_curtain",
        _ => "castle_concentric",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keep_and_curtain_matches_modular_cells() {
        let layout = keep_and_curtain();
        assert_eq!(layout.size_x(), 48.0);
        assert_eq!(layout.size_z(), 40.0);
        assert!((layout.keep_offset.x + 12.0).abs() < 1e-4);
        assert!((layout.keep_offset.y + 8.0).abs() < 1e-4);
        assert_eq!(layout.keep_half_x, 8.0);
        assert_eq!(layout.keep_half_z, 8.0);
    }

    #[test]
    fn concentric_inner_ward_is_centred() {
        let layout = concentric();
        assert_eq!(layout.size_x(), 64.0);
        assert_eq!(layout.size_z(), 48.0);
        assert!(layout.keep_offset.length() < 1e-4);
        assert!(layout.keep_is_ward);
    }

    #[test]
    fn catalog_sizes_match_the_layouts() {
        for id in [
            "castle_tower_house",
            "castle_small_bailey",
            "castle_keep_and_curtain",
            "castle_concentric",
        ] {
            let layout = layout_for(id).expect(id);
            let spec = crate::hamlet::spec_for(id).expect(id);
            assert!(spec.is_castle(), "{id}");
            assert!(
                (spec.size_x - layout.size_x()).abs() < 1e-4
                    && (spec.size_z - layout.size_z()).abs() < 1e-4,
                "{id} spec {}×{} vs layout {}×{}",
                spec.size_x,
                spec.size_z,
                layout.size_x(),
                layout.size_z()
            );
        }
    }
}
