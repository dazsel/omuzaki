//! Writes this crate's `link()` calls from its dependencies' `registers` flags (ADR 0054 section 6).

fn main() {
    omuzaki_link_build::emit();
}
