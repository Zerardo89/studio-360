use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use eframe::egui::{self, Rect, RichText, Stroke, StrokeKind, Ui};
use image::RgbaImage;

use crate::{
    ai_provider::{self, AiAnalysis, AnalysisSource, Thumbnail},
    app::Job,
    autoedit::{self, Segment, NO_VIDEO_SUBJECT, THUMBNAIL_SIZE},
    movie::{
        self, Curve, Filter, Movie, MovieRenderer, Music, OutputProfile, Speed, TargetDuration,
        Template, Transition, ASPECTS, MAX_PLAYBACK_SPEED, MIN_PLAYBACK_SPEED, RESOLUTIONS,
    },
    music_library::MusicLibrary,
    online::Online,
    overlay::Overlays,
    panorama::{Adjust, View, ADJUST_NAMES},
    playback::{self, format_time},
    settings::Settings,
    template_hub::Hub,
    templates::TemplateKind,
    theme, video,
};

const PREVIEW_SIZES: [(u32, u32); 2] = [(640, 360), (360, 640)];
const PREVIEW_FPS: f64 = 24.0;
const POLL_INTERVAL: Duration = Duration::from_millis(32);
const FIXED_DURATION: &str = "Il modello non supporta la regolazione della durata";
const FIXED_ASPECT: &str = "Il modello non supporta la regolazione del rapporto.";
const REQUIRED_SLOT: &str = "Questo spazio è necessario in questo modello";
const FILTER_LABELS: &[&str] = &[
    "Nessuno",
    "Cinema",
    "Vintage",
    "Città",
    "Paesaggio",
    "Notte",
    "Ritratto",
    "Sport",
    "Cibo",
    "Caldo",
    "Freddo",
    "Bianco e nero",
    "Creativo",
    "Interni",
];

struct Found {
    segment: Segment,
    selected: bool,
    texture: Option<egui::TextureHandle>,
}

struct AnalyzedSegments {
    segments: Vec<Segment>,
    ai: AiAnalysis,
}

enum TaskResult {
    Music(Music, Option<String>),
    Beat(Vec<f64>),
    Export(String, Vec<String>),
    Dismissed,
}

enum TaskKind {
    ChooseMusic,
    ExtractMusic,
    Beat,
    Export,
}

struct Task {
    kind: TaskKind,
    reply: Option<Job<Result<TaskResult, String>>>,
    progress: Arc<AtomicU8>,
    cancel: Arc<AtomicBool>,
}

impl Drop for Task {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

struct PreviewRequest {
    revision: u64,
    time: f64,
    aspect: usize,
    replacement: Option<Movie>,
}

struct MoviePreview {
    commands: mpsc::SyncSender<PreviewRequest>,
    replies: mpsc::Receiver<(u64, Result<RgbaImage, String>)>,
    pending: bool,
    last: Option<(u64, f64)>,
    texture: Option<egui::TextureHandle>,
}

impl MoviePreview {
    fn new(ctx: &egui::Context) -> Self {
        let ctx = ctx.clone();
        let (commands, requests) = mpsc::sync_channel::<PreviewRequest>(1);
        let (sender, replies) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut state = None;
            while let Ok(request) = requests.recv() {
                if let Some(movie) = request.replacement {
                    let timeline = movie.timeline();
                    state = Some((movie, timeline, MovieRenderer::default()));
                }
                let frame = state
                    .as_mut()
                    .ok_or_else(|| "Anteprima del filmato non disponibile.".to_owned())
                    .and_then(|(movie, timeline, renderer)| {
                        renderer.frame(
                            movie,
                            timeline,
                            request.time,
                            PREVIEW_SIZES[request.aspect],
                            false,
                        )
                    });
                if sender.send((request.revision, frame)).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        });
        Self {
            commands,
            replies,
            pending: false,
            last: None,
            texture: None,
        }
    }

    fn update(
        &mut self,
        ctx: &egui::Context,
        movie: &Movie,
        revision: u64,
        time: f64,
        aspect: usize,
    ) -> Result<(), String> {
        if self.pending {
            match self.replies.try_recv() {
                Ok((done, frame)) => {
                    self.pending = false;
                    if done == revision {
                        let frame = frame?;
                        let image = egui::ColorImage::from_rgba_unmultiplied(
                            [frame.width() as usize, frame.height() as usize],
                            &frame,
                        );
                        if let Some(texture) = &mut self.texture {
                            texture.set(image, egui::TextureOptions::LINEAR);
                        } else {
                            self.texture = Some(ctx.load_texture(
                                "Anteprima filmato",
                                image,
                                egui::TextureOptions::LINEAR,
                            ));
                        }
                    }
                }
                Err(mpsc::TryRecvError::Empty) => return Ok(()),
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err("Anteprima del filmato interrotta.".into())
                }
            }
        }
        if self.last != Some((revision, time)) {
            self.commands
                .try_send(PreviewRequest {
                    revision,
                    time,
                    aspect,
                    replacement: self
                        .last
                        .is_none_or(|(last, _)| last != revision)
                        .then(|| movie.clone()),
                })
                .map_err(|_| "Anteprima del filmato interrotta.")?;
            self.pending = true;
            self.last = Some((revision, time));
        }
        Ok(())
    }
}

pub struct MovieEditor {
    source: PathBuf,
    analysis: Option<Job<Result<AnalyzedSegments, String>>>,
    analysis_cancel: Arc<AtomicBool>,
    progress: Arc<AtomicU8>,
    analyzed: bool,
    analysis_source: Option<AnalysisSource>,
    auto_create: bool,
    found: Vec<Found>,
    movie: Option<Movie>,
    selected: usize,
    hub: Hub,
    suggested_filter: Option<Filter>,
    target: usize,
    output: OutputProfile,
    music: Option<Music>,
    music_library: MusicLibrary,
    overlays: Overlays,
    overlay_revision: u64,
    sync_done: bool,
    task: Option<Task>,
    notice: Option<(String, bool)>,
    warnings: Vec<String>,
    preview: Option<MoviePreview>,
    preview_error: Option<String>,
    revision: u64,
    showing_preview: bool,
    position: f64,
    clock: Option<(Instant, f64)>,
}

impl Drop for MovieEditor {
    fn drop(&mut self) {
        self.analysis_cancel.store(true, Ordering::Relaxed);
    }
}

impl MovieEditor {
    pub fn new(source: PathBuf) -> Self {
        Self {
            source,
            analysis: None,
            progress: Arc::new(AtomicU8::new(0)),
            analyzed: false,
            analysis_source: None,
            analysis_cancel: Arc::new(AtomicBool::new(false)),
            auto_create: false,
            found: Vec::new(),
            movie: None,
            selected: 0,
            hub: Hub::default(),
            suggested_filter: None,
            target: 0,
            output: OutputProfile {
                resolution: 1,
                ..OutputProfile::default()
            },
            music: None,
            music_library: MusicLibrary::default(),
            overlays: Overlays::default(),
            overlay_revision: 0,
            sync_done: false,
            task: None,
            notice: None,
            warnings: Vec::new(),
            preview: None,
            preview_error: None,
            revision: 0,
            showing_preview: false,
            position: 0.0,
            clock: None,
        }
    }

    pub fn analyze(
        &mut self,
        ctx: &egui::Context,
        auto_create: bool,
        settings: &Settings,
        online: &Online,
    ) {
        self.auto_create |= auto_create;
        if self.analysis.is_some() || self.analyzed {
            return;
        }
        self.progress = Arc::new(AtomicU8::new(0));
        self.notice = None;
        let source = self.source.clone();
        let progress = Arc::clone(&self.progress);
        self.analysis_cancel = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::clone(&self.analysis_cancel);
        let client = settings.ai_for_auto_edit();
        let has_ai = client.is_ok();
        let online = online.clone();
        self.analysis = Some(Job::start(ctx, move || {
            let cancelled = || cancelled.load(Ordering::Relaxed);
            let segments = movie::analyze_source(
                &source,
                |percent| {
                    progress.store(
                        ai_provider::local_progress(percent, has_ai),
                        Ordering::Relaxed,
                    );
                },
                cancelled,
            )?;
            if cancelled() {
                return Err("Analisi video annullata.".into());
            }
            let candidates: Vec<_> = segments
                .iter()
                .map(|segment| Thumbnail {
                    score: segment.score,
                    rgb: &segment.thumbnail,
                    size: THUMBNAIL_SIZE,
                })
                .collect();
            let ai = ai_provider::analyze(client, &candidates, online.enabled(), |phase| {
                progress.store(phase, Ordering::Relaxed);
            });
            Ok(AnalyzedSegments { segments, ai })
        }));
    }

    pub fn ready(&self) -> bool {
        self.analysis.is_none()
            && (!self.showing_preview()
                || self.preview_error.is_some()
                || self
                    .preview
                    .as_ref()
                    .is_some_and(|preview| preview.texture.is_some()))
    }

    pub fn pause(&mut self) {
        self.clock = None;
    }

    pub fn stop_preview(&mut self) {
        self.pause();
        self.showing_preview = false;
        self.music_library.stop_preview();
    }

    pub fn set_overlays(&mut self, revision: u64, overlays: &Overlays) {
        if self.overlay_revision == revision {
            return;
        }
        self.overlay_revision = revision;
        self.overlays = overlays.clone();
        if let Some(movie) = &mut self.movie {
            movie.overlays = self.overlays.clone();
        }
        self.invalidate();
    }

    pub fn open_music_library(&mut self) {
        self.music_library.open = true;
    }

    pub fn open_template_hub(&mut self, demo: bool) {
        self.hub.scroll_to_heading = true;
        if demo {
            self.hub.select(Template::UrbanPulse);
        }
    }

    fn invalidate(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        self.preview_error = None;
    }

    fn edited(&mut self) {
        self.sync_done = false;
        self.pause();
        self.invalidate();
    }

    fn selected_segments(&self) -> (Vec<usize>, Vec<Segment>) {
        self.found
            .iter()
            .enumerate()
            .filter(|(_, found)| found.selected)
            .map(|(i, found)| (i, found.segment.clone()))
            .unzip()
    }

    fn create(&mut self, ctx: &egui::Context, color: Adjust) {
        let (indices, segments) = self.selected_segments();
        if segments.is_empty() {
            return;
        }
        let mut movie = movie::build_from_template(
            &segments,
            self.hub.selected,
            TargetDuration::ALL[self.target],
        );
        for clip in &mut movie.clips {
            clip.segment_index = indices[clip.segment_index];
        }
        movie.colore = color;
        if let Some(previous) = &self.movie {
            movie.colore = previous.colore;
            movie.original_audio_volume = previous.original_audio_volume;
            movie.music_volume = previous.music_volume;
            movie.filter = previous.filter;
        } else if let Some(filter) = self.suggested_filter {
            movie.filter = filter;
        }
        movie.music = self.music.clone();
        movie.overlays = self.overlays.clone();
        self.movie = Some(movie);
        self.selected = 0;
        self.position = 0.0;
        self.showing_preview = true;
        self.notice = None;
        self.edited();
        self.start_sync(ctx);
    }

    pub fn poll(&mut self, ctx: &egui::Context, visible: bool, color: Adjust) {
        if !visible {
            self.pause();
            self.music_library.stop_preview();
        }
        if let Some(music) = self.music_library.poll() {
            self.music = Some(music);
            self.update_music();
        }
        let analyzing = self.analysis.is_some();
        if let Some(result) = Job::take(&mut self.analysis) {
            match result {
                Ok(AnalyzedSegments { segments, ai }) => {
                    self.analyzed = true;
                    self.analysis_source = Some(ai.source);
                    self.found = segments
                        .into_iter()
                        .enumerate()
                        .map(|(i, segment)| {
                            let texture = (segment.thumbnail.len()
                                == THUMBNAIL_SIZE.0 * THUMBNAIL_SIZE.1 * 3)
                                .then(|| {
                                    ctx.load_texture(
                                        format!("Segmento {i}"),
                                        egui::ColorImage::from_rgb(
                                            [THUMBNAIL_SIZE.0, THUMBNAIL_SIZE.1],
                                            &segment.thumbnail,
                                        ),
                                        egui::TextureOptions::LINEAR,
                                    )
                                });
                            Found {
                                segment,
                                selected: true,
                                texture,
                            }
                        })
                        .collect();
                    if let Some(verdict) = ai.verdict {
                        for change in verdict.clips {
                            let found = &mut self.found[change.index];
                            change.apply(
                                &mut found.segment.score,
                                &mut found.segment.scene,
                                &mut found.selected,
                            );
                        }
                        if let Some(template) = verdict.template {
                            self.hub.select(template);
                        }
                        self.suggested_filter = verdict.filter;
                    }
                    if self.auto_create {
                        self.create(ctx, color);
                    }
                }
                Err(error) => self.notice = Some((error, true)),
            }
            self.auto_create = false;
        } else if analyzing && self.analysis.is_none() {
            self.notice = Some(("Analisi del video interrotta.".into(), true));
            self.auto_create = false;
        }
        if let Some(mut task) = self.task.take() {
            if let Some(result) = Job::take(&mut task.reply) {
                self.apply_task(result, task.cancel.load(Ordering::Relaxed));
            } else if task.reply.is_some() {
                self.task = Some(task);
            } else {
                self.notice = Some(("Operazione interrotta.".into(), true));
            }
        }
        if visible && (self.analysis.is_some() || self.task.is_some() || self.clock.is_some()) {
            ctx.request_repaint_after(POLL_INTERVAL);
        }
    }

    fn apply_task(&mut self, result: Result<TaskResult, String>, cancelled: bool) {
        match result {
            Ok(TaskResult::Music(music, message)) => {
                if let Some(message) = message {
                    self.notice = Some((message, false));
                }
                self.music = Some(music);
                self.update_music();
            }
            Ok(TaskResult::Beat(beats)) => {
                let changes = self
                    .movie
                    .as_mut()
                    .map_or(0, |movie| movie.sync_to_beats(&beats));
                self.sync_done = !beats.is_empty();
                self.pause();
                self.invalidate();
                let message = if beats.is_empty() {
                    "Nessun battito rilevato nel tratto di musica selezionato.".to_owned()
                } else if changes == 0 {
                    "Nessun taglio da riallineare ai battiti vicini.".to_owned()
                } else {
                    format!("Tagli riallineati al ritmo: {changes}")
                };
                self.notice = Some((message, false));
            }
            Ok(TaskResult::Export(message, warnings)) => {
                self.notice = Some((message, false));
                self.warnings = warnings;
            }
            Ok(TaskResult::Dismissed) => (),
            Err(message) => self.notice = Some((message, !cancelled)),
        }
    }

    pub fn showing_preview(&self) -> bool {
        self.showing_preview && self.has_clips()
    }
    pub fn has_clips(&self) -> bool {
        self.movie
            .as_ref()
            .is_some_and(|movie| !movie.clips.is_empty())
    }
    pub fn aspect(&self) -> f32 {
        let (w, h) = PREVIEW_SIZES[self.movie_output().aspect];
        w as f32 / h as f32
    }

    fn locked_aspect(&self) -> Option<usize> {
        let info = self.movie.as_ref()?.template.info();
        info.aspect_locked.then_some(info.aspect)
    }

    fn movie_output(&self) -> OutputProfile {
        OutputProfile {
            aspect: self.locked_aspect().unwrap_or(self.output.aspect),
            ..self.output
        }
    }

    pub fn paint_preview(&mut self, ui: &Ui, rect: Rect) {
        let aspect = self.movie_output().aspect;
        if let Some(movie) = &self.movie {
            let length = movie.duration();
            self.position = self.position.min(length);
            if let Some((start, offset)) = self.clock {
                self.position = (offset + start.elapsed().as_secs_f64()).min(length);
                if self.position >= length {
                    self.clock = None;
                }
            }
            let time = ((self.position * PREVIEW_FPS).floor() / PREVIEW_FPS)
                .min((length - 1.0 / f64::from(movie::FPS)).max(0.0));
            if self.preview_error.is_none() {
                let preview = self
                    .preview
                    .get_or_insert_with(|| MoviePreview::new(ui.ctx()));
                if let Err(error) = preview.update(ui.ctx(), movie, self.revision, time, aspect) {
                    self.preview_error = Some(error);
                    self.clock = None;
                }
            }
        }
        ui.painter().rect_filled(rect, 8, theme::SIDE);
        if let Some(texture) = self
            .preview
            .as_ref()
            .and_then(|preview| preview.texture.as_ref())
            .filter(|_| self.preview_error.is_none())
        {
            let [w, h] = texture.size();
            theme::paint_image(
                ui,
                theme::fit(rect, w as f32 / h as f32),
                texture,
                theme::FULL_UV,
            );
        } else {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                self.preview_error
                    .as_deref()
                    .unwrap_or("Preparazione anteprima del filmato…"),
                egui::FontId::proportional(14.0),
                theme::MUTED,
            );
        }
        theme::image_frame(ui, rect);
    }

    pub fn timeline(&mut self, ui: &mut Ui, width: f32) -> bool {
        let Some(movie) = self.movie.as_ref().filter(|movie| !movie.clips.is_empty()) else {
            return false;
        };
        let timeline = movie.timeline();
        let shortened: Vec<_> = movie
            .shortened_slots()
            .map(|i| (i + 1).to_string())
            .collect();
        let mut open_export = false;
        let mut seek = None;
        let was_preview = self.showing_preview;
        theme::card(9).show(ui, |ui| {
            ui.set_width((width - 18.0).max(1.0));
            ui.horizontal_wrapped(|ui| {
                theme::heading(ui, "Timeline");
                theme::muted(
                    ui,
                    format!("{} clip · {:.1} s", movie.clips.len(), timeline.end),
                );
                if theme::chip(ui, "Sorgente", !self.showing_preview).clicked() {
                    self.showing_preview = false;
                    self.clock = None;
                }
                if theme::chip(ui, "Filmato", self.showing_preview).clicked() {
                    self.showing_preview = true;
                }
                if ui.button("Esporta filmato").clicked() {
                    open_export = true;
                }
                if !shortened.is_empty() {
                    ui.colored_label(theme::ERROR, "Spazi accorciati")
                        .on_hover_text(format!(
                            "Sorgente troppo breve negli spazi {}.",
                            shortened.join(", ")
                        ));
                }
            });
            if self.showing_preview {
                ui.horizontal(|ui| {
                    if ui
                        .button(if self.clock.is_some() {
                            "Pausa"
                        } else {
                            "Riproduci filmato"
                        })
                        .clicked()
                    {
                        if self.clock.is_some() {
                            self.clock = None;
                        } else {
                            if self.position >= timeline.end {
                                self.position = 0.0;
                            }
                            self.clock = Some((Instant::now(), self.position));
                            ui.ctx().request_repaint();
                        }
                    }
                    ui.spacing_mut().slider_width = (ui.available_width() - 90.0).max(24.0);
                    if ui
                        .add(
                            egui::Slider::new(&mut self.position, 0.0..=timeline.end)
                                .show_value(false),
                        )
                        .on_hover_text("Posizione nel filmato")
                        .changed()
                    {
                        if self.clock.is_some() {
                            self.clock = Some((Instant::now(), self.position));
                        }
                        ui.ctx().request_repaint();
                    }
                    ui.label(format!(
                        "{} / {}",
                        format_time(self.position),
                        format_time(timeline.end)
                    ));
                });
            }
            egui::ScrollArea::horizontal()
                .id_salt("clip_filmato")
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        for (i, clip) in movie.clips.iter().enumerate() {
                            let title = if let Some(expected) = movie.slot_duration(i) {
                                let end =
                                    timeline.starts.get(i + 1).copied().unwrap_or(timeline.end);
                                let actual = end - timeline.starts[i];
                                if actual < expected - 0.001 {
                                    format!("Spazio {} · {:.1}/{:.1} s", i + 1, actual, expected)
                                } else {
                                    format!("Spazio {} · {:.1} s", i + 1, expected)
                                }
                            } else {
                                format!("Clip {} · {:.1} s", i + 1, clip.duration())
                            };
                            let texture = self
                                .found
                                .get(clip.segment_index)
                                .and_then(|found| found.texture.as_ref());
                            ui.vertical(|ui| {
                                if thumb_button(ui, texture, self.selected == i, &title).clicked() {
                                    self.selected = i;
                                    self.showing_preview = true;
                                    seek = Some(timeline.starts[i]);
                                }
                                ui.label(RichText::new(&title).small());
                            });
                        }
                    })
                });
        });
        if let Some(position) = seek {
            self.position = position;
            self.pause();
        }
        if self.showing_preview != was_preview || seek.is_some() {
            ui.ctx().request_repaint();
        }
        open_export
    }

    fn start_task(
        &mut self,
        ctx: &egui::Context,
        kind: TaskKind,
        work: impl FnOnce(&AtomicU8, &AtomicBool) -> Result<TaskResult, String> + Send + 'static,
    ) {
        if self.task.is_some() || self.music_library.transferring() {
            return;
        }
        let progress = Arc::new(AtomicU8::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let meter = Arc::clone(&progress);
        let signal = Arc::clone(&cancel);
        let reply = Job::start(ctx, move || work(&meter, &signal));
        self.task = Some(Task {
            kind,
            reply: Some(reply),
            progress,
            cancel,
        });
        self.notice = None;
        self.warnings.clear();
    }

    fn start_sync(&mut self, ctx: &egui::Context) {
        let Some(music) = self.music.clone() else {
            return;
        };
        let Some(movie) = self.movie.as_ref().filter(|movie| movie.clips.len() > 1) else {
            return;
        };
        let seconds = movie.duration();
        self.start_task(ctx, TaskKind::Beat, move |_, _| {
            music.beats(seconds).map(TaskResult::Beat)
        });
    }

    fn update_music(&mut self) {
        self.sync_done = false;
        if let Some(movie) = &mut self.movie {
            movie.music = self.music.clone();
        }
    }

    fn choose_music(&mut self, ctx: &egui::Context) {
        self.start_task(ctx, TaskKind::ChooseMusic, |_, _| {
            let path = rfd::FileDialog::new()
                .set_title("Seleziona musica locale")
                .add_filter("File audio", &["mp3", "m4a", "aac", "wav", "wma"])
                .add_filter("Tutti i file", &["*"])
                .pick_file();
            match path {
                Some(path) => Music::open(path).map(|music| TaskResult::Music(music, None)),
                None => Ok(TaskResult::Dismissed),
            }
        });
    }

    fn extract_music(&mut self, ctx: &egui::Context) {
        let source = playback::primary_lens(&self.source).unwrap_or_else(|| self.source.clone());
        self.start_task(ctx, TaskKind::ExtractMusic, move |_, cancel| {
            let Some(path) = save_as(
                "Estrai musica dal video",
                "Audio AAC",
                "m4a",
                "Audio-Studio360.m4a",
            )?
            else {
                return Ok(TaskResult::Dismissed);
            };
            let duration = video::extract_audio(&source, &path, || cancel.load(Ordering::Relaxed))
                .map_err(|error| error.to_string())?;
            let message = format!("Musica estratta in {}", path.display());
            Ok(TaskResult::Music(
                Music {
                    path,
                    start: 0.0,
                    duration,
                    credit: None,
                },
                Some(message),
            ))
        });
    }

    fn export_movie(&mut self, ctx: &egui::Context, watermark: bool) {
        let Some(movie) = self.movie.clone() else {
            return;
        };
        let output = self.movie_output();
        self.start_task(ctx, TaskKind::Export, move |progress, cancel| {
            let Some(path) = save_as(
                "Esporta filmato",
                "Video MP4",
                "mp4",
                "Filmato-Studio360.mp4",
            )?
            else {
                return Ok(TaskResult::Dismissed);
            };
            let result = movie::render_movie(
                &movie,
                &path,
                output,
                watermark,
                |p| progress.store(p, Ordering::Relaxed),
                || cancel.load(Ordering::Relaxed),
            )?;
            Ok(TaskResult::Export(
                format!("Filmato salvato in {}", path.display()),
                result.warnings,
            ))
        });
    }

    fn export_selected(&mut self, ctx: &egui::Context, color: Adjust, watermark: bool) {
        let movies: Vec<_> = self
            .found
            .iter()
            .filter(|found| found.selected)
            .map(|found| {
                let mut movie = Movie::from_segment(&found.segment);
                movie.colore = color;
                movie.overlays = self.overlays.clone();
                movie
            })
            .collect();
        if movies.is_empty() {
            return;
        }
        let output = self.output;
        self.start_task(ctx, TaskKind::Export, move |progress, cancel| {
            let Some(dir) = rfd::FileDialog::new()
                .set_title("Salva i segmenti selezionati")
                .pick_folder()
            else {
                return Ok(TaskResult::Dismissed);
            };
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let mut warnings = Vec::new();
            let mut saved = 0;
            for (i, movie) in movies.iter().enumerate() {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                let path = dir.join(format!("Studio360-{stamp}-AutoEdit-{:02}.mp4", i + 1));
                let result = movie::render_movie(
                    movie,
                    &path,
                    output,
                    watermark,
                    |p| {
                        progress.store(
                            ((100 * i + usize::from(p)) / movies.len()) as u8,
                            Ordering::Relaxed,
                        )
                    },
                    || cancel.load(Ordering::Relaxed),
                );
                match result {
                    Ok(result) => {
                        saved += 1;
                        warnings.extend(result.warnings);
                    }
                    Err(error) if !cancel.load(Ordering::Relaxed) => {
                        warnings.push(format!("Segmento {}: {error}", i + 1))
                    }
                    Err(_) => break,
                }
            }
            let message = if cancel.load(Ordering::Relaxed) {
                format!(
                    "Esportazione annullata. Segmenti salvati: {saved}/{}.",
                    movies.len()
                )
            } else {
                format!(
                    "Segmenti salvati: {saved}/{} · {}",
                    movies.len(),
                    dir.display()
                )
            };
            Ok(TaskResult::Export(message, warnings))
        });
    }

    fn status(&mut self, ui: &mut Ui) {
        if let Some(task) = &self.task {
            match task.kind {
                TaskKind::Export => {
                    let percent = task.progress.load(Ordering::Relaxed);
                    ui.add(
                        egui::ProgressBar::new(f32::from(percent) / 100.0)
                            .text(format!("Rendering del video… {percent}%")),
                    );
                    let cancelled = task.cancel.load(Ordering::Relaxed);
                    if ui
                        .add_enabled(
                            !cancelled,
                            egui::Button::new(if cancelled {
                                "Annullamento…"
                            } else {
                                "Annulla"
                            }),
                        )
                        .clicked()
                    {
                        task.cancel.store(true, Ordering::Relaxed);
                    }
                }
                TaskKind::Beat => theme::muted(ui, "Sincronizzazione al ritmo…"),
                TaskKind::ChooseMusic => theme::muted(ui, "Selezione musica locale…"),
                TaskKind::ExtractMusic => theme::muted(ui, "Estrazione della musica…"),
            }
        }
        if let Some((message, failed)) = self
            .notice
            .as_ref()
            .filter(|(message, _)| message != REQUIRED_SLOT)
        {
            ui.colored_label(if *failed { theme::ERROR } else { theme::TEXT }, message);
        }
        for warning in &self.warnings {
            ui.colored_label(theme::ERROR, warning);
        }
    }

    pub fn show_auto_edit(
        &mut self,
        ui: &mut Ui,
        color: Adjust,
        watermark: bool,
        spherical: bool,
        settings: &Settings,
        online: &Online,
    ) {
        self.status(ui);
        if self.analysis.is_some() {
            let (fraction, label) = ai_provider::progress_display(
                self.progress.load(Ordering::Relaxed),
                "Analisi del video…",
            );
            ui.add(egui::ProgressBar::new(fraction).text(label));
            if ui.button("Annulla analisi").clicked() {
                self.analysis_cancel.store(true, Ordering::Relaxed);
                self.analysis = None;
                self.auto_create = false;
                self.notice = Some(("Analisi video annullata.".into(), false));
            }
            return;
        }
        if !self.analyzed {
            if ui.button("Riprova analisi").clicked() {
                self.analyze(ui.ctx(), false, settings, online);
            }
            return;
        }
        if let Some(source) = &self.analysis_source {
            autoedit::show_analysis_source(ui, source);
        }
        if self.found.is_empty() {
            ui.label(NO_VIDEO_SUBJECT);
            return;
        }
        ui.add_enabled_ui(self.task.is_none(), |ui| {
            let target = TargetDuration::ALL[self.target];
            let (_, segments) = self.selected_segments();
            let active = self.movie.as_ref().map(|movie| movie.template);
            self.hub.show(
                ui,
                active,
                &segments,
                target,
                !settings.preferences.performance && !settings.preferences.compatibility,
            );
            let info = self.hub.selected.info();
            ui.label(info.description);
            theme::muted(ui, format!("{} BPM · {}", info.bpm, ASPECTS[info.aspect]));
            match info.kind {
                TemplateKind::Free => {
                    combo_row(
                        ui,
                        "Durata",
                        "durata_obiettivo",
                        &mut self.target,
                        TargetDuration::LABELS,
                    );
                }
                TemplateKind::Sync { seconds, .. } => {
                    ui.add_enabled(
                        false,
                        egui::Button::new(format!("Durata · {}", format_time(seconds))),
                    )
                    .on_disabled_hover_text(FIXED_DURATION);
                    theme::muted(ui, FIXED_DURATION);
                }
            }
            let label = if self.movie.is_some() {
                "Cambia modello"
            } else {
                "Usa modello"
            };
            if ui
                .add_enabled(!segments.is_empty(), theme::primary(label))
                .clicked()
            {
                self.create(ui.ctx(), color);
            }
            self.template_filter(ui);
            let recommend_music = self.music.is_none()
                && ui
                    .add_enabled(
                        online.enabled()
                            && !self.music_library.searching()
                            && !self.music_library.transferring(),
                        egui::Button::new("Musica consigliata"),
                    )
                    .on_disabled_hover_text(if online.enabled() {
                        "Attendi il completamento dell’operazione in corso."
                    } else {
                        "Attiva internet nelle impostazioni per cercare musica."
                    })
                    .clicked();
            if recommend_music {
                self.music_library
                    .recommend(ui.ctx(), online, info.music_query);
            }
            ui.separator();
            egui::CollapsingHeader::new(format!("Segmenti rilevati ({})", self.found.len()))
                .id_salt("segmenti_rilevati")
                .default_open(true)
                .show(ui, |ui| self.segments(ui));
            let music = egui::CollapsingHeader::new("Musica")
                .open(recommend_music.then_some(true))
                .show(ui, |ui| self.music_controls(ui, online));
            if recommend_music {
                music.header_response.scroll_to_me(Some(egui::Align::TOP));
            }
            self.clip_controls(ui, spherical);
            self.export_controls(ui, color, watermark);
        });
    }

    fn template_filter(&mut self, ui: &mut Ui) {
        let Some(movie) = self.movie.as_mut().filter(|movie| !movie.clips.is_empty()) else {
            return;
        };
        let mut enabled = movie.template_filter_enabled();
        if ui.checkbox(&mut enabled, "Filtro del modello").changed() {
            movie.set_template_filter(enabled);
            self.invalidate();
        }
        if !enabled {
            theme::muted(ui, "Rimosso il filtro del modello da tutte le clip");
        }
    }

    pub fn show_export(&mut self, ui: &mut Ui, color: Adjust, watermark: bool) {
        self.status(ui);
        if self.has_clips() || !self.found.is_empty() {
            ui.add_enabled_ui(self.task.is_none(), |ui| {
                self.export_controls(ui, color, watermark)
            });
        } else {
            theme::muted(ui, "Apri Auto Edit per creare un filmato dal video.");
        }
    }

    fn export_controls(&mut self, ui: &mut Ui, color: Adjust, watermark: bool) {
        ui.separator();
        theme::heading(ui, "Esportazione MP4");
        combo_row(
            ui,
            "Risoluzione",
            "risoluzione_filmato",
            &mut self.output.resolution,
            &RESOLUTIONS,
        );
        if let Some(mut aspect) = self.locked_aspect() {
            ui.add_enabled_ui(false, |ui| {
                combo_row(ui, "Formato", "formato_filmato", &mut aspect, &ASPECTS);
            });
            theme::muted(ui, FIXED_ASPECT);
        } else if combo_row(
            ui,
            "Formato",
            "formato_filmato",
            &mut self.output.aspect,
            &ASPECTS,
        ) {
            self.invalidate();
        }
        let idle = !self.music_library.transferring();
        if ui
            .add_enabled(idle && self.has_clips(), theme::primary("Esporta filmato"))
            .clicked()
        {
            self.export_movie(ui.ctx(), watermark);
        }
        if ui
            .add_enabled(
                idle && self.found.iter().any(|found| found.selected),
                egui::Button::new("Esporta selezionate"),
            )
            .clicked()
        {
            self.export_selected(ui.ctx(), color, watermark);
        }
    }

    fn segments(&mut self, ui: &mut Ui) {
        let slot = self
            .movie
            .as_ref()
            .and_then(|movie| movie.slot_duration(self.selected))
            .map(|_| self.selected + 1);
        let mut replace = None;
        egui::ScrollArea::vertical()
            .id_salt("elenco_segmenti")
            .max_height(170.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for (i, found) in self.found.iter_mut().enumerate() {
                    let segment = &found.segment;
                    ui.push_id(i, |ui| {
                        ui.horizontal(|ui| {
                            let label = format!(
                                "{}–{}",
                                format_time(segment.start),
                                format_time(segment.end)
                            );
                            let checkbox = ui
                                .checkbox(&mut found.selected, "")
                                .on_hover_text(format!("Includi il segmento {label}"));
                            checkbox.widget_info(|| {
                                egui::WidgetInfo::selected(
                                    egui::Role::CheckBox,
                                    true,
                                    found.selected,
                                    &label,
                                )
                            });
                            if let Some(texture) = &found.texture {
                                ui.add(
                                    egui::Image::new(texture)
                                        .fit_to_exact_size(egui::vec2(68.0, 38.25)),
                                );
                            }
                            ui.vertical(|ui| {
                                ui.label(&label);
                                ui.label(
                                    RichText::new(format!(
                                        "{} · {}/100",
                                        segment.scene.label(),
                                        segment.score
                                    ))
                                    .small()
                                    .color(theme::MUTED),
                                );
                            });
                        });
                        if let Some(slot) = slot {
                            if ui.button(format!("Metti nello spazio {slot}")).clicked() {
                                replace = Some(i);
                            }
                        }
                    });
                }
            });
        if let Some(index) = replace {
            self.replace_clip(index);
        }
    }

    fn replace_clip(&mut self, index: usize) {
        if let Some(movie) = &mut self.movie {
            movie.replace(self.selected, &self.found[index].segment, index);
            if movie.slot_duration(self.selected).is_some() {
                self.position = movie.timeline().starts[self.selected];
                self.notice = None;
            }
            self.edited();
        }
    }

    pub fn show_music(&mut self, ui: &mut Ui, online: &Online) {
        theme::heading(ui, "Musica");
        self.status(ui);
        self.music_controls(ui, online);
    }

    fn music_controls(&mut self, ui: &mut Ui, online: &Online) {
        let idle = self.task.is_none() && !self.music_library.transferring();
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    idle,
                    egui::Button::new("Nessuna")
                        .selected(self.music.is_none() && !self.music_library.open),
                )
                .clicked()
            {
                self.music = None;
                self.music_library.open = false;
                self.update_music();
            }
            if ui
                .add_enabled(idle, egui::Button::new("Locale…"))
                .on_hover_text("Aggiungi musica locale")
                .clicked()
            {
                self.music_library.open = false;
                self.choose_music(ui.ctx());
            }
            if theme::chip(ui, "Online", self.music_library.open).clicked() {
                self.music_library.open = !self.music_library.open;
                if !self.music_library.open {
                    self.music_library.stop_preview();
                }
            }
        });
        if ui
            .add_enabled(idle, egui::Button::new("Estrai dal video…"))
            .clicked()
        {
            self.music_library.open = false;
            self.extract_music(ui.ctx());
        }
        if let Some(music) = &mut self.music {
            if let Some(credit) = &music.credit {
                ui.hyperlink_to(&credit.title, &credit.link);
                theme::muted(ui, &credit.creator);
                ui.hyperlink_to(&credit.license, &credit.license_url);
            } else {
                let name = playback::file_name(&music.path);
                ui.add(egui::Label::new(name.clone()).truncate())
                    .on_hover_text(name);
            }
            ui.label("Punto di inizio");
            let end = (music.duration - 0.01).max(0.0);
            let changed = if end > 0.0 {
                ui.add(
                    egui::Slider::new(&mut music.start, 0.0..=end)
                        .suffix(" s")
                        .fixed_decimals(2),
                )
                .changed()
            } else {
                ui.add(
                    egui::DragValue::new(&mut music.start)
                        .range(0.0..=f64::MAX)
                        .speed(0.05)
                        .suffix(" s")
                        .fixed_decimals(2),
                )
                .changed()
            };
            if changed {
                self.update_music();
            }
            let can_sync = self
                .movie
                .as_ref()
                .is_some_and(|movie| movie.clips.len() > 1);
            if ui
                .add_enabled(
                    can_sync && idle,
                    egui::Button::new(if self.sync_done {
                        "Sincronizza di nuovo al ritmo"
                    } else {
                        "Sincronizza al ritmo"
                    }),
                )
                .on_disabled_hover_text("Servono almeno due clip.")
                .clicked()
            {
                self.start_sync(ui.ctx());
            }
        }
        if let Some(movie) = &mut self.movie {
            ui.collapsing("Mixer", |ui| {
                ui.label("Volume musica");
                ui.add_enabled(
                    movie.music.is_some(),
                    egui::Slider::new(&mut movie.music_volume, 0.0..=1.0).fixed_decimals(2),
                );
                ui.label("Audio originale");
                ui.add(
                    egui::Slider::new(&mut movie.original_audio_volume, 0.0..=1.0)
                        .fixed_decimals(2),
                );
            });
        }
        if self.music_library.open {
            self.music_library.show(ui, online, self.task.is_none());
        }
    }

    fn clip_controls(&mut self, ui: &mut Ui, spherical: bool) {
        let Some(movie) = &mut self.movie else { return };
        let sync = matches!(movie.template.info().kind, TemplateKind::Sync { .. });
        let filter_enabled = movie.template_filter_enabled();
        let mut edited = false;
        let mut replace = None;
        let mut delete = false;
        let mut all_speed = None;
        ui.collapsing("Modifica clip", |ui| {
            let has_next = self.selected + 1 < movie.clips.len();
            let Some(clip) = movie.clips.get_mut(self.selected) else {
                theme::muted(ui, "Crea un filmato per aggiungere clip.");
                return;
            };
            let label = if sync { "Spazio" } else { "Clip" };
            ui.label(format!(
                "{label} {} · {:.2} s",
                self.selected + 1,
                clip.duration()
            ));
            ui.horizontal_wrapped(|ui| {
                ui.menu_button("Sostituisci", |ui| {
                    for (i, item) in self.found.iter().enumerate() {
                        if i != clip.segment_index
                            && ui
                                .button(format!(
                                    "{}–{} · {}",
                                    format_time(item.segment.start),
                                    format_time(item.segment.end),
                                    item.segment.scene.label()
                                ))
                                .clicked()
                        {
                            replace = Some(i);
                            ui.close();
                        }
                    }
                    if self.found.len() < 2 {
                        ui.label("Nessun altro segmento");
                    }
                });
                if ui.button("Elimina").clicked() {
                    if sync {
                        self.notice = Some((REQUIRED_SLOT.into(), false));
                    } else {
                        delete = true;
                    }
                }
            });
            if sync
                && self
                    .notice
                    .as_ref()
                    .is_some_and(|(text, _)| text == REQUIRED_SLOT)
            {
                theme::muted(ui, REQUIRED_SLOT);
            }
            ui.add_enabled_ui(filter_enabled, |ui| {
                edited |= combo_row(
                    ui,
                    "Filtro clip",
                    "filtro_clip",
                    &mut clip.filter.0,
                    FILTER_LABELS,
                );
            });
            if sync {
                theme::muted(ui, format!("Velocità del modello · {}×", clip.speed.rate()));
                if has_next {
                    theme::muted(ui, format!("Transizione · {}", clip.transition_out.label()));
                }
            } else {
                let mut transition = clip.transition_out as usize;
                ui.add_enabled_ui(has_next, |ui| {
                    if combo_row(
                        ui,
                        "Transizione in uscita",
                        "transizione",
                        &mut transition,
                        Transition::LABELS,
                    ) {
                        clip.transition_out = Transition::ALL[transition];
                        edited = true;
                    }
                })
                .response
                .on_disabled_hover_text(format!(
                    "{}. Serve una clip successiva.",
                    clip.transition_out.label()
                ));
                ui.collapsing("Velocità", |ui| {
                    edited |= speed_controls(ui, clip);
                    if ui.button("Applica a tutte le clip").clicked() {
                        all_speed = Some(clip.speed);
                        edited = true;
                    }
                });
            }
            ui.add_enabled_ui(spherical, |ui| {
                ui.collapsing("Movimento", |ui| {
                    ui.label("Vista iniziale");
                    edited |= movement(ui, "iniziale", &mut clip.view);
                    ui.label("Vista finale");
                    edited |= movement(ui, "finale", &mut clip.end_view);
                });
            });
        });
        if delete && self.selected < movie.clips.len() {
            movie.clips.remove(self.selected);
            self.selected = self.selected.min(movie.clips.len().saturating_sub(1));
            if movie.clips.is_empty() {
                self.showing_preview = false;
            }
            edited = true;
        }
        if let Some(speed) = all_speed {
            for clip in &mut movie.clips {
                clip.speed = speed;
            }
        }
        ui.collapsing("Regolazioni filmato", |ui| {
            edited |= combo_row(
                ui,
                "Filtro filmato",
                "filtro_filmato",
                &mut movie.filter.0,
                FILTER_LABELS,
            );
            ui.collapsing("Colore", |ui| {
                if ui.button("Reimposta").clicked() {
                    movie.colore = Adjust::default();
                    edited = true;
                }
                for (index, label) in ADJUST_NAMES.iter().enumerate() {
                    ui.label(*label);
                    edited |= ui
                        .add(
                            egui::Slider::new(&mut movie.colore.v[index], -100.0..=100.0)
                                .step_by(1.0),
                        )
                        .changed();
                }
            });
        });
        ui.collapsing("Statistiche", |ui| {
            let duration = match movie.template.info().kind {
                TemplateKind::Free => format!("Obiettivo {}", movie.target_duration.label()),
                TemplateKind::Sync { seconds, .. } => format!("Sync · {}", format_time(seconds)),
            };
            ui.label(format!("{} · {duration}", movie.template.label()));
            ui.label(format!(
                "{} clip · {:.2} s · {} fps",
                movie.clips.len(),
                movie.duration(),
                movie::FPS
            ));
        });
        if let Some(index) = replace {
            self.replace_clip(index);
        } else if edited {
            self.edited();
        }
    }
}

fn speed_controls(ui: &mut Ui, clip: &mut movie::Clip) -> bool {
    let mut choice = match clip.speed {
        Speed::Constant(_) => 0,
        Speed::Variable { curve, .. } => curve as usize + 1,
    };
    let labels = [
        "Velocità personalizzata",
        Curve::Ramp.label(),
        Curve::SlowCenter.label(),
        Curve::Accelerate.label(),
    ];
    let mut rate = clip.speed.rate();
    ui.label("Curva");
    let mut changed = theme::combo(ui, "curva_velocita", &mut choice, &labels);
    changed |= ui
        .add(
            egui::Slider::new(&mut rate, MIN_PLAYBACK_SPEED..=MAX_PLAYBACK_SPEED)
                .step_by(0.05)
                .suffix("×"),
        )
        .changed();
    let mut duration = clip.duration();
    ui.label("Durata d’uscita");
    let span = clip.end - clip.start;
    if ui
        .add(
            egui::DragValue::new(&mut duration)
                .range(span / MAX_PLAYBACK_SPEED..=span / MIN_PLAYBACK_SPEED)
                .suffix(" s")
                .fixed_decimals(2)
                .speed(0.02),
        )
        .changed()
    {
        rate = span / duration;
        changed = true;
    }
    if changed {
        clip.speed = if choice == 0 {
            Speed::Constant(rate)
        } else {
            Speed::Variable {
                rate,
                curve: Curve::ALL[choice - 1],
            }
        };
    }
    if ui.button("Ripristina durata originale").clicked() {
        clip.speed = Speed::Constant(1.0);
        changed = true;
    }
    changed
}

fn combo_row(ui: &mut Ui, label: &str, id: &str, selected: &mut usize, options: &[&str]) -> bool {
    ui.label(label);
    theme::combo(ui, id, selected, options)
}

fn movement(ui: &mut Ui, id: &str, view: &mut View) -> bool {
    ui.push_id(id, |ui| {
        ui.add(
            egui::Slider::new(&mut view.yaw, -180.0..=180.0)
                .text("Imbardata")
                .suffix("°"),
        )
        .changed()
            | ui.add(
                egui::Slider::new(&mut view.pitch, -89.0..=89.0)
                    .text("Inclinazione")
                    .suffix("°"),
            )
            .changed()
            | ui.add(
                egui::Slider::new(&mut view.fov, 30.0..=160.0)
                    .text("Campo visivo")
                    .suffix("°"),
            )
            .changed()
    })
    .inner
}

fn thumb_button(
    ui: &mut Ui,
    texture: Option<&egui::TextureHandle>,
    selected: bool,
    label: &str,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(100.0, 56.25), egui::Sense::click());
    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, selected, label));
    if let Some(texture) = texture {
        theme::paint_image(ui, rect, texture, theme::FULL_UV);
    } else {
        ui.painter().rect_filled(rect, 4, theme::SIDE);
    }
    let color = if selected || response.has_focus() {
        theme::ACCENT
    } else {
        theme::LINE
    };
    ui.painter().rect_stroke(
        rect,
        4,
        Stroke::new(if selected { 2.0 } else { 1.0 }, color),
        StrokeKind::Inside,
    );
    response.on_hover_text(label)
}

fn save_as(
    title: &str,
    format: &str,
    extension: &str,
    suggested: &str,
) -> Result<Option<PathBuf>, String> {
    let Some(path) = rfd::FileDialog::new()
        .set_title(title)
        .add_filter(format, &[extension])
        .set_file_name(suggested)
        .save_file()
    else {
        return Ok(None);
    };
    match path.extension() {
        Some(value) if value.eq_ignore_ascii_case(extension) => Ok(Some(path)),
        Some(_) => Err(format!("Scegli un nome con estensione .{extension}.")),
        None => Ok(Some(path.with_extension(extension))),
    }
}
