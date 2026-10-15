//! The clean window's decisions: the exact size at any DPI, and the key colour pick.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_core::colour::Colour;
use omuzaki_core::glam::UVec2;
use omuzaki_stream::window::{
    COLLISION_MARGIN, DEFAULT_KEY, DEFAULT_TITLE, MAX_SIDE, SizePolicy, WindowError, WindowSettings, check_title,
    pick_key, plan,
};

const FHD: UVec2 = UVec2::new(1920, 1080);

#[test]
fn output_policy_is_exact_at_any_scale_factor() {
    for (factor, logical) in [(1.0, (1920.0, 1080.0)), (1.25, (1536.0, 864.0)), (1.5, (1280.0, 720.0))] {
        let planned = plan(FHD, SizePolicy::Output, factor).unwrap();
        assert_eq!((planned.physical, planned.logical, planned.scale_factor), (FHD, logical, factor));
    }
    for step in 0..=70 {
        let factor = 0.5 + f64::from(step) * 0.05;
        assert_eq!(plan(FHD, SizePolicy::Output, factor).unwrap().physical, FHD, "factor {factor}");
    }
}

#[test]
fn scale_policy_rounds_the_output_size() {
    let planned = plan(FHD, SizePolicy::Scale(0.5), 2.0).unwrap();
    assert_eq!((planned.physical, planned.logical), (UVec2::new(960, 540), (480.0, 270.0)));
    assert_eq!(plan(UVec2::new(1001, 3), SizePolicy::Scale(0.5), 1.0).unwrap().physical, UVec2::new(501, 2));
}

#[test]
fn scale_policy_keeps_each_side_within_one_and_the_maximum() {
    assert_eq!(plan(UVec2::new(4, 4), SizePolicy::Scale(0.1), 1.0).unwrap().physical, UVec2::new(1, 1));
    assert_eq!(
        plan(UVec2::new(7680, 4320), SizePolicy::Scale(2.0), 1.0).unwrap().physical,
        UVec2::new(MAX_SIDE, MAX_SIDE)
    );
}

#[test]
fn bad_factors_scales_and_outputs_are_errors() {
    assert_eq!(plan(FHD, SizePolicy::Scale(0.0), 1.0), Err(WindowError::Scale(0.0)));
    assert!(matches!(plan(FHD, SizePolicy::Scale(f32::NAN), 1.0), Err(WindowError::Scale(_))));
    assert!(matches!(plan(FHD, SizePolicy::Scale(2.5), 1.0), Err(WindowError::Scale(_))));
    assert_eq!(plan(FHD, SizePolicy::Output, 5.0), Err(WindowError::ScaleFactor(5.0)));
    assert!(matches!(plan(FHD, SizePolicy::Output, f64::NAN), Err(WindowError::ScaleFactor(_))));
    assert_eq!(plan(FHD, SizePolicy::Output, 0.0), Err(WindowError::ScaleFactor(0.0)));
    assert_eq!(plan(FHD, SizePolicy::Output, 0.4), Err(WindowError::ScaleFactor(0.4)));
    assert_eq!(plan(UVec2::new(0, 1080), SizePolicy::Output, 1.0), Err(WindowError::EmptyOutput(0, 1080)));
}

fn colour(hex: &str) -> Colour {
    hex.parse().unwrap()
}

fn assert_pick(palette: &[&str], key: &str, margin: f32) {
    let palette: Vec<Colour> = palette.iter().map(|c| colour(c)).collect();
    let choice = pick_key(&palette);
    assert_eq!(choice.colour, colour(key), "{palette:?}");
    assert!((choice.margin - margin).abs() < 0.1, "{palette:?}: margin {} not {margin}", choice.margin);
    assert!(!choice.collides, "{palette:?}");
}

#[test]
fn pick_key_avoids_the_palette() {
    assert_pick(&["#00b140"], "#ff00ff", 615.5);
    assert_pick(&["#ff69b4", "#ffffff"], "#00b140", 465.6);
    assert_pick(&["#008080", "#000080"], "#ff00ff", 450.3);
}

#[test]
fn pick_key_collides_when_every_candidate_is_taken() {
    let choice = pick_key(&[colour("#00b140"), colour("#ff00ff"), colour("#0047bb")]);
    assert_eq!((choice.colour, choice.margin, choice.collides), (DEFAULT_KEY, 0.0, true));
}

#[test]
fn pick_key_on_an_empty_palette_is_chroma_green_without_collision() {
    let choice = pick_key(&[]);
    assert_eq!((choice.colour, choice.margin, choice.collides), (DEFAULT_KEY, f32::INFINITY, false));
}

#[test]
fn a_close_but_not_equal_palette_colour_collides_below_the_margin() {
    // Near-green skin tones are the case the margin exists for: close to #00b140 and to nothing else.
    let choice = pick_key(&[colour("#10b040"), colour("#fe00fe"), colour("#0047bb")]);
    assert!(choice.margin < COLLISION_MARGIN && choice.collides, "{choice:?}");
}

#[test]
fn alpha_does_not_change_the_pick() {
    assert_eq!(pick_key(&[Colour::rgba(0, 0xb1, 0x40, 10)]), pick_key(&[colour("#00b140")]));
}

#[test]
fn titles_are_one_to_sixty_four_printable_characters() {
    assert!(check_title(DEFAULT_TITLE).is_ok() && check_title("x").is_ok());
    assert!(check_title(&"é".repeat(64)).is_ok(), "the limit counts characters, not bytes");
    for bad in ["", &"x".repeat(65), "a\nb", "a\u{7f}b", "a\0b"] {
        assert_eq!(check_title(bad), Err(WindowError::Title), "{bad:?}");
    }
}

#[test]
fn the_default_settings_are_the_documented_ones() {
    let s = WindowSettings::default();
    assert_eq!((s.title.as_str(), s.size, s.hide_cursor), ("Omuzaki Output", SizePolicy::Output, true));
    assert_eq!(s.backdrop, omuzaki_stream::window::Backdrop::Key(colour("#00b140")));
}
