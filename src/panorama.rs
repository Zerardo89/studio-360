//! Equirectangular panoramas: sample generation, projection to a view, colour adjustments.

use std::f32::consts::PI;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// A flat or equirectangular image stored as RGB bytes.
pub struct Pano {
    pub id: u64,
    pub name: String,
    pub w: usize,
    pub h: usize,
    pub rgb: Vec<u8>,
    pub is_panorama: bool,
}

impl Pano {
    pub fn from_rgb(id: u64, name: String, w: usize, h: usize, rgb: Vec<u8>) -> Self {
        Self {
            id,
            name,
            w,
            h,
            rgb,
            is_panorama: true,
        }
    }

    /// Bilinear sample at longitude (-PI..PI) and latitude (-PI/2..PI/2).
    #[inline]
    fn sample(&self, lon: f32, lat: f32, sharpness: f32) -> [f32; 3] {
        let u = (lon / (2.0 * PI) + 0.5) * self.w as f32 - 0.5;
        let v = (0.5 - lat / PI) * self.h as f32 - 0.5;
        self.sample_detail(u, v, true, sharpness)
    }

    #[inline]
    fn sample_detail(&self, u: f32, v: f32, wrap: bool, sharpness: f32) -> [f32; 3] {
        let mut center = self.sample_uv(u, v, wrap);
        if sharpness != 0.0 {
            let left = self.sample_uv(u - 1.0, v, wrap);
            let right = self.sample_uv(u + 1.0, v, wrap);
            let above = self.sample_uv(u, v - 1.0, wrap);
            let below = self.sample_uv(u, v + 1.0, wrap);
            for channel in 0..3 {
                let average =
                    (left[channel] + right[channel] + above[channel] + below[channel]) * 0.25;
                center[channel] =
                    (center[channel] + sharpness * (center[channel] - average)).clamp(0.0, 1.0);
            }
        }
        center
    }

    #[inline]
    fn sample_uv(&self, u: f32, v: f32, wrap: bool) -> [f32; 3] {
        let w = self.w as i64;
        let u = if wrap {
            u
        } else {
            u.clamp(0.0, (self.w - 1) as f32)
        };
        let v = v.clamp(0.0, (self.h - 1) as f32);
        let x0 = u.floor();
        let y0 = v.floor();
        let fx = u - x0;
        let fy = v - y0;
        let index = |x: i64| if wrap { x.rem_euclid(w) } else { x.min(w - 1) } as usize;
        let xi0 = index(x0 as i64);
        let xi1 = index(x0 as i64 + 1);
        let yi0 = y0 as usize;
        let yi1 = (yi0 + 1).min(self.h - 1);
        let p = |x: usize, y: usize| {
            let i = (y * self.w + x) * 3;
            [
                self.rgb[i] as f32,
                self.rgb[i + 1] as f32,
                self.rgb[i + 2] as f32,
            ]
        };
        let a = p(xi0, yi0);
        let b = p(xi1, yi0);
        let c = p(xi0, yi1);
        let d = p(xi1, yi1);
        let mut out = [0.0; 3];
        for k in 0..3 {
            let top = a[k] + (b[k] - a[k]) * fx;
            let bot = c[k] + (d[k] - c[k]) * fx;
            out[k] = (top + (bot - top) * fy) / 255.0;
        }
        out
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Projection {
    /// Ordinary perspective view.
    Rectilinear,
    /// Tiny Planet: stereographic projection centred on the ground.
    Stereographic,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct View {
    /// Degrees, positive turns right.
    pub yaw: f32,
    /// Degrees, positive looks up.
    pub pitch: f32,
    /// Horizontal field of view in degrees.
    pub fov: f32,
    pub projection: Projection,
}

impl Default for View {
    fn default() -> Self {
        Self {
            yaw: 0.0,
            pitch: 0.0,
            fov: 100.0,
            projection: Projection::Rectilinear,
        }
    }
}

/// The 13 colour sliders, each -100..100 (0 = unchanged).
pub const ADJUST_NAMES: [&str; 13] = [
    "Esposizione",
    "Luminosità",
    "Contrasto",
    "Saturazione",
    "Vividezza",
    "Alte luci",
    "Ombre",
    "Punto del nero",
    "Brillantezza",
    "Definizione",
    "Nitidezza",
    "Temperatura",
    "Tinta",
];

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Adjust {
    pub v: [f32; 13],
    pub mono: bool,
}

impl Adjust {
    pub fn is_identity(&self) -> bool {
        !self.mono && self.v.iter().all(|x| *x == 0.0)
    }

    /// Combine user sliders with a filter preset (values add, then clamp).
    pub fn combined(&self, preset: &Adjust) -> Adjust {
        let mut out = *self;
        for i in 0..13 {
            out.v[i] = (self.v[i] + preset.v[i]).clamp(-100.0, 100.0);
        }
        out.mono = self.mono || preset.mono;
        out
    }

    /// Simple, predictable approximations of each slider, applied to a 0..1 RGB colour.
    #[inline]
    pub fn apply(&self, c: [f32; 3]) -> [f32; 3] {
        let s = |i: usize| self.v[i] / 100.0;
        let [mut r, mut g, mut b] = c;
        // Exposure: up to +/- 2 stops.
        let e = 2f32.powf(s(0) * 2.0);
        r *= e;
        g *= e;
        b *= e;
        // Temperature and tint.
        r += s(11) * 0.10;
        b -= s(11) * 0.10;
        g -= s(12) * 0.08;
        // Brightness.
        let br = s(1) * 0.25;
        r += br;
        g += br;
        b += br;
        // Black point.
        let bp = s(7) * 0.15;
        if bp != 0.0 {
            let k = 1.0 / (1.0 - bp).max(0.05);
            r = (r - bp) * k;
            g = (g - bp) * k;
            b = (b - bp) * k;
        }
        let luma = |r: f32, g: f32, b: f32| 0.299 * r + 0.587 * g + 0.114 * b;
        // Highlights, shadows and brilliance work on luminance.
        let l = luma(r, g, b).clamp(0.0, 1.0);
        let hi = if l > 0.5 { (l - 0.5) * 2.0 } else { 0.0 };
        let lo = if l < 0.5 { (0.5 - l) * 2.0 } else { 0.0 };
        let mut dl = s(5) * 0.25 * hi + s(6) * 0.25 * lo;
        dl += s(8) * 0.15 * (lo - hi * 0.5);
        r += dl;
        g += dl;
        b += dl;
        // Definition adds gentle mid-tone contrast.
        let midtones = 1.0 - (l - 0.5).abs() * 2.0;
        let k = (1.0 + s(2) * 0.8 + s(9) * 0.4 * midtones).max(0.05);
        r = (r - 0.5) * k + 0.5;
        g = (g - 0.5) * k + 0.5;
        b = (b - 0.5) * k + 0.5;
        // Saturation and vibrance.
        let l = luma(r, g, b);
        let maxc = r.max(g).max(b);
        let minc = r.min(g).min(b);
        let sat_now = if maxc > 0.0 {
            (maxc - minc) / maxc.max(1e-4)
        } else {
            0.0
        };
        let mut sk = 1.0 + s(3);
        sk += s(4) * (1.0 - sat_now.clamp(0.0, 1.0));
        if self.mono {
            sk = 0.0;
        }
        r = l + (r - l) * sk;
        g = l + (g - l) * sk;
        b = l + (b - l) * sk;
        [r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0)]
    }
}

pub const FRAME_ASPECT: f32 = 16.0 / 9.0;

/// Fraction of the full 16:9 view included in a centered crop.
#[derive(Clone, Copy)]
pub struct Region {
    pub width: f32,
    pub height: f32,
}

impl Region {
    pub const FULL: Self = Self {
        width: 1.0,
        height: 1.0,
    };

    pub fn crop(aspect: f32) -> Self {
        Self {
            width: (aspect / FRAME_ASPECT).min(1.0),
            height: (FRAME_ASPECT / aspect).min(1.0),
        }
    }
}

/// Render a centered region of the full view. Progress counts finished output rows.
pub fn render(
    pano: &Pano,
    view: &View,
    adj: &Adjust,
    (w, h): (usize, usize),
    region: Region,
    progress: Option<&AtomicUsize>,
) -> Vec<u8> {
    let mut out = vec![0u8; w * h * 3];
    if w == 0 || h == 0 {
        return out;
    }
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 16);
    let rows_per = h.div_ceil(threads);
    let identity = adj.is_identity();
    let sharpness = adj.v[10] / 100.0;
    let yaw = view.yaw.to_radians();
    let pitch = view.pitch.to_radians();
    let (sy, cy) = yaw.sin_cos();
    let (sp, cp) = pitch.sin_cos();
    let fov = view.fov.to_radians();
    let rectilinear_scale = (fov * 0.5).tan();
    let stereographic_scale = (fov * 0.25).tan();
    let flat = Region::crop(pano.w as f32 / pano.h as f32);
    std::thread::scope(|scope| {
        for (chunk_i, chunk) in out.chunks_mut(rows_per * w * 3).enumerate() {
            let y_start = chunk_i * rows_per;
            scope.spawn(move || {
                let rows = chunk.len() / (w * 3);
                for ry in 0..rows {
                    let y = y_start + ry;
                    let ny = ((h as f32 - 2.0 * y as f32 - 1.0) / h as f32) * region.height;
                    for x in 0..w {
                        let nx = ((2.0 * x as f32 + 1.0 - w as f32) / w as f32) * region.width;
                        let i = (ry * w + x) * 3;
                        let mut c = if pano.is_panorama {
                            let (lon, lat) = match view.projection {
                                Projection::Rectilinear => {
                                    let t = rectilinear_scale;
                                    let (dx, dy, dz) = (nx * t, ny * t / FRAME_ASPECT, 1.0f32);
                                    // Pitch around X, then yaw around Y.
                                    let dy2 = dy * cp + dz * sp;
                                    let dz2 = -dy * sp + dz * cp;
                                    let dx3 = dx * cy + dz2 * sy;
                                    let dz3 = -dx * sy + dz2 * cy;
                                    let len = (dx3 * dx3 + dy2 * dy2 + dz3 * dz3).sqrt();
                                    (dx3.atan2(dz3), (dy2 / len).clamp(-1.0, 1.0).asin())
                                }
                                Projection::Stereographic => {
                                    // Angle from the nadir grows with distance from the centre.
                                    let k = stereographic_scale;
                                    let px = nx * k;
                                    let py = ny * k / FRAME_ASPECT;
                                    let r = (px * px + py * py).sqrt();
                                    let theta = 2.0 * r.atan();
                                    let phi = py.atan2(px) + yaw;
                                    let lat = -PI / 2.0 + theta;
                                    let mut lon = phi;
                                    if lon > PI {
                                        lon -= 2.0 * PI;
                                    }
                                    if lon < -PI {
                                        lon += 2.0 * PI;
                                    }
                                    (lon, lat.clamp(-PI / 2.0, PI / 2.0))
                                }
                            };
                            pano.sample(lon, lat, sharpness)
                        } else if nx.abs() <= flat.width && ny.abs() <= flat.height {
                            let u = (nx / flat.width + 1.0) * 0.5 * pano.w as f32 - 0.5;
                            let v = (1.0 - ny / flat.height) * 0.5 * pano.h as f32 - 0.5;
                            pano.sample_detail(u, v, false, sharpness)
                        } else {
                            chunk[i..i + 3].copy_from_slice(&[18, 19, 22]);
                            continue;
                        };
                        if !identity {
                            c = adj.apply(c);
                        }
                        chunk[i] = (c[0] * 255.0 + 0.5) as u8;
                        chunk[i + 1] = (c[1] * 255.0 + 0.5) as u8;
                        chunk[i + 2] = (c[2] * 255.0 + 0.5) as u8;
                    }
                    if let Some(p) = progress {
                        p.fetch_add(1, Ordering::Relaxed);
                    }
                }
            });
        }
    });
    out
}

// ---------------------------------------------------------------------------
// Sample panoramas drawn by code (neutral placeholders, no photos).

struct Scene {
    name: &'static str,
    sky_top: [f32; 3],
    sky_horizon: [f32; 3],
    ground_a: [f32; 3],
    ground_b: [f32; 3],
    sun_lon: f32,
    sun_lat: f32,
    sun: [f32; 3],
    hills: [f32; 3],
    pole: [f32; 3],
}

const SCENES: [Scene; 4] = [
    Scene {
        name: "Esempio – Prato",
        sky_top: [0.18, 0.42, 0.82],
        sky_horizon: [0.72, 0.86, 0.98],
        ground_a: [0.30, 0.55, 0.25],
        ground_b: [0.24, 0.47, 0.20],
        sun_lon: 0.6,
        sun_lat: 0.55,
        sun: [1.0, 0.97, 0.85],
        hills: [0.22, 0.40, 0.30],
        pole: [0.91, 0.74, 0.0],
    },
    Scene {
        name: "Esempio – Tramonto",
        sky_top: [0.20, 0.16, 0.45],
        sky_horizon: [0.98, 0.55, 0.30],
        ground_a: [0.35, 0.28, 0.25],
        ground_b: [0.28, 0.22, 0.20],
        sun_lon: -1.2,
        sun_lat: 0.08,
        sun: [1.0, 0.85, 0.55],
        hills: [0.25, 0.15, 0.25],
        pole: [0.95, 0.95, 0.95],
    },
    Scene {
        name: "Esempio – Spiaggia",
        sky_top: [0.10, 0.50, 0.85],
        sky_horizon: [0.80, 0.93, 1.0],
        ground_a: [0.92, 0.84, 0.62],
        ground_b: [0.86, 0.78, 0.56],
        sun_lon: 2.2,
        sun_lat: 0.8,
        sun: [1.0, 1.0, 0.92],
        hills: [0.10, 0.45, 0.60],
        pole: [0.85, 0.25, 0.25],
    },
    Scene {
        name: "Esempio – Neve",
        sky_top: [0.45, 0.55, 0.70],
        sky_horizon: [0.85, 0.88, 0.92],
        ground_a: [0.95, 0.96, 0.98],
        ground_b: [0.86, 0.88, 0.92],
        sun_lon: -2.5,
        sun_lat: 0.35,
        sun: [1.0, 1.0, 1.0],
        hills: [0.55, 0.60, 0.70],
        pole: [0.20, 0.45, 0.85],
    },
];

pub fn sample_count() -> usize {
    SCENES.len()
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

/// Draw sample panorama `index` at `w` x `w/2`.
pub fn generate_sample(index: usize, w: usize) -> Pano {
    let s = &SCENES[index % SCENES.len()];
    let h = w / 2;
    let mut rgb = vec![0u8; w * h * 3];
    let seed = index as f32 * 1.7 + 0.3;
    let rows: Vec<(usize, &mut [u8])> = rgb.chunks_mut(w * 3).enumerate().collect();
    std::thread::scope(|scope| {
        let threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(1, 16);
        let per = rows.len().div_ceil(threads);
        let mut rows = rows;
        while !rows.is_empty() {
            let take = per.min(rows.len());
            let batch: Vec<(usize, &mut [u8])> = rows.drain(..take).collect();
            scope.spawn(move || {
                for (y, row) in batch {
                    let lat = PI / 2.0 - (y as f32 + 0.5) / h as f32 * PI;
                    for x in 0..w {
                        let lon = (x as f32 + 0.5) / w as f32 * 2.0 * PI - PI;
                        let c = scene_color(s, seed, lon, lat);
                        row[x * 3] = (c[0].clamp(0.0, 1.0) * 255.0) as u8;
                        row[x * 3 + 1] = (c[1].clamp(0.0, 1.0) * 255.0) as u8;
                        row[x * 3 + 2] = (c[2].clamp(0.0, 1.0) * 255.0) as u8;
                    }
                }
            });
        }
    });
    Pano::from_rgb(index as u64 + 1, s.name.to_string(), w, h, rgb)
}

fn scene_color(s: &Scene, seed: f32, lon: f32, lat: f32) -> [f32; 3] {
    let ridge = |phase: f32| {
        0.09 + 0.052 * (lon * 3.0 + seed + phase).sin()
            + 0.036 * (lon * 7.0 + seed * 2.0 - phase).sin()
            + 0.012 * (lon * 17.0 + phase).sin()
    };
    if lat >= 0.0 {
        let near = ridge(1.7) * 0.74;
        let middle = ridge(-0.8) * 1.12;
        let far = ridge(0.6) * 1.74;
        if lat < near {
            return mix(
                s.ground_b,
                s.hills,
                0.55 + 0.3 * (lon * 26.0 + lat * 19.0).sin(),
            );
        }
        if lat < middle {
            return mix(s.hills, s.sky_horizon, 0.24);
        }
        if lat < far {
            return mix(s.hills, s.sky_horizon, 0.52);
        }
        let t = (lat / (PI / 2.0)).powf(0.6);
        let mut c = mix(s.sky_horizon, s.sky_top, t);
        // Sun with a soft halo.
        let cosd =
            lat.sin() * s.sun_lat.sin() + lat.cos() * s.sun_lat.cos() * (lon - s.sun_lon).cos();
        let ang = cosd.clamp(-1.0, 1.0).acos();
        if ang < 0.06 {
            return s.sun;
        }
        let glow = (-(ang - 0.06) * 6.0).exp() * 0.45;
        c = mix(c, s.sun, glow);
        let cloud = (lon * 6.0 + seed + (lat * 13.0).sin()).sin() * 0.39
            + (lon * 11.0 - lat * 11.0 + seed).cos() * 0.21
            + (lat * 37.0 + (lon * 3.0).cos()).sin() * 0.4;
        let opacity = (cloud - 0.16).clamp(0.0, 0.6)
            * (lat * 3.0).clamp(0.0, 1.0)
            * (1.0 - (lat / 1.25).clamp(0.0, 1.0));
        c = mix(c, mix(s.sun, [0.91, 0.92, 0.94], 0.6), opacity);
        c
    } else {
        let d = 1.0 / (-lat).tan().max(0.04);
        let gx = d * lon.cos();
        let gz = d * lon.sin();
        let texture =
            (gx * 1.9 + (gz * 0.9).cos()).sin() * 0.31 + (gx * 11.0 + gz * 15.0).cos() * 0.12;
        let ground = mix(
            s.ground_a,
            s.ground_b,
            0.5 + texture * (-lat * 3.0).clamp(0.0, 1.0),
        );
        let trail = (gz - 0.43 * (gx * 0.73 + seed).sin()).abs();
        let trail_opacity = ((0.49 - trail) * 9.0).clamp(0.0, 1.0) * 0.7;
        mix(ground, mix(s.pole, s.ground_a, 0.76), trail_opacity)
    }
}

/// Shared handle used across the app.
pub type PanoRef = Arc<Pano>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portrait_export_samples_the_same_centered_area_as_preview() {
        let mut pano = generate_sample(0, 320);
        pano.is_panorama = true;
        let preview = render(
            &pano,
            &View::default(),
            &Adjust::default(),
            (160, 90),
            Region::FULL,
            None,
        );
        let crop = render(
            &pano,
            &View::default(),
            &Adjust::default(),
            (90, 90),
            Region::crop(1.0),
            None,
        );
        for (row, cropped_row) in crop.chunks_exact(90 * 3).enumerate() {
            let start = (row * 160 + 35) * 3;
            assert_eq!(&preview[start..start + 90 * 3], cropped_row);
        }
    }
}
