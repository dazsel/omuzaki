//! The premultiplied test image, drawn in code, and the geometry that maps it into a window.
//!
//! The picture is built to expose every way a transparent window can go wrong: a disc with a soft feathered edge (a
//! wrong alpha convention shows as a dark or light fringe), a hard one-pixel white ring and black ring (fringe over a
//! white or black desktop), a half transparent glass panel (blending), a row of alpha steps (0, 64, 128, 192, 255) and
//! a fully transparent margin (click-through and "is the background really see-through").

/// Width and height of the test image, in pixels.
pub const SIZE: u32 = 512;

/// Alpha at or above which a pixel counts as part of the mascot for hit testing (about 3%).
pub const HIT_ALPHA: u8 = 8;

/// Alpha values of the swatch row, left to right.
pub const SWATCH_ALPHAS: [u8; 5] = [0, 64, 128, 192, 255];

/// A rectangle in window pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    /// Left edge.
    pub x: u32,
    /// Top edge.
    pub y: u32,
    /// Width.
    pub w: u32,
    /// Height.
    pub h: u32,
}

/// An RGBA8 image with premultiplied alpha, row-major, top row first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scene {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height * 4` bytes of premultiplied RGBA.
    pub pixels: Vec<u8>,
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Source-over of a straight-alpha colour onto the premultiplied pixel `dst`.
fn over(dst: &mut [f32; 4], rgb: [f32; 3], alpha: f32) {
    let keep = 1.0 - alpha;
    for (d, c) in dst.iter_mut().zip(rgb) {
        *d = c * alpha + *d * keep;
    }
    dst[3] = alpha + dst[3] * keep;
}

impl Scene {
    /// Draws the test image. Deterministic: the same bytes on every call and machine.
    pub fn generate() -> Self {
        let n = SIZE as usize;
        let mut pixels = vec![0_u8; n * n * 4];
        for y in 0..SIZE {
            for x in 0..SIZE {
                let px = shade(x, y);
                let i = ((y * SIZE + x) * 4) as usize;
                for (slot, v) in pixels[i..i + 4].iter_mut().zip(px) {
                    // Rounding to the nearest 8-bit value keeps r, g, b <= a, the premultiplied invariant.
                    *slot = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
                }
            }
        }
        Self { width: SIZE, height: SIZE, pixels }
    }

    /// The alpha byte at pixel `(x, y)`, or 0 outside the image.
    pub fn alpha(&self, x: u32, y: u32) -> u8 {
        if x >= self.width || y >= self.height {
            return 0;
        }
        self.pixels[((y * self.width + x) * 4 + 3) as usize]
    }

    /// Composites the image over a flat background (`[r, g, b]` bytes) and returns opaque RGBA8. This is what a correct
    /// compositor puts on screen, so a screenshot of the real window can be compared with it.
    pub fn over_background(&self, bg: [u8; 3]) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.pixels.len());
        for px in self.pixels.as_chunks::<4>().0 {
            let keep = 255 - u32::from(px[3]);
            for (c, b) in px[..3].iter().zip(bg) {
                // Premultiplied source-over: out = src + dst * (1 - a), with rounding.
                out.push((u32::from(*c) + (u32::from(b) * keep + 127) / 255).min(255) as u8);
            }
            out.push(255);
        }
        out
    }
}

/// The colour of one pixel, premultiplied, in 0..=1 floats.
fn shade(x: u32, y: u32) -> [f32; 4] {
    let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
    let mid = SIZE as f32 / 2.0;
    let mut px = [0.0_f32; 4];

    // Glass panel: a blue rectangle at 50% alpha, partly over the disc and partly over nothing.
    if (230.0..470.0).contains(&fx) && (250.0..420.0).contains(&fy) {
        over(&mut px, [0.10, 0.35, 0.95], 0.5);
    }

    // Disc with a feathered edge: alpha falls from 1 to 0 over 14 pixels, centred on radius 170.
    let d = ((fx - mid + 30.0).powi(2) + (fy - mid + 40.0).powi(2)).sqrt();
    let disc = 1.0 - smoothstep(163.0, 177.0, d);
    if disc > 0.0 {
        // Warm gradient so a colour shift at the fringe is visible.
        let t = (fy / SIZE as f32).clamp(0.0, 1.0);
        over(&mut px, [1.0 - 0.3 * t, 0.55 - 0.25 * t, 0.15], disc);
    }

    // One-pixel hard rings, white and black, concentric with the disc: the classic fringe probes.
    let ring = |r: f32| (d - r).abs() < 0.5 + f32::EPSILON;
    if ring(120.0) {
        over(&mut px, [1.0, 1.0, 1.0], 1.0);
    }
    if ring(110.0) {
        over(&mut px, [0.0, 0.0, 0.0], 1.0);
    }

    // Swatches: five 60x40 squares of white at known alpha along the bottom, 20 px apart.
    if (440.0..480.0).contains(&fy) {
        for (i, a) in SWATCH_ALPHAS.iter().enumerate() {
            let left = 40.0 + i as f32 * 80.0;
            if (left..left + 60.0).contains(&fx) {
                over(&mut px, [1.0, 1.0, 1.0], f32::from(*a) / 255.0);
            }
        }
    }
    px
}

/// How the image sits in a window: uniformly scaled to fit and centred, so a window of another size or shape shows the
/// same picture with transparent bands, and the GPU draw and the CPU hit test agree on where every pixel is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fit {
    /// Window size in pixels.
    pub window: (u32, u32),
    /// Image size in pixels.
    pub image: (u32, u32),
    /// Window pixels per image pixel.
    pub scale: f32,
    /// Top-left corner of the image in window pixels.
    pub origin: (f32, f32),
}

impl Fit {
    /// Fits an `image`-sized picture into a `window`-sized area.
    pub fn new(window: (u32, u32), image: (u32, u32)) -> Self {
        let scale = (window.0 as f32 / image.0 as f32).min(window.1 as f32 / image.1 as f32);
        let origin =
            ((window.0 as f32 - image.0 as f32 * scale) / 2.0, (window.1 as f32 - image.1 as f32 * scale) / 2.0);
        Self { window, image, scale, origin }
    }

    /// The image pixel under window pixel `(x, y)`, or `None` in the bands around the image.
    pub fn to_image(&self, x: f32, y: f32) -> Option<(u32, u32)> {
        let ix = ((x - self.origin.0) / self.scale).floor();
        let iy = ((y - self.origin.1) / self.scale).floor();
        let inside = ix >= 0.0 && iy >= 0.0 && ix < self.image.0 as f32 && iy < self.image.1 as f32;
        inside.then_some((ix as u32, iy as u32))
    }

    /// Whether the mascot (alpha at or above [`HIT_ALPHA`]) is under window pixel `(x, y)`.
    pub fn hit(&self, scene: &Scene, x: f32, y: f32) -> bool {
        self.to_image(x, y).is_some_and(|(ix, iy)| scene.alpha(ix, iy) >= HIT_ALPHA)
    }

    /// The mascot's footprint as horizontal runs of window pixels, for an OS input region (X11 input shape, a Wayland
    /// input region). One rectangle per run per row, merged vertically where consecutive rows have identical runs.
    pub fn input_rects(&self, scene: &Scene) -> Vec<Rect> {
        let mut rects: Vec<Rect> = Vec::new();
        let mut previous: Vec<Rect> = Vec::new();
        for y in 0..self.window.1 {
            let mut row: Vec<Rect> = Vec::new();
            let mut start: Option<u32> = None;
            for x in 0..=self.window.0 {
                let on = x < self.window.0 && self.hit(scene, x as f32 + 0.5, y as f32 + 0.5);
                match (on, start) {
                    (true, None) => start = Some(x),
                    (false, Some(s)) => {
                        row.push(Rect { x: s, y, w: x - s, h: 1 });
                        start = None;
                    }
                    _ => {}
                }
            }
            // Extend the rectangles of the previous row that this row repeats exactly; start new ones for the rest.
            let mut carried: Vec<Rect> = Vec::new();
            for run in row {
                match previous.iter().position(|p| p.x == run.x && p.w == run.w && p.y + p.h == y) {
                    Some(i) => {
                        let mut grown = previous.swap_remove(i);
                        grown.h += 1;
                        carried.push(grown);
                    }
                    None => carried.push(run),
                }
            }
            rects.append(&mut previous);
            previous = carried;
        }
        rects.append(&mut previous);
        rects
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_is_deterministic() {
        assert_eq!(Scene::generate(), Scene::generate());
    }

    #[test]
    fn pixels_are_premultiplied() {
        let scene = Scene::generate();
        for px in scene.pixels.as_chunks::<4>().0 {
            assert!(px[0] <= px[3] && px[1] <= px[3] && px[2] <= px[3], "{px:?}");
        }
    }

    #[test]
    fn covers_the_alpha_range() {
        let scene = Scene::generate();
        let alphas: Vec<u8> = scene.pixels.as_chunks::<4>().0.iter().map(|p| p[3]).collect();
        assert!(alphas.contains(&0) && alphas.contains(&255));
        assert!(alphas.iter().any(|a| (100..=160).contains(a)), "no half transparent pixels");
        // The feathered edge has many distinct partial alphas, the hard rings none of their own.
        let mut distinct = alphas.clone();
        distinct.sort_unstable();
        distinct.dedup();
        assert!(distinct.len() > 100, "{}", distinct.len());
    }

    #[test]
    fn swatches_hold_their_alpha() {
        let scene = Scene::generate();
        for (i, a) in SWATCH_ALPHAS.iter().enumerate() {
            let x = 40 + i as u32 * 80 + 30;
            assert_eq!(scene.alpha(x, 460), *a, "swatch {i}");
        }
    }

    #[test]
    fn corners_are_transparent_and_centre_is_opaque() {
        let scene = Scene::generate();
        for (x, y) in [(0, 0), (SIZE - 1, 0), (0, SIZE - 1), (SIZE - 1, SIZE - 1)] {
            assert_eq!(scene.alpha(x, y), 0);
        }
        assert_eq!(scene.alpha(SIZE / 2 - 30, SIZE / 2 - 40 + 60), 255);
    }

    #[test]
    fn over_white_and_black_agree_with_the_formula() {
        let scene = Scene::generate();
        let white = scene.over_background([255; 3]);
        let black = scene.over_background([0; 3]);
        for ((s, w), b) in
            scene.pixels.as_chunks::<4>().0.iter().zip(white.as_chunks::<4>().0).zip(black.as_chunks::<4>().0)
        {
            assert_eq!(b[..3], s[..3], "over black is the premultiplied colour itself");
            assert_eq!(w[3], 255);
            // Over white, every channel is the colour plus the uncovered white: at least the colour, at most 255.
            assert!(w[..3].iter().zip(&s[..3]).all(|(w, c)| w >= c));
            if s[3] == 0 {
                assert_eq!(w[..3], [255, 255, 255]);
            }
        }
    }

    #[test]
    fn fit_is_identity_for_a_matching_window() {
        let fit = Fit::new((SIZE, SIZE), (SIZE, SIZE));
        assert_eq!(fit.to_image(10.5, 20.5), Some((10, 20)));
        assert_eq!(fit.to_image(-1.0, 3.0), None);
        assert_eq!(fit.to_image(SIZE as f32, 3.0), None);
    }

    #[test]
    fn fit_centres_in_a_wide_window() {
        let fit = Fit::new((1920, 1080), (SIZE, SIZE));
        assert!(fit.scale > 2.1 && fit.scale < 2.2);
        assert_eq!(fit.to_image(10.0, 540.0), None, "left band");
        assert!(fit.to_image(960.0, 540.0).is_some_and(|(x, y)| x.abs_diff(256) <= 1 && y.abs_diff(256) <= 1));
    }

    #[test]
    fn hit_test_matches_alpha() {
        let scene = Scene::generate();
        let fit = Fit::new((SIZE, SIZE), (SIZE, SIZE));
        assert!(fit.hit(&scene, 226.5, 276.5));
        assert!(!fit.hit(&scene, 2.5, 2.5));
        // The feather: pixels below the threshold are click-through, at or above it are not.
        for y in 0..SIZE {
            for x in 0..SIZE {
                assert_eq!(fit.hit(&scene, x as f32 + 0.5, y as f32 + 0.5), scene.alpha(x, y) >= HIT_ALPHA);
            }
        }
    }

    #[test]
    fn input_rects_cover_exactly_the_hit_pixels() {
        let scene = Scene::generate();
        for window in [(SIZE, SIZE), (300, 200)] {
            let fit = Fit::new(window, (SIZE, SIZE));
            let mut covered = vec![0_u8; (window.0 * window.1) as usize];
            for r in fit.input_rects(&scene) {
                for y in r.y..r.y + r.h {
                    for x in r.x..r.x + r.w {
                        covered[(y * window.0 + x) as usize] += 1;
                    }
                }
            }
            for y in 0..window.1 {
                for x in 0..window.0 {
                    let want = u8::from(fit.hit(&scene, x as f32 + 0.5, y as f32 + 0.5));
                    assert_eq!(covered[(y * window.0 + x) as usize], want, "({x}, {y}) in {window:?}");
                }
            }
        }
    }

    #[test]
    fn input_rects_merge_rows() {
        let scene = Scene::generate();
        let fit = Fit::new((SIZE, SIZE), (SIZE, SIZE));
        let rects = fit.input_rects(&scene);
        assert!(rects.len() < (SIZE * SIZE / 8) as usize, "{} rectangles", rects.len());
        assert!(rects.iter().any(|r| r.h > 1));
    }
}
