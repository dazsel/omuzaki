//! Plain glTF 2.0 and GLB for Omuzaki (spec v3 §27 EX-02), registered against `omuzaki-io-api` as format `glb`.
//!
//! One format, id `glb`, reads and writes both containers: a binary `.glb`, and a text `.gltf` whose buffers and images
//! are `data:` URIs or files the host supplies through the [`Resolver`](omuzaki_io_api::Resolver). The exporter's
//! [`Layout`](omuzaki_io_api::Layout) chooses between a single GLB and a `.gltf` with its sidecar files.
//!
//! Use [`run_import`](omuzaki_io_api::run_import) and [`run_export`](omuzaki_io_api::run_export), not the types here
//! directly: they add the size limits, scene invariants, rights rules and strict mode.
//!
//! The reading and writing is `omuzaki-gltf`'s (shared with the VRM crate, see ADR 0011): its crate documentation lists
//! what is read, what is dropped with a finding, the arena-order rule, the limits and the `gltf.*` rule table. What
//! this crate adds is the policy of *plain* glTF, which differs from VRM's:
//!
//! - **VRM data is not carried.** `VRMC_*` extensions, and the `VRM` extension of VRM 0.x on the root, are dropped
//!   with a warning that names the fix (import the file as `vrm1`, or `vrm0` for a 0.x file). Carrying them raw would
//!   let a plain-glTF export write licence terms the rights rules never saw. The warning for a root VRM extension
//!   quotes the licence fields its `meta` holds (`licenseName`, `commercialUssageName`, `licenseUrl`, ...), so a
//!   file whose terms were left behind isn't taken for one that had none, and the import carries
//!   [`LicenceEvidence::PresentNotImported`], so `run_import`
//!   reports `rights.license_not_imported` and never `rights.no_license_info` for it.
//! - **Unusable images and textures are dropped** (`gltf.image_dropped`, `gltf.texture_dropped`) rather than
//!   refusing the file; a plain scene refers to them through the compacted arenas.
//! - **Export** says what plain glTF can't hold: an avatar (`gltf.avatar_dropped`: humanoid, expressions and spring
//!   bones are VRM data) and `MToon` materials (`gltf.toon_dropped`: written as plain PBR).
#![deny(missing_docs)]

use omuzaki_gltf::{Carrier, Disposition, ExportHooks, ImportHooks, Tally};
use omuzaki_io_api::{
    Confidence, ExportCtx, ExportOptions, ExportOutcome, Exporter, FormatInfo, ImportCtx, ImportInput, ImportOutcome,
    Importer, IoError, IoScene, LicenceEvidence, Material, Report, Severity, Shading, register_exporter,
    register_importer,
};
use serde_json::Value;

fn info() -> FormatInfo {
    FormatInfo { id: "glb", name: "glTF 2.0", extensions: &["glb", "gltf"], media_type: "model/gltf-binary" }
}

/// The licence fields of VRM 0.x's `meta`, spelled as that spec spells them (`Ussage` and all).
const VRM0_LICENCE_FIELDS: [&str; 7] = [
    "licenseName",
    "allowedUserName",
    "violentUssageName",
    "sexualUssageName",
    "commercialUssageName",
    "otherPermissionUrl",
    "otherLicenseUrl",
];

/// The licence fields of VRM 1.0's `meta`: the permission flags and the licence documents, whichever of them a file has
/// (a meta with only a content flag still holds licence data).
const VRM1_LICENCE_FIELDS: [&str; 12] = [
    "licenseUrl",
    "avatarPermission",
    "allowExcessivelyViolentUsage",
    "allowExcessivelySexualUsage",
    "commercialUsage",
    "allowPoliticalOrReligiousUsage",
    "allowAntisocialOrHateUsage",
    "creditNotation",
    "allowRedistribution",
    "modification",
    "otherLicenseUrl",
    "thirdPartyLicenses",
];

/// The longest a quoted licence value may be, in characters: a licence name or a URL fits, a pasted licence text doesn't.
const QUOTE_CHARS: usize = 80;

/// `field=value` for each of `fields` that `meta` has with a non-empty value, values cut at [`QUOTE_CHARS`] and stripped
/// of control characters (the text is from an untrusted file and ends up in a report that may be printed to a
/// terminal). A value of the wrong type, an array or an object, is quoted as its JSON text: a malformed file still
/// holds licence data, and must never be read as holding none.
fn quote_licence_fields(meta: Option<&Value>, fields: &[&str]) -> Vec<String> {
    let quote = |value: &Value| {
        let text = match value {
            Value::String(text) => text.trim().to_owned(),
            Value::Bool(_) | Value::Number(_) => value.to_string(),
            Value::Array(items) if !items.is_empty() => value.to_string(),
            Value::Object(members) if !members.is_empty() => value.to_string(),
            _ => return None,
        };
        let clean: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
        let mut shown: String = clean.chars().take(QUOTE_CHARS).collect();
        if clean.chars().count() > QUOTE_CHARS {
            shown.push_str("...");
        }
        (!shown.is_empty()).then_some(shown)
    };
    fields.iter().filter_map(|&field| Some(format!("{field}={}", quote(meta?.get(field)?)?))).collect()
}

/// The VRM generation a root extension marks, and the licence fields of its `meta`.
fn vrm_root(name: &str) -> Option<(&'static str, &'static [&'static str])> {
    match name {
        "VRM" => Some(("VRM 0.x", &VRM0_LICENCE_FIELDS)),
        "VRMC_vrm" => Some(("VRM 1.0", &VRM1_LICENCE_FIELDS)),
        _ => None,
    }
}

/// What the "dropped" message of a VRM root extension adds: the licence terms are what matters, because a user must
/// not mistake a file whose terms were left behind for one that had none.
fn licence_note(version: &str, terms: &[String]) -> String {
    if terms.is_empty() {
        format!("; it holds no licence fields ({version} avatar)")
    } else {
        format!("; licence data present but not imported ({version} avatar): {}", terms.join(", "))
    }
}

/// The plain-glTF policy for reading. It remembers what the VRM root extensions it dropped said about the licence, so
/// the import can report it as such ([`LicenceEvidence`]).
struct PlainImport {
    licence: LicenceEvidence,
}

impl PlainImport {
    fn new() -> Self {
        Self { licence: LicenceEvidence::Absent }
    }

    /// Notes that a dropped extension held `terms` (quoted licence fields) of the `version` avatar, importable as
    /// `format`. A file with both a 0.x and a 1.0 extension keeps both summaries and names both importers.
    fn left_behind(&mut self, version: &str, terms: &[String], format: &'static str) {
        let summary = format!("{version} avatar: {}", terms.join(", "));
        match &mut self.licence {
            LicenceEvidence::PresentNotImported { summary: first, hint_format } => {
                first.push_str("; ");
                first.push_str(&summary);
                *hint_format = "vrm0 or vrm1";
            }
            other => *other = LicenceEvidence::PresentNotImported { summary, hint_format: format },
        }
    }
}

impl ImportHooks for PlainImport {
    fn extension(&mut self, carrier: Carrier, name: &str, body: &Value) -> Disposition {
        // `VRM` is the single extension of VRM 0.x, on the root; its meta carries the licence terms.
        let root = if carrier == Carrier::Root { vrm_root(name) } else { None };
        if root.is_none() && !name.starts_with("VRMC_") {
            return Disposition::Carry;
        }
        let mut message =
            format!("{} extension {name} was dropped: VRM data belongs to the VRM importer", carrier.noun());
        if let Some((version, fields)) = root {
            let terms = quote_licence_fields(body.get("meta"), fields);
            message.push_str(&licence_note(version, &terms));
            if !terms.is_empty() {
                self.left_behind(version, &terms, if name == "VRM" { "vrm0" } else { "vrm1" });
            }
        }
        let hint =
            if name == "VRM" { "import the file as vrm0 to keep it" } else { "import the file as vrm1 to keep it" };
        Disposition::Discard { severity: Severity::Warning, message, hint: Some(hint) }
    }
}

/// The plain-glTF policy for writing.
struct PlainExport;

impl ExportHooks for PlainExport {
    fn check_scene(&mut self, scene: &IoScene, tally: &mut Tally) {
        if scene.avatar.is_some() {
            let msg = "humanoid, expressions, spring bones and constraints are VRM data; export as vrm1 to keep them";
            tally.info("gltf.avatar_dropped", msg, "avatar");
        }
    }

    fn check_material(&mut self, index: usize, material: &Material, tally: &mut Tally) {
        if matches!(material.shading, Shading::Toon(_)) {
            let msg = "MToon materials were written as plain PBR: glTF core can't express toon shading";
            tally.warn("gltf.toon_dropped", msg, format!("materials/{index}"), Some("export as vrm1 to keep MToon"));
        }
    }
}

/// Reads glTF 2.0 as a GLB or as text JSON. Registered as format `glb`.
#[derive(Default)]
pub struct GltfImporter;

impl Importer for GltfImporter {
    fn info(&self) -> FormatInfo {
        info()
    }

    fn sniff(&self, input: &ImportInput<'_>) -> Confidence {
        if omuzaki_gltf::probe(input.bytes).is_some() { Confidence::Possible } else { Confidence::No }
    }

    fn import(&self, input: &ImportInput<'_>, ctx: &ImportCtx<'_>) -> Result<ImportOutcome, IoError> {
        ctx.progress.checkpoint("read glTF", 0.0)?;
        let parsed = omuzaki_gltf::parse(input.bytes, &ctx.options.limits)?;
        let mut hooks = PlainImport::new();
        let imported = omuzaki_gltf::read_scene(&parsed, ctx, &mut hooks)?;
        Ok(imported.into_outcome(hooks.licence))
    }
}

/// Writes glTF 2.0 as a GLB or as `.gltf` plus sidecar files. Registered as format `glb`.
#[derive(Default)]
pub struct GltfExporter;

impl Exporter for GltfExporter {
    fn info(&self) -> FormatInfo {
        info()
    }

    fn check(&self, scene: &IoScene, _: &ExportOptions) -> Report {
        omuzaki_gltf::check(scene, &mut PlainExport)
    }

    fn export(&self, scene: &IoScene, ctx: &ExportCtx<'_>) -> Result<ExportOutcome, IoError> {
        omuzaki_gltf::export(scene, ctx, &mut PlainExport)
    }
}

register_importer!(GltfImporter);
register_exporter!(GltfExporter);

omuzaki_core::define_link!();
