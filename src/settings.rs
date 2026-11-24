use std::{fs, io, path::PathBuf};

use eframe::egui::{self, Color32, RichText, Stroke, StrokeKind, Ui};

use crate::theme;

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
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            language: ITALIAN,
            performance: false,
            compatibility: false,
            watermark: false,
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
            "{{\n  \"language\": \"{}\",\n  \"performance\": {},\n  \"compatibility\": {},\n  \"watermark\": {}\n}}\n",
            LANGUAGES[prefs.language], prefs.performance, prefs.compatibility, prefs.watermark,
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

    pub fn show(&mut self, ui: &mut Ui) -> SettingsAction {
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
                    device_card(&mut columns[1]);
                });
            });
        action.changed = self.preferences != before;
        action
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
            _ => {}
        }
    }
    Some(result)
}
