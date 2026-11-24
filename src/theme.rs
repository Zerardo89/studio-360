use eframe::egui::{self, Color32, Pos2, Rect, RichText, Stroke, StrokeKind, Ui, Vec2};

pub const BG: Color32 = Color32::from_rgb(18, 19, 22);
pub const SIDE: Color32 = Color32::from_rgb(23, 24, 28);
pub const CARD: Color32 = Color32::from_rgb(30, 32, 37);
pub const LINE: Color32 = Color32::from_rgb(48, 51, 59);
pub const TEXT: Color32 = Color32::from_rgb(239, 240, 243);
pub const MUTED: Color32 = Color32::from_rgb(168, 174, 186);
pub const ACCENT: Color32 = Color32::from_rgb(232, 189, 0);
pub const ERROR: Color32 = Color32::from_rgb(255, 151, 139);
pub const OK: Color32 = Color32::from_rgb(142, 217, 173);

pub fn configure(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let emoji = crate::overlay_editor::EMOJI_FONT;
    fonts.font_data.insert(
        emoji.into(),
        std::sync::Arc::new(egui::FontData::from_static(
            epaint_default_fonts::NOTO_EMOJI_REGULAR,
        )),
    );
    fonts
        .families
        .insert(egui::FontFamily::Name(emoji.into()), vec![emoji.into()]);
    ctx.set_fonts(fonts);
    ctx.set_theme(egui::Theme::Dark);
    ctx.style_mut_of(egui::Theme::Dark, |style| {
        let v = &mut style.visuals;
        v.override_text_color = Some(TEXT);
        v.panel_fill = BG;
        v.window_fill = SIDE;
        v.extreme_bg_color = BG;
        v.faint_bg_color = CARD;
        v.selection.bg_fill = Color32::from_rgb(69, 60, 28);
        v.selection.stroke = Stroke::new(1.0, ACCENT);
        v.warn_fg_color = ACCENT;
        v.error_fg_color = ERROR;
        v.slider_trailing_fill = true;
        for (widget, fill) in [
            (&mut v.widgets.noninteractive, SIDE),
            (&mut v.widgets.inactive, CARD),
            (&mut v.widgets.hovered, Color32::from_rgb(46, 48, 55)),
            (&mut v.widgets.active, Color32::from_rgb(63, 58, 35)),
            (&mut v.widgets.open, CARD),
        ] {
            widget.corner_radius = 8.into();
            widget.bg_fill = fill;
            widget.weak_bg_fill = fill;
            widget.bg_stroke = Stroke::new(1.0, LINE);
            widget.fg_stroke = Stroke::new(1.0, TEXT);
            widget.expansion = 0.0;
        }
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.spacing.interact_size.y = 30.0;
        style
            .text_styles
            .get_mut(&egui::TextStyle::Body)
            .map(|font| font.size = 14.0);
        style
            .text_styles
            .get_mut(&egui::TextStyle::Button)
            .map(|font| font.size = 14.0);
    });
}

pub fn panel(fill: Color32, margin: i8) -> egui::Frame {
    egui::Frame::new().fill(fill).inner_margin(margin)
}

pub fn card(margin: i8) -> egui::Frame {
    panel(CARD, margin)
        .stroke(Stroke::new(1.0, LINE))
        .corner_radius(8)
}

pub fn title(ui: &mut Ui, title: &str, subtitle: &str) {
    ui.label(RichText::new(title).size(32.0).strong());
    if !subtitle.is_empty() {
        muted(ui, subtitle);
    }
    ui.add_space(12.0);
}

pub fn heading(ui: &mut Ui, label: &str) {
    ui.label(RichText::new(label).size(18.0).strong());
}

pub fn muted(ui: &mut Ui, label: impl Into<String>) {
    ui.label(RichText::new(label).color(MUTED));
}

pub fn primary(label: &str) -> egui::Button<'_> {
    egui::Button::new(RichText::new(label).color(BG).strong())
        .fill(ACCENT)
        .stroke(Stroke::NONE)
        .corner_radius(8)
}

pub fn chip(ui: &mut Ui, label: &str, active: bool) -> egui::Response {
    let button = egui::Button::new(RichText::new(label).color(if active { ACCENT } else { TEXT }))
        .selected(active)
        .corner_radius(8);
    ui.add(button)
}

pub fn combo(ui: &mut Ui, id: &str, value: &mut usize, options: &[&str]) -> bool {
    let previous = *value;
    egui::ComboBox::from_id_salt(id)
        .selected_text(options[*value])
        .show_ui(ui, |ui| {
            for (index, name) in options.iter().copied().enumerate() {
                ui.selectable_value(value, index, name);
            }
        });
    previous != *value
}

pub fn horizontal_scroll(ui: &mut Ui, id: &str, content: impl FnOnce(&mut Ui)) {
    ui.scope(|ui| {
        let scroll = &mut ui.spacing_mut().scroll;
        scroll.floating = false;
        scroll.bar_width = 5.0;
        scroll.bar_inner_margin = 6.0;
        egui::ScrollArea::horizontal()
            .id_salt(id)
            .auto_shrink([false, true])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
            .show(ui, |ui| ui.horizontal(content));
    });
}

pub fn fit(bounds: Rect, aspect: f32) -> Rect {
    let width = bounds.width().min(bounds.height() * aspect);
    Rect::from_center_size(bounds.center(), egui::vec2(width, width / aspect))
}

pub fn paint_image(ui: &Ui, rect: Rect, texture: &egui::TextureHandle, uv: Rect) {
    ui.painter().image(texture.id(), rect, uv, Color32::WHITE);
}

pub const FULL_UV: Rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));

pub fn cover_uv(texture_aspect: f32, frame_aspect: f32) -> Rect {
    let scale = if texture_aspect > frame_aspect {
        Vec2::new(frame_aspect / texture_aspect, 1.0)
    } else {
        Vec2::new(1.0, texture_aspect / frame_aspect)
    };
    Rect::from_center_size(FULL_UV.center(), scale)
}

pub fn image_frame(ui: &Ui, rect: Rect) {
    ui.painter()
        .rect_stroke(rect, 8, Stroke::new(1.0, LINE), StrokeKind::Inside);
}

pub fn nav_icon(ui: &Ui, rect: Rect, icon: usize, color: Color32) {
    let p = ui.painter();
    let s = Stroke::new(1.5, color);
    let at = |x, y| rect.min + egui::vec2(x, y);
    let line = |points: &[(f32, f32)]| {
        p.add(egui::Shape::line(
            points.iter().map(|&(x, y)| at(x, y)).collect(),
            s,
        ));
    };
    let outline = |x, y, w, h| {
        p.rect_stroke(
            Rect::from_min_size(at(x, y), egui::vec2(w, h)),
            2,
            s,
            StrokeKind::Middle,
        );
    };
    match icon {
        0 => {
            line(&[(1., 8.), (9., 1.), (17., 8.)]);
            line(&[
                (3., 7.),
                (3., 16.),
                (7., 16.),
                (7., 11.),
                (11., 11.),
                (11., 16.),
                (15., 16.),
                (15., 7.),
            ]);
        }
        1 => {
            outline(1., 2., 16., 14.);
            line(&[(2., 14.), (7., 9.), (10., 12.), (13., 8.), (17., 12.)]);
            p.circle_filled(at(6., 6.), 1.3, color);
        }
        2 => {
            outline(1., 5., 16., 11.);
            line(&[(5., 5.), (7., 2.), (11., 2.), (13., 5.)]);
            p.circle_stroke(at(9., 10.5), 2.8, s);
        }
        3 => {
            for (y, x) in [(4., 7.), (9., 12.), (14., 5.)] {
                line(&[(1., y), (17., y)]);
                p.circle(at(x, y), 2.2, SIDE, s);
            }
        }
        _ => {
            p.circle_stroke(at(9., 9.), 5.5, s);
            p.circle_stroke(at(9., 9.), 2.1, s);
            for index in 0..8 {
                let angle = index as f32 * std::f32::consts::FRAC_PI_4;
                let v = egui::Vec2::angled(angle);
                p.line_segment([at(9., 9.) + 5.5 * v, at(9., 9.) + 8.0 * v], s);
            }
        }
    }
}
