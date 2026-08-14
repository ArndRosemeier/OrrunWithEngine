//! Indexed finite alpine ridge systems.
//!
//! Height comes from a branching skeleton, not from distance to a point. A
//! sample takes the highest nearby ridge: crest table, then a slope break into
//! talus. Isolated cones are a function plot; this is a small range.

use crate::atlas::AlpineMassifSeed;

const INDEX_CELL_M: f32 = 6_000.0;
const CREST_TABLE_M: f32 = 140.0;

#[derive(Clone, Debug)]
pub(super) struct AlpineMassifField {
    sites: Vec<AlpineMassifSeed>,
    starts: Vec<u32>,
    items: Vec<u32>,
    world_extent_m: f32,
    cells_per_axis: usize,
}

impl AlpineMassifField {
    pub(super) fn build(sites: &[AlpineMassifSeed], world_extent_m: f32) -> Self {
        assert!(
            world_extent_m.is_finite() && world_extent_m > 0.0,
            "alpine massif index needs a finite positive world extent"
        );
        let cells_per_axis = (world_extent_m / INDEX_CELL_M).ceil().max(1.0) as usize;
        let mut buckets = vec![Vec::<u32>::new(); cells_per_axis * cells_per_axis];
        for (site_index, site) in sites.iter().enumerate() {
            let reach = site_reach(site);
            let min_x = cell_of(site.centre_x_m - reach, cells_per_axis);
            let max_x = cell_of(site.centre_x_m + reach, cells_per_axis);
            let min_z = cell_of(site.centre_z_m - reach, cells_per_axis);
            let max_z = cell_of(site.centre_z_m + reach, cells_per_axis);
            let item = u32::try_from(site_index).expect("too many alpine massif sites");
            for cz in min_z..=max_z {
                for cx in min_x..=max_x {
                    buckets[cz * cells_per_axis + cx].push(item);
                }
            }
        }

        let mut starts = Vec::with_capacity(buckets.len() + 1);
        let mut items = Vec::new();
        starts.push(0);
        for bucket in buckets {
            items.extend(bucket);
            starts.push(u32::try_from(items.len()).expect("alpine massif index exceeds u32"));
        }
        Self {
            sites: sites.to_vec(),
            starts,
            items,
            world_extent_m,
            cells_per_axis,
        }
    }

    pub(super) fn height(&self, x: f32, z: f32) -> f32 {
        if x < 0.0 || z < 0.0 || x >= self.world_extent_m || z >= self.world_extent_m {
            return 0.0;
        }
        let cx = cell_of(x, self.cells_per_axis);
        let cz = cell_of(z, self.cells_per_axis);
        let cell = cz * self.cells_per_axis + cx;
        let begin = self.starts[cell] as usize;
        let end = self.starts[cell + 1] as usize;
        self.items[begin..end]
            .iter()
            .map(|&index| massif_height(&self.sites[index as usize], x, z))
            .fold(0.0, f32::max)
    }
}

fn site_reach(site: &AlpineMassifSeed) -> f32 {
    site.ridges
        .iter()
        .take(site.ridge_count as usize)
        .map(|ridge| {
            let to_a = (ridge.ax_m - site.centre_x_m).hypot(ridge.az_m - site.centre_z_m);
            let to_b = (ridge.bx_m - site.centre_x_m).hypot(ridge.bz_m - site.centre_z_m);
            to_a.max(to_b) + ridge.left_half_m.max(ridge.right_half_m)
        })
        .fold(0.0, f32::max)
}

fn cell_of(value_m: f32, cells_per_axis: usize) -> usize {
    ((value_m / INDEX_CELL_M).floor() as i64).clamp(0, cells_per_axis as i64 - 1) as usize
}

fn massif_height(site: &AlpineMassifSeed, x: f32, z: f32) -> f32 {
    site.ridges
        .iter()
        .take(site.ridge_count as usize)
        .map(|ridge| ridge_height(ridge, x, z))
        .fold(0.0, f32::max)
}

fn ridge_height(ridge: &crate::atlas::AlpineRidgeSeed, x: f32, z: f32) -> f32 {
    let abx = ridge.bx_m - ridge.ax_m;
    let abz = ridge.bz_m - ridge.az_m;
    let ab_len_sq = abx * abx + abz * abz;
    if ab_len_sq < 1.0 {
        return 0.0;
    }
    let apx = x - ridge.ax_m;
    let apz = z - ridge.az_m;
    let t = ((apx * abx + apz * abz) / ab_len_sq).clamp(0.0, 1.0);
    let closest_x = ridge.ax_m + abx * t;
    let closest_z = ridge.az_m + abz * t;
    let dx = x - closest_x;
    let dz = z - closest_z;
    let dist_m = dx.hypot(dz);
    let side = abx * dz - abz * dx;
    let half_m = if side >= 0.0 {
        ridge.left_half_m
    } else {
        ridge.right_half_m
    };
    if dist_m >= half_m {
        return 0.0;
    }
    let along_m = ridge.height_a_m + (ridge.height_b_m - ridge.height_a_m) * t;
    along_m * cross_section(dist_m, half_m, ridge.break01)
}

/// Crest table, then a slope break, then talus to zero.
///
/// A power of radial distance is a function plot. The break is the landform.
fn cross_section(dist_m: f32, half_m: f32, break01: f32) -> f32 {
    let crest_m = CREST_TABLE_M.min(half_m * 0.22);
    if dist_m <= crest_m {
        return 1.0 - 0.06 * (dist_m / crest_m.max(1.0));
    }
    let rest_m = (half_m - crest_m).max(1.0);
    let v = ((dist_m - crest_m) / rest_m).clamp(0.0, 1.0);
    let brk = break01.clamp(0.15, 0.65);
    if v < brk {
        0.94 - (0.94 - 0.34) * (v / brk)
    } else {
        let t = (v - brk) / (1.0 - brk).max(0.001);
        0.34 * (1.0 - t).powf(1.25)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atlas::AlpineRidgeSeed;

    fn range() -> AlpineMassifSeed {
        let crest = |ax, az, bx, bz, ha, hb| AlpineRidgeSeed {
            ax_m: ax,
            az_m: az,
            bx_m: bx,
            bz_m: bz,
            height_a_m: ha,
            height_b_m: hb,
            left_half_m: 480.0,
            right_half_m: 1_600.0,
            break01: 0.26,
        };
        let spur = |ax, az, bx, bz, ha, hb| AlpineRidgeSeed {
            ax_m: ax,
            az_m: az,
            bx_m: bx,
            bz_m: bz,
            height_a_m: ha,
            height_b_m: hb,
            left_half_m: 1_100.0,
            right_half_m: 360.0,
            break01: 0.30,
        };
        AlpineMassifSeed {
            centre_x_m: 8_000.0,
            centre_z_m: 8_000.0,
            crest_axis_x: 1.0,
            crest_axis_z: 0.0,
            prominence_m: 800.0,
            summit_along_offset_m: 120.0,
            summit_across_offset_m: -80.0,
            ridges: [
                crest(5_600.0, 7_920.0, 7_200.0, 7_920.0, 110.0, 360.0),
                crest(7_200.0, 7_920.0, 8_120.0, 7_920.0, 360.0, 800.0),
                crest(8_120.0, 7_920.0, 9_000.0, 7_920.0, 800.0, 420.0),
                crest(9_000.0, 7_920.0, 10_200.0, 7_920.0, 420.0, 90.0),
                spur(8_120.0, 7_920.0, 8_120.0, 6_400.0, 700.0, 140.0),
                spur(7_200.0, 7_920.0, 6_400.0, 9_100.0, 330.0, 80.0),
                AlpineRidgeSeed {
                    ax_m: 0.0,
                    az_m: 0.0,
                    bx_m: 0.0,
                    bz_m: 0.0,
                    height_a_m: 0.0,
                    height_b_m: 0.0,
                    left_half_m: 400.0,
                    right_half_m: 400.0,
                    break01: 0.35,
                },
                AlpineRidgeSeed {
                    ax_m: 0.0,
                    az_m: 0.0,
                    bx_m: 0.0,
                    bz_m: 0.0,
                    height_a_m: 0.0,
                    height_b_m: 0.0,
                    left_half_m: 400.0,
                    right_half_m: 400.0,
                    break01: 0.35,
                },
            ],
            ridge_count: 6,
        }
    }

    fn summit(seed: &AlpineMassifSeed) -> (f32, f32) {
        let across_x = -seed.crest_axis_z;
        let across_z = seed.crest_axis_x;
        (
            seed.centre_x_m
                + seed.crest_axis_x * seed.summit_along_offset_m
                + across_x * seed.summit_across_offset_m,
            seed.centre_z_m
                + seed.crest_axis_z * seed.summit_along_offset_m
                + across_z * seed.summit_across_offset_m,
        )
    }

    #[test]
    fn ridge_system_is_finite_pointed_and_asymmetric() {
        let seed = range();
        let (sx, sz) = summit(&seed);
        assert!((massif_height(&seed, sx, sz) - seed.prominence_m).abs() < 1.0);
        assert_eq!(massif_height(&seed, sx + 5_500.0, sz), 0.0);
        let cliff = massif_height(&seed, sx + 500.0, sz + 280.0);
        let talus = massif_height(&seed, sx + 500.0, sz - 280.0);
        assert!(
            talus > cliff * 1.35,
            "asymmetric faces disappeared: cliff={cliff:.1}, talus={talus:.1}"
        );
    }

    #[test]
    fn ridges_break_radial_symmetry_and_leave_a_slope_break() {
        let seed = range();
        let (sx, sz) = summit(&seed);
        let samples = 72usize;
        let mut ring = Vec::with_capacity(samples);
        for i in 0..samples {
            let angle = i as f32 / samples as f32 * std::f32::consts::TAU;
            ring.push(massif_height(
                &seed,
                sx + angle.cos() * 700.0,
                sz + angle.sin() * 700.0,
            ));
        }
        let mean = ring.iter().sum::<f32>() / samples as f32;
        let var = ring
            .iter()
            .map(|h| {
                let d = *h - mean;
                d * d
            })
            .sum::<f32>()
            / samples as f32;
        let cv = var.sqrt() / mean.max(1.0);
        assert!(
            cv > 0.22,
            "a ring around the summit is still a cone (cv {cv:.2})"
        );

        let mut heights = Vec::new();
        for i in 0..36 {
            heights.push(massif_height(&seed, sx, sz + i as f32 * 40.0));
        }
        let slopes: Vec<f32> = heights.windows(2).map(|w| (w[1] - w[0]) / 40.0).collect();
        let breaks = slopes
            .windows(2)
            .filter(|w| (w[1] - w[0]).abs() > 0.18)
            .count();
        assert!(
            breaks >= 1,
            "the flank is a regular power curve; expected a slope break"
        );
    }

    #[test]
    fn bucket_index_is_exact_for_a_single_massif() {
        let seed = range();
        let field = AlpineMassifField::build(std::slice::from_ref(&seed), 20_000.0);
        for z in (3_000..13_000).step_by(211) {
            for x in (3_000..13_000).step_by(197) {
                assert_eq!(
                    field.height(x as f32, z as f32),
                    massif_height(&seed, x as f32, z as f32)
                );
            }
        }
    }
}
