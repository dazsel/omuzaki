//! [`EditFocus`]: what the edit panels work on, the open character and the selected parts, shared between them.
//!
//! The selection is the shell's state, not the document's (no op changes it), and three panels read it: the outliner
//! changes it, the inspector edits its part, the viewport frames it. The shell makes one `EditFocus` per window and
//! shares it as a view model, and every edit panel takes a clone ([`EditFocus::from_ctx`]); the clones share one state.

use std::cell::RefCell;
use std::rc::Rc;

use omuzaki_char_api::{CharId, PartId};
use omuzaki_ui_api::panel::PanelCtx;

/// The open character and the selected parts, shared by every clone.
#[derive(Clone, Debug, Default)]
pub struct EditFocus(Rc<RefCell<Focus>>);

#[derive(Debug, Default)]
struct Focus {
    character: Option<CharId>,
    /// The selection, in the order it was made; the last is the primary part, the one the inspector edits.
    selected: Vec<PartId>,
    /// Counts every change, so a panel notices one with a number compare instead of a list compare each frame.
    generation: u64,
    /// The outliner asked the inspector to take the keyboard (Enter).
    to_inspector: bool,
    /// Someone asked the viewport to frame the selection.
    to_frame: bool,
}

impl EditFocus {
    /// Focus on `character`, nothing selected.
    pub fn new(character: CharId) -> Self {
        let focus = Self::default();
        focus.open(Some(character));
        focus
    }

    /// The focus the shell shares as a view model, or a focus of the panel's own (nothing open) when it shares none.
    pub fn from_ctx(ctx: &PanelCtx<'_>) -> Self {
        ctx.view::<Self>().cloned().unwrap_or_default()
    }

    /// Opens `character` (or none) and clears the selection.
    pub fn open(&self, character: Option<CharId>) {
        let mut f = self.0.borrow_mut();
        f.character = character;
        f.selected.clear();
        f.generation += 1;
    }

    /// The open character.
    pub fn character(&self) -> Option<CharId> {
        self.0.borrow().character.clone()
    }

    /// A number that changes whenever the character or the selection does.
    pub fn generation(&self) -> u64 {
        self.0.borrow().generation
    }

    /// Selects `part` alone.
    pub fn select(&self, part: PartId) {
        self.set(vec![part]);
    }

    /// Replaces the selection; the last part is the primary one.
    pub fn set(&self, parts: Vec<PartId>) {
        let mut f = self.0.borrow_mut();
        if f.selected != parts {
            f.selected = parts;
            f.generation += 1;
        }
    }

    /// Adds `part` to the selection, or takes it out when it is in it (Ctrl+click).
    pub fn toggle(&self, part: &PartId) {
        let mut f = self.0.borrow_mut();
        if let Some(at) = f.selected.iter().position(|p| p == part) {
            f.selected.remove(at);
        } else {
            f.selected.push(part.clone());
        }
        f.generation += 1;
    }

    /// Clears the selection.
    pub fn clear(&self) {
        self.set(Vec::new());
    }

    /// Whether `part` is selected.
    pub fn is_selected(&self, part: &PartId) -> bool {
        self.0.borrow().selected.iter().any(|p| p == part)
    }

    /// The selected parts, oldest first.
    pub fn selected(&self) -> Vec<PartId> {
        self.0.borrow().selected.clone()
    }

    /// How many parts are selected.
    pub fn count(&self) -> usize {
        self.0.borrow().selected.len()
    }

    /// The primary part: the last one selected.
    pub fn primary(&self) -> Option<PartId> {
        self.0.borrow().selected.last().cloned()
    }

    /// Asks the inspector to take the keyboard on its next frame.
    pub fn send_to_inspector(&self) {
        self.0.borrow_mut().to_inspector = true;
    }

    /// Whether the inspector was asked to take the keyboard; asking again is needed after this returns `true`.
    pub fn take_inspector_request(&self) -> bool {
        std::mem::take(&mut self.0.borrow_mut().to_inspector)
    }

    /// Asks the viewport to frame the selection on its next frame.
    pub fn frame_selection(&self) {
        self.0.borrow_mut().to_frame = true;
    }

    /// Whether the viewport was asked to frame the selection; asking again is needed after this returns `true`.
    pub fn take_frame_request(&self) -> bool {
        std::mem::take(&mut self.0.borrow_mut().to_frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(id: &str) -> PartId {
        PartId::new(id).unwrap_or_else(|_| panic!("a part id"))
    }

    #[test]
    fn clones_share_one_selection_and_every_change_moves_the_generation() {
        let a = EditFocus::new(CharId::new("mock").unwrap_or_else(|_| panic!("an id")));
        let b = a.clone();
        let start = b.generation();
        a.select(part("hair"));
        assert_eq!(b.primary(), Some(part("hair")));
        a.toggle(&part("head"));
        assert_eq!(b.selected(), vec![part("hair"), part("head")]);
        a.toggle(&part("hair"));
        assert_eq!((b.count(), b.primary()), (1, Some(part("head"))));
        assert!(b.generation() > start);
        let same = b.generation();
        a.set(vec![part("head")]);
        assert_eq!(b.generation(), same, "setting the same selection is no change");
        a.open(None);
        assert_eq!((b.character(), b.count()), (None, 0));
    }

    #[test]
    fn requests_are_taken_once() {
        let focus = EditFocus::default();
        focus.send_to_inspector();
        focus.frame_selection();
        assert!(focus.take_inspector_request() && !focus.take_inspector_request());
        assert!(focus.take_frame_request() && !focus.take_frame_request());
    }
}
