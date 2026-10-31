//! The `render.*` ops: `render.preview`, `render.turnaround`, `render.contact_sheet`, `render.compare` and `render.eye`
//! (the 3D eye close up), run over the stage. Each stores its picture in the content store and returns an `omuzaki://render/<hash>.png` URI that the next
//! one can take.

pub mod compare;
pub mod contact_sheet;
pub mod eye;
pub mod history;
pub mod job;
pub mod picture;
pub mod pose;
pub mod preview;
pub mod still;
pub mod turnaround;
