//! Castle footprints. Same 4 m pitch as `catalogs/castle.json`.
//!
//! One Modular recipe — keep-and-curtain `enclosed_ward` — grown by adding
//! cells. Hamlet (tier 0) has no castle. Gate on the local −Z wall.

use glam::Vec2;

/// Pitch shared with `catalogs/castle.json` and the medieval house kit.
pub const PITCH_M: f32 = 4.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CastleLayout {
    pub id: &'static str,
    pub cells_x: i32,
    pub cells_z: i32,
    pub wall_m: f32,
    pub keep_half_x: f32,
    pub keep_half_z: f32,
    /// Keep centre relative to the outer AABB centre, local yaw 0 (+X east, +Y north).
    pub keep_offset: Vec2,
    pub keep_cells_x: i32,
    pub keep_cells_z: i32,
    pub keep_origin_x: i32,
    pub keep_origin_z: i32,
    pub bailey_storeys: u32,
    pub keep_storeys: u32,
    pub tower_extra: u32,
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

/// Bailey ring plus inset keep, in Modular cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Ward {
    id: &'static str,
    width: i32,
    depth: i32,
    keep_w: i32,
    keep_d: i32,
    keep_origin: (i32, i32),
    bailey_storeys: u32,
    keep_storeys: u32,
    tower_extra: u32,
}

impl Ward {
    fn layout(self) -> CastleLayout {
        assert!(self.width >= 2 && self.depth >= 2);
        assert!(
            self.keep_origin.0 + self.keep_w <= self.width
                && self.keep_origin.1 + self.keep_d <= self.depth,
            "{} keep does not fit the bailey",
            self.id
        );
        let outer = Vec2::new(self.width as f32 * 0.5, self.depth as f32 * 0.5);
        let keep = Vec2::new(
            self.keep_origin.0 as f32 + self.keep_w as f32 * 0.5,
            self.keep_origin.1 as f32 + self.keep_d as f32 * 0.5,
        );
        CastleLayout {
            id: self.id,
            cells_x: self.width,
            cells_z: self.depth,
            wall_m: PITCH_M,
            keep_half_x: self.keep_w as f32 * PITCH_M * 0.5,
            keep_half_z: self.keep_d as f32 * PITCH_M * 0.5,
            keep_offset: (keep - outer) * PITCH_M,
            keep_cells_x: self.keep_w,
            keep_cells_z: self.keep_d,
            keep_origin_x: self.keep_origin.0,
            keep_origin_z: self.keep_origin.1,
            bailey_storeys: self.bailey_storeys,
            keep_storeys: self.keep_storeys,
            tower_extra: self.tower_extra,
            keep_is_ward: false,
        }
    }
}

/// Village 8×6, town 12×10, port 16×14. Keep grows 4 → 6 → 8.
fn ward_for_tier(tier: u8) -> Option<Ward> {
    match tier {
        1 => Some(Ward {
            id: "castle_keep_8x6",
            width: 8,
            depth: 6,
            keep_w: 4,
            keep_d: 4,
            keep_origin: (1, 1),
            bailey_storeys: 2,
            keep_storeys: 4,
            tower_extra: 0,
        }),
        2 => Some(Ward {
            id: "castle_keep_12x10",
            width: 12,
            depth: 10,
            keep_w: 6,
            keep_d: 6,
            keep_origin: (2, 2),
            bailey_storeys: 3,
            keep_storeys: 6,
            tower_extra: 1,
        }),
        3 => Some(Ward {
            id: "castle_keep_16x14",
            width: 16,
            depth: 14,
            keep_w: 8,
            keep_d: 8,
            keep_origin: (2, 2),
            bailey_storeys: 4,
            keep_storeys: 6,
            tower_extra: 1,
        }),
        _ => None,
    }
}

pub fn layout_for(id: &str) -> Option<CastleLayout> {
    (1u8..=3).find_map(|tier| {
        let ward = ward_for_tier(tier)?;
        (ward.id == id).then(|| ward.layout())
    })
}

/// Keep-and-curtain for village and up. Hamlets have none.
pub fn id_for_tier(tier: u8) -> Option<&'static str> {
    ward_for_tier(tier).map(|w| w.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hamlets_have_no_castle() {
        assert!(id_for_tier(0).is_none());
        assert!(ward_for_tier(0).is_none());
    }

    #[test]
    fn bailey_and_keep_grow_with_tier() {
        let village = ward_for_tier(1).unwrap().layout();
        let town = ward_for_tier(2).unwrap().layout();
        let port = ward_for_tier(3).unwrap().layout();
        assert_eq!(village.size_x(), 32.0);
        assert_eq!(village.size_z(), 24.0);
        assert_eq!(town.size_x(), 48.0);
        assert_eq!(town.size_z(), 40.0);
        assert_eq!(port.size_x(), 64.0);
        assert_eq!(port.size_z(), 56.0);
        assert!(town.size_x() > village.size_x() && town.size_z() > village.size_z());
        assert!(port.size_x() > town.size_x() && port.size_z() > town.size_z());
        assert!(town.keep_half_x > village.keep_half_x);
        assert!(port.keep_half_x > town.keep_half_x);
        assert!(town.bailey_storeys > village.bailey_storeys);
        assert!(port.bailey_storeys > town.bailey_storeys);
    }

    #[test]
    fn catalog_sizes_match_the_layouts() {
        for tier in 1u8..=3 {
            let id = id_for_tier(tier).expect("castle tier");
            let layout = layout_for(id).expect(id);
            let spec = crate::hamlet::spec_for(id).expect(id);
            assert!(spec.is_castle(), "{id}");
            assert!(spec.min_tier <= tier, "{id}");
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
