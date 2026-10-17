//! Window plumbing shared by the app's secondary windows: the clean output window (#299) now, the desktop mascot
//! (#758) later, which reuses this module and adds only what it needs.
//!
//! What lives here is what both need and what the research (`docs/research/2026-10-mascot-window.md`, #133) settled:
//!
//! - [`size`]: a client area of an exact size in **physical** pixels, at any display scale, and the re-plan when the
//!   window lands on a monitor of another scale;
//! - [`placement`]: where the window was, remembered per monitor arrangement and restored only while that monitor
//!   still exists;
//! - [`surface`]: whether a transparent surface is verified for this platform and route, and the fallback when not.
//!
//! The windows are egui viewports on eframe's one event loop (winit allows one loop per process, research section 6).
//! The research's preferred route for transparency is the window's own wgpu surface on that shared loop; until that
//! lands, [`surface`] reports what the viewport route can honestly do.

pub mod placement;
pub mod size;
pub mod surface;

pub use placement::{Arrangement, PlacementBook};
pub use size::{PhysicalSize, points_for, resize_command};
pub use surface::{Backend, Platform, SurfaceRoute, Transparency, transparency};
