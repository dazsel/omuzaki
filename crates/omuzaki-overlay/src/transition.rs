//! Scene transitions (ST-04): how the program moves from one scene to the next. The outgoing scene's last frame is
//! held and the incoming scene draws live under it, so a transition never pauses the character.
//!
//! Every kind blends in premultiplied alpha, so a transparent program (the character alone, for OBS) fades without
//! dark or white fringes, whatever alpha encoding the frames carry.

use omuzaki_render_api::{AlphaEncoding, Image};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::model::parse_hex;

/// The longest transition, milliseconds.
pub const MAX_MS: u32 = 5_000;

/// The kinds of transition.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TransitionKind {
    /// No transition: the next frame is the new scene.
    Cut,
    /// A cross-fade.
    #[default]
    Fade,
    /// Fade to a colour, then from it.
    Dip,
    /// The new scene wipes across the old one, with a soft edge.
    Wipe,
    /// The new scene pushes the old one out.
    Slide,
    /// The new scene opens from the middle in a growing circle.
    Iris,
}

impl TransitionKind {
    /// Every kind, as `program.transitions` lists them.
    pub const ALL: [TransitionKind; 6] = [
        TransitionKind::Cut,
        TransitionKind::Fade,
        TransitionKind::Dip,
        TransitionKind::Wipe,
        TransitionKind::Slide,
        TransitionKind::Iris,
    ];

    /// Its name in ops.
    pub const fn name(self) -> &'static str {
        match self {
            TransitionKind::Cut => "cut",
            TransitionKind::Fade => "fade",
            TransitionKind::Dip => "dip",
            TransitionKind::Wipe => "wipe",
            TransitionKind::Slide => "slide",
            TransitionKind::Iris => "iris",
        }
    }

    /// One line on what it looks like.
    pub const fn summary(self) -> &'static str {
        match self {
            TransitionKind::Cut => "Straight to the new scene.",
            TransitionKind::Fade => "A cross-fade.",
            TransitionKind::Dip => "Fade to a colour (black by default), then into the new scene.",
            TransitionKind::Wipe => "The new scene wipes across the old one with a soft edge.",
            TransitionKind::Slide => "The new scene pushes the old one out of frame.",
            TransitionKind::Iris => "The new scene opens from the middle in a growing circle.",
        }
    }
}

/// Which way a wipe or slide moves.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Towards the left.
    #[default]
    Left,
    /// Towards the right.
    Right,
    /// Upwards.
    Up,
    /// Downwards.
    Down,
}

/// A transition: its kind, length and options.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct TransitionSpec {
    /// What it looks like.
    pub kind: TransitionKind,
    /// How long it takes, milliseconds (0 to 5000).
    pub ms: u32,
    /// The colour a dip passes through, `#rrggbb`.
    pub colour: String,
    /// Which way a wipe or slide moves.
    pub direction: Direction,
}

impl Default for TransitionSpec {
    fn default() -> Self {
        Self { kind: TransitionKind::Fade, ms: 400, colour: "#000000".to_owned(), direction: Direction::Left }
    }
}

impl TransitionSpec {
    /// Checks the fields.
    ///
    /// # Errors
    /// The field at fault and why.
    pub fn check(&self) -> Result<(), (String, String)> {
        if self.ms > MAX_MS {
            return Err(("ms".to_owned(), format!("{} is longer than {MAX_MS} ms", self.ms)));
        }
        if parse_hex(&self.colour).is_none() {
            return Err(("colour".to_owned(), format!("'{}' is not a #rrggbb colour", self.colour)));
        }
        Ok(())
    }

    /// Its length in seconds; 0 for a cut.
    pub fn seconds(&self) -> f64 {
        if self.kind == TransitionKind::Cut { 0.0 } else { f64::from(self.ms) / 1000.0 }
    }
}

/// Smoothstep easing: slow at both ends.
fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// A pixel as premultiplied floats `0..=1`.
#[inline]
fn premul(p: &[u8], alpha: AlphaEncoding) -> [f32; 4] {
    let a = f32::from(p[3]) / 255.0;
    let c = |v: u8| f32::from(v) / 255.0;
    match alpha {
        AlphaEncoding::Premultiplied => [c(p[0]), c(p[1]), c(p[2]), a],
        AlphaEncoding::Straight => [c(p[0]) * a, c(p[1]) * a, c(p[2]) * a, a],
    }
}

/// Writes premultiplied floats back in `alpha`'s encoding.
#[inline]
fn store(out: &mut [u8], px: [f32; 4], alpha: AlphaEncoding) {
    let a = px[3].clamp(0.0, 1.0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped to 0..=255 first")]
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    match alpha {
        AlphaEncoding::Premultiplied => {
            out[0] = byte(px[0].min(a));
            out[1] = byte(px[1].min(a));
            out[2] = byte(px[2].min(a));
        }
        AlphaEncoding::Straight => {
            let un = |v: f32| if a > 0.0 { (v / a).min(1.0) } else { 0.0 };
            out[0] = byte(un(px[0]));
            out[1] = byte(un(px[1]));
            out[2] = byte(un(px[2]));
        }
    }
    out[3] = byte(a);
}

#[inline]
fn lerp4(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t, a[3] + (b[3] - a[3]) * t]
}

/// Draws transitions into the program. It keeps one scratch frame between calls, so a transition allocates once.
#[derive(Debug, Default)]
pub struct Blender {
    /// The incoming frame as it arrived, read while the blend writes over it.
    incoming: Vec<u8>,
}

impl Blender {
    /// Draws the transition at progress `t` (`0..=1`, linear time) into `to`, which holds the incoming scene's
    /// frame; `from` is the outgoing scene's held frame. A `from` of another size or encoding than `to` is not
    /// blended (the cut shows): the program changed shape mid-transition.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "pixel coordinates are at most a few thousand; the casts stay exact"
    )]
    #[allow(clippy::many_single_char_names, reason = "pixel maths: width, height, time, easing and coordinates")]
    pub fn blend(&mut self, spec: &TransitionSpec, from: &Image, to: &mut Image, t: f32) {
        if from.size != to.size || from.alpha != to.alpha || spec.kind == TransitionKind::Cut || t >= 1.0 {
            return;
        }
        self.incoming.clear();
        self.incoming.extend_from_slice(&to.rgba8);
        let incoming = &self.incoming;
        let alpha = to.alpha;
        let (w, h) = (to.size.x as usize, to.size.y as usize);
        let e = ease(t);
        let dip = parse_hex(&spec.colour).unwrap_or([0, 0, 0]);
        let dip = [f32::from(dip[0]) / 255.0, f32::from(dip[1]) / 255.0, f32::from(dip[2]) / 255.0, 1.0];
        // The soft edge of a wipe and the iris, in pixels.
        let soft = (w.max(h) as f32 * 0.04).max(1.0);
        let radius_max = ((w * w + h * h) as f32).sqrt() * 0.5 + soft;
        let sample = |buf: &[u8], x: f32, y: f32| -> Option<[f32; 4]> {
            if x < 0.0 || y < 0.0 || x >= w as f32 || y >= h as f32 {
                return None;
            }
            let j = (y as usize * w + x as usize) * 4;
            buf.get(j..j + 4).map(|p| premul(p, alpha))
        };
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                let (Some(src), Some(dst)) = (from.rgba8.get(i..i + 4), incoming.get(i..i + 4)) else { continue };
                let old = premul(src, alpha);
                let new = premul(dst, alpha);
                let px = match spec.kind {
                    TransitionKind::Cut => new,
                    TransitionKind::Fade => lerp4(old, new, e),
                    TransitionKind::Dip => {
                        if e < 0.5 {
                            lerp4(old, dip, e * 2.0)
                        } else {
                            lerp4(dip, new, e * 2.0 - 1.0)
                        }
                    }
                    TransitionKind::Wipe => {
                        // How far the edge has travelled, against this pixel's place along the motion.
                        let (along, span) = match spec.direction {
                            Direction::Left => ((w - 1 - x) as f32, w as f32),
                            Direction::Right => (x as f32, w as f32),
                            Direction::Up => ((h - 1 - y) as f32, h as f32),
                            Direction::Down => (y as f32, h as f32),
                        };
                        let edge = e * (span + soft) - soft * 0.5;
                        lerp4(old, new, ((edge - along) / soft + 0.5).clamp(0.0, 1.0))
                    }
                    TransitionKind::Slide => {
                        // The old frame moves out and the new one follows it in, one frame's length behind.
                        let (dx, dy) = match spec.direction {
                            Direction::Left => (-(e * w as f32), 0.0),
                            Direction::Right => (e * w as f32, 0.0),
                            Direction::Up => (0.0, -(e * h as f32)),
                            Direction::Down => (0.0, e * h as f32),
                        };
                        let (ox, oy) = (x as f32 - dx, y as f32 - dy);
                        sample(&from.rgba8, ox, oy)
                            .or_else(|| sample(incoming, ox + w as f32 * dx.signum(), oy + h as f32 * dy.signum()))
                            .unwrap_or(new)
                    }
                    TransitionKind::Iris => {
                        let (cx, cy) = (w as f32 * 0.5, h as f32 * 0.5);
                        let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
                        let r = e * (radius_max + soft) - soft * 0.5;
                        lerp4(old, new, ((r - d) / soft + 0.5).clamp(0.0, 1.0))
                    }
                };
                if let Some(out) = to.rgba8.get_mut(i..i + 4) {
                    store(out, px, alpha);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use glam::UVec2;

    use super::*;

    fn solid(rgba: [u8; 4], alpha: AlphaEncoding) -> Image {
        let size = UVec2::new(8, 4);
        Image::new(size, alpha, rgba.repeat(32)).unwrap()
    }

    fn spec(kind: TransitionKind) -> TransitionSpec {
        TransitionSpec { kind, ..TransitionSpec::default() }
    }

    #[test]
    fn a_fade_is_halfway_at_half_time() {
        let from = solid([200, 0, 0, 255], AlphaEncoding::Straight);
        let mut to = solid([0, 0, 200, 255], AlphaEncoding::Straight);
        Blender::default().blend(&spec(TransitionKind::Fade), &from, &mut to, 0.5);
        assert_eq!(to.pixel(3, 2), Some([100, 0, 100, 255]));
    }

    #[test]
    fn the_ends_are_the_two_scenes() {
        for kind in TransitionKind::ALL {
            let from = solid([200, 10, 0, 255], AlphaEncoding::Premultiplied);
            let mut start = solid([0, 10, 200, 255], AlphaEncoding::Premultiplied);
            Blender::default().blend(&spec(kind), &from, &mut start, 0.0);
            if kind != TransitionKind::Cut {
                assert_eq!(start.pixel(4, 2), Some([200, 10, 0, 255]), "{kind:?} starts on the old scene");
            }
            let mut end = solid([0, 10, 200, 255], AlphaEncoding::Premultiplied);
            Blender::default().blend(&spec(kind), &from, &mut end, 1.0);
            assert_eq!(end.pixel(4, 2), Some([0, 10, 200, 255]), "{kind:?} ends on the new scene");
        }
    }

    #[test]
    fn a_transparent_fade_has_no_dark_fringe() {
        // The old scene is a solid white character; the new frame is empty. Halfway, the pixel is white at half
        // alpha: blending straight colour would have pulled it towards the empty pixel's black.
        let from = solid([255, 255, 255, 255], AlphaEncoding::Straight);
        let mut to = solid([0, 0, 0, 0], AlphaEncoding::Straight);
        Blender::default().blend(&spec(TransitionKind::Fade), &from, &mut to, 0.5);
        assert_eq!(to.pixel(0, 0), Some([255, 255, 255, 128]));
    }

    #[test]
    fn a_dip_passes_through_its_colour() {
        let from = solid([200, 0, 0, 255], AlphaEncoding::Straight);
        let mut to = solid([0, 0, 200, 255], AlphaEncoding::Straight);
        let dip = TransitionSpec { colour: "#00ff00".into(), ..spec(TransitionKind::Dip) };
        Blender::default().blend(&dip, &from, &mut to, 0.5);
        assert_eq!(to.pixel(1, 1), Some([0, 255, 0, 255]));
    }

    #[test]
    fn a_wipe_left_reveals_from_the_right() {
        let from = solid([255, 0, 0, 255], AlphaEncoding::Straight);
        let mut to = solid([0, 0, 255, 255], AlphaEncoding::Straight);
        Blender::default().blend(&spec(TransitionKind::Wipe), &from, &mut to, 0.5);
        assert_eq!(to.pixel(7, 0).map(|p| p[2]), Some(255), "the right edge is new");
        assert_eq!(to.pixel(0, 0).map(|p| p[0]), Some(255), "the left edge is still old");
    }

    #[test]
    fn a_slide_pushes_the_old_scene_out() {
        let from = solid([255, 0, 0, 255], AlphaEncoding::Straight);
        let mut to = solid([0, 0, 255, 255], AlphaEncoding::Straight);
        Blender::default().blend(&spec(TransitionKind::Slide), &from, &mut to, 0.5);
        assert_eq!(to.pixel(0, 0), Some([255, 0, 0, 255]), "the old scene on the left");
        assert_eq!(to.pixel(7, 0), Some([0, 0, 255, 255]), "the new one coming in on the right");
    }

    #[test]
    fn an_iris_opens_from_the_middle() {
        let size = UVec2::new(64, 64);
        let from = Image::new(size, AlphaEncoding::Straight, [255, 0, 0, 255].repeat(64 * 64)).unwrap();
        let mut to = Image::new(size, AlphaEncoding::Straight, [0, 0, 255, 255].repeat(64 * 64)).unwrap();
        Blender::default().blend(&spec(TransitionKind::Iris), &from, &mut to, 0.4);
        assert_eq!(to.pixel(32, 32).map(|p| p[2]), Some(255), "the middle is new");
        assert_eq!(to.pixel(0, 0).map(|p| p[0]), Some(255), "the corner is old");
    }

    #[test]
    fn a_frame_of_another_size_is_left_as_it_is() {
        let from = Image::new(UVec2::new(2, 2), AlphaEncoding::Straight, vec![255; 16]).unwrap();
        let mut to = solid([0, 0, 200, 255], AlphaEncoding::Straight);
        let before = to.clone();
        Blender::default().blend(&spec(TransitionKind::Fade), &from, &mut to, 0.5);
        assert_eq!(to, before);
    }

    #[test]
    fn specs_check_their_length_and_colour() {
        assert!(TransitionSpec::default().check().is_ok());
        assert_eq!(TransitionSpec { ms: 9_000, ..TransitionSpec::default() }.check().unwrap_err().0, "ms");
        assert_eq!(
            TransitionSpec { colour: "black".into(), ..TransitionSpec::default() }.check().unwrap_err().0,
            "colour"
        );
    }
}
