//! Crescent mountain belts stamped onto land elevation.

use engine::proc::Noise;
use rand::Rng;
use rand_chacha::ChaCha8Rng;
use rand::SeedableRng;

use super::landmask::collar_cells;
use super::{layer_seed, lerp, smoothstep};

pub fn apply_orogens(
    world_seed: i32,
    size: usize,
    land: &[u8],
    elev_code: &mut [u8],
    humidity: &mut [u8],
    relief: &mut [u8],
) {
    let count = size * size;
    let mut dist = vec![f32::INFINITY; count];
    let mut pass_field = vec![0.0f32; count];

    let mut rng = ChaCha8Rng::seed_from_u64(u64::from(layer_seed(world_seed, "atlas_orogens")));
    let belt_count = if size < 500 { 1 } else { 2 };
    for belt in 0..belt_count {
        stamp_orogen_arc(
            world_seed,
            size,
            &mut dist,
            &mut pass_field,
            &mut rng,
            belt,
        );
    }

    let core_r = 6.0f32.max(size as f32 * 0.024);
    let near_r = 12.0f32.max(size as f32 * 0.045);
    let foot_r = 22.0f32.max(size as f32 * 0.085);
    let massif_n = Noise::new(layer_seed(world_seed, "atlas_orogen_massif"));
    let valley_n = Noise::new(layer_seed(world_seed, "atlas_orogen_valley"));
    let peak_n = Noise::new(layer_seed(world_seed, "atlas_orogen_crest"));

    for i in 0..count {
        if land[i] == 0 {
            continue;
        }
        let d = dist[i];
        if d >= foot_r {
            continue;
        }
        let pass_u = pass_field[i].clamp(0.0, 1.0);
        let loft = if d < core_r {
            lerp(0.78, 1.0, 1.0 - d / core_r)
        } else if d < near_r {
            lerp(
                0.48,
                0.78,
                1.0 - (d - core_r) / (near_r - core_r).max(0.001),
            )
        } else {
            lerp(
                0.10,
                0.48,
                1.0 - (d - near_r) / (foot_r - near_r).max(0.001),
            )
        };
        let loft = lerp(loft, loft * 0.28, pass_u);

        let cx = (i % size) as f32;
        let cz = (i / size) as f32;
        let mut massif = massif_n.ridged2(cx * 0.085, cz * 0.085, 4, 2.0, 0.5) * 0.5 + 0.5;
        massif = massif.powf(1.28);
        let dissect = valley_n.fbm2(cz * 1.15 * 0.05, cx * 0.92 * 0.05, 3, 2.0, 0.5);
        let mut needle = peak_n.ridged2(cx * 1.35 * 0.16, cz * 1.35 * 0.16, 3, 2.0, 0.5) * 0.5 + 0.5;
        needle = needle.powf(1.65);

        let belt_w = smoothstep(0.08, 0.55, loft);
        let undulation = (massif - 0.48) * 0.50 + dissect * 0.20;
        let loft_h = (loft + undulation * belt_w).clamp(0.05, 1.25);

        let mut code_f = lerp(118.0, 168.0, (loft_h / 0.45).clamp(0.0, 1.0));
        if loft_h > 0.45 {
            code_f = lerp(168.0, 208.0, ((loft_h - 0.45) / 0.28).clamp(0.0, 1.0));
        }
        if loft_h > 0.70 {
            let peak_t =
                ((loft_h - 0.70) / 0.40).clamp(0.0, 1.0) * needle * lerp(0.55, 1.0, massif);
            code_f = lerp(208.0, 252.0, peak_t);
        }

        if loft > 0.35 {
            let incision = (1.0 - massif)
                * smoothstep(0.10, -0.45, dissect)
                * lerp(6.0, 48.0, loft);
            code_f -= incision;
        }

        let target = (code_f as i32).clamp(33, 255) as u8;
        if target > elev_code[i] {
            elev_code[i] = target;
        }

        let variance = undulation.abs() * belt_w + needle * loft;
        let rel_boost = (lerp(12.0, 58.0, loft) * (1.0 - pass_u * 0.55) + variance * 18.0) as i32;
        relief[i] = relief[i].max(rel_boost.clamp(0, 63) as u8);
        let dry = lerp(1.0, 0.55, loft * (1.0 - pass_u * 0.45));
        humidity[i] = ((humidity[i] as f32 * dry) as i32).clamp(0, 255) as u8;
    }
}

fn stamp_orogen_arc(
    world_seed: i32,
    size: usize,
    dist: &mut [f32],
    pass_field: &mut [f32],
    rng: &mut ChaCha8Rng,
    belt: i32,
) {
    let collar = collar_cells(size) as f32 + 4.0;
    let mut cx = lerp(size as f32 * 0.38, size as f32 * 0.62, rng.gen());
    let mut cz = lerp(size as f32 * 0.38, size as f32 * 0.62, rng.gen());
    if belt > 0 {
        cx = (cx + size as f32 * lerp(-0.18, 0.18, rng.gen())).clamp(collar, size as f32 - collar);
        cz = (cz + size as f32 * lerp(-0.18, 0.18, rng.gen())).clamp(collar, size as f32 - collar);
    }
    let radius = size as f32 * lerp(0.32, 0.44, rng.gen());
    let angle0 = rng.gen::<f32>() * std::f32::consts::TAU;
    let span = lerp(std::f32::consts::PI * 0.70, std::f32::consts::PI * 1.05, rng.gen());
    let steps = 32.max((radius * span * 1.15) as i32);
    let foot_r = 22.0f32.max(size as f32 * 0.085);
    let pass_name = format!("atlas_orogen_pass_{belt}");
    let warp_name = format!("atlas_orogen_warp_{belt}");
    let pass_n = Noise::new(layer_seed(world_seed, &pass_name));
    let warp_n = Noise::new(layer_seed(world_seed, &warp_name));

    let mut prev: Option<(f32, f32)> = None;
    for s in 0..=steps {
        let u = s as f32 / steps as f32;
        let ang = angle0 + span * u;
        let rad = radius
            * (1.0
                + warp_n.fbm2(ang * 3.0 * 0.03, belt as f32 * 7.0 * 0.03, 3, 2.0, 0.5) * 0.08);
        let mut px = cx + ang.cos() * rad;
        let mut pz = cz + ang.sin() * rad;
        px = px.clamp(collar, size as f32 - 1.0 - collar);
        pz = pz.clamp(collar, size as f32 - 1.0 - collar);
        let mut pass_amt = 0.0;
        let pn = pass_n.fbm2(u * 40.0 * 0.11, belt as f32 * 11.0 * 0.11, 2, 2.0, 0.5);
        if pn > 0.28 {
            pass_amt = smoothstep(0.28, 0.72, pn);
        }
        if let Some(p) = prev {
            stamp_orogen_segment(size, dist, pass_field, p, (px, pz), foot_r, pass_amt);
        }
        prev = Some((px, pz));
    }
}

fn stamp_orogen_segment(
    size: usize,
    dist: &mut [f32],
    pass_field: &mut [f32],
    a: (f32, f32),
    b: (f32, f32),
    foot_r: f32,
    pass_amt: f32,
) {
    let pad = foot_r + 2.0;
    let min_x = ((a.0.min(b.0) - pad).floor() as i32).clamp(0, size as i32 - 1);
    let max_x = ((a.0.max(b.0) + pad).ceil() as i32).clamp(0, size as i32 - 1);
    let min_z = ((a.1.min(b.1) - pad).floor() as i32).clamp(0, size as i32 - 1);
    let max_z = ((a.1.max(b.1) + pad).ceil() as i32).clamp(0, size as i32 - 1);
    let ab = (b.0 - a.0, b.1 - a.1);
    let ab_len_sq = ab.0 * ab.0 + ab.1 * ab.1;
    for az in min_z..=max_z {
        for ax in min_x..=max_x {
            let p = (ax as f32 + 0.5, az as f32 + 0.5);
            let t = if ab_len_sq > 0.0001 {
                (((p.0 - a.0) * ab.0 + (p.1 - a.1) * ab.1) / ab_len_sq).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let closest = (a.0 + ab.0 * t, a.1 + ab.1 * t);
            let d = (p.0 - closest.0).hypot(p.1 - closest.1);
            if d > foot_r + 1.0 {
                continue;
            }
            let idx = az as usize * size + ax as usize;
            if d < dist[idx] {
                dist[idx] = d;
            }
            if pass_amt > 0.0 {
                let crest_w = 1.0 - (d / (foot_r * 0.45).max(1.0)).clamp(0.0, 1.0);
                pass_field[idx] = pass_field[idx].max(pass_amt * crest_w);
            }
        }
    }
}
