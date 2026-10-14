//! What happens to each extension of the objects that carry them.
//!
//! Three outcomes, chosen by the format's [`ImportHooks::extension`]:
//! - **interpreted** (`KHR_materials_unlit`, `KHR_texture_transform`, ... are modelled here; a format's own, such as
//!   `VRMC_materials_mtoon`, are read by its hook): consumed into scene fields, never carried;
//! - **carried**: the raw JSON goes in the object's [`Passthrough`](omuzaki_io_api::Passthrough) bag and is written back,
//!   unless it points into buffer data this crate re-packs on export (then a carried copy would dangle);
//! - **discarded**: dropped with a finding, either because the format says so (the plain glTF importer drops `VRMC_*`)
//!   or because the scene has no place for it.

use serde_json::Value;

use crate::hooks::{Carrier, Disposition, ImportHooks};
use crate::parse::BUFFER_BOUND_EXTENSIONS;
use crate::schema::Extensions;
use crate::tally::Tally;

/// Applies the hooks' decision to one extension: true if the raw JSON should be carried. Anything else has been
/// reported (or is the format's own) by the time this returns.
fn keep(
    carrier: Carrier,
    (name, body): (&str, &Value),
    at: &str,
    hooks: &mut dyn ImportHooks,
    tally: &mut Tally,
) -> bool {
    let noun = carrier.noun();
    let location = format!("{at}/extensions/{name}");
    match hooks.extension(carrier, name, body) {
        Disposition::Interpreted => false,
        Disposition::Discard { severity, message, hint } => {
            tally.note(severity, "gltf.extension_dropped", message, location, hint);
            false
        }
        Disposition::Carry if carrier == Carrier::Mesh => {
            tally.info(
                "gltf.extension_dropped",
                format!("{noun} extension {name} has no place in the scene"),
                location,
            );
            false
        }
        Disposition::Carry if BUFFER_BOUND_EXTENSIONS.contains(&name) => {
            let msg = format!("{noun} extension {name} was dropped: it points into buffer data that export re-packs");
            tally.warn("gltf.extension_dropped", msg, location, None);
            false
        }
        Disposition::Carry => true,
    }
}

/// The extensions of one object that go into its pass-through bag; the rest are reported or are the format's own.
pub(crate) fn carried(
    extensions: Extensions,
    carrier: Carrier,
    at: &str,
    hooks: &mut dyn ImportHooks,
    tally: &mut Tally,
) -> Extensions {
    extensions.into_iter().filter(|(name, body)| keep(carrier, (name, body), at, hooks, tally)).collect()
}

/// Reports the extensions of an object that has no pass-through bag (a mesh), unless the format reads them itself.
pub(crate) fn report_without_bag(
    extensions: &Extensions,
    carrier: Carrier,
    at: &str,
    hooks: &mut dyn ImportHooks,
    tally: &mut Tally,
) {
    for (name, body) in extensions {
        keep(carrier, (name, body), at, hooks, tally);
    }
}

/// Reports extensions on objects the scene has no counterpart for at all (accessors, buffer views, samplers, ...): the
/// information is lost on import, and the user should know what kind it was. Repeats merge in the tally.
pub(crate) fn report_unmapped(doc: &crate::schema::Document, tally: &mut Tally) {
    let mut report = |noun: &str, place: String, extensions: &Extensions| {
        for name in extensions.keys() {
            tally.info(
                "gltf.extension_dropped",
                format!("{noun} extension {name} has no place in the scene"),
                format!("{place}/extensions/{name}"),
            );
        }
    };
    report("asset", "/asset".into(), &doc.asset.extensions);
    for (noun, array, extensions) in [
        ("scene", "scenes", doc.scenes.iter().map(|s| &s.extensions).collect::<Vec<_>>()),
        ("skin", "skins", doc.skins.iter().map(|s| &s.extensions).collect()),
        ("texture", "textures", doc.textures.iter().map(|t| &t.extensions).collect()),
        ("image", "images", doc.images.iter().map(|i| &i.extensions).collect()),
        ("sampler", "samplers", doc.samplers.iter().map(|s| &s.extensions).collect()),
        ("accessor", "accessors", doc.accessors.iter().map(|a| &a.extensions).collect()),
        ("buffer view", "bufferViews", doc.buffer_views.iter().map(|v| &v.extensions).collect()),
        ("buffer", "buffers", doc.buffers.iter().map(|b| &b.extensions).collect()),
    ] {
        for (i, extensions) in extensions.into_iter().enumerate() {
            report(noun, format!("/{array}/{i}"), extensions);
        }
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_io_api::Severity;
    use serde_json::json;

    use super::*;
    use crate::hooks::NoHooks;

    /// The plain-glTF policy of `omuzaki-io-gltf`, in miniature: VRM data is dropped with a warning.
    struct DropVrm;
    impl ImportHooks for DropVrm {
        fn extension(&mut self, carrier: Carrier, name: &str, _: &Value) -> Disposition {
            if name == "VRMC_own" {
                Disposition::Interpreted
            } else if name.starts_with("VRMC_") {
                Disposition::Discard {
                    severity: Severity::Warning,
                    message: format!("{} extension {name} was dropped", carrier.noun()),
                    hint: Some("import the file as vrm1 to keep it"),
                }
            } else {
                Disposition::Carry
            }
        }
    }

    fn map(entries: &[(&str, serde_json::Value)]) -> Extensions {
        entries.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect()
    }

    #[test]
    fn buffer_bound_extensions_are_dropped_and_the_rest_carried() {
        let ext = map(&[
            ("EXT_mesh_gpu_instancing", json!({"attributes": {"TRANSLATION": 3}})),
            ("KHR_materials_sheen", json!({"sheenRoughnessFactor": 0.8})),
        ]);
        let mut tally = Tally::default();
        let kept = carried(ext, Carrier::Node, "/nodes/0", &mut NoHooks, &mut tally);
        assert_eq!(kept.keys().collect::<Vec<_>>(), ["KHR_materials_sheen"]);
        let mut report = omuzaki_io_api::Report::new();
        tally.finish(&mut report);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].severity, Severity::Warning);
        assert_eq!(report.findings[0].location.as_deref(), Some("/nodes/0/extensions/EXT_mesh_gpu_instancing"));
    }

    #[test]
    fn the_format_decides_what_is_dropped_and_what_it_reads_itself() {
        let ext = map(&[
            ("VRMC_materials_mtoon", json!({})),
            ("VRMC_own", json!({})),
            ("KHR_materials_sheen", json!({"sheenRoughnessFactor": 0.8})),
        ]);
        let mut tally = Tally::default();
        let kept = carried(ext, Carrier::Material, "/materials/0", &mut DropVrm, &mut tally);
        assert_eq!(kept.keys().collect::<Vec<_>>(), ["KHR_materials_sheen"]);
        let mut report = omuzaki_io_api::Report::new();
        tally.finish(&mut report);
        assert_eq!(report.findings.len(), 1, "an interpreted extension is silent");
        let vrm = &report.findings[0];
        assert!(vrm.hint.as_deref().is_some_and(|h| h.contains("vrm1")));
        assert_eq!(vrm.location.as_deref(), Some("/materials/0/extensions/VRMC_materials_mtoon"));
    }

    #[test]
    fn a_mesh_has_no_bag_so_its_extensions_are_reported_unless_interpreted() {
        let mut tally = Tally::default();
        report_without_bag(&map(&[("EXT_a", json!({}))]), Carrier::Mesh, "/meshes/2", &mut NoHooks, &mut tally);
        let mut report = omuzaki_io_api::Report::new();
        tally.finish(&mut report);
        assert_eq!(report.findings.len(), 1);
        assert!(report.findings[0].message.contains("mesh extension EXT_a has no place"));
    }
}
