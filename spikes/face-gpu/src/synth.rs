//! A synthetic stand-in for the real face painter (whose sections are still stubs): `n` commands shaped like the
//! prototype's face: eye groups (a clip, sclera, iris radial gradient, pupil, highlights, round-capped lash and lid strokes),
//! large soft blush gradients, and small marks. Deterministic (fixed seed). Not a quality reference, a load model.
#![allow(clippy::all, clippy::pedantic)]
use omuzaki_core::glam::{Affine2, Vec2};
use omuzaki_face_api::{Cap, Cmd, Colour, DrawList, Join, Paint, Stops};

struct Rng(u64);
impl Rng {
    fn f(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 40) as f32) / (1u64 << 24) as f32
    }
    fn r(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.f()
    }
}

fn stops(a: [u8; 4], b: [u8; 4], c: [u8; 4]) -> Stops {
    Stops::new(&[(0.0, Colour::rgba(a[0], a[1], a[2], a[3])), (0.6, Colour::rgba(b[0], b[1], b[2], b[3])), (1.0, Colour::rgba(c[0], c[1], c[2], c[3]))])
}

/// About `n` commands over the rect x in -0.215..0.215, y in -0.2825..0.025 (metres).
pub fn face(n: usize, seed: u64) -> DrawList {
    let mut rng = Rng(seed);
    let mut d = DrawList::new();
    while d.cmds().len() < n {
        let c = Vec2::new(rng.r(-0.19, 0.19), rng.r(-0.25, 0.0));
        let s = rng.r(0.012, 0.045);
        let big = rng.f() < 0.04;
        if big {
            // Soft blush: a big ellipse with a radial gradient fading to transparent.
            let p = d.path().ellipse(c, s * 3.0, s * 2.0).finish();
            d.fill(p, Paint::Radial { centre: c, radius: s * 3.0, stops: stops([240, 120, 130, 150], [240, 130, 140, 70], [240, 140, 150, 0]) });
            continue;
        }
        d.push(Cmd::PushTransform(Affine2::from_translation(c) * Affine2::from_angle(rng.r(-0.3, 0.3))));
        let aperture = d
            .path()
            .move_to(Vec2::new(-s, 0.0))
            .cubic_to(Vec2::new(-s * 0.5, s * 0.9), Vec2::new(s * 0.5, s * 0.9), Vec2::new(s, 0.0))
            .cubic_to(Vec2::new(s * 0.5, -s * 0.7), Vec2::new(-s * 0.5, -s * 0.7), Vec2::new(-s, 0.0))
            .close()
            .finish();
        d.push(Cmd::PushClip(aperture));
        d.fill(aperture, Paint::Solid(Colour::rgba(250, 248, 252, 255)));
        let iris = d.path().ellipse(Vec2::new(rng.r(-0.2, 0.2) * s, 0.0), s * 0.55, s * 0.6).finish();
        d.fill(iris, Paint::Radial { centre: Vec2::ZERO, radius: s * 0.6, stops: stops([35, 85, 140, 255], [100, 160, 210, 255], [20, 40, 80, 255]) });
        let pupil = d.path().ellipse(Vec2::new(0.0, 0.0), s * 0.22, s * 0.3).finish();
        d.fill(pupil, Paint::Solid(Colour::rgba(15, 15, 30, 255)));
        let hi = d.path().ellipse(Vec2::new(s * 0.2, s * 0.25), s * 0.1, s * 0.1).finish();
        d.fill(hi, Paint::Solid(Colour::rgba(255, 255, 255, 230)));
        d.push(Cmd::PopClip);
        let lash = d.path().move_to(Vec2::new(-s, 0.0)).cubic_to(Vec2::new(-s * 0.5, s * 0.95), Vec2::new(s * 0.5, s * 0.95), Vec2::new(s * 1.15, s * 0.1)).finish();
        d.push(Cmd::Stroke { path: lash, paint: Paint::Solid(Colour::rgba(40, 24, 30, 255)), width: s * 0.12, cap: Cap::Round, join: Join::Round, opacity: 1.0 });
        let lid = d.path().move_to(Vec2::new(-s, -0.0)).cubic_to(Vec2::new(-s * 0.5, -s * 0.75), Vec2::new(s * 0.5, -s * 0.75), Vec2::new(s, 0.0)).finish();
        d.push(Cmd::Stroke { path: lid, paint: Paint::Solid(Colour::rgba(150, 90, 100, 200)), width: s * 0.06, cap: Cap::Round, join: Join::Round, opacity: 0.8 });
        d.push(Cmd::PopTransform);
    }
    d
}
