//! What self-registered into this binary, so a test can prove the linker kept every registration (ADR 0015), and
//! [`define_link!`](crate::define_link), which gives a crate the `link()` that keeps its registrations in a binary.
//!
//! Every `register_*!` macro in the workspace (ops, part kinds, templates, formats, bakers, ...) submits its
//! registration *and* a [`Registered`] record of it, from the same macro call. Both land in the same object file, so
//! the record is in the binary exactly when the registration is. [`registered`] lists them; the link gate (each App's
//! `tests/registrations.rs`) compares that list with what `cargo xtask registry` reads from the source, and fails on
//! any registration the binary lost.

use std::fmt;

/// One self-registration linked into this binary.
///
/// Written by the `register_*!` macros through [`__record_registration!`](crate::__record_registration); never built
/// by hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Registered {
    /// The package whose code called the macro (`omuzaki-char`).
    pub package: &'static str,
    /// The registry, named after the macro without `register_`: `op`, `part_kind`, `template`, ...
    pub kind: &'static str,
    /// The macro's first argument as written, without spaces (`CharacterCreate`, `"button"`). The gate reads the
    /// same text from the source, so the two can be compared by name.
    pub name: &'static str,
    /// The file of the macro call, as the compiler names it (`file!()`).
    pub file: &'static str,
    /// The line of the macro call.
    pub line: u32,
}

impl fmt::Display for Registered {
    /// `package kind name file:line`, tab-separated: what `omuzaki registrations` prints and the gate reads back.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}\t{}\t{}\t{}:{}", self.package, self.kind, self.name, self.file, self.line)
    }
}

inventory::collect!(Registered);

/// Every self-registration linked into this binary, sorted by package, kind and name.
pub fn registered() -> Vec<&'static Registered> {
    let mut all: Vec<&'static Registered> = inventory::iter::<Registered>.into_iter().collect();
    all.sort_unstable();
    all
}

/// Records one self-registration. Every `register_*!` macro calls it next to its own `inventory::submit!`, with its
/// kind and its first argument; the registry crates re-export it so the expansion can reach it through `$crate`.
#[doc(hidden)]
#[macro_export]
macro_rules! __record_registration {
    ($kind:literal, $($name:tt)+) => {
        $crate::__inventory::submit! {
            $crate::link::Registered {
                package: ::core::env!("CARGO_PKG_NAME"),
                kind: $kind,
                name: ::core::stringify!($($name)+),
                file: ::core::file!(),
                line: ::core::line!(),
            }
        }
    };
}

/// Defines the crate's `pub fn link()` (ADR 0015). Every crate whose library self-registers calls it once in
/// `src/lib.rs`, after the calls of the `link()` of the registering crates it depends on, if any:
///
/// ```ignore
/// omuzaki_core::define_link!();               // a crate that only registers
/// omuzaki_core::define_link! {                // one that also forwards, like omuzaki-stage
///     omuzaki_char::link();
///     omuzaki_anim::link();
/// }
/// ```
///
/// Every crate that depends on it calls `link()`, and Apps call it at startup, so a chain of real calls reaches every
/// registering crate. The function is never inlined, and it ends in `black_box`, which the optimiser must treat as
/// doing something unknown. An empty function would otherwise be one whose call link-time optimisation may delete,
/// taking with it the reference that keeps the crate's object file, and its registrations, in a wasm binary.
#[macro_export]
macro_rules! define_link {
    ($($forward:tt)*) => {
        /// Links the self-registrations of this crate, and of the crates it forwards to, into the binary that calls it
        /// (ADR 0015). Every crate that depends on this one calls it, and Apps call it at startup.
        #[inline(never)]
        pub fn link() {
            $($forward)*
            ::core::hint::black_box(());
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    // The recorder only stringifies its name, so the name needs no item behind it.
    crate::__record_registration!("probe", Probe);

    #[test]
    fn a_recorded_registration_names_its_package_kind_name_and_place() {
        let probes: Vec<&Registered> = registered().into_iter().filter(|r| r.kind == "probe").collect();
        assert_eq!(probes.len(), 1, "{probes:?}");
        let probe = probes[0];
        assert_eq!((probe.package, probe.name), ("omuzaki-core", "Probe"));
        assert!(probe.file.ends_with("link.rs"), "{}", probe.file);
        assert!(probe.line > 0);
    }
}
