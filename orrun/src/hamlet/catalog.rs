//! Minimal village footprints (from Orrun `assets/catalog/village.json`).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildingRole {
    Dwelling,
    Civic,
}

#[derive(Clone, Copy, Debug)]
pub struct BuildingSpec {
    pub id: &'static str,
    pub role: BuildingRole,
    pub min_tier: u8,
    pub size_x: f32,
    pub size_z: f32,
    pub yaw_offset: f32,
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
}

const SPECS: &[BuildingSpec] = &[
    BuildingSpec {
        id: "House_1",
        role: BuildingRole::Dwelling,
        min_tier: 0,
        size_x: 9.18,
        size_z: 11.39,
        yaw_offset: 0.0,
    },
    BuildingSpec {
        id: "House_2",
        role: BuildingRole::Dwelling,
        min_tier: 0,
        size_x: 9.51,
        size_z: 14.65,
        yaw_offset: 0.0,
    },
    BuildingSpec {
        id: "House_3",
        role: BuildingRole::Dwelling,
        min_tier: 0,
        size_x: 8.3,
        size_z: 9.09,
        yaw_offset: 0.0,
    },
    BuildingSpec {
        id: "House_4",
        role: BuildingRole::Dwelling,
        min_tier: 0,
        size_x: 8.3,
        size_z: 9.09,
        yaw_offset: 0.0,
    },
    BuildingSpec {
        id: "Well",
        role: BuildingRole::Civic,
        min_tier: 0,
        size_x: 2.85,
        size_z: 4.3,
        yaw_offset: 0.0,
    },
    BuildingSpec {
        id: "Inn",
        role: BuildingRole::Civic,
        min_tier: 2,
        size_x: 17.26,
        size_z: 17.22,
        yaw_offset: 0.0,
    },
    BuildingSpec {
        id: "Blacksmith",
        role: BuildingRole::Civic,
        min_tier: 2,
        size_x: 16.66,
        size_z: 14.06,
        yaw_offset: 0.0,
    },
    BuildingSpec {
        id: "Mill",
        role: BuildingRole::Civic,
        min_tier: 2,
        size_x: 14.55,
        size_z: 11.21,
        yaw_offset: 0.0,
    },
    BuildingSpec {
        id: "Sawmill",
        role: BuildingRole::Civic,
        min_tier: 2,
        size_x: 18.84,
        size_z: 14.06,
        yaw_offset: 0.0,
    },
    BuildingSpec {
        id: "Stable",
        role: BuildingRole::Civic,
        min_tier: 2,
        size_x: 20.13,
        size_z: 14.25,
        yaw_offset: 0.0,
    },
    BuildingSpec {
        id: "Bell_Tower",
        role: BuildingRole::Civic,
        min_tier: 3,
        size_x: 8.3,
        size_z: 9.54,
        yaw_offset: 0.0,
    },
    BuildingSpec {
        id: "Gazebo",
        role: BuildingRole::Civic,
        min_tier: 2,
        size_x: 4.44,
        size_z: 5.39,
        yaw_offset: 0.0,
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
