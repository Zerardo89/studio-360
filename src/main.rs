#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod ai_provider;
mod album;
mod app;
mod autoedit;
mod capture;
mod editor;
mod emoji_catalog;
mod export;
mod fisheye;
mod home;
mod movie;
mod movie_editor;
mod music_library;
mod online;
#[cfg(test)]
mod online_tests;
mod overlay;
mod overlay_editor;
#[cfg(test)]
mod overlay_tests;
mod panorama;
mod playback;
mod settings;
mod template_hub;
mod templates;
mod theme;
mod video;

use std::{
    path::PathBuf,
    process::ExitCode,
    sync::{Arc, Mutex},
};

use app::{App, Screenshot, ScreenshotResult, Tab};
use editor::InitialPanel;
use eframe::egui;

const USAGE: &str =
    "studio360 [--tab <home|album|scatto|modifica|impostazioni>] [--open <file>] [--auto-edit] [--panel <musica|sticker|testo|modelli>] [--screenshot <file.png>] [--export-movie <file.mp4>]\n\
     --open carica il file in Modifica, che diventa la scheda iniziale se manca --tab.\n\
     --auto-edit avvia l’analisi in Modifica e crea il primo modello per i video.\n\
     --panel apre un pannello di Modifica.\n\
     --export-movie esporta il video aperto come filmato a 720p ed esce.";

struct Arguments {
    tab: Tab,
    screenshot: Option<PathBuf>,
    auto_edit: bool,
    open: Option<PathBuf>,
    export_movie: Option<PathBuf>,
    panel: Option<InitialPanel>,
}

fn arguments() -> Result<Option<Arguments>, String> {
    let mut args = std::env::args_os().skip(1);
    let mut tab_selected = false;
    let mut result = Arguments {
        tab: Tab::Home,
        screenshot: None,
        auto_edit: false,
        open: None,
        export_movie: None,
        panel: None,
    };
    while let Some(flag) = args.next() {
        match flag.to_str() {
            Some("--help" | "-h") => return Ok(None),
            Some("--tab") => {
                tab_selected = true;
                result.tab = args
                    .next()
                    .as_deref()
                    .and_then(|value| value.to_str())
                    .and_then(Tab::parse)
                    .ok_or("Scheda iniziale non valida.")?;
            }
            Some("--open") => {
                result.open = Some(
                    args.next()
                        .map(PathBuf::from)
                        .filter(|path| !path.as_os_str().is_empty())
                        .ok_or("Specificare la foto o il video da aprire.")?,
                );
            }
            Some("--screenshot") => {
                result.screenshot = Some(
                    args.next()
                        .map(PathBuf::from)
                        .filter(|path| !path.as_os_str().is_empty())
                        .ok_or("Specificare il file PNG.")?,
                );
            }
            Some("--auto-edit") => result.auto_edit = true,
            Some("--panel") => {
                result.panel = Some(
                    args.next()
                        .as_deref()
                        .and_then(|value| value.to_str())
                        .and_then(InitialPanel::parse)
                        .ok_or("Pannello non valido. Scegli musica, sticker, testo o modelli.")?,
                );
            }
            Some("--export-movie") => {
                result.export_movie = Some(
                    args.next()
                        .map(PathBuf::from)
                        .filter(|path| {
                            path.extension()
                                .is_some_and(|ext| ext.eq_ignore_ascii_case("mp4"))
                        })
                        .ok_or("Specificare il file MP4 di destinazione.")?,
                );
            }
            _ => return Err(format!("Opzione sconosciuta: {}", flag.to_string_lossy())),
        }
    }
    if !tab_selected && (result.open.is_some() || result.panel.is_some()) {
        result.tab = Tab::Modifica;
    }
    Ok(Some(result))
}

fn run(args: Arguments) -> Result<(), String> {
    if let Some(destination) = &args.export_movie {
        if args.screenshot.is_some() {
            return Err("Scegli un’esportazione MP4 oppure uno screenshot.".into());
        }
        let path = args
            .open
            .as_deref()
            .filter(|path| playback::is_video(path))
            .ok_or("Per esportare un filmato, specifica un video con --open.")?;
        let mut last = None;
        let segments = movie::analyze_source(
            path,
            |percent| {
                if last != Some(percent / 10) {
                    last = Some(percent / 10);
                    println!("Analisi video… {percent}%");
                }
            },
            || false,
        )?;
        if segments.is_empty() {
            return Err(autoedit::NO_VIDEO_SUBJECT.into());
        }
        let montage = movie::build_from_template(
            &segments,
            movie::Template::Dynamic,
            movie::TargetDuration::UpTo30,
        );
        let mut last = None;
        let report = movie::render_movie(
            &montage,
            destination,
            movie::OutputProfile::default(),
            false,
            |percent| {
                if percent == 0 || percent == 100 || last != Some(percent / 10) {
                    last = Some(percent / 10);
                    println!("Rendering del video… {percent}%");
                }
            },
            || false,
        )?;
        for warning in report.warnings {
            eprintln!("{warning}");
        }
        let mut reader = video::VideoReader::open(destination).map_err(|e| e.to_string())?;
        let info = reader.info();
        if info.duration <= 0.0 || reader.next_frame().map_err(|e| e.to_string())?.is_none() {
            return Err(
                "Il filmato esportato non contiene fotogrammi con una durata positiva.".into(),
            );
        }
        println!(
            "MP4 verificato: {} × {}, {:.2} s, {:.2} fps — {}",
            info.width,
            info.height,
            info.duration,
            info.fps,
            destination.display()
        );
        return Ok(());
    }
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([960.0, 600.0]);
    let result: ScreenshotResult = Arc::new(Mutex::new(None));
    let screenshot = args
        .screenshot
        .map(|path| Screenshot::new(path, Arc::clone(&result)));
    let taking_screenshot = screenshot.is_some();
    if taking_screenshot {
        viewport = viewport.with_active(false);
        #[cfg(windows)]
        {
            viewport = viewport.with_window_level(egui::WindowLevel::AlwaysOnBottom);
        }
    }
    let options = eframe::NativeOptions {
        viewport,
        ..eframe::NativeOptions::default()
    };
    eframe::run_native(
        "Studio 360",
        options,
        Box::new(move |cc| {
            Ok(Box::new(App::new(
                cc,
                args.tab,
                screenshot,
                args.auto_edit,
                args.open,
                args.panel,
            )))
        }),
    )
    .map_err(|e| format!("Impossibile avviare Studio 360: {e}"))?;
    if taking_screenshot {
        return result
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .ok_or("La finestra è stata chiusa prima di salvare lo screenshot.")?;
    }
    Ok(())
}

fn main() -> ExitCode {
    match arguments() {
        Ok(None) => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Ok(Some(args)) => match run(args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{error}");
                ExitCode::FAILURE
            }
        },
        Err(error) => {
            eprintln!("{error}\n{USAGE}");
            ExitCode::from(2)
        }
    }
}
