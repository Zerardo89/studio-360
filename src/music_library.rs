use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use eframe::egui::{self, Ui};

use crate::{
    app::Job,
    movie::Music,
    online::{self, Online, Track},
    playback::format_time,
    theme, video,
};

const CATEGORIES: &[(&str, &str)] = &[
    ("Allegra", "happy upbeat"),
    ("Rilassante", "calm ambient"),
    ("Epica", "epic cinematic"),
    ("Viaggio", "travel instrumental"),
    ("Sport", "energetic workout"),
    ("Elettronica", "electronic"),
    ("Acustica", "acoustic"),
];

#[derive(Default)]
pub struct MusicLibrary {
    pub open: bool,
    query: String,
    results: Vec<Track>,
    lookup: Option<Job<Result<Vec<Track>, online::Error>>>,
    transfer: Option<Transfer>,
    search_error: Option<String>,
    transfer_error: Option<String>,
}

struct Transfer {
    job: Option<Job<Result<Option<Music>, String>>>,
    progress: Arc<AtomicU64>,
    playing: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
    preview: bool,
}

impl Drop for Transfer {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl MusicLibrary {
    pub fn searching(&self) -> bool {
        self.lookup.is_some()
    }

    pub fn recommend(&mut self, ctx: &egui::Context, online: &Online, query: &str) {
        self.open = true;
        self.query = query.to_owned();
        self.results.clear();
        self.search(ctx, online);
    }

    pub fn transferring(&self) -> bool {
        self.transfer.is_some()
    }

    pub fn stop_preview(&self) {
        if let Some(transfer) = self.transfer.as_ref().filter(|transfer| transfer.preview) {
            transfer.cancel.store(true, Ordering::Relaxed);
        }
    }

    pub fn poll(&mut self) -> Option<Music> {
        let was_searching = self.lookup.is_some();
        match Job::take(&mut self.lookup) {
            Some(Ok(tracks)) => {
                self.search_error = tracks.is_empty().then(|| "Nessun risultato".to_owned());
                self.results = tracks;
            }
            Some(Err(error)) => self.search_error = Some(error.to_string()),
            None if was_searching && self.lookup.is_none() => {
                self.search_error = Some("Ricerca interrotta.".into());
            }
            None => {}
        }
        let transfer = self.transfer.as_mut()?;
        let result = Job::take(&mut transfer.job);
        if transfer.job.is_some() {
            return None;
        }
        let cancelled = transfer.cancel.load(Ordering::Relaxed);
        self.transfer = None;
        if cancelled {
            return None;
        }
        match result {
            Some(Ok(music)) => music,
            Some(Err(error)) => {
                self.transfer_error = Some(error);
                None
            }
            None => {
                self.transfer_error = Some(online::MUSIC_ERROR.to_owned());
                None
            }
        }
    }

    fn search(&mut self, ctx: &egui::Context, online: &Online) {
        if self.lookup.is_some() || self.query.trim().is_empty() {
            return;
        }
        let query = self.query.clone();
        let online = online.clone();
        self.search_error = None;
        self.lookup = Some(Job::start(ctx, move || online.search(&query)));
    }

    fn download(&mut self, ctx: &egui::Context, online: &Online, track: Track, preview: bool) {
        let online = online.clone();
        let progress = Arc::new(AtomicU64::new(0));
        let playing = Arc::new(AtomicBool::new(false));
        let cancel = Arc::new(AtomicBool::new(false));
        let meter = Arc::clone(&progress);
        let audible = Arc::clone(&playing);
        let signal = Arc::clone(&cancel);
        let repaint = ctx.clone();
        let job = Job::start(ctx, move || {
            let path = online
                .download(&track, &meter, &signal)
                .map_err(|error| format!("{} · {error}", online::MUSIC_ERROR))?;
            if signal.load(Ordering::Relaxed) {
                return Ok(None);
            }
            if preview {
                video::play_music_preview(&path, &signal, || {
                    audible.store(true, Ordering::Relaxed);
                    repaint.request_repaint();
                })
                .map_err(|error| error.to_string())?;
                return Ok(None);
            }
            let mut music =
                Music::open(path).map_err(|error| format!("{} · {error}", online::MUSIC_ERROR))?;
            music.credit = Some(track.credit);
            Ok(Some(music))
        });
        self.transfer = Some(Transfer {
            job: Some(job),
            progress,
            playing,
            cancel,
            preview,
        });
        self.transfer_error = None;
    }

    pub fn show(&mut self, ui: &mut Ui, online: &Online, available: bool) {
        ui.separator();
        if !online.enabled() {
            theme::muted(ui, "Offline · cerca e usa i contenuti già in cache.");
        }
        ui.horizontal_wrapped(|ui| {
            for &(name, keywords) in CATEGORIES {
                let chip = theme::chip(ui, name, self.query == keywords);
                if chip.clicked() && self.lookup.is_none() {
                    self.query = keywords.to_owned();
                    self.search(ui.ctx(), online);
                }
            }
        });
        let field = ui.add(
            egui::TextEdit::singleline(&mut self.query)
                .hint_text("Cerca musica su Openverse")
                .char_limit(120)
                .desired_width(f32::INFINITY),
        );
        if ui
            .add_enabled(
                self.lookup.is_none() && !self.query.trim().is_empty(),
                theme::primary("Cerca"),
            )
            .clicked()
            || (field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)))
        {
            self.search(ui.ctx(), online);
        }
        if self.lookup.is_some() {
            theme::muted(ui, "Ricerca in corso…");
        } else if let Some(error) = &self.search_error {
            ui.colored_label(
                if self.results.is_empty() && error == "Nessun risultato" {
                    theme::MUTED
                } else {
                    theme::ERROR
                },
                error,
            );
        } else if self.results.is_empty() {
            theme::muted(ui, "Scegli una categoria o cerca un brano.");
        }
        if let Some(transfer) = &self.transfer {
            let progress = transfer.progress.load(Ordering::Relaxed);
            let playing = transfer.playing.load(Ordering::Relaxed);
            if playing {
                ui.colored_label(theme::OK, "Anteprima in riproduzione");
            } else if progress > 100 {
                theme::muted(
                    ui,
                    format!("Download… {:.1} MiB", (progress - 101) as f64 / 1_048_576.0),
                );
            } else {
                ui.add(
                    egui::ProgressBar::new(progress as f32 / 100.0)
                        .text(format!("Download… {progress}%")),
                );
            }
            if ui
                .button(if playing {
                    "Ferma anteprima"
                } else {
                    "Annulla download"
                })
                .clicked()
            {
                transfer.cancel.store(true, Ordering::Relaxed);
            }
            ui.ctx().request_repaint_after(Duration::from_millis(80));
        }
        if let Some(error) = &self.transfer_error {
            ui.colored_label(theme::ERROR, error);
        }
        let mut chosen = None;
        let can_download = available && self.transfer.is_none();
        for (i, track) in self.results.iter().enumerate() {
            ui.push_id(i, |ui| {
                ui.separator();
                ui.hyperlink_to(&track.credit.title, &track.credit.link);
                let time = if track.duration > 0.0 {
                    format_time(track.duration)
                } else {
                    "Durata non indicata".into()
                };
                theme::muted(ui, format!("{} · {time}", track.credit.creator));
                ui.hyperlink_to(&track.credit.license, &track.credit.license_url);
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(can_download, egui::Button::new("Anteprima"))
                        .clicked()
                    {
                        chosen = Some((i, true));
                    }
                    if ui
                        .add_enabled(can_download, theme::primary("Usa"))
                        .clicked()
                    {
                        chosen = Some((i, false));
                    }
                });
            });
        }
        if let Some((i, preview)) = chosen {
            self.download(ui.ctx(), online, self.results[i].clone(), preview);
        }
    }
}
