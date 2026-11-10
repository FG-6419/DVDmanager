//! Farben, Schriften und die kleinen wiederkehrenden Bausteine der Oberfläche.

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Rect, Response,
    RichText, Sense, Shadow, Stroke, StrokeKind, TextStyle, Ui, Vec2,
};

/// Farbpalette eines Designs.
#[derive(Clone, Copy)]
pub struct Pal {
    pub dark: bool,
    pub bg: Color32,
    pub panel: Color32,
    pub card: Color32,
    pub raised: Color32,
    pub hover: Color32,
    pub line: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub faint: Color32,
    pub accent: Color32,
    pub accent_soft: Color32,
    pub on_accent: Color32,
    pub video: Color32,
    pub audio: Color32,
    pub subs: Color32,
    pub other: Color32,
    pub good: Color32,
    pub warn: Color32,
    pub bad: Color32,
    pub track: Color32,
    pub screen: Color32,
}

pub const DARK: Pal = Pal {
    dark: true,
    bg: Color32::from_rgb(13, 15, 20),
    panel: Color32::from_rgb(18, 21, 28),
    card: Color32::from_rgb(24, 27, 36),
    raised: Color32::from_rgb(33, 37, 48),
    hover: Color32::from_rgb(38, 43, 56),
    line: Color32::from_rgb(40, 45, 58),
    text: Color32::from_rgb(236, 239, 245),
    muted: Color32::from_rgb(156, 164, 180),
    faint: Color32::from_rgb(104, 112, 128),
    accent: Color32::from_rgb(92, 137, 255),
    accent_soft: Color32::from_rgb(36, 50, 88),
    on_accent: Color32::WHITE,
    video: Color32::from_rgb(92, 137, 255),
    audio: Color32::from_rgb(173, 130, 255),
    subs: Color32::from_rgb(255, 182, 72),
    other: Color32::from_rgb(112, 122, 142),
    good: Color32::from_rgb(64, 214, 148),
    warn: Color32::from_rgb(255, 182, 72),
    bad: Color32::from_rgb(255, 104, 112),
    track: Color32::from_rgb(33, 37, 48),
    screen: Color32::from_rgb(6, 7, 10),
};

pub const LIGHT: Pal = Pal {
    dark: false,
    bg: Color32::from_rgb(241, 243, 247),
    panel: Color32::from_rgb(248, 249, 252),
    card: Color32::from_rgb(255, 255, 255),
    raised: Color32::from_rgb(236, 239, 245),
    hover: Color32::from_rgb(228, 232, 241),
    line: Color32::from_rgb(218, 223, 233),
    text: Color32::from_rgb(24, 28, 38),
    muted: Color32::from_rgb(88, 96, 112),
    faint: Color32::from_rgb(132, 140, 156),
    accent: Color32::from_rgb(52, 98, 232),
    accent_soft: Color32::from_rgb(220, 230, 255),
    on_accent: Color32::WHITE,
    video: Color32::from_rgb(52, 98, 232),
    audio: Color32::from_rgb(132, 84, 226),
    subs: Color32::from_rgb(224, 142, 24),
    other: Color32::from_rgb(142, 150, 166),
    good: Color32::from_rgb(20, 158, 100),
    warn: Color32::from_rgb(200, 122, 14),
    bad: Color32::from_rgb(216, 58, 70),
    track: Color32::from_rgb(232, 235, 242),
    screen: Color32::from_rgb(16, 18, 24),
};

pub fn semibold() -> FontFamily {
    FontFamily::Name("semibold".into())
}

/// Lädt die Windows-Systemschrift, falls vorhanden, und die Symbolschrift.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let load = |path: &str| std::fs::read(path).ok();
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
    let regular = load(&format!("{windir}\\Fonts\\segoeui.ttf"));
    let bold = load(&format!("{windir}\\Fonts\\seguisb.ttf"));

    let mut semibold_list = Vec::new();
    if let Some(bytes) = regular {
        fonts.font_data.insert("segoe".into(), FontData::from_owned(bytes).into());
        if let Some(l) = fonts.families.get_mut(&FontFamily::Proportional) {
            l.insert(0, "segoe".into());
        }
    }
    if let Some(bytes) = bold {
        fonts.font_data.insert("segoe-sb".into(), FontData::from_owned(bytes).into());
        semibold_list.push("segoe-sb".to_string());
    }
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    semibold_list.extend(fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default());
    fonts.families.insert(semibold(), semibold_list);
    ctx.set_fonts(fonts);
}

/// Überträgt die Palette auf die egui-Grundeinstellungen.
pub fn apply(ctx: &egui::Context, p: &Pal) {
    let pal = *p;
    ctx.all_styles_mut(move |s| {
        let v = &mut s.visuals;
        *v = if pal.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
        v.panel_fill = pal.bg;
        v.window_fill = pal.card;
        v.window_stroke = Stroke::new(1.0, pal.line);
        v.window_corner_radius = CornerRadius::same(14);
        v.menu_corner_radius = CornerRadius::same(10);
        v.extreme_bg_color = pal.raised;
        v.faint_bg_color = pal.panel;
        v.code_bg_color = pal.raised;
        v.override_text_color = Some(pal.text);
        v.selection.bg_fill = pal.accent;
        v.selection.stroke = Stroke::new(1.0, pal.on_accent);
        v.hyperlink_color = pal.accent;
        v.warn_fg_color = pal.warn;
        v.error_fg_color = pal.bad;
        v.window_shadow = Shadow { offset: [0, 10], blur: 34, spread: 0, color: Color32::from_black_alpha(90) };
        v.popup_shadow = Shadow { offset: [0, 6], blur: 18, spread: 0, color: Color32::from_black_alpha(70) };
        let radius = CornerRadius::same(8);
        for w in [
            &mut v.widgets.noninteractive,
            &mut v.widgets.inactive,
            &mut v.widgets.hovered,
            &mut v.widgets.active,
            &mut v.widgets.open,
        ] {
            w.corner_radius = radius;
        }
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, pal.line);
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, pal.text);
        v.widgets.inactive.bg_fill = pal.raised;
        v.widgets.inactive.weak_bg_fill = pal.raised;
        // Rahmen in allen Zuständen gleich stark (im Ruhezustand unsichtbar): egui rechnet
        // die Stärke in den Innenabstand ein; wechselt sie beim Darüberfahren, ändert sich
        // die Größe der Knöpfe und die Oberfläche "schwimmt".
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::TRANSPARENT);
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, pal.text);
        v.widgets.hovered.bg_fill = pal.hover;
        v.widgets.hovered.weak_bg_fill = pal.hover;
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, pal.line);
        v.widgets.hovered.fg_stroke = Stroke::new(1.0, pal.text);
        v.widgets.active.bg_fill = pal.accent_soft;
        v.widgets.active.weak_bg_fill = pal.accent_soft;
        v.widgets.active.bg_stroke = Stroke::new(1.0, pal.accent);
        v.widgets.active.fg_stroke = Stroke::new(1.0, pal.text);
        v.widgets.open.bg_fill = pal.hover;
        v.widgets.open.weak_bg_fill = pal.hover;
        v.widgets.open.bg_stroke = Stroke::new(1.0, pal.line);
        v.widgets.open.fg_stroke = Stroke::new(1.0, pal.text);

        s.spacing.item_spacing = Vec2::new(8.0, 8.0);
        s.spacing.button_padding = Vec2::new(12.0, 7.0);
        s.spacing.interact_size = Vec2::new(36.0, 30.0);
        s.spacing.slider_rail_height = 6.0;
        s.spacing.scroll.bar_width = 8.0;
        s.spacing.scroll.floating = true;

        s.text_styles.insert(TextStyle::Body, FontId::proportional(14.0));
        s.text_styles.insert(TextStyle::Button, FontId::proportional(14.0));
        s.text_styles.insert(TextStyle::Small, FontId::proportional(12.0));
        s.text_styles.insert(TextStyle::Heading, FontId::new(22.0, semibold()));
    });
}

/// Karte mit Rand als Grundbaustein der Flächen.
pub fn card<R>(ui: &mut Ui, p: &Pal, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0, p.line))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(14))
        .show(ui, add)
        .inner
}

/// Überschrift einer Gruppe in kleinen Großbuchstaben.
pub fn section(ui: &mut Ui, p: &Pal, icon: &str, text: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(RichText::new(icon).size(15.0).color(p.accent));
        ui.label(RichText::new(text).family(semibold()).size(13.0).color(p.muted));
    });
}

pub fn primary_button(ui: &mut Ui, p: &Pal, text: &str, enabled: bool) -> Response {
    let (fill, color) = if enabled { (p.accent, p.on_accent) } else { (p.raised, p.faint) };
    let b = egui::Button::new(RichText::new(text).family(semibold()).color(color).size(14.0))
        .fill(fill)
        .stroke(Stroke::NONE)
        .corner_radius(CornerRadius::same(9))
        .min_size(Vec2::new(0.0, 36.0));
    ui.add_enabled(enabled, b)
}

pub fn soft_button(ui: &mut Ui, p: &Pal, text: &str) -> Response {
    let b = egui::Button::new(RichText::new(text).size(14.0).color(p.text))
        .fill(p.raised)
        .stroke(Stroke::NONE)
        .corner_radius(CornerRadius::same(9))
        .min_size(Vec2::new(0.0, 36.0));
    ui.add(b)
}

pub fn icon_button(ui: &mut Ui, p: &Pal, icon: &str, tip: &str) -> Response {
    let b = egui::Button::new(RichText::new(icon).size(17.0).color(p.text))
        .fill(Color32::TRANSPARENT)
        .stroke(Stroke::NONE)
        .corner_radius(CornerRadius::same(8))
        .min_size(Vec2::new(34.0, 34.0));
    ui.add(b).on_hover_text(tip)
}

/// Segmentierte Auswahl: alle Optionen bleiben sichtbar.
pub fn segmented<T: PartialEq + Copy>(ui: &mut Ui, p: &Pal, value: &mut T, options: &[(T, &str)], width: f32) -> bool {
    let mut changed = false;
    let height = 32.0;
    let (outer, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    ui.painter().rect_filled(outer, CornerRadius::same(9), p.raised);
    let seg_w = width / options.len() as f32;
    for (i, (val, label)) in options.iter().enumerate() {
        let rect = Rect::from_min_size(
            outer.min + Vec2::new(seg_w * i as f32, 0.0),
            Vec2::new(seg_w, height),
        );
        let resp = ui.interact(rect, ui.scope_id().with(("seg", i, *label)), Sense::click());
        let selected = *value == *val;
        if selected {
            ui.painter().rect_filled(rect.shrink(2.5), CornerRadius::same(7), p.accent);
        } else if resp.hovered() {
            ui.painter().rect_filled(rect.shrink(2.5), CornerRadius::same(7), p.hover);
        }
        let color = if selected { p.on_accent } else { p.muted };
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, *label, FontId::new(13.0, semibold()), color);
        if resp.clicked() && !selected {
            *value = *val;
            changed = true;
        }
    }
    changed
}

/// Listenzeile mit Symbol, zwei Textzeilen und rechtsbündiger Größe.
#[allow(clippy::too_many_arguments)]
pub fn item_row(
    ui: &mut Ui,
    p: &Pal,
    id: impl egui::AsIdSalt,
    selected: bool,
    icon: &str,
    icon_color: Color32,
    title: &str,
    subtitle: &str,
    right_top: &str,
    right_bottom: &str,
) -> Response {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 54.0), Sense::hover());
    let resp = ui.interact(rect, ui.scope_id().with(id), Sense::click());
    let painter = ui.painter_at(rect);
    let radius = CornerRadius::same(10);
    if selected {
        painter.rect_filled(rect, radius, p.accent_soft);
        painter.rect_stroke(rect, radius, Stroke::new(1.0, p.accent), StrokeKind::Inside);
    } else if resp.hovered() {
        painter.rect_filled(rect, radius, p.hover);
    }
    let c = rect.left_center() + Vec2::new(24.0, 0.0);
    painter.text(c, Align2::CENTER_CENTER, icon, FontId::proportional(21.0), icon_color);

    let tx = rect.left() + 46.0;
    let right_w = 86.0;
    let text_w = (rect.width() - 46.0 - right_w - 10.0).max(40.0);
    let clip = Rect::from_min_size(egui::pos2(tx, rect.top()), Vec2::new(text_w, rect.height()));
    let tp = ui.painter_at(clip);
    tp.text(
        egui::pos2(tx, rect.center().y - 10.0),
        Align2::LEFT_CENTER,
        title,
        FontId::new(14.0, semibold()),
        p.text,
    );
    tp.text(
        egui::pos2(tx, rect.center().y + 10.0),
        Align2::LEFT_CENTER,
        subtitle,
        FontId::proportional(12.0),
        p.muted,
    );
    let rx = rect.right() - 12.0;
    painter.text(
        egui::pos2(rx, rect.center().y - 10.0),
        Align2::RIGHT_CENTER,
        right_top,
        FontId::new(13.0, semibold()),
        p.text,
    );
    painter.text(
        egui::pos2(rx, rect.center().y + 10.0),
        Align2::RIGHT_CENTER,
        right_bottom,
        FontId::proportional(12.0),
        p.muted,
    );
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// Gut sichtbares Häkchenfeld; liefert `true`, wenn es umgeschaltet wurde.
pub fn check(ui: &mut Ui, p: &Pal, on: &mut bool, text: &str) -> bool {
    let font = FontId::proportional(13.5);
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, p.text);
    let box_size = 18.0;
    let size = Vec2::new(box_size + 8.0 + galley.size().x, box_size.max(galley.size().y) + 4.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let boxed = Rect::from_min_size(
        egui::pos2(rect.left(), rect.center().y - box_size / 2.0),
        Vec2::splat(box_size),
    );
    let radius = CornerRadius::same(5);
    if *on {
        ui.painter().rect_filled(boxed, radius, p.accent);
        ui.painter().text(
            boxed.center(),
            Align2::CENTER_CENTER,
            egui_phosphor::regular::CHECK,
            FontId::proportional(14.0),
            p.on_accent,
        );
    } else {
        let fill = if resp.hovered() { p.hover } else { p.raised };
        ui.painter().rect_filled(boxed, radius, fill);
        ui.painter().rect_stroke(boxed, radius, Stroke::new(1.0, p.faint), StrokeKind::Inside);
    }
    let color = if ui.is_enabled() { p.text } else { p.faint };
    ui.painter().galley(
        egui::pos2(boxed.right() + 8.0, rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
    if resp.clicked() && ui.is_enabled() {
        *on = !*on;
        return true;
    }
    false
}

/// Kleine Pille mit Farbpunkt für Legenden.
pub fn legend(ui: &mut Ui, p: &Pal, color: Color32, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let (r, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
        ui.painter().rect_filled(r, CornerRadius::same(3), color);
        ui.label(RichText::new(label).size(12.5).color(p.muted));
        ui.label(RichText::new(value).size(12.5).family(semibold()).color(p.text));
    });
}
