//! The edit `face.set_mapping` and `face.set_rest` both plan: one value written into a copy of the face, checked, and
//! turned into the smallest patch. The plan also carries the resting paint (the neutral bus through the mapping) before
//! and after, so an agent sees what the edit does without rendering.

use omuzaki_char_api::{CharId, paths};
use omuzaki_doc_api::Patch;
use omuzaki_face_api::{FaceProfile, PaintParams};
use omuzaki_ops_api::{OpCtx, OpError, Plan};
use serde_json::{Number, Value, json};

use crate::ops_shared::check::check_edit;
use crate::ops_shared::edit::{changed, diff, effective, first_null, put, stored_face, working_copy};
use crate::ops_shared::path::{canonical, chain, template};

/// The keys below the face that a mapping path names, with `mapping` first: `face/mapping/eyes/wide` gives
/// `["mapping", "eyes", "wide"]`. The leading `face/` and `mapping/` may be left out.
///
/// # Errors
/// `Invalid` when the path names no coefficient (the whole mapping can't be replaced) or has an empty step.
pub(crate) fn mapping_keys(raw: &str) -> Result<Vec<String>, OpError> {
    let body = raw.strip_prefix("face/").unwrap_or(raw);
    let body = body.strip_prefix("mapping/").unwrap_or(body);
    if body.is_empty() || body == "mapping" {
        return Err(OpError::invalid("face/mapping", "the mapping can't be replaced whole")
            .hint("name one coefficient, for example face/mapping/eyes/wide"));
    }
    if body.split('/').any(str::is_empty) {
        return Err(OpError::invalid(raw, format!("'{raw}' has an empty step"))
            .hint("write the path as face/mapping/<section>/<key>, for example face/mapping/eyes/wide"));
    }
    Ok(std::iter::once("mapping").chain(body.split('/')).map(str::to_owned).collect())
}

/// Plans one write at `keys` (which start with `mapping`) on the character's face.
///
/// # Errors
/// `NotFound` for a missing character; `Invalid` for a null, an unknown key (with the nearest one), a value of the wrong
/// kind, or any field the edit leaves out of range; `Conflict` if the face isn't an object.
pub(crate) fn plan_edit(ctx: &OpCtx<'_>, character: &CharId, keys: &[String], value: Value) -> Result<Plan, OpError> {
    let stored = stored_face(ctx, character)?;
    let defaults = template()?;
    let chain = chain(&defaults, keys, 0)?;
    let field = canonical(keys);
    if let Some(at) = first_null(&value, &field) {
        return Err(OpError::invalid(at.clone(), format!("{at} is null, and the face holds no null"))
            .hint("give the field a number; a null can't be written"));
    }

    let mut working = working_copy(stored.as_ref());
    put(&mut working, "face", keys, &chain, value)?;
    check_edit(stored.as_ref(), &working, &defaults)?;

    let mut patch = Patch::new();
    diff(&paths::face(character), stored.as_ref(), Some(&working), &mut patch);
    let mut plan = Plan::edit(patch);
    plan.output = Some(json!({
        "changed": changed(&plan.patch),
        "neutral_paint": repaint_report(stored.as_ref(), Some(&working), |p| p.mapping.rest_params()),
    }));
    Ok(plan)
}

/// What an edit repaints, from the paint of a face before and after. An edit validates only what it writes, so the face
/// may hold damage elsewhere that its paint can't read. When either side can't be read, the report says `full`: the
/// whole face is dirty and the unreadable side names its reason. Otherwise it says `paint`, with both paints.
///
/// `paint` picks the parameters from a readable face: the neutral bus for a coefficient, the rest layer for a channel.
pub(crate) fn repaint_report(
    before: Option<&Value>,
    after: Option<&Value>,
    paint: fn(&FaceProfile) -> PaintParams,
) -> Value {
    let read = |face| effective(face).map(|profile| paint(&profile));
    match (read(before), read(after)) {
        (Ok(b), Ok(a)) => json!({ "repaint": "paint", "before": paint_json(&b), "after": paint_json(&a) }),
        (b, a) => json!({ "repaint": "full", "before": side(b), "after": side(a) }),
    }
}

/// One side of a repaint report: its paint, or why it can't be read.
fn side(read: Result<PaintParams, OpError>) -> Value {
    match read {
        Ok(paint) => paint_json(&paint),
        Err(error) => json!({ "unreadable": error.message }),
    }
}

/// A paint parameter set as JSON for an agent to read: every field, by its name.
fn paint_json(p: &PaintParams) -> Value {
    let eye = |e: &omuzaki_face_api::EyeParams| json!({ "open": num(e.open), "close": num(e.close), "arc": num(e.arc), "lower_lid": num(e.lower_lid) });
    let brow = |b: &omuzaki_face_api::BrowParams| json!({ "height": num(b.height), "tilt": num(b.tilt) });
    let fx = &p.effects;
    let m = &p.mouth;
    json!({
        "left_eye": eye(&p.left_eye),
        "right_eye": eye(&p.right_eye),
        "eye_tilt": num(p.eye_tilt),
        "pupil": num(p.pupil),
        "gaze": [num(p.gaze.x), num(p.gaze.y)],
        "vergence": num(p.vergence),
        "effects": {
            "heart": num(fx.heart), "star": num(fx.star), "gloom": num(fx.gloom), "shock": num(fx.shock),
            "squeeze": num(fx.squeeze), "spiral": num(fx.spiral), "sparkle": num(fx.sparkle), "tears": num(fx.tears),
        },
        "left_brow": brow(&p.left_brow),
        "right_brow": brow(&p.right_brow),
        "brow_arch": num(p.brow_arch),
        "mouth": {
            "open": num(m.open), "wide": num(m.wide), "smile_left": num(m.smile_left),
            "smile_right": num(m.smile_right), "round": num(m.round), "shift": num(m.shift), "press": num(m.press),
            "pucker": num(m.pucker), "teeth_upper": num(m.teeth_upper), "teeth_lower": num(m.teeth_lower),
            "tongue": num(m.tongue), "fangs": num(m.fangs),
        },
        "blush": num(p.blush),
        "cheek_puff": num(p.cheek_puff),
        "sweat": num(p.sweat),
    })
}

/// An `f32` as the decimal it was written as (`0.1`, not `0.10000000149`). Painter values are finite; a value that
/// isn't becomes text, never a `null`.
fn num(x: f32) -> Value {
    x.to_string()
        .parse::<f64>()
        .ok()
        .and_then(Number::from_f64)
        .map_or_else(|| Value::String(x.to_string()), Value::Number)
}
