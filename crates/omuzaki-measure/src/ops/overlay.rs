//! `measure.face_overlay`: the evidence image (design 5, 6.2 and spec §15's "a failed gate becomes a card with the
//! evidence image"). The outline's samples, the snapped landmarks and, with a reference, the reference's outline mapped
//! into this face's frame, drawn on a copy of the image and written to the store as a PNG.

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_store_api::{Commercial, Provenance, Source, Store};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{Resolved, Subject, resolve};
use crate::face::draw::{Canvas, EXCLUDED, LANDMARK, OCCLUDED, OK, REFERENCE, WORST};
use crate::face::likeness::{Compare, Tolerances, likeness};
use crate::face::outline::{ANGLES, SampleState, angle};
use crate::image::{ImageRef, Rgba8};

/// Draws the evidence image of a face's measurement (and, with a reference, the reference's outline over it), and
/// stores it as a PNG.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FaceOverlay {
    /// The face to draw on: a face, or a likeness target whose image is in the store.
    pub face: Subject,
    /// A reference whose outline is drawn over the face in blue, with the worst sector shaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<Subject>,
    /// The preset the worst sector is judged with (default `cross_look`).
    #[serde(default)]
    pub compare: Compare,
}

impl Op for FaceOverlay {
    fn meta() -> OpMeta {
        OpMeta {
            name: "measure.face_overlay",
            summary: "Draw a face's outline measurement on its image (green ok samples, red rays where hair covers \
                      the face, grey excluded rays, magenta snapped eyes and mouth) and, with a reference, the \
                      reference's outline in blue with the worst sector shaded; stores the PNG and returns its hash. \
                      Look at it before excluding a sector or after a failed likeness check",
            kind: OpKind::Query,
            scope: Scope::Render,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let face = resolve(ctx, &self.face, "face")?;
        let reference = self.reference.as_ref().map(|r| resolve(ctx, r, "reference")).transpose()?;
        let (Some(measure), Some(image)) = (&face.measure, &face.image) else {
            return Err(OpError::invalid(
                "face",
                "the target's image is not in the store, so there is nothing to draw on",
            )
            .hint("import the reference image into the store again"));
        };
        let drawn = draw(&face, measure, image, reference.as_ref(), self.compare);
        let png = encode(&drawn)?;
        let private = face.private || reference.as_ref().is_some_and(|r| r.private);
        let commercial = commercial(ctx, &self.face);
        if ctx.dry_run {
            return Ok(Plan::answer(json!({ "dry_run": true, "private": private, "bytes": png.len() })));
        }
        let provenance = Provenance {
            source: Source::Authored,
            author: None,
            licence: "LicenseRef-Omuzaki-User".to_owned(),
            attribution: None,
            commercial,
            consent: None,
            private_test: private,
        };
        let store = ctx.service::<dyn Store>()?;
        let asset = store.put(&png, provenance).map_err(|e| OpError::internal(format!("storing the overlay: {e}")))?;
        Ok(Plan::answer(json!({ "asset": asset.to_string(), "private": private })))
    }
}

register_op!(FaceOverlay);

/// The input image's commercial status, carried to the overlay; unknown when it has no provenance.
fn commercial(ctx: &OpCtx<'_>, subject: &Subject) -> Commercial {
    match subject {
        Subject::Face(face) => match &face.image {
            ImageRef::Asset { asset } => ctx
                .service::<dyn Store>()
                .ok()
                .and_then(|s| s.provenance(asset).ok())
                .map_or(Commercial::Unknown, |p| p.commercial),
            ImageRef::File { .. } => Commercial::Unknown,
        },
        Subject::Target { .. } => Commercial::Unknown,
    }
}

/// Draws the measurement of `face` over its image.
fn draw(
    face: &Resolved,
    m: &crate::face::FaceMeasure,
    image: &Rgba8,
    reference: Option<&Resolved>,
    compare: Compare,
) -> Rgba8 {
    let mut c = Canvas::new(image);
    let frame = &m.frame;
    let d = frame.iod_px;
    let dot = (d * 0.012).max(1.5);
    // The worst sector first, under everything else.
    if let Some(r) = reference
        && let Ok(l) = likeness(r.side(), face.side(), Tolerances::preset(compare))
        && let Some(w) = &l.worst
    {
        let points = &w.candidate_points;
        for pair in points.windows(2) {
            c.wedge(frame.origin, pair[0], pair[1], WORST, 0.35);
        }
    }
    for k in 0..ANGLES {
        let reach = face.outline.r[k].map_or(1.2 * d, |r| r * d);
        let end = frame.along(angle(k), reach);
        match face.outline.state[k] {
            SampleState::Ok => c.dot(end, dot, OK),
            SampleState::Occluded => c.line(frame.origin, end, OCCLUDED, 0.8),
            SampleState::Excluded => c.line(frame.origin, end, EXCLUDED, 0.6),
            SampleState::Cut | SampleState::Leak => {}
        }
    }
    if let Some(r) = reference {
        // The reference's outline in this face's frame: the same face coordinates, so the same angles and radii.
        let points: Vec<Option<[f64; 2]>> =
            (0..ANGLES).map(|k| r.outline.ok(k).map(|rad| frame.along(angle(k), rad * d))).collect();
        for pair in points.windows(2) {
            if let (Some(a), Some(b)) = (pair[0], pair[1]) {
                c.line(a, b, REFERENCE, 1.0);
            }
        }
    }
    let lm = &m.landmarks;
    let size = (d * 0.04).max(3.0);
    for s in [&lm.eye_right, &lm.eye_left] {
        c.ring(s.given, size * 0.6, LANDMARK);
        c.cross(s.used, size, LANDMARK);
    }
    if let Some(mouth) = &lm.mouth {
        c.ring(mouth.given, size * 0.6, LANDMARK);
        c.square(mouth.used, size * 0.7, LANDMARK);
    }
    if let Some(nose) = &lm.nose {
        c.ring(nose.given, size * 0.6, LANDMARK);
        c.cross(nose.used, size * 0.6, LANDMARK);
    }
    c.finish()
}

/// The image as a PNG.
fn encode(image: &Rgba8) -> Result<Vec<u8>, OpError> {
    let mut out = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut out);
    image::ImageEncoder::write_image(
        encoder,
        &image.pixels,
        image.width,
        image.height,
        image::ExtendedColorType::Rgba8,
    )
    .map_err(|e| OpError::internal(format!("encoding the overlay: {e}")))?;
    Ok(out)
}
