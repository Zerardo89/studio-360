use std::sync::Arc;

use eframe::egui::{self, Color32, Rect, RichText, Stroke, StrokeKind, Ui};

use crate::{
    app::Job,
    export::Export,
    panorama::{self, Adjust, PanoRef, Projection, Region, View, ADJUST_NAMES, FRAME_ASPECT},
    theme,
};

const TABS: &[&str] = &["Visuale", "Colore", "Filtri", "Ritaglio", "Esporta"];
const FOVS: &[(&str, f32)] = &[
    ("Narrow", 60.0),
    ("Linear", 80.0),
    ("Linear+", 90.0),
    ("Wide", 100.0),
    ("UltraWide", 120.0),
    ("MegaView", 140.0),
    ("Tiny Planet", 270.0),
    ("Custom", 0.0),
];
const WIDE: usize = 3;
const TINY_PLANET: usize = 6;
const CUSTOM: usize = 7;
const FOV_MIN: f32 = 30.0;
const FOV_MAX: f32 = 160.0;
const DRAG_SENSITIVITY: f32 = 0.2;
const PITCH_LIMIT: f32 = 89.0;
const ZOOM_STEP: f32 = 5.0;
const RATIOS: &[(&str, f32)] = &[
    ("16:9", 16.0 / 9.0),
    ("9:16", 9.0 / 16.0),
    ("1:1", 1.0),
    ("4:3", 4.0 / 3.0),
    ("3:4", 3.0 / 4.0),
    ("3:2", 3.0 / 2.0),
    ("2:3", 2.0 / 3.0),
    ("5:7", 5.0 / 7.0),
    ("7:5", 7.0 / 5.0),
    ("2.35:1", 2.35),
    ("1:2.35", 1.0 / 2.35),
];
const FILTERS: &[(&str, &[(usize, f32)])] = &[
    ("Default", &[]),
    ("Film", &[(2, 12.0), (3, -15.0), (8, 18.0), (11, 15.0)]),
    ("Vintage", &[(2, -25.0), (3, -29.0), (7, -15.0), (11, 38.0)]),
    ("City", &[(2, 28.0), (3, -18.0), (9, 19.0), (11, -20.0)]),
    ("Scenery", &[(3, 18.0), (4, 28.0), (5, -19.0), (9, 14.0)]),
    ("Night", &[(0, -16.0), (6, 22.0), (11, -24.0), (12, 11.0)]),
    ("Portrait", &[(1, 9.0), (2, -13.0), (5, -10.0), (11, 10.0)]),
    ("Sport", &[(2, 25.0), (4, 18.0), (9, 18.0)]),
    ("Food", &[(1, 6.0), (3, 24.0), (11, 24.0)]),
    ("Warm", &[(3, 8.0), (11, 52.0)]),
    ("Cold", &[(11, -52.0), (12, 14.0)]),
    ("Mono", &[(3, -100.0), (2, 19.0)]),
    ("Creative", &[(3, 34.0), (8, 21.0), (12, 50.0)]),
    ("Indoor", &[(0, 12.0), (5, -22.0), (6, 19.0), (11, -16.0)]),
];

fn preset(index: usize) -> Adjust {
    let mut result = Adjust::default();
    for &(slider, value) in FILTERS[index].1 {
        result.v[slider] = value;
    }
    result.mono = FILTERS[index].0 == "Mono";
    result
}

#[derive(Clone, Copy, PartialEq)]
struct PreviewKey {
    id: u64,
    view: View,
    adjust: Adjust,
    size: (usize, usize),
}

#[derive(Default)]
pub struct Preview {
    texture: Option<egui::TextureHandle>,
    rendered: Option<PreviewKey>,
    pending: Option<Job<(PreviewKey, Vec<u8>)>>,
}

impl Preview {
    pub fn update(
        &mut self,
        ctx: &egui::Context,
        pano: &PanoRef,
        view: View,
        adjust: Adjust,
        size: (usize, usize),
    ) {
        let key = PreviewKey {
            id: pano.id,
            view,
            adjust,
            size,
        };
        if let Some((done, rgb)) = Job::take(&mut self.pending) {
            if done.id == key.id {
                let image = egui::ColorImage::from_rgb([done.size.0, done.size.1], &rgb);
                let options = egui::TextureOptions::LINEAR;
                if let Some(texture) = &mut self.texture {
                    texture.set(image, options);
                } else {
                    self.texture = Some(ctx.load_texture("anteprima", image, options));
                }
                self.rendered = Some(done);
            }
        }
        if self.pending.is_none() && self.rendered != Some(key) {
            let pano = Arc::clone(pano);
            self.pending = Some(Job::start(ctx, move || {
                let rgb =
                    panorama::render(&pano, &key.view, &key.adjust, key.size, Region::FULL, None);
                (key, rgb)
            }));
        }
    }

    pub fn ready(&self) -> bool {
        self.texture.is_some()
    }

    pub fn paint(&self, ui: &Ui, rect: Rect) {
        ui.painter().rect_filled(rect, 8, theme::SIDE);
        if let Some(texture) = &self.texture {
            theme::paint_image(ui, rect, texture, theme::FULL_UV);
        } else {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Preparazione anteprima…",
                egui::FontId::proportional(14.0),
                theme::MUTED,
            );
        }
        theme::image_frame(ui, rect);
    }
}

pub struct Editor {
    pub export: Export,
    pub load_message: Option<String>,
    pub loading: bool,
    pano: Option<PanoRef>,
    preview: Preview,
    view: View,
    gear: usize,
    adjust: Adjust,
    filter: usize,
    crop: usize,
    panel: usize,
    scroll_pixels: f32,
}

impl Editor {
    pub fn new(watermark: bool) -> Self {
        Self {
            export: Export::new(watermark),
            load_message: None,
            loading: false,
            pano: None,
            preview: Preview::default(),
            view: View::default(),
            gear: WIDE,
            adjust: Adjust::default(),
            filter: 0,
            crop: 0,
            panel: 0,
            scroll_pixels: 0.0,
        }
    }

    pub fn has_image(&self) -> bool {
        self.pano.is_some()
    }

    pub fn show_export_controls(&mut self) {
        self.panel = TABS.len() - 1;
    }

    pub fn set_image(&mut self, pano: PanoRef) {
        self.pano = Some(pano);
        self.preview = Preview::default();
        self.view = View::default();
        self.gear = WIDE;
        self.adjust = Adjust::default();
        self.filter = 0;
        self.crop = 0;
        self.scroll_pixels = 0.0;
    }

    pub fn ready(&self) -> bool {
        self.preview.ready()
    }

    pub fn show(&mut self, ui: &mut Ui, performance: bool, compatibility: bool) {
        theme::title(
            ui,
            "Modifica",
            "Inquadra il panorama e prepara l’immagine da esportare.",
        );
        if self.loading {
            theme::muted(ui, "Apertura immagine…");
        }
        if let Some(error) = &self.load_message {
            ui.colored_label(theme::ERROR, error);
        }
        let Some(pano) = self.pano.as_ref().map(Arc::clone) else {
            theme::muted(ui, "Preparazione dei panorami di esempio…");
            return;
        };
        let width = if ui.available_width() < 800.0 {
            232.0
        } else {
            268.0
        };
        egui::Panel::right("strumenti_editor")
            .exact_size(width)
            .frame(theme::card(12))
            .show(ui, |ui| {
                self.tools(ui, &pano);
            });
        egui::CentralPanel::no_frame().show(ui, |ui| {
            let width = (ui.available_width() - 12.0).max(1.0);
            let height = (ui.available_height() - 86.0).max(1.0);
            let size = theme::fit(
                Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, height)),
                FRAME_ASPECT,
            )
            .size();
            ui.vertical_centered(|ui| {
                let sense = if pano.is_panorama {
                    egui::Sense::drag()
                } else {
                    egui::Sense::hover()
                };
                let (rect, response) = ui.allocate_exact_size(size, sense);
                if pano.is_panorama {
                    self.handle_input(ui, response);
                }
                let size = if self.loading
                    || compatibility
                    || ui.input(|i| i.pointer.is_decidedly_dragging())
                {
                    (480, 270)
                } else if performance {
                    (640, 360)
                } else {
                    (960, 540)
                };
                let adjust = self.adjust.combined(&preset(self.filter));
                self.preview
                    .update(ui.ctx(), &pano, self.view, adjust, size);
                self.preview.paint(ui, rect);
                if self.preview.ready() {
                    paint_crop(ui, rect, RATIOS[self.crop].1);
                }
                ui.add_space(4.0);
                theme::card(9).show(ui, |ui| {
                    ui.set_width((rect.width() - 18.0).max(1.0));
                    ui.horizontal(|ui| {
                        ui.add(egui::Label::new(RichText::new(&pano.name).strong()).truncate())
                            .on_hover_text(&pano.name);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add_enabled(pano.is_panorama, egui::Button::new("Centra"))
                                .clicked()
                            {
                                self.view.yaw = 0.0;
                                self.view.pitch =
                                    if self.gear == TINY_PLANET { -90.0 } else { 0.0 };
                                ui.ctx().request_repaint();
                            }
                        });
                    });
                    ui.horizontal_wrapped(|ui| {
                        if pano.is_panorama {
                            theme::muted(
                                ui,
                                format!(
                                    "Yaw {:.1}°     Pitch {:.1}°     FOV {:.0}°",
                                    self.view.yaw, self.view.pitch, self.view.fov
                                ),
                            );
                        } else {
                            theme::muted(ui, format!("Foto piatta · {} × {} px", pano.w, pano.h));
                        }
                    });
                });
            });
        });
    }

    fn handle_input(&mut self, ui: &Ui, response: egui::Response) {
        let delta = response.drag_delta() * DRAG_SENSITIVITY;
        if delta != egui::Vec2::ZERO {
            self.view.yaw = (self.view.yaw - delta.x + 180.0).rem_euclid(360.0) - 180.0;
            if self.gear != TINY_PLANET {
                self.view.pitch = (self.view.pitch + delta.y).clamp(-PITCH_LIMIT, PITCH_LIMIT);
            }
        }
        let hovered = response.hovered();
        response
            .on_hover_cursor(egui::CursorIcon::Grab)
            .on_hover_text("Trascina per ruotare. Usa la rotella per lo zoom.");
        if !hovered {
            self.scroll_pixels = 0.0;
            return;
        }
        let (smooth, steps) = ui.input(|input| {
            let mut steps = 0.0;
            for event in &input.events {
                if let egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Line,
                    delta,
                    ..
                } = event
                {
                    steps += delta.y;
                }
            }
            (input.smooth_scroll_delta.y, steps)
        });
        let steps = if steps != 0.0 {
            self.scroll_pixels = 0.0;
            steps
        } else {
            // High-resolution touchpads send smaller increments than a wheel notch.
            let has_pixel_event = ui.input(|input| {
                input.events.iter().any(|event| {
                    matches!(
                        event,
                        egui::Event::MouseWheel {
                            unit: egui::MouseWheelUnit::Point,
                            ..
                        }
                    )
                })
            });
            if has_pixel_event {
                self.scroll_pixels += smooth;
            }
            let steps = (self.scroll_pixels / 24.0).trunc();
            self.scroll_pixels -= steps * 24.0;
            steps
        };
        if steps != 0.0 {
            self.select_gear(CUSTOM);
            self.view.fov = (self.view.fov - steps * ZOOM_STEP).clamp(FOV_MIN, FOV_MAX);
        }
    }

    fn select_gear(&mut self, gear: usize) {
        if self.gear == gear {
            return;
        }
        if gear == TINY_PLANET {
            self.view.projection = Projection::Stereographic;
            self.view.pitch = -90.0;
        } else {
            if self.gear == TINY_PLANET {
                self.view.pitch = 0.0;
            }
            self.view.projection = Projection::Rectilinear;
        }
        self.view.fov = if gear == CUSTOM {
            self.view.fov.clamp(FOV_MIN, FOV_MAX)
        } else {
            FOVS[gear].1
        };
        self.gear = gear;
    }

    fn tools(&mut self, ui: &mut Ui, pano: &PanoRef) {
        ui.set_min_width(ui.available_width());
        ui.horizontal_wrapped(|ui| {
            for (index, tab) in TABS.iter().enumerate() {
                if theme::chip(ui, tab, self.panel == index).clicked() {
                    self.panel = index;
                }
            }
        });
        ui.add_space(4.0);
        ui.separator();
        ui.add_space(4.0);
        egui::ScrollArea::vertical()
            .id_salt(("pannello_editor", self.panel))
            .auto_shrink([false, false])
            .show(ui, |ui| match self.panel {
                0 => self.show_view(ui, pano.is_panorama),
                1 => self.show_color(ui),
                2 => self.show_filters(ui),
                3 => self.show_crop(ui),
                _ => {
                    let adjust = self.adjust.combined(&preset(self.filter));
                    self.export
                        .show(ui, pano, self.view, adjust, RATIOS[self.crop].1);
                }
            });
    }

    fn show_view(&mut self, ui: &mut Ui, is_panorama: bool) {
        theme::heading(ui, "Campo visivo");
        if !is_panorama {
            theme::muted(ui, "La visuale 360 è disponibile per i panorami 2:1.");
        }
        ui.add_enabled_ui(is_panorama, |ui| {
            for (index, (label, fov)) in FOVS.iter().enumerate() {
                let title = if index == CUSTOM {
                    label.to_string()
                } else {
                    format!("{label} · {fov:.0}°")
                };
                let button = egui::Button::new(RichText::new(title).color(if self.gear == index {
                    theme::ACCENT
                } else {
                    theme::TEXT
                }))
                .selected(self.gear == index)
                .min_size(egui::vec2(ui.available_width(), 32.0));
                if ui.add(button).clicked() {
                    self.select_gear(index);
                }
            }
            if self.gear == CUSTOM {
                ui.add(
                    egui::Slider::new(&mut self.view.fov, FOV_MIN..=FOV_MAX)
                        .step_by(1.0)
                        .suffix("°"),
                );
            }
        });
        if self.gear == TINY_PLANET {
            theme::muted(
                ui,
                "Centro bloccato al suolo. Trascina in orizzontale per ruotare.",
            );
        }
    }

    fn show_color(&mut self, ui: &mut Ui) {
        theme::heading(ui, "Regolazioni colore");
        if ui.button("Reimposta").clicked() {
            self.adjust = Adjust::default();
        }
        for (index, name) in ADJUST_NAMES.iter().enumerate() {
            ui.label(*name);
            ui.add(egui::Slider::new(&mut self.adjust.v[index], -100.0..=100.0).step_by(1.0));
        }
    }

    fn show_filters(&mut self, ui: &mut Ui) {
        theme::heading(ui, "Gruppi di filtri");
        for (row, group) in FILTERS.chunks(2).enumerate() {
            ui.horizontal(|ui| {
                let width = (ui.available_width() - ui.spacing().item_spacing.x) * 0.5;
                for (column, (name, _)) in group.iter().enumerate() {
                    let index = row * 2 + column;
                    let selected = self.filter == index;
                    let (rect, response) =
                        ui.allocate_exact_size(egui::vec2(width, 53.0), egui::Sense::click());
                    if response.clicked() {
                        self.filter = index;
                    }
                    response.widget_info(|| {
                        egui::WidgetInfo::selected(egui::Role::Button, true, selected, *name)
                    });
                    paint_filter(ui, rect, name, preset(index), selected, response.hovered());
                }
            });
        }
    }

    fn show_crop(&mut self, ui: &mut Ui) {
        theme::heading(ui, "Rapporto di ritaglio");
        ui.horizontal_wrapped(|ui| {
            for (index, (name, _)) in RATIOS.iter().enumerate() {
                if theme::chip(ui, name, self.crop == index).clicked() {
                    self.crop = index;
                }
            }
        });
        theme::muted(ui, "L’esportazione usa l’area delimitata dalla cornice.");
    }
}

fn paint_crop(ui: &Ui, rect: Rect, aspect: f32) {
    let inner = theme::fit(rect, aspect);
    let mask = Color32::from_black_alpha(145);
    for (min, max) in [
        (rect.min, egui::pos2(inner.left(), rect.bottom())),
        (egui::pos2(inner.right(), rect.top()), rect.max),
        (egui::pos2(inner.left(), rect.top()), inner.right_top()),
        (
            inner.left_bottom(),
            egui::pos2(inner.right(), rect.bottom()),
        ),
    ] {
        ui.painter()
            .rect_filled(Rect::from_min_max(min, max), 0, mask);
    }
    ui.painter().rect_stroke(
        inner,
        0,
        Stroke::new(1.5, theme::ACCENT),
        StrokeKind::Inside,
    );
}

fn paint_filter(ui: &Ui, rect: Rect, name: &str, adjust: Adjust, selected: bool, hovered: bool) {
    let border = if selected {
        theme::ACCENT
    } else if hovered {
        theme::MUTED
    } else {
        theme::LINE
    };
    ui.painter().rect(
        rect,
        8,
        theme::SIDE,
        Stroke::new(1.0, border),
        StrokeKind::Inside,
    );
    let preview = Rect::from_min_max(
        rect.min + egui::vec2(5.0, 5.0),
        egui::pos2(rect.right() - 5.0, rect.top() + 26.0),
    );
    let swatches = [
        [0.2, 0.39, 0.57],
        [0.47, 0.58, 0.44],
        [0.68, 0.59, 0.40],
        [0.92, 0.81, 0.67],
    ];
    for (index, color) in swatches.into_iter().enumerate() {
        let segment = preview.width() / swatches.len() as f32;
        let swatch = Rect::from_min_size(
            preview.min + egui::vec2(index as f32 * segment, 0.0),
            egui::vec2(segment, preview.height()),
        );
        let [r, g, b] = adjust.apply(color).map(|c| (c * 255.0).round() as u8);
        ui.painter()
            .rect_filled(swatch, 0, Color32::from_rgb(r, g, b));
    }
    ui.painter().text(
        rect.center_bottom() - egui::vec2(0.0, 5.0),
        egui::Align2::CENTER_BOTTOM,
        name,
        egui::FontId::proportional(13.0),
        if selected { theme::ACCENT } else { theme::TEXT },
    );
}
