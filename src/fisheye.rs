//! Approximate equidistant projection for two opposing fisheye lenses.

use std::f32::consts::{PI, TAU};

use image::{Rgb, RgbImage};

const LENS_FOV_DEGREES: f32 = 200.0;
const LENS_DIAMETER_RATIO: f32 = 1.0;
const BORDER_LEVEL: u8 = 24;
const BORDER_PROBE: f32 = 0.43;
const CENTER_PROBE: f32 = 0.24;
const MIN_DARK_CORNERS: usize = 3;
const MIN_LIT_PROBES: usize = 2;
const PANORAMA_ASPECT_TOLERANCE: f32 = 0.12;

pub fn is_equirectangular((width, height): (u32, u32)) -> bool {
    (width as f32 / height as f32 - 2.0).abs() <= PANORAMA_ASPECT_TOLERANCE
}

pub fn has_side_by_side_circles(image: &RgbImage) -> bool {
    if !is_equirectangular(image.dimensions()) || image.height() < 8 {
        return false;
    }
    let [left, right] = packed_lenses(image.dimensions());
    [left, right].into_iter().all(|lens| {
        let dark = |dx: f32, dy: f32| {
            let x = (lens.center.0 + dx * lens.width as f32).round();
            let y = (lens.center.1 + dy * lens.height as f32).round();
            image
                .get_pixel_checked(x as u32, y as u32)
                .is_some_and(|p| p.0.into_iter().all(|value| value <= BORDER_LEVEL))
        };
        let b = BORDER_PROBE;
        let c = CENTER_PROBE;
        [(-b, -b), (-b, b), (b, -b), (b, b)]
            .into_iter()
            .filter(|&(x, y)| dark(x, y))
            .count()
            >= MIN_DARK_CORNERS
            && [(0.0, 0.0), (-c, 0.0), (c, 0.0), (0.0, -c), (0.0, c)]
                .into_iter()
                .filter(|&(x, y)| !dark(x, y))
                .count()
                >= MIN_LIT_PROBES
    })
}

#[derive(Clone, Copy)]
struct Lens {
    offset: u32,
    width: u32,
    height: u32,
    center: (f32, f32),
    radius: f32,
}

impl Lens {
    fn new(offset: u32, width: u32, height: u32) -> Self {
        Self {
            offset,
            width,
            height,
            center: (
                offset as f32 + (width as f32 - 1.0) * 0.5,
                (height as f32 - 1.0) * 0.5,
            ),
            radius: width.min(height) as f32 * 0.5 * LENS_DIAMETER_RATIO,
        }
    }

    fn sample(self, image: &RgbImage, x: f32, y: f32, angle: f32) -> [f32; 3] {
        let transverse = (x * x + y * y).sqrt();
        let scale = if transverse > f32::EPSILON {
            self.radius * angle / (half_fov() * transverse)
        } else {
            0.0
        };
        let right = self.offset + self.width - 1;
        let bottom = self.height - 1;
        let u = (self.center.0 + scale * x).clamp(self.offset as f32, right as f32);
        let v = (self.center.1 - scale * y).clamp(0.0, bottom as f32);
        let (x0, y0) = (u as u32, v as u32);
        let (x1, y1) = ((x0 + 1).min(right), (y0 + 1).min(bottom));
        let dx = u - x0 as f32;
        let dy = v - y0 as f32;
        let [a, b, c, d] = [(x0, y0), (x1, y0), (x0, y1), (x1, y1)]
            .map(|(x, y)| image.get_pixel(x, y).0.map(f32::from));
        std::array::from_fn(|i| {
            let upper = a[i] + (b[i] - a[i]) * dx;
            let lower = c[i] + (d[i] - c[i]) * dx;
            upper + (lower - upper) * dy
        })
    }
}

fn half_fov() -> f32 {
    (LENS_FOV_DEGREES * 0.5).to_radians()
}

fn packed_lenses((width, height): (u32, u32)) -> [Lens; 2] {
    let split = width / 2;
    [
        Lens::new(0, split, height),
        Lens::new(split, width - split, height),
    ]
}

pub struct Stitcher {
    lenses: [Lens; 2],
    longitude: Vec<(f32, f32)>,
    latitude: Vec<(f32, f32)>,
}

impl Stitcher {
    pub fn side_by_side(size: (u32, u32), output_width: u32) -> Self {
        Self::new(packed_lenses(size), output_width)
    }

    pub fn separate(left: (u32, u32), right: (u32, u32), output_width: u32) -> Self {
        Self::new(
            [Lens::new(0, left.0, left.1), Lens::new(0, right.0, right.1)],
            output_width,
        )
    }

    fn new(lenses: [Lens; 2], output_width: u32) -> Self {
        let height = (output_width / 2).max(1);
        let width = height * 2;
        Self {
            lenses,
            longitude: (0..width)
                .map(|x| (((x as f32 + 0.5) / width as f32 - 0.5) * TAU).sin_cos())
                .collect(),
            latitude: (0..height)
                .map(|y| ((0.5 - (y as f32 + 0.5) / height as f32) * PI).sin_cos())
                .collect(),
        }
    }

    pub fn render(&self, left: &RgbImage, right: Option<&RgbImage>) -> RgbImage {
        let right = right.unwrap_or(left);
        let mut output = RgbImage::new(self.longitude.len() as u32, self.latitude.len() as u32);
        let stride = self.longitude.len() * 3;
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get().min(16));
        let rows_per_thread = self.latitude.len().div_ceil(threads);
        let limit = half_fov();
        let overlap = 2.0 * limit - PI;
        std::thread::scope(|scope| {
            for (band, bytes) in output.chunks_mut(stride * rows_per_thread).enumerate() {
                scope.spawn(move || {
                    for (row, bytes) in bytes.chunks_exact_mut(stride).enumerate() {
                        let (y, cos_lat) = self.latitude[band * rows_per_thread + row];
                        for (pixel, &(sin_lon, cos_lon)) in
                            bytes.chunks_exact_mut(3).zip(&self.longitude)
                        {
                            let x = sin_lon * cos_lat;
                            let angle = (cos_lon * cos_lat).clamp(-1.0, 1.0).acos();
                            let sample = if angle <= PI - limit {
                                self.lenses[0].sample(left, x, y, angle)
                            } else if angle >= limit {
                                self.lenses[1].sample(right, -x, y, PI - angle)
                            } else {
                                let a = self.lenses[0].sample(left, x, y, angle);
                                let b = self.lenses[1].sample(right, -x, y, PI - angle);
                                let t = (angle - (PI - limit)) / overlap;
                                let weight = t * t * (3.0 - 2.0 * t);
                                std::array::from_fn(|i| a[i] + (b[i] - a[i]) * weight)
                            };
                            pixel.copy_from_slice(&Rgb(sample.map(|c| c.round() as u8)).0);
                        }
                    }
                });
            }
        });
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scene(x: f32, y: f32, z: f32) -> Rgb<u8> {
        Rgb([
            (128.0 + 90.0 * x) as u8,
            (128.0 + 95.0 * y) as u8,
            (128.0 + 100.0 * z) as u8,
        ])
    }

    #[test]
    fn reconstructs_the_horizon_and_distinguishes_the_two_layouts() {
        const SIZE: u32 = 128;
        let panorama = RgbImage::from_fn(SIZE * 2, SIZE, |x, y| {
            let lon = ((x as f32 + 0.5) / (SIZE * 2) as f32 - 0.5) * TAU;
            let lat = (0.5 - (y as f32 + 0.5) / SIZE as f32) * PI;
            scene(lon.sin() * lat.cos(), lat.sin(), lon.cos() * lat.cos())
        });
        let packed = RgbImage::from_fn(SIZE * 2, SIZE, |x, y| {
            let sign = if x < SIZE { 1.0 } else { -1.0 };
            let u = (2.0 * (x % SIZE) as f32 + 1.0) / SIZE as f32 - 1.0;
            let v = 1.0 - (2.0 * y as f32 + 1.0) / SIZE as f32;
            let radius = u.hypot(v);
            if radius > 1.0 {
                return Rgb([0; 3]);
            }
            let angle = radius * 100_f32.to_radians();
            let scale = if radius > 0.0 {
                angle.sin() / radius
            } else {
                0.0
            };
            scene(sign * u * scale, v * scale, sign * angle.cos())
        });
        assert!(has_side_by_side_circles(&packed));
        assert!(!has_side_by_side_circles(&panorama));
        let result = Stitcher::side_by_side(packed.dimensions(), SIZE * 2).render(&packed, None);
        let left = image::imageops::crop_imm(&packed, 0, 0, SIZE, SIZE).to_image();
        let right = image::imageops::crop_imm(&packed, SIZE, 0, SIZE, SIZE).to_image();
        assert_eq!(
            result,
            Stitcher::separate(left.dimensions(), right.dimensions(), SIZE * 2)
                .render(&left, Some(&right))
        );
        for y in SIZE / 3..2 * SIZE / 3 {
            for x in 0..SIZE * 2 {
                let expected = panorama.get_pixel(x, y);
                let actual = result.get_pixel(x, y);
                for channel in 0..3 {
                    assert!(
                        expected[channel].abs_diff(actual[channel]) <= 7,
                        "({x}, {y})"
                    );
                }
            }
        }
    }
}
