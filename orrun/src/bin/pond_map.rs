//! Plan view of the sub-atlas pond layer, as a PNG.
//!
//! A first-person camera is the wrong instrument for judging the *shape* of a
//! basin: from head height a hollow and a hillside look alike. This draws what
//! the carve would do to the ground over a few square kilometres — shaded
//! relief, atlas water, ponds as the 4 m cells they actually flood.
//!
//! Usage: `pond_map [seed] [size] [x] [z] [span_m]`

use std::sync::Arc;

use engine::space::GlobalXZ;
use engine::texture::save_rgba8_png;
use glam::Vec2;
use orrun::atlas::ContinentAtlas;
use orrun::world::{ContinentalSurface, PondField, WaterBody};

const PIXELS: usize = 1024;

/// What one pixel of ground turned out to be.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Paint {
    Land,
    /// Ocean, lake or atlas river: this layer does not touch it.
    Atlas,
    Pond,
}

#[derive(Debug)]
struct Args {
    seed: i32,
    size: usize,
    centre: GlobalXZ,
    span_m: f64,
}

fn parse_value<T: std::str::FromStr>(
    value: Option<&String>,
    name: &str,
    default: T,
) -> Result<T, String>
where
    T::Err: std::fmt::Display,
{
    match value {
        Some(value) => value
            .parse::<T>()
            .map_err(|error| format!("invalid {name} '{value}': {error}")),
        None => Ok(default),
    }
}

fn parse_args_from(raw: &[String], bounds_m: Option<f64>) -> Result<Args, String> {
    if raw.len() > 5 {
        return Err(format!("expected at most 5 arguments, got {}", raw.len()));
    }
    let seed = parse_value(raw.first(), "seed", 20260809i32)?;
    let size = parse_value(raw.get(1), "size", 256usize)?;
    if !(32..=512).contains(&size) {
        return Err(format!("size must be in 32..=512, got {size}"));
    }
    let mid = bounds_m.unwrap_or(0.0) * 0.5;
    let x = parse_value(raw.get(2), "x coordinate", mid)?;
    let z = parse_value(raw.get(3), "z coordinate", mid)?;
    let span_m = parse_value(raw.get(4), "span", 3_000.0f64)?;
    if !x.is_finite() || !z.is_finite() {
        return Err(format!("coordinates must be finite, got ({x}, {z})"));
    }
    if !span_m.is_finite() || span_m <= 0.0 {
        return Err(format!("span must be finite and positive, got {span_m}"));
    }
    if let Some(bounds_m) = bounds_m {
        if x < 0.0 || z < 0.0 || x >= bounds_m || z >= bounds_m {
            return Err(format!(
                "centre ({x}, {z}) is outside atlas bounds [0, {bounds_m})"
            ));
        }
    }
    Ok(Args {
        seed,
        size,
        centre: GlobalXZ::at(x, z),
        span_m,
    })
}

fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let head = parse_args_from(&raw, None).unwrap_or_else(|error| panic!("{error}"));
    let atlas = ContinentAtlas::generate(head.seed, head.size);
    let surface = Arc::new(ContinentalSurface::new(&atlas).expect("canonical surface"));
    // Re-read the arguments now that the atlas can supply a default centre.
    let args = parse_args_from(&raw, Some(surface.bounds().metres()))
        .unwrap_or_else(|error| panic!("{error}"));

    let started = std::time::Instant::now();
    let field = PondField::build(&surface, args.centre);
    eprintln!(
        "seed {} size {}: {} ponds around ({:.0}, {:.0}) in {} ms",
        args.seed,
        args.size,
        field.ponds().len(),
        args.centre.x,
        args.centre.z,
        started.elapsed().as_millis()
    );

    let step = args.span_m / PIXELS as f64;
    let origin = GlobalXZ::at(
        args.centre.x - args.span_m * 0.5,
        args.centre.z - args.span_m * 0.5,
    );
    // One pass for the ground as the game carves it, then shade from that, so a
    // basin shows as the hollow it cuts and not just as the water in it.
    let point = |px: usize, py: usize| {
        GlobalXZ::at(
            origin.x + (px as f64 + 0.5) * step,
            origin.z + (py as f64 + 0.5) * step,
        )
    };
    let mut ground = vec![0.0f32; PIXELS * PIXELS];
    let mut paint = vec![Paint::Land; PIXELS * PIXELS];
    for py in 0..PIXELS {
        for px in 0..PIXELS {
            let p = point(px, py);
            let mut column = surface.column(p);
            let atlas_water = column.is_wet();
            field.carve(p, &mut column);
            let i = py * PIXELS + px;
            ground[i] = column.ground();
            paint[i] = match column.body() {
                _ if atlas_water => Paint::Atlas,
                Some(WaterBody::Pond) => Paint::Pond,
                _ => Paint::Land,
            };
        }
    }

    let mut rgba = vec![0u8; PIXELS * PIXELS * 4];
    let sun = Vec2::new(-0.6, -0.8).normalize();
    for py in 0..PIXELS {
        for px in 0..PIXELS {
            let i = py * PIXELS + px;
            let at = |x: usize, z: usize| ground[z.min(PIXELS - 1) * PIXELS + x.min(PIXELS - 1)];
            let slope = Vec2::new(
                at(px + 1, py) - at(px.saturating_sub(1), py),
                at(px, py + 1) - at(px, py.saturating_sub(1)),
            ) / (2.0 * step as f32);
            let light = (0.62 - slope.dot(sun) * 1.6).clamp(0.18, 1.0);
            let shade = |c: [f32; 3]| {
                [
                    (c[0] * light * 255.0) as u8,
                    (c[1] * light * 255.0) as u8,
                    (c[2] * light * 255.0) as u8,
                    255,
                ]
            };
            let colour = match paint[i] {
                Paint::Pond => [40, 120, 220, 255],
                Paint::Atlas => shade([0.16, 0.32, 0.52]),
                Paint::Land => shade([0.36, 0.42, 0.26]),
            };
            rgba[i * 4..i * 4 + 4].copy_from_slice(&colour);
        }
    }

    let path = match std::env::var("POND_MAP") {
        Ok(path) if path.trim().is_empty() => panic!("POND_MAP must not be empty"),
        Ok(path) => path,
        Err(std::env::VarError::NotPresent) => {
            "C:/Projekte/OrrunWithEngine/shots/pond-map.png".to_string()
        }
        Err(error) => panic!("POND_MAP is not valid Unicode: {error}"),
    };
    save_rgba8_png(&path, PIXELS as u32, PIXELS as u32, &rgba).expect("write the map");
    eprintln!("wrote {path} at {:.1} m per pixel", step);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_values_are_rejected() {
        assert!(parse_args_from(&["seed".into()], None)
            .unwrap_err()
            .contains("invalid seed"));
        assert!(parse_args_from(&["1".into(), "31".into()], None)
            .unwrap_err()
            .contains("32..=512"));
        assert!(
            parse_args_from(&["1".into(), "64".into(), "NaN".into()], Some(64_000.0))
                .unwrap_err()
                .contains("finite")
        );
        assert!(parse_args_from(
            &["1".into(), "64".into(), "1".into(), "2".into(), "0".into()],
            Some(64_000.0)
        )
        .unwrap_err()
        .contains("positive"));
    }

    #[test]
    fn absent_values_use_defaults() {
        let args = parse_args_from(&[], Some(256_000.0)).expect("defaults");
        assert_eq!(args.seed, 20260809);
        assert_eq!(args.size, 256);
        assert_eq!(args.centre, GlobalXZ::at(128_000.0, 128_000.0));
        assert_eq!(args.span_m, 3_000.0);
    }
}
