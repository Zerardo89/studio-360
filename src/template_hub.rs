//! Accessible template covers drawn with egui geometry.

use std::time::Duration;

use eframe::egui::{
    self, pos2, vec2, Align2, Color32, FontId, Painter, Rect, Response, Stroke, StrokeKind, Ui,
};

use crate::{
    autoedit::Segment,
    movie::{self, TargetDuration, Template},
    playback::format_time,
    templates::{Category, CoverStyle, TemplateInfo, TemplateKind, FEATURED},
    theme,
};

const CARD_HEIGHT: f32 = 102.0;
const ANIMATION_TICK: Duration = Duration::from_millis(45);

pub struct Hub {
    category: Category,
    pub selected: Template,
    reveal: bool,
    pub scroll_to_heading: bool,
}

impl Default for Hub {
    fn default() -> Self {
        Self {
            category: Category::Featured,
            selected: Template::Dynamic,
            reveal: false,
            scroll_to_heading: false,
        }
    }
}

impl Hub {
    pub fn select(&mut self, template: Template) {
        self.selected = template;
        self.category = if FEATURED.contains(&template) {
            Category::Featured
        } else {
            template.info().category
        };
        self.reveal = true;
    }

    pub fn show(
        &mut self,
        ui: &mut Ui,
        active: Option<Template>,
        segments: &[Segment],
        target: TargetDuration,
        animate: bool,
    ) {
        let heading = ui.label(egui::RichText::new("Modelli").size(18.0).strong());
        if std::mem::take(&mut self.scroll_to_heading) {
            heading.scroll_to_me_animation(
                Some(egui::Align::TOP),
                egui::style::ScrollAnimation::none(),
            );
        }
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().button_padding = vec2(8.0, 3.0);
            ui.spacing_mut().interact_size.y = 23.0;
            ui.spacing_mut().item_spacing = vec2(5.0, 5.0);
            for category in Category::ALL {
                if theme::chip(ui, category.label(), category == self.category).clicked() {
                    self.category = category;
                    self.reveal = false;
                }
            }
        });

        let templates: Vec<_> = if self.category == Category::Featured {
            FEATURED.to_vec()
        } else {
            Template::ALL
                .into_iter()
                .filter(|template| template.info().category == self.category)
                .collect()
        };
        egui::ScrollArea::vertical()
            .id_salt(("galleria_modelli", self.category as usize))
            .max_height(2.0 * CARD_HEIGHT + ui.spacing().item_spacing.y)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                let width = ((ui.available_width() - ui.spacing().item_spacing.x) / 2.0).floor();
                for row in templates.chunks(2) {
                    ui.horizontal(|ui| {
                        for &template in row {
                            let current = active == Some(template);
                            let selected = self.selected == template;
                            let response = ui
                                .push_id(template as usize, |ui| {
                                    cover(ui, template, width, current, selected, animate, || {
                                        movie::build_from_template(segments, template, target)
                                            .clips
                                            .len()
                                    })
                                })
                                .inner;
                            if self.reveal && selected {
                                response.scroll_to_me(None);
                                self.reveal = false;
                            }
                            if response.clicked() {
                                self.selected = template;
                            }
                        }
                    });
                }
            });
    }
}

fn cover(
    ui: &mut Ui,
    template: Template,
    width: f32,
    active: bool,
    selected: bool,
    animate: bool,
    free_count: impl FnOnce() -> usize,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(width, CARD_HEIGHT), egui::Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::Role::Button,
            ui.is_enabled(),
            selected,
            template.label(),
        )
    });
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let info = template.info();
    let (count, duration, sync) = match info.kind {
        TemplateKind::Free => (free_count(), "Libera".to_owned(), false),
        TemplateKind::Sync { slots, seconds, .. } => (slots.len(), format_time(seconds), true),
    };
    let stroke = if active {
        Stroke::new(2.0, theme::ACCENT)
    } else if selected || response.has_focus() {
        Stroke::new(1.5, theme::TEXT)
    } else if response.hovered() {
        Stroke::new(1.0, theme::MUTED)
    } else {
        Stroke::new(1.0, theme::LINE)
    };
    let painter = ui.painter();
    painter.rect_filled(rect, 8, if selected { theme::CARD } else { theme::SIDE });
    painter.rect_stroke(rect, 8, stroke, StrokeKind::Inside);
    if active && response.has_focus() {
        painter.rect_stroke(
            rect.shrink(3.0),
            5,
            Stroke::new(1.0, theme::TEXT),
            StrokeKind::Inside,
        );
    }
    let art = Rect::from_min_max(
        rect.min + vec2(5.0, 5.0),
        pos2(rect.right() - 5.0, rect.top() + 51.0),
    );
    let shift = if animate && response.hovered() {
        ui.ctx().request_repaint_after(ANIMATION_TICK);
        ui.input(|input| (input.time * 1.3).sin() as f32 * 4.0)
    } else {
        0.0
    };
    paint_art(&painter.with_clip_rect(art), art, info, shift);
    if sync {
        let badge = Rect::from_min_size(art.right_top() + vec2(-35.0, 4.0), vec2(31.0, 16.0));
        painter.rect_filled(badge, 3, theme::BG);
        painter.text(
            badge.center(),
            Align2::CENTER_CENTER,
            "Sync",
            FontId::proportional(10.0),
            theme::TEXT,
        );
    }
    let title = painter.layout(
        template.label().to_owned(),
        FontId::proportional(13.0),
        theme::TEXT,
        rect.width() - 15.0,
    );
    painter.galley(rect.min + vec2(7.0, 54.0), title, theme::TEXT);
    painter.text(
        rect.left_bottom() + vec2(7.0, -6.0),
        Align2::LEFT_BOTTOM,
        format!("{count} clip · {duration}"),
        FontId::proportional(11.0),
        if active { theme::ACCENT } else { theme::MUTED },
    );
    response.on_hover_text(if active {
        format!("{} · In uso", template.label())
    } else {
        template.label().to_owned()
    })
}

fn paint_art(painter: &Painter, rect: Rect, info: &TemplateInfo, shift: f32) {
    let [dark, mid, light] = info.colors.map(|[r, g, b]| Color32::from_rgb(r, g, b));
    let mut gradient = egui::Mesh::default();
    gradient.colored_vertex(rect.left_top(), mid);
    gradient.colored_vertex(rect.right_top(), dark);
    gradient.colored_vertex(rect.right_bottom(), mid);
    gradient.colored_vertex(rect.left_bottom(), dark);
    gradient.add_triangle(0, 1, 2);
    gradient.add_triangle(0, 2, 3);
    painter.add(egui::Shape::mesh(gradient));

    let line = Stroke::new(1.0, light.gamma_multiply(0.72));
    let soft = Stroke::new(1.0, light.gamma_multiply(0.34));
    match info.style {
        CoverStyle::Stripes => {
            for i in -2..9 {
                let x = rect.left() + i as f32 * 16.0 + shift;
                let from = pos2(x, rect.bottom());
                let to = pos2(x + 30.0, rect.top());
                painter.line_segment([from, to], if i % 3 == 0 { line } else { soft });
                if i % 3 == 0 {
                    painter.line_segment(
                        [from + vec2(4.0, 0.0), to + vec2(4.0, 0.0)],
                        Stroke::new(3.0, mid),
                    );
                }
            }
        }
        CoverStyle::Rings => {
            let center = pos2(
                rect.left() + rect.width() * 0.62 + shift,
                rect.bottom() - 9.0,
            );
            painter.circle_filled(center, 6.0, light);
            painter.circle_stroke(center, 11.0, line);
            painter.circle_stroke(center, 20.0, soft);
            painter.circle_stroke(center, 30.0, Stroke::new(4.0, light.gamma_multiply(0.38)));
            painter.circle_stroke(center, 37.0, soft);
            painter.line_segment(
                [pos2(rect.left(), center.y), center - vec2(11.0, 0.0)],
                soft,
            );
        }
        CoverStyle::Waves => {
            for band in 0..5 {
                let points = (0..=24)
                    .map(|step| {
                        let x = rect.left() + rect.width() * step as f32 / 24.0;
                        let y = rect.top()
                            + 12.0
                            + band as f32 * 7.0
                            + ((x - rect.left() - shift) * 0.056 + band as f32 * 0.3).sin() * 6.0;
                        pos2(x, y)
                    })
                    .collect();
                painter.add(egui::Shape::line(
                    points,
                    if band == 1 || band == 3 { line } else { soft },
                ));
            }
        }
        CoverStyle::Grid => {
            for column in -1_i32..8 {
                for row in 0..3 {
                    let origin = rect.left_top()
                        + vec2(4.0 + 16.0 * column as f32 + shift, 3.0 + 15.0 * row as f32);
                    let cell = Rect::from_min_size(origin, vec2(13.0, 12.0));
                    if (column + 2 * row).rem_euclid(5) == 0 {
                        painter.rect_filled(cell, 1, light.gamma_multiply(0.45));
                    } else {
                        painter.rect_stroke(cell, 1, soft, StrokeKind::Inside);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focused_cover_activates_with_enter() {
        let ctx = egui::Context::default();
        let draw = |ui: &mut Ui| cover(ui, Template::UrbanPulse, 116.0, false, false, false, || 0);
        ctx.run_ui(egui::RawInput::default(), |ui| draw(ui).request_focus())
            .textures_delta
            .clear();

        let mut activated = false;
        ctx.run_ui(
            egui::RawInput {
                events: vec![egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..Default::default()
            },
            |ui| activated |= draw(ui).clicked(),
        )
        .textures_delta
        .clear();
        assert!(activated);
    }
}
