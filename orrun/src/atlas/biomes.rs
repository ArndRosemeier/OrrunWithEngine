//! Atlas landcover ids.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Biome {
    Ocean = 0,
    Coast = 1,
    Plains = 2,
    Forest = 3,
    Wetland = 4,
    Arid = 5,
    Alpine = 6,
    Tundra = 7,
    Lake = 8,
}

impl Biome {
    pub fn from_id(id: i32) -> Self {
        match id {
            0 => Self::Ocean,
            1 => Self::Coast,
            2 => Self::Plains,
            3 => Self::Forest,
            4 => Self::Wetland,
            5 => Self::Arid,
            6 => Self::Alpine,
            7 => Self::Tundra,
            8 => Self::Lake,
            other => panic!("unknown atlas biome id {other}"),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Ocean => "ocean",
            Self::Coast => "coast",
            Self::Plains => "plains",
            Self::Forest => "forest",
            Self::Wetland => "wetland",
            Self::Arid => "arid",
            Self::Alpine => "alpine",
            Self::Tundra => "tundra",
            Self::Lake => "lake",
        }
    }

    /// Preview colour (sRGB bytes), matching the Godot atlas viewer.
    pub fn color_rgb(self) -> [u8; 3] {
        match self {
            Self::Ocean => [26, 56, 102],
            Self::Coast => [184, 168, 107],
            Self::Plains => [107, 148, 71],
            Self::Forest => [46, 102, 51],
            Self::Wetland => [71, 122, 92],
            Self::Arid => [158, 133, 82],
            Self::Alpine => [140, 143, 133],
            Self::Tundra => [122, 133, 112],
            Self::Lake => [41, 97, 148],
        }
    }
}

#[inline]
pub fn is_water(biome: Biome) -> bool {
    matches!(biome, Biome::Ocean | Biome::Lake)
}

#[inline]
pub fn is_land(biome: Biome) -> bool {
    !is_water(biome)
}
