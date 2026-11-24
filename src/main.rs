#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod album;
mod app;
mod capture;
mod editor;
mod export;
mod home;
mod panorama;
mod settings;
mod theme;

use std::{
    path::PathBuf,
    process::ExitCode,
    sync::{Arc, Mutex},
};

use app::{App, Screenshot, ScreenshotResult, Tab};
use eframe::egui;

const USAGE: &str =
    "studio360 [--tab <home|album|scatto|modifica|impostazioni>] [--screenshot <file.png>]";

struct Arguments {
    tab: Tab,
    screenshot: Option<PathBuf>,
}

fn arguments() -> Result<Option<Arguments>, String> {
    let mut args = std::env::args_os().skip(1);
    let mut result = Arguments {
        tab: Tab::Home,
        screenshot: None,
    };
    while let Some(flag) = args.next() {
        match flag.to_str() {
            Some("--help" | "-h") => return Ok(None),
            Some("--tab") => {
                result.tab = args
                    .next()
                    .as_deref()
                    .and_then(|value| value.to_str())
                    .and_then(Tab::parse)
                    .ok_or("Scheda iniziale non valida.")?;
            }
            Some("--screenshot") => {
                result.screenshot = Some(
                    args.next()
                        .map(PathBuf::from)
                        .filter(|path| !path.as_os_str().is_empty())
                        .ok_or("Specificare il file PNG.")?,
                );
            }
            _ => return Err(format!("Opzione sconosciuta: {}", flag.to_string_lossy())),
        }
    }
    Ok(Some(result))
}

fn run(args: Arguments) -> Result<(), String> {
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
        Box::new(move |cc| Ok(Box::new(App::new(cc, args.tab, screenshot)))),
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
