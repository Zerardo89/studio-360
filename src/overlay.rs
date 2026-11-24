use std::sync::{Arc, OnceLock};

use ab_glyph::{point, Font, FontRef, ScaleFont};
use image::{Rgba, RgbaImage};

pub const TEXT_PIXELS: f32 = 96.0;
pub const TEXT_LIMIT: usize = 160;
pub const LAYER_LIMIT: usize = 32;

#[derive(Clone, Debug)]
pub struct TextStyle {
    pub value: String,
    pub fill: [u8; 4],
    pub outline: [u8; 4],
    pub thickness: u8,
}

#[derive(Clone, Debug)]
pub enum Content {
    Sticker(&'static str),
    Text(TextStyle),
}

#[derive(Clone, Debug)]
pub struct Layer {
    pub content: Content,
    pub image: Arc<RgbaImage>,
    pub center: [f32; 2],
    pub size: f32,
    pub angle: f32,
    pub start: f64,
    pub end: Option<f64>,
}

impl Layer {
    pub fn sticker(name: &'static str, image: impl Into<Arc<RgbaImage>>) -> Self {
        Self::new(Content::Sticker(name), image.into(), 0.22)
    }

    pub fn text() -> Self {
        let mut layer = Self::new(
            Content::Text(TextStyle {
                value: "Il tuo testo".into(),
                fill: [255; 4],
                outline: [0, 0, 0, 255],
                thickness: 4,
            }),
            Arc::new(RgbaImage::new(1, 1)),
            0.095,
        );
        layer.refresh_text();
        layer
    }

    fn new(content: Content, image: Arc<RgbaImage>, size: f32) -> Self {
        Self {
            content,
            image,
            center: [0.5, 0.5],
            size,
            angle: 0.0,
            start: 0.0,
            end: None,
        }
    }

    pub fn refresh_text(&mut self) {
        if let Content::Text(text) = &self.content {
            self.image = Arc::new(rasterize_text(
                &text.value,
                text.fill,
                text.outline,
                text.thickness,
            ));
        }
    }

    pub fn is_sticker(&self) -> bool {
        matches!(self.content, Content::Sticker(_))
    }

    pub fn label(&self) -> &str {
        match &self.content {
            Content::Sticker(name) => name,
            Content::Text(text) if text.value.trim().is_empty() => "Testo vuoto",
            Content::Text(text) => &text.value,
        }
    }

    pub fn geometry(&self, width: f32, height: f32) -> Geometry {
        let short = width.min(height);
        let source = self.image.dimensions();
        let scale = if self.is_sticker() {
            short * self.size / source.0.max(source.1) as f32
        } else {
            (short * self.size / TEXT_PIXELS)
                .min(width * 0.94 / source.0 as f32)
                .min(height * 0.94 / source.1 as f32)
        };
        let (sin, cos) = self.angle.to_radians().sin_cos();
        Geometry {
            center: [width * self.center[0], height * self.center[1]],
            half: [scale * source.0 as f32 / 2.0, scale * source.1 as f32 / 2.0],
            sin,
            cos,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Overlays {
    pub layers: Vec<Layer>,
    pub show_stickers: bool,
}

impl Default for Overlays {
    fn default() -> Self {
        Self {
            layers: Vec::new(),
            show_stickers: true,
        }
    }
}

impl Overlays {
    pub fn visible(&self, layer: &Layer, time: f64) -> bool {
        (self.show_stickers || !layer.is_sticker())
            && time >= layer.start
            && layer.end.is_none_or(|end| time < end)
    }

    pub fn uses_twemoji(&self, start: f64, end: f64) -> bool {
        self.show_stickers
            && self.layers.iter().any(|layer| {
                layer.is_sticker() && layer.start < end && layer.end.is_none_or(|to| to > start)
            })
    }
}

pub struct Geometry {
    center: [f32; 2],
    half: [f32; 2],
    sin: f32,
    cos: f32,
}

impl Geometry {
    pub fn point(&self, x: f32, y: f32) -> [f32; 2] {
        [
            self.center[0] + x * self.half[0] * self.cos - y * self.half[1] * self.sin,
            self.center[1] + x * self.half[0] * self.sin + y * self.half[1] * self.cos,
        ]
    }

    pub fn uv(&self, x: f32, y: f32) -> [f32; 2] {
        let (x, y) = (x - self.center[0], y - self.center[1]);
        [
            0.5 + (x * self.cos + y * self.sin) / (2.0 * self.half[0]),
            0.5 + (-x * self.sin + y * self.cos) / (2.0 * self.half[1]),
        ]
    }
}

pub fn composite(
    pixels: &mut [u8],
    (w, h): (usize, usize),
    channels: usize,
    overlays: &Overlays,
    time: f64,
) {
    for layer in overlays
        .layers
        .iter()
        .filter(|layer| overlays.visible(layer, time))
    {
        if layer.image.width() == 0 || layer.image.height() == 0 {
            continue;
        }
        let geometry = layer.geometry(w as f32, h as f32);
        let extent_x =
            geometry.half[0] * geometry.cos.abs() + geometry.half[1] * geometry.sin.abs();
        let extent_y =
            geometry.half[0] * geometry.sin.abs() + geometry.half[1] * geometry.cos.abs();
        let x0 = (geometry.center[0] - extent_x).floor().max(0.0) as usize;
        let x1 = ((geometry.center[0] + extent_x).ceil() as usize).min(w);
        let y0 = (geometry.center[1] - extent_y).floor().max(0.0) as usize;
        let y1 = ((geometry.center[1] + extent_y).ceil() as usize).min(h);
        for y in y0..y1 {
            for x in x0..x1 {
                let uv = geometry.uv(x as f32 + 0.5, y as f32 + 0.5);
                if uv.into_iter().all(|v| (0.0..=1.0).contains(&v)) {
                    let sample = bilinear(&layer.image, uv);
                    let offset = (y * w + x) * channels;
                    blend(&mut pixels[offset..offset + channels], sample);
                }
            }
        }
    }
}

// Interpolate premultiplied texels to prevent colored fringes at transparent edges.
fn bilinear(image: &RgbaImage, [u, v]: [f32; 2]) -> [f32; 4] {
    let (w, h) = image.dimensions();
    let x = (u * w as f32 - 0.5).clamp(0.0, (w - 1) as f32);
    let y = (v * h as f32 - 0.5).clamp(0.0, (h - 1) as f32);
    let (x0, y0) = (x as u32, y as u32);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (dx, dy) = (x.fract(), y.fract());
    let mut value = [0.0; 4];
    for (x, y, weight) in [
        (x0, y0, (1.0 - dx) * (1.0 - dy)),
        (x1, y0, dx * (1.0 - dy)),
        (x0, y1, (1.0 - dx) * dy),
        (x1, y1, dx * dy),
    ] {
        let texel = image.get_pixel(x, y).0;
        let alpha = weight * f32::from(texel[3]) / 255.0;
        for channel in 0..3 {
            value[channel] += alpha * f32::from(texel[channel]);
        }
        value[3] += alpha;
    }
    value
}

fn blend(dst: &mut [u8], src: [f32; 4]) {
    if src[3] <= 0.0 {
        return;
    }
    let remaining = dst.get(3).map_or(1.0, |&a| f32::from(a) / 255.0) * (1.0 - src[3]);
    let alpha = src[3] + remaining;
    for channel in 0..3 {
        dst[channel] = ((src[channel] + f32::from(dst[channel]) * remaining) / alpha).round() as u8;
    }
    if let Some(a) = dst.get_mut(3) {
        *a = (255.0 * alpha).round() as u8;
    }
}

pub fn rasterize_text(value: &str, fill: [u8; 4], outline: [u8; 4], thickness: u8) -> RgbaImage {
    static FONTS: OnceLock<Vec<FontRef<'static>>> = OnceLock::new();
    let fonts = FONTS.get_or_init(|| {
        [
            epaint_default_fonts::UBUNTU_LIGHT,
            epaint_default_fonts::NOTO_EMOJI_REGULAR,
        ]
        .into_iter()
        .filter_map(|bytes| FontRef::try_from_slice(bytes).ok())
        .collect()
    });
    let Some(primary) = fonts.first().map(|font| font.as_scaled(TEXT_PIXELS)) else {
        return RgbaImage::new(1, 1);
    };
    let mut outlined = Vec::new();
    let mut x = 0.0;
    let mut baseline = primary.ascent();
    let mut line = 1;
    let mut last = None;
    for ch in value.chars().take(TEXT_LIMIT) {
        if ch == '\n' {
            if line == 14 {
                break;
            }
            line += 1;
            x = 0.0;
            baseline += TEXT_PIXELS * 1.12;
            last = None;
            continue;
        }
        if matches!(ch, '\r' | '\u{fe0e}' | '\u{fe0f}' | '\u{200d}') {
            continue;
        }
        let (i, glyph) = fonts
            .iter()
            .enumerate()
            .find_map(|(i, font)| {
                let glyph = font.glyph_id(ch);
                (glyph.0 != 0).then_some((i, glyph))
            })
            .unwrap_or((0, primary.glyph_id('?')));
        let font = fonts[i].as_scaled(TEXT_PIXELS);
        if x + font.h_advance(glyph) > 1750.0 {
            if line == 14 {
                break;
            }
            line += 1;
            baseline += TEXT_PIXELS * 1.12;
            x = 0.0;
            last = None;
        }
        if let Some((_, previous)) = last.filter(|(before, _)| *before == i) {
            x += font.kern(previous, glyph);
        }
        if let Some(glyph) =
            font.outline_glyph(glyph.with_scale_and_position(TEXT_PIXELS, point(x, baseline)))
        {
            outlined.push(glyph);
        }
        x += font.h_advance(glyph);
        last = Some((i, glyph));
    }
    if outlined.is_empty() {
        return RgbaImage::new(1, 1);
    }
    let radius = i32::from(thickness.min(8));
    let min_x = outlined
        .iter()
        .map(|glyph| glyph.px_bounds().min.x as i32)
        .min()
        .unwrap_or(0)
        - radius
        - 1;
    let min_y = outlined
        .iter()
        .map(|glyph| glyph.px_bounds().min.y as i32)
        .min()
        .unwrap_or(0)
        - radius
        - 1;
    let max_x = outlined
        .iter()
        .map(|glyph| glyph.px_bounds().max.x as i32)
        .max()
        .unwrap_or(0)
        + radius
        + 1;
    let max_y = outlined
        .iter()
        .map(|glyph| glyph.px_bounds().max.y as i32)
        .max()
        .unwrap_or(0)
        + radius
        + 1;
    let (w, h) = ((max_x - min_x) as usize, (max_y - min_y) as usize);
    let mut mask = vec![0u8; w * h];
    for glyph in &outlined {
        let bounds = glyph.px_bounds();
        let x = (bounds.min.x as i32 - min_x) as usize;
        let y = (bounds.min.y as i32 - min_y) as usize;
        glyph.draw(|u, v, coverage| {
            let i = (y + v as usize) * w + x + u as usize;
            mask[i] = mask[i].max((255.0 * coverage).round() as u8);
        });
    }
    let stroke = if radius == 0 {
        None
    } else {
        let mut horizontal = vec![0; mask.len()];
        for (i, &value) in mask.iter().enumerate().filter(|(_, value)| **value > 0) {
            let radius = radius as usize;
            for target in &mut horizontal[i - radius..=i + radius] {
                *target = (*target).max(value);
            }
        }
        let mut expanded = vec![0; mask.len()];
        for (i, value) in horizontal
            .into_iter()
            .enumerate()
            .filter(|(_, value)| *value > 0)
        {
            for dy in -radius..=radius {
                let target = &mut expanded[(i as isize + dy as isize * w as isize) as usize];
                *target = (*target).max(value);
            }
        }
        Some(expanded)
    };
    RgbaImage::from_fn(w as u32, h as u32, |x, y| {
        let i = y as usize * w + x as usize;
        let mut pixel = outline;
        pixel[3] = stroke.as_ref().map_or(0, |s| {
            ((u16::from(s[i]) * u16::from(outline[3])) / 255) as u8
        });
        let alpha = f32::from(mask[i]) * f32::from(fill[3]) / (255.0 * 255.0);
        blend(
            &mut pixel,
            [
                f32::from(fill[0]) * alpha,
                f32::from(fill[1]) * alpha,
                f32::from(fill[2]) * alpha,
                alpha,
            ],
        );
        Rgba(pixel)
    })
}
