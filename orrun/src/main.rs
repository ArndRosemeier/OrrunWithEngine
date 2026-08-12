//! Orrun — atlas map and walkable continent in one process.
//!
//! Usage: `cargo run -p orrun -- [seed] [size]`
//!
//! Map: drag to pan · scroll to zoom · F fit · left click travels there ·
//! right click reveals a cell overlay · C clears overlays · M returns to where
//! you were standing.
//! World (first person): click to look · Esc hands the mouse back · W/S walk ·
//! Q/E sidestep · A/D turn · Shift sprint · F fly (Space up, Ctrl down) ·
//! M summons the map · Esc with a free cursor quits.
//!
//! Where the player stood is written on exit, per seed and size, and walked
//! back to on the next launch.

use std::sync::{Arc, Mutex};

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
use orrun::atlas::{ContinentAtlas, EndpointKind, Kind, NodeKind};
use orrun::save::SavedStand;
use orrun::world::{
    install_daylight, install_materials, AtlasBounds, AtlasCell, ContinentalSurface, Heading,
    Locomotion, MapPoint, SessionState, WorldEntryRequest, WorldSession,
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
        }
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
            let p = panel_origin + self.cell_centre_panel(node.ax, node.az).to_vec2();
            let color = match node.kind {
                NodeKind::CoastalGate => Color32::from_rgb(242, 217, 89),
                NodeKind::LakeShore => Color32::from_rgb(102, 191, 242),
                NodeKind::Pass => Color32::from_rgb(191, 191, 204),
                NodeKind::ClaimReserved => Color32::from_rgb(217, 64, 179),
                NodeKind::Settlement => Color32::from_rgb(255, 247, 217),
                NodeKind::Landmark => Color32::from_rgb(242, 102, 89),
            };
            let mut radius = (self.zoom * 0.18).max(2.0);
            if node.kind == NodeKind::Settlement {
                radius = (self.zoom * 0.3).max(3.5);
                painter.circle_filled(
                    p,
                    radius * 1.6,
                    Color32::from_rgba_unmultiplied(38, 23, 13, 191),
                );
            }
            painter.circle_filled(p, radius, color);
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
        let (fx, fz) = point.fraction();
        let cell = point.cell();
        let p = panel_origin
            + self
                .local_to_panel(Pos2::ZERO, cell.ax(), cell.az(), [fx, fz])
                .to_vec2();
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
        let (fx, fz) = at.fraction();
        let cell = at.cell();
        let p = panel_origin
            + self
                .local_to_panel(Pos2::ZERO, cell.ax(), cell.az(), [fx, fz])
                .to_vec2();
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

fn parse_args() -> (i32, usize) {
    let mut args = std::env::args().skip(1);
    let seed = args.next().and_then(|s| s.parse().ok()).unwrap_or(20260809);
    let size = args.next().and_then(|s| s.parse().ok()).unwrap_or(256);
    (seed, size.max(32))
}

/// Ask the session to enter where the player last stood.
///
/// A remembered stand that no longer resolves is worth saying out loud: it
/// means entry and the saved world have drifted apart, and silently dropping
/// the player on the map would hide that.
fn resume_saved_stand(
    stand: SavedStand,
    bounds: AtlasBounds,
    session: &mut WorldSession,
    world: &mut World,
) {
    let point = match MapPoint::from_global(bounds, stand.at()) {
        Ok(point) => point,
        Err(err) => {
            eprintln!("saved stand is outside this atlas: {err}");
            return;
        }
    };
    let heading = Heading::from_degrees(stand.yaw_degrees).expect("a saved heading is finite");
    match session.begin_entry(world, WorldEntryRequest::at(point).facing(heading)) {
        Ok(pose) => eprintln!(
            "resuming at ({:.0} m, {:.0} m)",
            pose.ground().x,
            pose.ground().z
        ),
        Err(err) => eprintln!("cannot resume the saved stand: {err}"),
    }
}

fn main() {
    let (seed, size) = parse_args();
    eprintln!("generating atlas seed={seed} size={size}…");
    let atlas = Arc::new(ContinentAtlas::generate(seed, size));
    let surface = Arc::new(ContinentalSurface::new(&atlas).expect("canonical surface"));
    eprintln!(
        "ready: lakes={} rivers={} coasts={} nodes={} hash={:#x}",
        atlas.hydro.lakes.len(),
        atlas.hydro.rivers.len(),
        atlas.hydro.coasts.len(),
        atlas.nodes.len(),
        atlas.content_hash as u32,
    );

    let mut viewer = AtlasViewer::new(Arc::clone(&atlas), Arc::clone(&surface));
    let mut session = WorldSession::new(Arc::clone(&surface));
    let sea = surface.sea_surface_z();
    let bounds = surface.bounds();

    // Read before the window opens: a broken save should say so instead of
    // quietly dropping the player back on the map.
    let remembered = SavedStand::read(seed, size).unwrap_or_else(|err| panic!("{err}"));

    // The frame callback owns the session, so the last stand is handed out
    // here and written once the window has closed.
    let last_stand = Arc::new(Mutex::new(remembered));
    let stand_in_loop = Arc::clone(&last_stand);

    Engine::run("Orrun", move |world, frame| {
        if frame.first {
            install_daylight(world);
            install_materials(world, seed, sea);
            if let Some(stand) = remembered {
                resume_saved_stand(stand, bounds, &mut session, world);
            }
        }

        match session.state() {
            SessionState::World => install_daylight(world),
            _ => world.set_clear_color(rgb(12, 14, 18)),
        }

        if let Err(err) = session.update(world, frame) {
            panic!("world session failed: {err}");
        }

        if let (Some(p), Some(heading)) = (session.player_position(), session.player_heading()) {
            *stand_in_loop.lock().expect("last stand") =
                Some(SavedStand::new(seed, size, GlobalXZ::at(p.x, p.z), heading));
        }

        match session.state() {
            SessionState::Atlas => draw_atlas(&mut viewer, &mut session, world, frame),
            SessionState::Loading => draw_loading(&viewer, &session, frame),
            SessionState::World => draw_world_hud(&mut session, world, frame),
        }
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
            let panel = ui.available_size();
            if viewer.needs_fit {
                viewer.fit(panel);
            }
            if ui.input(|i| i.key_pressed(egui::Key::F)) {
                viewer.fit(panel);
            }

            let (rect, response) =
                ui.allocate_exact_size(panel, Sense::click_and_drag() | Sense::click());

            if response.dragged_by(PointerButton::Primary) {
                viewer.pan += response.drag_delta();
            }

            let scroll_y = ui.input(|i| i.raw_scroll_delta.y);
            if response.hovered() && scroll_y != 0.0 {
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

            let pointer = ui
                .input(|i| i.pointer.interact_pos())
                .map(|p| Pos2::new(p.x - rect.min.x, p.y - rect.min.y));

            // Left click travels there; right click reveals the overlay. A
            // click that came out of a drag was the player panning the map.
            if response.hovered() {
                let primary = ui.input(|i| i.pointer.button_clicked(PointerButton::Primary));
                let secondary = ui.input(|i| i.pointer.button_clicked(PointerButton::Secondary));
                if let Some(local) = pointer {
                    if primary && !response.dragged_by(PointerButton::Primary) {
                        if let Some(point) = viewer.screen_to_map(local) {
                            viewer.selection = Some(point);
                            travel_to(viewer, session, world);
                        }
                    }
                    if secondary {
                        if let Some(cell) = viewer.screen_to_cell(local) {
                            viewer.reveal_overlay(cell);
                        }
                    }
                }
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
        Ok(pose) => {
            let g = pose.ground();
            viewer.note = Some(format!(
                "travelling to ({:.0} m, {:.0} m), {:.0} m from the pick",
                g.x,
                g.z,
                pose.offset_m()
            ));
            eprintln!("{}", viewer.note.as_deref().unwrap_or_default());
        }
        Err(err) => {
            viewer.note = Some(format!("cannot land there: {err}"));
            eprintln!("entry refused: {err}");
        }
    }
}

fn draw_loading(viewer: &AtlasViewer, session: &WorldSession, frame: &Frame) {
    let ctx = frame.ui.ctx().clone();
    let progress = session.loading_progress();
    let where_to = session
        .spawn()
        .map(|s| {
            let g = s.ground();
            format!("({:.0} m, {:.0} m)", g.x, g.z)
        })
        .unwrap_or_default();
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(Color32::from_rgb(12, 14, 18)))
        .show(&ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() * 0.4);
                ui.label(
                    egui::RichText::new(format!("Travelling to {where_to}"))
                        .size(22.0)
                        .color(Color32::from_rgb(235, 230, 210)),
                );
                ui.add_space(10.0);
                ui.label(
                    egui::RichText::new(format!(
                        "streaming ground… {:.0}%   ({} chunks resident)",
                        progress * 100.0,
                        session.stream().resident_count()
                    ))
                    .size(14.0)
                    .color(Color32::from_rgb(150, 200, 240)),
                );
                if let Some(note) = &viewer.note {
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new(note).size(12.0));
                }
            });
        });
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
    let column = session
        .surface()
        .column(engine::space::GlobalXZ::at(p.x, p.z));
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
    let text = format!(
        "({:.0} m, {:.0} m)  y {:.1}  yaw {heading:.0}°  |  {stance}  |  chunks {}  |  {:.0} fps  |  F fly  |  M map  |  {mouse}",
        p.x,
        p.z,
        p.y,
        session.stream().resident_count(),
        frame.fps,
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
                });
        });
}
