//! The Live workspace (#305, `docs/design/ux/shell.md` sections 12 and 13) and the live slice's wiring (#312).
//!
//! | Module | Holds |
//! |---|---|
//! | [`channels`] | [`ChannelsPanel`](channels::ChannelsPanel): every channel's meter and owner, from the perform thread |
//! | [`health`] | [`HealthPanel`](health::HealthPanel): the live-metrics snapshot, and the publisher the frame loop writes it with |
//! | [`hotkeys`] | [`Hotkeys`](hotkeys::Hotkeys): the keyboard's presses and releases, sent to the bound triggers |
//! | [`model`] | [`LiveModel`](model::LiveModel): the status it reads and the ops it runs, apart from drawing |
//! | [`panel`] | [`LivePanel`](panel::LivePanel): the workspace's controls, top to bottom in Tab order |
//! | [`registration`] | the Live panel, the Perform workspace and the story, registered with `omuzaki-ui-api` |
//! | [`strip`] | [`ExpressionStrip`](strip::ExpressionStrip): the bound expressions along the stage's foot |
//! | [`story`] | the gallery story and the runner its states (and the evidence pictures) run on |
//! | [`text`] | every word the workspace says |
//! | [`viewport`] | the stage view: the kit character on the real renderer, the program frame and the latency it measures |
//! | [`wiring`] | the perform thread, the newest-wins hand-off, the render side and the quit order |

pub mod channels;
pub mod health;
pub mod hotkeys;
pub mod latency;
pub mod model;
pub mod panel;
pub mod registration;
pub mod story;
pub mod strip;
pub mod text;
pub mod viewport;
pub mod wiring;

pub use model::LiveModel;
pub use panel::LivePanel;
