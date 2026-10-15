//! The output window's picture, rendered headless through egui's wgpu renderer (#299): the program frame over the key
//! within 2 levels of the reference composite on the test card, and exactly 1920 × 1080 physical pixels at 100 %,
//! 125 % and 150 % scale. What needs a real desktop (the OS window's client area read back from the OS, an OS capture,
//! presenting while covered) is the owner's checklist on the PR.
//!
//! Set `OMUZAKI_OUTPUT_WINDOW_PNG=/some/dir` to write the rendered pictures.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use egui::{Color32, TextureOptions, vec2};
use egui_kittest::Harness;
use omuzaki_ui::aux_window::{PhysicalSize, points_for};
use omuzaki_ui::output_window::composite::image_over_key;
use omuzaki_ui::output_window::{DEFAULT_KEY, Program, paint_output, test_card};

/// The largest difference allowed between the window's pixels and the reference composite.
const TOLERANCE: u8 = 2;

/// Renders the output body at `size` physical pixels and `scale`, showing the test card of that size over `key`.
fn render(size: PhysicalSize, scale: f32, key: Color32) -> (image::RgbaImage, egui::ColorImage) {
    let card = test_card(size.width as usize, size.height as usize);
    let points = points_for(size, scale).expect("a usable scale");
    let shown = card.clone();
    let mut texture = None;
    let mut harness = Harness::builder().with_size(points).with_pixels_per_point(scale).wgpu().build_ui(|ui| {
        let handle =
            texture.get_or_insert_with(|| ui.ctx().load_texture("program", shown.clone(), TextureOptions::NEAREST));
        let program = Program { texture: handle.id(), size };
        paint_output(ui.painter(), ui.ctx().content_rect(), key, Some(program));
    });
    harness.run_steps(2);
    let image = harness.render().expect("a software or hardware adapter renders");
    if let Some(dir) = std::env::var_os("OMUZAKI_OUTPUT_WINDOW_PNG") {
        let path = std::path::Path::new(&dir).join(format!("output-{}x{}-{scale}.png", size.width, size.height));
        image.save(path).expect("the picture saves");
    }
    (image, card)
}

/// The largest per-channel difference between `image` and the card composited over `key` on the CPU.
fn worst_difference(image: &image::RgbaImage, card: &egui::ColorImage, key: Color32) -> u8 {
    let premultiplied: Vec<u8> = card.pixels.iter().flat_map(|p| p.to_array()).collect();
    let reference = image_over_key(&premultiplied, key);
    image
        .pixels()
        .zip(reference.chunks_exact(3))
        .flat_map(|(got, want)| (0..3).map(move |i| got.0[i].abs_diff(want[i])))
        .max()
        .unwrap_or(u8::MAX)
}

#[test]
fn the_program_over_the_key_is_within_2_levels_of_the_reference_composite() {
    for key in [DEFAULT_KEY, Color32::from_rgb(255, 0, 255), Color32::BLACK, Color32::WHITE] {
        let size = PhysicalSize::new(480, 270);
        let (image, card) = render(size, 1.0, key);
        assert_eq!(image.dimensions(), (480, 270));
        let worst = worst_difference(&image, &card, key);
        println!("key {key:?}: worst difference {worst} levels");
        assert!(worst <= TOLERANCE, "key {key:?}: a pixel is {worst} levels off");
    }
}

#[test]
fn the_output_is_exactly_1920_by_1080_physical_pixels_at_100_125_and_150_percent() {
    for scale in [1.0, 1.25, 1.5] {
        let (image, card) = render(PhysicalSize::FULL_HD, scale, DEFAULT_KEY);
        assert_eq!(image.dimensions(), (1920, 1080), "at {scale}");
        // At every scale the program pixels land one to one: the composite still matches.
        let worst = worst_difference(&image, &card, DEFAULT_KEY);
        println!("scale {scale}: {:?} physical pixels, worst difference {worst} levels", image.dimensions());
        assert!(worst <= TOLERANCE, "at {scale}: a pixel is {worst} levels off");
    }
}

#[test]
fn a_program_of_another_aspect_is_fitted_and_the_rest_is_key() {
    let size = PhysicalSize::new(320, 180);
    let card = test_card(90, 90);
    let mut texture = None;
    let mut harness = Harness::builder().with_size(vec2(320.0, 180.0)).wgpu().build_ui(|ui| {
        let handle =
            texture.get_or_insert_with(|| ui.ctx().load_texture("square", card.clone(), TextureOptions::NEAREST));
        let program = Program { texture: handle.id(), size: PhysicalSize::new(90, 90) };
        paint_output(ui.painter(), ui.ctx().content_rect(), DEFAULT_KEY, Some(program));
    });
    harness.run_steps(2);
    let image = harness.render().expect("renders");
    assert_eq!(image.dimensions(), (size.width, size.height));
    let key = DEFAULT_KEY.to_array();
    // The square is centred at full height: the side bands are pure key.
    for (x, y) in [(5, 90), (314, 90), (60, 10)] {
        assert_eq!(&image.get_pixel(x, y).0[..3], &key[..3], "({x}, {y}) is key");
    }
}
