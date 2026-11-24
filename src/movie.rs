//! Deterministic movie timeline, transitions and rendering.

use std::{
    collections::{hash_map::Entry, BTreeSet, HashMap},
    f64::consts::TAU,
    path::{Path, PathBuf},
};

use image::{Rgba, RgbaImage};

use crate::{
    autoedit::{self, Segment, VideoOptions},
    online::{self, Credit},
    overlay::{self, Overlays},
    panorama::{self, preset, Adjust, Pano, Region, View},
    playback::{self, PanoramaVideo},
    templates::{Slot, TemplateKind},
    video::{self, AudioReader, PendingOutput, VideoWriter, AUDIO_BLOCK, AUDIO_HZ},
};

const MIN_SPEED: f64 = 0.25;
const MAX_SPEED: f64 = 4.0;
const RAMP_AMOUNT: f64 = 0.34;
const CURVE_AMOUNT: f64 = 0.28;
const TRANSITION_SECONDS: f64 = 0.36;
const MAX_OVERLAP_FRACTION: f64 = 0.25;
const MIN_CLIP_SECONDS: f64 = 0.25;
const DURATION_SEARCH_STEPS: usize = 32;
const BEAT_WINDOW: usize = 480;
const BEAT_HISTORY: usize = 16;
const BEAT_MIN_ONSET: f64 = 0.012;
const BEAT_ONSET_GAIN: f64 = 2.4;
const BEAT_MIN_SPACING: f64 = 0.24;
const BEAT_SNAP_DISTANCE: f64 = 0.24;
const BLUR_DISTANCE: f32 = 0.13;
const BLUR_ZOOM: f32 = 0.46;
const BLUR_TAPS: [f32; 3] = [-1.0, 0.0, 1.0];
const SLICE_BLOCKS: u32 = 4;
const BLUR_SECOND_RIGHT_GAIN: f32 = 1.6;
const BLUR_SCROLL_GAIN: f32 = 1.5;
const BLUR_PIVOT_ANGLE: f32 = 0.28;
const BLUR_SPIN_ANGLE: f32 = 1.0;
const BLUR_ROTATION_SPREAD: f32 = 0.26;
const AUDIO_FADE_SECONDS: f64 = 0.8;
const EXPORT_PROGRESS_FINALIZE: u8 = 99;
const MAX_RENDER_SOURCE_WIDTH: u32 = 4096;
const BEAT_SEARCH_TOLERANCE: f64 = 0.0001;
const BEAT_REPORT_TOLERANCE: f64 = 0.001;
pub const MIN_PLAYBACK_SPEED: f64 = MIN_SPEED;
pub const MAX_PLAYBACK_SPEED: f64 = MAX_SPEED;
pub const FPS: u32 = 30;
pub const RESOLUTIONS: [&str; 4] = ["720p", "1080p", "2.7K", "4K"];
pub const ASPECTS: [&str; 2] = ["16:9", "9:16"];
const OUTPUT_SIZES: [(u32, u32); 4] = [(1280, 720), (1920, 1080), (2720, 1530), (3840, 2160)];
const OUTPUT_BITRATES: [u32; 4] = [6_000_000, 12_000_000, 22_000_000, 40_000_000];
const TARGET_SECONDS: [f64; 5] = [30.0, 60.0, 120.0, 180.0, 210.0];

macro_rules! choices {
    ($name:ident { $($variant:ident => $label:literal),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum $name { $($variant),+ }
        impl $name {
            pub const ALL: [Self; Self::LABELS.len()] = [$(Self::$variant),+];
            pub const LABELS: &[&str] = &[$($label),+];
            pub fn label(self) -> &'static str { Self::LABELS[self as usize] }
        }
    };
}

choices!(Transition {
    None => "Nessuna", Fade => "Dissolvenza in uscita", BlurRight2 => "Sfocatura a destra 2",
    BlurPivot => "Sfocatura a perno", BlurSpin => "Sfocatura rotante",
    BlurRight => "Sfocatura a destra", BlurScroll => "Sfocatura a scorrimento",
    RightSwipe => "Passaggio a destra", BlurZoom => "Zoom sfocato",
    BlurZoom2 => "Zoom sfocato 2", Slice => "Fette (4 blocchi)",
    SlideDown => "Scorri in basso", SlideLeft => "Scorri a sinistra",
    SlideRight => "Scorri a destra", SlideUp => "Scorri in alto",
});

choices!(Template {
    Dynamic => "Dinamico", TravelingDay => "Giornata in viaggio", InSync => "A tempo",
    Energy => "Energia", RhythmOfLife => "Ritmo della vita", AdventureDay => "Giornata d’avventura",
    Pulse => "Battito", Adventures => "Avventure",
    UrbanPulse => "Impulso urbano", Postcards => "Cartoline in corsa",
    Momentum => "Slancio", LittleThings => "Piccole cose", LastLight => "Ultima luce",
    StreetJournal => "Cronaca di strada", BeyondThePeak => "Oltre la vetta",
    KickDrum => "Colpo di cassa",
});

choices!(TargetDuration {
    UpTo30 => "0 s–30 s", UpTo60 => "30 s–60 s", UpTo120 => "1 min–2 min",
    UpTo180 => "2 min–3 min", Over180 => "3 min+",
});

impl TargetDuration {
    pub fn seconds(self) -> f64 {
        TARGET_SECONDS[self as usize]
    }
}

choices!(Curve {
    Ramp => "Rampa", SlowCenter => "Rallenta al centro", Accelerate => "Accelera",
});

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Speed {
    Constant(f64),
    Variable { rate: f64, curve: Curve },
}

impl Speed {
    pub fn rate(self) -> f64 {
        let (Self::Constant(rate) | Self::Variable { rate, .. }) = self;
        rate.clamp(MIN_SPEED, MAX_SPEED)
    }

    pub fn output_duration(self, span: f64) -> f64 {
        span / self.rate()
    }

    pub fn source_offset(self, output_time: f64, span: f64) -> f64 {
        let duration = self.output_duration(span);
        if duration <= 0.0 {
            return 0.0;
        }
        let u = (output_time / duration).clamp(0.0, 1.0);
        let fraction = match self {
            Self::Constant(_) => u,
            Self::Variable {
                curve: Curve::Ramp, ..
            } => u + RAMP_AMOUNT * u * (1.0 - u) * (2.0 * u - 1.0),
            Self::Variable {
                curve: Curve::SlowCenter,
                ..
            } => u + CURVE_AMOUNT * (TAU * u).sin() / TAU,
            Self::Variable {
                curve: Curve::Accelerate,
                ..
            } => u - RAMP_AMOUNT * u * (1.0 - u),
        };
        fraction.clamp(0.0, 1.0) * span
    }

    pub fn has_original_audio(self) -> bool {
        matches!(self, Self::Constant(rate) if rate == 1.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Filter(pub usize);

#[derive(Clone, Debug)]
pub struct Clip {
    pub source: PathBuf,
    pub start: f64,
    pub end: f64,
    pub view: View,
    pub end_view: View,
    pub speed: Speed,
    pub transition_out: Transition,
    pub filter: Filter,
    pub segment_index: usize,
    limit_end: f64,
    slot_source: Option<SlotSource>,
    saved_filter: Option<Filter>,
}

#[derive(Clone, Copy, Debug)]
struct SlotSource {
    start: f64,
    start_view: View,
    end_view: View,
}

impl Clip {
    fn from_segment(
        segment: &Segment,
        index: usize,
        profile: &TemplateProfile,
        position: usize,
    ) -> Self {
        let mut clip = Self::source_clip(segment, index);
        clip.speed = Speed::Constant(profile.speed);
        clip.transition_out = profile.transitions[position % profile.transitions.len()];
        clip.filter = profile.filter;
        clip
    }

    fn source_clip(segment: &Segment, index: usize) -> Self {
        Self {
            source: segment.source.clone(),
            start: segment.start,
            end: segment.end,
            view: segment.start_view,
            end_view: segment.end_view,
            speed: Speed::Constant(1.0),
            transition_out: Transition::None,
            filter: Filter(0),
            segment_index: index,
            limit_end: segment.end,
            slot_source: None,
            saved_filter: None,
        }
    }

    fn from_slot(segment: &Segment, index: usize, slot: Slot, filter: Filter) -> Self {
        let mut clip = Self::source_clip(segment, index);
        clip.speed = Speed::Constant(slot.speed);
        clip.transition_out = slot.transition;
        clip.filter = filter;
        clip.slot_source = Some(SlotSource {
            start: segment.start,
            start_view: segment.start_view,
            end_view: segment.end_view,
        });
        clip
    }

    fn set_slot_span(&mut self, preferred_start: f64, span: f64) {
        let source = self.slot_source.expect("Sync clip has a source interval");
        let length = self.limit_end - source.start;
        let latest_start = (self.limit_end - span).max(source.start);
        let start = preferred_start.clamp(source.start, latest_start);
        let end = (start + span).min(self.limit_end);
        let view_at = |time: f64| {
            if time == source.start {
                return source.start_view;
            }
            if time == self.limit_end {
                return source.end_view;
            }
            panorama::interpolate_view(
                source.start_view,
                source.end_view,
                ((time - source.start) / length) as f32,
            )
        };
        // Preserve manual reframing while updating the automatically interpolated views.
        if self.view == view_at(self.start) {
            self.view = view_at(start);
        }
        if self.end_view == view_at(self.end) {
            self.end_view = view_at(end);
        }
        self.start = start;
        self.end = end;
    }

    pub fn duration(&self) -> f64 {
        self.speed.output_duration(self.end - self.start)
    }
}

#[derive(Clone, Debug)]
pub struct Music {
    pub path: PathBuf,
    pub start: f64,
    pub duration: f64,
    pub credit: Option<Credit>,
}

#[derive(Clone, Debug)]
pub struct Movie {
    pub clips: Vec<Clip>,
    pub template: Template,
    pub target_duration: TargetDuration,
    pub music: Option<Music>,
    pub original_audio_volume: f64,
    pub music_volume: f64,
    pub filter: Filter,
    pub colore: Adjust,
    pub overlays: Overlays,
    slot_lengths: Vec<f64>,
}

struct TemplateProfile {
    pace: f64,
    transitions: &'static [Transition],
    filter: Filter,
    speed: f64,
}

use Transition::{
    BlurPivot, BlurRight, BlurRight2, BlurScroll, BlurSpin, BlurZoom, BlurZoom2, Fade, RightSwipe,
    Slice, SlideDown, SlideLeft, SlideRight, SlideUp,
};

const TEMPLATES: [TemplateProfile; 8] = [
    TemplateProfile {
        pace: 2.4,
        transitions: &[BlurRight2, SlideLeft, BlurZoom, Slice],
        filter: Filter(7),
        speed: 1.15,
    },
    TemplateProfile {
        pace: 4.2,
        transitions: &[SlideLeft, SlideRight, Fade],
        filter: Filter(4),
        speed: 1.0,
    },
    TemplateProfile {
        pace: 2.0,
        transitions: &[RightSwipe, Slice, SlideDown],
        filter: Filter(1),
        speed: 1.0,
    },
    TemplateProfile {
        pace: 1.8,
        transitions: &[BlurSpin, BlurZoom2, BlurRight],
        filter: Filter(7),
        speed: 1.35,
    },
    TemplateProfile {
        pace: 4.6,
        transitions: &[Fade, BlurPivot],
        filter: Filter(9),
        speed: 0.9,
    },
    TemplateProfile {
        pace: 3.4,
        transitions: &[SlideUp, BlurScroll, SlideLeft],
        filter: Filter(4),
        speed: 1.0,
    },
    TemplateProfile {
        pace: 1.6,
        transitions: &[Slice, BlurZoom, RightSwipe],
        filter: Filter(3),
        speed: 1.25,
    },
    TemplateProfile {
        pace: 3.8,
        transitions: &[BlurPivot, SlideDown, Fade],
        filter: Filter(12),
        speed: 1.05,
    },
];

fn overlap(a: &Clip, b: &Clip) -> f64 {
    if a.transition_out == Transition::None {
        return 0.0;
    }
    TRANSITION_SECONDS.min(MAX_OVERLAP_FRACTION * a.duration().min(b.duration()))
}

fn valid_segment(segment: &Segment) -> bool {
    segment.start.is_finite()
        && segment.end.is_finite()
        && segment.start >= 0.0
        && segment.end > segment.start
}

pub fn build_from_slots(segments: &[Segment], template: Template) -> Movie {
    let info = template.info();
    let TemplateKind::Sync { slots, filter, .. } = info.kind else {
        return build_free(segments, template, TargetDuration::UpTo30);
    };
    let mut order: Vec<_> = (0..segments.len())
        .filter(|&i| valid_segment(&segments[i]))
        .collect();
    order.sort_by(|&a, &b| {
        let (a, b) = (&segments[a], &segments[b]);
        b.score
            .cmp(&a.score)
            .then_with(|| a.source.cmp(&b.source))
            .then_with(|| a.start.total_cmp(&b.start))
    });
    let clips = slots
        .iter()
        .zip(order.into_iter().cycle())
        .map(|(&slot, index)| Clip::from_slot(&segments[index], index, slot, filter))
        .collect();
    let mut movie = Movie::with_clips(clips, template, TargetDuration::UpTo30);
    movie.slot_lengths = slots.iter().map(|slot| slot.seconds(info.bpm)).collect();
    fit_slots(&mut movie.clips, &movie.slot_lengths);

    // Spread repeated takes across the available interval, favoring the largest
    // distance from an already chosen start for the same highlight.
    let mut used_starts = HashMap::<usize, Vec<f64>>::new();
    for clip in &mut movie.clips {
        let starts = used_starts.entry(clip.segment_index).or_default();
        let span = clip.end - clip.start;
        let latest = (clip.limit_end - span).max(clip.start);
        if !starts.is_empty() && latest > clip.start {
            let mut bounds = vec![clip.start, latest];
            bounds.extend(starts.iter().map(|start| start.clamp(clip.start, latest)));
            bounds.sort_by(f64::total_cmp);
            let candidates = bounds
                .iter()
                .copied()
                .chain(bounds.windows(2).map(|pair| (pair[0] + pair[1]) * 0.5));
            let distance = |start: f64| {
                starts
                    .iter()
                    .map(|used| (start - used).abs())
                    .fold(f64::INFINITY, f64::min)
            };
            if let Some(start) = candidates.max_by(|&a, &b| distance(a).total_cmp(&distance(b))) {
                clip.set_slot_span(start, span);
            }
        }
        starts.push(clip.start);
    }
    movie
}

fn fit_slots(clips: &mut [Clip], lengths: &[f64]) {
    let mut next_duration = 0.0;
    for (clip, &length) in clips.iter_mut().zip(lengths).rev() {
        // Solve overlap <= fraction * (length + overlap) before sizing this clip.
        let shared = if clip.transition_out == Transition::None {
            0.0
        } else {
            TRANSITION_SECONDS
                .min(MAX_OVERLAP_FRACTION * next_duration)
                .min(MAX_OVERLAP_FRACTION / (1.0 - MAX_OVERLAP_FRACTION) * length)
        };
        clip.set_slot_span(clip.start, (length + shared) * clip.speed.rate());
        next_duration = clip.duration();
    }
}

pub fn build_from_template(
    segments: &[Segment],
    template: Template,
    duration: TargetDuration,
) -> Movie {
    match template.info().kind {
        TemplateKind::Free => build_free(segments, template, duration),
        TemplateKind::Sync { .. } => build_from_slots(segments, template),
    }
}

fn build_free(segments: &[Segment], template: Template, duration: TargetDuration) -> Movie {
    let profile = &TEMPLATES[template as usize];
    let mut order: Vec<_> = (0..segments.len())
        .filter(|&index| valid_segment(&segments[index]))
        .collect();
    order.sort_by(|&a, &b| segments[b].score.cmp(&segments[a].score).then(a.cmp(&b)));
    let target = duration.seconds();
    let mut selected = Vec::new();
    let mut estimated_length = 0.0;
    for index in order {
        let mut clip = Clip::from_segment(&segments[index], index, profile, selected.len());
        clip.end = clip.end.min(clip.start + profile.pace * profile.speed);
        let estimate = clip.duration() - selected.last().map_or(0.0, |last| overlap(last, &clip));
        estimated_length += estimate;
        selected.push(clip);
        if estimated_length >= target {
            break;
        }
    }
    selected.sort_by(|a, b| {
        a.source
            .cmp(&b.source)
            .then_with(|| a.start.total_cmp(&b.start))
    });
    if estimated_length < target && !selected.is_empty() {
        // Keep the model's pace where possible. When there are few highlights,
        // lengthen their cuts within the available source instead of repeating them.
        let length_at = |cap: f64| {
            let mut previous = None;
            selected
                .iter()
                .enumerate()
                .map(|(i, clip)| {
                    let length = clip
                        .speed
                        .output_duration(clip.limit_end - clip.start)
                        .min(cap);
                    let shared = previous.map_or(0.0_f64, |left: f64| {
                        if profile.transitions[(i - 1) % profile.transitions.len()]
                            == Transition::None
                        {
                            0.0
                        } else {
                            TRANSITION_SECONDS.min(MAX_OVERLAP_FRACTION * left.min(length))
                        }
                    });
                    previous = Some(length);
                    length - shared
                })
                .sum::<f64>()
        };
        let mut low = profile.pace;
        let mut high = selected
            .iter()
            .map(|clip| clip.speed.output_duration(clip.limit_end - clip.start))
            .fold(low, f64::max);
        if length_at(high) >= target {
            for _ in 0..DURATION_SEARCH_STEPS {
                let middle = (low + high) / 2.0;
                if length_at(middle) < target {
                    low = middle;
                } else {
                    high = middle;
                }
            }
        }
        for clip in &mut selected {
            clip.end = clip.limit_end.min(clip.start + high * clip.speed.rate());
        }
    }
    let mut length = 0.0;
    let mut count = 0;
    for i in 0..selected.len() {
        selected[i].transition_out = profile.transitions[i % profile.transitions.len()];
        let shared = if i == 0 {
            0.0
        } else {
            overlap(&selected[i - 1], &selected[i])
        };
        let available = selected[i].duration();
        let requested =
            (target - length + shared).clamp(MIN_CLIP_SECONDS.min(available), available);
        let clip = &mut selected[i];
        clip.end = (clip.start + requested * clip.speed.rate()).min(clip.limit_end);
        length += clip.duration()
            - if i == 0 {
                0.0
            } else {
                overlap(&selected[i - 1], &selected[i])
            };
        count += 1;
        if length >= target - MIN_CLIP_SECONDS {
            break;
        }
    }
    selected.truncate(count);
    for clip in &mut selected {
        let fraction = ((clip.end - clip.start) / (clip.limit_end - clip.start)) as f32;
        clip.end_view = panorama::interpolate_view(clip.view, clip.end_view, fraction);
    }
    Movie::with_clips(selected, template, duration)
}

impl Movie {
    fn with_clips(clips: Vec<Clip>, template: Template, target_duration: TargetDuration) -> Self {
        Self {
            clips,
            template,
            target_duration,
            music: None,
            original_audio_volume: 1.0,
            music_volume: 1.0,
            filter: Filter(0),
            colore: Adjust::default(),
            overlays: Overlays::default(),
            slot_lengths: Vec::new(),
        }
    }

    pub fn from_segment(segment: &Segment) -> Self {
        let mut movie = build_from_template(
            std::slice::from_ref(segment),
            Template::Dynamic,
            TargetDuration::UpTo30,
        );
        if let Some(clip) = movie.clips.first_mut() {
            clip.end = segment.end;
            clip.end_view = segment.end_view;
            clip.speed = Speed::Constant(1.0);
            clip.filter = Filter(0);
            clip.transition_out = Transition::None;
        }
        movie
    }

    pub fn replace(&mut self, selected: usize, segment: &Segment, segment_index: usize) {
        if !valid_segment(segment) {
            return;
        }
        let Some(old) = self.clips.get_mut(selected) else {
            return;
        };
        let mut new = if let TemplateKind::Sync { slots, .. } = self.template.info().kind {
            Clip::from_slot(segment, segment_index, slots[selected], old.filter)
        } else {
            let mut new = Clip::source_clip(segment, segment_index);
            new.speed = old.speed;
            new.end = new.end.min(new.start + old.duration() * new.speed.rate());
            let fraction = ((new.end - new.start) / (new.limit_end - new.start)) as f32;
            new.end_view = panorama::interpolate_view(new.view, new.end_view, fraction);
            new.filter = old.filter;
            new.transition_out = old.transition_out;
            new
        };
        new.saved_filter = old.saved_filter;
        *old = new;
        if !self.slot_lengths.is_empty() {
            fit_slots(&mut self.clips, &self.slot_lengths);
        }
    }

    pub fn slot_duration(&self, index: usize) -> Option<f64> {
        self.slot_lengths.get(index).copied()
    }

    pub fn shortened_slots(&self) -> impl Iterator<Item = usize> + '_ {
        self.slot_lengths
            .iter()
            .zip(&self.clips)
            .enumerate()
            .filter_map(|(i, (&length, clip))| {
                let shared = self
                    .clips
                    .get(i + 1)
                    .map_or(0.0, |next| overlap(clip, next));
                (clip.duration() - shared < length - BEAT_REPORT_TOLERANCE).then_some(i)
            })
    }

    pub fn template_filter_enabled(&self) -> bool {
        self.clips
            .first()
            .is_none_or(|clip| clip.saved_filter.is_none())
    }

    pub fn set_template_filter(&mut self, enabled: bool) {
        for clip in &mut self.clips {
            if enabled {
                if let Some(filter) = clip.saved_filter.take() {
                    clip.filter = filter;
                }
            } else if clip.saved_filter.is_none() {
                clip.saved_filter = Some(clip.filter);
                clip.filter = Filter(0);
            }
        }
    }

    pub fn timeline(&self) -> Timeline {
        let mut end = 0.0;
        let starts = self
            .clips
            .iter()
            .enumerate()
            .map(|(i, clip)| {
                if i > 0 {
                    end -= overlap(&self.clips[i - 1], clip);
                }
                let start = end;
                end += clip.duration();
                start
            })
            .collect();
        Timeline { starts, end }
    }

    pub fn duration(&self) -> f64 {
        self.timeline().end
    }

    pub fn sync_to_beats(&mut self, beats: &[f64]) -> usize {
        if !self.slot_lengths.is_empty() {
            return self.sync_slot_cuts(beats);
        }
        let mut changed = 0;
        let mut start = 0.0;
        for i in 0..self.clips.len().saturating_sub(1) {
            let next = &self.clips[i + 1];
            let clip = &self.clips[i];
            let cut = start + clip.duration() - overlap(clip, next);
            let mut revised = clip.clone();
            if let Some(beat) = nearby_beat(beats, cut) {
                // A short clip may contribute to the overlap. Solve for the new cut itself.
                let min_end =
                    (clip.start + MIN_CLIP_SECONDS * clip.speed.rate()).min(clip.limit_end);
                let cut_at = |end| {
                    let mut candidate = clip.clone();
                    candidate.end = end;
                    start + candidate.duration() - overlap(&candidate, next)
                };
                if (cut_at(min_end)..=cut_at(clip.limit_end)).contains(&beat) {
                    let (mut lo, mut hi) = (min_end, clip.limit_end);
                    while hi - lo > BEAT_SEARCH_TOLERANCE {
                        let mid = (lo + hi) / 2.0;
                        if cut_at(mid) < beat {
                            lo = mid;
                        } else {
                            hi = mid;
                        }
                    }
                    revised.end = (lo + hi) / 2.0;
                    changed += usize::from((revised.end - clip.end).abs() > BEAT_REPORT_TOLERANCE);
                }
            }
            start += revised.duration() - overlap(&revised, next);
            self.clips[i] = revised;
        }
        changed
    }

    fn sync_slot_cuts(&mut self, beats: &[f64]) -> usize {
        let mut changed = 0;
        let mut timeline = self.timeline();
        for i in 1..self.clips.len() {
            let start = timeline.starts[i];
            let Some(beat) = nearby_beat(beats, start) else {
                continue;
            };
            let delta = beat - start;
            if delta.abs() <= BEAT_REPORT_TOLERANCE
                || self.slot_lengths[i - 1] + delta < MIN_CLIP_SECONDS
                || self.slot_lengths[i] - delta < MIN_CLIP_SECONDS
            {
                continue;
            }
            let mut candidate = self.clone();
            candidate.slot_lengths[i - 1] += delta;
            candidate.slot_lengths[i] -= delta;
            fit_slots(&mut candidate.clips, &candidate.slot_lengths);
            let revised = candidate.timeline();
            let fits = (revised.end - timeline.end).abs() <= BEAT_REPORT_TOLERANCE
                && revised.starts.iter().zip(&timeline.starts).enumerate().all(
                    |(j, (new, old))| {
                        let desired = if i == j { beat } else { *old };
                        (*new - desired).abs() <= BEAT_REPORT_TOLERANCE
                    },
                );
            if fits {
                self.clips = candidate.clips;
                self.slot_lengths = candidate.slot_lengths;
                timeline = revised;
                changed += 1;
            }
        }
        changed
    }
}

fn nearby_beat(beats: &[f64], cut: f64) -> Option<f64> {
    let after = beats.partition_point(|beat| *beat < cut);
    [
        after.checked_sub(1).and_then(|index| beats.get(index)),
        beats.get(after),
    ]
    .into_iter()
    .flatten()
    .copied()
    .min_by(|a, b| (*a - cut).abs().total_cmp(&(*b - cut).abs()))
    .filter(|beat| (*beat - cut).abs() <= BEAT_SNAP_DISTANCE)
}

pub struct Timeline {
    pub starts: Vec<f64>,
    pub end: f64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct OutputProfile {
    pub resolution: usize,
    pub aspect: usize,
}

impl OutputProfile {
    pub fn dimensions(self) -> (u32, u32) {
        let (w, h) = OUTPUT_SIZES[self.resolution.min(OUTPUT_SIZES.len() - 1)];
        if self.aspect == 1 {
            (h, w)
        } else {
            (w, h)
        }
    }

    fn bitrate(self) -> u32 {
        OUTPUT_BITRATES[self.resolution.min(OUTPUT_BITRATES.len() - 1)]
    }
}

pub fn transition(kind: Transition, a: &RgbaImage, b: &RgbaImage, t: f32, out: &mut RgbaImage) {
    let (w, h) = a.dimensions();
    if b.dimensions() != (w, h) || !t.is_finite() || t <= 0.0 || kind == Transition::None && t < 1.0
    {
        out.clone_from(a);
        return;
    }
    if t >= 1.0 {
        out.clone_from(b);
        return;
    }
    if out.dimensions() != (w, h) {
        *out = RgbaImage::new(w, h);
    }
    let (wf, hf) = (w as f32, h as f32);
    let (cx, cy) = ((wf - 1.0) * 0.5, (hf - 1.0) * 0.5);
    let strength = (std::f32::consts::PI * t).sin();
    let rotation = match kind {
        BlurPivot => BLUR_PIVOT_ANGLE,
        BlurSpin => BLUR_SPIN_ANGLE,
        _ => 0.0,
    };
    let rotations = BLUR_TAPS
        .map(|tap| (rotation * (strength + tap * strength * BLUR_ROTATION_SPREAD)).sin_cos());
    let lookup = |image: &RgbaImage, x: f32, y: f32| {
        *image.get_pixel(
            x.round().clamp(0.0, wf - 1.0) as u32,
            y.round().clamp(0.0, hf - 1.0) as u32,
        )
    };
    for (x, y, pixel) in out.enumerate_pixels_mut() {
        let (xf, yf) = (x as f32, y as f32);
        *pixel = match kind {
            Transition::None => *a.get_pixel(x, y),
            Fade => {
                let (frame, gain) = if t < 0.5 {
                    (a, 1.0 - 2.0 * t)
                } else {
                    (b, 2.0 * t - 1.0)
                };
                let p = frame.get_pixel(x, y);
                Rgba([
                    (f32::from(p[0]) * gain).round() as u8,
                    (f32::from(p[1]) * gain).round() as u8,
                    (f32::from(p[2]) * gain).round() as u8,
                    p[3],
                ])
            }
            RightSwipe => {
                if xf < wf * t {
                    *b.get_pixel(x, y)
                } else {
                    *a.get_pixel(x, y)
                }
            }
            SlideRight | SlideLeft | SlideDown | SlideUp | Slice => {
                let (dx, dy) = match kind {
                    SlideRight => (wf, 0.0),
                    SlideLeft => (-wf, 0.0),
                    SlideDown => (0.0, hf),
                    SlideUp => (0.0, -hf),
                    _ => {
                        let block = u64::from(y) * u64::from(SLICE_BLOCKS) / u64::from(h);
                        (if block % 2 == 0 { wf } else { -wf }, 0.0)
                    }
                };
                let (sx, sy) = (xf - dx * t, yf - dy * t);
                if (0.0..wf).contains(&sx) && (0.0..hf).contains(&sy) {
                    lookup(a, sx, sy)
                } else {
                    lookup(b, sx + dx, sy + dy)
                }
            }
            _ => {
                let mut channels = [0.0_f32; 4];
                for (i, tap) in BLUR_TAPS.into_iter().enumerate() {
                    let (mut ax, mut ay, mut bx, mut by) = (xf, yf, xf, yf);
                    let blur = BLUR_DISTANCE * strength * tap;
                    match kind {
                        BlurRight => {
                            ax += (blur - BLUR_DISTANCE * strength) * wf;
                            bx += blur * wf;
                        }
                        BlurRight2 => {
                            ax -= blur * wf * BLUR_SECOND_RIGHT_GAIN;
                            bx += (blur + BLUR_DISTANCE * strength) * wf;
                        }
                        BlurScroll => {
                            ay += blur * hf * BLUR_SCROLL_GAIN;
                            by -= (blur + BLUR_DISTANCE * strength) * hf;
                        }
                        BlurZoom | BlurZoom2 => {
                            let direction = if kind == BlurZoom { 1.0 } else { -1.0 };
                            let zoom = direction * BLUR_ZOOM * strength;
                            let (dx, dy) = (xf - cx, yf - cy);
                            let (sa, sb) = (1.0 - zoom + blur, 1.0 + zoom + blur);
                            (ax, ay, bx, by) =
                                (cx + dx * sa, cy + dy * sa, cx + dx * sb, cy + dy * sb);
                        }
                        _ => {
                            let (sin, cos) = rotations[i];
                            let (ox, oy) = if kind == BlurPivot {
                                (0.0, cy)
                            } else {
                                (cx, cy)
                            };
                            let (dx, dy) = (xf - ox, yf - oy);
                            (ax, ay, bx, by) = (
                                ox + cos * dx - sin * dy,
                                oy + sin * dx + cos * dy,
                                ox + cos * dx + sin * dy,
                                oy - sin * dx + cos * dy,
                            );
                        }
                    }
                    let (pa, pb) = (lookup(a, ax, ay), lookup(b, bx, by));
                    for c in 0..4 {
                        channels[c] += f32::from(pa[c]) * (1.0 - t) + f32::from(pb[c]) * t;
                    }
                }
                Rgba(channels.map(|sum| (sum / BLUR_TAPS.len() as f32).round() as u8))
            }
        };
    }
}

#[cfg(test)]
fn detect_beats(pcm: &[[i16; 2]]) -> Vec<f64> {
    let mut detector = BeatDetector::default();
    detector.push(pcm);
    detector.finish()
}

#[derive(Default)]
struct BeatDetector {
    history: [f64; BEAT_HISTORY],
    energies: (f64, f64),
    window: usize,
    samples: usize,
    squares: f64,
    beats: Vec<f64>,
}

impl BeatDetector {
    fn push(&mut self, pcm: &[[i16; 2]]) {
        for &[l, r] in pcm {
            self.squares += (f64::from(l).powi(2) + f64::from(r).powi(2)) * 0.5;
            self.samples += 1;
            if self.samples == BEAT_WINDOW {
                let rms = (self.squares / BEAT_WINDOW as f64).sqrt() / 32768.0;
                self.advance(rms);
                self.samples = 0;
                self.squares = 0.0;
            }
        }
    }

    fn advance(&mut self, rms: f64) {
        let (previous_rms, previous_onset) = self.energies;
        let onset = (rms - previous_rms).max(0.0);
        if self.window > 0 {
            let threshold = BEAT_MIN_ONSET
                .max(self.history.iter().sum::<f64>() / BEAT_HISTORY as f64 * BEAT_ONSET_GAIN);
            let position = ((self.window - 1) * BEAT_WINDOW) as f64 / f64::from(AUDIO_HZ);
            if previous_onset >= threshold
                && previous_onset > onset
                && self
                    .beats
                    .last()
                    .is_none_or(|last| position - *last >= BEAT_MIN_SPACING)
            {
                self.beats.push(position);
            }
            self.history[(self.window - 1) % BEAT_HISTORY] = previous_onset;
        }
        self.energies = (rms, onset);
        self.window += 1;
    }

    fn finish(mut self) -> Vec<f64> {
        if self.samples > 0 {
            self.advance((self.squares / self.samples as f64).sqrt() / 32768.0);
        }
        self.advance(0.0);
        self.beats
    }
}

pub fn mix_sample(
    original: [i16; 2],
    music: [i16; 2],
    original_gain: f64,
    music_gain: f64,
) -> [i16; 2] {
    std::array::from_fn(|c| {
        (f64::from(original[c]) * original_gain + f64::from(music[c]) * music_gain)
            .round()
            .clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16
    })
}

impl Music {
    pub fn open(path: PathBuf) -> Result<Self, String> {
        let reader = AudioReader::open(&path)
            .map_err(|error| error.to_string())?
            .ok_or("Il file selezionato non contiene audio.")?;
        Ok(Self {
            path,
            start: 0.0,
            duration: reader.duration,
            credit: None,
        })
    }

    pub fn beats(&self, seconds: f64) -> Result<Vec<f64>, String> {
        let mut reader = AudioReader::open(&self.path)
            .map_err(|error| error.to_string())?
            .ok_or("Il file selezionato non contiene audio.")?;
        let mut detector = BeatDetector::default();
        let mut buffer = [[0, 0]; AUDIO_BLOCK];
        let mut offset = 0;
        let start = audio_frame(self.start);
        let length = audio_frame(seconds + BEAT_SNAP_DISTANCE);
        while offset < length {
            let remaining = (length - offset).min(AUDIO_BLOCK as u64) as usize;
            let read = reader
                .read_at(start + offset, &mut buffer[..remaining])
                .map_err(|e| e.to_string())?;
            if read == 0 {
                break;
            }
            detector.push(&buffer[..read]);
            offset += read as u64;
        }
        Ok(detector.finish())
    }
}

pub fn analyze_source(
    path: &Path,
    progress: impl FnMut(u8),
    cancelled: impl Fn() -> bool,
) -> Result<Vec<Segment>, String> {
    let mut source = PanoramaVideo::open(path, autoedit::VIDEO_ANALYSIS_WIDTH)?;
    autoedit::analyze_video(
        &mut source,
        VideoOptions {
            source: path,
            cancelled: &cancelled,
        },
        progress,
    )
}

#[derive(Default)]
pub struct MovieRenderer {
    readers: HashMap<usize, PanoramaVideo>,
}

impl MovieRenderer {
    pub fn frame(
        &mut self,
        movie: &Movie,
        timeline: &Timeline,
        time: f64,
        size: (u32, u32),
        watermark: bool,
    ) -> Result<RgbaImage, String> {
        let i = timeline
            .starts
            .partition_point(|&start| start <= time)
            .saturating_sub(1);
        let previous = i.checked_sub(1).filter(|&previous| {
            movie.clips[previous].transition_out != Transition::None
                && time < timeline.starts[previous] + movie.clips[previous].duration()
        });
        self.readers
            .retain(|&index, _| index == i || Some(index) == previous);
        let current = self.clip_frame(movie, timeline, i, time, size)?;
        let mut result = if let Some(previous) = previous {
            let a = self.clip_frame(movie, timeline, previous, time, size)?;
            let seconds = overlap(&movie.clips[previous], &movie.clips[i]);
            let mut blended = RgbaImage::default();
            transition(
                movie.clips[previous].transition_out,
                &a,
                &current,
                ((time - timeline.starts[i]) / seconds) as f32,
                &mut blended,
            );
            blended
        } else {
            current
        };
        if watermark {
            crate::export::draw_watermark_rgba(&mut result);
        }
        Ok(result)
    }

    fn clip_frame(
        &mut self,
        movie: &Movie,
        timeline: &Timeline,
        i: usize,
        time: f64,
        size: (u32, u32),
    ) -> Result<RgbaImage, String> {
        let clip = movie.clips.get(i).ok_or("Il filmato non contiene clip.")?;
        let source_width = size
            .0
            .max(size.1)
            .saturating_mul(2)
            .min(MAX_RENDER_SOURCE_WIDTH);
        let reader = match self.readers.entry(i) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => entry.insert(PanoramaVideo::open(&clip.source, source_width)?),
        };
        let span = clip.end - clip.start;
        let offset = clip.speed.source_offset(time - timeline.starts[i], span);
        let at = (clip.start + offset).min((clip.end - 1.0 / reader.fps).max(clip.start));
        let (_, image) = reader
            .sample_at(at)?
            .ok_or("Il video non contiene fotogrammi leggibili.")?;
        let mut pano = Pano::from_rgb(
            0,
            String::new(),
            image.width() as usize,
            image.height() as usize,
            image.as_raw().clone(),
        );
        pano.is_panorama = reader.is_panorama();
        let fraction = (offset / span) as f32;
        let smooth = fraction * fraction * (3.0 - 2.0 * fraction);
        let mut view = panorama::interpolate_view(clip.view, clip.end_view, smooth);
        if !pano.is_panorama {
            // Flat footage uses the centered crop for the selected aspect ratio.
            view = View::default();
        }
        let adjust = movie
            .colore
            .combined(&preset(movie.filter.0))
            .combined(&preset(clip.filter.0));
        let rgb = panorama::render(
            &pano,
            &view,
            &adjust,
            (size.0 as usize, size.1 as usize),
            Region::crop(size.0 as f32 / size.1 as f32),
            None,
        );
        let mut rgba = RgbaImage::new(size.0, size.1);
        for (out, rgb) in rgba.chunks_exact_mut(4).zip(rgb.chunks_exact(3)) {
            out[..3].copy_from_slice(rgb);
            out[3] = 255;
        }
        overlay::composite(
            rgba.as_mut(),
            (size.0 as usize, size.1 as usize),
            4,
            &movie.overlays,
            at,
        );
        Ok(rgba)
    }
}

fn audio_frame(seconds: f64) -> u64 {
    (seconds * f64::from(AUDIO_HZ)).round() as u64
}
fn time_at_frame(frame: u64) -> f64 {
    frame as f64 / f64::from(AUDIO_HZ)
}

struct AudioTrack {
    reader: AudioReader,
    exhausted: bool,
}
impl AudioTrack {
    fn read(&mut self, start: u64, pcm: &mut [[i16; 2]]) -> Result<(), String> {
        if self.exhausted {
            pcm.fill([0, 0]);
            return Ok(());
        }
        let frames = self.reader.read_at(start, pcm).map_err(|e| e.to_string())?;
        self.exhausted = frames < pcm.len();
        Ok(())
    }
}

struct MovieAudio {
    original_sources: BTreeSet<PathBuf>,
    tracks: HashMap<usize, AudioTrack>,
    music: Option<AudioTrack>,
    scratch: Vec<[i16; 2]>,
    original: Vec<[i16; 2]>,
    warnings: Vec<String>,
}

fn audio_source(path: &Path) -> PathBuf {
    playback::primary_lens(path).unwrap_or_else(|| path.to_path_buf())
}

impl MovieAudio {
    fn new(movie: &Movie) -> Self {
        let mut warnings = Vec::new();
        let original_sources = movie
            .clips
            .iter()
            .filter(|clip| movie.original_audio_volume > 0.0 && clip.speed.has_original_audio())
            .map(|clip| clip.source.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter(|path| match AudioReader::open(&audio_source(path)) {
                Ok(reader) => reader.is_some(),
                Err(error) => {
                    warnings.push(format!("Audio originale non disponibile: {error}"));
                    false
                }
            })
            .collect();
        let music = movie
            .music
            .as_ref()
            .filter(|_| movie.music_volume > 0.0)
            .and_then(|music| match AudioReader::open(&music.path) {
                Ok(Some(reader)) => Some(AudioTrack {
                    reader,
                    exhausted: false,
                }),
                Ok(None) => {
                    warnings.push("Il file musicale non contiene audio.".into());
                    None
                }
                Err(error) => {
                    warnings.push(format!("Musica non disponibile: {error}"));
                    None
                }
            });
        Self {
            original_sources,
            tracks: HashMap::new(),
            music,
            warnings,
            scratch: Vec::new(),
            original: Vec::new(),
        }
    }

    fn enabled(&self) -> bool {
        self.music.is_some() || !self.original_sources.is_empty()
    }

    fn fill(&mut self, movie: &Movie, timeline: &Timeline, start: u64, out: &mut [[i16; 2]]) {
        let end = start + out.len() as u64;
        out.fill([0, 0]);
        self.scratch.resize(out.len(), [0, 0]);
        self.original.resize(out.len(), [0, 0]);
        self.original.fill([0, 0]);
        let first = timeline
            .starts
            .partition_point(|&t| audio_frame(t) <= start)
            .saturating_sub(2);
        let last = timeline.starts.partition_point(|&t| audio_frame(t) < end);
        self.tracks
            .retain(|&index, _| (first..last).contains(&index));
        for i in first..last {
            let clip = &movie.clips[i];
            let start_time = timeline.starts[i];
            let first_frame = audio_frame(start_time);
            let clip_end = start_time + clip.duration();
            let from = first_frame.max(start);
            let to = audio_frame(clip_end).min(end);
            if from >= to
                || !clip.speed.has_original_audio()
                || !self.original_sources.contains(&clip.source)
            {
                continue;
            }
            match self.tracks.entry(i) {
                Entry::Occupied(_) => {}
                Entry::Vacant(entry) => match AudioReader::open(&audio_source(&clip.source)) {
                    Ok(Some(reader)) => {
                        entry.insert(AudioTrack {
                            reader,
                            exhausted: false,
                        });
                    }
                    Ok(None) => continue,
                    Err(error) => {
                        self.warnings
                            .push(format!("Audio originale interrotto: {error}"));
                        self.original_sources.remove(&clip.source);
                        continue;
                    }
                },
            }
            let offset = (from - start) as usize;
            let count = (to - from) as usize;
            let Some(track) = self.tracks.get_mut(&i) else {
                continue;
            };
            let scratch = &mut self.scratch[..count];
            if let Err(error) = track.read(audio_frame(clip.start) + from - first_frame, scratch) {
                self.warnings
                    .push(format!("Audio originale interrotto: {error}"));
                self.original_sources.remove(&clip.source);
                continue;
            }
            let fade_in = i
                .checked_sub(1)
                .map_or(0.0, |p| overlap(&movie.clips[p], clip));
            let fade_out = movie
                .clips
                .get(i + 1)
                .map_or(0.0, |next| overlap(clip, next));
            for (j, (output, &sample)) in self.original[offset..offset + count]
                .iter_mut()
                .zip(scratch.iter())
                .enumerate()
            {
                let time = time_at_frame(from + j as u64);
                let gain_in = if fade_in > 0.0 {
                    ((time - start_time) / fade_in).clamp(0.0, 1.0)
                } else {
                    1.0
                };
                let gain_out = if fade_out > 0.0 {
                    ((clip_end - time) / fade_out).clamp(0.0, 1.0)
                } else {
                    1.0
                };
                *output = mix_sample(*output, sample, 1.0, gain_in * gain_out);
            }
        }
        if let (Some(music), Some(track)) = (&movie.music, &mut self.music) {
            if let Err(error) = track.read(audio_frame(music.start) + start, out) {
                self.warnings.push(format!("Musica interrotta: {error}"));
                self.music = None;
                out.fill([0, 0]);
            }
        }
        for (i, (out, &original)) in out.iter_mut().zip(&self.original).enumerate() {
            let time = time_at_frame(start + i as u64);
            let fade = ((timeline.end - time) / AUDIO_FADE_SECONDS).clamp(0.0, 1.0);
            *out = mix_sample(
                original,
                *out,
                movie.original_audio_volume * fade,
                movie.music_volume * fade,
            );
        }
    }
}

pub struct ExportResult {
    pub warnings: Vec<String>,
}

enum RenderError {
    Video(String),
    Audio(String),
}

pub fn render_movie(
    movie: &Movie,
    destination: &Path,
    profile: OutputProfile,
    watermark: bool,
    mut progress: impl FnMut(u8),
    cancelled: impl Fn() -> bool,
) -> Result<ExportResult, String> {
    if cancelled() {
        return Err("Esportazione annullata.".into());
    }
    if movie.clips.is_empty() {
        return Err("Il filmato non contiene clip.".into());
    }
    if movie.clips.iter().any(|clip| {
        video::same_file(&clip.source, destination)
            || video::same_file(&audio_source(&clip.source), destination)
            || playback::secondary_lens(&clip.source)
                .is_some_and(|lens| video::same_file(&lens, destination))
    }) || movie
        .music
        .as_ref()
        .is_some_and(|music| video::same_file(&music.path, destination))
    {
        return Err("Scegli un file diverso dai file sorgente del filmato.".into());
    }
    let timeline = movie.timeline();
    let frames = (timeline.end * f64::from(FPS)).ceil() as u64;
    if frames == 0 {
        return Err("Il filmato ha durata zero.".into());
    }
    let (width, height) = profile.dimensions();
    let mut audio = MovieAudio::new(movie);
    let with_audio = audio.enabled();
    let music_credit = audio
        .music
        .as_ref()
        .and(movie.music.as_ref().and_then(|music| music.credit.as_ref()));
    let mut warnings = std::mem::take(&mut audio.warnings);
    let mut render = |audio: Option<&mut MovieAudio>| -> Result<(), RenderError> {
        let mut audio = audio;
        let audio_enabled = audio.is_some();
        let setup_error = |error: video::VideoError| {
            if audio_enabled {
                RenderError::Audio(error.to_string())
            } else {
                RenderError::Video(error.to_string())
            }
        };
        if cancelled() {
            return Err(RenderError::Video("Esportazione annullata.".into()));
        }
        let pending =
            PendingOutput::new(destination).map_err(|e| RenderError::Video(e.to_string()))?;
        let mut writer = if audio_enabled {
            VideoWriter::with_audio(&pending.path, width, height, FPS, profile.bitrate(), true)
        } else {
            VideoWriter::new(&pending.path, width, height, FPS, profile.bitrate())
        }
        .map_err(setup_error)?;
        let mut renderer = MovieRenderer::default();
        let mut pcm = vec![[0, 0]; (AUDIO_HZ / FPS) as usize];
        progress(0);
        for frame in 0..frames {
            if cancelled() {
                return Err(RenderError::Video("Esportazione annullata.".into()));
            }
            let image = renderer
                .frame(
                    movie,
                    &timeline,
                    frame as f64 / f64::from(FPS),
                    (width, height),
                    watermark,
                )
                .map_err(RenderError::Video)?;
            writer.write_frame(&image).map_err(setup_error)?;
            if let Some(audio) = &mut audio {
                audio.fill(
                    movie,
                    &timeline,
                    frame * u64::from(AUDIO_HZ / FPS),
                    &mut pcm,
                );
                writer
                    .write_audio(&pcm)
                    .map_err(|e| RenderError::Audio(e.to_string()))?;
            }
            progress(((frame + 1) * u64::from(EXPORT_PROGRESS_FINALIZE) / frames) as u8);
        }
        if cancelled() {
            return Err(RenderError::Video("Esportazione annullata.".into()));
        }
        writer.finish().map_err(setup_error)?;
        if cancelled() {
            return Err(RenderError::Video("Esportazione annullata.".into()));
        }
        pending
            .commit(destination)
            .map_err(|e| RenderError::Video(e.to_string()))?;
        progress(100);
        Ok(())
    };
    let mut credits = String::new();
    match render(with_audio.then_some(&mut audio)) {
        Ok(()) => {
            warnings.append(&mut audio.warnings);
            if let Some(credit) = music_credit {
                credits.push_str(&credit.credits());
            }
        }
        Err(RenderError::Video(error)) => return Err(error),
        Err(RenderError::Audio(error)) => {
            render(None).map_err(|error| match error {
                RenderError::Video(error) | RenderError::Audio(error) => error,
            })?;
            warnings.push(format!(
                "Filmato esportato senza audio. Codifica AAC non riuscita: {error}"
            ));
        }
    }
    if movie
        .clips
        .iter()
        .any(|clip| movie.overlays.uses_twemoji(clip.start, clip.end))
    {
        if !credits.is_empty() {
            credits.push('\n');
        }
        credits.push_str(online::TWEMOJI_CREDITS);
    }
    if !credits.is_empty() {
        if let Err(error) = online::write_credits(destination, &credits) {
            warnings.push(error);
        }
    }
    Ok(ExportResult { warnings })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{autoedit::Scene, panorama::View};
    use image::Rgba;
    use std::sync::Arc;

    fn highlight(start: f64, length: f64, score: u8) -> Segment {
        Segment {
            source: "source.mp4".into(),
            start,
            end: start + length,
            view: View::default(),
            start_view: View::default(),
            end_view: View::default(),
            score,
            scene: Scene::Nature,
            thumbnail: Arc::from([]),
        }
    }

    #[test]
    fn catalog_has_unique_names_and_complete_metadata() {
        assert!(Template::ALL.len() >= 16);
        assert_eq!(
            &Template::LABELS[..8],
            [
                "Dinamico",
                "Giornata in viaggio",
                "A tempo",
                "Energia",
                "Ritmo della vita",
                "Giornata d’avventura",
                "Battito",
                "Avventure"
            ],
        );
        let mut names = BTreeSet::new();
        for template in Template::ALL {
            let info = template.info();
            assert!(names.insert(template.label()));
            assert_ne!(info.category.label(), "In evidenza");
            assert!(!info.description.is_empty() && !info.music_query.is_empty());
            assert!(info.bpm > 0);
            assert_ne!(info.colors[0], info.colors[1]);
        }
    }

    #[test]
    fn sync_timeline_hits_bpm_grid_and_declared_duration_with_transitions() {
        let segments: Vec<_> = (0..8)
            .map(|i| highlight(f64::from(i) * 10.0, 8.0, 90))
            .collect();
        let mut sync_count = 0;
        for template in Template::ALL {
            let info = template.info();
            let TemplateKind::Sync { slots, seconds, .. } = info.kind else {
                continue;
            };
            sync_count += 1;
            let movie = build_from_template(&segments, template, TargetDuration::Over180);
            let timeline = movie.timeline();
            let beat = 60.0 / f64::from(info.bpm);
            assert_eq!(movie.clips.len(), slots.len(), "{template:?}");
            assert!(
                (timeline.end - seconds).abs() < 1.0 / f64::from(FPS),
                "{template:?}"
            );
            let ends = timeline.starts[1..].iter().copied().chain([timeline.end]);
            for ((start, end), slot) in timeline.starts.iter().zip(ends).zip(slots) {
                let beats = (end - start) / beat;
                assert!((beats - f64::from(slot.beats)).abs() < 1e-8, "{template:?}");
                assert!(
                    (end / beat - (end / beat).round()).abs() < 1e-8,
                    "{template:?}"
                );
            }
            assert_eq!(movie.shortened_slots().count(), 0);
            assert!(movie.clips.iter().map(Clip::duration).sum::<f64>() > timeline.end);
        }
        assert!(sync_count > 0);
    }

    #[test]
    fn sync_reuses_highlights_within_source_and_recovers_replaced_slot_duration() {
        let segments = [
            highlight(30.0, 9.0, 70),
            highlight(10.0, 9.0, 70),
            highlight(0.0, 0.2, 99),
        ];
        let mut movie = build_from_slots(&segments, Template::UrbanPulse);
        assert_eq!(movie.clips.len(), 6);
        assert_eq!(
            movie
                .clips
                .iter()
                .map(|clip| clip.segment_index)
                .collect::<Vec<_>>(),
            [2, 1, 0, 2, 1, 0]
        );
        assert_ne!(movie.clips[1].start, movie.clips[4].start);
        assert_eq!(movie.shortened_slots().collect::<Vec<_>>(), [0, 3]);
        for clip in &movie.clips {
            let segment = &segments[clip.segment_index];
            assert!(
                segment.start <= clip.start && clip.start < clip.end && clip.end <= segment.end
            );
        }
        assert!(build_from_slots(&[], Template::UrbanPulse).clips.is_empty());
        movie.replace(0, &segments[1], 1);
        movie.replace(3, &segments[0], 0);
        assert_eq!(movie.shortened_slots().count(), 0);
        assert!((movie.duration() - 15.0).abs() < 1e-8);
    }

    #[test]
    fn sync_music_moves_cuts_without_changing_the_total() {
        let segment = highlight(0.0, 8.0, 95);
        let mut movie = build_from_slots(std::slice::from_ref(&segment), Template::UrbanPulse);
        assert_eq!(movie.sync_to_beats(&[2.12]), 1);
        assert!((movie.timeline().starts[1] - 2.12).abs() < 1e-8);
        assert!((movie.duration() - 15.0).abs() < 1e-8);
        movie.replace(0, &segment, 0);
        assert!((movie.timeline().starts[1] - 2.12).abs() < 1e-8);
        assert_eq!(movie.sync_to_beats(&[2.12]), 0);
    }

    #[test]
    fn template_filter_toggle_restores_each_clip_even_after_replacement() {
        let segment = highlight(0.0, 9.0, 80);
        let mut movie = build_from_slots(std::slice::from_ref(&segment), Template::UrbanPulse);
        movie.clips[1].filter = Filter(12);
        movie.filter = Filter(4);
        let original: Vec<_> = movie.clips.iter().map(|clip| clip.filter).collect();
        assert!(movie.template_filter_enabled());
        movie.set_template_filter(false);
        movie.set_template_filter(false);
        movie.replace(1, &segment, 0);
        assert!(movie.clips.iter().all(|clip| clip.filter == Filter(0)));
        assert_eq!(movie.filter, Filter(4));
        movie.set_template_filter(true);
        assert_eq!(
            movie
                .clips
                .iter()
                .map(|clip| clip.filter)
                .collect::<Vec<_>>(),
            original
        );
    }

    #[test]
    fn template_fills_the_target_within_one_clip_and_stays_in_source() {
        let source = PathBuf::from("source.mp4");
        let segments: Vec<_> = (0..20)
            .map(|i| Segment {
                source: source.clone(),
                start: f64::from(i) * 7.0,
                end: f64::from(i) * 7.0 + 6.0,
                view: View::default(),
                start_view: View::default(),
                end_view: View::default(),
                score: 95 - i as u8,
                scene: Scene::Nature,
                thumbnail: Vec::new().into(),
            })
            .collect();
        let movie = build_from_template(&segments, Template::Dynamic, TargetDuration::UpTo30);
        assert!(
            (movie.duration() - 30.0).abs() <= 3.0,
            "{}",
            movie.duration()
        );
        assert!(movie.clips.iter().all(|clip| segments
            .iter()
            .any(|segment| clip.start >= segment.start && clip.end <= segment.end)));
        assert!(movie
            .clips
            .windows(2)
            .all(|pair| pair[0].end <= pair[1].start));
        // Six intervals cannot hit the target at the default pace. Their full
        // spans are long enough, so the builder must spend the available time.
        let longer_cuts =
            build_from_template(&segments[..7], Template::Dynamic, TargetDuration::UpTo30);
        assert!(
            (longer_cuts.duration() - 30.0).abs() < 0.3,
            "{}",
            longer_cuts.duration()
        );
        assert!(longer_cuts
            .clips
            .iter()
            .all(|clip| clip.end <= segments[clip.segment_index].end));
    }

    #[test]
    fn transitions_preserve_endpoints_even_on_tiny_frames() {
        for (w, h) in [(0, 0), (0, 4), (4, 0), (1, 1), (2, 3)] {
            let a = RgbaImage::from_pixel(w, h, Rgba([253, 20, 11, 255]));
            let b = RgbaImage::from_pixel(w, h, Rgba([49, 90, 222, 255]));
            let mut out = RgbaImage::new(w, h);
            for kind in Transition::ALL {
                transition(kind, &a, &b, 0.0, &mut out);
                assert_eq!(out, a, "{kind:?}");
                transition(kind, &a, &b, 0.47, &mut out);
                transition(kind, &a, &b, 1.0, &mut out);
                assert_eq!(out, b, "{kind:?}");
            }
        }
    }

    #[test]
    fn speed_curves_map_the_entire_source_at_the_selected_average_speed() {
        for curve in Curve::ALL {
            let speed = Speed::Variable { rate: 2.0, curve };
            let duration = speed.output_duration(8.0);
            assert!((duration - 4.0).abs() < 0.001);
            assert_eq!(speed.source_offset(0.0, 8.0), 0.0);
            assert!((speed.source_offset(duration, 8.0) - 8.0).abs() < 0.001);
            assert!((speed.source_offset(duration * 0.25, 8.0) - 2.0).abs() > 0.08);
            assert!((1..100).all(|i| speed
                .source_offset(duration * f64::from(i - 1) / 100.0, 8.0)
                < speed.source_offset(duration * f64::from(i) / 100.0, 8.0)));
        }
    }

    #[test]
    fn detects_120_bpm_clicks() {
        let mut pcm = vec![[0, 0]; 48_000 * 4];
        for click in (12_000..pcm.len()).step_by(24_000) {
            pcm[click..click + 240].fill([28_000, -28_000]);
        }
        let beats = detect_beats(&pcm);
        assert_eq!(beats.len(), 8, "{beats:?}");
        assert!(beats.windows(2).all(|p| (p[1] - p[0] - 0.5).abs() < 0.03));
    }

    #[test]
    fn mixer_saturates_both_polarities_and_applies_gain() {
        assert_eq!(
            mix_sample([30_000, -30_000], [20_000, -20_000], 1.0, 1.0),
            [32767, -32768]
        );
        assert_eq!(
            mix_sample([4_000, -9_000], [-1_000, 8_000], 0.5, 0.25),
            [1750, -2500]
        );
    }

    #[test]
    fn beat_sync_snaps_the_edit_inside_its_source_span() {
        let make = |start| Segment {
            source: "source.mp4".into(),
            start,
            end: start + 6.0,
            view: View::default(),
            start_view: View::default(),
            end_view: View::default(),
            score: 100,
            scene: Scene::Nature,
            thumbnail: Arc::from([]),
        };
        let mut movie = build_from_template(
            &[make(0.0), make(7.0), make(14.0)],
            Template::InSync,
            TargetDuration::UpTo30,
        );
        let initial = movie.timeline().starts[1];
        let beat = initial - 0.15;
        assert_eq!(movie.sync_to_beats(&[beat]), 1);
        assert!((movie.timeline().starts[1] - beat).abs() < 0.001);
        assert!(movie.clips[0].end <= 6.0);
        assert_eq!(movie.sync_to_beats(&[beat]), 0);
    }
}
