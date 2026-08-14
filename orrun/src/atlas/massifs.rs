//! Sparse alpine ridge systems placed on the authored orogen crests.
//!
//! A massif is not a radial bump. It is a short branching ridge graph: height
//! lives on the skeleton and falls sideways as a crest, a headwall, then talus.
//! Noise still supplies rock grain; it does not decide where the mountain is.

use glam::Vec2;
use rand::Rng;
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;

use super::hydro::HydroVectors;
use super::orogen::OrogenGuide;
use super::{layer_seed, pack, smoothstep, CELL_METRES};

const CANDIDATE_BLOCK_CELLS: usize = 4;
const MAX_CREST_DISTANCE_CELLS: f32 = 3.6;
const MIN_ELEVATION_CODE: u8 = 155;
const MIN_RELIEF_CODE: u8 = 28;
const MIN_SPACING_M: f32 = 5_200.0;
const MAX_SPACING_M: f32 = 9_200.0;
const MAX_MASSIFS: usize = 512;
pub(crate) const MAX_RIDGES: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AlpineRidgeSeed {
    pub ax_m: f32,
    pub az_m: f32,
    pub bx_m: f32,
    pub bz_m: f32,
    pub height_a_m: f32,
    pub height_b_m: f32,
    pub left_half_m: f32,
    pub right_half_m: f32,
    pub break01: f32,
}

impl AlpineRidgeSeed {
    fn empty() -> Self {
        Self {
            ax_m: 0.0,
            az_m: 0.0,
            bx_m: 0.0,
            bz_m: 0.0,
            height_a_m: 0.0,
            height_b_m: 0.0,
            left_half_m: 400.0,
            right_half_m: 400.0,
            break01: 0.35,
        }
    }

    fn is_valid(self) -> bool {
        let values = [
            self.ax_m,
            self.az_m,
            self.bx_m,
            self.bz_m,
            self.height_a_m,
            self.height_b_m,
            self.left_half_m,
            self.right_half_m,
            self.break01,
        ];
        let length_m = (self.bx_m - self.ax_m).hypot(self.bz_m - self.az_m);
        values.into_iter().all(f32::is_finite)
            && length_m >= 80.0
            && (0.0..=1_200.0).contains(&self.height_a_m)
            && (0.0..=1_200.0).contains(&self.height_b_m)
            && (220.0..=2_800.0).contains(&self.left_half_m)
            && (220.0..=2_800.0).contains(&self.right_half_m)
            && (0.12..=0.70).contains(&self.break01)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AlpineMassifSeed {
    pub centre_x_m: f32,
    pub centre_z_m: f32,
    pub crest_axis_x: f32,
    pub crest_axis_z: f32,
    pub prominence_m: f32,
    pub summit_along_offset_m: f32,
    pub summit_across_offset_m: f32,
    pub ridges: [AlpineRidgeSeed; MAX_RIDGES],
    pub ridge_count: u8,
}

impl AlpineMassifSeed {
    pub(crate) fn is_valid(&self, world_extent_m: f32) -> bool {
        let finite = [
            self.centre_x_m,
            self.centre_z_m,
            self.crest_axis_x,
            self.crest_axis_z,
            self.prominence_m,
            self.summit_along_offset_m,
            self.summit_across_offset_m,
        ]
        .into_iter()
        .all(f32::is_finite);
        let axis_length_sq =
            self.crest_axis_x * self.crest_axis_x + self.crest_axis_z * self.crest_axis_z;
        finite
            && (0.0..world_extent_m).contains(&self.centre_x_m)
            && (0.0..world_extent_m).contains(&self.centre_z_m)
            && (0.98..=1.02).contains(&axis_length_sq)
            && (250.0..=1_200.0).contains(&self.prominence_m)
            && (4..=MAX_RIDGES as u8).contains(&self.ridge_count)
            && self
                .ridges
                .iter()
                .take(self.ridge_count as usize)
                .all(|ridge| ridge.is_valid())
    }
}

#[derive(Clone, Copy)]
struct Candidate {
    index: usize,
    x_m: f32,
    z_m: f32,
    score: f32,
}

pub(crate) struct MassifSource<'a> {
    pub(crate) size: usize,
    pub(crate) land: &'a [u8],
    pub(crate) lake_id: &'a [i32],
    pub(crate) elev_code: &'a [u8],
    pub(crate) relief: &'a [u8],
    pub(crate) hydro: &'a HydroVectors,
}

pub(crate) fn seed_massifs(
    world_seed: i32,
    source: MassifSource<'_>,
    guide: &OrogenGuide,
) -> Vec<AlpineMassifSeed> {
    let MassifSource {
        size,
        land,
        lake_id,
        elev_code,
        relief,
        hydro,
    } = source;
    let mut candidates = Vec::new();
    for block_z in (0..size).step_by(CANDIDATE_BLOCK_CELLS) {
        for block_x in (0..size).step_by(CANDIDATE_BLOCK_CELLS) {
            let mut best: Option<Candidate> = None;
            let z1 = (block_z + CANDIDATE_BLOCK_CELLS).min(size);
            let x1 = (block_x + CANDIDATE_BLOCK_CELLS).min(size);
            for az in block_z..z1 {
                for ax in block_x..x1 {
                    let i = az * size + ax;
                    if land[i] == 0
                        || lake_id[i] >= 0
                        || elev_code[i] < MIN_ELEVATION_CODE
                        || relief[i] < MIN_RELIEF_CODE
                        || guide.distance_cells[i] > MAX_CREST_DISTANCE_CELLS
                        || guide.pass01[i] > 0.72
                    {
                        continue;
                    }
                    let centre = Vec2::new(
                        (ax as f32 + 0.5) * CELL_METRES,
                        (az as f32 + 0.5) * CELL_METRES,
                    );
                    if !hydro.contains_land(size, centre) {
                        continue;
                    }
                    let crest =
                        1.0 - smoothstep(0.35, MAX_CREST_DISTANCE_CELLS, guide.distance_cells[i]);
                    let altitude = ((elev_code[i] as f32 - MIN_ELEVATION_CODE as f32)
                        / (255.0 - MIN_ELEVATION_CODE as f32))
                        .clamp(0.0, 1.0);
                    let relief01 = relief[i] as f32 / 63.0;
                    let score = crest
                        * (0.45 + altitude * 0.55)
                        * relief01
                        * (1.0 - guide.pass01[i] * 0.82);
                    let candidate = Candidate {
                        index: i,
                        x_m: (ax as f32 + 0.5) * CELL_METRES,
                        z_m: (az as f32 + 0.5) * CELL_METRES,
                        score,
                    };
                    if best.map(|old| score > old.score).unwrap_or(true) {
                        best = Some(candidate);
                    }
                }
            }
            if let Some(candidate) = best {
                candidates.push(candidate);
            }
        }
    }

    candidates.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.index.cmp(&b.index))
    });
    let seed = u64::from(layer_seed(world_seed, "atlas_alpine_massifs"));
    let world_extent_m = size as f32 * CELL_METRES;
    let mut accepted = Vec::new();
    for candidate in candidates {
        if accepted.len() >= MAX_MASSIFS {
            break;
        }
        let ax = candidate.index % size;
        let az = candidate.index / size;
        let mut rng = ChaCha8Rng::seed_from_u64(
            seed ^ (ax as u64).wrapping_mul(0x9E37_79B9) ^ (az as u64).wrapping_mul(0x85EB_CA6B),
        );
        let spacing_m = rng.gen_range(MIN_SPACING_M..MAX_SPACING_M);
        if accepted.iter().any(|site: &AlpineMassifSeed| {
            (site.centre_x_m - candidate.x_m).hypot(site.centre_z_m - candidate.z_m) < spacing_m
        }) {
            continue;
        }

        let axis_x = guide.axis_x[candidate.index];
        let axis_z = guide.axis_z[candidate.index];
        let along_jitter_m = rng.gen_range(-420.0..420.0);
        let jittered_x_m =
            (candidate.x_m + axis_x * along_jitter_m).clamp(1.0, world_extent_m - 1.0);
        let jittered_z_m =
            (candidate.z_m + axis_z * along_jitter_m).clamp(1.0, world_extent_m - 1.0);
        let jittered_ax = (jittered_x_m / CELL_METRES).floor() as usize;
        let jittered_az = (jittered_z_m / CELL_METRES).floor() as usize;
        let jittered_index = jittered_az * size + jittered_ax;
        let (centre_x_m, centre_z_m) = if land[jittered_index] != 0 && lake_id[jittered_index] < 0 {
            (jittered_x_m, jittered_z_m)
        } else {
            (candidate.x_m, candidate.z_m)
        };
        let elevation_m =
            pack::elevation_to_metres(i32::from(elev_code[candidate.index])).max(0) as f32;
        let altitude01 = ((elevation_m - 650.0) / 2_850.0).clamp(0.0, 1.0);
        let prominence_m = rng.gen_range(360.0..690.0) + altitude01 * rng.gen_range(160.0..390.0);
        let summit_along_offset_m = rng.gen_range(-180.0..240.0);
        let summit_across_offset_m = rng.gen_range(-160.0..160.0);
        let (ridges, ridge_count) = build_ridges(
            &mut rng,
            Vec2::new(centre_x_m, centre_z_m),
            Vec2::new(axis_x, axis_z),
            summit_along_offset_m,
            summit_across_offset_m,
            prominence_m,
            size,
            hydro,
        );
        if ridge_count < 4 {
            continue;
        }
        let site = AlpineMassifSeed {
            centre_x_m,
            centre_z_m,
            crest_axis_x: axis_x,
            crest_axis_z: axis_z,
            prominence_m,
            summit_along_offset_m,
            summit_across_offset_m,
            ridges,
            ridge_count,
        };
        let summit = summit_of(&site);
        if hydro.contains_land(size, summit) {
            accepted.push(site);
        }
    }
    accepted
}

fn build_ridges(
    rng: &mut ChaCha8Rng,
    centre: Vec2,
    axis: Vec2,
    summit_along_offset_m: f32,
    summit_across_offset_m: f32,
    prominence_m: f32,
    size: usize,
    hydro: &HydroVectors,
) -> ([AlpineRidgeSeed; MAX_RIDGES], u8) {
    let across = Vec2::new(-axis.y, axis.x);
    let summit = centre + axis * summit_along_offset_m + across * summit_across_offset_m;
    let back_m = rng.gen_range(1_850.0..3_200.0);
    let forward_m = rng.gen_range(1_250.0..2_350.0);
    let rise_t = rng.gen_range(0.28..0.52);
    let mid_t = rng.gen_range(0.22..0.46);
    let tail = summit - axis * back_m;
    let rise = summit - axis * (back_m * rise_t);
    let mid = summit + axis * (forward_m * mid_t);
    let head = summit + axis * forward_m;

    let h_tail = prominence_m * rng.gen_range(0.08..0.18);
    let h_rise = prominence_m * rng.gen_range(0.30..0.56);
    let h_mid = prominence_m * rng.gen_range(0.38..0.66);
    let h_head = prominence_m * rng.gen_range(0.06..0.20);
    let cliff_half_m = rng.gen_range(360.0..680.0);
    let talus_half_m = rng.gen_range(1_150.0..2_150.0);
    let (left_half_m, right_half_m) = if rng.gen::<f32>() < 0.5 {
        (cliff_half_m, talus_half_m)
    } else {
        (talus_half_m, cliff_half_m)
    };
    let break01 = rng.gen_range(0.20..0.38);

    let mut ridges = [AlpineRidgeSeed::empty(); MAX_RIDGES];
    let mut count = 0u8;
    push_ridge(
        &mut ridges,
        &mut count,
        tail,
        rise,
        h_tail,
        h_rise,
        left_half_m,
        right_half_m,
        break01,
        size,
        hydro,
    );
    push_ridge(
        &mut ridges,
        &mut count,
        rise,
        summit,
        h_rise,
        prominence_m,
        left_half_m,
        right_half_m,
        break01,
        size,
        hydro,
    );
    push_ridge(
        &mut ridges,
        &mut count,
        summit,
        mid,
        prominence_m,
        h_mid,
        left_half_m,
        right_half_m,
        break01,
        size,
        hydro,
    );
    push_ridge(
        &mut ridges,
        &mut count,
        mid,
        head,
        h_mid,
        h_head,
        left_half_m,
        right_half_m,
        break01,
        size,
        hydro,
    );

    let spur_specs = [
        (summit, prominence_m * 0.88, 0.95, 1.25, 1_000.0, 1_850.0),
        (rise, h_rise * 0.92, -1.35, -0.85, 800.0, 1_550.0),
        (mid, h_mid * 0.90, 0.70, 1.15, 750.0, 1_450.0),
        (summit, prominence_m * 0.70, -1.20, -0.75, 700.0, 1_250.0),
    ];
    for (origin, attach_m, ang_lo, ang_hi, len_lo, len_hi) in spur_specs {
        if count as usize >= MAX_RIDGES {
            break;
        }
        let angle = rng.gen_range(ang_lo..ang_hi);
        let dir = rotate(axis, angle);
        let length_m = rng.gen_range(len_lo..len_hi);
        let tip = origin + dir * length_m;
        let tip_h = attach_m * rng.gen_range(0.12..0.30);
        let spur_cliff = rng.gen_range(280.0..520.0);
        let spur_talus = rng.gen_range(700.0..1_400.0);
        let (spur_left, spur_right) = if rng.gen::<f32>() < 0.5 {
            (spur_cliff, spur_talus)
        } else {
            (spur_talus, spur_cliff)
        };
        push_ridge(
            &mut ridges,
            &mut count,
            origin,
            tip,
            attach_m,
            tip_h,
            spur_left,
            spur_right,
            rng.gen_range(0.18..0.42),
            size,
            hydro,
        );
    }
    (ridges, count)
}

fn push_ridge(
    ridges: &mut [AlpineRidgeSeed; MAX_RIDGES],
    count: &mut u8,
    a: Vec2,
    b: Vec2,
    height_a_m: f32,
    height_b_m: f32,
    left_half_m: f32,
    right_half_m: f32,
    break01: f32,
    size: usize,
    hydro: &HydroVectors,
) {
    if *count as usize >= MAX_RIDGES {
        return;
    }
    if (b - a).length() < 80.0 {
        return;
    }
    if !hydro.contains_land(size, a) || !hydro.contains_land(size, b) {
        return;
    }
    ridges[*count as usize] = AlpineRidgeSeed {
        ax_m: a.x,
        az_m: a.y,
        bx_m: b.x,
        bz_m: b.y,
        height_a_m,
        height_b_m,
        left_half_m,
        right_half_m,
        break01,
    };
    *count += 1;
}

fn rotate(axis: Vec2, angle_rad: f32) -> Vec2 {
    let (s, c) = angle_rad.sin_cos();
    Vec2::new(axis.x * c - axis.y * s, axis.x * s + axis.y * c)
}

fn summit_of(site: &AlpineMassifSeed) -> Vec2 {
    Vec2::new(
        site.centre_x_m + site.crest_axis_x * site.summit_along_offset_m
            - site.crest_axis_z * site.summit_across_offset_m,
        site.centre_z_m
            + site.crest_axis_z * site.summit_along_offset_m
            + site.crest_axis_x * site.summit_across_offset_m,
    )
}
