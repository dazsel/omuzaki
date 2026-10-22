//! Which of the four report sections a finding belongs to (`docs/design/byom.md`, D6 "One report").
//!
//! The import report is one list of findings from every step. The screen groups them for people by rule id, never by
//! parsing text, into **Mapped** (what was matched or kept), **Made for you** (what the app changed so the model
//! works), **Not used** (what was dropped, inert or not drawn) and **Rights** (what the licence allows). The text of
//! each line and its fix are in [`crate::msg`], as `io-ops-<rule>` and `io-ops-<rule>-hint`.
//!
//! Choices this cut makes beyond the design's list:
//!
//! - Errors a person must fix (`avatar.too_many_bones`, `avatar.skeleton_invalid`, `avatar.invalid`,
//!   `avatar.schema_unknown`, `avatar.skin_invalid`, `avatar.too_many_meshes`, `gltf.layout_unsupported`) are
//!   **Not used**: the model or the setting cannot be used as it is. Their hint says how to fix them.
//! - Values that were pulled back into range (`vrm.limit_clamped`, `vrm.bind_weight_clamped`,
//!   `vrm.mtoon_queue_clamped`, `vrm.collider_normal_normalised`) and `avatar.weights_normalised` are **Made for you**.
//! - `vrm.perfect_sync_complete` is **Mapped** (every tracked channel has a shape); `vrm.perfect_sync_partial` is
//!   **Not used** (the missing shapes stay still).
//! - `humanoid.override_refound`, `humanoid.other_skeleton`, `humanoid.scale_invalid` and `humanoid.sides_overlap` are
//!   **Mapped**, as is everything else under `humanoid.`.
//! - A rule with no row below falls back by area: `humanoid.*` is Mapped, `rights.*` is Rights, `cleanup.*` and
//!   `arkit.generated` are Made for you, and everything else is Not used. A new importer rule therefore always
//!   appears somewhere, and the scan test in `tests/report_sections.rs` makes sure it also has its messages.

/// The four sections of the report screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Section {
    /// What was matched or kept: humanoid bones, expression channels present, physics kept.
    Mapped,
    /// What the app changed so the model works: synthesised channels, a normalised rest pose, welded vertices.
    MadeForYou,
    /// What was dropped, inert or not drawn.
    NotUsed,
    /// What the model's licence allows or asks for.
    Rights,
}

/// The section of the rule `rule` (`area.rule_name`, for example `arkit.missing`).
///
/// Total: an unknown rule falls back by its area (the text before the first dot), so the report never loses a finding.
#[must_use]
pub fn section(rule: &str) -> Section {
    match rule {
        "avatar.expression_custom" | "avatar.thumb_off_axis" | "vrm.perfect_sync_complete" => Section::Mapped,
        "avatar.rest_normalised"
        | "avatar.weights_normalised"
        | "arkit.synthesised"
        | "arkit.generated"
        | "vrm.collider_normal_normalised"
        | "vrm.limit_clamped"
        | "vrm.bind_weight_clamped"
        | "vrm.mtoon_queue_clamped" => Section::MadeForYou,
        _ => by_area(rule),
    }
}

/// The fallback for a rule without its own row: the section of its area.
fn by_area(rule: &str) -> Section {
    match rule.split('.').next().unwrap_or(rule) {
        "humanoid" => Section::Mapped,
        "rights" => Section::Rights,
        "cleanup" => Section::MadeForYou,
        _ => Section::NotUsed,
    }
}
