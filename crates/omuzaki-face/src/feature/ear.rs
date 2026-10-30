//! The ear, sculpted as a signed-distance field on a patch of the side of the head: the auricle's plate standing off
//! the head at the back, the helix rolling round its rim from inside the bowl up over the top and down the back to the
//! lobe, the antihelix's Y-shaped ridge inside it (its stem and two crura, with the triangular fossa between them and
//! the scapha's groove outside), the concha's bowl and the canal, the tragus in front of it and the antitragus below,
//! and the soft lobe. Seen from behind, the ear stands off the head with the groove behind it.
//!
//! Toward anime the same field morphs into a drawn ear: one rolled rim round a shallow bowl, the folds inside it
//! softened away.
//!
//! Frame: x along the head toward the back, y up, z out of the side of the head; metres. The ear canal's mouth is near
//! the origin.

use omuzaki_core::glam::{Mat3, Vec3};
pub use omuzaki_face_api::sculpt::EarDesign;

use super::sdf::{capsule, carve, ellipsoid, resample, smin, smoothstep, tube};

const MM: f32 = 0.001;

/// The ear's field, the patch of head it stands on included.
pub fn field(d: &EarDesign) -> impl Fn(Vec3) -> f32 + '_ {
    let shape = Shape::new(d);
    move |p| shape.distance(p)
}

/// The box the ear and its patch of head fill.
pub fn bounds(d: &EarDesign) -> (Vec3, Vec3) {
    let (h, w) = (0.5 * d.height + 0.006, 0.5 * d.width + 0.012);
    (Vec3::new(-w - 0.004, -h, -0.012), Vec3::new(w + 0.004, h, 0.006 + 0.6 * d.width))
}

/// How much redder the skin is at `p`: the ear's thin rim and lobe let the light through warm.
pub fn redness(d: &EarDesign, p: Vec3) -> f32 {
    let s = Shape::new(d);
    let rim = 1.0 - smoothstep(-1.5 * MM, 5.0 * MM, s.helix_distance(s.local(p)));
    rim * (1.0 - d.style)
}

/// The placed forms of one design, in the auricle's own frame (before it is turned out from the head).
struct Shape {
    style: f32,
    turn: Mat3,
    pivot: Vec3,
    helix: Vec<Vec3>,
    helix_r: Vec<f32>,
    stem: Vec<Vec3>,
    stem_r: Vec<f32>,
    crus: Vec<Vec3>,
    crus_r: Vec<f32>,
    plate: (Vec3, Vec3),
    cup: (Vec3, Vec3),
    concha: (Vec3, Vec3),
    tragus: (Vec3, Vec3),
    antitragus: (Vec3, Vec3),
    lobe: (Vec3, Vec3),
    root: (Vec3, Vec3),
    canal: (Vec3, Vec3),
    foot: (Vec3, Vec3),
}

impl Shape {
    fn new(d: &EarDesign) -> Self {
        // Laid out for a 62 by 32 mm ear and scaled to the design's.
        let (sy, sx) = (d.height / 0.062, d.width / 0.032);
        let at = |x: f32, y: f32, z: f32| Vec3::new(x * MM * sx, y * MM * sy, z * MM);
        let hr = d.helix.max(0.2);
        let ar = d.antihelix.max(0.0);
        let lobe = d.lobe.max(0.2);
        // From the crus inside the bowl, up and forward, over the top, down the back to the lobe.
        let helix = vec![
            at(-4.0, 4.0, 4.0),
            at(-9.0, 12.0, 4.0),
            at(-8.0, 22.0, 5.5),
            at(-1.0, 28.5, 6.2),
            at(8.0, 26.0, 6.4),
            at(13.5, 16.0, 6.4),
            at(14.5, 4.0, 6.0),
            at(12.0, -8.0, 5.4),
            at(8.0, -16.0, 4.4),
            at(6.0, -20.0, 3.6),
        ];
        let helix_r: Vec<f32> =
            [1.6, 2.1, 2.4, 2.5, 2.6, 2.6, 2.5, 2.3, 1.8, 1.0].iter().map(|r| r * MM * hr).collect();
        let (helix, helix_r) = resample(&helix, &helix_r, 3);
        let stem = [at(4.0, -12.0, 4.1), at(6.0, -2.0, 4.9), at(5.5, 8.0, 5.3), at(3.0, 17.0, 5.1), at(0.0, 23.0, 4.7)];
        let stem_r: Vec<f32> = [2.4, 2.8, 2.8, 2.4, 1.8].iter().map(|r| r * MM * ar).collect();
        let (stem, stem_r) = resample(&stem, &stem_r, 3);
        let crus = [at(5.5, 8.0, 5.3), at(0.0, 11.0, 4.9), at(-6.0, 12.5, 4.5)];
        let crus_r: Vec<f32> = [2.4, 2.0, 1.5].iter().map(|r| r * MM * ar).collect();
        let (crus, crus_r) = resample(&crus, &crus_r, 3);
        let turn = Mat3::from_rotation_y(-d.protrusion.to_radians());
        Self {
            style: d.style.clamp(0.0, 1.0),
            turn,
            pivot: at(-10.0, 0.0, 0.0),
            helix,
            helix_r,
            stem,
            stem_r,
            crus,
            crus_r,
            plate: (at(3.0, 5.0, 3.5), at(12.0, 23.0, 2.8)),
            cup: (at(1.0, -1.0, 0.0), at(10.0, 13.0, 5.0)),
            concha: (at(-1.5, -2.0, 8.0), at(7.0, 8.5, 6.0)),
            tragus: (at(-9.0, -4.5, 5.6), Vec3::new(3.0 * MM, 4.5 * MM, 3.0 * MM)),
            antitragus: (at(1.5, -11.0, 4.9), Vec3::new(3.2 * MM, 2.4 * MM, 2.4 * MM)),
            lobe: (at(4.0, -21.0, 3.1), Vec3::new(6.4 * MM * sx, 6.4 * MM * sy, 2.6 * MM) * lobe),
            root: (at(-8.0, 2.0, 0.5), at(5.0, 20.0, 4.0)),
            canal: (at(-7.0, -4.0, 5.5), at(-12.0, -3.0, -5.0)),
            foot: (at(-5.0, -11.0, 3.0), at(6.0, 7.0, 4.0)),
        }
    }

    /// A point of the head's frame in the auricle's (turned back about its root at the front).
    fn local(&self, p: Vec3) -> Vec3 {
        self.turn.transpose() * (p - self.pivot) + self.pivot
    }

    /// The side of the head: a broad dome.
    fn head(p: Vec3) -> f32 {
        ellipsoid(p, Vec3::new(0.0, 0.0, -0.075), Vec3::new(0.095, 0.12, 0.075))
    }

    fn helix_distance(&self, q: Vec3) -> f32 {
        tube(q, &self.helix, &self.helix_r, 0.0)
    }

    fn real(&self, p: Vec3) -> f32 {
        let q = self.local(p);
        let plate = ellipsoid(q, self.plate.0, self.plate.1);
        let mut ear = smin(plate, self.helix_distance(q), 2.2 * MM);
        // The conchal cup behind the bowl, which holds the ear to the head.
        ear = smin(ear, ellipsoid(q, self.cup.0, self.cup.1), 3.0 * MM);
        if self.stem_r.iter().any(|r| *r > 0.0) {
            let ridge = smin(tube(q, &self.stem, &self.stem_r, 0.0), tube(q, &self.crus, &self.crus_r, 0.0), 1.2 * MM);
            ear = smin(ear, ridge, 1.6 * MM);
        }
        ear = smin(ear, ellipsoid(q, self.lobe.0, self.lobe.1), 3.0 * MM);
        ear = smin(ear, ellipsoid(q, self.antitragus.0, self.antitragus.1), 1.4 * MM);
        // The bowl, cut down into the plate and the root.
        ear = carve(ear, ellipsoid(q, self.concha.0, self.concha.1), 2.0 * MM);
        // The root joins the ear to the head at the front; the tragus stands at the bowl's front edge.
        let root = ellipsoid(p, self.root.0, self.root.1);
        let mut d = smin(Self::head(p), root, 4.0 * MM);
        d = smin(d, ear, 2.5 * MM);
        d = smin(d, ellipsoid(p, self.tragus.0, self.tragus.1), 1.5 * MM);
        carve(d, capsule(p, self.canal.0, self.canal.1, 2.2 * MM), 1.5 * MM)
    }

    fn anime(&self, p: Vec3) -> f32 {
        let q = self.local(p);
        let plate = ellipsoid(q, self.plate.0, self.plate.1);
        let mut ear = smin(plate, self.helix_distance(q), 2.5 * MM);
        ear = smin(ear, ellipsoid(q, self.cup.0, self.cup.1), 3.0 * MM);
        ear = smin(ear, ellipsoid(q, self.lobe.0, self.lobe.1), 4.0 * MM);
        // A shallow bowl inside the rim.
        let bowl =
            ellipsoid(q, self.concha.0 + Vec3::new(0.0, 6.0 * MM, 1.5 * MM), self.concha.1 * Vec3::new(1.1, 1.7, 0.6));
        ear = carve(ear, bowl, 3.0 * MM);
        let root = ellipsoid(p, self.root.0, self.root.1);
        let d = smin(smin(Self::head(p), root, 5.0 * MM), ear, 3.0 * MM);
        // The bowl's foot closed in front of the lobe, so the drawn ear has no hole there.
        smin(d, ellipsoid(p, self.foot.0, self.foot.1), 3.0 * MM)
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
