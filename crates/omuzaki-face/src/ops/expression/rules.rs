//! The rules every expression op shares: which names are reserved, which kinds they carry, and what a delta or a curve may
//! be. Each rule runs before any patch is built, so a refusal names the argument at fault and changes nothing.

use omuzaki_face_api::{ChannelValues, ExpressionId, ExpressionKind, IntensityCurve, VISEMES, narrow};
use omuzaki_ops_api::OpError;

/// The blink expression's name: the VRM `blink` preset.
const BLINK: &str = "blink";

/// Whether `id` is one of the five visemes or `blink`. They are the VRM contract (lip sync and the VRM export name them),
/// so they can be tuned but never deleted, renamed or duplicated.
pub(super) fn is_reserved(id: &ExpressionId) -> bool {
    is_viseme_name(id.as_str()) || id.as_str() == BLINK
}

fn is_viseme_name(name: &str) -> bool {
    VISEMES.contains(&name)
}

/// Refuses a kind that doesn't fit the name. The five visemes must be kind `viseme`, `blink` must be kind `blink`, and
/// those two kinds belong to those names alone. `field` is the argument that carries the name or the kind.
///
/// # Errors
/// `Invalid`, naming `field`, with the kind the name needs.
pub(super) fn check_kind(id: &ExpressionId, kind: ExpressionKind, field: &str) -> Result<(), OpError> {
    let name = id.as_str();
    if is_viseme_name(name) && kind != ExpressionKind::Viseme {
        return Err(OpError::invalid(
            field,
            format!("'{name}' is one of the five visemes, so its kind must be viseme, not {}", kind_name(kind)),
        )
        .hint("the visemes are aa, ih, ou, ee and oh; a mood or accent needs another name"));
    }
    if name == BLINK && kind != ExpressionKind::Blink {
        return Err(OpError::invalid(
            field,
            format!("'blink' is the blink expression, so its kind must be blink, not {}", kind_name(kind)),
        )
        .hint("a mood or accent needs another name"));
    }
    if kind == ExpressionKind::Viseme && !is_viseme_name(name) {
        return Err(OpError::invalid(
            field,
            format!("kind viseme is for the five visemes (aa, ih, ou, ee, oh) only, and '{name}' is not one"),
        )
        .hint(format!("give '{name}' the kind emotion or accent")));
    }
    if kind == ExpressionKind::Blink && name != BLINK {
        return Err(OpError::invalid(
            field,
            format!("kind blink is for the blink expression only, and '{name}' is not it"),
        )
        .hint(format!("give '{name}' the kind emotion or accent")));
    }
    Ok(())
}

/// The `snake_case` name of a kind, as the document and the schema spell it.
fn kind_name(kind: ExpressionKind) -> &'static str {
    match kind {
        ExpressionKind::Emotion => "emotion",
        ExpressionKind::Viseme => "viseme",
        ExpressionKind::Blink => "blink",
        ExpressionKind::Accent => "accent",
    }
}

/// Checks the deltas of an expression: at least one channel, and each delta a finite number no larger than its channel's
/// full range either way. A bigger delta can only be a typo, because the bus clamps every channel to its range anyway.
///
/// # Errors
/// `Invalid`, naming `deltas` when there are none, or `deltas/<channel>` for a delta out of range.
pub(super) fn check_deltas(deltas: &ChannelValues) -> Result<(), OpError> {
    if deltas.0.is_empty() {
        return Err(OpError::invalid("deltas", "the expression moves no channel")
            .hint("name at least one channel and how far it moves at full weight, for example {\"jawOpen\": 0.5}"));
    }
    for (channel, value) in &deltas.0 {
        let (lo, hi) = channel.range();
        let span = hi - lo;
        let v = narrow(*value);
        if !(v.is_finite() && v.abs() <= span) {
            return Err(OpError::invalid(
                format!("deltas/{channel}"),
                format!("{channel} = {value} is out of range: a delta can move it by at most {span} either way"),
            )
            .hint(format!("{channel} runs from {lo} to {hi}, and a delta is how far it moves at full weight")));
        }
    }
    Ok(())
}

/// Checks an intensity curve: a power curve needs an exponent above zero.
///
/// # Errors
/// `Invalid`, naming `curve/exponent`.
pub(super) fn check_curve(curve: IntensityCurve) -> Result<(), OpError> {
    match curve {
        IntensityCurve::Power { exponent } if !(exponent.is_finite() && exponent > 0.0) => {
            Err(OpError::invalid("curve/exponent", format!("exponent = {exponent} must be a number above 0"))
                .hint("below 1 shows the expression early, above 1 late; 1 is the same as linear"))
        }
        _ => Ok(()),
    }
}

/// The refusal for an op that would delete, rename or duplicate a reserved name. `action` is the past participle.
pub(super) fn refuse_reserved(id: &ExpressionId, action: &str) -> OpError {
    OpError::invalid("id", format!("'{id}' is part of the VRM contract, so it can't be {action}")).hint(
        "the five visemes and blink can be tuned with face.expression.set, keeping their kind, but not removed, renamed \
         or copied",
    )
}
