use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

use eframe::egui::{self, Color32, RichText, Stroke, StrokeKind, Ui};
use image::{imageops::FilterType, GenericImageView, ImageReader, RgbImage};

use crate::{
    app::Job,
    panorama::{Pano, PanoRef},
    theme,
};

const THUMBNAIL: u32 = 160;
const PANORAMA_ASPECT: f32 = 2.0;
const EDITOR_MAX_WIDTH: u32 = 4096;
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
    pub fn load(&self, examples: &[PanoRef], id: u64) -> Result<PanoRef, String> {
        match self {
            Self::Example(index) => Ok(Arc::clone(&examples[*index])),
            Self::File(path) => {
                let image = decode(path)?;
                let (w, h) = image.dimensions();
                let is_panorama = is_panorama(w, h);
                let image = if w > EDITOR_MAX_WIDTH {
                    let h = (u64::from(h) * u64::from(EDITOR_MAX_WIDTH) / u64::from(w)).max(1);
                    image.resize_exact(EDITOR_MAX_WIDTH, h as u32, FilterType::Triangle)
                } else {
                    image
                };
                let (w, h) = image.dimensions();
                let mut pano = Pano::from_rgb(
                    id,
                    file_name(path),
                    w as usize,
                    h as usize,
                    image.into_rgb8().into_raw(),
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
}

struct Scanned {
    path: PathBuf,
    modified: Option<SystemTime>,
    is_panorama: bool,
    thumbnail: RgbImage,
}

struct Scan {
    files: Vec<Scanned>,
    skipped: usize,
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
        self.scans[slot] = Some(Job::start(ctx, move || {
            scan_folder(&path, source == Source::Camera)
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
                    self.scan_feedback[slot] = (scan.skipped > 0).then(|| {
                        format!(
                            "Impossibile leggere {} elementi nella cartella.",
                            scan.skipped
                        )
                    });
                    self.media.extend(scan.files.into_iter().map(|file| {
                        let name = file_name(&file.path);
                        Media {
                            thumbnail: Some(upload(ctx, &name, &file.thumbnail)),
                            name,
                            modified: file.modified,
                            source,
                            asset: Asset::File(file.path),
                            is_panorama: file.is_panorama,
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
            theme::muted(ui, "Le immagini aperte nell’album compariranno qui.");
        }
        self.grid(ui, &indexes, false)
    }

    pub fn show(&mut self, ui: &mut Ui) -> Option<Asset> {
        theme::title(
            ui,
            "Album",
            "Sfoglia le immagini e scegli quale aprire nell’editor.",
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
                        0 | 1 => true,
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
                "1 immagine".into()
            } else {
                format!("{} immagini", indexes.len())
            },
        );
        if indexes.is_empty() {
            if !self.scans.iter().any(Option::is_some) {
                theme::muted(
                    ui,
                    "Cambia filtro o scegli una cartella con immagini JPG o PNG.",
                );
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
                                let response = response.on_hover_text(&item.name);
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

fn is_panorama(w: u32, h: u32) -> bool {
    (w as f32 / h as f32 - PANORAMA_ASPECT).abs() <= 0.12
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

fn decode(path: &Path) -> Result<image::DynamicImage, String> {
    ImageReader::open(path)
        .map_err(|e| format!("{}: {e}", file_name(path)))?
        .decode()
        .map_err(|e| format!("{}: {e}", file_name(path)))
}

fn scan_folder(path: &Path, recursive: bool) -> Result<Scan, String> {
    let entries =
        fs::read_dir(path).map_err(|e| format!("Impossibile aprire {}: {e}", path.display()))?;
    let mut directories = vec![entries];
    let mut files = Vec::new();
    let mut skipped = 0;
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
                let supported = path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| {
                        ["jpg", "jpeg", "png"]
                            .iter()
                            .any(|valid| ext.eq_ignore_ascii_case(valid))
                    });
                if supported && kind.is_file() {
                    match decode(&path) {
                        Ok(image) => files.push(Scanned {
                            modified: entry.metadata().and_then(|m| m.modified()).ok(),
                            is_panorama: is_panorama(image.width(), image.height()),
                            thumbnail: image.thumbnail(THUMBNAIL, THUMBNAIL).into_rgb8(),
                            path,
                        }),
                        Err(_) => skipped += 1,
                    }
                }
                Ok(())
            })();
            if result.is_err() {
                skipped += 1;
            }
        }
    }
    files.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    Ok(Scan { files, skipped })
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
