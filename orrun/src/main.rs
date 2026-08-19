//! Orrun — title, atlas map, and walkable continent in one process.
//!
//! Usage: `cargo run -p orrun -- [seed] [size]`
//!
//! `size` is the continent edge in km (32–1000). It defaults to `continent_size`
//! in settings (256 if unset). A CLI size overrides settings for that launch only.
//!
//! Title: the painted vista while the continent is charted and the opening
//! stand is streamed, then Start. Map (M in the world): drag to pan · scroll
//! to zoom · F fit · left click travels there · right click reveals a cell
//! overlay · C clears overlays · M returns to where you were standing · the
//! largest-town button enters at the biggest settlement · nearest-dungeon
//! travels to the closest mouth. World (first person):
//! click to look · Esc hands the mouse back · W/S walk · Q/E sidestep · A/D
//! turn · Shift sprint · F fly · Space jump · M summons the map · Esc with a
//! free cursor quits.
//!
//! Where the player stood is written on exit, per seed and size, and used as
//! the opening stand on the next launch. A first launch starts at the largest
//! settlement.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use engine::egui::{
    self, Align2, Color32, ColorImage, FontId, PointerButton, Pos2, Rect, Sense, Stroke,
    StrokeKind, TextureHandle, TextureOptions, Vec2,
};
use engine::prelude::*;
use orrun::atlas::cell_overlay::{AtlasCellOverlay, OverlayStore, WaterBodyKind};
use orrun::atlas::features::{edge_owner, Dir};
use orrun::atlas::pack;
use orrun::atlas::preview;
use orrun::atlas::types::{Endpoint, Link};
use orrun::atlas::{ContinentAtlas, EndpointKind, Kind, NodeKind, SIZE as MAX_CONTINENT_SIZE};
use orrun::save::{SaveError, SavedStand};
use orrun::controls::{is_reserved, Action};
use orrun::settings::{self, clamp_continent_size, Settings};
use orrun::world::{
    best_settlement_entry, install_daylight, install_materials, Ambience, AtlasBounds, AtlasCell,
    ContinentProxySpec, ContinentalSurface, Heading, Locomotion, MapPoint, PondField, SessionState,
    WorldEntryRequest, WorldSession,
};

const MIN_ZOOM: f32 = 0.15;
const MAX_ZOOM: f32 = 384.0;
const ZOOM_STEP: f32 = 1.15;
/// Per-cell overlay starts once a cell is large enough for several lines.
const DETAIL_CELL_PX: f32 = 56.0;

struct AtlasViewer {
    atlas: Arc<ContinentAtlas>,
    surface: Arc<ContinentalSurface>,
    bounds: AtlasBounds,
    pixels: Vec<u8>,
    texture: Option<TextureHandle>,
    /// Screen pixels per atlas cell.
    zoom: f32,
    /// Top-left of the map in panel coordinates.
    pan: Vec2,
    needs_fit: bool,
    overlays: OverlayStore,
    /// Exact point the player picked to walk to.
    selection: Option<MapPoint>,
    note: Option<String>,
    show_hamlets: bool,
    show_dungeons: bool,
}

impl AtlasViewer {
    fn new(atlas: Arc<ContinentAtlas>, surface: Arc<ContinentalSurface>) -> Self {
        let pixels = preview::biome_rgba(&atlas);
        let bounds = surface.bounds();
        Self {
            atlas,
            surface,
            bounds,
            pixels,
            texture: None,
            zoom: 1.0,
            pan: Vec2::ZERO,
            needs_fit: true,
            overlays: OverlayStore::default(),
            selection: None,
            note: None,
            show_hamlets: false,
            show_dungeons: false,
        }
    }

    fn point_panel(&self, panel_origin: Pos2, point: MapPoint) -> Pos2 {
        let (fx, fz) = point.fraction();
        let cell = point.cell();
        panel_origin
            + self
                .local_to_panel(Pos2::ZERO, cell.ax(), cell.az(), [fx, fz])
                .to_vec2()
    }

    fn ensure_texture(&mut self, ctx: &egui::Context) -> TextureHandle {
        if let Some(tex) = &self.texture {
            return tex.clone();
        }
        let size = [self.atlas.size, self.atlas.size];
        let image = ColorImage::from_rgba_unmultiplied(size, &self.pixels);
        let tex = ctx.load_texture("atlas_biome", image, TextureOptions::NEAREST);
        self.texture = Some(tex.clone());
        tex
    }

    fn fit(&mut self, panel: Vec2) {
        let map = self.atlas.size.max(1) as f32;
        let fit = panel.min_elem() / map * 0.92;
        self.zoom = fit.clamp(MIN_ZOOM, MAX_ZOOM);
        let map_px = Vec2::splat(map * self.zoom);
        self.pan = (panel - map_px) * 0.5;
        self.needs_fit = false;
    }

    fn zoom_at(&mut self, screen_in_panel: Pos2, factor: f32) {
        let before = (screen_in_panel.to_vec2() - self.pan) / self.zoom;
        self.zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        self.pan = screen_in_panel.to_vec2() - before * self.zoom;
    }

    /// Exact fractional map position under a panel-local point.
    fn screen_to_map(&self, screen_in_panel: Pos2) -> Option<MapPoint> {
        let map_pos = (screen_in_panel.to_vec2() - self.pan) / self.zoom;
        MapPoint::from_cell_units(self.bounds, map_pos.x as f64, map_pos.y as f64).ok()
    }

    fn screen_to_cell(&self, screen_in_panel: Pos2) -> Option<AtlasCell> {
        self.screen_to_map(screen_in_panel).map(|p| p.cell())
    }

    fn cell_to_panel(&self, ax: i32, az: i32) -> Pos2 {
        Pos2::new(
            self.pan.x + ax as f32 * self.zoom,
            self.pan.y + az as f32 * self.zoom,
        )
    }

    fn cell_centre_panel(&self, ax: i32, az: i32) -> Pos2 {
        self.cell_to_panel(ax, az) + Vec2::splat(self.zoom * 0.5)
    }

    fn map_to_panel(&self, map_x: f32, map_z: f32) -> Pos2 {
        Pos2::new(
            self.pan.x + map_x * self.zoom,
            self.pan.y + map_z * self.zoom,
        )
    }

    fn edge_port_panel(&self, edge_key: i32, port_id: i32) -> Pos2 {
        let (ox, oz, dir) = edge_owner(edge_key);
        let ports = if let Some(p) = self.atlas.river_ports.get(&edge_key) {
            p.as_slice()
        } else if let Some(p) = self.atlas.road_ports.get(&edge_key) {
            p.as_slice()
        } else {
            &[]
        };
        let mut t = 0.5_f32;
        for p in ports {
            if p.id == port_id {
                t = p.t;
                break;
            }
        }
        if ports.len() == 1 {
            t = ports[0].t;
        }
        let (mx, mz) = match dir {
            Dir::East => (ox as f32 + 1.0, oz as f32 + t),
            Dir::South => (ox as f32 + t, oz as f32 + 1.0),
            other => panic!("canonical edge dir must be EAST or SOUTH, got {other:?}"),
        };
        self.map_to_panel(mx, mz)
    }

    fn endpoint_panel(&self, ax: i32, az: i32, endpoint: Endpoint) -> Pos2 {
        match endpoint.kind {
            EndpointKind::EdgePort => self.edge_port_panel(endpoint.ref_id, endpoint.port_id),
            EndpointKind::Ocean | EndpointKind::Lake => {
                let idx = self.atlas.index_of(ax, az);
                if let Some(&down) = self.atlas.river_receiver.get(idx) {
                    if down >= 0 {
                        let size = self.atlas.size as i32;
                        let dx = (down % size) - ax;
                        let dz = (down / size) - az;
                        if dx.abs() + dz.abs() == 1 {
                            return self.map_to_panel(
                                ax as f32 + 0.5 + dx as f32 * 0.5,
                                az as f32 + 0.5 + dz as f32 * 0.5,
                            );
                        }
                    }
                }
                self.cell_centre_panel(ax, az)
            }
            EndpointKind::Node if endpoint.ref_id >= 0 => {
                if let Some(node) = self.atlas.nodes.iter().find(|n| n.id == endpoint.ref_id) {
                    self.cell_centre_panel(node.ax, node.az)
                } else {
                    self.cell_centre_panel(ax, az)
                }
            }
            EndpointKind::Node => self.cell_centre_panel(ax, az),
        }
    }

    fn draw_link_curve(
        &self,
        painter: &egui::Painter,
        panel_origin: Pos2,
        ax: i32,
        az: i32,
        link: &Link,
        color: Color32,
        width: f32,
    ) {
        let a = panel_origin + self.endpoint_panel(ax, az, link.a).to_vec2();
        let b = panel_origin + self.endpoint_panel(ax, az, link.b).to_vec2();
        let mid = panel_origin + self.cell_centre_panel(ax, az).to_vec2();
        let line_w = (width * self.zoom * 0.08).max(width);
        let stroke = Stroke::new(line_w, color);

        let dist2 = |p: Pos2, q: Pos2| (p - q).length_sq();
        let points: Vec<Pos2> =
            if dist2(a, b) < 0.25 || dist2(a, mid) < 0.25 || dist2(b, mid) < 0.25 {
                vec![a, b]
            } else {
                let steps = 6;
                (0..=steps)
                    .map(|step| {
                        let t = step as f32 / steps as f32;
                        let omt = 1.0 - t;
                        Pos2::new(
                            omt * omt * a.x + 2.0 * omt * t * mid.x + t * t * b.x,
                            omt * omt * a.y + 2.0 * omt * t * mid.y + t * t * b.y,
                        )
                    })
                    .collect()
            };
        for win in points.windows(2) {
            painter.line_segment([win[0], win[1]], stroke);
        }
    }

    fn draw_feature_overlays(&self, painter: &egui::Painter, panel_origin: Pos2) {
        let size = self.atlas.size as i32;
        let min_river_class = if self.zoom < 12.0 { 1 } else { 0 };

        for (&cell_idx, links) in &self.atlas.river_links {
            let ax = cell_idx % size;
            let az = cell_idx / size;
            for link in links {
                if link.feature_class < min_river_class {
                    continue;
                }
                let width = 1.0 + link.feature_class as f32 * 0.7;
                self.draw_link_curve(
                    painter,
                    panel_origin,
                    ax,
                    az,
                    link,
                    Color32::from_rgba_unmultiplied(64, 166, 242, 230),
                    width,
                );
            }
        }

        for (&cell_idx, links) in &self.atlas.road_links {
            let ax = cell_idx % size;
            let az = cell_idx / size;
            for link in links {
                let width = if link.feature_class == 0 { 1.2 } else { 0.8 };
                self.draw_link_curve(
                    painter,
                    panel_origin,
                    ax,
                    az,
                    link,
                    Color32::from_rgba_unmultiplied(217, 184, 89, 230),
                    width,
                );
            }
        }

        for node in &self.atlas.nodes {
            if matches!(node.kind, NodeKind::Settlement | NodeKind::Dungeon) {
                continue;
            }
            let p = panel_origin + self.cell_centre_panel(node.ax, node.az).to_vec2();
            let color = match node.kind {
                NodeKind::CoastalGate => Color32::from_rgb(242, 217, 89),
                NodeKind::LakeShore => Color32::from_rgb(102, 191, 242),
                NodeKind::Pass => Color32::from_rgb(191, 191, 204),
                NodeKind::ClaimReserved => Color32::from_rgb(217, 64, 179),
                NodeKind::Settlement | NodeKind::Dungeon => unreachable!(),
                NodeKind::Landmark => Color32::from_rgb(242, 102, 89),
            };
            let radius = (self.zoom * 0.18).max(2.0);
            painter.circle_filled(p, radius, color);
        }
    }

    fn draw_settlement_markers(&self, painter: &egui::Painter, panel_origin: Pos2) {
        if !self.show_hamlets {
            return;
        }
        for pin in self.surface.settlements() {
            let Ok(point) = MapPoint::from_global(self.bounds, pin.at) else {
                continue;
            };
            let p = self.point_panel(panel_origin, point);
            let mut radius = (self.zoom * 0.3).max(3.5);
            if pin.tier >= 2 {
                radius *= 1.15;
            }
            painter.circle_filled(
                p,
                radius * 1.6,
                Color32::from_rgba_unmultiplied(38, 23, 13, 191),
            );
            let color = match pin.tier {
                3 => Color32::from_rgb(255, 220, 140),
                2 => Color32::from_rgb(255, 240, 200),
                _ => Color32::from_rgb(255, 247, 217),
            };
            painter.circle_filled(p, radius, color);
            if self.zoom >= DETAIL_CELL_PX {
                painter.text(
                    p + Vec2::new(radius + 2.0, -radius),
                    Align2::LEFT_BOTTOM,
                    settlement_tier_name(pin.tier),
                    FontId::proportional(11.0),
                    Color32::from_rgb(255, 247, 217),
                );
            }
        }
    }

    fn draw_dungeon_markers(&self, painter: &egui::Painter, panel_origin: Pos2) {
        if !self.show_dungeons {
            return;
        }
        for pin in self.surface.dungeon_pins() {
            let Ok(point) = MapPoint::from_global(self.bounds, pin.at) else {
                continue;
            };
            let p = self.point_panel(panel_origin, point);
            let radius = (self.zoom * 0.22).max(3.0) + pin.tier as f32 * 0.4;
            painter.circle_filled(
                p,
                radius * 1.5,
                Color32::from_rgba_unmultiplied(20, 12, 8, 200),
            );
            painter.circle_filled(p, radius, Color32::from_rgb(140, 92, 64));
            painter.circle_stroke(
                p,
                radius,
                Stroke::new(1.2_f32, Color32::from_rgb(90, 58, 38)),
            );
            if self.zoom >= DETAIL_CELL_PX {
                painter.text(
                    p + Vec2::new(radius + 2.0, -radius),
                    Align2::LEFT_BOTTOM,
                    pin.tier_name(),
                    FontId::proportional(11.0),
                    Color32::from_rgb(210, 168, 128),
                );
            }
        }
    }

    fn hover_line(&self, cell: AtlasCell) -> String {
        let (ax, az) = (cell.ax(), cell.az());
        let packed = self.atlas.cell_at(ax, az);
        let biome = pack::biome(packed);
        let elev = pack::elevation(packed);
        let metres = pack::elevation_to_metres(elev);
        let idx = self.atlas.index_of(ax, az);
        let mass = self.atlas.landmass_id[idx];
        let rivers = self.atlas.links_in_cell(ax, az, Kind::River).len();
        let roads = self.atlas.links_in_cell(ax, az, Kind::Road).len();
        format!(
            "cell {ax},{az}  {}  elev {elev} ({metres}m)  hum {}  relief {}  pop {}  mass {mass}  rivers {rivers}  roads {roads}",
            biome.name(),
            pack::humidity(packed),
            pack::relief(packed),
            pack::population(packed),
        )
    }

    fn reveal_overlay(&mut self, cell: AtlasCell) {
        let overlay = self.overlays.reveal(&self.atlas, &self.surface, cell);
        self.note = Some(overlay.summary.clone());
        eprintln!("{}", overlay.summary);
    }

    fn local_to_panel(&self, panel_origin: Pos2, ax: i32, az: i32, local: [f32; 2]) -> Pos2 {
        let origin = self.cell_to_panel(ax, az);
        Pos2::new(
            panel_origin.x + origin.x + local[0] * self.zoom,
            panel_origin.y + origin.y + local[1] * self.zoom,
        )
    }

    fn draw_overlays(&self, painter: &egui::Painter, panel_origin: Pos2) {
        for overlay in self.overlays.cells.values() {
            self.draw_one_overlay(painter, panel_origin, overlay);
        }
    }

    fn draw_one_overlay(
        &self,
        painter: &egui::Painter,
        panel_origin: Pos2,
        overlay: &AtlasCellOverlay,
    ) {
        let ax = overlay.ax();
        let az = overlay.az();
        let cell_origin = panel_origin + self.cell_to_panel(ax, az).to_vec2();
        let cell_rect = Rect::from_min_size(cell_origin, Vec2::splat(self.zoom));

        painter.rect_filled(cell_rect, 0.0, Color32::from_black_alpha(40));
        painter.rect_stroke(
            cell_rect,
            0.0,
            Stroke::new(2.0_f32, Color32::from_rgb(255, 220, 120)),
            StrokeKind::Inside,
        );

        if let Some(water) = &overlay.water {
            let res = water.res.max(1) as f32;
            let wet_fill = match water.kind {
                WaterBodyKind::Ocean => Color32::from_rgba_unmultiplied(35, 105, 195, 210),
                WaterBodyKind::Lake { .. } => Color32::from_rgba_unmultiplied(50, 135, 205, 215),
                WaterBodyKind::River => Color32::from_rgba_unmultiplied(45, 145, 225, 215),
            };
            let dry_fill = Color32::from_rgba_unmultiplied(118, 145, 88, 200);
            let step = self.zoom / res;
            for iz in 0..water.res {
                for ix in 0..water.res {
                    let wet = water.wet[iz * water.res + ix];
                    let p0 = self.local_to_panel(
                        panel_origin,
                        ax,
                        az,
                        [ix as f32 / res, iz as f32 / res],
                    );
                    painter.rect_filled(
                        Rect::from_min_size(p0, Vec2::splat(step + 0.5)),
                        0.0,
                        if wet { wet_fill } else { dry_fill },
                    );
                }
            }
            for line in &water.shore_lines {
                let pts: Vec<Pos2> = line
                    .iter()
                    .map(|p| self.local_to_panel(panel_origin, ax, az, *p))
                    .collect();
                for w in pts.windows(2) {
                    painter.line_segment(
                        [w[0], w[1]],
                        Stroke::new(3.0_f32, Color32::from_rgb(60, 210, 230)),
                    );
                }
            }
        }

        for river in &overlay.rivers {
            let pts: Vec<Pos2> = river
                .points
                .iter()
                .map(|p| self.local_to_panel(panel_origin, ax, az, *p))
                .collect();
            let width = (river.half_width_local * 2.0 * self.zoom).max(3.0);
            for w in pts.windows(2) {
                painter.line_segment(
                    [w[0], w[1]],
                    Stroke::new(width, Color32::from_rgba_unmultiplied(45, 145, 225, 220)),
                );
            }
            for w in pts.windows(2) {
                painter.line_segment(
                    [w[0], w[1]],
                    Stroke::new(1.5_f32, Color32::from_rgb(210, 245, 255)),
                );
            }
        }

        for road in &overlay.roads {
            let pts: Vec<Pos2> = road
                .points
                .iter()
                .map(|p| self.local_to_panel(panel_origin, ax, az, *p))
                .collect();
            let w = if road.class == 0 { 3.0_f32 } else { 2.0_f32 };
            for win in pts.windows(2) {
                painter.line_segment(
                    [win[0], win[1]],
                    Stroke::new(w, Color32::from_rgb(230, 190, 80)),
                );
            }
        }
    }

    fn draw_selection(&self, painter: &egui::Painter, panel_origin: Pos2) {
        let Some(point) = self.selection else {
            return;
        };
        let p = self.point_panel(panel_origin, point);
        let r = (self.zoom * 0.12).clamp(5.0, 18.0);
        painter.circle_stroke(p, r, Stroke::new(2.5_f32, Color32::from_rgb(255, 245, 200)));
        painter.line_segment(
            [Pos2::new(p.x - r * 1.6, p.y), Pos2::new(p.x + r * 1.6, p.y)],
            Stroke::new(1.5_f32, Color32::from_rgb(255, 245, 200)),
        );
        painter.line_segment(
            [Pos2::new(p.x, p.y - r * 1.6), Pos2::new(p.x, p.y + r * 1.6)],
            Stroke::new(1.5_f32, Color32::from_rgb(255, 245, 200)),
        );
    }

    /// Where the player is standing, so a summoned map is not a guess.
    fn draw_stand(&self, painter: &egui::Painter, panel_origin: Pos2, at: MapPoint) {
        let p = self.point_panel(panel_origin, at);
        let r = (self.zoom * 0.1).clamp(4.0, 14.0);
        painter.circle_filled(p, r, Color32::from_rgb(120, 200, 255));
        painter.circle_stroke(
            p,
            r + 2.0,
            Stroke::new(1.5_f32, Color32::from_rgb(20, 30, 40)),
        );
    }

    fn draw_cell_details(&self, painter: &egui::Painter, panel_origin: Pos2, panel_size: Vec2) {
        let top_left = -self.pan / self.zoom;
        let bottom_right = (panel_size - self.pan) / self.zoom;
        let size = self.atlas.size as i32;
        let x0 = top_left.x.floor().clamp(0.0, (size - 1) as f32) as i32;
        let z0 = top_left.y.floor().clamp(0.0, (size - 1) as f32) as i32;
        let x1 = bottom_right.x.ceil().clamp(0.0, size as f32) as i32;
        let z1 = bottom_right.y.ceil().clamp(0.0, size as f32) as i32;

        let font_size = (self.zoom * 0.16).clamp(11.0, 42.0);
        let line_step = font_size + 3.0;
        let pad = (self.zoom * 0.04).max(4.0);
        let font = FontId::proportional(font_size);
        let text_color = Color32::from_rgb(250, 250, 240);

        for az in z0..z1 {
            for ax in x0..x1 {
                let origin = panel_origin + self.cell_to_panel(ax, az).to_vec2();
                let cell_rect = Rect::from_min_size(origin, Vec2::splat(self.zoom));
                painter.rect_filled(cell_rect, 0.0, Color32::from_black_alpha(107));
                painter.rect_stroke(
                    cell_rect,
                    0.0,
                    Stroke::new(1.0_f32, Color32::from_white_alpha(46)),
                    StrokeKind::Inside,
                );

                let packed = self.atlas.cell_at(ax, az);
                let biome = pack::biome(packed);
                let elev = pack::elevation(packed);
                let metres = pack::elevation_to_metres(elev);
                let idx = self.atlas.index_of(ax, az);
                let lid = self.atlas.lake_id[idx];
                let mass = self.atlas.landmass_id[idx];
                let rivers = self.atlas.links_in_cell(ax, az, Kind::River).len();
                let roads = self.atlas.links_in_cell(ax, az, Kind::Road).len();
                let pop = pack::population(packed);

                let mut lines = vec![
                    biome.name().to_string(),
                    format!("e {elev} ({metres}m)"),
                    format!("h {}", pack::humidity(packed)),
                    format!("r {}", pack::relief(packed)),
                ];
                if lid >= 0 {
                    lines.push(format!("lake {lid}"));
                }
                if rivers > 0 || roads > 0 {
                    lines.push(format!("riv {rivers}  road {roads}"));
                }
                if pop > 0 {
                    lines.push(format!("pop {pop}"));
                }
                if mass >= 0 {
                    lines.push(format!("mass {mass}"));
                }

                let max_lines = ((self.zoom - pad * 2.0) / line_step).floor().max(1.0) as usize;
                let shown = lines.len().min(max_lines);
                for (i, line) in lines.iter().take(shown).enumerate() {
                    let pos = Pos2::new(
                        origin.x + pad,
                        origin.y + pad + (i as f32 + 1.0) * line_step - 2.0,
                    );
                    painter.text(pos, Align2::LEFT_BOTTOM, line, font.clone(), text_color);
                }
            }
        }
    }
}

fn parse_args(default_size: usize) -> (i32, usize) {
    let mut args = std::env::args().skip(1);
    let seed = args.next().and_then(|s| s.parse().ok()).unwrap_or(20260809);
    let size = args
        .next()
        .and_then(|s| s.parse().ok())
        .map(clamp_continent_size)
        .unwrap_or(default_size);
    (seed, size)
}

/// Where the title starts streaming before Start is pressed.
///
/// A remembered stand that no longer resolves is worth saying out loud: it
/// means entry and the saved world have drifted apart, and silently dropping
/// the player on the map would hide that. A first launch uses the largest
/// settlement — the same place the in-game map offers as a pin.
fn opening_entry(
    remembered: Option<SavedStand>,
    bounds: AtlasBounds,
    surface: &ContinentalSurface,
    ponds: &PondField,
) -> WorldEntryRequest {
    if let Some(stand) = remembered {
        let point = MapPoint::from_global(bounds, stand.at())
            .unwrap_or_else(|err| panic!("saved stand is outside this atlas: {err}"));
        let heading = Heading::from_degrees(stand.yaw_degrees).expect("a saved heading is finite");
        eprintln!("resuming at ({:.0} m, {:.0} m)", stand.x, stand.z);
        return WorldEntryRequest::at(point).facing(heading);
    }
    let (pin, request) = best_settlement_entry(surface, ponds).unwrap_or_else(|err| {
        panic!("this continent has no walkable settlement to start at: {err}")
    });
    if surface
        .largest_settlement()
        .is_some_and(|best| best.id != pin.id)
    {
        eprintln!(
            "starting at {} (pop {}); the largest {} is not walkable",
            settlement_tier_name(pin.tier),
            pin.population,
            settlement_tier_name(
                surface
                    .largest_settlement()
                    .expect("largest checked above")
                    .tier
            ),
        );
    } else {
        eprintln!(
            "starting at the largest {} (pop {})",
            settlement_tier_name(pin.tier),
            pin.population
        );
    }
    request
}

fn main() {
    let prefs = Settings::load().unwrap_or_else(|err| panic!("{err}"));
    let (seed, size) = parse_args(prefs.continent_size());
    // Read before the window opens. FORMAT 1 migrates. Garbage JSON is
    // skipped with a warning; other save errors still fail loud.
    let remembered = match SavedStand::read(seed, size) {
        Ok(stand) => stand,
        Err(SaveError::Unreadable { path, source }) => {
            eprintln!(
                "warning: save {} is not readable Orrun state ({source}); starting without it",
                path.display()
            );
            None
        }
        Err(err) => panic!("{err}"),
    };

    let status = Arc::new(Mutex::new(format!("Charting {size} km of continent…")));
    let status_job = Arc::clone(&status);
    eprintln!("generating atlas seed={seed} size={size}…");
    let mut generating: Option<
        JoinHandle<(
            Arc<ContinentAtlas>,
            Arc<ContinentalSurface>,
            ContinentProxySpec,
        )>,
    > = Some(
        std::thread::Builder::new()
            .name("atlas".into())
            .spawn(move || {
                let atlas = ContinentAtlas::generate(seed, size);
                *status_job.lock().expect("title status") = "Building continental terrain…".into();
                let surface = ContinentalSurface::new(&atlas).expect("canonical surface");
                *status_job.lock().expect("title status") = "Building travel proxy…".into();
                let proxy = ContinentProxySpec::build(&surface);
                (Arc::new(atlas), Arc::new(surface), proxy)
            })
            .expect("atlas thread"),
    );

    let mut viewer: Option<AtlasViewer> = None;
    let mut session: Option<WorldSession> = None;
    let mut ambience: Option<Ambience> = None;
    let mut title_art: Option<TextureHandle> = None;
    let mut on_title = true;
    let mut dressed = false;
    let mut settings_ui = SettingsUi {
        open: false,
        prefs,
        applied: false,
        active_continent_size: size,
        listening: None,
    };

    let last_stand = Arc::new(Mutex::new(remembered));
    let stand_in_loop = Arc::clone(&last_stand);

    Engine::run("Orrun", move |world, frame| {
        if frame.first {
            install_daylight(world);
            ambience = Some(Ambience::load().unwrap_or_else(|err| panic!("{err}")));
        }
        if !settings_ui.applied {
            apply_hitch_log(world, settings_ui.prefs.hitch_log, false);
            world.set_instance_submit(InstanceSubmit::GpuIndirect);
            settings_ui.applied = true;
        }
        apply_instance_submit_hotkeys(world, frame);

        if let Some(job) = generating.take() {
            if job.is_finished() {
                let (atlas, surface, proxy) = job.join().expect("atlas thread");
                eprintln!(
                    "ready: lakes={} rivers={} coasts={} nodes={} alpine_massifs={} hash={:#x}",
                    atlas.hydro.lakes.len(),
                    atlas.hydro.rivers.len(),
                    atlas.hydro.coasts.len(),
                    atlas.nodes.len(),
                    atlas.alpine_massifs.len(),
                    atlas.content_hash as u32,
                );
                *status.lock().expect("title status") = String::new();
                viewer = Some(AtlasViewer::new(Arc::clone(&atlas), Arc::clone(&surface)));
                let mut world_session = WorldSession::new(surface);
                world_session.attach_proxy(proxy);
                if let Some(stand) = remembered {
                    world_session.apply_save(&stand);
                }
                session = Some(world_session);
            } else {
                generating = Some(job);
            }
        }

        if !dressed {
            if let Some(session) = &session {
                let t = Instant::now();
                install_materials(world, seed, session.surface().sea_surface_z());
                world.hitch_span(
                    "materials",
                    t.elapsed().as_secs_f32() * 1000.0,
                    format!("seed={seed}"),
                );
                dressed = true;
            }
        }

        if on_title {
            if title_art.is_none() {
                title_art = Some(load_title_vista(frame.ui.ctx()));
            }
            let ready = viewer.is_some();
            if ready {
                let bounds = viewer.as_ref().expect("atlas ready").bounds;
                let session = session.as_mut().expect("atlas ready");
                if session.state() == SessionState::Atlas {
                    let ponds = session.ponds();
                    let request =
                        opening_entry(remembered, bounds, session.surface(), ponds.as_ref());
                    if let Err(err) = session.begin_entry(world, request) {
                        if remembered.is_some() {
                            eprintln!(
                                "saved stand is not walkable ({err}); starting at the largest town"
                            );
                            let fallback =
                                opening_entry(None, bounds, session.surface(), ponds.as_ref());
                            session.begin_entry(world, fallback).unwrap_or_else(|err| {
                                panic!("cannot open the starting stand: {err}")
                            });
                        } else {
                            panic!("cannot open the starting stand: {err}");
                        }
                    }
                }
                if session.state() == SessionState::Travel {
                    session
                        .update(world, frame)
                        .unwrap_or_else(|err| panic!("world session failed: {err}"));
                }
            }
            let line = if !ready {
                status.lock().expect("title status").clone()
            } else if session
                .as_ref()
                .is_some_and(|s| s.state() == SessionState::Travel)
            {
                session.as_ref().expect("atlas ready").loading_status()
            } else {
                String::new()
            };
            match draw_title(
                frame,
                title_art.as_ref().expect("title art"),
                seed,
                size,
                ready,
                &line,
            ) {
                TitleAction::Stay => {}
                TitleAction::Start => {
                    on_title = false;
                }
            }
            draw_settings(&mut settings_ui, world, frame);
            if let Some(ambience) = ambience.as_mut() {
                ambience
                    .silence(frame.dt)
                    .unwrap_or_else(|err| panic!("{err}"));
            }
            return;
        }

        let viewer = viewer
            .as_mut()
            .expect("left the title before the atlas was ready");
        let session = session
            .as_mut()
            .expect("left the title before the atlas was ready");

        session.set_key_binds(settings_ui.prefs.keys.clone());
        if let Err(err) = session.update(world, frame) {
            panic!("world session failed: {err}");
        }

        if let Some(stand) = session.saved_full(seed, size) {
            *stand_in_loop.lock().expect("last stand") = Some(stand);
        }

        match session.state() {
            SessionState::Atlas => draw_atlas(viewer, session, world, frame),
            SessionState::Travel => draw_travel(viewer, session, frame),
            SessionState::World => draw_world_hud(session, world, frame),
        }
        draw_settings(&mut settings_ui, world, frame);
        ambience
            .as_mut()
            .expect("audio opens with the window")
            .update(session, frame.dt)
            .unwrap_or_else(|err| panic!("{err}"));
    });

    let stand = *last_stand.lock().expect("last stand");
    if let Some(stand) = stand {
        let path = stand.write().unwrap_or_else(|err| panic!("{err}"));
        eprintln!(
            "saved ({:.0} m, {:.0} m) to {}",
            stand.x,
            stand.z,
            path.display()
        );
    }
}

enum TitleAction {
    Stay,
    Start,
}

fn load_title_vista(ctx: &egui::Context) -> TextureHandle {
    let path = title_vista_path();
    let (w, h, rgba) = load_rgba8_png(&path).unwrap_or_else(|err| panic!("{err}"));
    let image = ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
    ctx.load_texture("title_vista", image, TextureOptions::LINEAR)
}

fn title_vista_path() -> PathBuf {
    let mut tried = Vec::new();
    if let Some(dir) = std::env::var_os("ORRUN_ASSETS") {
        tried.push(PathBuf::from(dir).join("title").join("vista.png"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            tried.push(dir.join("assets").join("title").join("vista.png"));
        }
    }
    tried.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join("title")
            .join("vista.png"),
    );
    tried
        .into_iter()
        .find(|p| p.is_file())
        .unwrap_or_else(|| panic!("title vista missing; expected orrun/assets/title/vista.png"))
}

fn paint_vista_backdrop(ui: &egui::Ui, rect: Rect, art: &TextureHandle) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, Color32::BLACK);
    paint_cover(&painter, rect, art);
    paint_edge_fade(&painter, rect, true);
    paint_edge_fade(&painter, rect, false);
}

fn paint_cover(painter: &egui::Painter, rect: Rect, texture: &TextureHandle) {
    let size = texture.size_vec2();
    let scale = (rect.width() / size.x).max(rect.height() / size.y);
    let drawn = size * scale;
    let origin = rect.center() - drawn * 0.5;
    painter.image(
        texture.id(),
        Rect::from_min_size(origin, drawn),
        Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        Color32::WHITE,
    );
}

fn paint_edge_fade(painter: &egui::Painter, rect: Rect, from_top: bool) {
    let band = rect.height() * 0.16;
    for i in 0..8 {
        let t = i as f32 / 8.0;
        let (y0, y1) = if from_top {
            (
                rect.min.y + band * t,
                rect.min.y + band * ((i + 1) as f32 / 8.0),
            )
        } else {
            let fade_top = rect.max.y - band;
            (
                fade_top + band * t,
                fade_top + band * ((i + 1) as f32 / 8.0),
            )
        };
        let alpha = if from_top {
            (90.0 * (1.0 - t)) as u8
        } else {
            (28.0 + t * 110.0) as u8
        };
        painter.rect_filled(
            Rect::from_min_max(egui::pos2(rect.min.x, y0), egui::pos2(rect.max.x, y1)),
            0.0,
            Color32::from_black_alpha(alpha),
        );
    }
}

fn draw_title(
    frame: &Frame,
    art: &TextureHandle,
    seed: i32,
    size: usize,
    ready: bool,
    status: &str,
) -> TitleAction {
    let ctx = frame.ui.ctx().clone();
    let cream = Color32::from_rgb(235, 230, 210);
    let mute = Color32::from_rgb(168, 186, 204);
    let mut action = TitleAction::Stay;

    egui::CentralPanel::default()
        .frame(egui::Frame::NONE)
        .show(&ctx, |ui| {
            let rect = ui.max_rect();
            paint_vista_backdrop(ui, rect, art);

            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                ui.label(
                    egui::RichText::new("ORRUN")
                        .size(72.0)
                        .color(cream)
                        .strong(),
                );
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new("a continent to walk")
                        .size(20.0)
                        .italics()
                        .color(mute),
                );
                ui.add_space(16.0);
                ui.label(
                    egui::RichText::new(format!("seed {seed}   ·   {size} km"))
                        .size(14.0)
                        .color(Color32::from_rgb(140, 158, 176)),
                );
                if !status.is_empty() {
                    ui.add_space(18.0);
                    ui.label(egui::RichText::new(status).size(16.0).color(mute));
                } else if !ready {
                    ui.add_space(18.0);
                    ui.label(
                        egui::RichText::new("Shaping the land…")
                            .size(16.0)
                            .color(mute),
                    );
                }
            });

            if ready {
                let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                egui::Area::new(egui::Id::new("title_actions"))
                    .anchor(Align2::CENTER_BOTTOM, [0.0, -28.0])
                    .order(egui::Order::Foreground)
                    .show(ui.ctx(), |ui| {
                        ui.vertical_centered(|ui| {
                            if title_button(ui, "Start").clicked() || enter {
                                action = TitleAction::Start;
                            }
                        });
                    });
            }
        });

    action
}

fn title_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(
            egui::RichText::new(text)
                .size(18.0)
                .color(Color32::from_rgb(235, 230, 210)),
        )
        .fill(Color32::from_rgba_unmultiplied(10, 14, 20, 170))
        .stroke(egui::Stroke::new(1.0_f32, Color32::from_rgb(168, 186, 204)))
        .min_size(egui::vec2(240.0, 40.0)),
    )
}

fn draw_atlas(
    viewer: &mut AtlasViewer,
    session: &mut WorldSession,
    world: &mut World,
    frame: &Frame,
) {
    let ctx = frame.ui.ctx().clone();
    let texture = viewer.ensure_texture(&ctx);

    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(Color32::from_rgb(12, 14, 18)))
        .show(&ctx, |ui| {
            let mut go_largest = false;
            let mut go_dungeon = false;
            let travel_btns = egui::Area::new(egui::Id::new("atlas_travel_btns"))
                .anchor(Align2::RIGHT_TOP, [-12.0, 12.0])
                .order(egui::Order::Foreground)
                .show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style())
                        .inner_margin(egui::Margin::same(8))
                        .show(ui, |ui| {
                            ui.vertical(|ui| {
                                if ui.button("Go to largest town").clicked() {
                                    go_largest = true;
                                }
                                if ui.button("Go to nearest dungeon").clicked() {
                                    go_dungeon = true;
                                }
                            });
                        });
                });
            let btn_hit = travel_btns.response.contains_pointer();

            let panel = ui.available_size();
            if viewer.needs_fit {
                viewer.fit(panel);
            }
            if ui.input(|i| i.key_pressed(egui::Key::F)) {
                viewer.fit(panel);
            }

            let (rect, response) =
                ui.allocate_exact_size(panel, Sense::click_and_drag() | Sense::click());

            if !btn_hit && response.dragged_by(PointerButton::Primary) {
                viewer.pan += response.drag_delta();
            }

            let scroll_y = ui.input(|i| i.raw_scroll_delta.y);
            if !btn_hit && response.hovered() && scroll_y != 0.0 {
                let factor = if scroll_y > 0.0 {
                    ZOOM_STEP
                } else {
                    1.0 / ZOOM_STEP
                };
                let pivot = ui
                    .input(|i| i.pointer.hover_pos())
                    .unwrap_or_else(|| rect.center());
                let local = Pos2::new(pivot.x - rect.min.x, pivot.y - rect.min.y);
                viewer.zoom_at(local, factor);
            }

            if ui.input(|i| i.key_pressed(egui::Key::C)) {
                viewer.overlays.clear();
                viewer.note = Some("cleared cell overlays".into());
            }

            // Widget click, not a global mouse-up: releasing after a pan, or
            // clicking Settings / the HUD, used to plant a marker and then
            // fail to travel.
            if !btn_hit {
                if response.clicked() {
                    if let Some(local) = response.interact_pointer_pos().map(|p| {
                        Pos2::new(p.x - rect.min.x, p.y - rect.min.y)
                    }) {
                        if let Some(point) = viewer.screen_to_map(local) {
                            viewer.selection = Some(point);
                            travel_to(viewer, session, world);
                        }
                    }
                }
                if response.secondary_clicked() {
                    if let Some(local) = response.interact_pointer_pos().map(|p| {
                        Pos2::new(p.x - rect.min.x, p.y - rect.min.y)
                    }) {
                        if let Some(cell) = viewer.screen_to_cell(local) {
                            viewer.reveal_overlay(cell);
                        }
                    }
                }
            }

            if go_largest {
                travel_to_largest_town(viewer, session, world);
            }
            if go_dungeon {
                travel_to_nearest_dungeon(viewer, session, world);
            }

            // Enter still travels to the last pick, and M puts a summoned map
            // away again without moving anybody.
            if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                travel_to(viewer, session, world);
            }
            if ui.input(|i| i.key_pressed(egui::Key::M)) && session.spawn().is_some() {
                session.resume().expect("resume a loaded world");
            }

            let hover_cell = ui
                .input(|i| i.pointer.hover_pos())
                .filter(|_| response.hovered())
                .and_then(|p| {
                    let local = Pos2::new(p.x - rect.min.x, p.y - rect.min.y);
                    viewer.screen_to_cell(local)
                });

            let map = viewer.atlas.size as f32;
            let image_rect =
                Rect::from_min_size(rect.min + viewer.pan, Vec2::splat(map * viewer.zoom));
            let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
            let painter = ui.painter().with_clip_rect(rect);
            painter.image(texture.id(), image_rect, uv, Color32::WHITE);

            if viewer.zoom >= DETAIL_CELL_PX {
                viewer.draw_cell_details(&painter, rect.min, panel);
            }
            viewer.draw_feature_overlays(&painter, rect.min);
            viewer.draw_settlement_markers(&painter, rect.min);
            viewer.draw_dungeon_markers(&painter, rect.min);
            viewer.draw_overlays(&painter, rect.min);
            if let Some(stand) = session
                .player_position()
                .and_then(|p| MapPoint::from_global(viewer.bounds, GlobalXZ::at(p.x, p.z)).ok())
            {
                viewer.draw_stand(&painter, rect.min, stand);
            }
            viewer.draw_selection(&painter, rect.min);

            let status = format!(
                "seed {}  |  {} km  |  zoom {:.1} px/cell  |  overlays {}  |  LMB travel  |  drag pan  |  RMB overlay  |  C clear{}",
                viewer.atlas.world_seed,
                viewer.atlas.size,
                viewer.zoom,
                viewer.overlays.len(),
                if session.spawn().is_some() {
                    "  |  M back to where you stood"
                } else {
                    ""
                },
            );
            let hover_text = hover_cell.map(|cell| {
                let base = viewer.hover_line(cell);
                if viewer.overlays.contains(cell.ax(), cell.az()) {
                    format!("{base}  |  OVERLAY ON")
                } else {
                    format!("{base}  |  RMB → cell overlay")
                }
            });

            egui::Area::new(egui::Id::new("atlas_hud"))
                .fixed_pos(egui::pos2(12.0, 12.0))
                .order(egui::Order::Foreground)
                .show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style())
                        .inner_margin(egui::Margin::same(8))
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new(&status).size(14.0));
                            if let Some(line) = &hover_text {
                                ui.label(
                                    egui::RichText::new(line)
                                        .size(13.0)
                                        .color(Color32::from_rgb(217, 224, 179)),
                                );
                            }
                            if let Some(note) = &viewer.note {
                                ui.label(
                                    egui::RichText::new(note)
                                        .size(12.0)
                                        .color(Color32::from_rgb(120, 200, 255)),
                                );
                            }
                            if let Some(dungeon) = session.dungeon_build_status() {
                                ui.label(
                                    egui::RichText::new(dungeon)
                                        .size(13.0)
                                        .color(Color32::from_rgb(210, 168, 128)),
                                );
                            }
                            ui.add_space(4.0);
                            ui.checkbox(&mut viewer.show_hamlets, "Show hamlets");
                            ui.checkbox(&mut viewer.show_dungeons, "Show dungeons");
                        });
                });
        });
}

/// Travel to the picked point, entering the world or moving across it.
///
/// The same call serves first entry and a jump from a map summoned mid-walk:
/// entry resolves the nearest standable ground and streams it, so there is no
/// second teleport path that could land somewhere the walker cannot stand.
fn travel_to(viewer: &mut AtlasViewer, session: &mut WorldSession, world: &mut World) {
    let Some(point) = viewer.selection else {
        viewer.note = Some("click a spot on the map to travel there".into());
        return;
    };
    match session.begin_entry(world, WorldEntryRequest::at(point)) {
        Ok(()) => {
            let g = point.to_global();
            viewer.note = Some(format!("travelling to ({:.0} m, {:.0} m)", g.x, g.z));
            eprintln!("{}", viewer.note.as_deref().unwrap_or_default());
        }
        Err(err) => {
            viewer.note = Some(format!("cannot land there: {err}"));
            eprintln!("entry refused: {err}");
        }
    }
}

fn travel_to_nearest_dungeon(
    viewer: &mut AtlasViewer,
    session: &mut WorldSession,
    world: &mut World,
) {
    let from = session
        .player_position()
        .map(|p| GlobalXZ::at(p.x, p.z))
        .or_else(|| viewer.selection.map(|point| point.to_global()))
        .unwrap_or_else(|| {
            let half = viewer.bounds.metres() * 0.5;
            GlobalXZ::at(half, half)
        });
    let Some(pin) = viewer.surface.nearest_dungeon(from) else {
        viewer.note = Some("this continent has no dungeons".into());
        return;
    };
    let point = match MapPoint::from_global(viewer.bounds, pin.at) {
        Ok(point) => point,
        Err(err) => {
            viewer.note = Some(format!("nearest dungeon is off the map: {err}"));
            return;
        }
    };
    viewer.selection = Some(point);
    match session.begin_entry(world, WorldEntryRequest::at(point)) {
        Ok(()) => {
            viewer.note = Some(format!(
                "travelling to the nearest {} dungeon",
                pin.tier_name()
            ));
            eprintln!("{}", viewer.note.as_deref().unwrap_or_default());
        }
        Err(err) => {
            viewer.note = Some(format!("cannot land at the nearest dungeon: {err}"));
            eprintln!("entry refused: {err}");
        }
    }
}

fn travel_to_largest_town(viewer: &mut AtlasViewer, session: &mut WorldSession, world: &mut World) {
    let ponds = session.ponds();
    let (pin, request) = match best_settlement_entry(viewer.surface.as_ref(), ponds.as_ref()) {
        Ok(found) => found,
        Err(err) => {
            viewer.note = Some(format!("no settlement has walkable ground: {err}"));
            eprintln!("entry refused: {err}");
            return;
        }
    };
    let point = request.point();
    viewer.selection = Some(point);
    match session.begin_entry(world, request) {
        Ok(()) => {
            let note = if viewer
                .surface
                .largest_settlement()
                .is_some_and(|best| best.id != pin.id)
            {
                format!(
                    "travelling to {} (pop {}); the largest port is not walkable",
                    settlement_tier_name(pin.tier),
                    pin.population
                )
            } else {
                format!(
                    "travelling to the largest {} (pop {})",
                    settlement_tier_name(pin.tier),
                    pin.population
                )
            };
            viewer.note = Some(note);
            eprintln!("{}", viewer.note.as_deref().unwrap_or_default());
        }
        Err(err) => {
            viewer.note = Some(format!("cannot land at the largest town: {err}"));
            eprintln!("entry refused: {err}");
        }
    }
}

fn settlement_tier_name(tier: u8) -> &'static str {
    match tier {
        0 => "hamlet",
        1 => "village",
        2 => "town",
        3 => "port",
        other => panic!("settlement tier {other} is not 0..=3"),
    }
}

fn draw_travel(viewer: &AtlasViewer, session: &WorldSession, frame: &Frame) {
    let ctx = frame.ui.ctx().clone();
    let cream = Color32::from_rgb(235, 230, 210);
    let mute = Color32::from_rgb(168, 186, 204);
    let where_to = session
        .destination()
        .map(|g| format!("({:.0} m, {:.0} m)", g.x, g.z))
        .unwrap_or_default();
    let heading = viewer
        .note
        .as_deref()
        .filter(|note| !note.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("Travelling to {where_to}"));
    let phase = session.travel_phase().map(|p| p.name()).unwrap_or("travel");
    let status = format!("{phase}  ·  {}", session.loading_status());
    let veil = session.travel_veil();

    egui::CentralPanel::default()
        .frame(egui::Frame::NONE)
        .show(&ctx, |ui| {
            let rect = ui.max_rect();
            let painter = ui.painter_at(rect);
            if veil > 0.02 {
                paint_travel_veil(&painter, rect, veil);
            }

            egui::Area::new(egui::Id::new("travel_status"))
                .anchor(Align2::CENTER_BOTTOM, [0.0, -36.0])
                .order(egui::Order::Foreground)
                .interactable(false)
                .show(ui.ctx(), |ui| {
                    ui.vertical_centered(|ui| {
                        ui.label(egui::RichText::new(heading).size(20.0).color(cream));
                        ui.add_space(6.0);
                        ui.label(egui::RichText::new(status).size(14.0).color(mute));
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new("Space skips the camera, not the landing")
                                .size(12.0)
                                .color(Color32::from_rgb(140, 156, 168)),
                        );
                    });
                });
        });
}

fn paint_travel_veil(painter: &egui::Painter, rect: Rect, veil: f32) {
    let veil = veil.clamp(0.0, 1.0);
    paint_edge_fade(painter, rect, true);
    paint_edge_fade(painter, rect, false);
    let wash = (veil * 70.0) as u8;
    if wash > 0 {
        painter.rect_filled(rect, 0.0, Color32::from_white_alpha(wash));
    }
}

struct SettingsUi {
    open: bool,
    prefs: Settings,
    applied: bool,
    /// Atlas edge length for the running session (fixed at launch).
    active_continent_size: usize,
    /// Combat verb waiting for the next key-down. Esc cancels listen.
    listening: Option<Action>,
}

fn apply_instance_submit_hotkeys(world: &mut World, frame: &Frame) {
    let (gpu, cpu) = frame.ui.ctx().input(|input| {
        (
            input.key_pressed(egui::Key::F10),
            input.key_pressed(egui::Key::F11),
        )
    });
    match (gpu, cpu) {
        (true, false) => world.set_instance_submit(InstanceSubmit::GpuIndirect),
        (false, true) => world.set_instance_submit(InstanceSubmit::CpuIndexed),
        (false, false) => {}
        (true, true) => panic!("F10 and F11 selected conflicting instance-submit modes"),
    }
}

fn apply_hitch_log(world: &mut World, on: bool, replace: bool) {
    if on {
        let path = settings::hitch_log_path().unwrap_or_else(|err| panic!("{err}"));
        if replace {
            settings::begin_hitch_log(&path).unwrap_or_else(|err| panic!("{err}"));
        }
        world.set_hitch_log(Some(path));
    } else {
        world.set_hitch_log(None);
    }
}

fn draw_settings(ui_state: &mut SettingsUi, world: &mut World, frame: &Frame) {
    let ctx = frame.ui.ctx().clone();
    if !ui_state.open {
        ui_state.listening = None;
    }
    if ui_state.listening.is_some() && !world.bind_listen() && ui_state.open {
        // Engine Esc cancelled listen only.
        ui_state.listening = None;
    }
    if let Some(action) = ui_state.listening {
        if let Some(k) = frame.input.last_key_down() {
            if !is_reserved(k) {
                ui_state.prefs.keys.assign(action, k);
                ui_state
                    .prefs
                    .write()
                    .unwrap_or_else(|err| panic!("{err}"));
                ui_state.listening = None;
            }
        }
    }
    egui::Area::new(egui::Id::new("settings_btn"))
        .anchor(Align2::RIGHT_BOTTOM, [-12.0, -12.0])
        .order(egui::Order::Foreground)
        .show(&ctx, |ui| {
            if ui
                .add(
                    egui::Button::new(
                        egui::RichText::new("Settings")
                            .size(14.0)
                            .color(Color32::from_rgb(235, 230, 210)),
                    )
                    .fill(Color32::from_rgba_unmultiplied(10, 14, 20, 200))
                    .stroke(egui::Stroke::new(1.0_f32, Color32::from_rgb(168, 186, 204))),
                )
                .clicked()
            {
                ui_state.open = true;
                world.set_pointer_lock(false);
            }
        });

    let mut hitch = ui_state.prefs.hitch_log;
    let mut continent_size = ui_state.prefs.continent_size as i32;
    let log_path = settings::hitch_log_path().ok();
    frame.ui.modal("Settings", &mut ui_state.open, |panel, open| {
        let ui = panel.ui();
        if ui.checkbox(&mut hitch, "Hitch log").changed() {
            apply_hitch_log(world, hitch, hitch);
            ui_state.prefs.hitch_log = hitch;
            ui_state
                .prefs
                .write()
                .unwrap_or_else(|err| panic!("{err}"));
        }
        if hitch {
            let where_to = log_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "(no data directory)".into());
            ui.label(
                egui::RichText::new(format!(
                    "Frames over 33 ms append to\n{where_to}\nTurning this on replaces the last log."
                ))
                .size(13.0)
                .color(Color32::from_rgb(180, 188, 196)),
            );
        } else {
            ui.label(
                egui::RichText::new("Off. The console is not written.")
                    .size(13.0)
                    .color(Color32::from_rgb(180, 188, 196)),
            );
        }
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new("Continent size (km)")
                .size(14.0)
                .color(Color32::from_rgb(235, 230, 210)),
        );
        let min = settings::MIN_CONTINENT_SIZE as i32;
        let max = MAX_CONTINENT_SIZE as i32;
        if ui
            .add(
                egui::DragValue::new(&mut continent_size)
                    .range(min..=max)
                    .speed(8.0),
            )
            .changed()
        {
            ui_state.prefs.continent_size = clamp_continent_size(continent_size as usize);
            ui_state
                .prefs
                .write()
                .unwrap_or_else(|err| panic!("{err}"));
        }
        let session = ui_state.active_continent_size;
        let saved = ui_state.prefs.continent_size();
        let size_note = if saved == session {
            format!("This session charts {session} km.")
        } else {
            format!("This session charts {session} km. Restart to chart {saved} km.")
        };
        ui.label(
            egui::RichText::new(size_note)
                .size(13.0)
                .color(Color32::from_rgb(180, 188, 196)),
        );
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new("Keys")
                .size(14.0)
                .color(Color32::from_rgb(235, 230, 210)),
        );
        ui.label(
            egui::RichText::new("Click a row, then press a key. Esc cancels listen.")
                .size(13.0)
                .color(Color32::from_rgb(180, 188, 196)),
        );
        for verb in Action::ALL {
            let bound = ui_state.prefs.keys.display(verb);
            let listening = ui_state.listening == Some(verb);
            let label = if listening {
                format!("{}  (press a key)", verb.label())
            } else {
                format!("{}  {}", verb.label(), bound)
            };
            if ui
                .add(
                    egui::Button::new(
                        egui::RichText::new(label)
                            .size(13.0)
                            .color(Color32::from_rgb(235, 230, 210)),
                    )
                    .fill(Color32::from_rgba_unmultiplied(10, 14, 20, 80))
                    .min_size(egui::vec2(280.0, 22.0)),
                )
                .clicked()
            {
                ui_state.listening = Some(verb);
            }
        }
        ui.add_space(8.0);
        if ui.button("Close").clicked() {
            *open = false;
            ui_state.listening = None;
        }
    });
    world.set_bind_listen(ui_state.open && ui_state.listening.is_some());
}

fn draw_world_hud(session: &mut WorldSession, world: &mut World, frame: &Frame) {
    let ctx = frame.ui.ctx().clone();
    if ctx.input(|i| i.key_pressed(egui::Key::M)) {
        session.return_to_atlas();
        world.set_pointer_lock(false);
        return;
    }
    let Some(p) = session.player_position() else {
        return;
    };
    let heading = session
        .player_heading()
        .map(|h| h.degrees())
        .unwrap_or_default();
    let at = engine::space::GlobalXZ::at(p.x, p.z);
    let column = session.surface().column(at);
    let layers = session.surface().terrain_layers(at);
    let stance = match session.locomotion() {
        Some(Locomotion::Fly) => "flying",
        _ if column.is_wet() => "in water",
        _ => "on land",
    };
    let mouse = if world.pointer_lock() {
        "Esc frees the mouse"
    } else {
        "click to look"
    };
    let submit = match world.instance_submit() {
        InstanceSubmit::GpuIndirect => "GPU",
        InstanceSubmit::CpuIndexed => "CPU",
    };
    let door = session.door_hint().unwrap_or("");
    let dungeon = session.dungeon_build_status();
    let text = format!(
        "({:.0} m, {:.0} m)  y {:.1}  loft {:.0}  iq {:+.0}  massif {:.0}  yaw {heading:.0}°  |  {stance}  |  chunks {}  |  fauna {}  |  {:.0} fps {submit}  |  F fly  |  Space jump  |  M map  |  {mouse}{door}",
        p.x,
        p.z,
        p.y,
        layers.loft_m,
        layers.iq_m,
        layers.massif_m,
        session.stream().resident_count(),
        session.fauna_count(),
        frame.fps,
        door = if door.is_empty() {
            String::new()
        } else {
            format!("  |  {door}")
        },
    );
    // Non-interactive: the pointer belongs to mouse-look, not to the HUD.
    egui::Area::new(egui::Id::new("world_hud"))
        .fixed_pos(egui::pos2(12.0, 12.0))
        .order(egui::Order::Foreground)
        .interactable(false)
        .show(&ctx, |ui| {
            egui::Frame::popup(ui.style())
                .inner_margin(egui::Margin::same(8))
                .show(ui, |ui| {
                    ui.label(egui::RichText::new(text).size(14.0));
                    if let Some(dungeon) = dungeon {
                        ui.label(
                            egui::RichText::new(dungeon)
                                .size(14.0)
                                .color(Color32::from_rgb(210, 168, 128)),
                        );
                    }
                });
        });
}
