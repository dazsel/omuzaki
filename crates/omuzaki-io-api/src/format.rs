//! Formats, and self-registration of importers and exporters by format (spec §39 rule 2).
//!
//! Each format crate registers its importer and/or exporter with [`register_importer!`](crate::register_importer) and
//! [`register_exporter!`](crate::register_exporter) next to the implementation. Nothing edits a central list; the
//! registry is whatever the linker collected, and the op layer generates `io.list_formats`, `io.import` and
//! `io.export` (with a `format` argument) from it.

use schemars::JsonSchema;
use serde::Serialize;

use crate::{Exporter, ImportInput, Importer};

omuzaki_core::slug_id!(
    /// A format's id: `glb`, `gltf`, `vrm1`, `vrm0`. Lower-case, so it is safe in op arguments and file names.
    FormatId
);

/// Static facts about a format, shared by every surface that lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FormatInfo {
    /// Stable id; must be a valid [`FormatId`] (the registry test checks every registered one).
    pub id: &'static str,
    /// Human name, "VRM 1.0".
    pub name: &'static str,
    /// File extensions without the dot, most common first (`["vrm"]`).
    pub extensions: &'static [&'static str],
    /// IANA media type (`model/gltf-binary`), or a vendor type where none exists.
    pub media_type: &'static str,
}

/// How sure an importer is that it can read some bytes.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// Not this format.
    No,
    /// The container matches but nothing more specific does: every GLB is "possibly" plain glTF.
    Possible,
    /// The distinguishing marker is present: `VRMC_vrm` in the JSON chunk makes a GLB a VRM 1.0.
    Certain,
}

/// One importer's answer when sniffing a file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Detection {
    /// The format id.
    pub format: &'static str,
    /// How sure.
    pub confidence: Confidence,
}

/// Registers an importer type so the registry (and every surface) finds it. `T` must be `Default`.
pub struct ImporterRegistration {
    /// Builds an instance.
    pub create: fn() -> Box<dyn Importer>,
}

/// Registers an exporter type. `T` must be `Default`.
pub struct ExporterRegistration {
    /// Builds an instance.
    pub create: fn() -> Box<dyn Exporter>,
}

fn make_importer<T: Importer + Default + 'static>() -> Box<dyn Importer> {
    Box::new(T::default())
}

fn make_exporter<T: Exporter + Default + 'static>() -> Box<dyn Exporter> {
    Box::new(T::default())
}

impl ImporterRegistration {
    /// The registration for `T`. A `const fn` so [`register_importer!`](crate::register_importer) works at link time.
    pub const fn of<T: Importer + Default + 'static>() -> Self {
        Self { create: make_importer::<T> }
    }
}

impl ExporterRegistration {
    /// The registration for `T`. A `const fn` so [`register_exporter!`](crate::register_exporter) works at link time.
    pub const fn of<T: Exporter + Default + 'static>() -> Self {
        Self { create: make_exporter::<T> }
    }
}

inventory::collect!(ImporterRegistration);
inventory::collect!(ExporterRegistration);

/// Every registered importer, in link order. Surfaces sort by id before showing them.
pub fn importers() -> impl Iterator<Item = Box<dyn Importer>> {
    inventory::iter::<ImporterRegistration>.into_iter().map(|r| (r.create)())
}

/// Every registered exporter, in link order.
pub fn exporters() -> impl Iterator<Item = Box<dyn Exporter>> {
    inventory::iter::<ExporterRegistration>.into_iter().map(|r| (r.create)())
}

/// The importer for `format`.
pub fn find_importer(format: &str) -> Option<Box<dyn Importer>> {
    importers().find(|i| i.info().id == format)
}

/// The exporter for `format`.
pub fn find_exporter(format: &str) -> Option<Box<dyn Exporter>> {
    exporters().find(|e| e.info().id == format)
}

/// What a format can do, for `io.list_formats`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FormatSupport {
    /// The format.
    pub info: FormatInfo,
    /// An importer is registered.
    pub import: bool,
    /// An exporter is registered.
    pub export: bool,
}

/// Every format with an importer, an exporter or both, sorted by id.
pub fn formats() -> Vec<FormatSupport> {
    let mut out: Vec<FormatSupport> = Vec::new();
    let mut note = |info: FormatInfo, import: bool, export: bool| match out.iter_mut().find(|f| f.info.id == info.id) {
        Some(f) => {
            f.import |= import;
            f.export |= export;
        }
        None => out.push(FormatSupport { info, import, export }),
    };
    for i in importers() {
        note(i.info(), true, false);
    }
    for e in exporters() {
        note(e.info(), false, true);
    }
    out.sort_by_key(|f| f.info.id);
    out
}

/// Asks every importer whether it can read `input`; the answers that aren't [`Confidence::No`], most sure first, ties
/// by id. This is how `io.import` picks a format when the user doesn't name one: a `.vrm` that contains `VRMC_vrm`
/// ranks the VRM 1.0 importer (`Certain`) above plain glTF (`Possible`).
pub fn sniff(input: &ImportInput<'_>) -> Vec<Detection> {
    let mut found: Vec<Detection> = importers()
        .map(|i| Detection { format: i.info().id, confidence: i.sniff(input) })
        .filter(|d| d.confidence != Confidence::No)
        .collect();
    found.sort_by(|a, b| b.confidence.cmp(&a.confidence).then(a.format.cmp(b.format)));
    found
}

/// Registers an importer type: `register_importer!(VrmImporter);`. Put it next to the type.
#[macro_export]
macro_rules! register_importer {
    ($ty:ty) => {
        $crate::__inventory::submit! { $crate::ImporterRegistration::of::<$ty>() }
        $crate::__record_registration!("importer", $ty);
    };
}

/// Registers an exporter type: `register_exporter!(VrmExporter);`. Put it next to the type.
#[macro_export]
macro_rules! register_exporter {
    ($ty:ty) => {
        $crate::__inventory::submit! { $crate::ExporterRegistration::of::<$ty>() }
        $crate::__record_registration!("exporter", $ty);
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confidence_orders_certain_above_possible_above_no() {
        assert!(Confidence::Certain > Confidence::Possible && Confidence::Possible > Confidence::No);
    }

    #[test]
    fn format_ids_are_validated_slugs() {
        assert!("vrm1".parse::<FormatId>().is_ok());
        assert!("VRM1".parse::<FormatId>().is_err());
    }
}
