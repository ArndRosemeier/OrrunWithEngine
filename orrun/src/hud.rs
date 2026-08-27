//! Visible combat chrome: one hotbar row and a short always-hit log.

use engine::egui::{self, Color32, Sense, StrokeKind};
use engine::world::World;
use engine::Frame;
use glam::{Vec3, Vec4};

use crate::combat::CombatHudSnapshot;
use crate::controls::KeyBinds;

pub fn draw_hotbar(ctx: &egui::Context, combat: &CombatHudSnapshot, binds: &KeyBinds) {
    let screen = ctx.screen_rect();
    let slot = 64.0;
    let gap = 6.0;
    let roster = combat.actions();
    let n = roster.len() as f32;
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
                for action in roster {
                    let action_id = action.id();
                    let gated = binds.get(action_id).is_none();
                    let cd = action.cooldown_fraction();
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
                        binds.display(action_id),
                        egui::FontId::proportional(18.0),
                        key_col,
                    );
                    ui.painter().text(
                        rect.center() + egui::vec2(0.0, 12.0),
                        egui::Align2::CENTER_CENTER,
                        action.name(),
                        egui::FontId::proportional(10.0),
                        key_col,
                    );
                }
            });
        });
}

pub fn draw_combat_log(ctx: &egui::Context, combat: &CombatHudSnapshot) {
    let screen = ctx.screen_rect();
    let lines: Vec<&str> = combat.log_lines().iter().map(String::as_str).collect();
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

pub fn draw_target_frame(ctx: &egui::Context, combat: &CombatHudSnapshot) {
    if combat.is_dead() {
        return;
    }
    let Some(h) = combat.locked_actor().filter(|actor| actor.is_alive()) else {
        return;
    };
    let max = h.hp_max().max(1.0);
    let frac = (h.hp() / max).clamp(0.0, 1.0) as f32;
    let fill = if frac <= 0.20 {
        Color32::from_rgb(200, 32, 32)
    } else if frac <= 0.50 {
        Color32::from_rgb(220, 190, 32)
    } else {
        Color32::from_rgb(40, 180, 64)
    };
    let name_color = Color32::WHITE;
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
                        egui::RichText::new(h.name().to_string())
                            .size(16.0)
                            .color(name_color),
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

/// One on-screen nameplate for playtester JSON and HUD draw.
#[derive(Clone, Debug)]
pub struct NameplateInfo {
    pub actor_id: crate::combat::ActorId,
    pub name: String,
    pub on_screen: bool,
    pub screen_x: f32,
    pub screen_y: f32,
}

const NAMEPLATE_RANGE_M: f64 = 28.0;

/// Nearby living hostiles projected to screen. Locked target is included.
pub fn nameplate_report(
    combat: &CombatHudSnapshot,
    eye: Vec3,
    view_proj: glam::Mat4,
    screen_w: f32,
    screen_h: f32,
) -> Vec<NameplateInfo> {
    let mut out = Vec::new();
    for h in combat.actors() {
        if !h.is_alive() {
            continue;
        }
        let dx = h.head_position().0 - f64::from(eye.x);
        let dz = h.head_position().2 - f64::from(eye.z);
        let dist = (dx * dx + dz * dz).sqrt();
        if dist > NAMEPLATE_RANGE_M {
            continue;
        }
        let world = Vec4::new(
            h.head_position().0 as f32,
            h.head_position().1 as f32,
            h.head_position().2 as f32,
            1.0,
        );
        let clip = view_proj * world;
        if clip.w.abs() < 1e-5 {
            continue;
        }
        let ndc = clip.truncate() / clip.w;
        let on_screen = ndc.z >= 0.0 && ndc.z <= 1.0 && ndc.x.abs() <= 1.05 && ndc.y.abs() <= 1.05;
        let sx = (ndc.x * 0.5 + 0.5) * screen_w;
        let sy = (1.0 - (ndc.y * 0.5 + 0.5)) * screen_h;
        out.push(NameplateInfo {
            actor_id: h.id(),
            name: h.name().to_owned(),
            on_screen,
            screen_x: sx,
            screen_y: sy,
        });
    }
    out
}

pub fn draw_nameplates(
    ctx: &egui::Context,
    combat: &CombatHudSnapshot,
    eye: Vec3,
    view_proj: glam::Mat4,
) {
    if combat.is_dead() {
        return;
    }
    let screen = ctx.screen_rect();
    let plates = nameplate_report(combat, eye, view_proj, screen.width(), screen.height());
    for plate in &plates {
        if !plate.on_screen {
            continue;
        }
        let color = Color32::WHITE;
        let x = plate.screen_x - 40.0;
        let y = plate.screen_y - 28.0;
        egui::Area::new(egui::Id::new(("nameplate", plate.actor_id)))
            .fixed_pos(egui::pos2(x, y))
            .order(egui::Order::Foreground)
            .interactable(false)
            .show(ctx, |ui| {
                ui.label(
                    egui::RichText::new(plate.name.clone())
                        .size(13.0)
                        .color(color),
                );
            });
    }
}

pub fn draw_cast_bar(ctx: &egui::Context, combat: &CombatHudSnapshot) {
    let Some(frac) = combat.cast_fraction() else {
        return;
    };
    let Some(label) = combat.cast_label() else {
        return;
    };
    let screen = ctx.screen_rect();
    let slot = 64.0;
    let hotbar_y = screen.height() - slot - 18.0;
    let x = (screen.width() * 0.5 - 110.0).max(12.0);
    // Sit just above the hotbar. Fail toast lives at 38% height — do not join it.
    let y = hotbar_y - 56.0;
    let fill = Color32::from_rgb(220, 150, 40);
    egui::Area::new(egui::Id::new("cast_bar"))
        .fixed_pos(egui::pos2(x, y))
        .order(egui::Order::Foreground)
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style())
                .inner_margin(egui::Margin::same(8))
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(label)
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

pub fn draw_fail_toast(ctx: &egui::Context, combat: &CombatHudSnapshot) {
    let Some(line) = combat.fail_tell() else {
        return;
    };
    let screen = ctx.screen_rect();
    egui::Area::new(egui::Id::new("fail_toast"))
        .fixed_pos(egui::pos2(
            screen.width() * 0.5 - 90.0,
            screen.height() * 0.38,
        ))
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

pub fn draw_hotbar_and_log(ctx: &egui::Context, combat: &CombatHudSnapshot, binds: &KeyBinds) {
    draw_target_frame(ctx, combat);
    draw_hotbar(ctx, combat, binds);
    draw_cast_bar(ctx, combat);
    draw_combat_log(ctx, combat);
    draw_fail_toast(ctx, combat);
}

use crate::inventory::{load_icon, EquipSlot, Family, IconAsset, Item};
use crate::world::WorldSession;
use std::cell::RefCell;
use std::collections::HashMap;

const SLOT: f32 = 56.0;
const INK: Color32 = Color32::from_rgb(235, 230, 210);
const MUTED: Color32 = Color32::from_rgb(180, 188, 196);

thread_local! {
    static ICON_TEX: RefCell<HashMap<&'static str, egui::TextureHandle>> = RefCell::new(HashMap::new());
}

fn icon_texture(ctx: &egui::Context, asset: IconAsset) -> egui::TextureHandle {
    let key = match asset {
        IconAsset::Item(family) => family.icon_file(),
        IconAsset::Shaken => "shaken.png",
    };
    ICON_TEX.with(|cell| {
        let mut map = cell.borrow_mut();
        if let Some(texture) = map.get(key) {
            return texture.clone();
        }
        let pixels = load_icon(asset)
            .unwrap_or_else(|error| panic!("required HUD icon {asset:?} failed to load: {error}"));
        let texture = ctx.load_texture(
            format!("icon-{key}"),
            egui::ColorImage::from_rgba_unmultiplied(
                [pixels.width as usize, pixels.height as usize],
                &pixels.rgba,
            ),
            egui::TextureOptions::NEAREST,
        );
        map.insert(key, texture.clone());
        texture
    })
}

/// 48x48 cracked-shield next to the HP bar when Shaken.
pub fn paint_shaken_icon(ui: &mut egui::Ui) {
    let texture = icon_texture(ui.ctx(), IconAsset::Shaken);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(48.0, 48.0), Sense::hover());
    ui.painter().image(
        texture.id(),
        rect,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        Color32::WHITE,
    );
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
        let tex = icon_texture(ui.ctx(), IconAsset::Item(item.family()));
        {
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
        let tex = icon_texture(ui.ctx(), IconAsset::Item(Family::Coin));
        ui.add(egui::Image::new((tex.id(), egui::vec2(22.0, 22.0))));
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
        ui.label(egui::RichText::new("Take / Take all").size(14.0).color(INK));
        ui.add_space(6.0);
        if let Some(pile) = session.ground_pile().cloned() {
            paint_coin_row(ui, pile.coin);
            for (i, item) in pile.items.iter().enumerate() {
                ui.horizontal(|ui| {
                    let _ = paint_item_slot(ui, Some(*item), "");
                    ui.label(egui::RichText::new(item.name()).size(15.0).color(INK));
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

pub fn draw_summon_window(session: &mut WorldSession, world: &mut World, frame: &Frame) {
    let ctx = frame.ui.ctx().clone();
    draw_skill_window(session, world, frame);
    draw_level_up_notice(&ctx, session);
    if ctx.input(|input| input.key_pressed(egui::Key::O)) {
        let open = !session.summon_open();
        session.set_summon_open(open);
        world.set_pointer_lock(false);
    }
    if !session.summon_open() {
        return;
    }

    let mobs = session.summonable_mobs();
    let mut open = true;
    let mut summon = None;
    frame
        .ui
        .modal("Summon combat mob", &mut open, |panel, modal_open| {
            let ui = panel.ui();
            ui.label(
                egui::RichText::new("Spawns one combat-backed mob 30 m ahead.")
                    .size(14.0)
                    .color(INK),
            );
            ui.label(
                egui::RichText::new(
                    "Tab acquires targets within 20 m, so approach after summoning.",
                )
                .size(13.0)
                .color(MUTED),
            );
            ui.add_space(8.0);
            for (id, name) in &mobs {
                if ui.button(format!("{name}  ({id})")).clicked() {
                    summon = Some(id.clone());
                    *modal_open = false;
                }
            }
            ui.add_space(6.0);
            if ui.button("Close").clicked() {
                *modal_open = false;
            }
        });

    let summoned = summon.is_some();
    if let Some(mob_id) = summon {
        session
            .summon_mob(world, &mob_id)
            .unwrap_or_else(|err| panic!("summon {mob_id} failed: {err}"));
    }
    session.set_summon_open(open && !summoned);
}

pub fn draw_world_loot(session: &mut WorldSession, world: &mut World, frame: &Frame) {
    draw_loot_windows(session, world, frame);
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProgressionRow {
    label: String,
    level: i32,
    xp: u64,
    xp_total: u64,
}

impl ProgressionRow {
    pub fn label(&self) -> &str {
        &self.label
    }
    pub fn level(&self) -> i32 {
        self.level
    }
    pub fn xp(&self) -> u64 {
        self.xp
    }
    pub fn xp_total(&self) -> u64 {
        self.xp_total
    }
    pub fn progress(&self) -> f32 {
        self.xp as f32 / self.xp_total as f32
    }
}

pub fn progression_report(session: &WorldSession) -> Vec<ProgressionRow> {
    session
        .combat_hud_snapshot()
        .progression()
        .iter()
        .map(|track| ProgressionRow {
            label: track.label().to_owned(),
            level: track.level(),
            xp: track.xp(),
            xp_total: track.xp_total(),
        })
        .collect()
}

fn paint_progression_row(ui: &mut egui::Ui, row: &ProgressionRow) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(row.label()).size(15.0).color(INK));
        ui.label(
            egui::RichText::new(format!("Level {}", row.level()))
                .size(14.0)
                .color(MUTED),
        );
    });
    ui.add(
        egui::ProgressBar::new(row.progress())
            .desired_width(320.0)
            .text(format!("{} / {} XP", row.xp(), row.xp_total())),
    );
    ui.add_space(5.0);
}

pub fn draw_skill_window(session: &mut WorldSession, world: &mut World, frame: &Frame) {
    let ctx = frame.ui.ctx().clone();
    if ctx.input(|input| input.key_pressed(egui::Key::K)) {
        session.set_skill_open(!session.skill_open());
        world.set_pointer_lock(false);
    }
    if !session.skill_open() {
        return;
    }
    let rows = progression_report(session);
    let mut open = true;
    frame.ui.modal("Skills", &mut open, |panel, modal_open| {
        let ui = panel.ui();
        ui.label(
            egui::RichText::new("Known skills and resource proficiencies")
                .size(13.0)
                .color(MUTED),
        );
        ui.add_space(8.0);
        for row in &rows {
            paint_progression_row(ui, row);
        }
        if ui.button("Close").clicked() {
            *modal_open = false;
        }
    });
    session.set_skill_open(open);
}

pub fn draw_level_up_notice(ctx: &egui::Context, session: &WorldSession) {
    let Some(notice) = session.current_level_up_notice() else {
        return;
    };
    let screen = ctx.screen_rect();
    egui::Area::new(egui::Id::new("level_up_notice"))
        .fixed_pos(egui::pos2(
            screen.width() * 0.5 - 130.0,
            screen.height() * 0.28,
        ))
        .order(egui::Order::Foreground)
        .interactable(false)
        .show(ctx, |ui| {
            ui.label(
                egui::RichText::new(format!(
                    "{} reached level {}",
                    notice.name(),
                    notice.level()
                ))
                .size(24.0)
                .color(Color32::from_rgb(240, 210, 100)),
            );
        });
}

#[cfg(test)]
mod progression_view_tests {
    use super::ProgressionRow;

    #[test]
    fn progression_row_reports_exact_progress() {
        let row = ProgressionRow {
            label: "Pyromancy".to_string(),
            level: 3,
            xp: 225,
            xp_total: 900,
        };
        assert_eq!(row.label(), "Pyromancy");
        assert_eq!(row.level(), 3);
        assert_eq!(row.xp(), 225);
        assert_eq!(row.xp_total(), 900);
        assert_eq!(row.progress(), 0.25);
    }
}
