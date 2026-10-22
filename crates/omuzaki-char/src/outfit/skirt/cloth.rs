//! The skirt's cloth hook: the sheet a cloth simulation of the skirt would run on.
//!
//! A stub, until the skirt's cloth task fills it (the skirt is skinned to the skeleton while
//! [`ClothSpec::enabled`](crate::outfit::common::ClothSpec) is `false`, which its `check` insists on).

use omuzaki_char_api::BodyFit;

use super::SkirtParams;

/// What a simulated skirt needs to start from: the cloth task adds its fields.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct ClothSheetSpec {
    /// How many rows at the waist are pinned to the hips.
    pub pinned_rows: usize,
}

/// The sheet to simulate `params` over `fit`, or `None` while the skirt is skinned.
pub fn cloth_sheet(params: &SkirtParams, fit: &dyn BodyFit) -> Option<ClothSheetSpec> {
    let _ = (params, fit);
    None
}
