//! Visible combat chrome: one hotbar row and a short always-hit log.

use engine::egui::{self, Color32, Sense, StrokeKind};

use crate::combat::WorldCombat;
use crate::controls::{Action, KeyBinds};

const HOTBAR: [Action; 9] = [
    Action::Strike,
    Action::Bash,
    Action::AimedShot,
    Action::Pin,
    Action::Ember,
    Action::Bind,
    Action::Mend,
    Action::Ward,
    Action::Potion,
];

pub fn draw_hotbar(ctx: &egui::Context, combat: &WorldCombat, binds: &KeyBinds) {
    let screen = ctx.screen_rect();
    let slot = 64.0;
    let gap = 6.0;
    let n = HOTBAR.len() as f32;
    let bar_w = n * slot + (n - 1.0) * gap;
    let bar_x = ((screen.width() - bar_w) * 0.5).max(12.0);
    let bar_y = screen.height() - slot - 18.0;

    egui::Area::new(egui::Id::new("combat_hotbar"))
        .fixed_pos(egui::pos2(bar_x, bar_y))
        .order(egui::Order::Foreground)
        .interactable(false)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                let ranks = combat.player.stats.ranks;
                for action in HOTBAR {
                    let gated = !action.rank_ok(ranks.martial, ranks.hunt, ranks.arcane)
                        || binds.get(action).is_none();
                    let cd = combat.verb_cd_frac(action);
                    let fill = if gated {
                        Color32::from_rgb(48, 48, 48)
                    } else {
                        Color32::from_rgb(36, 52, 72)
                    };
                    let key_col = if gated {
                        Color32::from_rgb(110, 110, 110)
                    } else {
                        Color32::from_rgb(230, 220, 190)
                    };
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(slot, slot), Sense::hover());
                    ui.painter().rect_filled(rect, 4.0, fill);
                    if cd > 0.0 {
                        let mut cover = rect;
                        cover.set_height(rect.height() * cd);
                        ui.painter()
                            .rect_filled(cover, 0.0, Color32::from_rgba_unmultiplied(20, 20, 40, 170));
                    }
                    ui.painter().rect_stroke(
                        rect,
                        4.0,
                        egui::Stroke::new(1.0_f32, Color32::from_rgb(90, 90, 90)),
                        StrokeKind::Inside,
                    );
                    ui.painter().text(
                        rect.center() + egui::vec2(0.0, -8.0),
                        egui::Align2::CENTER_CENTER,
                        binds.display(action),
                        egui::FontId::proportional(18.0),
                        key_col,
                    );
                    ui.painter().text(
                        rect.center() + egui::vec2(0.0, 12.0),
                        egui::Align2::CENTER_CENTER,
                        action.label(),
                        egui::FontId::proportional(10.0),
                        key_col,
                    );
                }
            });
        });
}

pub fn draw_combat_log(ctx: &egui::Context, combat: &WorldCombat) {
    let screen = ctx.screen_rect();
    let lines: Vec<&str> = combat.log.lines().collect();
    if lines.is_empty() {
        return;
    }
    let bar_y = screen.height() - 64.0 - 18.0;
    egui::Area::new(egui::Id::new("combat_log"))
        .fixed_pos(egui::pos2(20.0, (bar_y - 8.0 * 22.0 - 16.0).max(12.0)))
        .order(egui::Order::Foreground)
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style())
                .inner_margin(egui::Margin::same(8))
                .show(ui, |ui| {
                    for line in lines {
                        ui.label(
                            egui::RichText::new(line)
                                .size(15.0)
                                .color(Color32::from_rgb(230, 220, 190)),
                        );
                    }
                });
        });
}


pub fn draw_target_frame(ctx: &egui::Context, combat: &WorldCombat) {
    if combat.dead {
        return;
    }
    let Some(id) = combat.lock else {
        return;
    };
    let Some(h) = combat
        .hostiles
        .iter()
        .find(|h| h.idx == id && h.alive)
    else {
        return;
    };
    let max = h.max_hp.max(1.0);
    let frac = (h.hp / max).clamp(0.0, 1.0) as f32;
    let fill = if frac <= 0.20 {
        Color32::from_rgb(200, 32, 32)
    } else if frac <= 0.50 {
        Color32::from_rgb(220, 190, 32)
    } else {
        Color32::from_rgb(40, 180, 64)
    };
    let screen = ctx.screen_rect();
    let x = (screen.width() * 0.5 - 110.0).max(12.0);
    egui::Area::new(egui::Id::new("target_frame"))
        .fixed_pos(egui::pos2(x, 20.0))
        .order(egui::Order::Foreground)
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style())
                .inner_margin(egui::Margin::same(8))
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(&h.name)
                            .size(16.0)
                            .color(Color32::from_rgb(240, 210, 80)),
                    );
                    ui.add(
                        egui::ProgressBar::new(frac)
                            .fill(fill)
                            .desired_width(200.0)
                            .desired_height(16.0),
                    );
                });
        });
}

pub fn draw_hotbar_and_log(ctx: &egui::Context, combat: &WorldCombat, binds: &KeyBinds) {
    draw_target_frame(ctx, combat);
    draw_hotbar(ctx, combat, binds);
    draw_combat_log(ctx, combat);
}
