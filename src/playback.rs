//! Pairing, panorama preparation and the editor's playback worker.

use std::{
    path::{Path, PathBuf},
    sync::{mpsc, Arc},
    time::{Duration, Instant},
};

use eframe::egui;
use image::{imageops, DynamicImage, RgbImage};

use crate::{
    fisheye::{self, Stitcher},
    panorama::{Pano, PanoRef},
    video::{self, VideoFrame, VideoReader},
};

pub const EDITOR_MAX_WIDTH: u32 = 4096;
const LENS_SYNC_FRAMES: f64 = 0.75;
const SEEK_TOLERANCE_FRAMES: f64 = 0.5;
const FORWARD_SEEK_SECONDS: f64 = 1.0;

pub fn is_video(path: &Path) -> bool {
    path.extension().is_some_and(|ext| {
        ["insv", "mp4", "mov"]
            .into_iter()
            .any(|format| ext.eq_ignore_ascii_case(format))
    })
}

fn other_lens(path: &Path, suffix: &str, other: &str) -> Option<PathBuf> {
    if !video::is_insv(path) {
        return None;
    }
    let name = path.file_name()?.to_str()?;
    let index = name.rfind(suffix)?;
    if name.rfind(other).is_some_and(|other| other > index) {
        return None;
    }
    let (prefix, end) = (&name[..index], &name[index + suffix.len()..]);
    let candidate = path.with_file_name(format!("{prefix}{other}{end}"));
    candidate.is_file().then_some(candidate)
}

pub fn primary_lens(path: &Path) -> Option<PathBuf> {
    other_lens(path, "_10_", "_00_")
}

pub fn secondary_lens(path: &Path) -> Option<PathBuf> {
    other_lens(path, "_00_", "_10_")
}

pub fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

pub fn format_time(seconds: f64) -> String {
    let seconds = (seconds.max(0.0) + 0.001) as u64;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

pub fn panorama_photo(rgb: RgbImage, is_insp: bool, max_width: u32) -> (RgbImage, bool) {
    if (is_insp && rgb.width() >= 2 && rgb.height() > 0) || fisheye::has_side_by_side_circles(&rgb)
    {
        let stitched =
            Stitcher::side_by_side(rgb.dimensions(), rgb.width().min(max_width)).render(&rgb, None);
        return (stitched, true);
    }
    let is_panorama = fisheye::is_equirectangular(rgb.dimensions());
    (limit_width(rgb, max_width), is_panorama)
}

fn limit_width(rgb: RgbImage, max_width: u32) -> RgbImage {
    if rgb.width() <= max_width {
        return rgb;
    }
    let height = (u64::from(rgb.height()) * u64::from(max_width) / u64::from(rgb.width())).max(1);
    imageops::resize(
        &rgb,
        max_width,
        height as u32,
        imageops::FilterType::Triangle,
    )
}

pub struct PanoramaVideo {
    primary: VideoReader,
    secondary: Option<VideoReader>,
    two_lenses: bool,
    lens_frames: [Option<VideoFrame>; 2],
    stitcher: Option<(LensLayout, Stitcher)>,
    cached: Option<(f64, RgbImage)>,
    cached_request: f64,
    at_end: bool,
    max_width: u32,
    pub duration: f64,
    pub fps: f64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LensLayout {
    Packed((u32, u32)),
    Separate((u32, u32), (u32, u32)),
}

impl PanoramaVideo {
    pub fn open(path: &Path, max_width: u32) -> Result<Self, String> {
        let primary_path = primary_lens(path);
        let path = primary_path.as_deref().unwrap_or(path);
        let primary = VideoReader::open(path).map_err(|e| e.to_string())?;
        let info = primary.info();
        let dual_stream = video::is_insv(path) && info.video_streams >= 2;
        let secondary = if dual_stream {
            None
        } else {
            secondary_lens(path)
                .map(|path| {
                    VideoReader::open(&path).map_err(|e| format!("{}: {e}", file_name(&path)))
                })
                .transpose()?
        };
        let other_duration = secondary
            .as_ref()
            .map_or(0.0, |reader| reader.info().duration);
        let duration = if info.duration > 0.0 && other_duration > 0.0 {
            info.duration.min(other_duration)
        } else {
            info.duration.max(other_duration)
        };
        Ok(Self {
            two_lenses: dual_stream || secondary.is_some(),
            primary,
            secondary,
            lens_frames: [None, None],
            stitcher: None,
            cached: None,
            cached_request: 0.0,
            at_end: false,
            max_width,
            duration,
            fps: info.fps,
        })
    }

    pub fn is_panorama(&self) -> bool {
        let info = self.primary.info();
        self.two_lenses
            || self.stitcher.is_some()
            || fisheye::is_equirectangular((info.width, info.height))
    }

    fn raw_frames(&mut self) -> Result<Option<(VideoFrame, Option<VideoFrame>)>, String> {
        if !self.two_lenses {
            return self
                .primary
                .next_frame()
                .map(|frame| frame.map(|frame| (frame, None)))
                .map_err(|e| e.to_string());
        }
        let tolerance = LENS_SYNC_FRAMES / self.fps;
        loop {
            if self.lens_frames[0].is_none() {
                self.lens_frames[0] = self.primary.next_frame().map_err(|e| e.to_string())?;
            }
            if self.lens_frames[1].is_none() {
                self.lens_frames[1] = match &mut self.secondary {
                    Some(reader) => reader.next_frame(),
                    None => self.primary.next_on(1),
                }
                .map_err(|e| e.to_string())?;
            }
            let [Some(first), Some(second)] = &self.lens_frames else {
                return Ok(None);
            };
            let delta = first.seconds - second.seconds;
            if delta.abs() <= tolerance {
                if let [Some(first), Some(second)] = std::mem::take(&mut self.lens_frames) {
                    return Ok(Some((first, Some(second))));
                }
            } else {
                self.lens_frames[usize::from(delta > 0.0)] = None;
            }
        }
    }

    pub fn next_frame(&mut self) -> Result<Option<(f64, RgbImage)>, String> {
        Ok(self
            .raw_frames()?
            .map(|(first, second)| self.project(first, second)))
    }

    pub fn frame_at(&mut self, requested: f64) -> Result<Option<(f64, RgbImage)>, String> {
        self.cached = None;
        self.at_end = false;
        let seconds = if self.duration > 0.0 {
            requested.min((self.duration - 1.0 / self.fps).max(0.0))
        } else {
            requested
        };
        self.primary.seek(seconds).map_err(|e| e.to_string())?;
        if let Some(reader) = &mut self.secondary {
            reader.seek(seconds).map_err(|e| e.to_string())?;
        }
        self.lens_frames = [None, None];
        let target = seconds - SEEK_TOLERANCE_FRAMES / self.fps;
        let mut last = None;
        while let Some((first, second)) = self.raw_frames()? {
            if first.seconds >= target {
                return Ok(Some(self.project(first, second)));
            }
            last = Some((first, second));
        }
        self.at_end = true;
        Ok(last.map(|(first, second)| self.project(first, second)))
    }

    /// Decode forward between nearby requests, projecting only the selected frame.
    pub fn sample_at(&mut self, seconds: f64) -> Result<Option<(f64, &RgbImage)>, String> {
        if self.cached.is_none()
            || seconds < self.cached_request
            || seconds - self.cached_request > FORWARD_SEEK_SECONDS
        {
            self.cached = self.frame_at(seconds)?;
        } else {
            let target = seconds - SEEK_TOLERANCE_FRAMES / self.fps;
            let mut last = None;
            while !self.at_end && self.cached.as_ref().is_some_and(|(time, _)| *time < target) {
                let Some((first, second)) = self.raw_frames()? else {
                    self.at_end = true;
                    if let Some((first, second)) = last {
                        self.cached = Some(self.project(first, second));
                    }
                    break;
                };
                if first.seconds >= target {
                    self.cached = Some(self.project(first, second));
                    break;
                }
                if self.duration > 0.0 && first.seconds + 1.0 / self.fps >= self.duration {
                    self.cached = Some(self.project(first, second));
                    break;
                }
                last = Some((first, second));
            }
        }
        self.cached_request = seconds;
        Ok(self.cached.as_ref().map(|(time, image)| (*time, image)))
    }

    pub fn at_end(&self) -> bool {
        self.at_end
    }

    fn project(&mut self, first: VideoFrame, second: Option<VideoFrame>) -> (f64, RgbImage) {
        let left = DynamicImage::ImageRgba8(first.rgba).into_rgb8();
        let right = second.map(|frame| DynamicImage::ImageRgba8(frame.rgba).into_rgb8());
        let size = left.dimensions();
        let layout = if let Some(right) = &right {
            Some(LensLayout::Separate(size, right.dimensions()))
        } else if self
            .stitcher
            .as_ref()
            .is_some_and(|(layout, _)| *layout == LensLayout::Packed(size))
            || fisheye::has_side_by_side_circles(&left)
        {
            Some(LensLayout::Packed(size))
        } else {
            None
        };
        let rgb = if let Some(layout) = layout {
            if self
                .stitcher
                .as_ref()
                .is_none_or(|(current, _)| *current != layout)
            {
                let stitcher = match layout {
                    LensLayout::Packed(size) => {
                        Stitcher::side_by_side(size, size.0.min(self.max_width))
                    }
                    LensLayout::Separate(left, right) => Stitcher::separate(
                        left,
                        right,
                        left.0.saturating_add(right.0).min(self.max_width),
                    ),
                };
                self.stitcher = Some((layout, stitcher));
            }
            match &self.stitcher {
                Some((_, stitcher)) => stitcher.render(&left, right.as_ref()),
                None => limit_width(left, self.max_width),
            }
        } else {
            limit_width(left, self.max_width)
        };
        (first.seconds.max(0.0), rgb)
    }
}

type Still = (f64, PanoRef);

#[derive(Clone, Copy)]
enum Request {
    Next,
    Seek(f64),
}

enum Reply {
    Opened { duration: f64, first: Still },
    Frame(Option<Still>),
    Error(String),
}

pub struct Playback {
    requests: mpsc::SyncSender<Request>,
    replies: mpsc::Receiver<Reply>,
    in_flight: Option<Request>,
    next: Option<Still>,
    pending_seek: Option<f64>,
    opened: bool,
    end: bool,
    clock: Option<(Instant, f64)>,
    position: f64,
    duration: f64,
}

impl Playback {
    pub fn position(&self) -> f64 {
        self.position
    }

    pub fn duration(&self) -> f64 {
        self.duration
    }

    pub fn start(ctx: &egui::Context, path: PathBuf, id: u64) -> Self {
        let (requests, commands) = mpsc::sync_channel(1);
        let (events, replies) = mpsc::sync_channel(1);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let send = |reply| {
                let received = events.send(reply).is_ok();
                if received {
                    ctx.request_repaint();
                }
                received
            };
            if let Err(error) = decode_video(&path, id, &commands, &send) {
                send(Reply::Error(error));
            }
        });
        Self {
            requests,
            replies,
            in_flight: None,
            next: None,
            pending_seek: None,
            opened: false,
            end: false,
            clock: None,
            position: 0.0,
            duration: 0.0,
        }
    }

    pub fn poll(&mut self, ctx: &egui::Context) -> Result<Option<PanoRef>, String> {
        let mut ready = None;
        match self.replies.try_recv() {
            Ok(Reply::Opened { duration, first }) => {
                self.opened = true;
                self.duration = duration;
                self.position = first.0;
                ready = Some(first.1);
            }
            Ok(Reply::Frame(frame)) => {
                let request = self.in_flight.take();
                if self.pending_seek.is_none() {
                    match (request, frame) {
                        (Some(Request::Seek(_)), Some((seconds, pano))) => {
                            self.position = seconds;
                            if self.clock.is_some() {
                                self.clock = Some((Instant::now(), seconds));
                            }
                            ready = Some(pano);
                        }
                        (_, Some(frame)) => self.next = Some(frame),
                        (_, None) => {
                            self.end = true;
                            self.pause();
                            self.position = self.duration.max(self.position);
                        }
                    }
                }
            }
            Ok(Reply::Error(error)) => return Err(error),
            Err(mpsc::TryRecvError::Disconnected) => {
                return Err("La lettura del video è stata interrotta.".into())
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
        if self.opened && self.in_flight.is_none() {
            if let Some(seconds) = self.pending_seek.take() {
                self.request(Request::Seek(seconds))?;
            } else if let Some((start, at)) = self.clock {
                if let Some((seconds, _)) = &self.next {
                    let wait = seconds - (at + start.elapsed().as_secs_f64());
                    if wait > 0.0 {
                        ctx.request_repaint_after(Duration::from_secs_f64(wait.min(1.0)));
                    } else if let Some((seconds, pano)) = self.next.take() {
                        self.position = seconds;
                        ready = Some(pano);
                    }
                }
                if self.next.is_none() {
                    self.request(Request::Next)?;
                }
            }
        }
        Ok(ready)
    }

    fn request(&mut self, request: Request) -> Result<(), String> {
        self.requests
            .try_send(request)
            .map_err(|_| "Impossibile richiedere un fotogramma.".to_owned())?;
        self.in_flight = Some(request);
        Ok(())
    }

    pub fn pause(&mut self) {
        self.clock = None;
    }

    fn seek(&mut self, seconds: f64) {
        self.pending_seek = Some(seconds);
        self.next = None;
        self.end = false;
    }

    pub fn controls(&mut self, ui: &mut egui::Ui, width: f32) {
        ui.horizontal(|ui| {
            let playing = self.clock.is_some();
            if ui
                .button(if playing { "Pausa" } else { "Riproduci" })
                .clicked()
            {
                if playing {
                    self.pause();
                } else {
                    if self.end {
                        self.seek(0.0);
                    }
                    self.clock = Some((Instant::now(), self.position));
                }
                ui.ctx().request_repaint();
            }
            let mut seconds = self
                .pending_seek
                .or_else(|| match self.in_flight {
                    Some(Request::Seek(seconds)) => Some(seconds),
                    _ => None,
                })
                .unwrap_or(self.position);
            ui.spacing_mut().slider_width = (width - 194.0).max(24.0);
            let slider = egui::Slider::new(&mut seconds, 0.0..=self.duration)
                .show_value(false)
                .trailing_fill(true);
            if ui
                .add_enabled(self.duration > 0.0, slider)
                .on_hover_text("Posizione nel video")
                .changed()
            {
                self.seek(seconds);
                ui.ctx().request_repaint();
            }
            ui.label(format!(
                "{} / {}",
                format_time(seconds),
                format_time(self.duration)
            ));
        });
    }
}

fn decode_video(
    path: &Path,
    id: u64,
    commands: &mpsc::Receiver<Request>,
    send: &impl Fn(Reply) -> bool,
) -> Result<(), String> {
    let mut video = PanoramaVideo::open(path, EDITOR_MAX_WIDTH)?;
    let name = file_name(path);
    let still = |(seconds, image): (f64, RgbImage)| {
        let size = image.dimensions();
        let mut pano = Pano::from_rgb(
            id,
            name.clone(),
            size.0 as usize,
            size.1 as usize,
            image.into_raw(),
        );
        pano.is_panorama = fisheye::is_equirectangular(size);
        (seconds, Arc::new(pano))
    };
    let first = video
        .next_frame()?
        .map(still)
        .ok_or("Il video non contiene fotogrammi leggibili.")?;
    if !send(Reply::Opened {
        duration: video.duration,
        first,
    }) {
        return Ok(());
    }
    while let Ok(command) = commands.recv() {
        let frame = match command {
            Request::Next => video.next_frame(),
            Request::Seek(seconds) => video.frame_at(seconds),
        }?;
        if !send(Reply::Frame(frame.map(still))) {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn check_worker_playback(path: &Path) -> Result<(), String> {
    fn wait_for_frame(player: &mut Playback, ctx: &egui::Context) -> Result<PanoRef, String> {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(5) {
            if let Some(frame) = player.poll(ctx)? {
                return Ok(frame);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        Err("Il worker non ha consegnato il fotogramma atteso.".into())
    }

    let ctx = egui::Context::default();
    let mut player = Playback::start(&ctx, path.to_path_buf(), 7);
    let first = wait_for_frame(&mut player, &ctx)?;
    assert_eq!(player.position, 0.0);
    assert!(player.clock.is_none());
    player.clock = Some((Instant::now(), player.position));
    let next = wait_for_frame(&mut player, &ctx)?;
    assert!(player.position > 0.0);
    assert_ne!(first.rgb, next.rgb);
    player.pause();
    player.seek(0.5);
    player.seek(1.4);
    let sought = wait_for_frame(&mut player, &ctx)?;
    assert!((player.position - 1.4).abs() < 0.06);
    assert!(player.clock.is_none() && sought.is_panorama);
    Ok(())
}
