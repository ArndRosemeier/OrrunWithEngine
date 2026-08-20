//! Visible combat chrome: one hotbar row and a short always-hit log.

use engine::egui::{self, Color32, Sense, StrokeKind};
use engine::world::World;
use engine::Frame;

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
                        // Remaining-time pie: verb_cd_frac is left/max (1.0 just used, 0.0 ready).
                        let center = rect.center();
                        let radius = rect.width() * 0.5;
                        let color = Color32::from_rgba_unmultiplied(20, 20, 40, 170);
                        if cd >= 0.999 {
                            ui.painter().circle_filled(center, radius, color);
                        } else {
                            let steps = ((cd * 48.0).ceil() as i32).max(3);
                            let mut pts = Vec::with_capacity((steps + 2) as usize);
                            pts.push(center);
                            let start = -std::f32::consts::FRAC_PI_2;
                            let sweep = cd * std::f32::consts::TAU;
                            for i in 0..=steps {
                                let t = i as f32 / steps as f32;
                                let a = start + sweep * t;
                                pts.push(center + radius * egui::vec2(a.cos(), a.sin()));
                            }
                            ui.painter().add(egui::Shape::convex_polygon(
                                pts,
                                color,
                                egui::Stroke::NONE,
                            ));
                        }
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

pub fn draw_fail_toast(ctx: &egui::Context, combat: &WorldCombat) {
    let Some(line) = combat.fail_tell() else {
        return;
    };
    let screen = ctx.screen_rect();
    egui::Area::new(egui::Id::new("fail_toast"))
        .fixed_pos(egui::pos2(screen.width() * 0.5 - 90.0, screen.height() * 0.38))
        .order(egui::Order::Foreground)
        .interactable(false)
        .show(ctx, |ui| {
            ui.label(
                egui::RichText::new(line)
                    .size(28.0)
                    .color(Color32::from_rgb(240, 200, 80)),
            );
        });
}

pub fn draw_hotbar_and_log(ctx: &egui::Context, combat: &WorldCombat, binds: &KeyBinds) {
    draw_target_frame(ctx, combat);
    draw_hotbar(ctx, combat, binds);
    draw_combat_log(ctx, combat);
    draw_fail_toast(ctx, combat);
}


use crate::inventory::{load_icon, EquipSlot, Family, Item};
use crate::world::WorldSession;
use std::cell::RefCell;
use std::collections::HashMap;

const SLOT: f32 = 56.0;
const INK: Color32 = Color32::from_rgb(235, 230, 210);
const MUTED: Color32 = Color32::from_rgb(180, 188, 196);

thread_local! {
    static ICON_TEX: RefCell<HashMap<&'static str, egui::TextureHandle>> = RefCell::new(HashMap::new());
}

fn icon_texture(ctx: &egui::Context, family: Family) -> Option<egui::TextureHandle> {
    let pix = load_icon(family)?;
    let key = family.icon_file();
    ICON_TEX.with(|cell| {
        let mut map = cell.borrow_mut();
        if let Some(tex) = map.get(key) {
            return Some(tex.clone());
        }
        let tex = ctx.load_texture(
            format!("icon-{key}"),
            egui::ColorImage::from_rgba_unmultiplied(
                [pix.width as usize, pix.height as usize],
                &pix.rgba,
            ),
            egui::TextureOptions::NEAREST,
        );
        map.insert(key, tex.clone());
        Some(tex)
    })
}

fn paint_item_slot(ui: &mut egui::Ui, item: Option<Item>, label: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(SLOT, SLOT), Sense::click());
    ui.painter()
        .rect_filled(rect, 4.0, Color32::from_rgb(28, 36, 48));
    ui.painter().rect_stroke(
        rect,
        4.0,
        egui::Stroke::new(1.0_f32, Color32::from_rgb(90, 90, 90)),
        StrokeKind::Inside,
    );
    if let Some(item) = item {
        if let Some(tex) = icon_texture(ui.ctx(), item.family()) {
            let inner = rect.shrink(6.0);
            ui.painter().image(
                tex.id(),
                inner,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        if item.count > 1 {
            ui.painter().text(
                rect.right_bottom() + egui::vec2(-4.0, -4.0),
                egui::Align2::RIGHT_BOTTOM,
                format!("{}", item.count),
                egui::FontId::proportional(12.0),
                INK,
            );
        }
    } else {
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(10.0),
            MUTED,
        );
    }
    response
}

fn paint_coin_row(ui: &mut egui::Ui, coin: i32) {
    ui.horizontal(|ui| {
        if let Some(tex) = icon_texture(ui.ctx(), Family::Coin) {
            ui.add(egui::Image::new((tex.id(), egui::vec2(22.0, 22.0))));
        }
        ui.label(
            egui::RichText::new(format!("Coin: {coin}"))
                .size(16.0)
                .color(INK),
        );
    });
}

/// I bag + corpse loot modal. Clone of Settings (`UiFrame::modal` + pointer free).
pub fn draw_loot_windows(session: &mut WorldSession, world: &mut World, frame: &Frame) {
    let mut bag_open = session.bag_open();
    frame.ui.modal("Bag", &mut bag_open, |panel, open| {
        let ui = panel.ui();
        ui.label(
            egui::RichText::new("I bag. Click a bag item to equip.")
                .size(13.0)
                .color(MUTED),
        );
        ui.add_space(6.0);
        ui.label(egui::RichText::new("Equip").size(14.0).color(INK));
        ui.horizontal(|ui| {
            let melee = session.inventory().melee;
            if paint_item_slot(ui, melee, "Melee").clicked() {
                session.inventory_mut().click_equip(EquipSlot::Melee);
            }
            let bow = session.inventory().bow;
            if paint_item_slot(ui, bow, "Bow").clicked() {
                session.inventory_mut().click_equip(EquipSlot::Bow);
            }
            let body = session.inventory().body;
            if paint_item_slot(ui, body, "Body").clicked() {
                session.inventory_mut().click_equip(EquipSlot::Body);
            }
            let charm = session.inventory().charm;
            if paint_item_slot(ui, charm, "Charm").clicked() {
                session.inventory_mut().click_equip(EquipSlot::Charm);
            }
        });
        ui.add_space(8.0);
        ui.label(egui::RichText::new("Bag").size(14.0).color(INK));
        ui.horizontal(|ui| {
            for i in 0..4 {
                let item = session.inventory().bag[i];
                if paint_item_slot(ui, item, "").clicked() {
                    session.inventory_mut().click_bag(i);
                }
            }
        });
        ui.horizontal(|ui| {
            for i in 4..8 {
                let item = session.inventory().bag[i];
                if paint_item_slot(ui, item, "").clicked() {
                    session.inventory_mut().click_bag(i);
                }
            }
        });
        ui.add_space(8.0);
        paint_coin_row(ui, session.inventory().coin);
        ui.add_space(8.0);
        if ui.button("Close").clicked() {
            *open = false;
        }
    });
    if bag_open != session.bag_open() {
        session.set_bag_open(bag_open);
    }

    let mut loot_open = session.loot_open();
    frame.ui.modal("Loot", &mut loot_open, |panel, open| {
        let ui = panel.ui();
        ui.label(
            egui::RichText::new("Take / Take all")
                .size(14.0)
                .color(INK),
        );
        ui.add_space(6.0);
        if let Some(pile) = session.ground_pile().cloned() {
            paint_coin_row(ui, pile.coin);
            for (i, item) in pile.items.iter().enumerate() {
                ui.horizontal(|ui| {
                    let _ = paint_item_slot(ui, Some(*item), "");
                    ui.label(
                        egui::RichText::new(item.name())
                            .size(15.0)
                            .color(INK),
                    );
                    if ui.button("Take").clicked() {
                        session.take_loot_item(world, i);
                    }
                });
            }
            ui.add_space(8.0);
            if ui.button("Take all").clicked() {
                session.take_all_loot(world);
            }
        } else {
            ui.label(egui::RichText::new("Empty.").size(14.0).color(MUTED));
        }
        ui.add_space(6.0);
        if ui.button("Close").clicked() {
            *open = false;
        }
    });
    if !loot_open {
        session.close_loot();
    }
}

pub fn draw_world_loot(session: &mut WorldSession, world: &mut World, frame: &Frame) {
    draw_loot_windows(session, world, frame);
}
