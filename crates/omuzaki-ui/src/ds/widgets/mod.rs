//! The components. One file per component; every one takes a required accessible label (`design-system.md` section 7).
//!
//! They are custom-painted egui widgets: a row costs one interaction rectangle and a handful of shapes, reads its
//! colours from the theme in the context, and does not allocate once its text is cached.

pub mod approval_card;
pub mod banner;
pub mod button;
pub mod channel_meter;
pub mod chip;
pub mod colour_field;
pub mod curve_editor;
pub mod dialog;
pub mod dropdown;
pub mod empty_state;
pub mod form_row;
mod ids;
pub(crate) mod ime;
pub mod job_row;
pub mod keycap;
pub mod listbox;
pub mod number_field;
pub mod param_slider;
pub mod section;
pub mod segmented;
pub mod step_card;
pub mod tab;
pub mod text_field;
pub mod thumbnail;
pub mod toast;
pub mod toggle;
pub mod tooltip;
pub mod tree_row;

pub use approval_card::{ApprovalCard, Decision, PlanStep, Variant as ApprovalVariant};
pub use banner::{Banner, BannerOutput};
pub use button::{Button, ButtonKind};
pub use channel_meter::{Channel, ChannelMeter, MeterGrid, MeterGridOutput, Owner};
pub use chip::{ActorChip, ActorKind, CostChip, GateChip, ScopeBadge, StatusChip};
pub use colour_field::{ColourEvent, ColourField, ColourOutput, PaletteSlot};
pub use curve_editor::{Curve, CurveCause, CurveEditor, CurveError, CurveEvent, CurveOutput, CurvePoint, Preset};
pub use dialog::{Dialog, DialogAction, FirstFocus};
pub use dropdown::{Choice, Dropdown};
pub use empty_state::{EmptyOutput, EmptyState};
pub use form_row::FormRow;
pub use job_row::{JobOutput, JobRow, JobState};
pub use keycap::KeyCaps;
pub use listbox::{ListItem, Listbox, ListboxOutput, Row};
pub use number_field::{NumberField, NumberOutput};
pub use param_slider::{AgentTouch, CommitCause, OpCall, ParamEvent, ParamEvents, ParamOutput, ParamSlider};
pub use section::{Section, SectionOutput};
pub use segmented::{Segment, Segmented};
pub use step_card::{Change, Metric, StepAction, StepCard, StepOutput, StepState, Wants};
pub use tab::{TabBar, TabBarOutput, TabItem, TabKind};
pub use text_field::TextField;
pub use thumbnail::{Thumb, ThumbnailTile, TileOutput};
pub use toast::{LogEntry, Toast, ToastCard, ToastEvent, Toasts};
pub use toggle::{Toggle, TriState};
pub use tooltip::{Tip, TipExt};
pub use tree_row::{DropLine, Expand, RowHit, RowIcon, TreeRow, TreeRowOutput};

#[cfg(test)]
mod tests;
