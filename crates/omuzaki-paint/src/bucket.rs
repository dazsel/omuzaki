//! The bucket fill: flood the region round a point, bounded by line art, closing small gaps in it and growing under
//! its anti-aliased edge, as a colourist fills flats.

use std::collections::VecDeque;

use crate::canvas::{Canvas, Mask, Rect};
use crate::colour::Px;
use crate::raster::morph;

/// How a bucket fill is bounded.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct FloodOptions {
    /// The largest difference from the start pixel, in 8-bit levels, that still fills.
    pub tolerance: f64,
    /// Gaps in the bounds this many pixels wide are treated as closed.
    pub gap: u32,
    /// Pixels to grow (or, negative, shrink) the filled region by.
    pub grow: i32,
}

/// The largest difference between two premultiplied pixels, per channel, in 8-bit levels (0 to 255). Premultiplied,
/// so a clear pixel matches any other clear pixel whatever colour it was.
fn distance(a: Px, b: Px) -> f64 {
    f64::from((0..4).map(|i| (a[i] - b[i]).abs()).fold(0.0f32, f32::max)) * 255.0
}

/// The region of `source` connected to `(x, y)` (4-way) within the options' bounds, as a 0/1 mask.
#[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap, clippy::cast_sign_loss)] // canvas-sized values
pub fn flood(source: &Canvas, x: f64, y: f64, o: FloodOptions) -> Mask {
    let bounds = source.bounds();
    let (sx, sy) = (x.floor() as i32, y.floor() as i32);
    let mut out = Mask::new(bounds);
    if sx < 0 || sy < 0 || sx >= bounds.x1 || sy >= bounds.y1 {
        return Mask::new(Rect::default());
    }
    let seed = source.get(sx, sy);
    let (w, h) = (bounds.width(), bounds.height());
    // Walls: pixels too different from the start. Closing gaps thickens them before the flood.
    let mut walls = Mask::new(bounds);
    for (i, v) in walls.values.iter_mut().enumerate() {
        let p = source.get((i % w) as i32, (i / w) as i32);
        if distance(p, seed) > o.tolerance {
            *v = 1.0;
        }
    }
    let thick = if o.gap > 0 { morph(&walls, o.gap.div_ceil(2) as i32, bounds) } else { walls.clone() };
    let blocked = |x: i32, y: i32| thick.get(x, y) >= 0.5;
    if blocked(sx, sy) {
        // The start is inside a thickened wall (a thin gap): fill nothing rather than leak.
        return Mask::new(Rect::default());
    }
    let mut queue = VecDeque::new();
    let mut seen = vec![false; w * h];
    seen[sy as usize * w + sx as usize] = true;
    queue.push_back((sx, sy));
    while let Some((x, y)) = queue.pop_front() {
        out.values[y as usize * w + x as usize] = 1.0;
        for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
            let (nx, ny) = (x + dx, y + dy);
            if nx < 0 || ny < 0 || nx as usize >= w || ny as usize >= h {
                continue;
            }
            let i = ny as usize * w + nx as usize;
            if !seen[i] && !blocked(nx, ny) {
                seen[i] = true;
                queue.push_back((nx, ny));
            }
        }
    }
    // Closing gaps pulled the fill back from the walls by the same amount; give it back, but not into the walls.
    let back = if o.gap > 0 { o.gap.div_ceil(2) as i32 } else { 0 };
    let mut region = if back > 0 { regrow(&out, &walls, back, bounds) } else { out };
    if o.grow != 0 {
        region = morph(&region, o.grow, bounds);
    }
    region
}

/// Grows `region` by `n` pixels, one at a time, never into `walls`.
fn regrow(region: &Mask, walls: &Mask, n: i32, clip: Rect) -> Mask {
    let mut r = region.clone();
    for _ in 0..n {
        let grown = morph(&r, 1, clip);
        let mut next = Mask::new(clip);
        for y in clip.y0..clip.y1 {
            for x in clip.x0..clip.x1 {
                if grown.get(x, y) >= 0.5
                    && walls.get(x, y) < 0.5
                    && let Some(v) = next.at_mut(x, y)
                {
                    *v = 1.0;
                }
            }
        }
        r = next;
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::colour::Colour;

    /// A white canvas with a black square outline, 1 px thick, from 10 to 50, with a gap of `gap` pixels on its top.
    fn boxed(gap: i32) -> Canvas {
        let mut c = Canvas::filled(64, 64, Colour::WHITE).unwrap();
        for i in 10..=50 {
            for (x, y) in [(i, 10), (i, 50), (10, i), (50, i)] {
                if y == 10 && (28..28 + gap).contains(&x) {
                    continue;
                }
                if let Some(p) = c.at_mut(x, y) {
                    *p = Colour::BLACK.premul();
                }
            }
        }
        c
    }

    const O: FloodOptions = FloodOptions { tolerance: 24.0, gap: 0, grow: 0 };

    #[test]
    fn fills_inside_the_lines_only() {
        let m = flood(&boxed(0), 30.0, 30.0, O);
        assert!((m.area() - 39.0 * 39.0).abs() < 1e-6, "{}", m.area());
        assert_eq!(m.get(5, 5), 0.0);
    }

    #[test]
    fn a_gap_leaks_unless_it_is_closed() {
        let leaky = flood(&boxed(3), 30.0, 30.0, O);
        assert!(leaky.get(5, 5) > 0.5);
        let closed = flood(&boxed(3), 30.0, 30.0, FloodOptions { gap: 4, ..O });
        assert_eq!(closed.get(5, 5), 0.0);
        // Closing the gap does not shrink the fill away from the lines.
        assert!(closed.get(11, 30) > 0.5 && closed.get(49, 30) > 0.5);
    }

    #[test]
    fn growing_slips_under_the_line() {
        let m = flood(&boxed(0), 30.0, 30.0, FloodOptions { grow: 1, ..O });
        assert!(m.get(10, 30) > 0.5 && m.get(9, 30) == 0.0);
    }
}
