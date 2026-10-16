//! The GPU clip and transform stacks: Vello's layers must balance, so the encoder tracks what is open.

use omuzaki_face_api::FaceError;
use vello::kurbo::Affine;

/// A stack of open pushes, each remembering the index of the command that opened it.
#[derive(Default)]
pub(super) struct OpenStack<T> {
    open: Vec<(usize, T)>,
}

impl<T> OpenStack<T> {
    /// Forgets every open push, keeping the capacity.
    pub(super) fn clear(&mut self) {
        self.open.clear();
    }

    /// Opens a push made by command `index`.
    pub(super) fn push(&mut self, index: usize, value: T) {
        self.open.push((index, value));
    }

    /// Closes the innermost push for the pop at command `index`.
    ///
    /// # Errors
    /// [`FaceError::Raster`] naming `index` and `what` if nothing is open.
    pub(super) fn pop(&mut self, index: usize, what: &str) -> Result<T, FaceError> {
        self.open
            .pop()
            .map(|(_, v)| v)
            .ok_or_else(|| FaceError::Raster(format!("command {index}: Pop{what} without a Push{what}")))
    }

    /// The innermost value.
    pub(super) fn top(&self) -> Option<&T> {
        self.open.last().map(|(_, v)| v)
    }

    /// The command index of the oldest push that is still open, if any.
    pub(super) fn oldest_open(&self) -> Option<usize> {
        self.open.first().map(|(i, _)| *i)
    }

    /// How many pushes are open.
    pub(super) fn depth(&self) -> usize {
        self.open.len()
    }
}

/// The composed transforms of the open `PushTransform`s; empty means the layer-to-pixel transform alone.
pub(super) type Transforms = OpenStack<Affine>;
