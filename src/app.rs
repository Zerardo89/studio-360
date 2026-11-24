use std::{
    path::PathBuf,
    sync::{mpsc, Arc, Mutex},
};

use eframe::egui::{self, RichText, Stroke, StrokeKind, Ui};

use crate::{
    album::{Album, Asset},
    capture::Capture,
    editor::Editor,
    home::{Action, Home},
    panorama::{self, PanoRef},
    settings::Settings,
    theme,
};

pub struct Job<T>(mpsc::Receiver<T>);

impl<T: Send + 'static> Job<T> {
    pub fn start(ctx: &egui::Context, work: impl FnOnce() -> T + Send + 'static) -> Self {
        let (sender, receiver) = mpsc::sync_channel(1);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = work();
            if sender.send(result).is_ok() {
                ctx.request_repaint();
            }
        });
        Self(receiver)
    }

    pub fn take(slot: &mut Option<Self>) -> Option<T> {
        match slot.as_ref()?.0.try_recv() {
            Ok(value) => {
                *slot = None;
                Some(value)
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                *slot = None;
                None
            }
            Err(mpsc::TryRecvError::Empty) => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Home,
    Album,
    Scatto,
    Modifica,
    Impostazioni,
}

impl Tab {
    const ALL: [Self; 5] = [
        Self::Home,
        Self::Album,
        Self::Scatto,
        Self::Modifica,
        Self::Impostazioni,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Home => "Home",
            Self::Album => "Album",
            Self::Scatto => "Scatto",
            Self::Modifica => "Modifica",
            Self::Impostazioni => "Impostazioni",
        }
    }

    pub fn parse(input: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|tab| tab.label().eq_ignore_ascii_case(input))
    }
}

pub type ScreenshotResult = Arc<Mutex<Option<Result<(), String>>>>;

pub struct Screenshot {
    path: Option<PathBuf>,
    frames: u32,
    result: ScreenshotResult,
}

impl Screenshot {
    pub fn new(path: PathBuf, result: ScreenshotResult) -> Self {
        Self {
            path: Some(path),
            frames: 0,
            result,
        }
    }

    fn update(&mut self, ctx: &egui::Context, ready: bool) {
        const WARMUP_FRAMES: u32 = 120;
        ctx.request_repaint();
        ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaintWhileHidden);
        self.frames = self.frames.saturating_add(1);
        if ready && self.frames >= WARMUP_FRAMES {
            if let Some(path) = self.path.take() {
                let result = Arc::clone(&self.result);
                ctx.request_screenshot(move |image: Arc<egui::ColorImage>| {
                    let written = save_screenshot(&path, &image);
                    let mut slot = result.lock().unwrap_or_else(|e| e.into_inner());
                    *slot = Some(written);
                });
            }
        }
        if self
            .result
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

fn save_screenshot(path: &std::path::Path, image: &egui::ColorImage) -> Result<(), String> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Impossibile creare {}: {e}", parent.display()))?;
    }
    let rgba: Vec<_> = image
        .pixels
        .iter()
        .flat_map(|pixel| pixel.to_array())
        .collect();
    image::save_buffer_with_format(
        path,
        &rgba,
        image.size[0] as u32,
        image.size[1] as u32,
        image::ColorType::Rgba8,
        image::ImageFormat::Png,
    )
    .map_err(|e| format!("Impossibile salvare {}: {e}", path.display()))
}

pub struct App {
    tab: Tab,
    album: Album,
    capture: Capture,
    editor: Editor,
    home: Home,
    settings: Settings,
    examples: Vec<PanoRef>,
    generate_examples: Option<Job<Vec<PanoRef>>>,
    load_image: Option<Job<Result<PanoRef, String>>>,
    next_id: u64,
    screenshot: Option<Screenshot>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, tab: Tab, screenshot: Option<Screenshot>) -> Self {
        theme::configure(&cc.egui_ctx);
        let width = if screenshot.is_some() { 1024 } else { 2048 };
        let generate_examples = Job::start(&cc.egui_ctx, move || {
            (0..panorama::sample_count())
                .map(|index| Arc::new(panorama::generate_sample(index, width)))
                .collect()
        });
        let settings = Settings::new(screenshot.is_none());
        Self {
            tab,
            album: Album::new(),
            capture: Capture::default(),
            home: Home::default(),
            editor: Editor::new(settings.preferences.watermark),
            settings,
            examples: Vec::new(),
            generate_examples: Some(generate_examples),
            load_image: None,
            next_id: 100,
            screenshot,
        }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        if let Some(examples) = Job::take(&mut self.generate_examples) {
            self.examples = examples;
            self.album.add_examples(ctx, &self.examples);
            if !self.editor.has_image() && self.load_image.is_none() {
                if let Some(first) = self.examples.first() {
                    self.editor.set_image(Arc::clone(first));
                }
            }
        }
        for (folder, source) in self.home.poll().into_iter().flatten() {
            self.album.open_folder(ctx, folder, source);
            self.tab = Tab::Album;
        }
        self.album.poll(ctx);
        if let Some(result) = Job::take(&mut self.load_image) {
            self.editor.loading = false;
            match result {
                Ok(pano) => self.editor.set_image(pano),
                Err(error) => {
                    if !self.editor.has_image() {
                        if let Some(first) = self.examples.first() {
                            self.editor.set_image(Arc::clone(first));
                        }
                    }
                    self.editor.load_message =
                        Some(format!("Impossibile aprire l’immagine: {error}"));
                }
            }
        }
        self.editor.export.poll();
    }

    fn open_image(&mut self, ctx: &egui::Context, asset: Asset) {
        self.tab = Tab::Modifica;
        self.editor.load_message = None;
        self.editor.loading = true;
        let examples = if matches!(asset, Asset::Example(_)) {
            self.examples.clone()
        } else {
            Vec::new()
        };
        let id = self.next_id;
        self.next_id += 1;
        self.load_image = Some(Job::start(ctx, move || asset.load(&examples, id)));
    }

    fn sidebar(&mut self, ui: &mut Ui) {
        egui::Panel::left("navigazione")
            .exact_size(220.0)
            .frame(theme::panel(theme::SIDE, 16))
            .show(ui, |ui| {
                ui.add_space(5.0);
                ui.horizontal(|ui| {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(39.0, 39.0), egui::Sense::hover());
                    ui.painter().rect_stroke(
                        rect,
                        8,
                        Stroke::new(1.4, theme::ACCENT),
                        StrokeKind::Inside,
                    );
                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "360",
                        egui::FontId::monospace(15.0),
                        theme::ACCENT,
                    );
                    ui.add_space(2.0);
                    ui.label(RichText::new("Studio 360").size(19.0).strong());
                });
                ui.add_space(20.0);
                ui.separator();
                ui.add_space(9.0);
                for (index, tab) in Tab::ALL.into_iter().enumerate() {
                    let selected = self.tab == tab;
                    let (rect, response) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 42.0),
                        egui::Sense::click(),
                    );
                    if response.clicked() {
                        self.tab = tab;
                    }
                    response.widget_info(|| {
                        egui::WidgetInfo::selected(egui::Role::Button, true, selected, tab.label())
                    });
                    if selected || response.hovered() || response.has_focus() {
                        ui.painter().rect_filled(rect, 8, theme::CARD);
                    }
                    if selected {
                        let mark = egui::Rect::from_min_size(
                            rect.left_top() + egui::vec2(0.0, 10.0),
                            egui::vec2(3.0, 22.0),
                        );
                        ui.painter().rect_filled(mark, 2, theme::ACCENT);
                    }
                    if response.has_focus() {
                        ui.painter().rect_stroke(
                            rect,
                            8,
                            Stroke::new(1.0, theme::ACCENT),
                            StrokeKind::Inside,
                        );
                    }
                    let icon = egui::Rect::from_min_size(
                        rect.left_center() + egui::vec2(12.0, -9.0),
                        egui::vec2(18.0, 18.0),
                    );
                    theme::nav_icon(
                        ui,
                        icon,
                        index,
                        if selected {
                            theme::ACCENT
                        } else {
                            theme::MUTED
                        },
                    );
                    ui.painter().text(
                        rect.left_center() + egui::vec2(42.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        tab.label(),
                        egui::FontId::proportional(15.0),
                        if selected { theme::TEXT } else { theme::MUTED },
                    );
                }
            });
    }

    fn workspace(&mut self, ui: &mut Ui) {
        let preferences = self.settings.preferences;
        if matches!(self.tab, Tab::Home | Tab::Album) {
            self.album.ensure_cache(ui.ctx(), &self.examples);
        }
        match self.tab {
            Tab::Home => match self.home.show(ui, &mut self.album) {
                Some(Action::Go(tab)) => self.tab = tab,
                Some(Action::Open(asset)) => self.open_image(ui.ctx(), asset),
                None => {}
            },
            Tab::Album => {
                if let Some(asset) = self.album.show(ui) {
                    self.open_image(ui.ctx(), asset);
                }
            }
            Tab::Scatto => self.capture.show(
                ui,
                self.examples.first(),
                preferences.performance || preferences.compatibility,
            ),
            Tab::Modifica => {
                self.editor
                    .show(ui, preferences.performance, preferences.compatibility)
            }
            Tab::Impostazioni => {
                let action = self.settings.show(ui);
                if action.clear_cache {
                    self.album.clear_cache();
                }
                if action.open_export {
                    self.editor.show_export_controls();
                    self.tab = Tab::Modifica;
                }
                if action.changed {
                    if preferences.watermark != self.settings.preferences.watermark {
                        self.editor
                            .export
                            .set_watermark(self.settings.preferences.watermark);
                    }
                    if self.screenshot.is_none() {
                        self.settings.save();
                    }
                }
            }
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.poll(ui.ctx());
        self.sidebar(ui);
        egui::CentralPanel::no_frame()
            .frame(theme::panel(theme::BG, 24))
            .show(ui, |ui| self.workspace(ui));
        if let Some(screenshot) = &mut self.screenshot {
            let ready = match self.tab {
                Tab::Home | Tab::Album => !self.examples.is_empty(),
                Tab::Scatto => self.capture.ready(),
                Tab::Modifica => self.editor.ready(),
                Tab::Impostazioni => true,
            };
            screenshot.update(ui.ctx(), ready);
        }
    }
}
