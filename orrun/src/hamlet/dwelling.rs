//! Village dwelling briefs: footprint, storeys, and theme for the house generator.

/// Kit XZ pitch from `Modular/catalogs/medieval.json`.
pub const PITCH_XZ: f32 = 4.0;
/// Storey / plinth height from the same catalog.
pub const FOUNDATION_M: f32 = 2.7;

/// Finish / roof filter. `Any` accepts every compatible family member.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum HouseTheme {
    #[default]
    Any,
}

/// Size and style advice for one house. Metres are `cells × `[`PITCH_XZ`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DwellingBrief {
    pub cells_x: u8,
    pub cells_z: u8,
    pub storeys: u8,
    pub theme: HouseTheme,
}

impl DwellingBrief {
    pub fn new(cells_x: u8, cells_z: u8, storeys: u8, theme: HouseTheme) -> Self {
        let brief = Self {
            cells_x,
            cells_z,
            storeys,
            theme,
        };
        brief.validate();
        brief
    }

    pub fn validate(self) {
        assert!(
            self.cells_x >= 3,
            "dwelling width {} needs a south door bay (cells_x >= 3)",
            self.cells_x
        );
        assert!(
            self.cells_z >= 2,
            "dwelling depth {} is below the two-cell minimum",
            self.cells_z
        );
        assert!(
            (1..=2).contains(&self.storeys),
            "dwelling storeys {} must be 1 or 2",
            self.storeys
        );
    }

    pub fn size_x(self) -> f32 {
        f32::from(self.cells_x) * PITCH_XZ
    }

    pub fn size_z(self) -> f32 {
        f32::from(self.cells_z) * PITCH_XZ
    }

    pub fn half_x(self) -> f32 {
        self.size_x() * 0.5
    }

    pub fn half_z(self) -> f32 {
        self.size_z() * 0.5
    }

    pub fn label(self) -> String {
        format!("{}×{}×{}", self.cells_x, self.cells_z, self.storeys)
    }
}

/// Allowed village footprints (door on the south edge ⇒ width ≥ 3).
pub const FOOTPRINTS: &[(u8, u8)] = &[
    (3, 2),
    (3, 3),
    (3, 4),
    (4, 2),
    (4, 3),
    (4, 4),
    (5, 3),
    (5, 4),
];

fn footprint_area(cells: (u8, u8)) -> u32 {
    u32::from(cells.0) * u32::from(cells.1)
}

/// Largest depth among [`FOOTPRINTS`], for frontier expansion.
pub fn max_footprint_depth_m() -> f32 {
    FOOTPRINTS
        .iter()
        .map(|(_, z)| f32::from(*z) * PITCH_XZ)
        .fold(0.0_f32, f32::max)
}

/// Weighted footprint pick: smaller plans are more common, especially on low tiers.
pub fn roll_footprint(tier: u8, rng: &mut impl rand::Rng) -> (u8, u8) {
    let weights: Vec<(u32, (u8, u8))> = FOOTPRINTS
        .iter()
        .copied()
        .map(|cells| {
            let area = footprint_area(cells);
            // Inverse-area bias; tier 0 doubles the preference for the smallest rings.
            let mut w = 40u32 / area.max(1);
            w = w.max(1);
            if tier == 0 && area <= 8 {
                w *= 2;
            }
            if tier >= 2 && area >= 16 {
                w += 2;
            }
            (w, cells)
        })
        .collect();
    let total: u32 = weights.iter().map(|(w, _)| *w).sum();
    let mut pick = rng.gen_range(0..total);
    for (w, cells) in weights {
        if pick < w {
            return cells;
        }
        pick -= w;
    }
    FOOTPRINTS[0]
}

/// Preferred footprint first, then every other footprint sorted by ascending area
/// (and by depth) so tight holes can still accept a house.
pub fn footprints_fallback_order(preferred: (u8, u8)) -> Vec<(u8, u8)> {
    let mut rest: Vec<(u8, u8)> = FOOTPRINTS
        .iter()
        .copied()
        .filter(|c| *c != preferred)
        .collect();
    rest.sort_by_key(|c| (footprint_area(*c), c.1, c.0));
    let mut out = Vec::with_capacity(FOOTPRINTS.len());
    out.push(preferred);
    out.extend(rest);
    out
}

/// ~70% single storey, ~30% two.
pub fn roll_storeys(rng: &mut impl rand::Rng) -> u8 {
    if rng.gen_bool(0.30) {
        2
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    #[test]
    fn every_listed_footprint_is_valid() {
        for &(x, z) in FOOTPRINTS {
            DwellingBrief::new(x, z, 1, HouseTheme::Any).validate();
            DwellingBrief::new(x, z, 2, HouseTheme::Any).validate();
        }
    }

    #[test]
    fn fallback_puts_preferred_first() {
        let order = footprints_fallback_order((5, 4));
        assert_eq!(order[0], (5, 4));
        assert_eq!(order.len(), FOOTPRINTS.len());
    }

    #[test]
    fn roll_footprint_stays_in_table() {
        let mut rng = StdRng::seed_from_u64(9);
        for _ in 0..64 {
            let cells = roll_footprint(0, &mut rng);
            assert!(FOOTPRINTS.contains(&cells));
        }
    }
}
