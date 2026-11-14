//! Colors, fonts and painter-drawn icons for the browser chrome.

use std::f32::consts::{PI, TAU};

use eframe::egui::{self, pos2, vec2, Color32, Painter, Pos2, Rect, Stroke};

#[derive(Clone, Copy)]
pub struct Palette {
    /// Tab strip and window background.
    pub frame: Color32,
    /// Active tab and toolbar.
    pub toolbar: Color32,
    pub tab_hover: Color32,
    pub field: Color32,
    pub field_hover: Color32,
    pub button_hover: Color32,
    pub text: Color32,
    pub text_muted: Color32,
    pub accent: Color32,
    pub separator: Color32,
    pub page: Color32,
    pub danger: Color32,
}

impl Palette {
    pub fn new(dark: bool) -> Self {
        let hex = |rgb: u32| Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8);
        if dark {
            Palette {
                frame: hex(0x141517),
                toolbar: hex(0x222327),
                tab_hover: hex(0x1c1d20),
                field: hex(0x18191c),
                field_hover: hex(0x1d1e22),
                button_hover: hex(0x2f3035),
                text: hex(0xe6e6e9),
                text_muted: hex(0x9b9ca3),
                accent: hex(0x6ea8fe),
                separator: hex(0x2c2d32),
                page: hex(0x1a1b1e),
                danger: hex(0xc42b1c),
            }
        } else {
            Palette {
                frame: hex(0xe4e5e8),
                toolbar: hex(0xfbfbfc),
                tab_hover: hex(0xd9dade),
                field: hex(0xeeeff1),
                field_hover: hex(0xe7e8eb),
                button_hover: hex(0xe3e4e7),
                text: hex(0x1d1e21),
                text_muted: hex(0x63656c),
                accent: hex(0x2463d6),
                separator: hex(0xd5d6da),
                page: hex(0xf6f6f7),
                danger: hex(0xc42b1c),
            }
        }
    }
}

pub fn apply_style(ctx: &egui::Context, palette: &Palette) {
    ctx.global_style_mut(|style| {
        let visuals = &mut style.visuals;
        visuals.override_text_color = Some(palette.text);
        visuals.selection.bg_fill = palette.accent.gamma_multiply(0.45);
        visuals.selection.stroke = Stroke::new(1.0, palette.text);
        visuals.text_cursor.stroke = Stroke::new(1.5, palette.accent);
        visuals.window_fill = palette.toolbar;
        visuals.panel_fill = palette.frame;
        visuals.extreme_bg_color = palette.field;
        visuals.widgets.hovered.weak_bg_fill = palette.button_hover;
        visuals.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
        visuals.window_stroke = Stroke::new(1.0, palette.separator);
        style.spacing.item_spacing = vec2(6.0, 6.0);
        style.interaction.selectable_labels = false;
    });
}

/// Use the platform's UI font so the chrome feels native. Falls back to
/// egui's built-in fonts when none of the usual system fonts is present.
pub fn install_fonts(ctx: &egui::Context) {
    let ui_candidates: &[&str] = &[
        "C:\\Windows\\Fonts\\segoeui.ttf",
        "/System/Library/Fonts/SFNS.ttf",
        "/System/Library/Fonts/Helvetica.ttc",
        "/usr/share/fonts/truetype/inter/Inter-Regular.ttf",
        "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
        "/usr/share/fonts/noto/NotoSans-Regular.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
    ];
    let fallback_candidates: &[&str] = &[
        "C:\\Windows\\Fonts\\seguisym.ttf",
        "C:\\Windows\\Fonts\\msyh.ttc",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    ];
    let mut fonts = egui::FontDefinitions::default();
    let mut added = Vec::new();
    for (name, candidates) in [("ui", ui_candidates), ("fallback", fallback_candidates)] {
        if let Some(bytes) = candidates.iter().find_map(|path| std::fs::read(path).ok()) {
            fonts
                .font_data
                .insert(name.into(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
            added.push(name);
        }
    }
    if added.is_empty() {
        return;
    }
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        let list = fonts.families.entry(family.clone()).or_default();
        for (position, name) in added.iter().enumerate() {
            if family == egui::FontFamily::Proportional {
                list.insert(position, name.to_string());
            } else {
                list.push(name.to_string());
            }
        }
    }
    ctx.set_fonts(fonts);
}

fn stroke(color: Color32) -> Stroke {
    Stroke::new(1.5, color)
}

pub fn arrow(painter: &Painter, center: Pos2, color: Color32, left: bool) {
    let dir = if left { -1.0 } else { 1.0 };
    let tip = center + vec2(6.0 * dir, 0.0);
    painter.line_segment([center - vec2(6.0 * dir, 0.0), tip], stroke(color));
    painter.line_segment([tip, tip + vec2(-5.0 * dir, -5.0)], stroke(color));
    painter.line_segment([tip, tip + vec2(-5.0 * dir, 5.0)], stroke(color));
}

pub fn reload(painter: &Painter, center: Pos2, color: Color32) {
    let radius = 6.0;
    let start = -PI * 0.25;
    let points: Vec<Pos2> = (0..=24)
        .map(|i| {
            let angle = start + (i as f32 / 24.0) * (TAU * 0.8);
            center + vec2(angle.cos(), angle.sin()) * radius
        })
        .collect();
    painter.add(egui::Shape::line(points, stroke(color)));
    let head = center + vec2(start.cos(), start.sin()) * radius;
    painter.line_segment([head, head + vec2(0.5, -4.5)], stroke(color));
    painter.line_segment([head, head + vec2(-4.5, -0.5)], stroke(color));
}

pub fn cross(painter: &Painter, center: Pos2, half: f32, color: Color32) {
    painter.line_segment([center - vec2(half, half), center + vec2(half, half)], stroke(color));
    painter.line_segment([center + vec2(-half, half), center + vec2(half, -half)], stroke(color));
}

pub fn plus(painter: &Painter, center: Pos2, half: f32, color: Color32) {
    painter.line_segment([center - vec2(half, 0.0), center + vec2(half, 0.0)], stroke(color));
    painter.line_segment([center - vec2(0.0, half), center + vec2(0.0, half)], stroke(color));
}

pub fn dots(painter: &Painter, center: Pos2, color: Color32) {
    for dy in [-5.0, 0.0, 5.0] {
        painter.circle_filled(center + vec2(0.0, dy), 1.6, color);
    }
}

pub fn lock(painter: &Painter, center: Pos2, color: Color32) {
    let body = Rect::from_center_size(center + vec2(0.0, 2.0), vec2(9.0, 7.0));
    painter.rect_filled(body, 1.5, color);
    let points: Vec<Pos2> = (0..=12)
        .map(|i| {
            let angle = PI + (i as f32 / 12.0) * PI;
            pos2(center.x, center.y - 1.5) + vec2(angle.cos() * 2.8, angle.sin() * 3.2)
        })
        .collect();
    painter.add(egui::Shape::line(points, Stroke::new(1.4, color)));
}

pub fn info(painter: &Painter, center: Pos2, color: Color32) {
    painter.circle_stroke(center, 6.0, Stroke::new(1.3, color));
    painter.line_segment([center + vec2(0.0, -0.5), center + vec2(0.0, 3.0)], Stroke::new(1.5, color));
    painter.circle_filled(center + vec2(0.0, -3.0), 0.9, color);
}

pub fn search(painter: &Painter, center: Pos2, color: Color32) {
    let lens = center + vec2(-1.5, -1.5);
    painter.circle_stroke(lens, 4.5, stroke(color));
    painter.line_segment([lens + vec2(3.3, 3.3), lens + vec2(7.0, 7.0)], Stroke::new(1.8, color));
}

/// A rotating arc, driven by the frame clock.
pub fn spinner(painter: &Painter, center: Pos2, radius: f32, color: Color32, time: f64) {
    let start = (time * 5.0) as f32 % TAU;
    let points: Vec<Pos2> = (0..=16)
        .map(|i| {
            let angle = start + (i as f32 / 16.0) * (TAU * 0.7);
            center + vec2(angle.cos(), angle.sin()) * radius
        })
        .collect();
    painter.add(egui::Shape::line(points, Stroke::new(1.8, color)));
}

pub fn minimize(painter: &Painter, center: Pos2, color: Color32) {
    painter.line_segment([center - vec2(5.0, 0.0), center + vec2(5.0, 0.0)], Stroke::new(1.0, color));
}

pub fn maximize(painter: &Painter, center: Pos2, color: Color32, restored: bool) {
    let stroke = Stroke::new(1.0, color);
    if restored {
        let back = Rect::from_center_size(center + vec2(1.5, -1.5), vec2(8.0, 8.0));
        painter.line_segment([back.left_top(), back.right_top()], stroke);
        painter.line_segment([back.right_top(), back.right_bottom()], stroke);
        let front = Rect::from_center_size(center + vec2(-1.0, 1.0), vec2(8.0, 8.0));
        painter.rect_stroke(front, 0.0, stroke, egui::StrokeKind::Inside);
    } else {
        let frame = Rect::from_center_size(center, vec2(10.0, 10.0));
        painter.rect_stroke(frame, 0.0, stroke, egui::StrokeKind::Inside);
    }
}

/// A round letter badge standing in for a site icon.
pub fn monogram(painter: &Painter, center: Pos2, title: &str) {
    let letter = title
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default();
    let hue = title.bytes().fold(7u32, |acc, b| acc.wrapping_mul(31).wrapping_add(u32::from(b)));
    let tint = [
        Color32::from_rgb(0x4f, 0x8c, 0xd9),
        Color32::from_rgb(0x3f, 0xa7, 0x7a),
        Color32::from_rgb(0xd0, 0x8a, 0x2e),
        Color32::from_rgb(0xc2, 0x55, 0x5a),
        Color32::from_rgb(0x5b, 0x9a, 0xa8),
        Color32::from_rgb(0x8a, 0x8f, 0x3a),
    ][(hue % 6) as usize];
    painter.circle_filled(center, 8.0, tint);
    painter.text(
        center,
        egui::Align2::CENTER_CENTER,
        letter,
        egui::FontId::proportional(10.5),
        Color32::WHITE,
    );
}
