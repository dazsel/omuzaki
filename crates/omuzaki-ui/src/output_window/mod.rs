//! The clean output window (#299, BR-04): a second OS window that shows only the program output, for OBS's window
//! capture, Discord and Zoom screen share, or any other capture tool.
//!
//! - Borderless and not resizable, its client area **exactly** the output size in physical pixels at any display
//!   scale; it re-plans when it lands on a monitor of another scale ([`crate::aux_window::size`]).
//! - It shows the program frame the viewport already rendered (the same texture), never a second render, over the key
//!   colour ([`composite`]) or, where #133's research verified a transparent surface, over nothing; elsewhere it falls
//!   back to the key and says `fallback` ([`crate::aux_window::surface`]).
//! - It keeps presenting at the plan's rate while covered by other windows; a minimised window can't be captured, and
//!   its status says so rather than pretending.
//! - The pointer hides over it when the plan says so; no title bar, menu or overlay of the editor is drawn into it;
//!   always-on-top is off unless the person turns it on. Esc or the OS close it.
//! - Its position is remembered per monitor arrangement and restored only while that monitor exists.
//!
//! Until the viewport hands it the program texture it shows its backdrop; the [`test_card`] is what the evidence
//! renders through it. The `output.window.*` ops drive it through [`OutputHost`], the app's `WindowHost` (#298), and
//! the Live workspace's toggle runs those ops, so a person and an agent open the same window the same way.

pub mod composite;
pub mod host;
pub mod plan;
pub mod test_card;
pub mod window;

pub use host::OutputHost;
pub use plan::{DEFAULT_FPS, DEFAULT_KEY, OutputBackdrop, OutputPlan, Shown};
pub use test_card::test_card;
pub use window::{OutputStatus, OutputWindow, Program, paint_output};
