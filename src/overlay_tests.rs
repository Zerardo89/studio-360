use image::{Rgba, RgbaImage};

use crate::overlay::{composite, rasterize_text, Layer, Overlays};

#[test]
fn sticker_composes_alpha_and_respects_time_and_visibility() {
    let sticker = Layer::sticker(
        "Cuore",
        RgbaImage::from_pixel(1, 1, Rgba([200, 20, 40, 128])),
    );
    let layers = Overlays {
        layers: vec![sticker],
        show_stickers: true,
    };
    let mut frame = RgbaImage::from_pixel(5, 5, Rgba([20, 40, 100, 255]));
    composite(frame.as_mut(), (5, 5), 4, &layers, 0.0);
    assert_eq!(frame.get_pixel(2, 2).0, [110, 30, 70, 255]);
    assert_eq!(frame.get_pixel(0, 0).0, [20, 40, 100, 255]);
    let mut layers = layers;
    layers.layers[0].start = 2.0;
    let mut blank = RgbaImage::from_pixel(5, 5, Rgba([20, 40, 100, 255]));
    composite(blank.as_mut(), (5, 5), 4, &layers, 1.0);
    layers.show_stickers = false;
    composite(blank.as_mut(), (5, 5), 4, &layers, 3.0);
    assert_eq!(blank.get_pixel(2, 2).0, [20, 40, 100, 255]);
}

#[test]
fn text_rasterizes_accents_and_handles_empty_and_emoji() {
    for text in ["Città", "😀"] {
        let image = rasterize_text(text, [255; 4], [0, 0, 0, 255], 3);
        assert!(image.pixels().any(|p| p[3] > 0), "{text}");
    }
    assert!(rasterize_text("", [255; 4], [0; 4], 0)
        .pixels()
        .all(|p| p[3] == 0));
}
