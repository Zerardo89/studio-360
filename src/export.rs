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
    online,
    overlay::{self, Overlays},
    panorama::{self, Adjust, Pano, PanoRef, Region, View},
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
    result: Option<Job<Report>>,
    rows: Arc<AtomicUsize>,
    height: usize,
    batch: bool,
}

enum Views {
    Single(View),
    Selected(Vec<View>),
}

impl Views {
    fn len(&self) -> usize {
        match self {
            Self::Single(_) => 1,
            Self::Selected(views) => views.len(),
        }
    }
}

struct Report {
    batch: bool,
    files: Vec<Result<PathBuf, String>>,
    warnings: Vec<String>,
}

struct Output {
    destination: PathBuf,
    size: (usize, usize),
    region: Region,
    jpg: bool,
    watermark: bool,
    overlays: Overlays,
    overlay_time: f64,
}

impl Output {
    fn render(&self, pano: &Pano, views: Views, adjust: Adjust, rows: &AtomicUsize) -> Report {
        let batch = matches!(views, Views::Selected(_));
        let views = match views {
            Views::Single(view) => vec![view],
            Views::Selected(views) => views,
        };
        let timestamp = timestamp();
        let mut warnings = Vec::new();
        let files = views
            .into_iter()
            .enumerate()
            .map(|(index, view)| {
                let mut rgb =
                    panorama::render(pano, &view, &adjust, self.size, self.region, Some(rows));
                overlay::composite(&mut rgb, self.size, 3, &self.overlays, self.overlay_time);
                if self.watermark {
                    draw_watermark(&mut rgb, self.size);
                }
                let result = save(
                    &self.destination,
                    &rgb,
                    self.size,
                    self.jpg,
                    batch.then_some((timestamp, index + 1)),
                );
                if let Ok(path) = &result {
                    if self.overlays.layers.iter().any(|layer| {
                        layer.is_sticker() && self.overlays.visible(layer, self.overlay_time)
                    }) {
                        if let Err(error) = online::write_credits(path, online::TWEMOJI_CREDITS) {
                            warnings.push(error);
                        }
                    }
                }
                result
            })
            .collect();
        Report {
            batch,
            files,
            warnings,
        }
    }
}

pub struct Export {
    profile: Profile,
    directory: String,
    dialog: Option<Job<Option<PathBuf>>>,
    running: Option<RenderJob>,
    result: Option<Report>,
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

    pub fn watermark(&self) -> bool {
        self.profile.watermark
    }

    pub fn poll(&mut self) {
        if let Some(Some(folder)) = Job::take(&mut self.dialog) {
            self.directory = folder.to_string_lossy().into_owned();
        }
        if let Some(job) = &mut self.running {
            if let Some(report) = Job::take(&mut job.result) {
                self.result = Some(report);
            }
            if job.result.is_none() {
                if self.result.is_none() {
                    self.result = Some(Report {
                        batch: job.batch,
                        files: vec![Err("Esportazione interrotta.".into())],
                        warnings: Vec::new(),
                    });
                }
                self.running = None;
            }
        }
    }

    pub fn can_start(&self) -> bool {
        self.running.is_none() && !self.directory.trim().is_empty()
    }

    pub fn start_selected(
        &mut self,
        ctx: &egui::Context,
        pano: &PanoRef,
        views: Vec<View>,
        adjust: Adjust,
        aspect: f32,
        overlays: (&Overlays, f64),
    ) {
        self.start(ctx, pano, Views::Selected(views), adjust, aspect, overlays);
    }

    fn start(
        &mut self,
        ctx: &egui::Context,
        pano: &PanoRef,
        views: Views,
        adjust: Adjust,
        aspect: f32,
        overlays: (&Overlays, f64),
    ) {
        if !self.can_start() || views.len() == 0 {
            return;
        }
        let output = Output {
            destination: PathBuf::from(self.directory.trim()),
            size: output_size(LONG_EDGES[self.profile.resolution], aspect),
            region: Region::crop(aspect),
            jpg: self.profile.format == 1,
            watermark: self.profile.watermark,
            overlays: overlays.0.clone(),
            overlay_time: overlays.1,
        };
        let batch = matches!(views, Views::Selected(_));
        let height = output.size.1 * views.len();
        let source = Arc::clone(pano);
        let rows = Arc::new(AtomicUsize::new(0));
        let progress = Arc::clone(&rows);
        let result = Job::start(ctx, move || {
            output.render(&source, views, adjust, &progress)
        });
        self.running = Some(RenderJob {
            result: Some(result),
            rows,
            height,
            batch,
        });
        self.result = None;
    }

    pub fn show(
        &mut self,
        ui: &mut Ui,
        pano: &PanoRef,
        view: View,
        adjust: Adjust,
        aspect: f32,
        overlays: (&Overlays, f64),
    ) {
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
            .add_enabled(self.can_start(), theme::primary("Esporta"))
            .clicked()
        {
            self.start(
                ui.ctx(),
                pano,
                Views::Single(view),
                adjust,
                aspect,
                overlays,
            );
        }
        self.show_status(ui, false);
    }

    pub fn show_batch_status(&self, ui: &mut Ui) {
        self.show_status(ui, true);
    }

    fn show_status(&self, ui: &mut Ui, batch_only: bool) {
        if let Some(job) = self.running.as_ref().filter(|job| !batch_only || job.batch) {
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
        let Some(report) = self
            .result
            .as_ref()
            .filter(|report| !batch_only || report.batch)
        else {
            return;
        };
        for warning in &report.warnings {
            ui.colored_label(theme::ERROR, warning);
        }
        if !report.batch {
            match report.files.first() {
                Some(Ok(path)) => {
                    ui.colored_label(theme::OK, "Esportazione completata");
                    ui.add(egui::Label::new(path.to_string_lossy()).wrap());
                }
                Some(Err(error)) => {
                    ui.colored_label(theme::ERROR, error);
                }
                None => {}
            }
            return;
        }
        let saved = report.files.iter().filter_map(|file| file.as_ref().ok());
        if let Some(folder) = saved.clone().next().and_then(|file| file.parent()) {
            ui.colored_label(theme::OK, format!("Clip esportate: {}", saved.count()));
            ui.add(egui::Label::new(folder.to_string_lossy()).wrap());
        }
        for (index, error) in report
            .files
            .iter()
            .enumerate()
            .filter_map(|(i, file)| file.as_ref().err().map(|error| (i + 1, error)))
        {
            ui.colored_label(theme::ERROR, format!("Clip {index} · {error}"));
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
    batch: Option<(u128, usize)>,
) -> Result<PathBuf, String> {
    fs::create_dir_all(directory).map_err(|e| format!("Impossibile creare la cartella: {e}"))?;
    let (path, file) = create_file(directory, if jpg { "jpg" } else { "png" }, batch)
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

fn timestamp() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn create_file(
    directory: &Path,
    extension: &str,
    batch: Option<(u128, usize)>,
) -> std::io::Result<(PathBuf, File)> {
    let timestamp = batch.map_or_else(timestamp, |(timestamp, _)| timestamp);
    for index in 0..100 {
        let name = match batch {
            Some((_, clip)) if index == 0 => {
                format!("Studio360-{timestamp}-AutoEdit-{clip:02}.{extension}")
            }
            Some((_, clip)) => {
                format!("Studio360-{timestamp}-AutoEdit-{index:02}-{clip:02}.{extension}")
            }
            None => format!("Studio360-{timestamp}-{index:02}.{extension}"),
        };
        let path = directory.join(name);
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
    paint_watermark(rgb, (w, h), 3);
}

pub(crate) fn draw_watermark_rgba(rgba: &mut image::RgbaImage) {
    let (w, h) = rgba.dimensions();
    paint_watermark(rgba, (w as usize, h as usize), 4);
}

fn paint_watermark(bytes: &mut [u8], (w, h): (usize, usize), channels: usize) {
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
    if w < margin + width || h < margin + height {
        return;
    }
    let left = w - margin - width;
    let top = h - margin - height;
    for y in top..top + height {
        for x in left..left + width {
            let pixel = (y * w + x) * channels;
            for channel in &mut bytes[pixel..pixel + 3] {
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
                    let offset = ((y + dy) * w + x) * channels;
                    for pixel in bytes[offset..offset + scale * channels].chunks_exact_mut(channels)
                    {
                        pixel[..3].fill(245);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exports_selected_views_with_progressive_suffixes() {
        let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("export-test");
        let pano = panorama::generate_sample(0, 320);
        let output = Output {
            destination: directory,
            size: (128, 72),
            region: Region::FULL,
            jpg: false,
            watermark: false,
            overlays: Overlays {
                layers: vec![overlay::Layer::sticker(
                    "Test",
                    image::RgbaImage::from_pixel(1, 1, image::Rgba([255, 1, 255, 255])),
                )],
                show_stickers: true,
            },
            overlay_time: 0.0,
        };
        let views = vec![
            View::default(),
            View {
                yaw: 120.0,
                ..View::default()
            },
        ];
        let rows = AtomicUsize::new(0);
        let result = output.render(&pano, Views::Selected(views), Adjust::default(), &rows);
        assert!(result.batch);
        assert_eq!(result.files.len(), 2);
        assert!(result.warnings.is_empty());
        assert_eq!(rows.load(Ordering::Relaxed), 144);
        let mut images = Vec::new();
        for (index, file) in result.files.into_iter().enumerate() {
            let file = file.expect("clip salvata");
            let expected = format!("-{:02}.png", index + 1);
            assert!(file.to_string_lossy().ends_with(&expected));
            let image = image::open(&file).expect("clip leggibile").into_rgb8();
            assert_eq!(image.get_pixel(64, 36).0, [255, 1, 255]);
            images.push(image);
            let credits = file.with_extension("crediti.txt");
            assert_eq!(
                fs::read_to_string(&credits).expect("crediti leggibili"),
                online::TWEMOJI_CREDITS
            );
            fs::remove_file(credits).expect("rimozione dei crediti di test");
            fs::remove_file(file).expect("rimozione della clip di test");
        }
        assert_ne!(images[0], images[1]);
    }

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
            let path =
                save(&directory, &rgb, size, jpg, None).expect("scrittura dell’immagine di test");
            let image = image::open(&path).expect("decodifica dell’immagine esportata");
            fs::remove_file(path).expect("rimozione dell’immagine di test");
            assert_eq!((image.width() as usize, image.height() as usize), expected);
        }
    }
}
