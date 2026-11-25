use std::{
    fs, io,
    path::PathBuf,
    time::{Duration, Instant},
};

use eframe::egui::{self, Color32, RichText, Stroke, StrokeKind, Ui};
use serde::{Deserialize, Serialize};

use crate::{
    ai_provider::{
        self, AiClient, AiConfig, ApiKey, ConnectionTest, Effort, ModelInfo, ProviderKind,
    },
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

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct Preferences {
    #[serde(serialize_with = "write_language", deserialize_with = "read_language")]
    language: usize,
    pub performance: bool,
    pub compatibility: bool,
    pub watermark: bool,
    pub online: bool,
    ai: AiConfig,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            language: ITALIAN,
            performance: false,
            compatibility: false,
            watermark: false,
            online: true,
            ai: AiConfig::default(),
        }
    }
}

#[derive(Default)]
pub struct SettingsAction {
    pub clear_cache: bool,
    pub open_export: bool,
    pub changed: bool,
}

#[derive(Default)]
struct ModelCatalog {
    models: Vec<ModelInfo>,
    job: Option<Job<Result<Vec<ModelInfo>, String>>>,
    result: Option<Result<Option<String>, String>>,
    last_frame: Option<u64>,
    refresh_on_open: bool,
}

impl ModelCatalog {
    fn clear(&mut self) {
        self.models.clear();
        self.job = None;
        self.result = None;
    }

    fn poll(&mut self, ctx: &egui::Context, config: &mut AiConfig) -> bool {
        let Some(result) = Job::take(&mut self.job) else {
            return false;
        };
        let result = result.map(|models| {
            let chosen = if models.iter().any(|model| model.id == config.model) {
                None
            } else {
                config.suggested_model(&models).map(|model| {
                    config.model.clone_from(&model.id);
                    model.label.clone()
                })
            };
            self.models = models;
            chosen
        });
        let changed_model = matches!(&result, Ok(Some(_)));
        self.result = Some(result);
        ctx.request_repaint();
        changed_model
    }

    fn show_status(&self, ui: &mut Ui) {
        if self.job.is_some() {
            theme::muted(ui, "Scarico l’elenco dei modelli…");
        } else if let Some(result) = &self.result {
            match result {
                Ok(chosen) => {
                    let count = self.models.len();
                    if count == 1 {
                        theme::muted(ui, "1 modello disponibile");
                    } else {
                        theme::muted(ui, format!("{count} modelli disponibili"));
                    }
                    if let Some(label) = chosen {
                        theme::muted(ui, format!("Modello scelto automaticamente: {label}"));
                    }
                }
                Err(error) => {
                    ui.colored_label(theme::ERROR, error);
                }
            }
        }
    }
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
    focus_ai: bool,
    ai_key: Result<Option<ApiKey>, String>,
    ai_key_input: String,
    ai_models: ModelCatalog,
    ai_test: Option<Job<Result<ConnectionTest, String>>>,
    ai_notice: Option<Result<&'static str, String>>,
}

impl Settings {
    pub fn new(read_disk: bool, focus_ai: bool) -> Self {
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
            focus_ai,
            ai_key: Ok(None),
            ai_key_input: String::new(),
            ai_models: ModelCatalog::default(),
            ai_test: None,
            ai_notice: None,
        };
        if read_disk {
            result.load();
            result.ai_key = result
                .key_path()
                .and_then(|path| ai_provider::load_key(&path));
        }
        result
    }

    fn load(&mut self) {
        let Some(path) = &self.path else {
            self.error = Some("Cartella APPDATA non disponibile.".into());
            return;
        };
        match fs::read_to_string(path) {
            Ok(text) => match serde_json::from_str(&text) {
                Ok(preferences) => self.preferences = preferences,
                Err(_) => self.error = Some(
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
        let key_input = self.ai_key_input.trim();
        if self
            .ai_key
            .as_ref()
            .is_ok_and(|key| key.as_ref().is_some_and(|key| self.key_in_config(key)))
            || (key_input.chars().count() >= 20
                && ApiKey::new(key_input).is_ok_and(|key| self.key_in_config(&key)))
        {
            return Err("Rimuovi la chiave dall’indirizzo o dal nome del modello.".into());
        }
        let text = serde_json::to_string_pretty(&self.preferences)
            .map_err(|_| "Impossibile preparare le impostazioni.")?;
        let temporary = parent.join(format!("settings-{}.tmp", std::process::id()));
        let result = fs::create_dir_all(parent)
            .and_then(|()| fs::write(&temporary, text + "\n"))
            .and_then(|()| fs::rename(&temporary, path));
        if let Err(error) = result {
            let _ = fs::remove_file(temporary);
            return Err(format!("Impossibile salvare le impostazioni: {error}"));
        }
        Ok(())
    }

    pub fn show(&mut self, ui: &mut Ui, online: &Online) -> SettingsAction {
        let before = self.preferences.clone();
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
                    columns[0].add_space(12.0);
                    self.ai_card(&mut columns[0]);
                    columns[0].add_space(16.0);
                    device_card(&mut columns[1]);
                });
            });
        action.changed = self.preferences != before;
        action
    }

    pub fn ai_for_auto_edit(&self) -> Result<AiClient, String> {
        if !self.preferences.online {
            return Err(ai_provider::INTERNET_DISABLED.into());
        }
        if !self.preferences.ai.enabled {
            return Err("funzione disattivata nelle impostazioni".into());
        }
        self.ai_client()
    }

    fn ai_client(&self) -> Result<AiClient, String> {
        let key = self.ai_key.as_ref().map_err(Clone::clone)?;
        let client = AiClient::new(&self.preferences.ai, key.as_ref());
        client.validate()?;
        Ok(client)
    }

    fn key_path(&self) -> Result<PathBuf, String> {
        self.path
            .as_ref()
            .map(|path| path.with_file_name("ai-key.bin"))
            .ok_or_else(|| "Cartella APPDATA non disponibile.".into())
    }

    fn key_in_config(&self, key: &ApiKey) -> bool {
        let config = &self.preferences.ai;
        key.appears_in(&config.base_url) || key.appears_in(&config.model)
    }

    fn store_ai_key(&mut self) -> Result<&'static str, String> {
        let key = ApiKey::new(&self.ai_key_input)?;
        if self.key_in_config(&key) {
            return Err("Rimuovi la chiave dall’indirizzo o dal nome del modello.".into());
        }
        ai_provider::save_key(&self.key_path()?, &key)?;
        self.ai_key = Ok(Some(key));
        Ok("Chiave API salvata.")
    }

    fn delete_ai_key(&mut self) -> Result<&'static str, String> {
        ai_provider::remove_key(&self.key_path()?)?;
        self.ai_key = Ok(None);
        Ok("Chiave API rimossa.")
    }

    fn refresh_models(&mut self, ctx: &egui::Context) {
        if !self.preferences.online {
            return;
        }
        self.ai_models.refresh_on_open = false;
        self.ai_models.result = None;
        let key = match &self.ai_key {
            Ok(key) => key.clone(),
            Err(error) => {
                self.ai_models.result = Some(Err(error.clone()));
                return;
            }
        };
        let config = self.preferences.ai.clone();
        self.ai_models.job = Some(Job::start(ctx, move || {
            ai_provider::list_models(&config, key.as_ref())
        }));
    }

    fn ai_card(&mut self, ui: &mut Ui) {
        let online = self.preferences.online;
        let frame = ui.ctx().cumulative_frame_nr();
        if self
            .ai_models
            .last_frame
            .is_none_or(|last| frame.saturating_sub(last) > 1)
        {
            self.ai_models.refresh_on_open =
                self.ai_models.models.is_empty() && self.ai_models.job.is_none();
        }
        self.ai_models.last_frame = Some(frame);
        if !online {
            self.ai_test = None;
            if self.ai_models.job.take().is_some() {
                self.ai_models.refresh_on_open = self.ai_models.models.is_empty();
            }
        }
        let card = theme::card(16).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            theme::heading(ui, "AI personale");
            if !online {
                theme::muted(ui, "Internet è disattivato. L’Auto Edit usa l’analisi locale.");
            }
            ui.add_enabled_ui(online && self.ai_test.is_none(), |ui| self.ai_controls(ui));
            if let Some(result) = Job::take(&mut self.ai_test) {
                self.ai_notice = Some(result.map(|outcome| match outcome {
                    ConnectionTest::Success => "Connessione riuscita.",
                    ConnectionTest::UsedDefaultEffort => "Connessione riuscita. Il modello non accetta il livello di ragionamento: verrà usato quello predefinito.",
                }));
                ui.ctx().request_repaint();
            }
            if self.ai_test.is_some() {
                theme::muted(ui, "Verifica della connessione…");
            } else if let Some(result) = &self.ai_notice {
                match result {
                    Ok(message) => ui.colored_label(theme::OK, *message),
                    Err(error) => ui.colored_label(theme::ERROR, error),
                };
            } else if let Err(error) = &self.ai_key {
                ui.colored_label(theme::ERROR, error);
            }
            theme::muted(
                ui,
                "Le miniature delle scene scelte vengono inviate al servizio indicato. Per i server locali (Ollama, LM Studio) la chiave può restare vuota.",
            );
        });
        if std::mem::take(&mut self.focus_ai) {
            card.response.scroll_to_me_animation(
                Some(egui::Align::BOTTOM),
                egui::style::ScrollAnimation::none(),
            );
        }
    }

    fn ai_controls(&mut self, ui: &mut Ui) {
        let config = &mut self.preferences.ai;
        ui.checkbox(&mut config.enabled, "Usa la mia AI per l’Auto Edit");
        let mut provider = config.kind as usize;
        let service = ui.label("Servizio");
        let combo = egui::ComboBox::from_id_salt("ai_provider")
            .selected_text(ProviderKind::LABELS[provider])
            .width(ui.available_width())
            .wrap()
            .show_ui(ui, |ui| {
                for (index, label) in ProviderKind::LABELS.iter().enumerate() {
                    ui.selectable_value(&mut provider, index, *label);
                }
            });
        combo.response.labelled_by(service.id);
        let kind = ProviderKind::ALL[provider];
        let provider_changed = kind != config.kind;
        if provider_changed {
            if config.base_url.trim().trim_end_matches('/') == config.kind.default_url() {
                config.base_url = kind.default_url().into();
            }
            if self
                .ai_models
                .models
                .iter()
                .any(|model| model.id == config.model)
            {
                config.model.clear();
            }
            config.kind = kind;
            self.ai_models.clear();
            self.ai_notice = None;
        }
        let address_changed = text_field(ui, "Indirizzo", &mut config.base_url).changed();
        if address_changed {
            self.ai_models.clear();
            self.ai_notice = None;
        }
        let model_selected = model_picker(ui, &self.ai_models.models, &mut config.model);
        let model_edited = text_field(ui, "Modello (manuale)", &mut config.model).changed();
        let effort_changed = effort_picker(ui, &mut config.effort);
        if model_selected || model_edited {
            if let Some(Ok(chosen)) = &mut self.ai_models.result {
                *chosen = None;
            }
        }
        if model_selected || model_edited || effort_changed {
            self.ai_notice = None;
        }
        let update_clicked = ui
            .add_enabled(
                self.ai_models.job.is_none(),
                egui::Button::new("Aggiorna elenco modelli"),
            )
            .clicked();
        let mut refresh = provider_changed || update_clicked;
        let label = ui.label("Chiave API");
        let key_input = ui
            .add(
                egui::TextEdit::singleline(&mut self.ai_key_input)
                    .desired_width(f32::INFINITY)
                    .char_limit(4096)
                    .password(true),
            )
            .labelled_by(label.id);
        text_context_menu(&key_input, None);
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    !self.ai_key_input.trim().is_empty(),
                    egui::Button::new("Salva chiave"),
                )
                .clicked()
            {
                let saved = self.store_ai_key();
                if saved.is_ok() {
                    clear_key_input(ui.ctx(), key_input.id, &mut self.ai_key_input);
                    self.ai_models.clear();
                    refresh = true;
                }
                self.ai_notice = Some(saved);
            }
            if ui
                .add_enabled(
                    !matches!(self.ai_key, Ok(None)),
                    egui::Button::new("Rimuovi chiave"),
                )
                .clicked()
            {
                let removed = self.delete_ai_key();
                if removed.is_ok() {
                    clear_key_input(ui.ctx(), key_input.id, &mut self.ai_key_input);
                    self.ai_models.clear();
                    self.ai_models.refresh_on_open = false;
                }
                self.ai_notice = Some(removed);
            }
            theme::muted(
                ui,
                if matches!(self.ai_key, Ok(Some(_))) {
                    "Chiave salvata"
                } else {
                    "Nessuna chiave"
                },
            );
        });
        let can_refresh_models = matches!(self.ai_key, Ok(Some(_)))
            || ai_provider::is_loopback_url(&self.preferences.ai.base_url);
        if ui.is_enabled() {
            let auto_refresh = self.ai_models.refresh_on_open && !address_changed;
            if can_refresh_models && (refresh || auto_refresh) {
                self.refresh_models(ui.ctx());
            }
        }
        if !can_refresh_models {
            theme::muted(ui, "Salva la chiave API per vedere l’elenco dei modelli.");
        } else if self.preferences.online {
            if self.ai_models.poll(ui.ctx(), &mut self.preferences.ai) {
                self.ai_test = None;
            }
            self.ai_models.show_status(ui);
        }
        if ui.button("Prova connessione").clicked() {
            self.ai_notice = None;
            match self.ai_client() {
                Ok(client) => {
                    self.ai_test = Some(Job::start(ui.ctx(), move || client.test_connection()))
                }
                Err(error) => self.ai_notice = Some(Err(error)),
            }
        }
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
                "Usa internet per musiche, emoji e AI personale",
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

fn model_text(model: &ModelInfo) -> egui::Atoms<'_> {
    use egui::IntoAtoms as _;

    let label = model.label.as_str();
    if model.label == model.id {
        label.into_atoms()
    } else {
        (
            label,
            RichText::new(format!("({})", model.id)).color(theme::MUTED),
        )
            .into_atoms()
    }
}

fn model_picker(ui: &mut Ui, models: &[ModelInfo], model: &mut String) -> bool {
    use egui::IntoAtoms as _;

    let label = ui.label("Modello");
    let selected = models.iter().find(|item| item.id == *model);
    let selected_text = selected.map_or_else(
        || {
            if models.is_empty() {
                "Elenco non disponibile"
            } else {
                "Scegli un modello"
            }
            .into_atoms()
        },
        model_text,
    );
    let combo = ui
        .add_enabled_ui(!models.is_empty(), |ui| {
            egui::ComboBox::from_id_salt("ai_model")
                .selected_text(selected_text)
                .width(ui.available_width())
                .wrap()
                .show_ui(ui, |ui| {
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
                    for item in models {
                        if ui
                            .selectable_label(item.id == *model, model_text(item))
                            .clicked()
                        {
                            return Some(&item.id);
                        }
                    }
                    None
                })
        })
        .inner;
    combo.response.labelled_by(label.id);
    if let Some(id) = combo.inner.flatten().filter(|&id| id != model) {
        model.clone_from(id);
        return true;
    }
    false
}

fn effort_picker(ui: &mut Ui, effort: &mut Effort) -> bool {
    const LEVELS: [(Effort, &str); 4] = [
        (Effort::Default, "Predefinito del modello"),
        (Effort::Low, "Basso"),
        (Effort::Medium, "Medio"),
        (Effort::High, "Alto"),
    ];
    let previous = *effort;
    let label = ui.label("Livello di ragionamento");
    egui::ComboBox::from_id_salt("ai_effort")
        .selected_text(LEVELS[*effort as usize].1)
        .width(ui.available_width())
        .wrap()
        .show_ui(ui, |ui| {
            for (value, label) in LEVELS {
                ui.selectable_value(effort, value, label);
            }
        })
        .response
        .labelled_by(label.id);
    theme::muted(
        ui,
        "Più alto = risposte più ragionate ma più lente e costose.",
    );
    *effort != previous
}

fn text_field(ui: &mut Ui, label: &str, text: &mut String) -> egui::Response {
    let label = ui.label(label);
    let response = ui
        .add(egui::TextEdit::singleline(text).desired_width(f32::INFINITY))
        .labelled_by(label.id);
    text_context_menu(&response, Some(text));
    response
}

fn text_context_menu(response: &egui::Response, copyable_text: Option<&str>) {
    response.context_menu(|ui| {
        if ui.button("Incolla").clicked() {
            ui.close();
            response.request_focus();
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::RequestPaste);
            ui.ctx().request_repaint();
        }
        if let Some(text) = copyable_text {
            if ui.button("Copia").clicked() {
                ui.ctx().copy_text(text.to_owned());
                ui.close();
            }
            if ui.button("Seleziona tutto").clicked() {
                ui.close();
                response.request_focus();
                if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), response.id) {
                    state
                        .cursor
                        .set_char_range(Some(egui::text::CCursorRange::two(
                            egui::text::CCursor::new(0),
                            egui::text::CCursor::new(text.chars().count()),
                        )));
                    egui::TextEdit::store_state(ui.ctx(), response.id, state);
                }
            }
        }
    });
}

fn clear_key_input(ctx: &egui::Context, id: egui::Id, text: &mut String) {
    *text = String::new();
    if let Some(mut state) = egui::TextEdit::load_state(ctx, id) {
        state.clear_undoer();
        egui::TextEdit::store_state(ctx, id, state);
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

fn write_language<S: serde::Serializer>(
    language: &usize,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(LANGUAGES[*language])
}

fn read_language<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<usize, D::Error> {
    let language = String::deserialize(deserializer)?;
    LANGUAGES
        .iter()
        .position(|&name| name == language)
        .ok_or_else(|| serde::de::Error::custom("Lingua delle impostazioni non valida."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_ignores_short_unsaved_key_fragment() {
        let directory =
            std::env::temp_dir().join(format!("studio360-settings-{}", std::process::id()));
        let path = directory.join("settings.json");
        let mut settings = Settings::new(false, false);
        settings.path = Some(path.clone());
        settings.preferences.ai.base_url = "https://example.com/v1".into();
        settings.ai_key_input = "   s   ".into();

        let result = settings.write();
        let _ = fs::remove_file(path);
        let _ = fs::remove_dir(directory);
        result.unwrap();
    }

    #[test]
    fn focused_password_field_accepts_paste() {
        let ctx = egui::Context::default();
        let mut key = String::new();
        let draw = |ui: &mut egui::Ui, key: &mut String| {
            ui.add(egui::TextEdit::singleline(key).password(true))
        };
        ctx.run_ui(egui::RawInput::default(), |ui| {
            draw(ui, &mut key).request_focus();
        })
        .textures_delta
        .clear();

        ctx.run_ui(
            egui::RawInput {
                events: vec![egui::Event::Paste("sk-prova".into())],
                ..Default::default()
            },
            |ui| {
                draw(ui, &mut key);
            },
        )
        .textures_delta
        .clear();
        assert_eq!(key, "sk-prova");
    }

    #[test]
    fn settings_keep_old_defaults_and_round_trip_ai_without_key() {
        let mut preferences: Preferences =
            serde_json::from_str(r#"{"language":"Italiano","performance":true,"online":false}"#)
                .unwrap();
        assert!(preferences.performance);
        assert!(!preferences.online);
        assert!(preferences.ai == AiConfig::default());
        preferences.ai.enabled = true;
        preferences.ai.model = "visione: \"uno,due\"".into();
        preferences.ai.effort = Effort::High;
        let json = serde_json::to_value(&preferences).unwrap();
        assert_eq!(json["ai"].as_object().unwrap().len(), 5);
        assert_eq!(json["ai"]["effort"], "high");
        assert!(serde_json::from_value::<Preferences>(json).unwrap() == preferences);
    }
}
