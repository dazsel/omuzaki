//! [`ShellRequests`]: what a panel asks the shell to do (open a workspace, show or close a panel). Panels only get a
//! [`PanelCtx`](omuzaki_ui_api::panel::PanelCtx), which reads; the shell shares this queue with them as a view model and
//! carries out what it holds at the start of its next frame, in the order it was asked.

use std::cell::RefCell;
use std::rc::Rc;

use omuzaki_doc_api::Value;

/// One thing asked of the shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShellRequest {
    /// Make the workspace registered as this id the active one.
    Workspace(&'static str),
    /// Show the panel registered as this id in the active workspace, as View › Show panel does.
    ShowPanel(&'static str),
    /// Take the panel registered as this id out of the active workspace.
    ClosePanel(&'static str),
    /// Open the command palette on `op`'s form, filled with `args` (History's Adjust).
    Adjust {
        /// The op's name.
        op: String,
        /// Its arguments as they were run.
        args: Value,
    },
}

/// The queue of requests, shared by the shell and every panel (clones are one queue).
#[derive(Clone, Debug, Default)]
pub struct ShellRequests(Rc<RefCell<Vec<ShellRequest>>>);

impl ShellRequests {
    /// Asks for `request`; the shell carries it out at the start of its next frame.
    pub fn push(&self, request: ShellRequest) {
        self.0.borrow_mut().push(request);
    }

    /// Takes every request asked so far, oldest first.
    pub fn take(&self) -> Vec<ShellRequest> {
        std::mem::take(&mut *self.0.borrow_mut())
    }
}
