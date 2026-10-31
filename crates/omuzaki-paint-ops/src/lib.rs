//! The illustrator's ops, `paint.*`: a painting is kept in the document under `paintings/<id>` as layers of marks
//! (the `omuzaki-paint` model), so a person in the panels and an agent over MCP draw on the same painting, every mark
//! is one undoable step, and the pixels are always rebuilt from the marks.
//!
//! The flow an agent follows to redraw a part of a character:
//! 1. `paint.new` a painting the size of the picture, then `paint.add_layer` the layers a painter would use (line
//!    art, flats, shadow clipped to the flats, highlights, marks);
//! 2. `paint.brushes` to see the brushes, then `paint.draw` marks on a layer: strokes, shapes, bucket fills inside
//!    the line art, hatching; a batch of marks is one step;
//! 3. `paint.render` to look (a PNG in the asset store, `omuzaki://render/<hash>.png`, optionally cropped and scaled,
//!    or written to a project file), `paint.sample` to read colours back;
//! 4. fix what is wrong with `paint.replace_mark`, `paint.remove_marks` or undo, and `paint.set_layer` for blend,
//!    opacity, clipping and visibility;
//! 5. `paint.export_layers` writes the layers as layered art (PNGs and a `*.layers.toml`) that the 2D puppet's auto-rig
//!    turns into a puppet.
//!
//! No op takes pixels in: every mark is drawn by the caller, so what a painting holds is the caller's own work.

mod doc;
mod ops;

pub use doc::{PAINTINGS, painting_path};

omuzaki_core::define_link!();
