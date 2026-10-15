//! Chrome-canvas probes for how the CPU rasteriser covers a path whose edges share pixels or run long: a contour drawn
//! twice, and paths with many points over a wide span (the case Skia's `SkScan::AntiFillPath` hands to its 4-sub-scanline
//! supersampler when the build has one).
//! Every Chrome value is asserted inline, measured with headless Chromium 141.0.7390.37 in software (through
//! Playwright's `chromium.launch`); each test's doc comment holds the script that draws it, so a run of it can be compared.
//! The rule is `raster_probes`'s: 6 levels per channel.
//!
//! What the sweep of point count against width found (#828): this Chromium fills such paths with the analytic coverage
//! at every size tried, never with the supersampler. A vertical edge at x = 10.3 reads 180 (analytic, 0.703) from 5 up to
//! 1500 points at widths 60 to 1000 px; supersampling would snap x to a quarter pixel and read 192. So the rasteriser has
//! no supersampling fallback, and the probes below pin the analytic values on long, many-point paths.
//!
//! Chrome's canvas fills a rectangle by its own fast path with exact coverage, so a probe path here is never a rectangle.

// Test code: a panic is a clear failure.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss)] // image sizes and point indices are tiny

use omuzaki_core::glam::Vec2;
use omuzaki_face::raster::SkiaRasterizer;
use omuzaki_face_api::{Cmd, Colour, DrawList, FaceLayerId, FaceLayerSpec, FaceRasterizer, FaceRect, Paint, Raster};

/// Levels per channel a pixel may differ from Chrome's (the rule of `raster_probes`).
const TOLERANCE: i32 = 6;
const PX_PER_M: f32 = 1000.0;

/// Fills the closed `contours` (pixel coordinates, one path, non-zero) white on a `width` x `height` image and returns
/// the alpha plane. Pixel (x, y) is plane (x, -y) / 1000 m, so the plane rect is the image.
fn alpha(width: u32, height: u32, contours: &[&[[f32; 2]]]) -> Vec<u8> {
    let mut list = DrawList::new();
    let mut path = list.path();
    for points in contours {
        let points: Vec<Vec2> = points.iter().map(|p| Vec2::new(p[0] / PX_PER_M, -p[1] / PX_PER_M)).collect();
        path = path.polyline(&points).close();
    }
    let path = path.finish();
    list.push(Cmd::Fill { path, paint: Paint::Solid(Colour::rgba(255, 255, 255, 255)), opacity: 1.0 });
    let rect = FaceRect { x0: 0.0, x1: width as f32 / PX_PER_M, y0: -(height as f32) / PX_PER_M, y1: 0.0 };
    let spec = FaceLayerSpec::for_rect(rect, FaceLayerId::Base, PX_PER_M);
    let mut raster = Raster::new(width, height);
    SkiaRasterizer::new().rasterize(&spec, &list, PX_PER_M, &mut raster).unwrap();
    assert_eq!((raster.width, raster.height), (width, height));
    raster.pixels.iter().skip(3).step_by(4).copied().collect()
}

/// Asserts `alphas` (a width-wide plane) holds Chrome's value at every `(x, y, chrome)` within the rule.
fn assert_matches_chrome(alphas: &[u8], width: u32, expected: &[(u32, u32, u8)]) {
    for &(x, y, chrome) in expected {
        let ours = alphas[(y * width + x) as usize];
        assert!((i32::from(ours) - i32::from(chrome)).abs() <= TOLERANCE, "({x}, {y}): ours {ours}, Chrome {chrome}");
    }
}

/// #695: a contour drawn twice in one path (same winding) is as covered as drawn once, also at partly covered edges.
/// Chrome does not add the layers' partial areas. Drawn by
///
/// ```js
/// const sq = [[5.3, 4.6], [17.8, 4.6], [17.8, 12.9], [5.3, 12.9]];
/// g.beginPath();
/// for (let k = 0; k < 2; k++) { g.moveTo(...sq[0]); for (let i = 1; i < 4; i++) g.lineTo(...sq[i]); g.closePath(); }
/// g.fill('nonzero');   // 40 x 30 canvas, read with getImageData(x, y, 1, 1).data[3]
/// ```
#[test]
fn a_contour_drawn_twice_is_as_covered_as_drawn_once() {
    let square = [[5.3, 4.6], [17.8, 4.6], [17.8, 12.9], [5.3, 12.9]];
    let twice = alpha(40, 30, &[&square, &square]);
    // corner, top edge, left edge, right edge, corner, bottom edge, interior: Chrome 141's alphas.
    assert_matches_chrome(
        &twice,
        40,
        &[(5, 4, 90), (10, 4, 128), (5, 8, 180), (17, 8, 204), (17, 12, 204), (10, 12, 255), (10, 8, 255)],
    );
}

/// A path of 400 points over 236 px keeps its analytic edge: a vertical edge at x = 10.3 reads 180 (0.703 of the pixel).
/// Skia's supersampler would snap the edge to a quarter pixel and read 192, 12 levels off. The points zigzag along the top
/// so none is collinear. Drawn by
///
/// ```js
/// const n = 400, w = 236, L = 10.3, R = L + w, B = 30;   // 276 x 40 canvas
/// g.beginPath(); g.moveTo(L, B); g.lineTo(L, 6);
/// for (let i = 1; i <= n - 4; i++) { const t = i / (n - 3); g.lineTo(L + (R - L) * t, 6 + (i % 2 ? 0 : 1.3)); }
/// g.lineTo(R, 6); g.lineTo(R, B); g.closePath(); g.fill();
/// // getImageData(10, 20, 1, 1).data[3] -> 180;  getImageData(w + 10, 20, 1, 1).data[3] -> 76
/// ```
#[test]
fn a_path_of_many_points_keeps_its_analytic_edge() {
    let (n, w, left, right, bottom) = (400_usize, 236.0_f32, 10.3_f32, 246.3_f32, 30.0_f32);
    let mut points = vec![[left, bottom], [left, 6.0]];
    for i in 1..=n - 4 {
        let t = i as f32 / (n - 3) as f32;
        points.push([left + w * t, 6.0 + if i % 2 == 1 { 0.0 } else { 1.3 }]);
    }
    points.extend([[right, 6.0], [right, bottom]]);
    assert_eq!(points.len(), n);
    let image = alpha(276, 40, &[&points]);
    assert_matches_chrome(&image, 276, &[(10, 20, 180), (246, 20, 76)]);
}

/// A 46-point sliver 236 px wide, the size of the prototype's brow: Chrome 141 fills it analytically, so its edge alphas
/// are not the quarters (64, 128, 192) a supersampler gives. Drawn by
///
/// ```js
/// const pts = [], r = (v) => +v.toFixed(2);   // 256 x 64 canvas; both coordinates rounded to 2 decimals, as below
/// for (let i = 0; i < 23; i++) { const t = i / 22; pts.push([r(10.37 + 236 * t), r(40.13 - 14 * Math.sin(Math.PI * t) - 6 * t)]); }
/// for (let i = 22; i >= 0; i--) { const t = i / 22; pts.push([r(10.37 + 236 * t), r(40.13 - 14 * Math.sin(Math.PI * t) - 6 * t + 3 + 5 * Math.sin(Math.PI * t))]); }
/// g.beginPath(); g.moveTo(...pts[0]); for (const p of pts.slice(1)) g.lineTo(...p); g.closePath(); g.fill();
/// ```
#[test]
fn a_46_point_sliver_matches_chrome_edge_for_edge() {
    let sliver = [
        [10.37, 40.13],
        [21.1, 37.86],
        [31.82, 35.64],
        [42.55, 33.5],
        [53.28, 31.47],
        [64.01, 29.6],
        [74.73, 27.91],
        [85.46, 26.44],
        [96.19, 25.21],
        [106.92, 24.24],
        [117.64, 23.55],
        [128.37, 23.13],
        [139.1, 23.0],
        [149.82, 23.15],
        [160.55, 23.58],
        [171.28, 24.26],
        [182.01, 25.19],
        [192.73, 26.33],
        [203.46, 27.65],
        [214.19, 29.13],
        [224.92, 30.73],
        [235.64, 32.41],
        [246.37, 34.13],
        [246.37, 37.13],
        [235.64, 36.12],
        [224.92, 35.14],
        [214.19, 34.21],
        [203.46, 33.36],
        [192.73, 32.6],
        [182.01, 31.96],
        [171.28, 31.47],
        [160.55, 31.13],
        [149.82, 30.95],
        [139.1, 30.95],
        [128.37, 31.13],
        [117.64, 31.49],
        [106.92, 32.04],
        [96.19, 32.76],
        [85.46, 33.65],
        [74.73, 34.69],
        [64.01, 35.87],
        [53.28, 37.17],
        [42.55, 38.57],
        [31.82, 40.05],
        [21.1, 41.58],
        [10.37, 43.13],
    ];
    assert_eq!(sliver.len(), 46);
    let image = alpha(256, 64, &[&sliver]);
    // The first and last partly covered pixel of columns 20, 60, 100, 180, 220 and 240: Chrome 141's alphas.
    assert_matches_chrome(
        &image,
        256,
        &[
            (20, 37, 26),
            (20, 41, 153),
            (60, 30, 217),
            (60, 36, 61),
            (100, 24, 39),
            (100, 32, 115),
            (180, 25, 228),
            (180, 31, 239),
            (220, 30, 222),
            (220, 34, 215),
            (240, 33, 181),
            (240, 36, 145),
        ],
    );
}

/// Same-winding layers whose edges cross one pixel at different places cover it from the outer edge. Chrome 141 reads
/// 205 (0.8) where the left edges are at 5.2 and 5.8, and 204 where the right edges are at 17.2 and 17.8; the average
/// of the layers would give 0.5, the sum 1.0. Drawn by
///
/// ```js
/// const rect = (x0, y0, x1, y1) => [[x0, y0], [x1, y0], [x1, y1], [x0, y1]];   // 40 x 30 canvas
/// g.beginPath();
/// for (const pts of [rect(5.2, 4, 17.2, 12), rect(5.8, 4, 17.8, 12)]) {
///   g.moveTo(...pts[0]); for (const q of pts.slice(1)) g.lineTo(...q); g.closePath();
/// }
/// g.fill('nonzero');   // getImageData(5, 8) -> 205; (17, 8) -> 204; (6, 8) -> 255
/// ```
#[test]
fn offset_same_winding_layers_cover_from_the_outer_edge() {
    let first = [[5.2, 4.0], [17.2, 4.0], [17.2, 12.0], [5.2, 12.0]];
    let second = [[5.8, 4.0], [17.8, 4.0], [17.8, 12.0], [5.8, 12.0]];
    let image = alpha(40, 30, &[&first, &second]);
    assert_matches_chrome(&image, 40, &[(5, 8, 205), (17, 8, 204), (6, 8, 255)]);
}

/// A hole stays a hole: an outer square and an opposite-wound inner one (counter-clockwise on screen). Chrome 141 reads
/// 102 and 26 on the hole's left and right edge pixels (the ring covers 0.4 and 0.1 of them) and 0 inside. Drawn by
///
/// ```js
/// const rect = (x0, y0, x1, y1) => [[x0, y0], [x1, y0], [x1, y1], [x0, y1]];   // 40 x 30 canvas
/// const contours = [rect(4, 4, 30, 25), rect(10.4, 10, 22.9, 19).reverse()];
/// g.beginPath();
/// for (const pts of contours) { g.moveTo(...pts[0]); for (const q of pts.slice(1)) g.lineTo(...q); g.closePath(); }
/// g.fill('nonzero');   // getImageData(10, 12) -> 102; (22, 12) -> 26; (15, 12) -> 0
/// ```
#[test]
fn an_opposite_wound_hole_stays_a_hole() {
    let outer = [[4.0, 4.0], [30.0, 4.0], [30.0, 25.0], [4.0, 25.0]];
    let inner = [[10.4, 19.0], [22.9, 19.0], [22.9, 10.0], [10.4, 10.0]];
    let image = alpha(40, 30, &[&outer, &inner]);
    assert_matches_chrome(&image, 40, &[(10, 12, 102), (22, 12, 26), (15, 12, 0)]);
}

/// Draws `contours` (rectangles as `[x0, y0, x1, y1]`, one path, non-zero) on a 40 x 30 canvas, as the scripts below do.
fn rects(contours: &[[f32; 4]]) -> Vec<u8> {
    let polygons: Vec<[[f32; 2]; 4]> =
        contours.iter().map(|&[x0, y0, x1, y1]| [[x0, y0], [x1, y0], [x1, y1], [x0, y1]]).collect();
    let refs: Vec<&[[f32; 2]]> = polygons.iter().map(<[[f32; 2]; 4]>::as_slice).collect();
    alpha(40, 30, &refs)
}

/// A rectangle drawn twice with a partly covered left edge is half covered there, not opaque: summing the layers' area
/// gives a whole number that is not full coverage. Chrome 141 reads 128 at (5, 8) and (17, 8). Drawn by
///
/// ```js
/// const rect = (x0, y0, x1, y1) => [[x0, y0], [x1, y0], [x1, y1], [x0, y1]];   // 40 x 30 canvas
/// g.beginPath();
/// for (const pts of [rect(5.5, 4, 17.5, 13), rect(5.5, 4, 17.5, 13)]) {
///   g.moveTo(...pts[0]); for (const q of pts.slice(1)) g.lineTo(...q); g.closePath();
/// }
/// g.fill('nonzero');   // getImageData(5, 8) -> 128; (17, 8) -> 128; (10, 8) -> 255
/// ```
#[test]
fn a_duplicated_half_covered_edge_stays_half_covered() {
    let image = rects(&[[5.5, 4.0, 17.5, 13.0], [5.5, 4.0, 17.5, 13.0]]);
    assert_matches_chrome(&image, 40, &[(5, 8, 128), (17, 8, 128), (10, 8, 255)]);
}

/// Two same-winding rectangles whose left edges (5.25, 5.5) cross one pixel cover it to the union, 0.75 (192), not the
/// average 0.625. Drawn as above with `rect(5.25, 4, 17, 13)` and `rect(5.5, 4, 17, 13)`: getImageData(5, 8) -> 192.
#[test]
fn nested_left_edges_in_one_pixel_cover_to_the_outer_one() {
    let image = rects(&[[5.25, 4.0, 17.0, 13.0], [5.5, 4.0, 17.0, 13.0]]);
    assert_matches_chrome(&image, 40, &[(5, 8, 192), (10, 8, 255)]);
}

/// A sliver narrower than a pixel, drawn twice, has no fully covered pixel to clamp: both layers cover the same quarter
/// of it, so Chrome 141 reads 64 (the single sliver reads 64 too), not 128. Drawn as above with
/// `rect(5.125, 4, 5.375, 13)` twice: getImageData(5, 8) -> 64.
#[test]
fn a_duplicated_sliver_is_not_doubled() {
    let image = rects(&[[5.125, 4.0, 5.375, 13.0], [5.125, 4.0, 5.375, 13.0]]);
    assert_matches_chrome(&image, 40, &[(5, 8, 64)]);
}
