//! 2D hamlet marketplace lab — pan/zoom plan viewer with live knobs.
//!
//! Usage: `cargo run -p orrun --bin hamlet_lab`
//! Controls: drag to pan, scroll to zoom, R regenerate, F fit, Escape to quit.

use engine::egui::{self, Align2, Color32, FontId, Pos2, Sense, Stroke, Vec2};
use engine::prelude::*;
use orrun::hamlet::{plan, HamletLabConfig, Plan2D, ShapeKind};

const MIN_ZOOM: f32 = 0.5;
const MAX_ZOOM: f32 = 48.0;
const ZOOM_STEP: f32 = 1.15;

struct HamletLab {
    config: HamletLabConfig,
    plan: Plan2D,
    error: Option<String>,
    zoom: f32,
    pan: Vec2,
    needs_fit: bool,
    dirty: bool,
}

impl HamletLab {
    fn new() -> Self {
        let mut config = HamletLabConfig::default();
        config.apply_tier_defaults(0);
        config.seed = 1;
        config.show_occupancy = false;
        let mut lab = Self {
            config,
            plan: Plan2D::default(),
            error: None,
            zoom: 4.0,
            pan: Vec2::ZERO,
            needs_fit: true,
            dirty: true,
        };
        lab.regenerate();
        lab
    }

    fn regenerate(&mut self) {
        self.dirty = false;
        match plan(&self.config) {
            Ok(p) => {
                self.plan = p;
                self.error = if self.plan.underfill_message.is_empty() {
                    None
                } else {
                    Some(self.plan.underfill_message.clone())
                };
            }
            Err(e) => {
                self.error = Some(e.to_string());
            }
        }
        self.needs_fit = true;
    }

    fn fit(&mut self, panel: Vec2) {
        let r = self.plan.built_envelope.max(20.0) * 2.2;
        let fit = panel.min_elem() / r.max(1.0);
        self.zoom = fit.clamp(MIN_ZOOM, MAX_ZOOM);
        self.pan = panel * 0.5;
        self.needs_fit = false;
    }

    fn world_to_panel(&self, origin: Pos2, w: glam::Vec2) -> Pos2 {
        Pos2::new(
            origin.x + self.pan.x + w.x * self.zoom,
            origin.y + self.pan.y + w.y * self.zoom,
        )
    }

    fn zoom_at(&mut self, screen_in_panel: Pos2, factor: f32) {
        let before = (screen_in_panel.to_vec2() - self.pan) / self.zoom;
        self.zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        self.pan = screen_in_panel.to_vec2() - before * self.zoom;
    }

    fn draw_plan(&self, painter: &egui::Painter, origin: Pos2) {
        let env = self.plan.built_envelope.max(16.0);
        painter.circle_filled(
            self.world_to_panel(origin, glam::Vec2::ZERO),
            env * self.zoom,
            Color32::from_rgb(52, 58, 48),
        );

        for poly in &self.plan.markets {
            if poly.len() < 3 {
                continue;
            }
            let pts: Vec<Pos2> = poly
                .iter()
                .map(|p| self.world_to_panel(origin, *p))
                .collect();
            painter.add(egui::Shape::convex_polygon(
                pts,
                Color32::from_rgb(74, 122, 74),
                Stroke::new(1.5_f32, Color32::from_rgb(168, 212, 140)),
            ));
        }

        for dot in &self.plan.occupancy_dots {
            painter.circle_filled(
                self.world_to_panel(origin, *dot),
                1.2,
                Color32::from_rgba_unmultiplied(255, 255, 255, 40),
            );
        }

        for shape in &self.plan.shapes {
            if shape.kind != ShapeKind::House {
                continue;
            }
            let color = catalog_color(&shape.catalog_id);
            let corners = obb_corners(
                shape.center,
                shape.half_size.x,
                shape.half_size.y,
                shape.yaw,
            );
            let pts: Vec<Pos2> = corners
                .iter()
                .map(|p| self.world_to_panel(origin, *p))
                .collect();
            painter.add(egui::Shape::convex_polygon(
                pts,
                color,
                Stroke::new(1.0_f32, Color32::from_rgb(20, 20, 20)),
            ));
            let facing = glam::Vec2::new(shape.yaw.sin(), shape.yaw.cos());
            let door = shape.center - facing * shape.half_size.y;
            painter.circle_filled(
                self.world_to_panel(origin, door),
                2.0,
                Color32::from_rgb(255, 220, 120),
            );
        }

        painter.circle_filled(
            self.world_to_panel(origin, self.plan.plaza),
            3.0,
            Color32::from_rgb(255, 247, 217),
        );
    }
}

fn catalog_color(id: &str) -> Color32 {
    match id {
        "Well" => Color32::from_rgb(120, 180, 220),
        "Inn" => Color32::from_rgb(180, 120, 90),
        "Blacksmith" => Color32::from_rgb(140, 140, 150),
        "Mill" | "Sawmill" => Color32::from_rgb(160, 130, 90),
        "Stable" => Color32::from_rgb(150, 110, 70),
        "Bell_Tower" => Color32::from_rgb(200, 180, 120),
        "Gazebo" => Color32::from_rgb(100, 160, 100),
        _ => Color32::from_rgb(190, 150, 110),
    }
}

fn obb_corners(center: glam::Vec2, half_x: f32, half_z: f32, yaw: f32) -> [glam::Vec2; 4] {
    let x_axis = glam::Vec2::new(yaw.cos(), -yaw.sin());
    let z_axis = glam::Vec2::new(yaw.sin(), yaw.cos());
    [
        center + x_axis * half_x + z_axis * half_z,
        center - x_axis * half_x + z_axis * half_z,
        center - x_axis * half_x - z_axis * half_z,
        center + x_axis * half_x - z_axis * half_z,
    ]
}

fn main() {
    let mut lab = HamletLab::new();

    Engine::run("Orrun — Hamlet Lab", move |world, frame| {
        if frame.first {
            world.clear_color = Color::rgb(20, 22, 24);
        }

        let ctx = frame.ui.ctx().clone();
        if ctx.input(|i| i.key_pressed(egui::Key::R)) {
            lab.dirty = true;
        }
        if lab.dirty {
            lab.regenerate();
        }

        egui::SidePanel::left("hamlet_knobs")
            .default_width(280.0)
            .show(&ctx, |ui| {
                ui.heading("Hamlet Lab");
                ui.label("2D marketplace packer (no height)");
                ui.separator();

                let mut seed = lab.config.seed as i64;
                if ui
                    .add(egui::Slider::new(&mut seed, 0..=999_999).text("seed"))
                    .changed()
                {
                    lab.config.seed = seed as u64;
                    lab.dirty = true;
                }

                let mut tier = lab.config.tier as i32;
                if ui
                    .add(egui::Slider::new(&mut tier, 0..=3).text("tier"))
                    .changed()
                {
                    lab.config.apply_tier_defaults(tier as u8);
                    lab.dirty = true;
                }

                ui.separator();
                ui.label(format!(
                    "dwellings {}–{}",
                    lab.config.dwelling_min, lab.config.dwelling_max
                ));
                let mut dmin = lab.config.dwelling_min as i32;
                let mut dmax = lab.config.dwelling_max as i32;
                if ui
                    .add(egui::Slider::new(&mut dmin, 1..=200).text("dwelling min"))
                    .changed()
                {
                    lab.config.dwelling_min = dmin as u32;
                    lab.dirty = true;
                }
                if ui
                    .add(egui::Slider::new(&mut dmax, 1..=200).text("dwelling max"))
                    .changed()
                {
                    lab.config.dwelling_max = dmax as u32;
                    lab.dirty = true;
                }

                if ui
                    .add(
                        egui::Slider::new(&mut lab.config.market_radius, 4.0..=40.0)
                            .text("market radius"),
                    )
                    .changed()
                {
                    lab.dirty = true;
                }
                if ui
                    .add(
                        egui::Slider::new(&mut lab.config.market_radius_jitter, 0.0..=0.9)
                            .text("radius jitter"),
                    )
                    .changed()
                {
                    lab.dirty = true;
                }
                if ui
                    .add(
                        egui::Slider::new(&mut lab.config.market_angle_jitter, 0.0..=0.95)
                            .text("angle jitter"),
                    )
                    .changed()
                {
                    lab.dirty = true;
                }
                if ui
                    .add(egui::Slider::new(&mut lab.config.alley, 0.0..=2.0).text("alley"))
                    .changed()
                {
                    lab.dirty = true;
                }
                if ui
                    .add(
                        egui::Slider::new(&mut lab.config.select_temperature, 0.05..=1.0)
                            .text("softmax temp"),
                    )
                    .changed()
                {
                    lab.dirty = true;
                }
                if ui
                    .checkbox(&mut lab.config.show_occupancy, "show occupancy")
                    .changed()
                {
                    lab.dirty = true;
                }

                ui.separator();
                if ui.button("Regenerate (R)").clicked() {
                    lab.dirty = true;
                }
                if ui.button("Fit view (F)").clicked() {
                    lab.needs_fit = true;
                }

                ui.separator();
                ui.label(format!(
                    "houses {} / {}   civics {}   markets {}",
                    lab.plan.house_count,
                    lab.plan.want_count,
                    lab.plan.civic_count,
                    lab.plan.markets.len()
                ));
                ui.label(format!("envelope {:.1} m", lab.plan.built_envelope));
                if let Some(err) = &lab.error {
                    ui.colored_label(Color32::from_rgb(255, 120, 100), err);
                }
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(Color32::from_rgb(28, 30, 32)))
            .show(&ctx, |ui| {
                let panel = ui.available_size();
                if lab.needs_fit {
                    lab.fit(panel);
                }
                if ui.input(|i| i.key_pressed(egui::Key::F)) {
                    lab.fit(panel);
                }

                let (rect, response) = ui.allocate_exact_size(panel, Sense::click_and_drag());
                if response.dragged() {
                    lab.pan += response.drag_delta();
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
                    lab.zoom_at(local, factor);
                }

                let painter = ui.painter().with_clip_rect(rect);
                lab.draw_plan(&painter, rect.min);
                painter.text(
                    rect.left_top() + Vec2::new(12.0, 12.0),
                    Align2::LEFT_TOP,
                    format!("zoom {:.1} px/m  |  drag pan  |  scroll zoom", lab.zoom),
                    FontId::proportional(14.0),
                    Color32::from_rgb(200, 200, 200),
                );
            });

        let _ = world;
    });
}
