//! Deterministic analysis of photos and video.

use std::{
    collections::BTreeMap,
    f32::consts::PI,
    path::{Path, PathBuf},
    sync::Arc,
};

use image::{imageops, ImageBuffer, Rgb};

use crate::panorama::{
    equirectangular_uv, luminance, Adjust, Pano, Projection, Rectilinear, View, EXPOSURE_STOPS,
    TEMPERATURE_OFFSET, TINT_OFFSET,
};
use crate::{
    panorama::{self, Region},
    playback::PanoramaVideo,
};

const ANALYSIS_SIZE: (u32, u32) = (512, 256);
pub const VIDEO_ANALYSIS_WIDTH: u32 = ANALYSIS_SIZE.0;
pub const VIDEO_SAMPLE_SECONDS: f64 = 0.5;
const VIDEO_MIN_SEGMENT: f64 = 2.0;
pub const VIDEO_MAX_SEGMENT: f64 = 6.0;
const VIDEO_GAP: f64 = 0.5;
const VIDEO_MOTION_WEIGHT: f32 = 0.65;
const VIDEO_PEAK_THRESHOLD: f32 = 0.34;
const VIDEO_SALIENCE_FLOOR: f32 = 0.0025;
const VIDEO_MOVE_FRACTION: f32 = 0.15;
const VIDEO_TRACK_SALIENCE_FRACTION: f32 = 0.55;
const VIDEO_SMOOTH_CENTER: f32 = 2.0;
const VIDEO_NEIGHBOR_FRACTION: f32 = 0.55;
pub const NO_VIDEO_SUBJECT: &str = "Nessun altro soggetto in questo video";
const VIEW_GRID: (usize, usize) = (32, 18);
pub const THUMBNAIL_SIZE: (usize, usize) = (256, 144);
const FULL_TURN: f32 = 360.0;
const YAW_STEP: f32 = 15.0;
const PITCHES: [f32; 4] = [-30.0, -15.0, 0.0, 15.0];
const VIEW_FOV: f32 = 90.0;
const MIN_YAW_DISTANCE: f32 = 60.0;
const MAX_HIGHLIGHTS: usize = 4;
pub const MAX_SCORE: u8 = 100;
const MIN_RAW_SCORE: f32 = 0.0018;
const MIN_RELATIVE_SCORE: f32 = 0.34;

const SOBEL_SCALE: f32 = 4.0;
const CONTRAST_WEIGHT: f32 = 0.75;
const COLORFULNESS_WEIGHT: f32 = 0.25;
const COLORFULNESS_MEAN_WEIGHT: f32 = 0.3;
const FULL_ACTIVITY: f32 = 0.095;
const POLE_LATITUDE_DEGREES: f32 = 60.0;
const POLE_WEIGHT: f32 = 0.25;
const SKY_MIN_LATITUDE_DEGREES: f32 = 5.0;
const SKY_MIN_LUMA: f32 = 0.58;
const SKY_MAX_GRADIENT: f32 = 0.10;
const SKY_MAX_CHROMA: f32 = 0.12;
const SKY_WEIGHT: f32 = 0.18;

const NIGHT_MAX_LUMA: f32 = 0.23;
const NIGHT_MIN_SHARE: f32 = 0.62;
const SNOW_MIN_LUMA: f32 = 0.72;
const SNOW_MAX_CHROMA: f32 = 0.16;
const SNOW_MIN_SHARE: f32 = 0.42;
const SUNSET_MIN_RED_GREEN: f32 = 0.11;
const SUNSET_MIN_GREEN_BLUE: f32 = 0.065;
const SUNSET_MIN_SHARE: f32 = 0.27;
const SEA_MIN_BLUE_RED: f32 = 0.10;
const SEA_MIN_GREEN_RED: f32 = 0.05;
const SAND_MIN_LUMA: f32 = 0.54;
const SAND_MAX_RED_GREEN: f32 = 0.11;
const SAND_MIN_GREEN_BLUE: f32 = 0.075;
const BEACH_MIN_SHARE: f32 = 0.47;
const NATURE_MIN_GREEN_ADVANTAGE: f32 = 0.035;
const NATURE_MIN_SHARE: f32 = 0.26;

const LUMA_BINS: usize = 256;
const LOW_PERCENTILE: f32 = 0.05;
const MEDIAN_PERCENTILE: f32 = 0.50;
const HIGH_PERCENTILE: f32 = 0.95;
const TARGET_MEDIAN_LUMA: f32 = 0.45;
const MIN_MEDIAN_LUMA: f32 = 1.0 / u8::MAX as f32;
const TARGET_LUMA_SPREAD: f32 = 0.65;
const CONTRAST_GAIN: f32 = 75.0;
const TARGET_COLORFULNESS: f32 = 0.24;
const SATURATION_GAIN: f32 = 25.0;
const VIBRANCE_GAIN: f32 = 40.0;
const SLIDER_FULL_SCALE: f32 = 100.0;
const SLIDER_LIMIT: f32 = 60.0;
const EXPOSURE: usize = 0;
const CONTRAST: usize = 2;
const SATURATION: usize = 3;
const VIBRANCE: usize = 4;
const TEMPERATURE: usize = 11;
const TINT: usize = 12;
pub const COLOR_SLIDERS: [usize; 6] = [EXPOSURE, CONTRAST, SATURATION, VIBRANCE, TEMPERATURE, TINT];

const RESIZED_PROGRESS: u8 = 12;
const SALIENCE_PROGRESS: u8 = 45;
const COLOR_PROGRESS: u8 = 55;
pub const ANALYSIS_PROGRESS_END: u8 = 90;
pub const COMPLETE_PROGRESS: u8 = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scene {
    Nature,
    Beach,
    Snow,
    Sunset,
    Night,
    CityIndoors,
}

impl Scene {
    pub fn label(self) -> &'static str {
        match self {
            Self::Nature => "Natura",
            Self::Beach => "Spiaggia/Mare",
            Self::Snow => "Neve",
            Self::Sunset => "Tramonto",
            Self::Night => "Notte",
            Self::CityIndoors => "Città/Interni",
        }
    }

    pub fn suggested_filter(self) -> &'static str {
        match self {
            Self::Nature | Self::Beach => "Scenery",
            Self::Snow => "Cold",
            Self::Sunset => "Warm",
            Self::Night => "Night",
            Self::CityIndoors => "City",
        }
    }
}

#[derive(Debug)]
pub struct Highlight {
    pub view: View,
    pub score: u8,
    pub scene: Scene,
}

pub struct Analysis {
    pub highlights: Vec<Highlight>,
    pub color: Adjust,
}

struct AnalysisImage {
    width: usize,
    height: usize,
    rgb: Vec<[f32; 3]>,
}

impl AnalysisImage {
    fn from_pano(pano: &Pano) -> Option<Self> {
        let w = u32::try_from(pano.w).ok()?;
        let h = u32::try_from(pano.h).ok()?;
        let bytes = pano.w.checked_mul(pano.h)?.checked_mul(3)?;
        if !pano.is_panorama || w == 0 || h == 0 || bytes != pano.rgb.len() {
            return None;
        }
        let source = ImageBuffer::<Rgb<u8>, _>::from_raw(w, h, pano.rgb.as_slice())?;
        let width = w.min(ANALYSIS_SIZE.0);
        let height = h.min(ANALYSIS_SIZE.1);
        let resized = imageops::resize(&source, width, height, imageops::FilterType::Triangle);
        Some(Self::from_rgb(&resized))
    }

    fn from_rgb(image: &image::RgbImage) -> Self {
        Self {
            width: image.width() as usize,
            height: image.height() as usize,
            rgb: image.pixels().map(|p| p.0.map(normalize_byte)).collect(),
        }
    }

    fn motion(&self, previous: &Self, spherical: bool) -> f32 {
        if (self.width, self.height) != (previous.width, previous.height) {
            return 0.0;
        }
        let (mut difference, mut total) = (0.0, 0.0);
        for (y, (a, b)) in self
            .rgb
            .chunks_exact(self.width)
            .zip(previous.rgb.chunks_exact(self.width))
            .enumerate()
        {
            let weight = if spherical {
                self.latitude(y).cos()
            } else {
                1.0
            };
            for (&a, &b) in a.iter().zip(b) {
                difference += weight
                    * ((a[0] - b[0]).abs() + (a[1] - b[1]).abs() + (a[2] - b[2]).abs())
                    / 3.0;
            }
            total += weight * self.width as f32;
        }
        difference / total
    }

    fn index(&self, x: isize, y: isize) -> usize {
        let x = x.rem_euclid(self.width as isize) as usize;
        let y = y.clamp(0, self.height as isize - 1) as usize;
        y * self.width + x
    }

    fn latitude(&self, y: usize) -> f32 {
        (0.5 - (y as f32 + 0.5) / self.height as f32) * PI
    }

    fn sample(&self, salience: &[f32], lon: f32, lat: f32) -> (f32, [f32; 3]) {
        let (u, v) = equirectangular_uv(lon, lat, (self.width, self.height));
        let x = u.floor() as isize;
        let y = v.floor() as isize;
        let fx = u - u.floor();
        let fy = v - v.floor();
        let mix = |a: f32, b: f32, t: f32| a + (b - a) * t;
        let top = mix(
            salience[self.index(x, y)],
            salience[self.index(x + 1, y)],
            fx,
        );
        let bottom = mix(
            salience[self.index(x, y + 1)],
            salience[self.index(x + 1, y + 1)],
            fx,
        );
        let rgb = self.rgb[self.index(u.round() as isize, v.round() as isize)];
        (mix(top, bottom, fy), rgb)
    }
}

fn normalize_byte(value: u8) -> f32 {
    f32::from(value) / f32::from(u8::MAX)
}

/// Hasler–Süsstrunk opponent channels with weighted population moments.
#[derive(Default)]
struct Colorfulness {
    weight: f32,
    sum: [f32; 2],
    squares: [f32; 2],
}

impl Colorfulness {
    fn add(&mut self, [r, g, b]: [f32; 3], weight: f32) {
        self.weight += weight;
        for (i, value) in [r - g, (r + g) * 0.5 - b].into_iter().enumerate() {
            self.sum[i] += value * weight;
            self.squares[i] += value * value * weight;
        }
    }

    fn measure(&self) -> (f32, f32) {
        let [rg, yb] = self.sum.map(|value| value / self.weight);
        let [rg_sq, yb_sq] = self.squares.map(|value| value / self.weight);
        let stddev = ((rg_sq - rg * rg).max(0.0) + (yb_sq - yb * yb).max(0.0)).sqrt();
        (stddev, stddev + COLORFULNESS_MEAN_WEIGHT * rg.hypot(yb))
    }
}

fn salience_map(image: &AnalysisImage) -> Vec<f32> {
    let luma: Vec<_> = image.rgb.iter().copied().map(luminance).collect();
    let mut map = Vec::with_capacity(luma.len());
    for y in 0..image.height {
        let latitude = image.latitude(y);
        let pole = if latitude.abs() > POLE_LATITUDE_DEGREES.to_radians() {
            POLE_WEIGHT
        } else {
            1.0
        };
        for x in 0..image.width {
            let index = |dx, dy| image.index(x as isize + dx, y as isize + dy);
            let l = |dx, dy| luma[index(dx, dy)];
            let gx = (l(1, -1) - l(-1, -1)) + 2.0 * (l(1, 0) - l(-1, 0)) + (l(1, 1) - l(-1, 1));
            let gy = (l(-1, 1) - l(-1, -1)) + 2.0 * (l(0, 1) - l(0, -1)) + (l(1, 1) - l(1, -1));
            let gradient = gx.hypot(gy) / SOBEL_SCALE;
            let mut color = Colorfulness::default();
            for dy in -1..=1 {
                for dx in -1..=1 {
                    color.add(image.rgb[index(dx, dy)], 1.0);
                }
            }
            let (variation, colorfulness) = color.measure();
            let activity = (gradient.max(variation) / FULL_ACTIVITY).min(1.0);
            let [r, g, b] = image.rgb[index(0, 0)];
            let sky = if latitude >= SKY_MIN_LATITUDE_DEGREES.to_radians()
                && l(0, 0) >= SKY_MIN_LUMA
                && gradient <= SKY_MAX_GRADIENT
                && (b >= r || chroma([r, g, b]) <= SKY_MAX_CHROMA)
            {
                SKY_WEIGHT
            } else {
                1.0
            };
            map.push(
                (gradient * CONTRAST_WEIGHT + colorfulness * COLORFULNESS_WEIGHT)
                    * activity
                    * latitude.cos()
                    * pole
                    * sky,
            );
        }
    }
    map
}

fn chroma([r, g, b]: [f32; 3]) -> f32 {
    r.max(g).max(b) - r.min(g).min(b)
}

#[derive(Default)]
struct SceneColors {
    dark: f32,
    white: f32,
    warm: f32,
    beach: f32,
    green: f32,
}

impl SceneColors {
    fn add(&mut self, rgb: [f32; 3], weight: f32) {
        let [r, g, b] = rgb;
        let luma = luminance(rgb);
        if luma < NIGHT_MAX_LUMA {
            self.dark += weight;
        }
        if luma >= SNOW_MIN_LUMA && chroma(rgb) <= SNOW_MAX_CHROMA {
            self.white += weight;
        }
        if r - g >= SUNSET_MIN_RED_GREEN && g - b >= SUNSET_MIN_GREEN_BLUE {
            self.warm += weight;
        }
        let sea = b - r >= SEA_MIN_BLUE_RED && g - r >= SEA_MIN_GREEN_RED;
        let sand = luma >= SAND_MIN_LUMA
            && (0.0..SAND_MAX_RED_GREEN).contains(&(r - g))
            && g - b >= SAND_MIN_GREEN_BLUE;
        if sea || sand {
            self.beach += weight;
        }
        if g - r.max(b) >= NATURE_MIN_GREEN_ADVANTAGE {
            self.green += weight;
        }
    }

    fn classify(&self, weight: f32) -> Scene {
        if self.dark >= weight * NIGHT_MIN_SHARE {
            Scene::Night
        } else if self.white >= weight * SNOW_MIN_SHARE {
            Scene::Snow
        } else if self.warm >= weight * SUNSET_MIN_SHARE {
            Scene::Sunset
        } else if self.green >= weight * NATURE_MIN_SHARE {
            Scene::Nature
        } else if self.beach >= weight * BEACH_MIN_SHARE {
            Scene::Beach
        } else {
            Scene::CityIndoors
        }
    }
}

struct Candidate {
    view: View,
    salience: f32,
    scene: Scene,
}

fn evaluate_view(image: &AnalysisImage, map: &[f32], view: View) -> Candidate {
    let camera = Rectilinear::new(&view);
    let (width, height) = VIEW_GRID;
    let mut total_weight = 0.0;
    let mut salience = 0.0;
    let mut colors = SceneColors::default();
    for y in 0..height {
        let ny = 1.0 - (2 * y + 1) as f32 / height as f32;
        for x in 0..width {
            let nx = (2 * x + 1) as f32 / width as f32 - 1.0;
            let (lon, lat, ray_length) = camera.project(nx, ny);
            // Solid angle of a perspective cell, to avoid favouring the frame edges.
            let weight = ray_length.recip().powi(3);
            let (sample, rgb) = image.sample(map, lon, lat);
            salience += sample * weight;
            colors.add(rgb, weight);
            total_weight += weight;
        }
    }
    Candidate {
        view,
        salience: salience / total_weight,
        scene: colors.classify(total_weight),
    }
}

fn yaw_distance(a: f32, b: f32) -> f32 {
    let distance = (a - b).abs();
    distance.min(FULL_TURN - distance)
}

fn suppress_neighbours(mut candidates: Vec<Candidate>) -> Vec<Highlight> {
    candidates.sort_by(|a, b| {
        b.salience
            .total_cmp(&a.salience)
            .then_with(|| a.view.pitch.abs().total_cmp(&b.view.pitch.abs()))
    });
    let mut highlights: Vec<Highlight> = Vec::new();
    if let Some(best) = candidates.first().map(|candidate| candidate.salience) {
        let minimum = MIN_RAW_SCORE.max(best * MIN_RELATIVE_SCORE);
        for candidate in candidates {
            if candidate.salience < minimum || highlights.len() == MAX_HIGHLIGHTS {
                break;
            }
            if highlights
                .iter()
                .all(|other| yaw_distance(other.view.yaw, candidate.view.yaw) >= MIN_YAW_DISTANCE)
            {
                highlights.push(Highlight {
                    view: candidate.view,
                    score: (candidate.salience / best * f32::from(MAX_SCORE)).round() as u8,
                    scene: candidate.scene,
                });
            }
        }
    }
    highlights
}

fn percentile(histogram: &[f32; LUMA_BINS], weight: f32, fraction: f32) -> f32 {
    let mut cumulative = 0.0;
    for (bin, count) in histogram.iter().enumerate() {
        cumulative += count;
        if cumulative >= weight * fraction {
            return normalize_byte(bin as u8);
        }
    }
    1.0
}

fn automatic_color(image: &AnalysisImage) -> Adjust {
    let mut histogram = [0.0; LUMA_BINS];
    let mut rgb_sum = [0.0; 3];
    let mut color = Colorfulness::default();
    for (y, row) in image.rgb.chunks_exact(image.width).enumerate() {
        let weight = image.latitude(y).cos();
        for &rgb in row {
            let bin = (luminance(rgb) * f32::from(u8::MAX)).round() as usize;
            histogram[bin] += weight;
            for (sum, channel) in rgb_sum.iter_mut().zip(rgb) {
                *sum += channel * weight;
            }
            color.add(rgb, weight);
        }
    }
    let weight = histogram.iter().sum();
    let median = percentile(&histogram, weight, MEDIAN_PERCENTILE).max(MIN_MEDIAN_LUMA);
    let spread = percentile(&histogram, weight, HIGH_PERCENTILE)
        - percentile(&histogram, weight, LOW_PERCENTILE);
    let [r, g, b] = rgb_sum.map(|sum| sum / weight);
    let exposure = (TARGET_MEDIAN_LUMA / median).log2() / EXPOSURE_STOPS * SLIDER_FULL_SCALE;
    let exposure_gain = 2.0_f32
        .powf(exposure.clamp(-SLIDER_LIMIT, SLIDER_LIMIT) / SLIDER_FULL_SCALE * EXPOSURE_STOPS);
    let muted = (1.0 - color.measure().1 / TARGET_COLORFULNESS).max(0.0);
    let values = [
        exposure,
        (TARGET_LUMA_SPREAD - spread) * CONTRAST_GAIN,
        muted * SATURATION_GAIN,
        muted * VIBRANCE_GAIN,
        (b - r) * exposure_gain * SLIDER_FULL_SCALE / (2.0 * TEMPERATURE_OFFSET),
        (g - (r + b) * 0.5) * exposure_gain * SLIDER_FULL_SCALE / TINT_OFFSET,
    ];
    let mut adjust = Adjust::default();
    for (index, value) in COLOR_SLIDERS.into_iter().zip(values) {
        adjust.v[index] = value.round().clamp(-SLIDER_LIMIT, SLIDER_LIMIT);
    }
    adjust
}

/// Analyze immutable RGB data. Progress is reported in ascending percentages.
pub fn analyze(pano: &Pano, mut progress: impl FnMut(u8)) -> Option<Analysis> {
    let image = AnalysisImage::from_pano(pano)?;
    progress(RESIZED_PROGRESS);
    let map = salience_map(&image);
    progress(SALIENCE_PROGRESS);
    let color = automatic_color(&image);
    progress(COLOR_PROGRESS);

    let candidates = candidate_views(&image, &map, |step, yaw_steps| {
        let done = (ANALYSIS_PROGRESS_END - COLOR_PROGRESS) as usize * (step + 1) / yaw_steps;
        progress(COLOR_PROGRESS + done as u8);
    });
    let highlights = suppress_neighbours(candidates);
    Some(Analysis { highlights, color })
}

fn candidate_views(
    image: &AnalysisImage,
    map: &[f32],
    mut progress: impl FnMut(usize, usize),
) -> Vec<Candidate> {
    let yaw_steps = (FULL_TURN / YAW_STEP) as usize;
    let mut candidates = Vec::with_capacity(yaw_steps * PITCHES.len());
    for step in 0..yaw_steps {
        let yaw = step as f32 * YAW_STEP - FULL_TURN * 0.5;
        for pitch in PITCHES {
            let view = View {
                yaw,
                pitch,
                fov: VIEW_FOV,
                projection: Projection::Rectilinear,
            };
            candidates.push(evaluate_view(image, map, view));
        }
        progress(step, yaw_steps);
    }
    candidates
}

#[derive(Clone, Debug)]
pub struct Segment {
    pub source: PathBuf,
    pub start: f64,
    pub end: f64,
    pub view: View,
    pub start_view: View,
    pub end_view: View,
    pub score: u8,
    pub scene: Scene,
    pub thumbnail: Arc<[u8]>,
}

pub struct VideoOptions<'a> {
    pub source: &'a Path,
    pub cancelled: &'a dyn Fn() -> bool,
}

struct Sample {
    time: f64,
    best: Candidate,
    strength: f32,
}

pub fn analyze_video(
    reader: &mut PanoramaVideo,
    options: VideoOptions<'_>,
    mut progress: impl FnMut(u8),
) -> Result<Vec<Segment>, String> {
    progress(0);
    let duration = reader.duration;
    let mut samples = Vec::new();
    let mut previous = None;
    let mut time = 0.0;
    while duration <= 0.0 || time < duration {
        if (options.cancelled)() {
            return Err("Analisi video annullata.".into());
        }
        let Some((actual, rgb)) = reader.sample_at(time)? else {
            break;
        };
        if samples
            .last()
            .is_some_and(|sample: &Sample| actual <= sample.time)
        {
            if reader.at_end() {
                break;
            }
            time += VIDEO_SAMPLE_SECONDS;
            continue;
        }
        let image = if rgb.width() > ANALYSIS_SIZE.0 || rgb.height() > ANALYSIS_SIZE.1 {
            let width = u64::from(rgb.width());
            let height = u64::from(rgb.height());
            let (max_w, max_h) = (u64::from(ANALYSIS_SIZE.0), u64::from(ANALYSIS_SIZE.1));
            let size = (
                (height * max_w / width).min(max_h).max(1),
                (width * max_h / height).min(max_w).max(1),
            );
            AnalysisImage::from_rgb(&imageops::resize(
                rgb,
                size.1 as u32,
                size.0 as u32,
                imageops::FilterType::Triangle,
            ))
        } else {
            AnalysisImage::from_rgb(rgb)
        };
        let spherical = reader.is_panorama();
        let map = salience_map(&image);
        let best = if spherical {
            candidate_views(&image, &map, |_, _| {})
                .into_iter()
                .max_by(|a, b| {
                    a.salience
                        .total_cmp(&b.salience)
                        .then_with(|| b.view.pitch.abs().total_cmp(&a.view.pitch.abs()))
                })
        } else {
            let mut colors = SceneColors::default();
            for &rgb in &image.rgb {
                colors.add(rgb, 1.0);
            }
            Some(Candidate {
                salience: map.iter().sum::<f32>() / map.len() as f32,
                view: View::default(),
                scene: colors.classify(map.len() as f32),
            })
        };
        if let Some(best) = best {
            let motion = previous
                .as_ref()
                .map_or(0.0, |old| image.motion(old, spherical));
            samples.push(Sample {
                time: actual,
                strength: best.salience + VIDEO_MOTION_WEIGHT * motion,
                best,
            });
        }
        previous = Some(image);
        time += VIDEO_SAMPLE_SECONDS;
        if duration > 0.0 {
            progress(
                (time / duration * f64::from(ANALYSIS_PROGRESS_END))
                    .min(f64::from(ANALYSIS_PROGRESS_END)) as u8,
            );
        }
    }
    let duration = if duration > 0.0 {
        duration
    } else {
        samples
            .last()
            .map_or(0.0, |sample| sample.time + 1.0 / reader.fps)
    };
    let peaks = video_peaks(&samples, duration);
    let mut segments = Vec::with_capacity(peaks.len());
    let maximum = peaks
        .values()
        .map(|(_, _, strength)| *strength)
        .fold(0.0, f32::max);
    let spherical = reader.is_panorama();
    for (index, (start, end, strength)) in peaks.values().copied().enumerate() {
        if (options.cancelled)() {
            return Err("Analisi video annullata.".into());
        }
        let from = samples.partition_point(|sample| sample.time < start);
        let to = samples.partition_point(|sample| sample.time < end);
        let window = &samples[from..to];
        let best = window.iter().max_by(|a, b| {
            a.best
                .salience
                .total_cmp(&b.best.salience)
                .then_with(|| a.strength.total_cmp(&b.strength))
        });
        let Some(sample) = best else { continue };
        let thumbnail = if let Some((_, image)) = reader.sample_at(sample.time)? {
            let mut pano = Pano::from_rgb(
                0,
                String::new(),
                image.width() as usize,
                image.height() as usize,
                image.as_raw().clone(),
            );
            pano.is_panorama = spherical;
            panorama::render(
                &pano,
                &sample.best.view,
                &Adjust::default(),
                THUMBNAIL_SIZE,
                Region::FULL,
                None,
            )
        } else {
            Vec::new()
        };
        let track_view = |edge: Option<&Sample>| {
            edge.filter(|edge| {
                edge.best.salience >= sample.best.salience * VIDEO_TRACK_SALIENCE_FRACTION
            })
            .map_or(sample.best.view, |edge| edge.best.view)
        };
        segments.push(Segment {
            source: options.source.to_path_buf(),
            start,
            end,
            view: sample.best.view,
            start_view: track_view(window.first()),
            end_view: track_view(window.last()),
            score: (strength / maximum * f32::from(MAX_SCORE)).round() as u8,
            scene: sample.best.scene,
            thumbnail: thumbnail.into(),
        });
        progress(
            ANALYSIS_PROGRESS_END
                + ((index + 1) * usize::from(COMPLETE_PROGRESS - ANALYSIS_PROGRESS_END)
                    / peaks.len()) as u8,
        );
    }
    for i in 1..segments.len() {
        let next = segments[i].view;
        let previous = &mut segments[i - 1];
        previous.end_view =
            panorama::interpolate_view(previous.end_view, next, VIDEO_MOVE_FRACTION);
    }
    progress(COMPLETE_PROGRESS);
    Ok(segments)
}

fn video_peaks(samples: &[Sample], duration: f64) -> BTreeMap<usize, (f64, f64, f32)> {
    let mut peaks = BTreeMap::<usize, (f64, f64, f32)>::new();
    let smooth: Vec<_> = samples
        .iter()
        .enumerate()
        .map(|(i, sample)| {
            let before = samples.get(i.wrapping_sub(1)).unwrap_or(sample);
            let after = samples.get(i + 1).unwrap_or(sample);
            (before.strength + VIDEO_SMOOTH_CENTER * sample.strength + after.strength)
                / (2.0 + VIDEO_SMOOTH_CENTER)
        })
        .collect();
    let maximum = smooth.iter().copied().fold(0.0, f32::max);
    let threshold = (maximum * VIDEO_PEAK_THRESHOLD).max(VIDEO_SALIENCE_FLOOR);
    let mut candidates: Vec<_> = (0..smooth.len())
        .filter(|&i| {
            smooth[i] >= threshold
                && smooth[i] > smooth.get(i.wrapping_sub(1)).copied().unwrap_or(0.0)
                && smooth[i] >= smooth.get(i + 1).copied().unwrap_or(0.0)
        })
        .collect();
    candidates.sort_by(|&a, &b| smooth[b].total_cmp(&smooth[a]).then(a.cmp(&b)));
    for i in candidates {
        let center = samples[i].time;
        let mut start = center - VIDEO_MIN_SEGMENT / 2.0;
        let mut end = center + VIDEO_MIN_SEGMENT / 2.0;
        let shoulder = (smooth[i] * VIDEO_NEIGHBOR_FRACTION).max(threshold);
        let reach = ((VIDEO_MAX_SEGMENT / 2.0) / VIDEO_SAMPLE_SECONDS) as usize;
        for j in (i.saturating_sub(reach)..i)
            .rev()
            .take_while(|&j| smooth[j] >= shoulder)
        {
            start = start.min(samples[j].time);
        }
        for j in (i + 1..=(i + reach).min(samples.len() - 1)).take_while(|&j| smooth[j] >= shoulder)
        {
            end = end.max(samples[j].time);
        }
        start = start.clamp(0.0, (duration - VIDEO_MIN_SEGMENT).max(0.0));
        end = end
            .max(start + VIDEO_MIN_SEGMENT)
            .min(duration)
            .min(start + VIDEO_MAX_SEGMENT);
        let overlaps = peaks
            .range(..i)
            .next_back()
            .is_some_and(|(_, (_, tail, _))| start < tail + VIDEO_GAP)
            || peaks
                .range(i..)
                .next()
                .is_some_and(|(_, (head, _, _))| end + VIDEO_GAP > *head);
        if end > start && !overlaps {
            peaks.insert(i, (start, end, smooth[i]));
        }
    }
    peaks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_peak_finds_the_colored_spot_between_three_and_five_seconds() -> Result<(), String> {
        let directory = std::path::Path::new("target");
        let path = directory.join(format!("autoedit-spot-{}.mp4", std::process::id()));
        let mut writer = match crate::video::VideoWriter::new(&path, 320, 160, 10, 600_000) {
            Err(error) if error.is_unavailable() => {
                eprintln!("Analisi video saltata: {error}");
                return Ok(());
            }
            result => result.map_err(|error| error.to_string())?,
        };
        let check = (|| {
            for frame in 0..80 {
                let rgba = image::RgbaImage::from_fn(320, 160, |x, y| {
                    let active = (30..50).contains(&frame)
                        && x.abs_diff(240).pow(2) + y.abs_diff(80).pow(2) <= 22 * 22;
                    image::Rgba(if active {
                        [250, 10, 195, 255]
                    } else {
                        [90, 90, 90, 255]
                    })
                });
                writer
                    .write_frame(&rgba)
                    .map_err(|error| error.to_string())?;
            }
            writer.finish().map_err(|error| error.to_string())?;
            let mut reader = crate::playback::PanoramaVideo::open(&path, ANALYSIS_SIZE.0)?;
            analyze_video(
                &mut reader,
                VideoOptions {
                    source: &path,
                    cancelled: &|| false,
                },
                |_| {},
            )
        })();
        let _ = std::fs::remove_file(&path);
        let segments = check?;
        assert_eq!(segments.len(), 1);
        let segment = &segments[0];
        assert!(segment.start < 5.0 && segment.end > 3.0);
        assert!(
            yaw_distance(segment.view.yaw, 90.0) <= 15.0,
            "{:?}",
            segment.view
        );
        Ok(())
    }

    fn test_panorama(color: impl Fn(usize, usize) -> [u8; 3]) -> Pano {
        let (w, h) = (512, 256);
        let mut rgb = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            for x in 0..w {
                rgb.extend(color(x, y));
            }
        }
        Pano::from_rgb(1, String::new(), w, h, rgb)
    }

    #[test]
    fn finds_the_subject_at_yaw_ninety() {
        let pano = test_panorama(|x, y| {
            if (357..411).contains(&x) && (108..151).contains(&y) {
                return if (x / 5 + y / 5) % 2 == 0 {
                    [248, 210, 0]
                } else {
                    [18, 4, 109]
                };
            }
            [90; 3]
        });
        let analysis = analyze(&pano, |_| {}).expect("panorama valido");
        let first = analysis.highlights.first().expect("momento saliente");
        assert!((first.view.yaw - 90.0).abs() <= 15.0, "{first:?}");
        assert!((0..=100).contains(&first.score));
    }

    #[test]
    fn corrects_darkness_and_a_blue_cast() {
        let dark = test_panorama(|_, _| [30; 3]);
        let blue = test_panorama(|_, _| [100, 115, 155]);
        let dark = analyze(&dark, |_| {}).expect("panorama valido");
        let blue = analyze(&blue, |_| {}).expect("panorama valido");
        assert!(dark.color.v[0] > 0.0);
        assert!(blue.color.v[11] > 0.0);
        assert!(dark.highlights.is_empty());
        assert!(blue.highlights.is_empty());
    }

    #[test]
    fn separates_highlights_across_the_seam() {
        let pano = test_panorama(|x, y| {
            if !(100..160).contains(&y) || (x + 24) % 128 >= 48 {
                return [90; 3];
            }
            if (x / 5 + y / 5) % 2 == 0 {
                [240, 210, 0]
            } else {
                [25, 15, 139]
            }
        });
        let analysis = analyze(&pano, |_| {}).expect("panorama valido");
        assert!((2..=4).contains(&analysis.highlights.len()));
        for (i, first) in analysis.highlights.iter().enumerate() {
            for second in &analysis.highlights[i + 1..] {
                let distance = (first.view.yaw - second.view.yaw).abs();
                assert!(distance.min(360.0 - distance) >= 60.0);
            }
        }
    }

    #[test]
    fn handles_tiny_and_invalid_images() {
        let tiny = Pano::from_rgb(1, String::new(), 2, 1, vec![100; 6]);
        let invalid = Pano::from_rgb(1, String::new(), usize::MAX, 1, vec![]);
        let empty = Pano::from_rgb(1, String::new(), 0, 0, vec![]);
        assert!(analyze(&tiny, |_| {})
            .expect("panorama valido")
            .highlights
            .is_empty());
        assert!(analyze(&invalid, |_| {}).is_none());
        assert!(analyze(&empty, |_| {}).is_none());
    }
}
