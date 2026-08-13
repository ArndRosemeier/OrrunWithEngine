//! Deterministic randomness keyed by place.
//!
//! Everything the world scatters or grows below atlas scale — tufts, stones,
//! trees, the ponds they stand beside — has to come out the same on every
//! machine and every launch, and has to do so without anything being stored. So nothing here has state that outlives a call: a draw is a function
//! of the world seed, a lattice cell, and which stream is asking.
//!
//! Two consumers must never share a stream. A salt per purpose is what keeps a
//! grass tuft's roll from deciding where a spring rises.

/// A stream of `[0, 1)` draws for one lattice cell.
///
/// Slicing a single hash into fixed bit windows ran out of bits: a variant roll
/// taken from the top twenty could never exceed 1/16, and so every class placed
/// nothing but its first mesh across the whole world.
pub(super) struct CellRng(u64);

impl CellRng {
    pub(super) fn new(seed: u64, x: i64, z: i64) -> Self {
        Self(hash3(seed, x, z))
    }

    pub(super) fn unit(&mut self) -> f32 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        unit01(mix(self.0))
    }

    /// A draw in `[lo, hi)`.
    pub(super) fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.unit()
    }

    /// How many of `max` to place, rounding by chance rather than truncating, so
    /// a density of 0.3 per cell reads as three cells in ten and not as none.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn count(&mut self, expected: f32, max: usize) -> usize {
        let whole = expected.floor();
        let extra = if self.unit() < expected - whole {
            1.0
        } else {
            0.0
        };
        ((whole + extra).max(0.0) as usize).min(max)
    }
}

/// SplitMix64 over a lattice cell — cheap, and independent per salt.
pub(super) fn hash3(seed: u64, x: i64, z: i64) -> u64 {
    mix(seed
        ^ (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (z as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F))
}

#[inline]
fn mix(mut v: u64) -> u64 {
    v ^= v >> 30;
    v = v.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    v ^= v >> 27;
    v = v.wrapping_mul(0x94D0_49BB_1331_11EB);
    v ^ (v >> 31)
}

/// Bits of a hash as a `[0, 1)` fraction.
#[inline]
pub(super) fn unit01(h: u64) -> f32 {
    (h & 0xFF_FFFF) as f32 / 16_777_216.0
}

/// Smooth value noise on a unit lattice, in `[-1, 1]`.
pub(super) fn value_noise(seed: u64, x: f64, z: f64) -> f32 {
    let x0 = x.floor();
    let z0 = z.floor();
    let tx = smooth((x - x0) as f32);
    let tz = smooth((z - z0) as f32);
    let ix = x0 as i64;
    let iz = z0 as i64;
    let corner = |dx: i64, dz: i64| unit01(hash3(seed, ix + dx, iz + dz)) * 2.0 - 1.0;
    let a = blend(corner(0, 0), corner(1, 0), tx);
    let b = blend(corner(0, 1), corner(1, 1), tx);
    blend(a, b, tz)
}

#[inline]
fn smooth(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

#[inline]
fn blend(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_draw_in_a_cell_covers_the_whole_range() {
        // Placement reads six numbers per cell, and the last of them chose the
        // mesh. Drawn from a spent hash it never rose above 1/16, so every tuft
        // on the continent was the same tuft.
        const DRAWS: usize = 6;
        let mut lo = [1.0f32; DRAWS];
        let mut hi = [0.0f32; DRAWS];
        let mut sum = [0.0f64; DRAWS];
        let cells = 64 * 64;
        for z in 0..64i64 {
            for x in 0..64i64 {
                let mut rng = CellRng::new(0x51EED, x, z);
                for d in 0..DRAWS {
                    let v = rng.unit();
                    lo[d] = lo[d].min(v);
                    hi[d] = hi[d].max(v);
                    sum[d] += v as f64;
                }
            }
        }
        for d in 0..DRAWS {
            let mean = sum[d] / cells as f64;
            assert!(lo[d] < 0.02, "draw {d} never went low: {}", lo[d]);
            assert!(hi[d] > 0.98, "draw {d} never went high: {}", hi[d]);
            assert!(
                (mean - 0.5).abs() < 0.02,
                "draw {d} is biased: mean {mean:.3}"
            );
        }
    }

    #[test]
    fn a_cell_reads_the_same_whatever_reached_it_first() {
        // The whole point: a pond found from one lattice cell and the same pond
        // found from another have to be the same pond.
        let first: Vec<f32> = (0..8).map(|_| CellRng::new(7, -3, 91).unit()).collect();
        assert!(first.iter().all(|v| (*v - first[0]).abs() < f32::EPSILON));
        assert_ne!(
            CellRng::new(7, -3, 91).unit(),
            CellRng::new(7, 91, -3).unit(),
            "a cell and its transpose must not share a draw"
        );
    }

    #[test]
    fn a_fractional_count_lands_on_both_sides() {
        let mut none = 0;
        let mut one = 0;
        for x in 0..1_000i64 {
            match CellRng::new(1, x, 0).count(0.25, 3) {
                0 => none += 1,
                1 => one += 1,
                n => panic!("0.25 of at most 3 gave {n}"),
            }
        }
        assert!(one > 150 && one < 350, "0.25 came out as {one} in 1000");
        assert!(none > 650);
    }
}
