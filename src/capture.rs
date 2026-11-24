use std::time::Duration;

use eframe::egui::{self, Rect, RichText, Ui};

use crate::{
    editor::Preview,
    panorama::{Adjust, PanoRef, View, FRAME_ASPECT},
    theme,
};

const MODES: &[&str] = &[
    "Foto",
    "Foto 360",
    "Foto HDR",
    "Video",
    "Video HDR",
    "Active HDR",
    "Timelapse",
    "TimeShift",
    "Rallentatore",
    "Bullet Time",
    "Raffica",
    "Intervallo",
    "Registrazione in loop",
    "Scatto notturno",
    "Starlapse",
    "PureVideo",
    "Pro Video",
    "FreeFrame Video",
    "Me Mode",
    "Road Mode",
    "Cinematic",
];
const EXPOSURE: &[&str] = &["Auto", "Priorità ISO", "Priorità otturatore", "Manuale"];
const SHARPNESS: &[&str] = &["Bassa", "Media", "Alta", "Massima"];
const PANO_EXPOSURE: &[&str] = &["Disattivata", "Auto", "Standard", "Alta", "Isolata"];
const EV_LIMIT: f64 = 4.0;
const EV_STEP: f64 = 0.3;
const EV_TICKS: i32 = 14;
const ISO: &[&str] = &["100", "200", "400", "800", "1600", "3200", "6400"];
const SHUTTER: &[&str] = &[
    "1/4000 s", "1/2000 s", "1/1000 s", "1/500 s", "1/250 s", "1/125 s", "1/60 s", "1/30 s",
    "1/15 s", "1/8 s", "1/4 s", "1/2 s", "1 s",
];

pub struct Capture {
    mode: usize,
    exposure: usize,
    iso: usize,
    shutter: usize,
    sharpness: usize,
    pano_exposure: usize,
    ev_ticks: i32,
    preview: Preview,
    started: Option<f64>,
}

impl Default for Capture {
    fn default() -> Self {
        Self {
            mode: 1,
            exposure: 0,
            iso: 0,
            shutter: 6,
            sharpness: 1,
            pano_exposure: 1,
            ev_ticks: 0,
            preview: Preview::default(),
            started: None,
        }
    }
}

impl Capture {
    pub fn ready(&self) -> bool {
        self.preview.ready()
    }

    pub fn show(&mut self, ui: &mut Ui, pano: Option<&PanoRef>, performance: bool) {
        theme::title(ui, "Scatto", "Configura la modalità di acquisizione.");
        theme::horizontal_scroll(ui, "modalita_scatto", |ui| {
            for (index, mode) in MODES.iter().enumerate() {
                if theme::chip(ui, mode, self.mode == index).clicked() {
                    self.mode = index;
                }
            }
        });
        ui.add_space(12.0);
        egui::Panel::right("controlli_scatto")
            .exact_size(246.0)
            .frame(theme::card(16))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| self.controls(ui));
            });
        egui::CentralPanel::no_frame().show(ui, |ui| {
            let bounds = Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(
                    ui.available_width() - 12.0,
                    (ui.available_height() - 105.0).max(1.0),
                ),
            );
            ui.vertical_centered(|ui| {
                let (rect, _) = ui.allocate_exact_size(
                    theme::fit(bounds, FRAME_ASPECT).size(),
                    egui::Sense::hover(),
                );
                if let Some(pano) = pano {
                    let now = ui.input(|i| i.time);
                    let seconds = (now - *self.started.get_or_insert(now)).max(0.0);
                    let yaw = (((seconds * 8.0).floor() / 8.0) * 7.0 + 8.0).rem_euclid(360.0);
                    let view = View {
                        yaw: yaw as f32,
                        ..View::default()
                    };
                    let mut adjustments = Adjust::default();
                    adjustments.v[0] = ev_value(f64::from(self.ev_ticks)) as f32 * 20.0;
                    let size = if performance { (480, 270) } else { (640, 360) };
                    self.preview.update(ui.ctx(), pano, view, adjustments, size);
                    ui.ctx().request_repaint_after(Duration::from_millis(125));
                }
                self.preview.paint(ui, rect);
                let badge =
                    Rect::from_min_size(rect.min + egui::vec2(12.0, 12.0), egui::vec2(144.0, 25.0));
                ui.painter()
                    .rect_filled(badge, 6, egui::Color32::from_black_alpha(200));
                ui.painter().circle_filled(
                    badge.left_center() + egui::vec2(12.0, 0.0),
                    3.0,
                    theme::ACCENT,
                );
                ui.painter().text(
                    badge.center() + egui::vec2(6.0, 0.0),
                    egui::Align2::CENTER_CENTER,
                    "Anteprima simulata",
                    egui::FontId::proportional(12.0),
                    theme::TEXT,
                );
                ui.add_space(10.0);
                ui.add_enabled(
                    false,
                    egui::Button::new(
                        RichText::new("Connetti una fotocamera per scattare").size(15.0),
                    )
                    .min_size(egui::vec2(rect.width(), 48.0)),
                );
            });
        });
    }

    fn controls(&mut self, ui: &mut Ui) {
        theme::heading(ui, "Esposizione");
        theme::combo(ui, "esposizione_scatto", &mut self.exposure, EXPOSURE);
        ui.add_space(4.0);
        ui.label("Compensazione EV");
        ui.add(
            egui::Slider::new(&mut self.ev_ticks, -EV_TICKS..=EV_TICKS)
                .custom_formatter(|ticks, _| format!("{:.1}", ev_value(ticks)).replace('.', ","))
                .custom_parser(|text| {
                    let value: f64 = text.replace(',', ".").parse().ok()?;
                    if !value.is_finite() {
                        return None;
                    }
                    let ticks = if value.abs() >= EV_LIMIT {
                        value.signum() * f64::from(EV_TICKS)
                    } else {
                        (value / EV_STEP).round()
                    };
                    Some(ticks)
                }),
        );
        if matches!(self.exposure, 1 | 3) {
            ui.label("ISO");
            theme::combo(ui, "iso_scatto", &mut self.iso, ISO);
        }
        if matches!(self.exposure, 2 | 3) {
            ui.label("Otturatore");
            theme::combo(ui, "otturatore_scatto", &mut self.shutter, SHUTTER);
        }
        ui.add_space(8.0);
        ui.separator();
        ui.add_space(6.0);
        ui.label("Nitidezza");
        theme::combo(ui, "nitidezza_scatto", &mut self.sharpness, SHARPNESS);
        ui.label("Esposizione panoramica");
        theme::combo(
            ui,
            "esposizione_panorama",
            &mut self.pano_exposure,
            PANO_EXPOSURE,
        );
    }
}

fn ev_value(ticks: f64) -> f64 {
    (ticks * EV_STEP).clamp(-EV_LIMIT, EV_LIMIT)
}
