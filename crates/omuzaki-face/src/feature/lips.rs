//! The lips, sculpted as a signed-distance field on the patch of face round the mouth: the muzzle the teeth hold out,
//! the upper lip with its Cupid's bow and the tubercle at its middle, the fuller lower lip in two soft lobes, the
//! parting between them and the corners tucked into the cheeks, the philtrum's two ridges running up to the nose, and
//! the chin below the groove under the lower lip. The vermilion (the red of the lips) is where the lips' own forms are
//! the surface, so its border follows the sculpt.
//!
//! Toward anime the same field morphs into a drawn mouth: a small, nearly flat mouth whose line is inked, with only a
//! soft rise for the lower lip.
//!
//! Frame: x across the face (to the person's left), y up, z out of the face; metres. The mouth's middle, where the lips
//! meet, is the origin.

use omuzaki_core::glam::Vec3;
pub use omuzaki_face_api::sculpt::LipsDesign;

use super::sdf::{capsule, carve, ellipsoid, smin, smoothstep, tube};

const MM: f32 = 0.001;

/// The lips' field, the face round them included.
pub fn field(d: &LipsDesign) -> impl Fn(Vec3) -> f32 + '_ {
    let shape = Shape::new(d);
    move |p| shape.distance(p)
}

/// The box the mouth and its patch of face fill.
pub fn bounds(d: &LipsDesign) -> (Vec3, Vec3) {
    let half = (0.65 * d.width).max(0.03);
    (Vec3::new(-half, -0.034, -0.014), Vec3::new(half, 0.024, 0.016))
}

/// The skin's linear colour at a surface point `p`: the lips' red where their own forms are the surface, fading
/// over a fraction of a millimetre at the vermilion border; toward anime, the mouth's inked line.
pub fn colour_at(d: &LipsDesign, p: Vec3) -> Vec3 {
    let shape = Shape::new(d);
    let tone = d.tone.to_linear();
    let lip = d.colour.to_linear();
    let red = (1.0 - smoothstep(-0.15 * MM, 0.45 * MM, shape.lips(p))) * (1.0 - d.style);
    let mut c = tone.lerp(lip, red);
    // The parting and the corners darken: the inside of the mouth shows through.
    let parting = 1.0 - smoothstep(0.0, 0.3 * MM, (p.y - shape.line(p.x)).abs());
    let inside = parting * (1.0 - smoothstep(0.3, 0.46, p.x.abs() / d.width.max(1e-4)));
    c = c.lerp(c * 0.45, 0.6 * inside * (1.0 - d.style));
    // The anime mouth's line, thicker in the middle and fading at the corners.
    let along = (p.x.abs() / (0.5 * d.width.max(1e-4))).min(1.0);
    let width = 0.45 * MM * (1.0 - 0.5 * along);
    let ink = (1.0 - smoothstep(width, width + 0.4 * MM, (p.y - shape.line(p.x)).abs()))
        * (1.0 - smoothstep(0.7, 1.0, along));
    c.lerp(Vec3::new(0.18, 0.07, 0.07), 0.9 * ink * d.style)
}

/// How wet the surface is at `p`: the lips' red, most along the parting.
pub fn wet_at(d: &LipsDesign, p: Vec3) -> f32 {
    let shape = Shape::new(d);
    let red = 1.0 - smoothstep(-0.15 * MM, 0.45 * MM, shape.lips(p));
    let near = 1.0 - smoothstep(0.0, 3.0 * MM, (p.y - shape.line(p.x)).abs());
    red * (0.25 + 0.5 * near) * (1.0 - d.style)
}

/// `points` (corner to middle, the curve mirrored across the middle) and their `radii` resampled along a Catmull-Rom
/// spline into `SAMPLES` evenly spaced steps, so a tube along them swells and narrows without beads at its joints.
fn spline(points: &[Vec3; 4], radii: &[f32; 4]) -> (Vec<Vec3>, Vec<f32>) {
    // The point past the middle is the mirror of the one before it.
    let mirror = |v: Vec3| Vec3::new(-v.x, v.y, v.z);
    let ext = [points[0] * 2.0 - points[1], points[0], points[1], points[2], points[3], mirror(points[2])];
    let rext = [radii[0], radii[0], radii[1], radii[2], radii[3], radii[2]];
    let mut out = (Vec::with_capacity(SAMPLES * 3 + 1), Vec::with_capacity(SAMPLES * 3 + 1));
    for seg in 0..3 {
        for i in 0..SAMPLES {
            #[allow(clippy::cast_precision_loss)]
            let t = i as f32 / SAMPLES as f32;
            let (p0, p1, p2, p3) = (ext[seg], ext[seg + 1], ext[seg + 2], ext[seg + 3]);
            let (r0, r1, r2, r3) = (rext[seg], rext[seg + 1], rext[seg + 2], rext[seg + 3]);
            let w = [
                -0.5 * t * t * t + t * t - 0.5 * t,
                1.5 * t * t * t - 2.5 * t * t + 1.0,
                -1.5 * t * t * t + 2.0 * t * t + 0.5 * t,
                0.5 * t * t * t - 0.5 * t * t,
            ];
            out.0.push(p0 * w[0] + p1 * w[1] + p2 * w[2] + p3 * w[3]);
            out.1.push((r0 * w[0] + r1 * w[1] + r2 * w[2] + r3 * w[3]).max(0.0002));
        }
    }
    // On a little past the middle, so the two mirrored halves overlap there instead of meeting in a seam.
    for t in [0.0, 0.2, 0.4] {
        let (p0, p1, p2, p3) = (points[2], points[3], mirror(points[2]), mirror(points[1]));
        let w = [
            -0.5 * t * t * t + t * t - 0.5 * t,
            1.5 * t * t * t - 2.5 * t * t + 1.0,
            -1.5 * t * t * t + 2.0 * t * t + 0.5 * t,
            0.5 * t * t * t - 0.5 * t * t,
        ];
        out.0.push(p0 * w[0] + p1 * w[1] + p2 * w[2] + p3 * w[3]);
        out.1.push(radii[2] * w[0] + radii[3] * w[1] + radii[2] * w[2] + radii[1] * w[3]);
    }
    out
}

const SAMPLES: usize = 4;

/// The placed forms of one design.
struct Shape {
    style: f32,
    half: f32,
    upper: (Vec<Vec3>, Vec<f32>),
    lower: (Vec<Vec3>, Vec<f32>),
    tubercle: Vec3,
    tubercle_r: Vec3,
    corners: f32,
    philtrum: f32,
    columns: (Vec3, Vec3),
    anime_lower: Vec3,
    anime_lower_r: Vec3,
}

impl Shape {
    fn new(d: &LipsDesign) -> Self {
        let half = 0.5 * d.width.max(0.005);
        let (u, l, f) = (d.upper.max(0.001), d.lower.max(0.001), d.fullness.max(0.1));
        let bow = d.bow.clamp(0.0, 1.0);
        let lift = d.corners;
        Self {
            style: d.style.clamp(0.0, 1.0),
            half,
            // Corner to middle (mirrored across): the corner tucked in, the lip swelling to the bow's peak, the middle
            // dipping by the bow.
            // Corner to middle (mirrored across). Each lip's centre line sits a radius off the mouth's line, so the
            // lips meet along it; the upper lip's middle is a little thinner, which dips its top into the bow.
            upper: spline(
                &[
                    Vec3::new(half, lift + 0.1 * MM, -2.0 * MM),
                    Vec3::new(0.6 * half, 0.36 * u - 0.15 * MM, 2.6 * MM * f),
                    Vec3::new(0.22 * half, 0.45 * u - 0.15 * MM, 4.4 * MM * f),
                    Vec3::new(0.0, (0.45 - 0.06 * bow) * u - 0.15 * MM, 4.8 * MM * f),
                ],
                &[0.5 * MM, 0.36 * u, 0.45 * u, (0.45 - 0.06 * bow) * u],
            ),
            lower: spline(
                &[
                    Vec3::new(half, lift - 0.1 * MM, -2.0 * MM),
                    Vec3::new(0.6 * half, -0.38 * l + 0.15 * MM, 2.6 * MM * f),
                    Vec3::new(0.25 * half, -0.5 * l + 0.15 * MM, 4.4 * MM * f),
                    Vec3::new(0.0, -0.48 * l + 0.15 * MM, 4.4 * MM * f),
                ],
                &[0.5 * MM, 0.38 * l, 0.5 * l, 0.48 * l],
            ),
            tubercle: Vec3::new(0.0, 0.28 * u, 5.6 * MM * f),
            tubercle_r: Vec3::new(4.0 * MM, 0.3 * u, 0.3 * u),
            corners: lift,
            philtrum: d.philtrum.max(0.0),
            columns: (Vec3::new(0.2 * half, u + 0.2 * MM, 3.0 * MM), Vec3::new(3.4 * MM, 18.0 * MM, 0.6 * MM)),
            anime_lower: Vec3::new(0.0, -0.6 * l - 0.6 * MM, 3.4 * MM),
            anime_lower_r: Vec3::new(0.8 * half, 0.8 * l, 1.1 * MM),
        }
    }

    /// The mouth's line at `x`: where the lips meet, rising to the corners by their lift.
    fn line(&self, x: f32) -> f32 {
        let t = (x.abs() / self.half).min(1.0);
        self.corners * t * t
    }

    /// The face round the mouth: the broad front, the muzzle the teeth hold out, and the chin.
    fn face(p: Vec3) -> f32 {
        let front = ellipsoid(p, Vec3::new(0.0, 0.0, -0.06), Vec3::new(0.06, 0.1, 0.062));
        let muzzle = ellipsoid(p, Vec3::new(0.0, -0.002, -0.022), Vec3::new(0.031, 0.032, 0.026));
        let chin = ellipsoid(p, Vec3::new(0.0, -0.027, -0.012), Vec3::new(0.017, 0.012, 0.016));
        smin(smin(front, muzzle, 0.012), chin, 0.006)
    }

    /// The lips' own forms, unblended: their surface is the vermilion.
    fn lips(&self, p: Vec3) -> f32 {
        let m = Vec3::new(p.x.abs(), p.y, p.z);
        let upper = smin(
            tube(m, &self.upper.0, &self.upper.1, 1.0 * MM),
            ellipsoid(m, self.tubercle, self.tubercle_r),
            1.5 * MM,
        );
        let lower = tube(m, &self.lower.0, &self.lower.1, 1.0 * MM);
        upper.min(lower)
    }

    fn real(&self, p: Vec3) -> f32 {
        let m = Vec3::new(p.x.abs(), p.y, p.z);
        let mut d = Self::face(p);
        // The philtrum's ridges, with the groove between them left low.
        if self.philtrum > 0.0 {
            let column = capsule(m, self.columns.0, self.columns.1, 0.7 * MM * self.philtrum);
            d = smin(d, column, 4.5 * MM);
        }
        d = smin(d, self.lips(p), 1.6 * MM);
        // The parting: a thin cut along the line where the lips meet, deepest in the middle.
        let gap = ellipsoid(
            Vec3::new(p.x, p.y - self.line(p.x), p.z),
            Vec3::new(0.0, 0.0, 2.0 * MM),
            Vec3::new(0.86 * self.half, 0.16 * MM, 6.0 * MM),
        );
        carve(d, gap, 0.2 * MM)
    }

    fn anime(&self, p: Vec3) -> f32 {
        // Nearly flat: the lower lip a soft rise under the drawn line.
        let lip = ellipsoid(p, self.anime_lower, self.anime_lower_r);
        smin(Self::face(p), lip, 2.5 * MM)
    }

    fn distance(&self, p: Vec3) -> f32 {
        if self.style <= 0.0 {
            return self.real(p);
        }
        if self.style >= 1.0 {
            return self.anime(p);
        }
        self.real(p) + (self.anime(p) - self.real(p)) * self.style
    }
}
