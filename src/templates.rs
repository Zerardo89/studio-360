//! Local video-template catalog shared by Auto Edit and the movie builder.

use crate::movie::{Filter, Template, Transition};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Featured,
    Travel,
    Action,
    Vlog,
    Rhythm,
}

impl Category {
    pub const ALL: [Self; 5] = [
        Self::Featured,
        Self::Travel,
        Self::Action,
        Self::Vlog,
        Self::Rhythm,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Featured => "In evidenza",
            Self::Travel => "Viaggio",
            Self::Action => "Sport e azione",
            Self::Vlog => "Vlog",
            Self::Rhythm => "Ritmo",
        }
    }
}

pub const FEATURED: [Template; 4] = [
    Template::UrbanPulse,
    Template::LastLight,
    Template::TravelingDay,
    Template::Dynamic,
];

#[derive(Clone, Copy)]
pub enum CoverStyle {
    Stripes,
    Rings,
    Waves,
    Grid,
}

#[derive(Clone, Copy)]
pub struct Slot {
    pub beats: u8,
    pub transition: Transition,
    pub speed: f64,
}

impl Slot {
    pub fn seconds(self, bpm: u16) -> f64 {
        f64::from(self.beats) * 60.0 / f64::from(bpm)
    }
}

#[derive(Clone, Copy)]
pub enum TemplateKind {
    Free,
    Sync {
        slots: &'static [Slot],
        seconds: f64,
        filter: Filter,
    },
}

pub struct TemplateInfo {
    pub category: Category,
    pub description: &'static str,
    pub bpm: u16,
    pub music_query: &'static str,
    pub aspect: usize,
    pub aspect_locked: bool,
    pub colors: [[u8; 3]; 3],
    pub style: CoverStyle,
    pub kind: TemplateKind,
}

impl Template {
    pub fn info(self) -> &'static TemplateInfo {
        &CATALOG[self as usize]
    }
}

use Category::{Action, Rhythm, Travel, Vlog};
use CoverStyle::{Grid, Rings, Stripes, Waves};

const fn slot(beats: u8, transition: Transition, speed: f64) -> Slot {
    Slot {
        beats,
        transition,
        speed,
    }
}

const CATALOG: [TemplateInfo; Template::ALL.len()] = [
    TemplateInfo {
        category: Action,
        description: "Tagli rapidi e zoom nei momenti più movimentati.",
        bpm: 128,
        music_query: "upbeat action electronic",
        aspect: 0,
        aspect_locked: false,
        colors: [[20, 46, 66], [35, 125, 142], [142, 224, 224]],
        style: Stripes,
        kind: TemplateKind::Free,
    },
    TemplateInfo {
        category: Travel,
        description: "Vedute ampie e passaggi morbidi per raccontare un viaggio.",
        bpm: 110,
        music_query: "upbeat travel instrumental",
        aspect: 0,
        aspect_locked: false,
        colors: [[50, 38, 61], [155, 73, 63], [255, 201, 137]],
        style: Grid,
        kind: TemplateKind::Free,
    },
    TemplateInfo {
        category: Rhythm,
        description: "Cambi d’inquadratura brevi con passaggi netti.",
        bpm: 120,
        music_query: "groovy rhythmic instrumental",
        aspect: 0,
        aspect_locked: false,
        colors: [[40, 34, 69], [104, 90, 176], [199, 175, 255]],
        style: Rings,
        kind: TemplateKind::Free,
    },
    TemplateInfo {
        category: Action,
        description: "Accelerazioni e rotazioni per le scene più intense.",
        bpm: 140,
        music_query: "energetic workout electronic",
        aspect: 0,
        aspect_locked: false,
        colors: [[50, 28, 56], [162, 62, 87], [255, 156, 130]],
        style: Stripes,
        kind: TemplateKind::Free,
    },
    TemplateInfo {
        category: Vlog,
        description: "Inquadrature distese per i piccoli momenti della giornata.",
        bpm: 90,
        music_query: "warm acoustic vlog",
        aspect: 0,
        aspect_locked: false,
        colors: [[51, 51, 41], [107, 118, 76], [228, 219, 155]],
        style: Grid,
        kind: TemplateKind::Free,
    },
    TemplateInfo {
        category: Travel,
        description: "Dal primo panorama all’ultima tappa, con tagli ariosi.",
        bpm: 112,
        music_query: "outdoor travel cinematic",
        aspect: 0,
        aspect_locked: false,
        colors: [[27, 50, 51], [48, 114, 105], [154, 225, 188]],
        style: Waves,
        kind: TemplateKind::Free,
    },
    TemplateInfo {
        category: Rhythm,
        description: "Cambi veloci e riprese accelerate su un ritmo incalzante.",
        bpm: 132,
        music_query: "punchy house instrumental",
        aspect: 0,
        aspect_locked: false,
        colors: [[59, 35, 69], [145, 83, 150], [255, 194, 228]],
        style: Rings,
        kind: TemplateKind::Free,
    },
    TemplateInfo {
        category: Action,
        description: "Movimenti decisi alternati a pause panoramiche.",
        bpm: 108,
        music_query: "cinematic adventure percussion",
        aspect: 0,
        aspect_locked: false,
        colors: [[43, 47, 73], [85, 83, 129], [183, 194, 255]],
        style: Grid,
        kind: TemplateKind::Free,
    },
    TemplateInfo {
        category: Rhythm,
        description: "Sei tagli di strada, dai primi accenti al finale lungo.",
        bpm: 120,
        music_query: "urban electronic groove",
        aspect: 0,
        aspect_locked: true,
        colors: [[24, 43, 58], [19, 110, 115], [130, 230, 202]],
        style: Rings,
        kind: TemplateKind::Sync {
            slots: &[
                slot(4, Transition::Slice, 1.15),
                slot(4, Transition::BlurRight, 1.0),
                slot(4, Transition::SlideLeft, 1.1),
                slot(4, Transition::BlurZoom, 1.2),
                slot(6, Transition::Fade, 1.0),
                slot(8, Transition::None, 1.0),
            ],
            seconds: 15.0,
            filter: Filter(3),
        },
    },
    TemplateInfo {
        category: Travel,
        description: "Cinque vedute in sequenza, come cartoline di un viaggio.",
        bpm: 100,
        music_query: "upbeat travel acoustic",
        aspect: 0,
        aspect_locked: false,
        colors: [[56, 46, 40], [147, 96, 73], [255, 216, 162]],
        style: Grid,
        kind: TemplateKind::Sync {
            slots: &[
                slot(4, Transition::SlideLeft, 1.0),
                slot(3, Transition::SlideUp, 1.0),
                slot(3, Transition::SlideRight, 1.1),
                slot(4, Transition::Fade, 1.0),
                slot(6, Transition::None, 0.9),
            ],
            seconds: 12.0,
            filter: Filter(4),
        },
    },
    TemplateInfo {
        category: Action,
        description: "Scatti brevi e accelerazioni per salti, curve e discese.",
        bpm: 150,
        music_query: "fast sports breakbeat",
        aspect: 1,
        aspect_locked: true,
        colors: [[57, 36, 61], [164, 63, 74], [255, 174, 118]],
        style: Stripes,
        kind: TemplateKind::Sync {
            slots: &[
                slot(4, Transition::BlurZoom2, 1.2),
                slot(3, Transition::BlurSpin, 1.35),
                slot(3, Transition::Slice, 1.25),
                slot(4, Transition::BlurRight2, 1.15),
                slot(4, Transition::RightSwipe, 1.1),
                slot(7, Transition::None, 1.0),
            ],
            seconds: 10.0,
            filter: Filter(7),
        },
    },
    TemplateInfo {
        category: Vlog,
        description: "Quattro dettagli quotidiani con passaggi delicati.",
        bpm: 96,
        music_query: "soft indie acoustic vlog",
        aspect: 1,
        aspect_locked: true,
        colors: [[49, 48, 82], [108, 92, 154], [223, 197, 250]],
        style: Grid,
        kind: TemplateKind::Sync {
            slots: &[
                slot(3, Transition::Fade, 0.9),
                slot(3, Transition::SlideUp, 1.0),
                slot(4, Transition::BlurPivot, 0.9),
                slot(6, Transition::None, 0.9),
            ],
            seconds: 10.0,
            filter: Filter(9),
        },
    },
    TemplateInfo {
        category: Travel,
        description: "Quattro panorami lenti con dissolvenze e colori caldi.",
        bpm: 90,
        music_query: "golden hour ambient instrumental",
        aspect: 0,
        aspect_locked: true,
        colors: [[52, 37, 61], [147, 76, 98], [255, 194, 145]],
        style: Waves,
        kind: TemplateKind::Sync {
            slots: &[
                slot(4, Transition::Fade, 0.8),
                slot(4, Transition::BlurScroll, 0.9),
                slot(4, Transition::Fade, 0.85),
                slot(6, Transition::None, 0.9),
            ],
            seconds: 12.0,
            filter: Filter(9),
        },
    },
    TemplateInfo {
        category: Vlog,
        description: "Una sequenza verticale per raccontare un luogo in cinque viste.",
        bpm: 120,
        music_query: "city vlog chillhop",
        aspect: 1,
        aspect_locked: true,
        colors: [[34, 47, 75], [70, 95, 152], [158, 201, 255]],
        style: Grid,
        kind: TemplateKind::Sync {
            slots: &[
                slot(4, Transition::SlideUp, 1.0),
                slot(4, Transition::RightSwipe, 1.1),
                slot(4, Transition::SlideDown, 1.0),
                slot(4, Transition::Slice, 1.15),
                slot(8, Transition::None, 1.0),
            ],
            seconds: 12.0,
            filter: Filter(3),
        },
    },
    TemplateInfo {
        category: Action,
        description: "Una salita in cinque tempi con ampio spazio al panorama finale.",
        bpm: 120,
        music_query: "mountain cinematic drums",
        aspect: 0,
        aspect_locked: false,
        colors: [[37, 55, 46], [66, 110, 86], [174, 221, 172]],
        style: Stripes,
        kind: TemplateKind::Sync {
            slots: &[
                slot(4, Transition::BlurScroll, 1.1),
                slot(6, Transition::SlideUp, 1.0),
                slot(4, Transition::BlurZoom, 1.15),
                slot(6, Transition::Fade, 0.9),
                slot(8, Transition::None, 0.85),
            ],
            seconds: 14.0,
            filter: Filter(4),
        },
    },
    TemplateInfo {
        category: Rhythm,
        description: "Due accenti ravvicinati aprono un montaggio da pista.",
        bpm: 144,
        music_query: "club kick drum techno",
        aspect: 1,
        aspect_locked: false,
        colors: [[49, 36, 80], [113, 60, 160], [241, 157, 240]],
        style: Rings,
        kind: TemplateKind::Sync {
            slots: &[
                slot(2, Transition::Slice, 1.2),
                slot(2, Transition::RightSwipe, 1.25),
                slot(4, Transition::BlurSpin, 1.0),
                slot(4, Transition::BlurZoom2, 1.15),
                slot(4, Transition::SlideRight, 1.1),
                slot(8, Transition::None, 1.0),
            ],
            seconds: 10.0,
            filter: Filter(12),
        },
    },
];
