//! The face mapping ops (FE-06): `face.set_mapping` writes one exaggeration coefficient, and `face.describe_mapping`
//! lists every coefficient with its formula. The resting face is set by `face.set_rest` (in `rest/`), which shares this
//! module's edit.
//!
//! Each op plans against the document and returns a patch of leaf changes; none writes the document itself. The check
//! and the patch are the shared face rules (`ops_shared`), so a refusal names every field the edit breaks.

mod describe;
mod edit;
mod set;

pub use describe::DescribeMapping;
pub use set::SetMapping;

pub(crate) use edit::{plan_edit, repaint_report};
