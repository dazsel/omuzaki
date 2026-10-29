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
//! character's parts and application records allow now and reports every field on which the record grants more (`licence.exceeds_parts`),
//! the way `licence.describe` does. `io.export` must call `run` before it writes anything, so an export and its
//! check can never disagree about a record that has gone stale.
//!
//! **Variants.** The character is checked as it is stored; a `variant` argument comes with `io.export`, which
//! must resolve it the same way.
//!
//! **Imported avatars.** A character imported from a model (`characters/<id>/avatar`, design D5) has no parts to
//! build: the scene it would export is its avatar rebuilt from the stored original and gathered back (D8), carrying
//! its source row's provenance, and the model's own terms come with its meta. So the rights gate refuses to publish a
//! model whose terms forbid sharing it, and lets the same file through for a private export. When the project holds a
//! licence record for it (someone read the model's actual licence, corrected the row and ran `licence.set`), the
//! record's terms and provenance replace the file's, and the record is held against the rows as for a kit character.

use omuzaki_avatar_api::body::BodyView;
use omuzaki_char_api::{
    BuildError, BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, CharacterDef,
    LookDef, PartCategory, PartId, Quality, RegistryBuilder, VariantError, VariantId, VariantSet, find_kind, paths,
    resolve_variant,
};
use omuzaki_core::glam::Vec3;
use omuzaki_doc_api::DocPath;
use omuzaki_face_api::{FaceProfile, FaceProjection};
use omuzaki_io_api::{
    CreditNotation, ExportOptions, Exporter, Finding, FormatId, IoScene, NodeIx, PerfectSync, Purpose, Report,
    Severity, VrmMeta, find_exporter, formats, preflight,
};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_store_api::{Provenance, Store};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::avatar::import::AVATAR_PART;
use super::avatar::{read_def, rebuild};
use super::licence::derive::load;
use super::licence::findings::against_stored_record;
use super::licence::source::{PartSource, source_path};
use crate::gather::avatar_import::gather_imported;
use crate::ops::avatar::attach::attach_parts;
use crate::gather::avatar::{LicenceRecord, add_avatar, add_rights, record_path};
use crate::gather::{GatherError, GatherOptions, GatherPurpose, Gathered, gather};

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

/// How `io.export` builds the character it writes; the check itself builds the stored character at its own size.
#[derive(Clone, Debug, Default)]
pub(crate) struct Shape {
    /// The variant to apply.
    pub variant: Option<VariantId>,
    /// The height to scale to, in metres (`GatherOptions::target_height`).
    pub target_height: Option<f32>,
}

/// The verdict on an export and the scene it would write, before the face is baked.
pub(crate) struct ExportScene {
    /// What `io.check_export` answers.
    pub check: ExportCheck,
    /// The scene, `None` when it can't be gathered (the check holds the error).
    pub scene: Option<IoScene>,
    /// The painted face to bake into it: `None` for an imported model, which carries its own expressions, and for a
    /// character without a face or a head mesh.
    pub face: Option<FaceToBake>,
}

/// What the face baker needs: the face, where it lands on the head, and the head's mesh node in the scene.
pub(crate) struct FaceToBake {
    /// The face design, palette slots resolved.
    pub profile: FaceProfile,
    /// Where the painted layers land on the head.
    pub projection: FaceProjection,
    /// The head centre the face is placed from, in the scene's (scaled) units.
    pub head_centre: Vec3,
    /// The scene's metres per head-local unit of the projection.
    pub scale: f32,
    /// The node whose mesh receives the face's morph targets.
    pub target: NodeIx,
}

/// The face `built` would bake into `gathered`: its face binding, where its head material places it, and the node of
/// its head part's head mesh (`<part>.<mesh>`, the mesh named `head` or `*-head`, as the stage frames the head). `None` when any is missing.
fn face_to_bake(built: &BuiltCharacter, gathered: &Gathered) -> Option<FaceToBake> {
    let binding = built.face.as_ref()?;
    let profile = binding.profile.resolve_colours(&built.palette).ok()?;
    let head_part = built.def.parts.iter().find_map(|(id, part)| {
        let category = (find_kind(part.kind.as_str())?.meta)().category;
        (part.enabled && category == PartCategory::Head).then_some(id)
    })?;
    let mesh = built.meshes.iter().find(|m| {
        &m.part == head_part && (m.mesh.name == "head" || m.mesh.name.ends_with("-head"))
    })?;
    let name = format!("{}.{}", mesh.part, mesh.mesh.name);
    let node = gathered.scene.nodes.iter().position(|n| n.name == name && n.mesh.is_some())?;
    // The face sits where the renderer draws it: the head material's overlay window, which places the projection (in
    // the painter's head-local units) on the head at the character's stature. The head bone is the head's base, not
    // its centre, and the stature scales the window.
    let overlay = built.materials.get(&mesh.mesh.material)?.overlays.first()?;
    let (centre, scale) = binding.projection.placed(overlay.layer, &overlay.window)?;
    Some(FaceToBake {
        profile,
        projection: binding.projection,
        head_centre: centre * gathered.scale,
        scale: scale * gathered.scale,
        target: NodeIx(u32::try_from(node).ok()?),
    })
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
    /// the parts and application records allow (an error for `publish`, a warning for `private`, an error for the private-test flag in both)
    /// and `rights.gate_private_test` for a private test part the record forgot to flag.
    ///
    /// # Errors
    /// `NotFound` for a character the document lacks; `Invalid` for a character that does not read or build, a record
    /// or a source row that does not read, and everything [`check`](Self::check) refuses.
    pub(crate) fn run(&self, ctx: &OpCtx<'_>) -> Result<ExportCheck, OpError> {
        self.run_scene(ctx, &Shape::default()).map(|exported| exported.check)
    }

    /// [`run`](Self::run) for `io.export`: the same verdict, and the scene the file is written from (built in `shape`,
    /// before the face is baked) with what the face bake needs. The scene is `None` only when the verdict already holds
    /// the error that stops the export (a creature cannot be a VRM).
    ///
    /// # Errors
    /// As [`run`](Self::run); `Invalid` for a variant the character lacks.
    pub(crate) fn run_scene(&self, ctx: &OpCtx<'_>, shape: &Shape) -> Result<ExportScene, OpError> {
        if ctx.doc.get(&paths::character(&self.character).join("avatar")).is_some() {
            return self.run_imported(ctx, shape);
        }
        let mut def = read_character(ctx, &self.character)?;
        if let Some(variant) = &shape.variant {
            def = with_variant(ctx, &self.character, &def, variant)?;
        }
        let record = read_record(ctx, &self.character)?;
        let parts = load(ctx, &self.character)?;
        let mut built = build(&self.character, &def, shape.variant.clone())?;
        apply_look(ctx, &self.character, &mut built)?;
        let (mut check, gathered) = self.check_gathered(&built, record.as_ref(), shape.target_height)?;
        check.add_findings(against_stored_record(&self.character, self.purpose, record.as_ref(), &parts));
        let face = gathered.as_ref().and_then(|g| face_to_bake(&built, g));
        Ok(ExportScene { check, scene: gathered.map(|g| g.scene), face })
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
        self.check_gathered(built, record, None).map(|(check, _)| check)
    }

    /// [`check`](Self::check), keeping the gathered scene (with its rights and, for a VRM, its avatar block) that the
    /// verdict is about, scaled to `target_height` if given; `None` when it can't be gathered (the verdict says why).
    fn check_gathered(
        &self,
        built: &BuiltCharacter,
        record: Option<&LicenceRecord>,
        target_height: Option<f32>,
    ) -> Result<(ExportCheck, Option<Gathered>), OpError> {
        let (exporter, options, vrm) = self.exporter()?;
        let purpose = if vrm { GatherPurpose::Vrm } else { GatherPurpose::Gltf };
        let mut report = Report::new();
        let kept = match gather(built, &GatherOptions { target_height, purpose }) {
            Ok(mut gathered) => {
                add_rights(&mut gathered.scene, built, record);
                if vrm {
                    add_avatar(&mut gathered, built).map_err(|e| gather_failed(&self.character, &e))?;
                }
                report.extend(std::mem::take(&mut gathered.report));
                report.extend(preflight(&gathered.scene, &options));
                report.extend(exporter.check(&gathered.scene, &options));
                Some(gathered)
            }
            // A creature cannot be a VRM: that is the answer to the question asked, not a failure to answer it.
            Err(GatherError::NotHumanoid(why)) => {
                report.push(
                    Finding::error("vrm.not_humanoid", format!("{why}; a VRM needs a humanoid skeleton"))
                        .at("skeleton")
                        .hint("export it as glb, or build the character from a humanoid body template"),
                );
                None
            }
            Err(other) => return Err(gather_failed(&self.character, &other)),
        };
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
        Ok((self.answer(report), kept))
    }

    /// The check of an imported avatar (see the module docs): the scene its export writes, with the provenance of its
    /// source row, through `preflight` and the exporter's own check.
    ///
    /// # Errors
    /// `Invalid` for a `format` with no exporter, an avatar node or source row that does not read, and an avatar that
    /// does not gather; `NotFound` for an original the store lacks, and no store service is an error too.
    fn run_imported(&self, ctx: &OpCtx<'_>, shape: &Shape) -> Result<ExportScene, OpError> {
        if shape.variant.is_some() {
            return Err(OpError::invalid("variant", "an imported character has no variants: export it without one"));
        }
        if shape.target_height.is_some() {
            return Err(OpError::invalid("target_height", "an imported character exports at the size its file has")
                .hint("leave target_height out; resize the model in the tool that made it"));
        }
        let (exporter, options, _) = self.exporter()?;
        let def = read_def(ctx.doc, &self.character)?;
        let mut avatar = rebuild(ctx, def.clone())?.avatar;
        // Its attachments, built as the stage builds them: what a person sees on the model is what the file carries.
        attach_parts(&mut avatar, &self.character, &def.parts, &BuildServices::default(), Quality::Offline)?;
        let store = ctx.service::<dyn Store>()?;
        let mut scene = gather_imported(&BodyView::imported(&avatar), &def, &|hash| store.get(hash).ok())
            .map_err(|e| {
                OpError::invalid("character", format!("character '{}' can't be written to a file: {e}", self.character))
            })?;
        scene.provenance = imported_provenance(ctx, &self.character)?;
        // A stored record (`licence.set`, bounded by the source rows) is what the project says the model's licence is,
        // so its terms replace the file's own: a file's meta may say less than its licence (a VRoid Hub `Other` reads
        // as "no modification, no redistribution" for a model published under CC0). The record is held against the
        // rows below, so a record that has gone stale is reported, never trusted.
        let record = read_record(ctx, &self.character)?;
        if let Some(record) = &record {
            apply_record(&mut scene.meta, record);
            scene.provenance = Some(record.provenance.clone());
        }
        let mut report = preflight(&scene, &options);
        report.extend(exporter.check(&scene, &options));
        // Its face is the model's own expressions, which the scene already carries: there is nothing to bake.
        let mut check = self.answer(report);
        if record.is_some() {
            check.add_findings(against_stored_record(&self.character, self.purpose, record.as_ref(), &load(ctx, &self.character)?));
        }
        Ok(ExportScene { check, scene: Some(scene), face: None })
    }

    /// The exporter of `self.format`, the options the check runs it with, and whether the format is a VRM one.
    ///
    /// # Errors
    /// `Invalid` for a format with no exporter.
    fn exporter(&self) -> Result<(Box<dyn Exporter>, ExportOptions, bool), OpError> {
        let exporter = find_exporter(self.format.as_str()).ok_or_else(|| {
            OpError::invalid("format", format!("no exporter is registered for format {}", self.format))
                .hint(exportable_formats())
        })?;
        // Format ids are slugs (`glb`, `vrm1`, `vrm0`), and only the VRM ones have an avatar block.
        let vrm = self.format.as_str().starts_with("vrm");
        let options = ExportOptions::default().with_purpose(self.purpose).with_perfect_sync(PerfectSync::Off);
        Ok((exporter, options, vrm))
    }

    /// The answer for `report`: the fix hints filled in, errors first, and the counts.
    fn answer(&self, mut report: Report) -> ExportCheck {
        add_fix_hints(&mut report, &record_path(&self.character));
        // A stable sort: a person or agent reading the answer meets what blocks the export before the notes.
        report.findings.sort_by_key(|f| std::cmp::Reverse(f.severity));
        ExportCheck {
            character: self.character.to_string(),
            format: self.format.to_string(),
            purpose: self.purpose,
            ok: !report.has_errors(),
            errors: report.count(Severity::Error),
            warnings: report.count(Severity::Warning),
            report,
        }
    }
}

/// The provenance of an imported character's model: its `sources/avatar` row's, which the import recorded from the
/// asset; `None` when the row is missing (the rights gate then reports the missing record).
///
/// # Errors
/// `Invalid` for a row that does not read.
fn imported_provenance(ctx: &OpCtx<'_>, id: &CharId) -> Result<Option<Provenance>, OpError> {
    let part = PartId::new(AVATAR_PART).map_err(|e| OpError::internal(format!("the avatar part id: {e}")))?;
    let path = source_path(id, &part);
    let row = ctx.doc.get(&path).map(PartSource::from_node).transpose().map_err(|e| {
        OpError::invalid("sources/avatar", format!("the source row of the imported model at {path}: {e}"))
            .hint("the row is written by io.import; import the model again")
    })?;
    Ok(row.map(|r| r.provenance))
}

/// Writes an imported model's licence from its stored `record` over the file's own meta: the terms, the licence
/// documents and the copyright line, with the record's attribution as the credit (as [`add_rights`] carries it). The
/// rest of the file's meta (its name, authors, thumbnail and vendor keys) stays, since it describes this file.
fn apply_record(meta: &mut VrmMeta, record: &LicenceRecord) {
    let given = &record.meta;
    meta.terms = given.terms.clone();
    if !given.license_url.trim().is_empty() {
        meta.license_url.clone_from(&given.license_url);
    }
    meta.other_license_url.clone_from(&given.other_license_url);
    if given.copyright_information.as_deref().is_some_and(|c| !c.trim().is_empty()) {
        meta.copyright_information.clone_from(&given.copyright_information);
    }
    if let Some(credit) = record.provenance.attribution.as_deref().filter(|a| !a.trim().is_empty()) {
        meta.terms.credit_notation = CreditNotation::Required;
        if meta.copyright_information.as_deref().is_none_or(|c| c.trim().is_empty()) {
            meta.copyright_information = Some(credit.to_owned());
        }
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

/// Draws `built` in the character's look (`characters/<id>/look`), so the file carries it: a VRM exported in cel-flat
/// looks cel-flat in any `MToon` renderer. A character with no look node is left as built, the default look.
///
/// # Errors
/// `Invalid` (field `look`) for a look node that doesn't read or resolve, or that the materials can't take.
fn apply_look(ctx: &OpCtx<'_>, id: &CharId, built: &mut BuiltCharacter) -> Result<(), OpError> {
    let Some(node) = ctx.doc.get(&paths::look(id)) else { return Ok(()) };
    let look = LookDef::from_node(Some(node)).and_then(|def| def.resolve()).map_err(|e| {
        OpError::invalid("look", format!("the look of '{id}' doesn't resolve: {e}"))
            .hint("set it again with look.set_preset or look.set_param")
    })?;
    built.apply_look(&look).map(drop).map_err(|e| {
        OpError::invalid("look", format!("the look of '{id}' can't be drawn: {e}"))
            .hint("set it again with look.set_preset or look.set_param")
    })
}

/// `base` with `variant` applied, from the variants stored under the character.
fn with_variant(ctx: &OpCtx<'_>, id: &CharId, base: &CharacterDef, variant: &VariantId) -> Result<CharacterDef, OpError> {
    let variants: VariantSet = match ctx.doc.get(&paths::variants(id)) {
        Some(node) => serde_json::from_value(node.clone())
            .map_err(|e| OpError::invalid("variant", format!("the variants of '{id}' don't read: {e}")))?,
        None => VariantSet::new(),
    };
    resolve_variant(base, &variants, variant).map_err(|e| match e {
        VariantError::Unknown(missing) if &missing == variant => {
            let names: Vec<&str> = variants.keys().map(VariantId::as_str).collect();
            OpError::not_found(format!("there is no variant '{variant}' on '{id}'")).hint(if names.is_empty() {
                "the character has no variants".to_owned()
            } else {
                format!("its variants: {}", names.join(", "))
            })
        }
        other => OpError::invalid("variant", format!("variant '{variant}' of '{id}' doesn't resolve: {other}")),
    })
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

fn build(id: &CharId, def: &CharacterDef, variant: Option<VariantId>) -> Result<BuiltCharacter, OpError> {
    // What an export needs: offline detail, and the spring chains and skinned meshes that hair and cloth bake for it.
    let options = BuildOptions { variant, quality: Quality::Offline, rig_export: true };
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
