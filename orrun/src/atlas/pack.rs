//! Climate bit packing and elevation↔metres mapping.

const SEA_FLOOR_M: f32 = -80.0;
const SEA_CODE_MAX: i32 = 32;
const COAST_CODE_MIN: i32 = 33;
const COAST_CODE_MAX: i32 = 40;
const COAST_MAX_M: f32 = 20.0;
const LAND_CODE_MIN: i32 = 41;
const LAND_CODE_MAX: i32 = 255;
const PEAK_MAX_M: f32 = 4000.0;
const LAND_EXP_K: f32 = 4.0;

use super::biomes::Biome;

#[inline]
fn clamp_i(v: i32, lo: i32, hi: i32) -> i32 {
    v.clamp(lo, hi)
}

#[inline]
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

pub fn pack(elevation: i32, humidity: i32, biome: Biome, relief: i32, population: i32) -> i32 {
    clamp_i(elevation, 0, 255)
        | (clamp_i(humidity, 0, 255) << 8)
        | (clamp_i(biome as i32, 0, 63) << 16)
        | (clamp_i(relief, 0, 63) << 22)
        | (clamp_i(population, 0, 15) << 28)
}

#[inline]
pub fn elevation(cell: i32) -> i32 {
    cell & 0xFF
}

#[inline]
pub fn humidity(cell: i32) -> i32 {
    (cell >> 8) & 0xFF
}

#[inline]
pub fn biome(cell: i32) -> Biome {
    Biome::from_id((cell >> 16) & 0x3F)
}

#[inline]
pub fn relief(cell: i32) -> i32 {
    (cell >> 22) & 0x3F
}

#[inline]
pub fn population(cell: i32) -> i32 {
    (cell >> 28) & 0xF
}

/// Quantized metres for an elevation code.
pub fn elevation_to_metres(code: i32) -> i32 {
    let c = clamp_i(code, 0, 255);
    if c <= SEA_CODE_MAX {
        return lerp(SEA_FLOOR_M, 0.0, c as f32 / SEA_CODE_MAX as f32) as i32;
    }
    if c <= COAST_CODE_MAX {
        let u = (c - COAST_CODE_MIN) as f32 / (COAST_CODE_MAX - COAST_CODE_MIN) as f32;
        return lerp(0.0, COAST_MAX_M, u) as i32;
    }
    let t = (c - LAND_CODE_MIN) as f32 / (LAND_CODE_MAX - LAND_CODE_MIN) as f32;
    land_exp_metres(t) as i32
}

pub fn metres_to_elevation(metres: i32) -> i32 {
    let m = metres as f32;
    if m <= 0.0 {
        return clamp_i(
            ((m - SEA_FLOOR_M) / (0.0 - SEA_FLOOR_M) * SEA_CODE_MAX as f32) as i32,
            0,
            SEA_CODE_MAX,
        );
    }
    if m <= COAST_MAX_M {
        let u = m / COAST_MAX_M;
        return COAST_CODE_MIN + (u * (COAST_CODE_MAX - COAST_CODE_MIN) as f32) as i32;
    }
    let span = PEAK_MAX_M - COAST_MAX_M;
    let ratio = ((m - COAST_MAX_M) / span).clamp(0.0, 1.0);
    let ek = LAND_EXP_K.exp();
    let inside = 1.0 + ratio * (ek - 1.0);
    let t = inside.ln() / LAND_EXP_K;
    clamp_i(
        LAND_CODE_MIN + (t * (LAND_CODE_MAX - LAND_CODE_MIN) as f32 + 0.5) as i32,
        LAND_CODE_MIN,
        LAND_CODE_MAX,
    )
}

fn land_exp_metres(t01: f32) -> f32 {
    let t = t01.clamp(0.0, 1.0);
    let ek = LAND_EXP_K.exp();
    let shaped = ((LAND_EXP_K * t).exp() - 1.0) / (ek - 1.0);
    COAST_MAX_M + (PEAK_MAX_M - COAST_MAX_M) * shaped
}

#[cfg(test)]
mod elev_tests {
    use super::*;

    #[test]
    fn elevation_endpoints() {
        assert!(elevation_to_metres(0) < 0);
        assert_eq!(elevation_to_metres(32), 0);
        assert!(elevation_to_metres(40) > 0);
        assert!(elevation_to_metres(40) <= COAST_MAX_M as i32 + 1);
        assert!(elevation_to_metres(120) < 500);
        assert!(elevation_to_metres(200) >= 1200);
        assert!(elevation_to_metres(255) >= 3500);
        assert!(elevation_to_metres(255) <= PEAK_MAX_M as i32);
        assert!((elevation_to_metres(metres_to_elevation(100)) - 100).abs() <= 12);
        assert!((elevation_to_metres(metres_to_elevation(3000)) - 3000).abs() <= 80);
    }
}
