//! Orrun atlas viewer — full-screen pan/zoom map.
//!
//! Usage: `cargo run -p orrun -- [seed] [size]`
//! Controls: drag to pan, scroll to zoom, F to fit, Escape to quit.
//! Hover shows cell fields; zoom past ~56 px/cell draws them inside cells.
//! Right-click a cell to bake & keep its vector sublayer; C clears sublayers.

use engine::egui::{
    self, Align2, ColorImage, Color32, FontId, PointerButton, Pos2, Rect, Sense, Stroke,
    StrokeKind, TextureHandle, TextureOptions, Vec2,
};
use engine::prelude::*;
use orrun::atlas::cell_sublayer::{CellSublayer, SublayerStore, WaterBodyKind};
use orrun::atlas::features::{edge_owner, Dir};
use orrun::atlas::pack;
use orrun::atlas::preview;
use orrun::atlas::types::{Endpoint, Link};
use orrun::atlas::{ContinentAtlas, EndpointKind, Kind, NodeKind};

const MIN_ZOOM: f32 = 0.15;
const MAX_ZOOM: f32 = 384.0;
const ZOOM_STEP: f32 = 1.15;
/// Per-cell overlay starts once a cell is large enough for several lines.
const DETAIL_CELL_PX: f32 = 56.0;

struct AtlasViewer {
    atlas: ContinentAtlas,
    pixels: Vec<u8>,
    texture: Option<TextureHandle>,
    /// Screen pixels per atlas cell.
    zoom: f32,
    /// Top-left of the map in panel coordinates.
    pan: Vec2,
    needs_fit: bool,
    /// Right-click revealed cell vector sublayers (persisted for the session).
    sublayers: SublayerStore,
    last_sublayer_note: Option<String>,
}

impl AtlasViewer {
    fn new(seed: i32, size: usize) -> Self {
        let atlas = ContinentAtlas::generate(seed, size);
        let pixels = preview::biome_rgba(&atlas);
        Self {
            atlas,
            pixels,
            texture: None,
            zoom: 1.0,
            pan: Vec2::ZERO,
            needs_fit: true,
            sublayers: SublayerStore::default(),
            last_sublayer_note: None,
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

    fn screen_to_cell(&self, screen_in_panel: Pos2) -> Option<(i32, i32)> {
        let map_pos = (screen_in_panel.to_vec2() - self.pan) / self.zoom;
        let ax = map_pos.x.floor() as i32;
        let az = map_pos.y.floor() as i32;
        if self.atlas.in_bounds(ax, az) {
            Some((ax, az))
        } else {
            None
        }
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
        let points: Vec<Pos2> = if dist2(a, b) < 0.25 || dist2(a, mid) < 0.25 || dist2(b, mid) < 0.25
        {
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

    fn hover_line(&self, ax: i32, az: i32) -> String {
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

    fn reveal_sublayer(&mut self, ax: i32, az: i32) {
        let layer = self.sublayers.reveal(&self.atlas, ax, az);
        self.last_sublayer_note = Some(layer.summary.clone());
        eprintln!("{}", layer.summary);
    }

    fn local_to_panel(&self, panel_origin: Pos2, ax: i32, az: i32, local: [f32; 2]) -> Pos2 {
        let origin = self.cell_to_panel(ax, az);
        Pos2::new(
            panel_origin.x + origin.x + local[0] * self.zoom,
            panel_origin.y + origin.y + local[1] * self.zoom,
        )
    }

    fn draw_sublayers(&self, painter: &egui::Painter, panel_origin: Pos2) {
        for layer in self.sublayers.cells.values() {
            self.draw_one_sublayer(painter, panel_origin, layer);
        }
    }

    fn draw_one_sublayer(
        &self,
        painter: &egui::Painter,
        panel_origin: Pos2,
        layer: &CellSublayer,
    ) {
        let ax = layer.ax;
        let az = layer.az;
        let cell_origin = panel_origin + self.cell_to_panel(ax, az).to_vec2();
        let cell_rect = Rect::from_min_size(cell_origin, Vec2::splat(self.zoom));

        // Dim cell so vectors read clearly when zoomed out a bit.
        painter.rect_filled(cell_rect, 0.0, Color32::from_black_alpha(40));
        painter.rect_stroke(
            cell_rect,
            0.0,
            Stroke::new(2.0_f32, Color32::from_rgb(255, 220, 120)),
            StrokeKind::Inside,
        );

        // Hydro shore field: repaint both wet and dry so biome stairs don't show through.
        if let Some(water) = &layer.water {
            let res = water.res.max(1) as f32;
            let wet_fill = match water.kind {
                WaterBodyKind::Ocean => Color32::from_rgba_unmultiplied(35, 105, 195, 210),
                WaterBodyKind::Lake { .. } => Color32::from_rgba_unmultiplied(50, 135, 205, 215),
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
            // Authoritative hydro coast/lake segments (continuous across cell edges).
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

        // River ribbons (blue waterways) + pale centreline for edge connectivity.
        for river in &layer.rivers {
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

        // Roads (gold).
        for road in &layer.roads {
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
    let seed = args
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20260809);
    let size = args.next().and_then(|s| s.parse().ok()).unwrap_or(256);
    (seed, size.max(32))
}

fn main() {
    let (seed, size) = parse_args();
    eprintln!("generating atlas seed={seed} size={size}…");
    let mut viewer = AtlasViewer::new(seed, size);
    if std::env::var_os("ORRUN_REVEAL_SHORES").is_some() {
        eprintln!("revealing all shore sublayers…");
        viewer.sublayers.reveal_all_shores(&viewer.atlas);
        eprintln!("revealed {} cells", viewer.sublayers.len());
        // Frame a coast for screenshots.
        if let Some(((ax, az), _)) = viewer
            .sublayers
            .cells
            .iter()
            .find(|(_, l)| matches!(l.biome, orrun::atlas::Biome::Coast))
        {
            viewer.zoom = 120.0;
            viewer.pan = Vec2::new(
                400.0 - *ax as f32 * viewer.zoom,
                300.0 - *az as f32 * viewer.zoom,
            );
            viewer.needs_fit = false;
        }
    }
    eprintln!(
        "ready: lakes={} nodes={} river_edges={} road_edges={} hash={:#x}",
        viewer.atlas.lakes.len(),
        viewer.atlas.nodes.len(),
        viewer.atlas.river_ports.len(),
        viewer.atlas.road_ports.len(),
        viewer.atlas.content_hash as u32,
    );

    Engine::run("Orrun — Atlas", move |world, frame| {
        if frame.first {
            world.clear_color = engine::Color::rgb(12, 14, 18);
        }

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
                    viewer.sublayers.clear();
                    viewer.last_sublayer_note = Some("cleared cell sublayers".into());
                }

                // Right-click: bake & keep vector sublayer for that cell.
                if response.hovered() {
                    let secondary = ui.input(|i| i.pointer.button_clicked(PointerButton::Secondary));
                    if secondary {
                        if let Some(p) = ui.input(|i| i.pointer.interact_pos()) {
                            let local = Pos2::new(p.x - rect.min.x, p.y - rect.min.y);
                            if let Some((ax, az)) = viewer.screen_to_cell(local) {
                                viewer.reveal_sublayer(ax, az);
                            }
                        }
                    }
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

                // Cell text under corridors so rivers/roads stay continuous when zoomed in.
                if viewer.zoom >= DETAIL_CELL_PX {
                    viewer.draw_cell_details(&painter, rect.min, panel);
                }
                viewer.draw_feature_overlays(&painter, rect.min);
                viewer.draw_sublayers(&painter, rect.min);

                let status = format!(
                    "seed {}  |  {} km  |  zoom {:.1} px/cell  |  sublayers {}  |  drag pan  |  scroll zoom  |  F fit  |  RMB cell vector  |  C clear{}",
                    viewer.atlas.world_seed,
                    viewer.atlas.size,
                    viewer.zoom,
                    viewer.sublayers.len(),
                    if viewer.zoom < DETAIL_CELL_PX {
                        "  |  zoom in for cell text"
                    } else {
                        ""
                    },
                );
                let hover_text = hover_cell.map(|(ax, az)| {
                    let base = viewer.hover_line(ax, az);
                    if viewer.sublayers.cells.contains_key(&(ax, az)) {
                        format!("{base}  |  SUBLAYER ON")
                    } else {
                        format!("{base}  |  RMB → vector sublayer")
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
                                if let Some(note) = &viewer.last_sublayer_note {
                                    ui.label(
                                        egui::RichText::new(note)
                                            .size(12.0)
                                            .color(Color32::from_rgb(120, 200, 255)),
                                    );
                                }
                            });
                    });
            });
    });
}
