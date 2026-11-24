use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        Arc,
    },
    time::Duration,
};

use eframe::egui::{self, Color32, Rect, RichText, Stroke, StrokeKind, Ui};

use crate::{
    ai_provider::{self, AiAnalysis, AnalysisSource, Thumbnail},
    app::Job,
    autoedit::{
        self, Highlight, ANALYSIS_PROGRESS_END, COLOR_SLIDERS, COMPLETE_PROGRESS, MAX_SCORE,
        THUMBNAIL_SIZE,
    },
    export::Export,
    movie::Filter,
    movie_editor::MovieEditor,
    online::Online,
    overlay_editor::OverlayEditor,
    panorama::{
        self, preset, Adjust, PanoRef, Projection, Region, View, ADJUST_NAMES, FILTERS,
        FRAME_ASPECT,
    },
    settings::Settings,
    theme,
};

const TABS: &[&str] = &[
    "Visuale", "Colore", "Filtri", "Ritaglio", "Sticker", "Testo", "Esporta",
];
const STICKER_PANEL: usize = 4;
const TEXT_PANEL: usize = 5;
const EXPORT_PANEL: usize = TABS.len() - 1;
const AUTO_EDIT_PANEL: usize = TABS.len();
const MUSIC_PANEL: usize = TABS.len() + 1;
const AUTO_EDIT_PROGRESS_REFRESH: Duration = Duration::from_millis(32);
const MOVIE_TIMELINE_HEIGHT: f32 = 169.0;
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
#[derive(Clone, Copy, PartialEq)]
struct PreviewKey {
    id: u64,
    revision: u64,
    view: View,
    adjust: Adjust,
    size: (usize, usize),
    time: f64,
}

#[derive(Clone, Copy)]
pub enum InitialPanel {
    Music,
    Sticker,
    Text,
    Templates,
}

impl InitialPanel {
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "musica" => Some(Self::Music),
            "sticker" => Some(Self::Sticker),
            "testo" => Some(Self::Text),
            "modelli" => Some(Self::Templates),
            _ => None,
        }
    }
}

#[derive(Default)]
pub struct Preview {
    texture: Option<egui::TextureHandle>,
    rendered: Option<PreviewKey>,
    displayed: Option<PanoRef>,
    pending: Option<Job<(PreviewKey, PanoRef, Vec<u8>)>>,
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
        self.update_frame(ctx, pano, (0, 0.0), view, adjust, size);
    }

    fn update_frame(
        &mut self,
        ctx: &egui::Context,
        pano: &PanoRef,
        (revision, time): (u64, f64),
        view: View,
        adjust: Adjust,
        size: (usize, usize),
    ) {
        let key = PreviewKey {
            id: pano.id,
            revision,
            view,
            adjust,
            size,
            time,
        };
        if let Some((done, source, rgb)) = Job::take(&mut self.pending) {
            if done.id == key.id {
                let image = egui::ColorImage::from_rgb([done.size.0, done.size.1], &rgb);
                let options = egui::TextureOptions::LINEAR;
                if let Some(texture) = &mut self.texture {
                    texture.set(image, options);
                } else {
                    self.texture = Some(ctx.load_texture("anteprima", image, options));
                }
                self.rendered = Some(done);
                self.displayed = Some(source);
            }
        }
        if self.pending.is_none() && self.rendered != Some(key) {
            let pano = Arc::clone(pano);
            self.pending = Some(Job::start(ctx, move || {
                let rgb =
                    panorama::render(&pano, &key.view, &key.adjust, key.size, Region::FULL, None);
                (key, pano, rgb)
            }));
        }
    }

    pub fn ready(&self) -> bool {
        self.texture.is_some()
    }

    fn time(&self) -> f64 {
        self.rendered.map_or(0.0, |key| key.time)
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
    video: Option<crate::playback::Playback>,
    movie: Option<MovieEditor>,
    revision: u64,
    preview: Preview,
    view: View,
    gear: usize,
    adjust: Adjust,
    filter: usize,
    crop: usize,
    panel: usize,
    scroll_pixels: f32,
    auto_edit: Option<AutoEdit>,
    overlays: OverlayEditor,
}

struct EditorState {
    view: View,
    gear: usize,
    adjust: Adjust,
    filter: usize,
    crop: usize,
    panel: usize,
    scroll_pixels: f32,
}

struct AutoEdit {
    previous: EditorState,
    source: PanoRef,
    state: AutoEditState,
    cancel: Arc<AtomicBool>,
}

impl Drop for AutoEdit {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

enum AutoEditState {
    Analyzing {
        job: Option<Job<Option<RenderedHighlights>>>,
        progress: Arc<AtomicU8>,
    },
    Finished {
        clips: Vec<Clip>,
        color: Adjust,
        before_color: Option<Adjust>,
        source: AnalysisSource,
        suggested_filter: Option<Filter>,
    },
    Failed,
}

struct RenderedHighlights {
    color: Adjust,
    clips: Vec<(Highlight, Vec<u8>)>,
    ai: AiAnalysis,
}

struct Clip {
    highlight: Highlight,
    texture: egui::TextureHandle,
    selected: bool,
}

impl Editor {
    pub fn new(watermark: bool) -> Self {
        Self {
            export: Export::new(watermark),
            load_message: None,
            loading: false,
            pano: None,
            video: None,
            movie: None,
            revision: 0,
            preview: Preview::default(),
            view: View::default(),
            gear: WIDE,
            adjust: Adjust::default(),
            filter: 0,
            crop: 0,
            panel: 0,
            scroll_pixels: 0.0,
            auto_edit: None,
            overlays: OverlayEditor::default(),
        }
    }

    pub fn has_image(&self) -> bool {
        self.pano.is_some()
    }

    pub fn start_video(&mut self, video: crate::playback::Playback, path: PathBuf) {
        self.video = Some(video);
        self.movie = Some(MovieEditor::new(path));
        self.pano = None;
        self.preview = Preview::default();
    }

    pub fn stop_video(&mut self) {
        self.movie = None;
        if self.video.take().is_some() {
            self.pano = None;
            self.preview = Preview::default();
        }
    }

    pub fn poll_video(&mut self, ctx: &egui::Context, visible: bool) {
        self.overlays.poll(ctx);
        if let Some(movie) = &mut self.movie {
            movie.set_overlays(self.overlays.revision(), self.overlays.snapshot());
            movie.poll(ctx, visible, self.adjust.combined(&preset(self.filter)));
        }
        let Some(video) = &mut self.video else {
            return;
        };
        if !visible
            || self
                .movie
                .as_ref()
                .is_some_and(MovieEditor::showing_preview)
        {
            video.pause();
        }
        match video.poll(ctx) {
            Ok(Some(pano)) if self.loading => {
                self.loading = false;
                self.set_image(pano);
            }
            Ok(Some(pano)) => {
                self.pano = Some(pano);
                self.revision = self.revision.wrapping_add(1);
            }
            Ok(None) => (),
            Err(error) => {
                self.loading = false;
                self.video = None;
                self.movie = None;
                self.load_message = Some(format!("Impossibile leggere il video: {error}"));
            }
        }
    }

    pub fn show_export_controls(&mut self) {
        self.panel = EXPORT_PANEL;
    }

    pub fn open_initial_panel(
        &mut self,
        ctx: &egui::Context,
        panel: InitialPanel,
        demo: bool,
        auto_create: bool,
        settings: &Settings,
        online: &Online,
    ) {
        self.panel = match panel {
            InitialPanel::Music => {
                if let Some(movie) = &mut self.movie {
                    movie.open_music_library();
                }
                MUSIC_PANEL
            }
            InitialPanel::Sticker => STICKER_PANEL,
            InitialPanel::Text => TEXT_PANEL,
            InitialPanel::Templates => {
                if let Some(movie) = &mut self.movie {
                    movie.open_template_hub(demo);
                    self.open_auto_edit(ctx, demo || auto_create, settings, online);
                }
                return;
            }
        };
        if matches!(panel, InitialPanel::Sticker | InitialPanel::Text) {
            if let Some(movie) = &mut self.movie {
                movie.stop_preview();
            }
        }
        if demo && matches!(panel, InitialPanel::Sticker) {
            self.overlays.start_demo(ctx, online);
        }
    }

    pub fn set_image(&mut self, pano: PanoRef) {
        self.pano = Some(pano);
        self.revision = 0;
        self.preview = Preview::default();
        self.view = View::default();
        self.gear = WIDE;
        self.adjust = Adjust::default();
        self.filter = 0;
        self.crop = 0;
        self.scroll_pixels = 0.0;
        self.auto_edit = None;
        self.overlays = OverlayEditor::default();
        if self.panel == AUTO_EDIT_PANEL {
            self.panel = 0;
        }
    }

    pub fn ready(&self) -> bool {
        !self.loading
            && self.overlays.demo_ready()
            && self.movie.as_ref().is_none_or(MovieEditor::ready)
            && (self.preview.ready()
                || self
                    .movie
                    .as_ref()
                    .is_some_and(MovieEditor::showing_preview))
            && !matches!(
                self.auto_edit.as_ref().map(|session| &session.state),
                Some(AutoEditState::Analyzing { .. })
            )
    }

    pub fn open_auto_edit(
        &mut self,
        ctx: &egui::Context,
        auto_create: bool,
        settings: &Settings,
        online: &Online,
    ) {
        if let Some(movie) = &mut self.movie {
            if let Some(video) = &mut self.video {
                video.pause();
            }
            movie.analyze(ctx, auto_create, settings, online);
            self.panel = AUTO_EDIT_PANEL;
            return;
        }
        let Some(source) = self
            .current_frame()
            .filter(|pano| pano.is_panorama && !self.loading)
            .cloned()
        else {
            return;
        };
        if self.auto_edit.is_none() {
            let previous = EditorState {
                view: self.view,
                gear: self.gear,
                adjust: self.adjust,
                filter: self.filter,
                crop: self.crop,
                panel: self.panel,
                scroll_pixels: self.scroll_pixels,
            };
            let analysis_source = Arc::clone(&source);
            let progress = Arc::new(AtomicU8::new(0));
            let meter = Arc::clone(&progress);
            let cancel = Arc::new(AtomicBool::new(false));
            let cancelled = Arc::clone(&cancel);
            let client = settings.ai_for_auto_edit();
            let has_ai = client.is_ok();
            let online = online.clone();
            let job = Job::start(ctx, move || {
                let update = |percent| {
                    meter.store(
                        ai_provider::local_progress(percent, has_ai),
                        Ordering::Relaxed,
                    );
                };
                let analysis = autoedit::analyze(&analysis_source, update)?;
                let count = analysis.highlights.len();
                let mut clips = Vec::with_capacity(count);
                for (index, highlight) in analysis.highlights.into_iter().enumerate() {
                    if cancelled.load(Ordering::Relaxed) {
                        return None;
                    }
                    let rgb = panorama::render(
                        &analysis_source,
                        &highlight.view,
                        &Adjust::default(),
                        THUMBNAIL_SIZE,
                        Region::FULL,
                        None,
                    );
                    let done = usize::from(COMPLETE_PROGRESS - ANALYSIS_PROGRESS_END) * (index + 1)
                        / count;
                    update(ANALYSIS_PROGRESS_END + done as u8);
                    clips.push((highlight, rgb));
                }
                if cancelled.load(Ordering::Relaxed) {
                    return None;
                }
                let candidates: Vec<_> = clips
                    .iter()
                    .map(|(highlight, rgb)| Thumbnail {
                        score: highlight.score,
                        rgb,
                        size: THUMBNAIL_SIZE,
                    })
                    .collect();
                let ai = ai_provider::analyze(client, &candidates, online.enabled(), |phase| {
                    meter.store(phase, Ordering::Relaxed);
                });
                Some(RenderedHighlights {
                    color: analysis.color,
                    clips,
                    ai,
                })
            });
            self.auto_edit = Some(AutoEdit {
                previous,
                source,
                state: AutoEditState::Analyzing {
                    job: Some(job),
                    progress,
                },
                cancel,
            });
        }
        self.panel = AUTO_EDIT_PANEL;
    }

    fn current_frame(&self) -> Option<&PanoRef> {
        if self.video.is_some() {
            self.preview.displayed.as_ref().or(self.pano.as_ref())
        } else {
            self.pano.as_ref()
        }
    }

    fn poll_auto_edit(&mut self, ctx: &egui::Context) {
        let Some(session) = &mut self.auto_edit else {
            return;
        };
        let AutoEditState::Analyzing { job, .. } = &mut session.state else {
            return;
        };
        if let Some(Some(rendered)) = Job::take(job) {
            let mut clips: Vec<_> = rendered
                .clips
                .into_iter()
                .enumerate()
                .map(|(index, (highlight, rgb))| {
                    let image =
                        egui::ColorImage::from_rgb([THUMBNAIL_SIZE.0, THUMBNAIL_SIZE.1], &rgb);
                    Clip {
                        texture: ctx.load_texture(
                            format!("Auto Edit {index}"),
                            image,
                            egui::TextureOptions::LINEAR,
                        ),
                        highlight,
                        selected: true,
                    }
                })
                .collect();
            let mut suggested_filter = None;
            if let Some(verdict) = rendered.ai.verdict {
                for change in verdict.clips {
                    let clip = &mut clips[change.index];
                    change.apply(
                        &mut clip.highlight.score,
                        &mut clip.highlight.scene,
                        &mut clip.selected,
                    );
                }
                clips.sort_by_key(|clip| std::cmp::Reverse(clip.highlight.score));
                suggested_filter = verdict.filter;
                if let Some(filter) = suggested_filter {
                    self.filter = filter.0;
                }
            }
            session.state = AutoEditState::Finished {
                clips,
                color: rendered.color,
                before_color: None,
                source: rendered.ai.source,
                suggested_filter,
            };
        } else if job.is_none() {
            session.state = AutoEditState::Failed;
        }
    }

    fn undo_auto_edit(&mut self) {
        if let Some(session) = self.auto_edit.take() {
            let previous = &session.previous;
            self.view = previous.view;
            self.gear = previous.gear;
            self.adjust = previous.adjust;
            self.filter = previous.filter;
            self.crop = previous.crop;
            self.panel = previous.panel;
            self.scroll_pixels = previous.scroll_pixels;
        }
    }

    pub fn show(&mut self, ui: &mut Ui, settings: &Settings, online: &Online) {
        let performance = settings.preferences.performance;
        let compatibility = settings.preferences.compatibility;
        self.poll_auto_edit(ui.ctx());
        theme::title(
            ui,
            "Modifica",
            if self.video.is_some() {
                "Inquadra il video o assembla i momenti migliori in un filmato."
            } else {
                "Inquadra il panorama e prepara l’immagine da esportare."
            },
        );
        if self.loading {
            theme::muted(
                ui,
                if self.video.is_some() {
                    "Apertura video…"
                } else {
                    "Apertura immagine…"
                },
            );
        }
        if let Some(error) = &self.load_message {
            ui.colored_label(theme::ERROR, error);
        }
        let Some(pano) = self.pano.as_ref().map(Arc::clone) else {
            if !self.loading && self.load_message.is_none() {
                theme::muted(ui, "Preparazione dei panorami di esempio…");
            }
            return;
        };
        let visible = self
            .current_frame()
            .cloned()
            .unwrap_or_else(|| Arc::clone(&pano));
        let width = if ui.available_width() < 800.0 {
            232.0
        } else {
            268.0
        };
        egui::Panel::right("strumenti_editor")
            .exact_size(width)
            .frame(theme::card(12))
            .show(ui, |ui| {
                self.tools(ui, &visible, settings, online);
            });
        egui::CentralPanel::no_frame().show(ui, |ui| {
            let width = (ui.available_width() - 12.0).max(1.0);
            let montage = self.movie.as_ref().filter(|movie| movie.showing_preview());
            let has_timeline = self.movie.as_ref().is_some_and(MovieEditor::has_clips);
            let controls_height = if montage.is_some() {
                MOVIE_TIMELINE_HEIGHT - 70.0
            } else if self.video.is_some() {
                40.0 + if has_timeline {
                    MOVIE_TIMELINE_HEIGHT
                } else {
                    0.0
                }
            } else {
                0.0
            };
            let height = (ui.available_height() - 86.0 - controls_height).max(1.0);
            let size = theme::fit(
                Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, height)),
                montage.map_or(FRAME_ASPECT, MovieEditor::aspect),
            )
            .size();
            ui.vertical_centered(|ui| {
                if let Some(movie) = self.movie.as_mut().filter(|movie| movie.showing_preview()) {
                    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                    movie.paint_preview(ui, rect);
                    ui.add_space(4.0);
                    if movie.timeline(ui, width) {
                        self.show_export_controls();
                    }
                    return;
                }
                let editing = match self.panel {
                    STICKER_PANEL => Some(true),
                    TEXT_PANEL => Some(false),
                    _ => None,
                };
                let sense = if editing.is_some() {
                    egui::Sense::click_and_drag()
                } else if pano.is_panorama {
                    egui::Sense::drag()
                } else {
                    egui::Sense::hover()
                };
                let (rect, response) = ui.allocate_exact_size(size, sense);
                let crop = theme::fit(rect, RATIOS[self.crop].1);
                let moving = self.preview.ready()
                    && editing.is_some_and(|sticker| {
                        self.overlays.handle_input(
                            ui,
                            &response,
                            crop,
                            self.preview.time(),
                            sticker,
                        )
                    });
                if pano.is_panorama && !moving {
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
                let time = self
                    .video
                    .as_ref()
                    .map_or(0.0, crate::playback::Playback::position);
                self.preview.update_frame(
                    ui.ctx(),
                    &pano,
                    (self.revision, time),
                    self.view,
                    adjust,
                    size,
                );
                self.preview.paint(ui, rect);
                if self.preview.ready() {
                    paint_crop(ui, rect, crop);
                    self.overlays.paint(ui, crop, self.preview.time(), editing);
                }
                ui.add_space(4.0);
                if let Some(video) = &mut self.video {
                    ui.allocate_ui_with_layout(
                        egui::vec2(rect.width(), 32.0),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| video.controls(ui, rect.width()),
                    );
                }
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
                            let kind = if self.video.is_some() {
                                "Video piatto"
                            } else {
                                "Foto piatta"
                            };
                            theme::muted(ui, format!("{kind} · {} × {} px", pano.w, pano.h));
                        }
                    });
                });
                if let Some(movie) = &mut self.movie {
                    if movie.timeline(ui, width) {
                        self.show_export_controls();
                    }
                }
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

    fn tools(&mut self, ui: &mut Ui, pano: &PanoRef, settings: &Settings, online: &Online) {
        ui.set_min_width(ui.available_width());
        ui.horizontal_wrapped(|ui| {
            for (index, tab) in TABS.iter().enumerate() {
                if theme::chip(ui, tab, self.panel == index).clicked() {
                    self.panel = index;
                    if let Some(movie) = self.movie.as_mut().filter(|_| index != EXPORT_PANEL) {
                        movie.stop_preview();
                    }
                }
            }
            let auto_edit = ui.add_enabled(
                (pano.is_panorama || self.movie.is_some()) && !self.loading,
                theme::primary("Auto Edit").selected(self.panel == AUTO_EDIT_PANEL),
            );
            if auto_edit.clicked() {
                self.open_auto_edit(ui.ctx(), false, settings, online);
            }
            if !pano.is_panorama && self.movie.is_none() {
                auto_edit.on_disabled_hover_text("Disponibile per i panorami 360°.");
            }
            if self.movie.is_some()
                && theme::chip(ui, "Musica", self.panel == MUSIC_PANEL).clicked()
            {
                self.panel = MUSIC_PANEL;
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
                STICKER_PANEL => self.overlays.stickers(
                    ui,
                    online,
                    self.video.as_ref().map(crate::playback::Playback::duration),
                ),
                TEXT_PANEL => self.overlays.text(
                    ui,
                    self.video.as_ref().map(crate::playback::Playback::duration),
                ),
                MUSIC_PANEL => {
                    if let Some(movie) = &mut self.movie {
                        movie.show_music(ui, online);
                    } else {
                        theme::muted(ui, "Apri un video per aggiungere musica al filmato.");
                    }
                }
                AUTO_EDIT_PANEL => {
                    if let Some(movie) = &mut self.movie {
                        let color = self.adjust.combined(&preset(self.filter));
                        movie.show_auto_edit(
                            ui,
                            color,
                            self.export.watermark(),
                            pano.is_panorama,
                            settings,
                            online,
                        );
                    } else {
                        self.show_auto_edit(ui);
                    }
                }
                _ => {
                    let adjust = self.adjust.combined(&preset(self.filter));
                    let overlays = (self.overlays.snapshot(), self.preview.time());
                    if let Some(movie) = &mut self.movie {
                        movie.show_export(ui, adjust, self.export.watermark());
                        ui.collapsing("Esporta fotogramma", |ui| {
                            self.export.show(
                                ui,
                                pano,
                                self.view,
                                adjust,
                                RATIOS[self.crop].1,
                                overlays,
                            );
                        });
                    } else {
                        self.export.show(
                            ui,
                            pano,
                            self.view,
                            adjust,
                            RATIOS[self.crop].1,
                            overlays,
                        );
                    }
                }
            });
    }

    fn show_auto_edit(&mut self, ui: &mut Ui) {
        let Some(session) = &mut self.auto_edit else {
            return;
        };
        match &mut session.state {
            AutoEditState::Analyzing { progress, .. } => {
                let (fraction, label) = ai_provider::progress_display(
                    progress.load(Ordering::Relaxed),
                    "Analisi di soggetti, scene e momenti salienti…",
                );
                theme::muted(ui, label);
                ui.add(egui::ProgressBar::new(fraction).fill(theme::ACCENT));
                ui.ctx().request_repaint_after(AUTO_EDIT_PROGRESS_REFRESH);
            }
            AutoEditState::Finished {
                clips,
                color,
                before_color,
                source,
                suggested_filter,
            } => {
                autoedit::show_analysis_source(ui, source);
                theme::heading(ui, "Momenti salienti");
                if let Some(filter) = suggested_filter {
                    theme::muted(ui, format!("Filtro suggerito · {}", FILTERS[filter.0].0));
                }
                if clips.is_empty() {
                    theme::muted(ui, "Nessun altro soggetto in questa immagine");
                }
                for (index, clip) in clips.iter_mut().enumerate() {
                    ui.push_id(index, |ui| {
                        ui.horizontal(|ui| {
                            let view = clip.highlight.view;
                            let active = self.view == view;
                            if clip.thumbnail(ui, active).clicked() {
                                self.view = view;
                                self.gear = FOVS
                                    .iter()
                                    .position(|&(_, fov)| fov == view.fov)
                                    .unwrap_or(CUSTOM);
                                self.scroll_pixels = 0.0;
                            }
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 1.0;
                                ui.checkbox(&mut clip.selected, clip.highlight.scene.label());
                                ui.label(
                                    RichText::new(format!(
                                        "{} / {MAX_SCORE}",
                                        clip.highlight.score
                                    ))
                                    .strong(),
                                );
                                theme::muted(
                                    ui,
                                    format!("Filtro · {}", clip.highlight.scene.suggested_filter()),
                                );
                            });
                        });
                    });
                }
                ui.separator();
                let mut automatic = before_color.is_some();
                if ui.checkbox(&mut automatic, "Colore automatico").changed() {
                    let correction = if automatic {
                        *before_color = Some(self.adjust);
                        *color
                    } else {
                        before_color.take().unwrap_or(self.adjust)
                    };
                    for index in COLOR_SLIDERS {
                        self.adjust.v[index] = correction.v[index];
                    }
                }
                let selected: Vec<_> = clips
                    .iter()
                    .filter(|clip| clip.selected)
                    .map(|clip| clip.highlight.view)
                    .collect();
                ui.label(RichText::new(format!("Clip selezionate ({})", selected.len())).strong());
                if selected.is_empty() {
                    theme::muted(ui, "Seleziona almeno una clip");
                }
                let export = ui.add_enabled(
                    !self.loading && !selected.is_empty() && self.export.can_start(),
                    theme::primary("Esporta selezionate"),
                );
                if export.clicked() {
                    let adjust = self.adjust.combined(&preset(self.filter));
                    self.export.start_selected(
                        ui.ctx(),
                        &session.source,
                        selected,
                        adjust,
                        RATIOS[self.crop].1,
                        (self.overlays.snapshot(), 0.0),
                    );
                }
                self.export.show_batch_status(ui);
            }
            AutoEditState::Failed => {
                ui.colored_label(theme::ERROR, "Impossibile analizzare il panorama.");
            }
        }
        if ui.button("Annulla Auto Edit").clicked() {
            self.undo_auto_edit();
        }
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

impl Clip {
    fn thumbnail(&self, ui: &mut Ui, active: bool) -> egui::Response {
        ui.scope(|ui| {
            ui.spacing_mut().button_padding = egui::vec2(3.0, 3.0);
            let width = (ui.available_width() * 0.43).min(104.0);
            let size = egui::vec2(width, width / FRAME_ASPECT);
            let image = egui::Image::new((self.texture.id(), size)).corner_radius(4);
            let response = ui.add(egui::Button::image(image).selected(active));
            response.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::Role::Button,
                    true,
                    active,
                    format!(
                        "Modifica vista · {} · {:.0}°",
                        self.highlight.scene.label(),
                        self.highlight.view.yaw
                    ),
                )
            });
            response
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text("Modifica vista")
        })
        .inner
    }
}

fn paint_crop(ui: &Ui, rect: Rect, inner: Rect) {
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
