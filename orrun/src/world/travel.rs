//! Atlas flyover travel: phase clock, continent proxy, and camera script.
//!
//! The streamed world only reaches tens of kilometres. A trip across the
//! continent therefore lifts out of the local view, crosses a low-resolution
//! proxy of the whole landmass, and descends once the destination ring is
//! actually resident. The proxy is sampled from [`ContinentalSurface`] so the
//! silhouette matches the walked landform, not the pre-hydro atlas loft.

use engine::camera::Camera;
use engine::color::Color;
use engine::mesh::Mesh;
use engine::space::{GlobalPosition, GlobalXZ};
use engine::EngineResult;

use super::surface::ContinentalSurface;
use crate::atlas::Biome;

/// How much the proxy stretches height so relief reads from orbit.
pub const PROXY_EXAGGERATION: f32 = 10.0;
/// Hard cap on the proxy lattice. A 256 km continent is one vertex per cell;
/// a larger atlas is downsampled rather than uploading a million-triangle mesh.
pub const MAX_PROXY_AXIS: usize = 256;
/// Default first-person field of view, restored on landing.
pub const WORLD_FOV_DEGREES: f32 = 55.0;

/// One cinematic beat of an atlas trip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TravelPhase {
    /// Rising out of the source stand. Source terrain is still resident.
    Ascent,
    /// Crossing the continent proxy toward the destination.
    Transfer,
    /// Holding above the destination until the streamed ring is ready.
    Hold,
    /// Descending into the resident destination stand.
    Descent,
}

impl TravelPhase {
    pub fn name(self) -> &'static str {
        match self {
            Self::Ascent => "ascent",
            Self::Transfer => "transfer",
            Self::Hold => "hold",
            Self::Descent => "descent",
        }
    }
}

impl std::fmt::Display for TravelPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// Durations of the scripted beats. Hold has no duration: it lasts until the
/// destination is ready.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TravelTimings {
    pub ascent_s: f32,
    pub transfer_s: f32,
    pub descent_s: f32,
}

impl TravelTimings {
    pub fn new(ascent_s: f32, transfer_s: f32, descent_s: f32) -> Self {
        for (name, seconds) in [
            ("ascent", ascent_s),
            ("transfer", transfer_s),
            ("descent", descent_s),
        ] {
            if !seconds.is_finite() || seconds < 0.0 {
                panic!("{name} duration must be a finite number of seconds ≥ 0, got {seconds}");
            }
        }
        Self {
            ascent_s,
            transfer_s,
            descent_s,
        }
    }

    /// Playable flyover. Long enough to read, short enough to skip.
    pub fn cinematic() -> Self {
        Self::new(2.4, 2.2, 2.6)
    }

    /// One frame per beat. Tests and headless tools use this so they wait on
    /// streaming, not on the camera script.
    pub fn instant() -> Self {
        Self::new(0.0, 0.0, 0.0)
    }

    pub fn duration_of(self, phase: TravelPhase) -> f32 {
        match phase {
            TravelPhase::Ascent => self.ascent_s,
            TravelPhase::Transfer => self.transfer_s,
            TravelPhase::Hold => f32::INFINITY,
            TravelPhase::Descent => self.descent_s,
        }
    }
}

/// First-person pose the ascent lifts out of.
#[derive(Clone, Copy, Debug)]
pub struct TravelSource {
    pub eye: GlobalPosition,
    pub yaw_degrees: f32,
    pub pitch_degrees: f32,
}

/// A look-from / look-at pair in global metres.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TravelLook {
    pub eye: GlobalPosition,
    pub target: GlobalPosition,
}

/// How the camera, haze, and field of view should read for one travel frame.
#[derive(Clone, Copy, Debug)]
pub struct TravelView {
    pub look: TravelLook,
    pub fov_y_degrees: f32,
    pub near_m: f32,
    pub view_distance_m: f32,
    pub haze_visibility_m: f32,
    /// 0 is a clear frame, 1 is a full speed/cloud veil.
    pub veil: f32,
}

/// CPU description of the continent proxy, built once per seed.
#[derive(Clone, Debug)]
pub struct ContinentProxySpec {
    cells: usize,
    axis: usize,
    cell_metres: f32,
    exaggeration: f32,
    sea_z: f32,
    heights: Vec<f32>,
    colors: Vec<Color>,
}

impl ContinentProxySpec {
    /// Sample the canonical surface onto a regular lattice.
    pub fn build(surface: &ContinentalSurface) -> Self {
        let cells = surface.bounds().size();
        let axis = cells.clamp(2, MAX_PROXY_AXIS);
        let cell_metres = surface.bounds().metres() as f32 / axis as f32;
        let sea_z = surface.sea_surface_z();
        let mut heights = Vec::with_capacity(axis * axis);
        let mut colors = Vec::with_capacity(axis * axis);
        for iz in 0..axis {
            for ix in 0..axis {
                let p = GlobalXZ::at(
                    (ix as f64 + 0.5) * f64::from(cell_metres),
                    (iz as f64 + 0.5) * f64::from(cell_metres),
                );
                let column = surface.column(p);
                let biome = surface.fields().biome_at(p.x as f32, p.z as f32);
                let (height, color) = if let Some(sheet) = column.water_top() {
                    (sheet, water_color(column.body(), biome))
                } else {
                    (column.ground(), land_color(biome, column.ground(), sea_z))
                };
                if !height.is_finite() {
                    panic!(
                        "continent proxy sample at ({:.0}, {:.0}) is not finite",
                        p.x, p.z
                    );
                }
                heights.push(height);
                colors.push(color);
            }
        }
        Self {
            cells,
            axis,
            cell_metres,
            exaggeration: PROXY_EXAGGERATION,
            sea_z,
            heights,
            colors,
        }
    }

    pub fn cells(&self) -> usize {
        self.cells
    }

    pub fn axis(&self) -> usize {
        self.axis
    }

    pub fn cell_metres(&self) -> f32 {
        self.cell_metres
    }

    pub fn exaggeration(&self) -> f32 {
        self.exaggeration
    }

    pub fn sea_z(&self) -> f32 {
        self.sea_z
    }

    pub fn extent_m(&self) -> f64 {
        f64::from(self.axis as f32 * self.cell_metres)
    }

    pub fn vertex_count(&self) -> usize {
        self.axis * self.axis
    }

    /// Display height of the proxy surface at `p`, including exaggeration.
    pub fn height_at(&self, p: GlobalXZ) -> f32 {
        let span = self.cell_metres;
        let fx = ((p.x as f32 / span) - 0.5).clamp(0.0, (self.axis - 1) as f32);
        let fz = ((p.z as f32 / span) - 0.5).clamp(0.0, (self.axis - 1) as f32);
        let x0 = fx.floor() as usize;
        let z0 = fz.floor() as usize;
        let x1 = (x0 + 1).min(self.axis - 1);
        let z1 = (z0 + 1).min(self.axis - 1);
        let tx = fx - x0 as f32;
        let tz = fz - z0 as f32;
        let h00 = self.heights[z0 * self.axis + x0];
        let h10 = self.heights[z0 * self.axis + x1];
        let h01 = self.heights[z1 * self.axis + x0];
        let h11 = self.heights[z1 * self.axis + x1];
        let h = h00 + (h10 - h00) * tx + (h01 - h00) * tz + (h11 - h10 - h01 + h00) * tx * tz;
        h * self.exaggeration
    }

    pub fn land_mesh(&self) -> EngineResult<Mesh> {
        let mut mesh = Mesh::new();
        let mut ids = Vec::with_capacity(self.vertex_count());
        for iz in 0..self.axis {
            for ix in 0..self.axis {
                let x = (ix as f32 + 0.5) * self.cell_metres;
                let z = (iz as f32 + 0.5) * self.cell_metres;
                let y = self.heights[iz * self.axis + ix] * self.exaggeration;
                let id = mesh.add_point((x, y, z))?;
                mesh.set_point_color(id, self.colors[iz * self.axis + ix])?;
                ids.push(id);
            }
        }
        for iz in 0..self.axis - 1 {
            for ix in 0..self.axis - 1 {
                let i00 = ids[iz * self.axis + ix];
                let i10 = ids[iz * self.axis + ix + 1];
                let i01 = ids[(iz + 1) * self.axis + ix];
                let i11 = ids[(iz + 1) * self.axis + ix + 1];
                mesh.add_quad(i00, i01, i11, i10)?;
            }
        }
        Ok(mesh)
    }

    pub fn sea_mesh(&self) -> EngineResult<Mesh> {
        let mut mesh = Mesh::new();
        let y = self.sea_z * self.exaggeration - 4.0;
        let pad = self.cell_metres;
        let max = self.axis as f32 * self.cell_metres;
        let a = mesh.add_point((-pad, y, -pad))?;
        let b = mesh.add_point((max + pad, y, -pad))?;
        let c = mesh.add_point((max + pad, y, max + pad))?;
        let d = mesh.add_point((-pad, y, max + pad))?;
        let water = Color::rgb(26, 56, 102);
        for id in [a, b, c, d] {
            mesh.set_point_color(id, water)?;
        }
        mesh.add_quad(a, d, c, b)?;
        Ok(mesh)
    }
}

/// Altitude that frames the whole continent in `fov_y_degrees`.
pub fn overview_altitude_m(extent_m: f64, fov_y_degrees: f32) -> f64 {
    if !extent_m.is_finite() || extent_m <= 0.0 {
        panic!("continent extent must be a positive finite length, got {extent_m}");
    }
    if !fov_y_degrees.is_finite() || fov_y_degrees <= 0.0 || fov_y_degrees >= 180.0 {
        panic!("overview FOV must be a finite angle in (0, 180), got {fov_y_degrees}");
    }
    let half = extent_m * 0.5;
    half / f64::from((fov_y_degrees * 0.5).to_radians().tan())
}

/// Far plane that keeps the continent and a margin of sky in view.
pub fn overview_view_distance_m(extent_m: f64, fov_y_degrees: f32) -> f32 {
    let altitude = overview_altitude_m(extent_m, fov_y_degrees);
    (altitude + extent_m * 0.75) as f32
}

pub fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn lerp_pos(a: GlobalPosition, b: GlobalPosition, t: f32) -> GlobalPosition {
    let t = f64::from(t);
    GlobalPosition::at(
        a.x + (b.x - a.x) * t,
        a.y + (b.y - a.y) * t,
        a.z + (b.z - a.z) * t,
    )
}

fn above(p: GlobalXZ, altitude: f64) -> GlobalPosition {
    GlobalPosition::at(p.x, altitude, p.z)
}

fn ground(p: GlobalXZ) -> GlobalPosition {
    GlobalPosition::at(p.x, 0.0, p.z)
}

/// Rise from the source eye to an overview above the same stand.
pub fn ascent_look(source: TravelSource, overview_alt: f64, t: f32) -> TravelLook {
    let t = smoothstep(t);
    let start_target = {
        let look = Camera::direction(source.yaw_degrees, source.pitch_degrees);
        GlobalPosition::at(
            source.eye.x + f64::from(look.x) * 24.0,
            source.eye.y + f64::from(look.y) * 24.0,
            source.eye.z + f64::from(look.z) * 24.0,
        )
    };
    let end_eye = above(source.eye.horizontal(), overview_alt);
    let end_target = ground(source.eye.horizontal());
    TravelLook {
        eye: lerp_pos(source.eye, end_eye, t),
        target: lerp_pos(start_target, end_target, t),
    }
}

/// Slide the overview camera from the source stand to the destination.
pub fn transfer_look(from: GlobalXZ, to: GlobalXZ, overview_alt: f64, t: f32) -> TravelLook {
    let t = smoothstep(t);
    TravelLook {
        eye: lerp_pos(above(from, overview_alt), above(to, overview_alt), t),
        target: lerp_pos(ground(from), ground(to), t),
    }
}

/// Gentle hover above the destination while the ring streams in.
pub fn hold_look(dest: GlobalXZ, overview_alt: f64, elapsed_s: f32) -> TravelLook {
    let yaw = elapsed_s * 0.12;
    let radius = overview_alt * 0.04;
    let eye = GlobalPosition::at(
        dest.x + f64::from(yaw.cos()) * radius,
        overview_alt,
        dest.z + f64::from(yaw.sin()) * radius,
    );
    TravelLook {
        eye,
        target: ground(dest),
    }
}

/// Drop from overview into the landing eye, looking where the walker will.
pub fn descent_look(
    landing_eye: GlobalPosition,
    heading_degrees: f32,
    overview_alt: f64,
    t: f32,
) -> TravelLook {
    let t = smoothstep(t);
    let start = above(landing_eye.horizontal(), overview_alt);
    let look = Camera::direction(heading_degrees, 0.0);
    let end_target = GlobalPosition::at(
        landing_eye.x + f64::from(look.x) * 18.0,
        landing_eye.y + f64::from(look.y) * 18.0,
        landing_eye.z + f64::from(look.z) * 18.0,
    );
    TravelLook {
        eye: lerp_pos(start, landing_eye, t),
        target: lerp_pos(ground(landing_eye.horizontal()), end_target, t),
    }
}

/// Camera script for one travel frame.
pub fn travel_view(
    phase: TravelPhase,
    t: f32,
    elapsed_s: f32,
    source: Option<TravelSource>,
    from: GlobalXZ,
    to: GlobalXZ,
    landing_eye: Option<GlobalPosition>,
    heading_degrees: f32,
    extent_m: f64,
) -> TravelView {
    let overview_fov = 48.0;
    let overview_alt = overview_altitude_m(extent_m, overview_fov);
    let overview_far = overview_view_distance_m(extent_m, overview_fov);
    let look = match phase {
        TravelPhase::Ascent => {
            let source = source.expect("ascent requires a source stand");
            ascent_look(source, overview_alt, t)
        }
        TravelPhase::Transfer => transfer_look(from, to, overview_alt, t),
        TravelPhase::Hold => hold_look(to, overview_alt, elapsed_s),
        TravelPhase::Descent => {
            let eye = landing_eye.expect("descent requires a landing eye");
            descent_look(eye, heading_degrees, overview_alt, t)
        }
    };
    let (fov, near, far, haze, veil) = match phase {
        TravelPhase::Ascent => {
            let t = smoothstep(t);
            (
                WORLD_FOV_DEGREES + 14.0 * t,
                0.1 + 8.0 * t,
                35_000.0 + (overview_far - 35_000.0) * t,
                12_000.0 * (1.0 - 0.92 * t),
                t,
            )
        }
        TravelPhase::Transfer => (overview_fov, 24.0, overview_far, overview_far * 0.55, 0.35),
        TravelPhase::Hold => (overview_fov, 24.0, overview_far, overview_far * 0.55, 0.22),
        TravelPhase::Descent => {
            let t = smoothstep(t);
            (
                overview_fov + (WORLD_FOV_DEGREES - overview_fov) * t,
                24.0 * (1.0 - t) + 0.1 * t,
                overview_far * (1.0 - t) + 35_000.0 * t,
                800.0 + 11_200.0 * t,
                1.0 - t,
            )
        }
    };
    TravelView {
        look,
        fov_y_degrees: fov,
        near_m: near,
        view_distance_m: far,
        haze_visibility_m: haze.max(80.0),
        veil,
    }
}

fn water_color(body: Option<super::surface::WaterBody>, biome: Biome) -> Color {
    match body {
        Some(super::surface::WaterBody::Lake { .. }) => Color::rgb(41, 97, 148),
        Some(super::surface::WaterBody::River { .. }) => Color::rgb(56, 118, 156),
        Some(super::surface::WaterBody::Pond) => Color::rgb(48, 110, 140),
        _ => {
            let rgb = biome.color_rgb();
            Color::rgb(rgb[0], rgb[1], rgb[2])
        }
    }
}

fn land_color(biome: Biome, ground: f32, sea: f32) -> Color {
    let rgb = biome.color_rgb();
    let lift = ((ground - sea) / 2_400.0).clamp(0.0, 1.0);
    Color::rgb(
        rgb[0].saturating_add((lift * 36.0) as u8),
        rgb[1].saturating_add((lift * 28.0) as u8),
        rgb[2].saturating_add((lift * 18.0) as u8),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atlas::ContinentAtlas;
    use crate::world::ContinentalSurface;
    use std::sync::Arc;

    fn surface(seed: i32, size: usize) -> Arc<ContinentalSurface> {
        let atlas = ContinentAtlas::generate(seed, size);
        Arc::new(ContinentalSurface::new(&atlas).expect("surface"))
    }

    #[test]
    fn overview_altitude_frames_the_continent() {
        let extent = 256_000.0;
        let alt = overview_altitude_m(extent, 48.0);
        assert!(
            alt > extent * 0.4,
            "altitude {alt} is too low for {extent} m"
        );
        assert!(alt < extent * 2.0, "altitude {alt} is needlessly high");
        let far = overview_view_distance_m(extent, 48.0);
        assert!(
            far > alt as f32,
            "the far plane must clear the overview eye"
        );
    }

    #[test]
    fn the_proxy_is_finite_and_covers_the_atlas() {
        let surface = surface(1, 32);
        let started = std::time::Instant::now();
        let spec = ContinentProxySpec::build(&surface);
        eprintln!(
            "proxy {}×{} ({} cells) built in {:?}",
            spec.axis(),
            spec.axis(),
            spec.cells(),
            started.elapsed()
        );
        assert_eq!(spec.cells(), 32);
        assert_eq!(spec.axis(), 32);
        assert_eq!(spec.vertex_count(), 32 * 32);
        assert!((spec.extent_m() - 32_000.0).abs() < 1.0);
        for &h in &spec.heights {
            assert!(h.is_finite(), "proxy height {h} is not finite");
        }
        let land = spec.land_mesh().expect("land");
        assert_eq!(land.point_count(), spec.vertex_count());
        assert!(land.face_count() > 0);
        let sea = spec.sea_mesh().expect("sea");
        assert_eq!(sea.point_count(), 4);
        let mid = spec.height_at(GlobalXZ::at(16_000.0, 16_000.0));
        assert!(mid.is_finite());
    }

    #[test]
    fn proxy_colors_are_deterministic() {
        let surface = surface(1, 32);
        let a = ContinentProxySpec::build(&surface);
        let b = ContinentProxySpec::build(&surface);
        assert_eq!(a.colors, b.colors);
        assert_eq!(a.heights, b.heights);
    }

    #[test]
    fn camera_paths_are_finite_and_continuous() {
        let source = TravelSource {
            eye: GlobalPosition::at(1_000.0, 12.0, 2_000.0),
            yaw_degrees: 40.0,
            pitch_degrees: -8.0,
        };
        let from = source.eye.horizontal();
        let to = GlobalXZ::at(20_000.0, 18_000.0);
        let landing = GlobalPosition::at(to.x, 14.0, to.z);
        let mut prev_ascent: Option<TravelLook> = None;
        let mut prev_transfer: Option<TravelLook> = None;
        let mut prev_descent: Option<TravelLook> = None;
        for i in 0..=20 {
            let t = i as f32 / 20.0;
            let ascent = ascent_look(source, 40_000.0, t);
            let transfer = transfer_look(from, to, 40_000.0, t);
            let descent = descent_look(landing, 90.0, 40_000.0, t);
            for look in [ascent, transfer, descent] {
                assert!(look.eye.x.is_finite() && look.eye.y.is_finite() && look.eye.z.is_finite());
                assert!(
                    look.target.x.is_finite()
                        && look.target.y.is_finite()
                        && look.target.z.is_finite()
                );
            }
            for (look, prev) in [
                (ascent, &mut prev_ascent),
                (transfer, &mut prev_transfer),
                (descent, &mut prev_descent),
            ] {
                if let Some(prev) = *prev {
                    let jump = (look.eye.x - prev.eye.x)
                        .hypot(look.eye.y - prev.eye.y)
                        .hypot(look.eye.z - prev.eye.z);
                    assert!(
                        jump < 8_000.0,
                        "camera jumped {jump} m between adjacent samples"
                    );
                }
                *prev = Some(look);
            }
        }
        let start = ascent_look(source, 40_000.0, 0.0);
        assert!((start.eye.x - source.eye.x).abs() < 1e-6);
        assert!((start.eye.z - source.eye.z).abs() < 1e-6);
        let end = ascent_look(source, 40_000.0, 1.0);
        assert!(end.eye.y > start.eye.y, "ascent must rise");
        let down0 = descent_look(landing, 90.0, 40_000.0, 0.0);
        let down1 = descent_look(landing, 90.0, 40_000.0, 1.0);
        assert!(down0.eye.y > down1.eye.y, "descent must fall");
        assert!((down1.eye.x - landing.x).abs() < 1e-6);
    }

    #[test]
    fn hold_keeps_the_destination_in_view() {
        let dest = GlobalXZ::at(12_000.0, 8_000.0);
        let look = hold_look(dest, 50_000.0, 3.5);
        assert!((look.target.x - dest.x).abs() < 1.0);
        assert!((look.target.z - dest.z).abs() < 1.0);
        assert!(look.eye.y > 1_000.0);
    }
}
