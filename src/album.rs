use std::{
    collections::{HashMap, HashSet},
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

use eframe::egui::{self, Color32, RichText, Stroke, StrokeKind, Ui};
use image::{DynamicImage, ImageFormat, ImageReader, RgbImage};

use crate::{
    app::Job,
    panorama::{Pano, PanoRef},
    playback::{self, file_name, PanoramaVideo, EDITOR_MAX_WIDTH},
    theme,
};

const THUMBNAIL: u32 = 160;
const THUMBNAIL_SECONDS: f64 = 1.0;
const TYPES: &[&str] = &[
    "Tutti",
    "Foto",
    "Video",
    "360",
    "Piatte",
    "Preferiti",
    "Foto HDR",
    "Video HDR",
    "Timelapse",
    "TimeShift",
    "Bullet Time",
    "Raffica",
    "Intervallo",
    "Registrazione in loop",
    "Rallentatore",
    "Starlapse",
    "Scatto notturno",
    "Pro Video",
    "PureVideo",
];
const SOURCES: &[&str] = &[
    "Tutte",
    "Fotocamera",
    "Memoria interna",
    "Scheda SD",
    "Locale",
    "Esempi",
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Camera,
    Local,
    Examples,
}

impl Source {
    fn index(self) -> usize {
        match self {
            Self::Camera => 1,
            Self::Local => 4,
            Self::Examples => 5,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub enum Asset {
    Example(usize),
    File(PathBuf),
}

impl Asset {
    pub fn load_photo(&self, examples: &[PanoRef], id: u64) -> Result<PanoRef, String> {
        match self {
            Self::Example(index) => examples
                .get(*index)
                .cloned()
                .ok_or("Panorama di esempio mancante.".into()),
            Self::File(path) => {
                let (image, is_panorama) = decode_photo(path, EDITOR_MAX_WIDTH)?;
                let (w, h) = image.dimensions();
                let mut pano = Pano::from_rgb(
                    id,
                    file_name(path),
                    w as usize,
                    h as usize,
                    image.into_raw(),
                );
                pano.is_panorama = is_panorama;
                Ok(Arc::new(pano))
            }
        }
    }
}

struct Media {
    asset: Asset,
    source: Source,
    name: String,
    modified: Option<SystemTime>,
    is_panorama: bool,
    thumbnail: Option<egui::TextureHandle>,
    video: bool,
    duration: Option<f64>,
    thumbnail_error: Option<String>,
}

struct Scanned {
    path: PathBuf,
    modified: Option<SystemTime>,
    video: bool,
    thumbnail: Thumbnail,
}

#[derive(Clone)]
struct Thumbnail {
    is_panorama: bool,
    rgb: Option<Arc<RgbImage>>,
    duration: Option<f64>,
    error: Option<String>,
}

#[derive(PartialEq, Eq)]
struct FileStamp(SystemTime, u64);

struct CachedThumbnail {
    stamp: FileStamp,
    second: Option<FileStamp>,
    thumbnail: Thumbnail,
}

type ThumbnailCache = HashMap<PathBuf, CachedThumbnail>;

struct Scan {
    files: Vec<Scanned>,
    skipped: usize,
    cache: ThumbnailCache,
}

type ScanJob = Job<Result<Scan, String>>;

pub struct Album {
    media: Vec<Media>,
    favorite: HashSet<Asset>,
    filter: usize,
    source: usize,
    path_text: String,
    folders: [Option<PathBuf>; 2],
    scans: [Option<ScanJob>; 2],
    thumbnails: [Arc<ThumbnailCache>; 2],
    scan_feedback: [Option<String>; 2],
    dialog: Option<Job<Option<PathBuf>>>,
    restore_cache: bool,
}

impl Album {
    pub fn new() -> Self {
        Self {
            media: Vec::new(),
            favorite: HashSet::new(),
            filter: 0,
            source: 0,
            path_text: String::new(),
            folders: [None, None],
            scans: [None, None],
            thumbnails: Default::default(),
            scan_feedback: [None, None],
            dialog: None,
            restore_cache: false,
        }
    }

    pub fn add_examples(&mut self, ctx: &egui::Context, examples: &[PanoRef]) {
        for (index, pano) in examples.iter().enumerate() {
            self.media.push(Media {
                asset: Asset::Example(index),
                source: Source::Examples,
                name: pano.name.clone(),
                modified: None,
                is_panorama: true,
                thumbnail: Some(example_thumb(ctx, pano)),
                video: false,
                duration: None,
                thumbnail_error: None,
            });
        }
    }

    pub fn open_folder(&mut self, ctx: &egui::Context, path: PathBuf, source: Source) {
        self.path_text = path.to_string_lossy().into_owned();
        self.source = source.index();
        self.filter = 0;
        self.scan(ctx, path, source);
    }

    fn scan(&mut self, ctx: &egui::Context, path: PathBuf, source: Source) {
        let slot = usize::from(source == Source::Local);
        self.scan_feedback[slot] = None;
        self.folders[slot] = Some(path.clone());
        self.media.retain(|item| item.source != source);
        let cache = Arc::clone(&self.thumbnails[slot]);
        self.scans[slot] = Some(Job::start(ctx, move || {
            scan_folder(&path, source == Source::Camera, &cache)
        }));
    }

    pub fn poll(&mut self, ctx: &egui::Context) {
        if let Some(Some(path)) = Job::take(&mut self.dialog) {
            self.open_folder(ctx, path, Source::Local);
        }
        for (slot, source) in [Source::Camera, Source::Local].into_iter().enumerate() {
            let Some(result) = Job::take(&mut self.scans[slot]) else {
                continue;
            };
            match result {
                Ok(scan) => {
                    self.thumbnails[slot] = Arc::new(scan.cache);
                    self.scan_feedback[slot] = (scan.skipped > 0).then(|| {
                        format!(
                            "Impossibile leggere {} elementi nella cartella.",
                            scan.skipped
                        )
                    });
                    self.media.extend(scan.files.into_iter().map(|file| {
                        let name = file_name(&file.path);
                        Media {
                            thumbnail: file
                                .thumbnail
                                .rgb
                                .as_deref()
                                .map(|rgb| upload(ctx, &name, rgb)),
                            name,
                            modified: file.modified,
                            source,
                            asset: Asset::File(file.path),
                            is_panorama: file.thumbnail.is_panorama,
                            video: file.video,
                            duration: file.thumbnail.duration,
                            thumbnail_error: file.thumbnail.error,
                        }
                    }));
                    self.media.sort_by_key(|item| {
                        (
                            item.source == Source::Examples,
                            std::cmp::Reverse(item.modified),
                        )
                    });
                }
                Err(message) => self.scan_feedback[slot] = Some(message),
            }
        }
    }

    pub fn clear_cache(&mut self) {
        for item in &mut self.media {
            item.thumbnail = None;
        }
        self.scans = [None, None];
        self.thumbnails = Default::default();
        self.restore_cache = true;
    }

    pub fn ensure_cache(&mut self, ctx: &egui::Context, examples: &[PanoRef]) {
        if !std::mem::take(&mut self.restore_cache) {
            return;
        }
        for item in &mut self.media {
            if let Asset::Example(index) = item.asset {
                item.thumbnail = examples.get(index).map(|pano| example_thumb(ctx, pano));
            }
        }
        for (slot, source) in [Source::Camera, Source::Local].into_iter().enumerate() {
            if let Some(path) = self.folders[slot].clone() {
                self.scan(ctx, path, source);
            }
        }
    }

    pub fn latest(&self) -> Option<Asset> {
        self.media.first().map(|item| item.asset.clone())
    }

    pub fn recent(&mut self, ui: &mut Ui) -> Option<Asset> {
        let indexes: Vec<_> = (0..self.media.len().min(4)).collect();
        if indexes.is_empty() {
            theme::muted(ui, "Le foto e i video dell’album compariranno qui.");
        }
        self.grid(ui, &indexes, false)
    }

    pub fn show(&mut self, ui: &mut Ui) -> Option<Asset> {
        theme::title(
            ui,
            "Album",
            "Sfoglia foto e video e scegli quale aprire nell’editor.",
        );
        ui.horizontal(|ui| {
            let field = ui.add_sized(
                [ui.available_width().min(530.0) - 105.0, 32.0],
                egui::TextEdit::singleline(&mut self.path_text)
                    .hint_text("Percorso di una cartella, poi Invio")
                    .desired_width(f32::INFINITY),
            );
            let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if enter && !self.path_text.trim().is_empty() {
                self.open_folder(
                    ui.ctx(),
                    PathBuf::from(self.path_text.trim()),
                    Source::Local,
                );
            }
            if ui
                .add_enabled(self.dialog.is_none(), egui::Button::new("Sfoglia…"))
                .clicked()
            {
                self.dialog = Some(Job::start(ui.ctx(), || {
                    rfd::FileDialog::new().pick_folder()
                }));
            }
        });
        ui.horizontal(|ui| {
            ui.label("Sorgente");
            theme::combo(ui, "sorgente_album", &mut self.source, SOURCES);
        });
        ui.add_space(6.0);
        theme::horizontal_scroll(ui, "tipi_album", |ui| {
            for (index, label) in TYPES.iter().enumerate() {
                if theme::chip(ui, label, self.filter == index).clicked() {
                    self.filter = index;
                }
            }
        });
        ui.add_space(8.0);
        ui.separator();
        for slot in 0..2 {
            let source = if slot == 0 {
                Source::Camera
            } else {
                Source::Local
            };
            if self.source == 0 || self.source == source.index() {
                if self.scans[slot].is_some() {
                    theme::muted(
                        ui,
                        format!("{} · Caricamento miniature…", SOURCES[source.index()]),
                    );
                }
                if let Some(message) = &self.scan_feedback[slot] {
                    ui.colored_label(theme::ERROR, message);
                }
            }
        }
        let indexes: Vec<_> = self
            .media
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                (self.source == 0 || self.source == item.source.index())
                    && match self.filter {
                        0 => true,
                        1 => !item.video,
                        2 => item.video,
                        3 => item.is_panorama,
                        4 => !item.is_panorama,
                        5 => self.favorite.contains(&item.asset),
                        _ => false,
                    }
            })
            .map(|(index, _)| index)
            .collect();
        theme::muted(
            ui,
            if indexes.len() == 1 {
                "1 elemento".into()
            } else {
                format!("{} elementi", indexes.len())
            },
        );
        if indexes.is_empty() {
            if !self.scans.iter().any(Option::is_some) {
                theme::muted(ui, "Cambia filtro o scegli una cartella con foto o video.");
            }
            return None;
        }
        egui::ScrollArea::vertical()
            .id_salt("griglia_album")
            .show(ui, |ui| self.grid(ui, &indexes, true))
            .inner
    }

    fn grid(&mut self, ui: &mut Ui, indexes: &[usize], controls: bool) -> Option<Asset> {
        let spacing = ui.spacing().item_spacing.x;
        let available = ui.available_width();
        let columns = (((available + spacing) / (168.0 + spacing)) as usize)
            .clamp(1, indexes.len().clamp(1, if controls { 5 } else { 4 }));
        let width = (available - spacing * (columns - 1) as f32) / columns as f32 - 2.0;
        let mut open = None;
        for row in indexes.chunks(columns) {
            ui.horizontal_top(|ui| {
                for &index in row {
                    let item = &self.media[index];
                    ui.push_id(index, |ui| {
                        theme::card(0).show(ui, |ui| {
                            ui.vertical(|ui| {
                                ui.set_width(width);
                                let (rect, response) = ui.allocate_exact_size(
                                    egui::vec2(width, (width * 0.54).clamp(76.0, 150.0)),
                                    egui::Sense::click(),
                                );
                                if let Some(texture) = &item.thumbnail {
                                    let [w, h] = texture.size();
                                    theme::paint_image(
                                        ui,
                                        rect,
                                        texture,
                                        theme::cover_uv(w as f32 / h as f32, rect.aspect_ratio()),
                                    );
                                } else {
                                    ui.painter().rect_filled(rect, 8, theme::SIDE);
                                }
                                let response = if let Some(error) = &item.thumbnail_error {
                                    response.on_hover_text(format!("{}\n{error}", item.name))
                                } else {
                                    response.on_hover_text(&item.name)
                                };
                                if response.double_clicked() || (!controls && response.clicked()) {
                                    open = Some(item.asset.clone());
                                }
                                if item.is_panorama {
                                    let badge = egui::Rect::from_min_size(
                                        rect.min + egui::vec2(8.0, 8.0),
                                        egui::vec2(36.0, 22.0),
                                    );
                                    ui.painter().rect_filled(
                                        badge,
                                        6,
                                        Color32::from_black_alpha(185),
                                    );
                                    ui.painter().text(
                                        badge.center(),
                                        egui::Align2::CENTER_CENTER,
                                        "360",
                                        egui::FontId::proportional(12.0),
                                        Color32::WHITE,
                                    );
                                }
                                if item.video {
                                    video_badge(ui, rect, item.duration);
                                }
                                egui::Frame::new()
                                    .inner_margin(egui::Margin::symmetric(9, 6))
                                    .show(ui, |ui| {
                                        let name = ui
                                            .add(
                                                egui::Label::new(
                                                    RichText::new(&item.name).strong(),
                                                )
                                                .truncate()
                                                .sense(egui::Sense::click()),
                                            )
                                            .on_hover_text(&item.name);
                                        if name.double_clicked() || (!controls && name.clicked()) {
                                            open = Some(item.asset.clone());
                                        }
                                        if controls {
                                            ui.horizontal(|ui| {
                                                let favorite = self.favorite.contains(&item.asset);
                                                if favorite_button(ui, favorite).clicked() {
                                                    if !self.favorite.remove(&item.asset) {
                                                        self.favorite.insert(item.asset.clone());
                                                    }
                                                }
                                                if ui.button("Modifica").clicked() {
                                                    open = Some(item.asset.clone());
                                                }
                                            });
                                        }
                                    });
                            });
                        });
                    });
                }
            });
        }
        open
    }
}

fn favorite_button(ui: &mut Ui, selected: bool) -> egui::Response {
    let label = if selected {
        "Rimuovi dai preferiti"
    } else {
        "Aggiungi ai preferiti"
    };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(32.0, 30.0), egui::Sense::click());
    let response = response.on_hover_text(label);
    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, selected, label));
    let visuals = ui.style().interact(&response);
    let color = if selected || response.has_focus() {
        theme::ACCENT
    } else {
        theme::MUTED
    };
    let border = if response.has_focus() {
        Stroke::new(1.0, theme::ACCENT)
    } else {
        visuals.bg_stroke
    };
    ui.painter()
        .rect(rect, 8, visuals.weak_bg_fill, border, StrokeKind::Inside);
    let center = rect.center();
    let points = (0..=10)
        .map(|point| {
            let angle = point as f32 * std::f32::consts::PI / 5.0 - std::f32::consts::FRAC_PI_2;
            let radius = if point % 2 == 0 { 8.0 } else { 3.8 };
            center + egui::Vec2::angled(angle) * radius
        })
        .collect::<Vec<_>>();
    if selected {
        for point in points.windows(2) {
            ui.painter().add(egui::Shape::convex_polygon(
                vec![center, point[0], point[1]],
                color,
                Stroke::NONE,
            ));
        }
    }
    ui.painter()
        .add(egui::Shape::line(points, Stroke::new(1.25, color)));
    response
}

fn video_badge(ui: &Ui, rect: egui::Rect, duration: Option<f64>) {
    let duration = duration.map_or_else(|| "–:––".to_owned(), playback::format_time);
    let width = 22.0 + duration.len() as f32 * 7.0;
    let badge = egui::Rect::from_min_size(
        rect.right_bottom() - egui::vec2(width + 8.0, 30.0),
        egui::vec2(width, 22.0),
    );
    let p = badge.left_center() + egui::vec2(8.0, 0.0);
    ui.painter()
        .rect_filled(badge, 6, Color32::from_black_alpha(185));
    ui.painter().add(egui::Shape::convex_polygon(
        vec![
            p - egui::vec2(0.0, 4.0),
            p + egui::vec2(7.0, 0.0),
            p + egui::vec2(0.0, 4.0),
        ],
        Color32::WHITE,
        Stroke::NONE,
    ));
    ui.painter().text(
        badge.left_center() + egui::vec2(19.0, 0.0),
        egui::Align2::LEFT_CENTER,
        duration,
        egui::FontId::monospace(12.0),
        Color32::WHITE,
    );
}

fn decode_photo(path: &Path, max_width: u32) -> Result<(RgbImage, bool), String> {
    let mut reader = ImageReader::open(path).map_err(|e| format!("{}: {e}", file_name(path)))?;
    let is_insp = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("insp"));
    if is_insp {
        reader.set_format(ImageFormat::Jpeg);
    }
    let image = reader
        .decode()
        .map_err(|e| format!("{}: {e}", file_name(path)))?;
    Ok(playback::panorama_photo(
        image.into_rgb8(),
        is_insp,
        max_width,
    ))
}

fn video_thumbnail(path: &Path) -> Thumbnail {
    let mut thumbnail = Thumbnail {
        is_panorama: crate::video::is_insv(path),
        rgb: None,
        duration: None,
        error: None,
    };
    let result = (|| {
        let mut video = PanoramaVideo::open(path, THUMBNAIL)?;
        thumbnail.is_panorama = video.is_panorama();
        thumbnail.duration = (video.duration > 0.0).then_some(video.duration);
        let frame = if video.duration > 0.0 {
            match video.frame_at(THUMBNAIL_SECONDS) {
                Ok(Some(frame)) => Some(frame),
                Ok(None) | Err(_) => PanoramaVideo::open(path, THUMBNAIL)?.next_frame()?,
            }
        } else {
            video.next_frame()?
        };
        let (_, rgb) = frame.ok_or("Il video non contiene fotogrammi leggibili.")?;
        Ok(Arc::new(
            DynamicImage::ImageRgb8(rgb)
                .thumbnail(THUMBNAIL, THUMBNAIL)
                .into_rgb8(),
        ))
    })();
    match result {
        Ok(rgb) => thumbnail.rgb = Some(rgb),
        Err(error) => thumbnail.error = Some(error),
    }
    thumbnail
}

fn photo_thumbnail(path: &Path) -> Result<Thumbnail, String> {
    let (rgb, is_panorama) = decode_photo(path, THUMBNAIL)?;
    Ok(Thumbnail {
        is_panorama,
        duration: None,
        error: None,
        rgb: Some(Arc::new(
            DynamicImage::ImageRgb8(rgb)
                .thumbnail(THUMBNAIL, THUMBNAIL)
                .into_rgb8(),
        )),
    })
}

fn stamp(metadata: &fs::Metadata) -> Option<FileStamp> {
    Some(FileStamp(metadata.modified().ok()?, metadata.len()))
}

fn scan_folder(path: &Path, recursive: bool, previous: &ThumbnailCache) -> Result<Scan, String> {
    let entries =
        fs::read_dir(path).map_err(|e| format!("Impossibile aprire {}: {e}", path.display()))?;
    let mut directories = vec![entries];
    let mut files = Vec::new();
    let mut skipped = 0;
    let mut cache = HashMap::new();
    while let Some(entries) = directories.pop() {
        for entry in entries {
            let result = (|| -> io::Result<()> {
                let entry = entry?;
                let path = entry.path();
                let kind = entry.file_type()?;
                if recursive && kind.is_dir() && !is_reparse_point(&entry)? {
                    directories.push(fs::read_dir(path)?);
                    return Ok(());
                }
                if !kind.is_file() {
                    return Ok(());
                }
                let video = playback::is_video(&path);
                if !video
                    && !path.extension().is_some_and(|ext| {
                        ["jpg", "jpeg", "png", "insp"]
                            .iter()
                            .any(|valid| ext.eq_ignore_ascii_case(valid))
                    })
                {
                    return Ok(());
                }
                if playback::primary_lens(&path).is_some() {
                    return Ok(());
                }
                let current = entry.metadata().ok().as_ref().and_then(stamp);
                let second = playback::secondary_lens(&path);
                let second_stamp = second
                    .as_ref()
                    .and_then(|path| path.metadata().ok())
                    .as_ref()
                    .and_then(stamp);
                let cacheable = current.is_some() && (second.is_none() || second_stamp.is_some());
                let cached = previous.get(&path).filter(|cached| {
                    cacheable
                        && current.as_ref() == Some(&cached.stamp)
                        && second_stamp == cached.second
                });
                let thumbnail = if let Some(cached) = cached {
                    cached.thumbnail.clone()
                } else if video {
                    video_thumbnail(&path)
                } else {
                    match photo_thumbnail(&path) {
                        Ok(thumbnail) => thumbnail,
                        Err(_) => {
                            skipped += 1;
                            return Ok(());
                        }
                    }
                };
                let modified = current.as_ref().map(|stamp| stamp.0);
                if let Some(stamp) = current.filter(|_| cacheable && thumbnail.error.is_none()) {
                    cache.insert(
                        path.clone(),
                        CachedThumbnail {
                            stamp,
                            second: second_stamp,
                            thumbnail: thumbnail.clone(),
                        },
                    );
                }
                files.push(Scanned {
                    path,
                    modified,
                    video,
                    thumbnail,
                });
                Ok(())
            })();
            if result.is_err() {
                skipped += 1;
            }
        }
    }
    files.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    Ok(Scan {
        files,
        skipped,
        cache,
    })
}

#[cfg(test)]
pub(super) fn check_video_thumbnails(directory: &Path) -> Result<(), String> {
    let initial = scan_folder(directory, false, &HashMap::new())?;
    let cached = scan_folder(directory, false, &initial.cache)?;
    assert_eq!(initial.skipped, 0);
    assert_eq!(initial.files.len(), 2);
    let pair = initial
        .files
        .iter()
        .find(|file| crate::video::is_insv(&file.path))
        .ok_or("La coppia INSV non appare nell’album")?;
    assert!(pair.path.ends_with("VID_demo_00_001.insv"));
    for (original, repeated) in initial.files.iter().zip(&cached.files) {
        assert!(original.video);
        assert!(original.thumbnail.is_panorama);
        assert_eq!(
            original
                .thumbnail
                .duration
                .map(playback::format_time)
                .as_deref(),
            Some("0:02")
        );
        let thumb = original
            .thumbnail
            .rgb
            .as_ref()
            .ok_or_else(|| original.thumbnail.error.clone().unwrap_or_default())?;
        assert_eq!(thumb.dimensions(), (160, 80));
        assert!(repeated
            .thumbnail
            .rgb
            .as_ref()
            .is_some_and(|rgb| Arc::ptr_eq(rgb, thumb)));
    }
    Ok(())
}

#[cfg(windows)]
fn is_reparse_point(entry: &fs::DirEntry) -> io::Result<bool> {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    Ok(entry.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
}

#[cfg(not(windows))]
fn is_reparse_point(entry: &fs::DirEntry) -> io::Result<bool> {
    Ok(entry.file_type()?.is_symlink())
}

fn upload(ctx: &egui::Context, name: &str, rgb: &RgbImage) -> egui::TextureHandle {
    let image =
        egui::ColorImage::from_rgb([rgb.width() as usize, rgb.height() as usize], rgb.as_raw());
    ctx.load_texture(name, image, egui::TextureOptions::LINEAR)
}

fn example_thumb(ctx: &egui::Context, pano: &Pano) -> egui::TextureHandle {
    let width = THUMBNAIL as usize;
    let height = width / 2;
    let pixels = (0..height)
        .flat_map(|y| {
            (0..width).flat_map(move |x| {
                let offset = ((y * pano.h / height) * pano.w + x * pano.w / width) * 3;
                [pano.rgb[offset], pano.rgb[offset + 1], pano.rgb[offset + 2]]
            })
        })
        .collect::<Vec<_>>();
    let image = egui::ColorImage::from_rgb([width, height], &pixels);
    ctx.load_texture(&pano.name, image, egui::TextureOptions::LINEAR)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_an_insp_jpeg_and_places_the_two_lenses_on_opposite_sides(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let path = Path::new("target").join(format!("studio360-photo-{}.insp", std::process::id()));
        let packed = RgbImage::from_fn(128, 64, |x, y| {
            let radius = (x % 64).abs_diff(32).pow(2) + y.abs_diff(32).pow(2);
            image::Rgb(if radius >= 32 * 32 {
                [0, 0, 0]
            } else if x < 64 {
                [210, 90, 45]
            } else {
                [45, 90, 210]
            })
        });
        packed.save_with_format(&path, ImageFormat::Jpeg)?;
        let photo = Asset::File(path.clone()).load_photo(&[], 73);
        fs::remove_file(&path)?;
        let photo = photo?;
        assert!(photo.is_panorama);
        assert_eq!((photo.w, photo.h), (128, 64));
        let front = (32 * photo.w + 64) * 3;
        let back = 32 * photo.w * 3;
        assert!(photo.rgb[front] > 200 && photo.rgb[front + 2] < 60);
        assert!(photo.rgb[back] < 60 && photo.rgb[back + 2] > 200);
        Ok(())
    }
}
