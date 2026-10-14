//! One error type for the whole tracker: every fallible step returns a value instead of panicking.

use omuzaki_ai_api::{RunError, Signature};

/// What can go wrong while building a tracker or tracking a frame. "No face in this frame" is not an error.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum FaceTrackError {
    /// A model session refused a call or failed to run, or a graph's signature is not the one the tracker needs.
    #[error("model session: {0}")]
    Session(String),
    /// The pixel buffer does not hold a packed RGB8 image of this size (a zero side, a stride under `3 * width`, or
    /// too few bytes).
    #[error("{width}x{height} is not a valid RGB frame")]
    BadFrame {
        /// Width in pixels, as given.
        width: u32,
        /// Height in pixels, as given.
        height: u32,
    },
    /// The geometry metadata is not a `GeometryPipelineMetadata` with the canonical 468-vertex mesh.
    #[error("geometry metadata: {0}")]
    Metadata(String),
    /// A graph's output has the wrong number of values (or the region handed to
    /// [`FaceTracker::track_roi`](crate::FaceTracker::track_roi), graph `"roi"`, has no area).
    #[error("graph `{graph}` has the wrong shape")]
    Shape {
        /// Which graph: `"detector"`, `"landmarks"`, `"blendshapes"` or `"roi"`.
        graph: &'static str,
    },
}

/// Result alias used across the crate.
pub type Result<T> = std::result::Result<T, FaceTrackError>;

impl From<RunError> for FaceTrackError {
    fn from(error: RunError) -> Self {
        Self::Session(error.to_string())
    }
}

/// Checks that a session presents exactly the signature a graph of the tracker needs: it must fit
/// ([`Signature::accepts`]) and list its tensors in the same order with no extras, because the graphs read their
/// outputs by position.
pub(crate) fn check_signature(graph: &str, expected: &Signature, found: &Signature) -> Result<()> {
    let names = |specs: &[omuzaki_ai_api::TensorSpec]| specs.iter().map(|s| s.name.clone()).collect::<Vec<_>>();
    let ordered = names(&expected.inputs) == names(&found.inputs) && names(&expected.outputs) == names(&found.outputs);
    expected
        .accepts(found)
        .map_err(|mismatches| {
            let list: Vec<String> = mismatches.iter().map(ToString::to_string).collect();
            FaceTrackError::Session(format!("the {graph} session does not fit: {}", list.join("; ")))
        })
        .and_then(|()| {
            if ordered {
                Ok(())
            } else {
                Err(FaceTrackError::Session(format!(
                    "the {graph} session lists its tensors in another order or has extras"
                )))
            }
        })
}

#[cfg(test)]
mod tests {
    use omuzaki_ai_api::{DType, Dim, TensorSpec};

    use super::*;

    fn spec(name: &str) -> TensorSpec {
        TensorSpec::new(name, DType::F32, [Dim::Fixed(1)])
    }

    #[test]
    fn reordered_or_extra_tensors_are_refused_at_construction() {
        let expected = Signature { inputs: vec![spec("in")], outputs: vec![spec("a"), spec("b")] };
        assert!(check_signature("g", &expected, &expected).is_ok());
        let swapped = Signature { inputs: vec![spec("in")], outputs: vec![spec("b"), spec("a")] };
        assert!(
            matches!(check_signature("g", &expected, &swapped), Err(FaceTrackError::Session(m)) if m.contains("order"))
        );
        let extra = Signature { inputs: vec![spec("in")], outputs: vec![spec("a"), spec("b"), spec("c")] };
        assert!(check_signature("g", &expected, &extra).is_err());
    }
}
