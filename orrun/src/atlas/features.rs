//! Continuity enums and edge-key helpers.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Kind {
    River = 0,
    Road = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Dir {
    East = 0,
    South = 1,
    West = 2,
    North = 3,
}

impl Dir {
    pub fn opposite(self) -> Self {
        match self {
            Self::East => Self::West,
            Self::West => Self::East,
            Self::South => Self::North,
            Self::North => Self::South,
        }
    }

    pub fn delta(self) -> (i32, i32) {
        match self {
            Self::East => (1, 0),
            Self::West => (-1, 0),
            Self::South => (0, 1),
            Self::North => (0, -1),
        }
    }

    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::East,
            1 => Self::South,
            2 => Self::West,
            3 => Self::North,
            other => panic!("invalid Dir {other}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum EndpointKind {
    EdgePort = 0,
    Ocean = 1,
    Lake = 2,
    Node = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum NodeKind {
    CoastalGate = 0,
    LakeShore = 1,
    Pass = 2,
    Landmark = 3,
    Settlement = 4,
    ClaimReserved = 5,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum RoadClass {
    Primary = 0,
    Secondary = 1,
    Trail = 2,
}

/// Canonical edge key owned by the west cell (EAST) or north cell (SOUTH).
pub fn edge_key(ax: i32, az: i32, dir: Dir, size: usize) -> i32 {
    let (oax, oaz, odir) = match dir {
        Dir::West => (ax - 1, az, Dir::East),
        Dir::North => (ax, az - 1, Dir::South),
        other => (ax, az, other),
    };
    assert!(
        oax >= 0 && oaz >= 0 && (oax as usize) < size && (oaz as usize) < size,
        "edge owner out of bounds ({oax},{oaz}) size={size}"
    );
    assert!(
        matches!(odir, Dir::East | Dir::South),
        "canonical edge dir must be EAST or SOUTH"
    );
    oax | (oaz << 12) | ((odir as i32) << 24)
}

pub fn edge_owner(key: i32) -> (i32, i32, Dir) {
    let ax = key & 0xFFF;
    let az = (key >> 12) & 0xFFF;
    let dir = Dir::from_u8(((key >> 24) & 0xF) as u8);
    (ax, az, dir)
}

pub fn opposite_dir(dir: Dir) -> Dir {
    dir.opposite()
}
