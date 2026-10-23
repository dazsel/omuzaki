//! `EdgeLines::coverage`, the one definition of where depth-jump lines fall (#2319).

use omuzaki_render_api::{Colour, EdgeLines};

const W: i32 = 12;

/// A 12-pixel-wide picture of one row, its depth given per column (`INFINITY` for the background).
fn coverage(lines: &EdgeLines, row: &[f32], scale: f32) -> Vec<f32> {
    let depth = |x: i32, y: i32| (y == 0 && (0..W).contains(&x)).then(|| row[usize::try_from(x).unwrap_or(0)]);
    (0..W).map(|x| lines.coverage(depth, x, 0, scale)).collect()
}

#[test]
fn the_band_lies_on_the_near_side_of_a_jump_and_is_width_px_wide() {
    let lines = EdgeLines::new(Colour::BLACK, 1.5);
    // A chin at 1 m over a neck at 1.06 m.
    let row: Vec<f32> = (0..W).map(|x| if x < 6 { 1.0 } else { 1.06 }).collect();
    let got = coverage(&lines, &row, 1.0);
    assert_eq!(&got[..4], &[0.0; 4]);
    assert!((got[4] - 0.5).abs() < 1e-6, "the second pixel in is half covered: {got:?}");
    assert!((got[5] - 1.0).abs() < 1e-6, "the pixel next to the jump is covered: {got:?}");
    assert!(got[6..].iter().all(|c| *c == 0.0), "the far side draws nothing: {got:?}");
}

#[test]
fn a_silhouette_against_the_background_is_a_jump_and_the_picture_edge_is_not() {
    let lines = EdgeLines::new(Colour::BLACK, 1.0);
    let row: Vec<f32> = (0..W).map(|x| if (3..9).contains(&x) { 2.0 } else { f32::INFINITY }).collect();
    let got = coverage(&lines, &row, 1.0);
    assert_eq!(got, [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0]);
    let full = coverage(&lines, &[2.0; W as usize], 1.0);
    assert!(full.iter().all(|c| *c == 0.0), "a surface that runs off the picture has no line at the edge: {full:?}");
}

#[test]
fn a_steep_slope_continues_itself_and_draws_nothing() {
    let lines = EdgeLines::new(Colour::BLACK, 2.0);
    // 10 cm deeper per pixel: far more than 2% of the depth, but a plane.
    let row: Vec<f32> = (0..W).map(|x| 1.0 + 0.1 * x as f32).collect();
    assert!(coverage(&lines, &row, 1.0).iter().all(|c| *c == 0.0));
    // A small jump below the step draws nothing either.
    let row: Vec<f32> = (0..W).map(|x| if x < 6 { 1.0 } else { 1.01 }).collect();
    assert!(coverage(&lines, &row, 1.0).iter().all(|c| *c == 0.0));
}

#[test]
fn a_supersampled_picture_draws_the_same_width_in_output_pixels() {
    let lines = EdgeLines::new(Colour::BLACK, 1.0);
    let row: Vec<f32> = (0..W).map(|x| if x < 6 { 1.0 } else { 1.5 }).collect();
    let got = coverage(&lines, &row, 2.0);
    assert_eq!(&got[3..7], &[0.0, 1.0, 1.0, 0.0], "two internal pixels make one output pixel: {got:?}");
}
