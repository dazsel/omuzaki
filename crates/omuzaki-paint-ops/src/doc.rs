//! Where paintings live in the document, and the conversions every `paint.*` op shares.

use omuzaki_doc_api::{DocPath, DocRead, Value};
use omuzaki_ops_api::OpError;
use omuzaki_paint::{PaintError, Painting, check_id};
use serde::Serialize;

/// The document section that holds the paintings, by id.
pub const PAINTINGS: &str = "paintings";

/// The document path of painting `id`.
pub fn painting_path(id: &str) -> DocPath {
    DocPath::root().join(PAINTINGS).join(id)
}

/// The path of a field of painting `id` (`["layers", "lines", "marks"]`).
pub(crate) fn path_in(id: &str, segments: &[&str]) -> DocPath {
    segments.iter().fold(painting_path(id), |p, s| p.join(*s))
}

/// An op error for a painting the engine refuses.
pub(crate) fn from_paint(e: PaintError) -> OpError {
    match e {
        PaintError::Invalid { field, reason } => OpError::invalid(field, reason),
        PaintError::NoLayer(id) => {
            OpError::not_found(format!("there is no layer {id:?}")).hint("paint.get lists the painting's layers")
        }
        PaintError::SampleLoop(chain) => {
            OpError::invalid("sample", format!("layers sample each other in a loop: {chain}"))
                .hint("a bucket fill may sample only layers that don't sample it back")
        }
        PaintError::Colour(c) => {
            OpError::invalid("colour", format!("{c:?} is not a colour: write #rrggbb or #rrggbbaa"))
        }
        other => OpError::internal(other.to_string()),
    }
}

/// Checks an id an op was given.
pub(crate) fn id(field: &str, value: &str) -> Result<(), OpError> {
    check_id(field, value).map_err(from_paint)
}

/// The ids of the paintings in the document.
pub(crate) fn ids(doc: &dyn DocRead) -> Vec<String> {
    match doc.get(&DocPath::root().join(PAINTINGS)) {
        Some(Value::Object(map)) => map.keys().cloned().collect(),
        _ => Vec::new(),
    }
}

/// Painting `id` as the document holds it.
pub(crate) fn load(doc: &dyn DocRead, painting: &str) -> Result<Painting, OpError> {
    id("painting", painting)?;
    let Some(v) = doc.get(&painting_path(painting)) else {
        let known = ids(doc);
        let hint = if known.is_empty() {
            "there are no paintings yet: make one with paint.new".to_owned()
        } else {
            format!("the paintings are {}", known.join(", "))
        };
        return Err(OpError::not_found(format!("there is no painting {painting:?}")).hint(hint));
    };
    serde_json::from_value(v.clone())
        .map_err(|e| OpError::internal(format!("painting {painting:?} in the document doesn't read: {e}")))
}

/// `value` as document JSON.
pub(crate) fn json<T: Serialize>(value: &T) -> Result<Value, OpError> {
    serde_json::to_value(value).map_err(|e| OpError::internal(format!("serialising a painting failed: {e}")))
}
