//! `check_crate!` on this crate's own (empty) text and its existing locale files.

mod fixture {
    /// This crate registers nothing, so there is nothing to link.
    pub fn link() {}
}

omuzaki_l10n_api::check_crate!(fixture);
