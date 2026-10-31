//! Contract for the procedural face (spec v3 §19 and CB-20..25; design: `docs/design/char-face-api.md`).
//!
//! `omuzaki-face-api` is an API crate (§39 rule 1): small, stable, documented. The **channel table** (the ARKit-native
//! bus) belongs to `omuzaki-anim-api`; this crate owns what the face *does* with it:
//! - [`mapping`]: [`FaceMapping`], the formulas that turn channels into [`PaintParams`], with their coefficients
//! - [`profile`]: [`FaceProfile`], one character's face design (layout, eye/brow/mouth styles, mapping, expressions)
//! - [`eye3d`]: [`eye3d::EyeDesign`], the photoreal 3D eye and the skin round it, stored on the face as `eye3d`
//! - [`sculpt`]: the sculpted 3D nose, lips and ear, stored on the face as `nose3d`, `lips3d` and `ear3d`
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
pub mod eye3d;
pub mod grammar;
pub mod mapping;
pub mod paint;
pub mod palette;
pub mod profile;
mod resolve;
pub mod sculpt;
pub mod underlay;

pub use baker::{BakerInput, FaceBakerConflict, FaceBakerFactory, FaceBakerRegistration, face_baker};
pub use check::{Check, FieldRange, ParamError, ParamErrors, Validate};
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
    LayerTarget, Paint, PathBuilder, PathEl, PathRef, PlanarWindow, Raster, RasterRect, Stops, narrow,
};
pub use palette::{ColourError, ColourRef, Palette, PaletteSlot};
pub use profile::{
    AngerVeinStyle, Aperture, BlushKind, BrowStyle, CLOSE_SHOT, Catchlight, CatchlightShape, CheekStyle, Collarette,
    Crease, EffectStyle, EyeShadow, EyeStyle, FaceLayout, FaceLines, FaceMark, FaceProfile, FaceRatios, FangKind,
    FangStyle, Highlights, Iris, IrisColours, IrisDetail, IrisFibres, IrisFlecks, IrisPaint, Lash, LashStrands,
    LimbalRing, Lips, LowerLidStyle, MAX_CATCHLIGHTS, MAX_MARK_POINTS, MarkKind, MarkSide, MouthInterior, MouthStyle,
    NoseKind, NoseStyle, Pupil, PupilShape, Sclera, StrandRow, TeethStyle, TongueStyle, WIDE_SHOT,
};
pub use resolve::ResolveError;

#[doc(hidden)]
pub use inventory as __inventory;
#[doc(hidden)]
pub use omuzaki_core::__record_registration;
