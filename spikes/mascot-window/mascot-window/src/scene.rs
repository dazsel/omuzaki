//! The test picture as pixels, and the geometry that maps it into a window.
//!
//! The picture is drawn by `scene.wgsl`, a copy of the Spout spike's test picture (soft-edged shapes with every alpha
//! level at their borders, known-value patches, opaque tag rows), and read back from the GPU once so the CPU has its
//! alpha for per-pixel click-through. The window draws the same shader live; this copy is the hit-test mask and the
//! reference for the fringe check.

/// Alpha at or above which a pixel counts as part of the mascot for hit testing (about 3%).
pub const HIT_ALPHA: u8 = 8;

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

impl Scene {
    /// A picture of fully opaque pixels: a mask under which every pixel of the window takes mouse input.
    pub fn opaque(width: u32, height: u32) -> Self {
        Self { width, height, pixels: vec![255; width as usize * height as usize * 4] }
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

    /// Whether the mascot is under window pixel `(x, y)`: its alpha as shown, which is the picture's times the window's
    /// `opacity`, is at or above [`HIT_ALPHA`]. A fully faded mascot therefore catches no clicks.
    pub fn hit(&self, scene: &Scene, opacity: f32, x: f32, y: f32) -> bool {
        self.to_image(x, y).is_some_and(|(ix, iy)| f32::from(scene.alpha(ix, iy)) * opacity >= f32::from(HIT_ALPHA))
    }

    /// The mascot's footprint as horizontal runs of window pixels, for an OS input region (X11 input shape, a Wayland
    /// input region). One rectangle per run per row, merged vertically where consecutive rows have identical runs.
    pub fn input_rects(&self, scene: &Scene, opacity: f32) -> Vec<Rect> {
        let mut rects: Vec<Rect> = Vec::new();
        let mut previous: Vec<Rect> = Vec::new();
        for y in 0..self.window.1 {
            let mut row: Vec<Rect> = Vec::new();
            let mut start: Option<u32> = None;
            for x in 0..=self.window.0 {
                let on = x < self.window.0 && self.hit(scene, opacity, x as f32 + 0.5, y as f32 + 0.5);
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

/// The picture as a hit-test mask: where it sits in the window and how opaque the window shows it.
#[derive(Clone, Copy, Debug)]
pub struct Mask<'a> {
    /// How the picture sits in the window.
    pub fit: Fit,
    /// The picture's pixels.
    pub scene: &'a Scene,
    /// The window's opacity, 0..=1.
    pub opacity: f32,
}

impl Mask<'_> {
    /// Whether the mascot is under window pixel `(x, y)`; see [`Fit::hit`].
    pub fn hit(&self, x: f32, y: f32) -> bool {
        self.fit.hit(self.scene, self.opacity, x, y)
    }

    /// The mascot's footprint for an OS input region; see [`Fit::input_rects`].
    pub fn input_rects(&self) -> Vec<Rect> {
        self.fit.input_rects(self.scene, self.opacity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: u32 = 64;

    /// A disc of alpha 255 with a one-pixel feather of alpha 100 and nothing else: enough shape for the geometry tests.
    fn disc() -> Scene {
        let mut pixels = vec![0_u8; (SIZE * SIZE * 4) as usize];
        for y in 0..SIZE {
            for x in 0..SIZE {
                let d = ((x as f32 + 0.5 - 32.0).powi(2) + (y as f32 + 0.5 - 32.0).powi(2)).sqrt();
                let alpha = if d < 15.0 {
                    255
                } else if d < 16.0 {
                    100
                } else {
                    0
                };
                let i = ((y * SIZE + x) * 4) as usize;
                pixels[i..i + 4].copy_from_slice(&[alpha / 2, alpha / 2, alpha / 2, alpha]);
            }
        }
        Scene { width: SIZE, height: SIZE, pixels }
    }

    #[test]
    fn over_black_is_the_premultiplied_colour_and_over_white_adds_the_rest() {
        let scene = disc();
        let white = scene.over_background([255; 3]);
        let black = scene.over_background([0; 3]);
        for ((s, w), b) in
            scene.pixels.as_chunks::<4>().0.iter().zip(white.as_chunks::<4>().0).zip(black.as_chunks::<4>().0)
        {
            assert_eq!(b[..3], s[..3]);
            assert_eq!(w[3], 255);
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
        assert!((fit.scale - 16.875).abs() < 1e-3);
        assert_eq!(fit.to_image(10.0, 540.0), None, "left band");
        assert!(fit.to_image(960.0, 540.0).is_some_and(|(x, y)| x.abs_diff(32) <= 1 && y.abs_diff(32) <= 1));
    }

    #[test]
    fn hit_test_matches_alpha() {
        let scene = disc();
        let fit = Fit::new((SIZE, SIZE), (SIZE, SIZE));
        assert!(fit.hit(&scene, 1.0, 32.5, 32.5));
        assert!(!fit.hit(&scene, 1.0, 2.5, 2.5));
        for y in 0..SIZE {
            for x in 0..SIZE {
                assert_eq!(fit.hit(&scene, 1.0, x as f32 + 0.5, y as f32 + 0.5), scene.alpha(x, y) >= HIT_ALPHA);
            }
        }
    }

    #[test]
    fn input_rects_cover_exactly_the_hit_pixels() {
        let scene = disc();
        for window in [(SIZE, SIZE), (40, 30)] {
            let fit = Fit::new(window, (SIZE, SIZE));
            let mut covered = vec![0_u8; (window.0 * window.1) as usize];
            for r in fit.input_rects(&scene, 1.0) {
                for y in r.y..r.y + r.h {
                    for x in r.x..r.x + r.w {
                        covered[(y * window.0 + x) as usize] += 1;
                    }
                }
            }
            for y in 0..window.1 {
                for x in 0..window.0 {
                    let want = u8::from(fit.hit(&scene, 1.0, x as f32 + 0.5, y as f32 + 0.5));
                    assert_eq!(covered[(y * window.0 + x) as usize], want, "({x}, {y}) in {window:?}");
                }
            }
        }
    }

    #[test]
    fn an_opaque_scene_catches_every_pixel() {
        let scene = Scene::opaque(40, 30);
        let fit = Fit::new((40, 30), (40, 30));
        let rects = fit.input_rects(&scene, 1.0);
        assert_eq!(rects, vec![Rect { x: 0, y: 0, w: 40, h: 30 }]);
    }

    #[test]
    fn opacity_shrinks_the_hit_area_to_nothing() {
        let scene = disc();
        let fit = Fit::new((SIZE, SIZE), (SIZE, SIZE));
        // The feather has alpha 100 and the body 255: at 0.05 only the body (255 * 0.05 = 12.75 >= 8) still hits.
        assert!(fit.hit(&scene, 1.0, 32.5, 16.5));
        assert!(!fit.hit(&scene, 0.05, 32.5, 16.5));
        assert!(fit.hit(&scene, 0.05, 32.5, 32.5));
        assert!(fit.input_rects(&scene, 0.0).is_empty(), "invisible pixels catch no clicks");
    }

    #[test]
    fn input_rects_merge_rows() {
        let scene = disc();
        let fit = Fit::new((SIZE, SIZE), (SIZE, SIZE));
        let rects = fit.input_rects(&scene, 1.0);
        assert!(rects.len() < (SIZE * SIZE / 8) as usize, "{} rectangles", rects.len());
        assert!(rects.iter().any(|r| r.h > 1));
    }
}
