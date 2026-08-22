//! EQ-soft consideration coloring by level delta.

/// Consideration band for a mob relative to the player.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConBand {
    Grey,
    Green,
    Blue,
    White,
    Yellow,
    Orange,
    Red,
}

impl ConBand {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Grey => "grey",
            Self::Green => "green",
            Self::Blue => "blue",
            Self::White => "white",
            Self::Yellow => "yellow",
            Self::Orange => "orange",
            Self::Red => "red",
        }
    }

    /// Opaque RGB for HUD text / nameplates.
    pub fn rgb(self) -> (u8, u8, u8) {
        match self {
            Self::Grey => (140, 140, 140),
            Self::Green => (64, 180, 64),
            Self::Blue => (80, 140, 220),
            Self::White => (235, 235, 235),
            Self::Yellow => (230, 210, 64),
            Self::Orange => (230, 140, 40),
            Self::Red => (220, 48, 48),
        }
    }
}

/// `delta = mob_level - player_level`.
pub fn con_band(player_level: i32, mob_level: i32) -> ConBand {
    let delta = mob_level - player_level;
    match delta {
        ..=-6 => ConBand::Grey,
        -5..=-3 => ConBand::Green,
        -2..=-1 => ConBand::Blue,
        0 => ConBand::White,
        1..=2 => ConBand::Yellow,
        3..=4 => ConBand::Orange,
        _ => ConBand::Red,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn con_bands_cover_eq_soft_deltas() {
        assert_eq!(con_band(10, 1).as_str(), "grey");
        assert_eq!(con_band(10, 4), ConBand::Grey);
        assert_eq!(con_band(10, 5), ConBand::Green);
        assert_eq!(con_band(10, 7), ConBand::Green);
        assert_eq!(con_band(10, 8), ConBand::Blue);
        assert_eq!(con_band(10, 9), ConBand::Blue);
        assert_eq!(con_band(10, 10), ConBand::White);
        assert_eq!(con_band(10, 11), ConBand::Yellow);
        assert_eq!(con_band(10, 12), ConBand::Yellow);
        assert_eq!(con_band(10, 13), ConBand::Orange);
        assert_eq!(con_band(10, 14), ConBand::Orange);
        assert_eq!(con_band(10, 15), ConBand::Red);
        assert_eq!(con_band(1, 20), ConBand::Red);
    }

    #[test]
    fn even_con_is_white_not_gold() {
        let (r, g, b) = ConBand::White.rgb();
        assert!(r > 200 && g > 200 && b > 200);
        assert_ne!((r, g, b), (240, 210, 80));
    }
}
