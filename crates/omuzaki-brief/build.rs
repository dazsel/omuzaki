//! Writes the `link()` calls of the registering crates this one depends on (ADR 0054 §6).

fn main() {
    omuzaki_link_build::emit();
}
