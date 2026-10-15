//! The Live workspace (#305, `docs/design/ux/shell.md` section 12) and the live slice's wiring (#312).
//!
//! | Module | Holds |
//! |---|---|
//! | [`wiring`] | the perform thread, the newest-wins hand-off, the render side and the quit order |

pub mod wiring;
