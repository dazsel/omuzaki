//! Hand styles for the `body` part.

use omuzaki_char_api::{BodyParams, MaterialName, PartMesh};

use crate::kit::KitError;
use crate::template::layout::BodyLayout;

pub mod five;
pub mod mitten;
pub mod simple;

/// The hand meshes of `params.hands` for both hands of `layout`, using `material`. Empty until the hand styles
/// are filled in; the body part calls this so they only have to replace its body.
///
/// # Errors
/// [`KitError`] if a hand cannot be built.
pub fn build(_params: &BodyParams, _layout: &BodyLayout, _material: &MaterialName) -> Result<Vec<PartMesh>, KitError> {
    Ok(Vec::new())
}
