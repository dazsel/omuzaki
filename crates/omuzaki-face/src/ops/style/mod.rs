//! The face style ops (CB-20): `face.set_style` and `face.describe_style` edit and read a character's face by path,
//! and `face.add_mark` and `face.remove_mark` change its marks list at the end.
//!
//! Every op plans against the document and returns a patch of leaf changes; none writes the document itself. A value is
//! checked on a copy of the whole face with `FaceProfile::validate` (from `omuzaki_face_api::Validate`), so a refusal
//! names every bad field and changes nothing. The paths, the defaults and the rules for a bad one are in
//! `ops_shared/path.rs`.

mod describe;
mod marks;
mod set;

pub use describe::DescribeStyle;
pub use marks::{AddMark, RemoveMark};
pub use set::SetStyle;
