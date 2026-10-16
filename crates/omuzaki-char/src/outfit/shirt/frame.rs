//! The upright column a shirt and its neckwear hang on: the torso with the neck above it, addressed by height.
//!
//! The prototype's garments are written in heights (`tp(y, ph, off)`, `uniform.js` 23-35): the section of the shirt at
//! height `y`, pushed `off` outwards. [`Column`] is that function over any [`BodyFit`]: it joins the fit's torso and neck
//! regions into one column, so a height above the torso's top is a ring of the neck, and hands out [`Ring`]s the same
//! way [`ring_at`] does. Both regions must stand upright (their axes along +y), which every template's torso and neck
//! do; a fit that doesn't is an error here rather than a crooked garment.
//!
//! A shirt is cut as a superellipse and publishes its [`Envelope`](crate::outfit::common::Envelope) widened by
//! [`enclosing_scale`], so the parts that hang on the shirt (collar, tie) read the shirt's own surface back with
//! [`shirt_shell`], which undoes that widening.

use std::sync::Arc;

use omuzaki_anim_api::HumanoidPose;
use omuzaki_char_api::{BodyFit, BodyRegion, JointRef, Section};
use omuzaki_core::glam::{DVec2, DVec3, Vec2, Vec3};

use crate::outfit::common::{GarmentError, enclosing_scale};
use crate::outfit::loft::{Ring, ring_at};

/// How far the axis of the torso or neck may lean from straight up, as the least `y` of its unit axis.
const UPRIGHT: f32 = 0.99;
/// The shortest the torso or the neck may be, metres: less is a table mistake, and heights would not map to it.
const MIN_SPAN: f64 = 1e-3;

/// The extent of one region along the column.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Span {
    region: BodyRegion,
    bottom: f64,
    top: f64,
}

impl Span {
    /// The span of `region` of `fit`: where its first and last sections sit.
    fn of(fit: &dyn BodyFit, region: BodyRegion) -> Result<Self, GarmentError> {
        let end = |t: f32| fit.section(region, t).ok_or(GarmentError::NoSection { region, t: f64::from(t) });
        let (first, last) = (end(0.0)?, end(1.0)?);
        for section in [first, last] {
            if section.axis.y < UPRIGHT || !section.centre.is_finite() {
                return Err(GarmentError::invalid(
                    "body",
                    format!("the {region:?} is not upright (axis {}); a shirt hangs on an upright torso", section.axis),
                ));
            }
        }
        let (bottom, top) = (f64::from(first.centre.y), f64::from(last.centre.y));
        if top - bottom < MIN_SPAN {
            return Err(GarmentError::invalid("body", format!("the {region:?} is only {} m tall", top - bottom)));
        }
        Ok(Self { region, bottom, top })
    }

    /// The fraction of the region at height `y`.
    fn t(&self, y: f64) -> f64 {
        (y - self.bottom) / (self.top - self.bottom)
    }
}

/// The torso and neck of a fit as one column, cut with one superellipse exponent.
#[derive(Clone, Debug)]
pub struct Column {
    fit: Arc<dyn BodyFit>,
    torso: Span,
    neck: Span,
    exponent: f64,
}

impl Column {
    /// The column of `fit`, its rings cut with `exponent`.
    ///
    /// # Errors
    /// [`GarmentError`] if the fit has no torso or neck, either leans away from upright, or `exponent` is outside
    /// [`MIN_EXPONENT`](crate::outfit::loft::MIN_EXPONENT)..=[`MAX_EXPONENT`](crate::outfit::loft::MAX_EXPONENT).
    pub fn new(fit: Arc<dyn BodyFit>, exponent: f64) -> Result<Self, GarmentError> {
        crate::outfit::loft::check_exponent(exponent)?;
        let torso = Span::of(fit.as_ref(), BodyRegion::Torso)?;
        let neck = Span::of(fit.as_ref(), BodyRegion::Neck)?;
        Ok(Self { fit, torso, neck, exponent })
    }

    /// The fit the column reads.
    pub fn fit(&self) -> &Arc<dyn BodyFit> {
        &self.fit
    }

    /// The height of the torso's lowest section.
    pub fn bottom(&self) -> f64 {
        self.torso.bottom
    }

    /// The height where the torso ends and the neck begins.
    pub fn top(&self) -> f64 {
        self.torso.top
    }

    /// The height of the neck's highest section, under the chin.
    pub fn neck_top(&self) -> f64 {
        self.neck.top
    }

    /// The torso's length.
    pub fn length(&self) -> f64 {
        self.torso.top - self.torso.bottom
    }

    /// The ring at height `y` (clamped to the column), `offset` metres out: a torso ring up to [`Column::top`], a neck
    /// ring above it.
    ///
    /// # Errors
    /// As [`ring_at`]; also for a `y` that is not a number.
    pub fn ring(&self, y: f64, offset: f64) -> Result<Ring, GarmentError> {
        if y.is_nan() {
            return Err(GarmentError::invalid("height", "a ring's height is not a number".to_owned()));
        }
        let y = y.clamp(self.torso.bottom, self.neck.top);
        let span = if y <= self.torso.top { self.torso } else { self.neck };
        ring_at(self.fit.as_ref(), span.region, span.t(y).clamp(0.0, 1.0), self.exponent, offset)
    }

    /// The point at height `y` and angle `phi` (`0` the front, `pi / 2` the left), `offset` metres out: the
    /// prototype's `tp`.
    ///
    /// # Errors
    /// As [`Column::ring`].
    pub fn point(&self, y: f64, phi: f64, offset: f64) -> Result<DVec3, GarmentError> {
        Ok(self.ring(y, offset)?.point(phi))
    }

    /// `p` moved straight out from the column's axis, in its own height's plane, until it is at least `least` metres
    /// outside the surface; `p` itself if it already is. Above and below the column the end rings are the surface.
    ///
    /// # Errors
    /// As [`Column::ring`].
    pub fn lift(&self, p: DVec3, least: f64) -> Result<DVec3, GarmentError> {
        let ring = self.ring(p.y, 0.0)?;
        let d = p - ring.centre;
        let q = DVec2::new(d.dot(ring.across), d.dot(ring.through));
        let (_, outside) = ring.along_ray(q);
        let Some(dir) = q.try_normalize().filter(|_| outside < least) else {
            return Ok(p);
        };
        let moved = dir * (least - outside);
        Ok(p + ring.across * moved.x + ring.through * moved.y)
    }

    /// The `z` of the surface at the front centre (`x = 0`) at height `y`, `offset` out: the prototype's `frontZ`.
    ///
    /// # Errors
    /// As [`Column::ring`].
    pub fn front_z(&self, y: f64, offset: f64) -> Result<f64, GarmentError> {
        Ok(self.point(y, 0.0, offset)?.z)
    }
}

/// A fit whose radii are those of `inner` divided by a factor: the shirt's surface read back from its envelope.
#[derive(Debug)]
struct Shell {
    inner: Arc<dyn BodyFit>,
    scale: f32,
}

impl BodyFit for Shell {
    fn section(&self, region: BodyRegion, t: f32) -> Option<Section> {
        let section = self.inner.section(region, t)?;
        Some(Section { radii: section.radii / Vec2::splat(self.scale), ..section })
    }

    fn joint(&self, joint: &JointRef) -> Option<Vec3> {
        self.inner.joint(joint)
    }

    fn bind(&self) -> HumanoidPose {
        self.inner.bind()
    }
}

/// The surface of a garment cut with `exponent`, read from its `envelope`: the envelope's radii are the garment's
/// widened by [`enclosing_scale`], and this divides that out, so a ring of the result at offset 0 is the garment itself.
pub fn shirt_shell(envelope: Arc<dyn BodyFit>, exponent: f64) -> Arc<dyn BodyFit> {
    #[allow(clippy::cast_possible_truncation)] // a fit is f32
    let scale = enclosing_scale(exponent) as f32;
    Arc::new(Shell { inner: envelope, scale })
}
