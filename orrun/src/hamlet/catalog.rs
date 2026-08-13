//! Village footprints. Dwellings are Modular medieval kit recipes; civics keep lab sizes.

/// Storey/plinth cell height from `catalogs/medieval.json`.
const KIT_STOREY_M: f32 = 2.7;
/// 3×2 ring at 4 m pitch: door on the long (−Z) wall.
const COTTAGE_SIZE_X: f32 = 12.0;
const COTTAGE_SIZE_Z: f32 = 8.0;
/// 3×4 ring at 4 m pitch: same door wall, two extra cells of depth.
const HALL_SIZE_X: f32 = 12.0;
const HALL_SIZE_Z: f32 = 16.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildingRole {
    Dwelling,
    Civic,
    Castle,
}

#[derive(Clone, Copy, Debug)]
pub struct BuildingSpec {
    pub id: &'static str,
    pub role: BuildingRole,
    pub min_tier: u8,
    pub size_x: f32,
    pub size_z: f32,
    pub yaw_offset: f32,
    /// Authored plinth height. Door-sill seating may bury this much of the back wall.
    pub foundation_m: f32,
}

impl BuildingSpec {
    pub fn half_x(self) -> f32 {
        self.size_x * 0.5
    }

    pub fn half_z(self) -> f32 {
        self.size_z * 0.5
    }

    pub fn is_dwelling(self) -> bool {
        self.role == BuildingRole::Dwelling
    }

    pub fn is_civic(self) -> bool {
        self.role == BuildingRole::Civic
    }

    pub fn is_castle(self) -> bool {
        self.role == BuildingRole::Castle
    }
}

const SPECS: &[BuildingSpec] = &[
    BuildingSpec {
        id: "house_hut_thatch",
        role: BuildingRole::Dwelling,
        min_tier: 0,
        size_x: COTTAGE_SIZE_X,
        size_z: COTTAGE_SIZE_Z,
        yaw_offset: 0.0,
        foundation_m: KIT_STOREY_M,
    },
    BuildingSpec {
        id: "house_cabin_timber",
        role: BuildingRole::Dwelling,
        min_tier: 0,
        size_x: COTTAGE_SIZE_X,
        size_z: COTTAGE_SIZE_Z,
        yaw_offset: 0.0,
        foundation_m: KIT_STOREY_M,
    },
    BuildingSpec {
        id: "house_cottage_stone",
        role: BuildingRole::Dwelling,
        min_tier: 0,
        size_x: COTTAGE_SIZE_X,
        size_z: COTTAGE_SIZE_Z,
        yaw_offset: 0.0,
        foundation_m: KIT_STOREY_M,
    },
    BuildingSpec {
        id: "house_hall_large",
        role: BuildingRole::Dwelling,
        min_tier: 1,
        size_x: HALL_SIZE_X,
        size_z: HALL_SIZE_Z,
        yaw_offset: 0.0,
        foundation_m: KIT_STOREY_M,
    },
    BuildingSpec {
        id: "Well",
        role: BuildingRole::Civic,
        min_tier: 0,
        size_x: 2.2,
        size_z: 2.2,
        yaw_offset: 0.0,
        foundation_m: 0.4,
    },
    BuildingSpec {
        id: "Inn",
        role: BuildingRole::Civic,
        min_tier: 2,
        size_x: 17.26,
        size_z: 17.22,
        yaw_offset: 0.0,
        foundation_m: 0.8,
    },
    BuildingSpec {
        id: "Blacksmith",
        role: BuildingRole::Civic,
        min_tier: 2,
        size_x: 16.66,
        size_z: 14.06,
        yaw_offset: 0.0,
        foundation_m: 0.8,
    },
    BuildingSpec {
        id: "Mill",
        role: BuildingRole::Civic,
        min_tier: 2,
        size_x: 14.55,
        size_z: 11.21,
        yaw_offset: 0.0,
        foundation_m: 0.8,
    },
    BuildingSpec {
        id: "Sawmill",
        role: BuildingRole::Civic,
        min_tier: 2,
        size_x: 18.84,
        size_z: 14.06,
        yaw_offset: 0.0,
        foundation_m: 0.8,
    },
    BuildingSpec {
        id: "Stable",
        role: BuildingRole::Civic,
        min_tier: 2,
        size_x: 20.13,
        size_z: 14.25,
        yaw_offset: 0.0,
        foundation_m: 0.8,
    },
    BuildingSpec {
        id: "Bell_Tower",
        role: BuildingRole::Civic,
        min_tier: 3,
        size_x: 8.3,
        size_z: 9.54,
        yaw_offset: 0.0,
        foundation_m: 0.8,
    },
    BuildingSpec {
        id: "Gazebo",
        role: BuildingRole::Civic,
        min_tier: 2,
        size_x: 4.44,
        size_z: 5.39,
        yaw_offset: 0.0,
        foundation_m: 0.5,
    },
    BuildingSpec {
        id: "castle_keep_8x6",
        role: BuildingRole::Castle,
        min_tier: 1,
        size_x: 32.0,
        size_z: 24.0,
        yaw_offset: 0.0,
        foundation_m: KIT_STOREY_M,
    },
    BuildingSpec {
        id: "castle_keep_12x10",
        role: BuildingRole::Castle,
        min_tier: 2,
        size_x: 48.0,
        size_z: 40.0,
        yaw_offset: 0.0,
        foundation_m: KIT_STOREY_M,
    },
    BuildingSpec {
        id: "castle_keep_16x14",
        role: BuildingRole::Castle,
        min_tier: 3,
        size_x: 64.0,
        size_z: 56.0,
        yaw_offset: 0.0,
        foundation_m: KIT_STOREY_M,
    },
];

pub fn spec_for(id: &str) -> Option<&'static BuildingSpec> {
    SPECS.iter().find(|s| s.id == id)
}

pub fn ids_with_role(role: BuildingRole, max_tier: u8) -> Vec<&'static str> {
    SPECS
        .iter()
        .filter(|s| s.role == role && s.min_tier <= max_tier)
        .map(|s| s.id)
        .collect()
}
