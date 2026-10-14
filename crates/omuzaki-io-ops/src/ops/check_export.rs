//! `io.check_export`: the export preflight as a query. It builds and gathers the character, runs `preflight` and the
//! exporter's own checks, and returns the findings with their fix hints without writing anything.
//!
//! The check is the dry half of `io.export` for everything that can be known before the face is painted: the licence
//! gate and the file's terms, the VRM meta, the humanoid, the scene's own invariants and the exporter's rules. It
//! builds the character the way an export does (offline quality, with the spring chains and skinned bake meshes of
//! hair and cloth) and reads its [`LicenceRecord`] from the document.
//!
//! **The 52 face channels are not checked.** They are baked when the file is written, by the face baker the app
//! registered; the check runs without it, so it turns that rule off and says so in a finding instead of reporting 52
//! missing channels on every call.
//!
//! **The record against the parts.** The stored record is a snapshot, so `CheckExport::run` also derives what the
//! character's parts allow now and reports every field on which the record grants more (`licence.exceeds_parts`),
//! the way `licence.describe` does. `io.export` must call `run` before it writes anything, so an export and its
//! check can never disagree about a record that has gone stale.
//!
//! **Variants.** The character is checked as it is stored; a `variant` argument comes with `io.export`, which
//! must resolve it the same way.

use omuzaki_char_api::{
    BuildError, BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, CharacterDef, Quality,
    RegistryBuilder, paths,
};
use omuzaki_doc_api::DocPath;
use omuzaki_io_api::{
    ExportOptions, Finding, FormatId, PerfectSync, Purpose, Report, Severity, find_exporter, formats, preflight,
};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::licence::derive::load;
use super::licence::findings::against_stored_record;
use crate::gather::avatar::{LicenceRecord, add_avatar, add_rights, record_path};
use crate::gather::{GatherError, GatherOptions, GatherPurpose, gather};

/// Checks whether a character would export cleanly, and says how to fix what would not. Writes nothing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CheckExport {
    /// The character to check.
    pub character: CharId,
    /// The format to check it for (see `io.list_formats`): `vrm1`, `glb`.
    pub format: FormatId,
    /// What the file is for. `private` (the default) is a file for your own machine; `publish` is anything that leaves
    /// it, and the licence gate must pass.
    #[serde(default)]
    pub purpose: Purpose,
}

/// What `io.check_export` returns.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExportCheck {
    /// The character that was checked.
    pub character: String,
    /// The format it was checked for.
    pub format: String,
    /// The purpose it was checked for.
    pub purpose: Purpose,
    /// True when no finding is an error: nothing the check can see would stop the export.
    pub ok: bool,
    /// How many findings are errors.
    pub errors: usize,
    /// How many findings are warnings.
    pub warnings: usize,
    /// Every finding with its rule id, location and fix hint: errors first, then warnings, then notes, and within each
    /// in the order the checks ran (gather, `preflight`, the exporter's own).
    pub report: Report,
}

impl ExportCheck {
    /// Adds findings found after the scene-level check, keeping the order (errors first, a stable sort) and the counts
    /// true.
    fn add_findings(&mut self, findings: Vec<Finding>) {
        for finding in findings {
            self.report.push(finding);
        }
        self.report.findings.sort_by_key(|f| std::cmp::Reverse(f.severity));
        self.errors = self.report.count(Severity::Error);
        self.warnings = self.report.count(Severity::Warning);
        self.ok = !self.report.has_errors();
    }
}

impl CheckExport {
    /// The whole check, from the document: reads the character, its licence record and its parts' source rows, builds
    /// it, runs [`check`](Self::check) and then holds the record against the parts as they are now.
    ///
    /// This is the one path to a verdict on an export, so `io.check_export` and `io.export` share it. The record is a
    /// snapshot that goes stale when a part changes, and [`check`](Self::check), which sees the built character and the
    /// record only, cannot tell; this adds `licence.exceeds_parts` for every field on which the record grants more than
    /// the parts allow (an error for `publish`, a warning for `private`, an error for the private-test flag in both)
    /// and `rights.gate_private_test` for a private test part the record forgot to flag.
    ///
    /// # Errors
    /// `NotFound` for a character the document lacks; `Invalid` for a character that does not read or build, a record
    /// or a source row that does not read, and everything [`check`](Self::check) refuses.
    pub(crate) fn run(&self, ctx: &OpCtx<'_>) -> Result<ExportCheck, OpError> {
        let def = read_character(ctx, &self.character)?;
        let record = read_record(ctx, &self.character)?;
        let parts = load(ctx, &self.character)?;
        let built = build(&self.character, &def)?;
        let mut check = self.check(&built, record.as_ref())?;
        check.add_findings(against_stored_record(&self.character, self.purpose, record.as_ref(), &parts));
        Ok(check)
    }

    /// Checks `built` against the exporter of `self.format` under the rights in `record`, and nothing about the parts:
    /// `run` holds the record against them.
    ///
    /// Everything about the character that makes the export fail is a finding in the answer. Only a request that cannot
    /// be checked at all is an error.
    ///
    /// # Errors
    /// `Invalid` for a `format` with no exporter and for a character that cannot be gathered into a scene.
    pub fn check(&self, built: &BuiltCharacter, record: Option<&LicenceRecord>) -> Result<ExportCheck, OpError> {
        let exporter = find_exporter(self.format.as_str()).ok_or_else(|| {
            OpError::invalid("format", format!("no exporter is registered for format {}", self.format))
                .hint(exportable_formats())
        })?;
        // Format ids are slugs (`glb`, `vrm1`, `vrm0`), and only the VRM ones have an avatar block.
        let vrm = self.format.as_str().starts_with("vrm");
        let options = ExportOptions::default().with_purpose(self.purpose).with_perfect_sync(PerfectSync::Off);
        let purpose = if vrm { GatherPurpose::Vrm } else { GatherPurpose::Gltf };
        let mut report = Report::new();
        match gather(built, &GatherOptions { target_height: None, purpose }) {
            Ok(mut gathered) => {
                add_rights(&mut gathered.scene, built, record);
                if vrm {
                    add_avatar(&mut gathered, built).map_err(|e| gather_failed(&self.character, &e))?;
                }
                report.extend(gathered.report);
                report.extend(preflight(&gathered.scene, &options));
                report.extend(exporter.check(&gathered.scene, &options));
            }
            // A creature cannot be a VRM: that is the answer to the question asked, not a failure to answer it.
            Err(GatherError::NotHumanoid(why)) => report.push(
                Finding::error("vrm.not_humanoid", format!("{why}; a VRM needs a humanoid skeleton"))
                    .at("skeleton")
                    .hint("export it as glb, or build the character from a humanoid body template"),
            ),
            Err(other) => return Err(gather_failed(&self.character, &other)),
        }
        if vrm {
            let msg = "the 52 face channels are baked when the file is written, so this check does not look for them";
            report
                .push(Finding::info("perfect_sync.deferred", msg).hint("io.export bakes them and lists every channel"));
        }
        // Whether a real person is involved is not in the character's data, so the gate's consent rule is not applied
        // here; say so unless the record already holds a consent.
        if record.is_some_and(|r| r.provenance.consent.is_none()) {
            let msg = "this check can't tell whether a real person's likeness or voice is involved, so the consent rule is not applied";
            report.push(Finding::info("rights.consent_not_checked", msg));
        }
        add_fix_hints(&mut report, &record_path(&self.character));
        // A stable sort: a person or agent reading the answer meets what blocks the export before the notes.
        report.findings.sort_by_key(|f| std::cmp::Reverse(f.severity));
        Ok(ExportCheck {
            character: self.character.to_string(),
            format: self.format.to_string(),
            purpose: self.purpose,
            ok: !report.has_errors(),
            errors: report.count(Severity::Error),
            warnings: report.count(Severity::Warning),
            report,
        })
    }
}

/// "exportable formats: glb, vrm1", for hints.
fn exportable_formats() -> String {
    let ids: Vec<&str> = formats().iter().filter(|f| f.export).map(|f| f.info.id).collect();
    if ids.is_empty() {
        "this build has no exporters".to_owned()
    } else {
        format!("exportable formats: {}", ids.join(", "))
    }
}

fn gather_failed(character: &CharId, error: &GatherError) -> OpError {
    OpError::invalid("character", format!("character '{character}' can't be written to a file: {error}")).hint(
        "the message names the mesh or bone at fault; fix that part, or report it if the part kind is a library one",
    )
}

/// Fills in the hint of a finding that has none, for the rules a person can fix in the project: the licence gate and
/// the VRM meta. `rights.provenance_missing` gets the op layer's hint even though `omuzaki-io-api` has one: its "for a
/// built character the op layer records it" is not true of a project nobody recorded yet.
///
/// The humanoid rules (`vrm.humanoid_*`) cannot fire for a built character: a humanoid `Skeleton` cannot exist without
/// its 15 required bones, so the only bone problem is a generic skeleton, which `vrm.not_humanoid` answers.
fn add_fix_hints(report: &mut Report, record: &DocPath) {
    for finding in &mut report.findings {
        let replaces = finding.rule == "rights.provenance_missing";
        if replaces || finding.hint.is_none() {
            finding.hint = fix_hint(&finding.rule, record).or(finding.hint.take());
        }
    }
}

fn fix_hint(rule: &str, record: &DocPath) -> Option<String> {
    let hint = match rule {
        "rights.provenance_missing" => format!(
            "the project holds no licence record for this character: run licence.derive and store its record at \
             {record} with licence.set (a provenance table and, for a VRM, the meta keys); rights are never guessed \
             from the parts"
        ),
        "rights.gate_non_commercial" => format!(
            "replace the non-commercial parts, or correct the record at {record} if it is wrong; for your own \
             machine export with purpose \"private\""
        ),
        "rights.gate_unknown" => format!(
            "record where the character comes from and under what licence at {record}, and mark it commercial \"ok\" \
             only once someone has read that licence; for your own machine export with purpose \"private\""
        ),
        "rights.gate_private_test" => {
            "private test material never leaves this machine: export with purpose \"private\"".to_owned()
        }
        "rights.consent_not_checked" => format!(
            "if a real person's likeness or voice is involved, record their consent in provenance.consent at {record}; \
             the gate asks for it once the export is told a real person is involved"
        ),
        "vrm.meta_name_missing" => format!("name the character, or set name in the meta of {record}"),
        "vrm.meta_authors_missing" => format!("set provenance.author or the meta authors in {record}"),
        _ => return None,
    };
    Some(hint)
}

/// The character stored under `id`.
fn read_character(ctx: &OpCtx<'_>, id: &CharId) -> Result<CharacterDef, OpError> {
    let node = ctx.doc.get(&paths::character(id)).ok_or_else(|| {
        OpError::not_found(format!("there is no character '{id}'"))
            .hint("check the id; characters live under characters/<id>")
    })?;
    CharacterDef::from_node(node)
        .map_err(|e| OpError::invalid("character", format!("character '{id}' doesn't read as a character: {e}")))
}

/// The character's licence record, if the project holds one. The licence ops read it the same way.
pub(crate) fn read_record(ctx: &OpCtx<'_>, id: &CharId) -> Result<Option<LicenceRecord>, OpError> {
    let path = record_path(id);
    ctx.doc.get(&path).map(LicenceRecord::from_node).transpose().map_err(|e| {
        OpError::invalid(e.field(), format!("the licence record at {path}: {e}")).hint(
            "a record is `schema = 1`, a `provenance` table and, optionally, a `meta` table with the keys of a VRM's meta",
        )
    })
}

fn build(id: &CharId, def: &CharacterDef) -> Result<BuiltCharacter, OpError> {
    // What an export needs: offline detail, and the spring chains and skinned meshes that hair and cloth bake for it.
    let options = BuildOptions { variant: None, quality: Quality::Offline, rig_export: true };
    RegistryBuilder.build(id, def, &options, &BuildServices::default()).map_err(|e: BuildError| {
        OpError::invalid("character", format!("character '{id}' doesn't build: {e}"))
            .hint("fix the part the message names, then check again")
    })
}

impl Op for CheckExport {
    fn meta() -> OpMeta {
        OpMeta {
            name: "io.check_export",
            summary: "Check whether a character would export to a format (VRM, glTF) and say how to fix what would \
                      not: licence gate, VRM meta, humanoid bones. Writes nothing.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let check = self.run(ctx)?;
        let output = serde_json::to_value(check)
            .map_err(|e| OpError::internal(format!("serialising the export check failed: {e}")))?;
        Ok(Plan::answer(output))
    }
}

register_op!(CheckExport);
