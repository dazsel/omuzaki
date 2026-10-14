//! The body ops (CB-02..04): the proportion sliders, the body-shape space and its presets, the hand style and the body
//! template. Each op is one file; the mechanics they share (finding the body part, validating an edit, writing the
//! smallest patch, the "did you mean" refusal) are in `params`.

mod params;

pub mod apply_shape_preset;
pub mod set_hand_style;
pub mod set_proportion;
pub mod set_shape;
pub mod set_template;
