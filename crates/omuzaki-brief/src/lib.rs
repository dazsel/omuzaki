//! Characters from words (#2578): a written description to a finished character, through the kit's own ops.
//!
//! - [`lexicon`]: the words it knows, as data (`library/lexicon.toml`): facets, their options and what each does, named
//!   colours and the colour priors.
//! - [`parse`]: a description to choices, colours bound to what they colour, a name, and what was set aside or not
//!   understood.
//! - [`recipe`]: choices (from the caller, the words, or the priors by seed) to the kit's ops.
//! - [`ops`]: `character.create_from_description`, which plans every step in one transaction and checks the result
//!   against the choices, and `character.brief_vocabulary`, which lists the words for people and agents.
//!
//! It runs offline and is deterministic: the same words and seed always make the same character. An agent with a
//! language model of its own does better by reading the description itself and naming the options in `facets`.

pub mod lexicon;
pub mod ops;
pub mod parse;
pub mod recipe;
pub mod shades;

// Every registering crate the brief's ops call by name, linked so a binary that links this crate keeps their
// registrations (ADR 0015). `build.rs` writes the calls from the dependencies' `registers` flags (ADR 0054 section 6).
omuzaki_core::define_link! {
    include!(concat!(env!("OUT_DIR"), "/link_calls.rs"));
}
