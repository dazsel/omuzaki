//! Project goldens (spec VF-03): `render.golden.bless` records what a character's variants look like in a set of
//! shots (`render.golden.set_scene` edits them), and `render.golden.check` renders them again after an edit and holds each to its golden, so an edit meant
//! for the close-up face can't quietly move the game face.

pub mod bless;
pub mod check;
pub mod manifest;
pub mod scene;
