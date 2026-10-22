//! Contract for the procedural face (spec v3 §19 and CB-20..25; design: `docs/design/char-face-api.md`).
//!
//! `omuzaki-face-api` is an API crate (§39 rule 1): small, stable, documented. The **channel table** (the ARKit-native
//! bus) belongs to `omuzaki-anim-api`; this crate owns what the face *does* with it:
//! - [`mapping`]: [`FaceMapping`], the formulas that turn channels into [`PaintParams`], with their coefficients
//! - [`profile`]: [`FaceProfile`], one character's face design (layout, eye/brow/mouth styles, mapping, expressions)
//! - [`expression`]: named expressions (channel deltas) and the five lip-sync visemes
//! - [`grammar`]: the anime grammar rule switches (FE-05), which the grammar stage in `omuzaki-anim` reads
//! - [`paint`]: the [`FacePainter`] contract and the vector [`DrawList`] it produces, and how layers composite
//! - [`underlay`]: what `face.underlay.set` needs from the app that shows the head (the sheet's pixels go to it as plain data)
//! - [`palette`] and [`check`]: palette slots (colours are `omuzaki_core::colour::Colour`) and the validation helper shared
//!   with `omuzaki-char-api`
//!
//! Everything that is stored is `serde` + `JsonSchema`, so ops, the MCP server and docs derive from the types.
#![deny(missing_docs)]

mod baker;
pub mod check;
pub mod expression;
pub mod grammar;
pub mod mapping;
pub mod paint;
pub mod palette;
pub mod profile;
mod resolve;
pub mod underlay;

pub use baker::{BakerInput, FaceBakerConflict, FaceBakerFactory, FaceBakerRegistration, face_baker};
pub use check::{Check, FieldRange, ParamError, ParamErrors, Problem, Refusal, Validate, refuse_introduced};
pub use expression::{
    ChannelValues, Expression, ExpressionId, ExpressionKind, ExpressionSet, IntensityCurve, Playback, PlaybackLayer,
    UnknownExpression, VISEMES, Weight,
};
pub use grammar::{GrammarRules, RESERVED_RULES, RuleConfig, RuleId};
pub use mapping::{
    Amplify, BrowMap, BrowParams, CheekMap, EyeEffects, EyeMap, EyeParams, FaceMapping, GazeMap, MouthMap, MouthParams,
    PaintParams, RestShape, UNMAPPED,
};
pub use omuzaki_core::colour::Colour;
pub use paint::{
    Cap, Cmd, ColourExt, Composite, DEFAULT_PX_PER_M, DrawList, FaceDrawing, FaceError, FaceInput, FaceLayerId,
    FaceLayerSpec, FacePainter, FaceProjection, FaceRasterizer, FaceRect, FaceView, Fade, GradientStop, Join,
    LayerTarget, Paint, PathBuilder, PathEl, PathRef, PlanarWindow, Raster, Stops, narrow,
};
pub use palette::{ColourError, ColourRef, Palette, PaletteSlot};
pub use profile::{
    Aperture, BlushKind, BrowStyle, CLOSE_SHOT, CatchlightShape, CheekStyle, Crease, EyeStyle, FaceLayout, FaceMark,
    FaceProfile, FaceRatios, FangKind, FangStyle, Highlights, Iris, Lash, Lips, LowerLidStyle, MarkKind, MouthInterior,
    MouthStyle, NoseKind, NoseStyle, Pupil, PupilShape, Sclera, WIDE_SHOT,
};
pub use resolve::ResolveError;

#[doc(hidden)]
pub use inventory as __inventory;
#[doc(hidden)]
pub use omuzaki_core::__record_registration;
