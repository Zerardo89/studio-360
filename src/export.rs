use std::{
    fs::{self, File, OpenOptions},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use eframe::egui::{self, Ui};
use image::{
    codecs::{jpeg::JpegEncoder, png::PngEncoder},
    ExtendedColorType, ImageEncoder,
};

use crate::{
    app::Job,
    panorama::{self, Adjust, PanoRef, Region, View},
    theme,
};

const RESOLUTIONS: &[&str] = &["720P", "1080P", "2.7K", "4K"];
const LONG_EDGES: [usize; 4] = [1280, 1920, 2720, 3840];
const FORMATS: &[&str] = &["PNG", "JPG"];

struct Profile {
    resolution: usize,
    format: usize,
    watermark: bool,
}

struct RenderJob {
    result: Option<Job<Result<PathBuf, String>>>,
    rows: Arc<AtomicUsize>,
    height: usize,
}

pub struct Export {
    profile: Profile,
    directory: String,
    dialog: Option<Job<Option<PathBuf>>>,
    running: Option<RenderJob>,
    result: Option<Result<PathBuf, String>>,
}

impl Export {
    pub fn new(watermark: bool) -> Self {
        let home = std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .unwrap_or_default();
        Self {
            profile: Profile {
                resolution: 1,
                format: 0,
                watermark,
            },
            directory: home
                .join("Pictures")
                .join("Studio360")
                .to_string_lossy()
                .into_owned(),
            dialog: None,
            running: None,
            result: None,
        }
    }

    pub fn set_watermark(&mut self, enabled: bool) {
        self.profile.watermark = enabled;
    }

    pub fn poll(&mut self) {
        if let Some(Some(folder)) = Job::take(&mut self.dialog) {
            self.directory = folder.to_string_lossy().into_owned();
        }
        if let Some(result) = self
            .running
            .as_mut()
            .and_then(|job| Job::take(&mut job.result))
        {
            self.running = None;
            self.result = Some(result);
        }
    }

    pub fn show(&mut self, ui: &mut Ui, pano: &PanoRef, view: View, adjust: Adjust, aspect: f32) {
        ui.label("Risoluzione");
        theme::combo(ui, "risoluzione", &mut self.profile.resolution, RESOLUTIONS);
        let size = output_size(LONG_EDGES[self.profile.resolution], aspect);
        theme::muted(ui, format!("{} × {} px", size.0, size.1));
        if self.profile.resolution > 1 {
            ui.colored_label(theme::ACCENT, "L’esportazione richiederà più tempo.");
        }
        ui.add_space(6.0);
        ui.label("Formato");
        theme::combo(ui, "formato", &mut self.profile.format, FORMATS);
        ui.checkbox(&mut self.profile.watermark, "Filigrana");
        ui.add_space(6.0);
        ui.label("Cartella di destinazione");
        ui.add(egui::TextEdit::singleline(&mut self.directory).desired_width(f32::INFINITY));
        if ui
            .add_enabled(self.dialog.is_none(), egui::Button::new("Sfoglia…"))
            .clicked()
        {
            self.dialog = Some(Job::start(ui.ctx(), || {
                rfd::FileDialog::new().pick_folder()
            }));
        }
        ui.add_space(10.0);
        if ui
            .add_enabled(
                self.running.is_none() && !self.directory.trim().is_empty(),
                theme::primary("Esporta"),
            )
            .clicked()
        {
            let destination = PathBuf::from(self.directory.trim());
            let source = Arc::clone(pano);
            let rows = Arc::new(AtomicUsize::new(0));
            let progress = Arc::clone(&rows);
            let jpg = self.profile.format == 1;
            let watermark = self.profile.watermark;
            let result = Job::start(ui.ctx(), move || {
                let mut rgb = panorama::render(
                    &source,
                    &view,
                    &adjust,
                    size,
                    Region::crop(aspect),
                    Some(&progress),
                );
                if watermark {
                    draw_watermark(&mut rgb, size);
                }
                save(&destination, &rgb, size, jpg)
            });
            self.running = Some(RenderJob {
                result: Some(result),
                rows,
                height: size.1,
            });
            self.result = None;
        }
        if let Some(job) = &self.running {
            let fraction = job.rows.load(Ordering::Relaxed) as f32 / job.height as f32;
            ui.add(
                egui::ProgressBar::new(fraction * 0.94)
                    .fill(theme::ACCENT)
                    .text(if fraction >= 1.0 {
                        "Salvataggio…"
                    } else {
                        "Rendering…"
                    }),
            );
            ui.ctx().request_repaint_after(Duration::from_millis(75));
        }
        match &self.result {
            Some(Ok(path)) => {
                ui.colored_label(theme::OK, "Esportazione completata");
                ui.add(egui::Label::new(path.to_string_lossy()).wrap());
            }
            Some(Err(error)) => {
                ui.colored_label(theme::ERROR, error);
            }
            None => {}
        }
    }
}

fn output_size(long: usize, aspect: f32) -> (usize, usize) {
    if aspect >= 1.0 {
        (long, (long as f32 / aspect).round() as usize)
    } else {
        ((long as f32 * aspect).round() as usize, long)
    }
}

fn save(
    directory: &Path,
    rgb: &[u8],
    (w, h): (usize, usize),
    jpg: bool,
) -> Result<PathBuf, String> {
    fs::create_dir_all(directory).map_err(|e| format!("Impossibile creare la cartella: {e}"))?;
    let (path, file) = create_file(directory, if jpg { "jpg" } else { "png" })
        .map_err(|e| format!("Impossibile creare il file: {e}"))?;
    let mut writer = BufWriter::new(file);
    let encoded = if jpg {
        JpegEncoder::new_with_quality(&mut writer, 92).encode(
            rgb,
            w as u32,
            h as u32,
            ExtendedColorType::Rgb8,
        )
    } else {
        PngEncoder::new(&mut writer).write_image(rgb, w as u32, h as u32, ExtendedColorType::Rgb8)
    };
    let result = encoded
        .map_err(|e| e.to_string())
        .and_then(|()| writer.flush().map_err(|e| e.to_string()));
    drop(writer);
    if let Err(error) = result {
        let cleanup = fs::remove_file(&path);
        return Err(match cleanup {
            Ok(()) => format!("Impossibile salvare l’immagine: {error}"),
            Err(e) => format!(
                "Impossibile salvare l’immagine: {error}. File parziale: {} ({e})",
                path.display()
            ),
        });
    }
    Ok(path)
}

fn create_file(directory: &Path, extension: &str) -> std::io::Result<(PathBuf, File)> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    for index in 0..100 {
        let path = directory.join(format!("Studio360-{timestamp}-{index:02}.{extension}"));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "Nessun nome disponibile",
    ))
}

fn draw_watermark(rgb: &mut [u8], (w, h): (usize, usize)) {
    const GLYPHS: [[u8; 7]; 10] = [
        [15, 16, 16, 14, 1, 1, 30],
        [31, 4, 4, 4, 4, 4, 4],
        [17, 17, 17, 17, 17, 17, 14],
        [30, 17, 17, 17, 17, 17, 30],
        [31, 4, 4, 4, 4, 4, 31],
        [14, 17, 17, 17, 17, 17, 14],
        [0, 0, 0, 0, 0, 0, 0],
        [30, 1, 1, 14, 1, 1, 30],
        [14, 16, 16, 30, 17, 17, 14],
        [14, 17, 19, 21, 25, 17, 14],
    ];
    let scale = (w.max(h) / 780).max(1);
    let margin = 12 * scale;
    let width = GLYPHS.len() * 6 * scale + 7 * scale;
    let height = 15 * scale;
    let left = w - margin - width;
    let top = h - margin - height;
    for y in top..top + height {
        for x in left..left + width {
            let pixel = (y * w + x) * 3;
            for channel in &mut rgb[pixel..pixel + 3] {
                *channel /= 3;
            }
        }
    }
    for (glyph, rows) in GLYPHS.iter().enumerate() {
        for (row, bits) in rows.iter().enumerate() {
            for col in 0..5 {
                if bits & (1 << (4 - col)) == 0 {
                    continue;
                }
                let x = left + (4 + glyph * 6 + col) * scale;
                let y = top + (4 + row) * scale;
                for dy in 0..scale {
                    let offset = ((y + dy) * w + x) * 3;
                    rgb[offset..offset + scale * 3].fill(245);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exports_a_watermarked_portrait_to_4k_jpeg_and_720p_png() {
        let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("export-test");
        let pano = panorama::generate_sample(0, 320);
        let aspect = 9.0 / 16.0;
        for (long_edge, jpg, expected) in [
            (LONG_EDGES[3], true, (2160, 3840)),
            (LONG_EDGES[0], false, (720, 1280)),
        ] {
            let size = output_size(long_edge, aspect);
            assert_eq!(size, expected);
            let rows = AtomicUsize::new(0);
            let mut rgb = panorama::render(
                &pano,
                &View::default(),
                &Adjust::default(),
                size,
                Region::crop(aspect),
                Some(&rows),
            );
            assert_eq!(rows.load(Ordering::Relaxed), size.1);
            let plain = rgb.clone();
            draw_watermark(&mut rgb, size);
            assert_ne!(plain, rgb);
            let path = save(&directory, &rgb, size, jpg).expect("scrittura dell’immagine di test");
            let image = image::open(&path).expect("decodifica dell’immagine esportata");
            fs::remove_file(path).expect("rimozione dell’immagine di test");
            assert_eq!((image.width() as usize, image.height() as usize), expected);
        }
    }
}
