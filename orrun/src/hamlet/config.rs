//! Knobs for the evolutionary marketplace hamlet lab.

/// Civic catalog ids placed once per settlement when the tier allows.
pub const CIVIC_BY_TIER: [&[&str]; 4] = [
    &["Well"],
    &["Well"],
    &["Well", "Inn", "Blacksmith", "Sawmill", "Stable", "Gazebo"],
    &[
        "Well",
        "Inn",
        "Blacksmith",
        "Mill",
        "Sawmill",
        "Stable",
        "Bell_Tower",
        "Gazebo",
    ],
];

#[derive(Clone, Debug)]
pub struct HamletLabConfig {
    pub seed: u64,
    /// 0=hamlet … 3=port.
    pub tier: u8,
    /// Geometric-mean marketplace semi-axis (oriented ellipse base).
    pub market_radius: f32,
    pub market_aspect_min: f32,
    pub market_aspect_max: f32,
    /// Fractional radial jitter vs local ellipse radius (0–~0.9).
    pub market_radius_jitter: f32,
    /// Fractional angular jitter vs half-sector (0–~0.95).
    pub market_angle_jitter: f32,
    /// Clear gap from market rim to house front wall.
    pub market_front_gap: f32,
    /// How far beyond the first ring settlers may push.
    pub max_settle_radius: f32,
    pub dwelling_min: u32,
    pub dwelling_max: u32,
    /// Soft alley dilation used when testing free plots (metres).
    pub alley: f32,
    pub occupancy_cell: f32,
    /// Candidate poses evaluated per settler.
    pub candidates_per_settler: u32,
    /// Softmax temperature: lower = greedier toward best plot.
    pub select_temperature: f32,
    /// Weight: closer door-to-market is better.
    pub weight_market: f32,
    /// Weight: flatter, drier ground is better. Unused when no plot is given.
    pub weight_ground: f32,
    /// Score bump when a candidate shares a wall.
    pub wall_share_boost: f32,
    /// Small random noise on fitness.
    pub fitness_noise: f32,
    pub show_occupancy: bool,
}

impl Default for HamletLabConfig {
    fn default() -> Self {
        Self {
            seed: 1,
            tier: 0,
            market_radius: 4.0,
            market_aspect_min: 1.45,
            market_aspect_max: 2.85,
            market_radius_jitter: 0.62,
            market_angle_jitter: 0.88,
            market_front_gap: 2.5,
            max_settle_radius: 55.0,
            dwelling_min: 6,
            dwelling_max: 15,
            alley: 0.6,
            occupancy_cell: 0.35,
            candidates_per_settler: 80,
            select_temperature: 0.18,
            weight_market: 2.2,
            weight_ground: 1.6,
            wall_share_boost: 0.12,
            fitness_noise: 0.08,
            show_occupancy: false,
        }
    }
}

impl HamletLabConfig {
    pub fn market_side_count(&self) -> usize {
        tier_market_sides(self.tier)
    }

    pub fn apply_tier_defaults(&mut self, tier: u8) {
        self.tier = tier.min(3);
        match self.tier {
            0 => {
                self.dwelling_min = 6;
                self.dwelling_max = 15;
                self.market_radius = 8.0;
                self.max_settle_radius = 80.0;
                self.candidates_per_settler = 80;
                self.occupancy_cell = 0.4;
            }
            1 => {
                self.dwelling_min = 20;
                self.dwelling_max = 80;
                self.market_radius = 12.0;
                self.max_settle_radius = 160.0;
                self.candidates_per_settler = 100;
                self.occupancy_cell = 0.45;
            }
            2 => {
                self.dwelling_min = 100;
                self.dwelling_max = 400;
                self.market_radius = 18.0;
                self.max_settle_radius = 320.0;
                self.candidates_per_settler = 80;
                self.occupancy_cell = 0.6;
            }
            _ => {
                self.dwelling_min = 500;
                self.dwelling_max = 2000;
                self.market_radius = 28.0;
                self.max_settle_radius = 600.0;
                self.candidates_per_settler = 60;
                self.occupancy_cell = 0.8;
            }
        }
    }
}

/// Tier level 0=hamlet … 3=port; sides = (tier+1)*6.
pub fn tier_market_sides(tier: u8) -> usize {
    (tier.min(3) as usize + 1) * 6
}

pub fn tier_market_radius(tier: u8) -> f32 {
    match tier.min(3) {
        0 => 8.0,
        1 => 12.0,
        2 => 18.0,
        _ => 28.0,
    }
}
