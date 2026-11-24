use std::{
    fs, io,
    path::PathBuf,
    time::{Duration, Instant},
};

use eframe::egui::{self, Color32, RichText, Stroke, StrokeKind, Ui};

use crate::{
    app::Job,
    online::{self, Online},
    theme,
};

const LANGUAGES: &[&str] = &[
    "English",
    "Italiano",
    "Deutsch",
    "Español",
    "Français",
    "日本語",
    "한국어",
    "中文",
];
const ITALIAN: usize = 1;
const DEVICE: &[&str] = &[
    "Connetti dispositivo",
    "Wi-Fi della fotocamera",
    "Memoria",
    "Versione firmware",
    "Informazioni",
    "Calibrazione giroscopio",
    "Spegnimento automatico",
    "Suono",
    "Spia",
    "Anti-sfarfallio",
    "Ripristino di fabbrica",
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Preferences {
    language: usize,
    pub performance: bool,
    pub compatibility: bool,
    pub watermark: bool,
    pub online: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            language: ITALIAN,
            performance: false,
            compatibility: false,
            watermark: false,
            online: true,
        }
    }
}

#[derive(Default)]
pub struct SettingsAction {
    pub clear_cache: bool,
    pub open_export: bool,
    pub changed: bool,
}

pub struct Settings {
    pub preferences: Preferences,
    path: Option<PathBuf>,
    error: Option<String>,
    cache_cleared: bool,
    online_cache_size: Option<u64>,
    online_cache_job: Option<Job<Result<u64, online::Error>>>,
    online_cache_check: Option<Instant>,
    online_cache_error: Option<String>,
    online_cleared: bool,
}

impl Settings {
    pub fn new(read_disk: bool) -> Self {
        let path = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .map(|folder| folder.join("Studio360").join("settings.json"));
        let mut result = Self {
            preferences: Preferences::default(),
            path,
            error: None,
            cache_cleared: false,
            online_cache_size: None,
            online_cache_job: None,
            online_cache_check: None,
            online_cache_error: None,
            online_cleared: false,
        };
        if read_disk {
            result.load();
        }
        result
    }

    fn load(&mut self) {
        let Some(path) = &self.path else {
            self.error = Some("Cartella APPDATA non disponibile.".into());
            return;
        };
        match fs::read_to_string(path) {
            Ok(text) => match read_preferences(&text) {
                Some(preferences) => self.preferences = preferences,
                None => self.error = Some(
                    "Il file delle impostazioni non è valido. Sono in uso i valori predefiniti."
                        .into(),
                ),
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => self.error = Some(format!("Impossibile leggere le impostazioni: {e}")),
        }
    }

    pub fn save(&mut self) {
        self.error = self.write().err();
    }

    fn write(&self) -> Result<(), String> {
        let path = self
            .path
            .as_ref()
            .ok_or("Cartella APPDATA non disponibile.")?;
        let parent = path
            .parent()
            .ok_or("Cartella delle impostazioni non valida.")?;
        let prefs = self.preferences;
        let text = format!(
            "{{\n  \"language\": \"{}\",\n  \"performance\": {},\n  \"compatibility\": {},\n  \"watermark\": {},\n  \"online\": {}\n}}\n",
            LANGUAGES[prefs.language], prefs.performance, prefs.compatibility, prefs.watermark, prefs.online,
        );
        let temporary = parent.join(format!("settings-{}.tmp", std::process::id()));
        let result = fs::create_dir_all(parent)
            .and_then(|()| fs::write(&temporary, text))
            .and_then(|()| fs::rename(&temporary, path));
        if let Err(error) = result {
            let _ = fs::remove_file(temporary);
            return Err(format!("Impossibile salvare le impostazioni: {error}"));
        }
        Ok(())
    }

    pub fn show(&mut self, ui: &mut Ui, online: &Online) -> SettingsAction {
        let before = self.preferences;
        let mut action = SettingsAction::default();
        theme::title(
            ui,
            "Impostazioni",
            "Preferenze di Studio 360 e controlli della fotocamera.",
        );
        if let Some(error) = &self.error {
            ui.colored_label(theme::ERROR, error);
        }
        egui::ScrollArea::vertical()
            .id_salt("preferenze")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.columns(2, |columns| {
                    self.app_card(&mut columns[0], &mut action);
                    columns[0].add_space(12.0);
                    self.online_card(&mut columns[0], online);
                    device_card(&mut columns[1]);
                });
            });
        action.changed = self.preferences != before;
        action
    }

    fn online_card(&mut self, ui: &mut Ui, online: &Online) {
        if let Some(result) = Job::take(&mut self.online_cache_job) {
            match result {
                Ok(bytes) => {
                    self.online_cache_size = Some(bytes);
                    self.online_cache_error = None;
                }
                Err(error) => {
                    self.online_cleared = false;
                    self.online_cache_error = Some(error.to_string());
                }
            }
        }
        if self.online_cache_job.is_none()
            && self
                .online_cache_check
                .is_none_or(|check| check.elapsed() >= Duration::from_secs(3))
        {
            self.online_cache_check = Some(Instant::now());
            let online = online.clone();
            self.online_cache_job = Some(Job::start(ui.ctx(), move || online.cache_size()));
        }
        ui.ctx().request_repaint_after(Duration::from_secs(3));
        theme::card(16).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            theme::heading(ui, "Contenuti online");
            ui.checkbox(
                &mut self.preferences.online,
                "Usa internet per musiche ed emoji",
            );
            if let Some(bytes) = self.online_cache_size {
                theme::muted(
                    ui,
                    if bytes < 1024 {
                        format!("Cache online · {bytes} B")
                    } else if bytes < 1024 * 1024 {
                        format!("Cache online · {:.1} KiB", bytes as f64 / 1024.0)
                    } else {
                        format!("Cache online · {:.1} MiB", bytes as f64 / 1_048_576.0)
                    },
                );
            }
            if ui
                .add_enabled(
                    self.online_cache_job.is_none(),
                    egui::Button::new("Svuota cache"),
                )
                .clicked()
            {
                let online = online.clone();
                self.online_cache_check = Some(Instant::now());
                self.online_cache_job = Some(Job::start(ui.ctx(), move || online.clear_cache()));
                self.online_cleared = true;
            }
            if let Some(error) = &self.online_cache_error {
                ui.colored_label(theme::ERROR, error);
            } else if self.online_cleared
                && self.online_cache_job.is_none()
                && self.online_cache_size == Some(0)
            {
                ui.colored_label(theme::OK, "Cache online svuotata.");
            }
            ui.separator();
            ui.hyperlink_to(
                "Openverse · CC BY, CC BY-SA, CC0 e PDM",
                "https://api.openverse.org/v1/",
            );
            ui.hyperlink_to(online::TWEMOJI_CREDIT, online::TWEMOJI_LICENSE);
        });
    }

    fn app_card(&mut self, ui: &mut Ui, action: &mut SettingsAction) {
        theme::card(16).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            theme::heading(ui, "App");
            ui.separator();
            row(ui, "Lingua", |ui| {
                theme::combo(ui, "lingua", &mut self.preferences.language, LANGUAGES);
            });
            if self.preferences.language != ITALIAN {
                theme::muted(ui, "L’interfaccia è disponibile in italiano.");
            }
            row(ui, "Svuota cache", |ui| {
                if ui.button("Svuota").clicked() {
                    action.clear_cache = true;
                    self.cache_cleared = true;
                }
            });
            if self.cache_cleared {
                ui.colored_label(theme::OK, "Cache delle miniature svuotata.");
            }
            row(ui, "Tempo di esportazione", |ui| {
                if ui.button("Apri esportazione").clicked() {
                    action.open_export = true;
                }
            });
            ui.add_space(2.0);
            toggle(
                ui,
                "Modalità prestazioni",
                &mut self.preferences.performance,
            );
            toggle(
                ui,
                "Modalità compatibilità",
                &mut self.preferences.compatibility,
            );
            toggle(ui, "Filigrana", &mut self.preferences.watermark);
            ui.add_space(6.0);
            ui.separator();
            ui.horizontal(|ui| {
                ui.label("Info");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    theme::muted(ui, concat!("Studio 360 · ", env!("CARGO_PKG_VERSION")));
                });
            });
        });
    }
}

fn row(ui: &mut Ui, label: &str, content: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), content);
    });
}

fn device_card(ui: &mut Ui) {
    theme::card(16).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        theme::heading(ui, "Dispositivo");
        theme::muted(ui, "Richiede una fotocamera connessa");
        ui.separator();
        for label in DEVICE {
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), 28.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| ui.add_enabled(false, egui::Button::new(RichText::new(*label)).frame(false)),
            );
        }
    });
}

fn toggle(ui: &mut Ui, label: &str, value: &mut bool) {
    ui.push_id(label, |ui| {
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 29.0), egui::Sense::click());
        if response.clicked() {
            *value = !*value;
        }
        response
            .widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, true, *value, label));
        let track = egui::Rect::from_center_size(
            rect.right_center() - egui::vec2(20.0, 0.0),
            egui::vec2(38.0, 20.0),
        );
        let fill = if *value {
            theme::ACCENT
        } else if response.hovered() {
            theme::LINE
        } else {
            theme::SIDE
        };
        ui.painter().rect(
            track,
            8,
            fill,
            Stroke::new(1.0, theme::LINE),
            StrokeKind::Inside,
        );
        let offset = if *value { 8.0 } else { -8.0 };
        ui.painter().circle_filled(
            track.center() + egui::vec2(offset, 0.0),
            6.5,
            if *value {
                theme::BG
            } else {
                Color32::LIGHT_GRAY
            },
        );
        ui.painter().text(
            rect.left_center(),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(14.0),
            theme::TEXT,
        );
        if response.has_focus() {
            ui.painter().rect_stroke(
                rect.expand(2.0),
                8,
                Stroke::new(1.0, theme::ACCENT),
                StrokeKind::Outside,
            );
        }
    });
}

// This file uses a fixed schema written by the application. Each field is a known
// language name or a JSON boolean.
fn read_preferences(text: &str) -> Option<Preferences> {
    let fields = text.trim().strip_prefix('{')?.strip_suffix('}')?;
    let mut result = Preferences::default();
    for pair in fields.split(',').filter(|part| !part.trim().is_empty()) {
        let (key, value) = pair.split_once(':')?;
        let value = value.trim();
        match key.trim() {
            "\"language\"" => {
                let language = value.strip_prefix('"')?.strip_suffix('"')?;
                result.language = LANGUAGES.iter().position(|&name| name == language)?;
            }
            "\"performance\"" => result.performance = value.parse().ok()?,
            "\"compatibility\"" => result.compatibility = value.parse().ok()?,
            "\"watermark\"" => result.watermark = value.parse().ok()?,
            "\"online\"" => result.online = value.parse().ok()?,
            _ => {}
        }
    }
    Some(result)
}
