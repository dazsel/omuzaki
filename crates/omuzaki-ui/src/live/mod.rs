//! The Live workspace (#305, `docs/design/ux/shell.md` sections 12 and 13) and the live slice's wiring (#312).
//!
//! | Module | Holds |
//! |---|---|
//! | [`model`] | [`LiveModel`](model::LiveModel): the status it reads and the ops it runs, apart from drawing |
//! | [`panel`] | [`LivePanel`](panel::LivePanel): the workspace's controls, top to bottom in Tab order |
//! | [`registration`] | the Live panel, the Perform workspace and the story, registered with `omuzaki-ui-api` |
//! | [`story`] | the gallery story and the runner its states (and the evidence pictures) run on |
//! | [`text`] | every word the workspace says |
//! | [`wiring`] | the perform thread, the newest-wins hand-off, the render side and the quit order |

pub mod model;
pub mod panel;
pub mod registration;
pub mod story;
pub mod text;
pub mod wiring;

pub use model::LiveModel;
pub use panel::LivePanel;
