//! The history's pictures (spec VC-01): a thumbnail of the document after every committed transaction, rendered on a
//! worker of its own and indexed by transaction id, so the history panel can show what each step looked like.

pub mod index;
pub mod thumbs;
