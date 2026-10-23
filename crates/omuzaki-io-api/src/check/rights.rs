//! The rights rules (`rights.*`) every export passes through [`preflight`](super::preflight).
//!
//! Two layers, because two different things can forbid an export:
//! - **Omuzaki's record** (the §7 provenance): the store's licence gate, and the cap that stops a file from granting more
//!   than the record holds (`rights.terms_exceed_provenance`): a record that is not cleared for commercial use, or is
//!   private test material, can only carry the strictest value of every permission, not just of commercial use.
//! - **The model's own terms** (`scene.meta`, what the creator wrote in the file): a [`Purpose::Publish`] export of
//!   someone else's model is **refused** when the terms do not allow the result to be shared (spec VF-08), with an error
//!   that names the licence field, its value and the creator to credit. The record's commercial status says nothing
//!   about this: a model whose creator forbids redistribution passes the store gate as long as commercial use is
//!   allowed. This used to live in the VRM exporters; it is here so that every exporter, plain glTF included, gets it.
//!
//! The model's-terms rule applies to [`Source::Imported`] material only. A model made in Omuzaki, taken from the library
//! or generated has the user as licensor, and the user can publish their own work under any terms they set.
//!
//! The scene does not record whether an imported model was edited before it is exported again, so the rule assumes it
//! may have been: a model is publishable only if the creator allows redistribution *and* allows modified copies to be
//! redistributed (`modification: allowModificationRedistribution`). A creator who allows redistribution of the
//! unmodified model only (`modification: prohibited`, the "no derivatives" licences) is not covered, because an export
//! is a conversion Omuzaki cannot prove unchanged. [`Purpose::Private`] is never blocked; it carries an info finding so
//! the person knows the file must stay on their machine.

use omuzaki_store_api::{Commercial, GateError, Provenance, Source, licence_gate};

use crate::meta::spelled;
use crate::{ExportOptions, Finding, IoScene, Modification, Purpose, Report, UseTerms};

fn gate_rule(e: &GateError) -> &'static str {
    match e {
        GateError::NonCommercial { .. } => "rights.gate_non_commercial",
        GateError::Unknown { .. } => "rights.gate_unknown",
        GateError::PrivateTest => "rights.gate_private_test",
        GateError::NoConsent => "rights.gate_no_consent",
    }
}

/// The rights rules:
/// - a provenance record must exist (`rights.provenance_missing`);
/// - for [`Purpose::Publish`] the licence gate must pass, with the gate's own explanation;
/// - the file's terms must not grant more than Omuzaki holds (`rights.terms_exceed_provenance`, one error per permission):
///   material that isn't cleared for commercial use, and private test material, can only carry the most restrictive
///   terms (an imported file keeps its own, see [`terms_within_record`]);
/// - an imported model's own terms must allow what the export is for (`rights.licence_forbids_redistribution`,
///   `rights.licence_forbids_modification`, or the info `rights.private_export_restricted`).
pub(super) fn rights(scene: &IoScene, options: &ExportOptions) -> Report {
    let mut r = Report::new();
    let Some(p) = &scene.provenance else {
        r.push(
            Finding::error("rights.provenance_missing", "the scene has no provenance record")
                .hint("import sets one; for a built character the op layer records it"),
        );
        return r;
    };
    r.extend(gate(p, options));
    r.extend(terms_within_record(&scene.meta.terms, p));
    r.extend(sharing_terms(scene, p, options.purpose));
    r
}

/// The store's licence gate for a [`Purpose::Publish`] export, with the gate's own explanation; nothing for a private
/// one. Shared by the model preflight and the animation preflight.
pub(super) fn gate(p: &Provenance, options: &ExportOptions) -> Report {
    let mut r = Report::new();
    if options.purpose == Purpose::Publish
        && let Err(e) = licence_gate(p, options.involves_real_person)
    {
        r.push(Finding::error(gate_rule(&e), e.to_string()).at("provenance"));
    }
    r
}

/// The file's terms may not grant more than the record holds. What the record holds depends on its status:
/// - **Private test material** holds no permission at all: every field is capped at the strictest value.
/// - **Not cleared for commercial use** (`Commercial::No`, a non-commercial or non-profit licence, or `Unknown`, which
///   the gate treats as `No`) holds no clearance either, so every permission is capped at the strictest value:
///   redistribution, modification, commercial use, who may be the avatar, the content flags and credit. A record that
///   asks for more would write a file claiming a permission nobody gave. Even a private file is refused, because a
///   private file leaks.
/// - **The exception is an imported file**: its terms are the creator's own, which is exactly what its source grants,
///   so they are written back as they were (the file's own sharing terms are checked by [`sharing_terms`]). Only
///   commercial use, the one permission Omuzaki's record settles, is capped.
/// - A **cleared** record caps nothing: its owner chooses the terms.
fn terms_within_record(terms: &UseTerms, p: &Provenance) -> Report {
    let mut r = Report::new();
    let strictest = UseTerms::default();
    let exceeding = terms.exceeding(&strictest);
    if p.private_test {
        let reason = "private test material can only be exported with the most restrictive terms".to_owned();
        let hint = "private test material never ships: set it back to";
        r.extend(exceeding_findings(terms, &exceeding, &strictest, &reason, hint));
        r.push(Finding::info("rights.private_test_export", "this file contains private test material: never share it"));
    } else if p.commercial != Commercial::Ok {
        let imported = matches!(p.source, Source::Imported { .. });
        let capped: Vec<&str> =
            exceeding.into_iter().filter(|field| !imported || *field == "commercialUsage").collect();
        let reason =
            format!("commercial status is {:?}, so only the most restrictive terms may be written", p.commercial);
        r.extend(exceeding_findings(
            terms,
            &capped,
            &strictest,
            &reason,
            "clear the source's licence first, or set it back to",
        ));
    }
    r
}

/// One `rights.terms_exceed_provenance` error per field, naming it, its value and the value it may have.
fn exceeding_findings(terms: &UseTerms, fields: &[&str], strictest: &UseTerms, reason: &str, hint: &str) -> Report {
    let (own, allowed) = (as_object(terms), as_object(strictest));
    let mut r = Report::new();
    for field in fields {
        let (value, default) = (spelled_member(&own, field), spelled_member(&allowed, field));
        r.push(
            Finding::error("rights.terms_exceed_provenance", format!("{reason}, but {field} is {value}"))
                .at(format!("meta/{field}"))
                .hint(format!("{hint} {default}")),
        );
    }
    r
}

fn as_object(terms: &UseTerms) -> serde_json::Value {
    serde_json::to_value(terms).unwrap_or_default()
}

fn spelled_member(object: &serde_json::Value, field: &str) -> String {
    object.get(field).map(ToString::to_string).unwrap_or_default()
}

/// Checks an imported model's own terms against the purpose of the export.
fn sharing_terms(scene: &IoScene, p: &Provenance, purpose: Purpose) -> Report {
    let mut report = Report::new();
    if !matches!(p.source, Source::Imported { .. }) {
        return report;
    }
    let breaches = breaches(scene);
    match purpose {
        Purpose::Publish => {
            for breach in breaches {
                let msg = format!("Publishing is refused. {}", breach.sentence);
                report.push(Finding::error(breach.rule, msg).at(breach.field).hint(breach.hint));
            }
        }
        Purpose::Private => {
            if let Some(first) = breaches.first() {
                // One note is enough: the gate would list the fields if the person published.
                let msg = format!("{} The file is for private use only: do not share it.", first.sentence);
                report.push(Finding::info("rights.private_export_restricted", msg).at(first.field));
            }
        }
    }
    report
}

/// One way the terms forbid publishing.
struct Breach {
    rule: &'static str,
    field: &'static str,
    /// The sentence naming the field, its value and the creator.
    sentence: String,
    hint: String,
}

fn breaches(scene: &IoScene) -> Vec<Breach> {
    let terms = &scene.meta.terms;
    let creator = creator(scene);
    let hint = match scene.meta.contact_information.as_deref().filter(|c| !c.trim().is_empty()) {
        Some(contact) => {
            format!("ask the creator ({contact}) for permission, or export for private use only")
        }
        None => "ask the creator for permission, or export for private use only".to_owned(),
    };
    let mut out = Vec::new();
    if !terms.allow_redistribution {
        out.push(Breach {
            rule: "rights.licence_forbids_redistribution",
            field: "meta/allowRedistribution",
            sentence: format!(
                "The licence field allowRedistribution is {} for {creator}, so the model may not be shared.",
                spelled(&terms.allow_redistribution)
            ),
            hint: hint.clone(),
        });
    }
    if terms.modification != Modification::AllowModificationRedistribution {
        let why = match terms.modification {
            Modification::Prohibited => "modification is prohibited, and an export is a conversion",
            _ => "modified copies may not be redistributed, and an export may differ from the original",
        };
        out.push(Breach {
            rule: "rights.licence_forbids_modification",
            field: "meta/modification",
            sentence: format!(
                "The licence field modification is {} for {creator}: {why}.",
                spelled(&terms.modification)
            ),
            hint,
        });
    }
    out
}

/// Who to credit: the model's name, its authors and its copyright holder, as the file gives them.
fn creator(scene: &IoScene) -> String {
    let meta = &scene.meta;
    let mut credit =
        if meta.name.trim().is_empty() { "the file's creator".to_owned() } else { format!("\"{}\"", meta.name) };
    if !meta.authors.is_empty() {
        credit = format!("{credit} by {}", meta.authors.join(", "));
    }
    match meta.copyright_information.as_deref().filter(|c| !c.trim().is_empty()) {
        Some(holder) => format!("{credit} ({holder})"),
        None => credit,
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_store_api::Consent;

    use super::*;
    use crate::{AvatarPermission, CommercialUsage, CreditNotation, Severity, VRM_LICENSE_URLS};

    fn imported(licence: &str, commercial: Commercial) -> Provenance {
        Provenance {
            source: Source::Imported { file: "m.vrm".into(), origin: None },
            author: Some("Someone".into()),
            licence: licence.into(),
            attribution: None,
            commercial,
            consent: None,
            private_test: false,
        }
    }

    /// An imported model by Someone under the given sharing terms, cleared for commercial use.
    fn model(redistribution: bool, modification: Modification) -> IoScene {
        let mut scene = IoScene::new();
        scene.meta = crate::VrmMeta::new("Minimal", vec!["Someone".into()], VRM_LICENSE_URLS[0]);
        scene.meta.copyright_information = Some("(c) Studio Fox".into());
        scene.meta.terms.allow_redistribution = redistribution;
        scene.meta.terms.modification = modification;
        scene.provenance = Some(imported("CC-BY-4.0", Commercial::Ok));
        scene
    }

    fn run(scene: &IoScene, purpose: Purpose) -> Report {
        rights(scene, &ExportOptions::default().with_purpose(purpose))
    }

    fn rules(report: &Report) -> Vec<&str> {
        let mut out: Vec<&str> = report.findings.iter().map(|f| f.rule.as_str()).collect();
        out.sort_unstable();
        out
    }

    #[test]
    fn publishing_a_restricted_model_is_refused_naming_the_field_the_value_and_the_creator() {
        let report = run(&model(false, Modification::Prohibited), Purpose::Publish);
        assert_eq!(rules(&report), ["rights.licence_forbids_modification", "rights.licence_forbids_redistribution"]);
        let f = report.findings.iter().find(|f| f.rule == "rights.licence_forbids_redistribution").unwrap();
        assert_eq!(f.location.as_deref(), Some("meta/allowRedistribution"));
        assert!(f.message.contains("allowRedistribution is false"), "{}", f.message);
        assert!(f.message.contains("\"Minimal\" by Someone ((c) Studio Fox)"), "{}", f.message);
        assert!(f.hint.as_deref().is_some_and(|h| h.contains("permission")));
    }

    #[test]
    fn each_forbidding_term_fires_on_its_own_and_a_free_licence_publishes() {
        let free = Modification::AllowModificationRedistribution;
        assert_eq!(
            rules(&run(&model(true, Modification::Prohibited), Purpose::Publish)),
            ["rights.licence_forbids_modification"]
        );
        assert_eq!(
            rules(&run(&model(true, Modification::AllowModification), Purpose::Publish)),
            ["rights.licence_forbids_modification"]
        );
        assert_eq!(rules(&run(&model(false, free), Purpose::Publish)), ["rights.licence_forbids_redistribution"]);
        assert_eq!(rules(&run(&model(true, free), Purpose::Publish)), Vec::<&str>::new());
    }

    #[test]
    fn a_private_export_is_allowed_with_an_info_note() {
        let report = run(&model(false, Modification::Prohibited), Purpose::Private);
        assert_eq!(rules(&report), ["rights.private_export_restricted"]);
        assert!(!report.has_errors());
        let free = model(true, Modification::AllowModificationRedistribution);
        assert_eq!(rules(&run(&free, Purpose::Private)), Vec::<&str>::new());
    }

    #[test]
    fn a_model_made_in_omuzaki_is_the_users_to_publish() {
        let mut scene = model(false, Modification::Prohibited);
        scene.provenance.as_mut().unwrap().source = Source::Authored;
        assert_eq!(rules(&run(&scene, Purpose::Publish)), Vec::<&str>::new());
    }

    #[test]
    fn an_unnamed_file_has_a_creator_to_ask_all_the_same() {
        let mut scene = model(false, Modification::Prohibited);
        scene.meta.name.clear();
        scene.meta.authors.clear();
        scene.meta.copyright_information = None;
        let report = run(&scene, Purpose::Publish);
        assert!(report.findings.iter().all(|f| f.message.contains("the file's creator")), "{report:?}");
    }

    #[test]
    fn the_store_gate_and_the_record_cap_still_apply() {
        let free = Modification::AllowModificationRedistribution;
        let mut scene = model(true, free);
        scene.provenance.as_mut().unwrap().commercial = Commercial::No;
        assert_eq!(rules(&run(&scene, Purpose::Publish)), ["rights.gate_non_commercial"]);
        assert!(run(&scene, Purpose::Private).findings.is_empty(), "a private export is not gated");
        scene.meta.terms.commercial_usage = CommercialUsage::PersonalProfit;
        assert!(run(&scene, Purpose::Private).has_rule("rights.terms_exceed_provenance"));

        let mut real_person = model(true, free);
        let options = ExportOptions::default().with_purpose(Purpose::Publish).with_real_person();
        assert!(rights(&real_person, &options).has_rule("rights.gate_no_consent"));
        real_person.provenance.as_mut().unwrap().consent =
            Some(Consent { person: "P".into(), scope: "this avatar".into(), document: None });
        assert!(!rights(&real_person, &options).has_errors());
    }

    /// Opens one permission of the terms.
    type Open = fn(&mut UseTerms);

    /// One permission opened at a time, by the `VRMC_vrm.meta` field it opens.
    const OPEN: [(&str, Open); 9] = [
        ("allowRedistribution", |t| t.allow_redistribution = true),
        ("modification", |t| t.modification = Modification::AllowModification),
        ("commercialUsage", |t| t.commercial_usage = CommercialUsage::PersonalProfit),
        ("avatarPermission", |t| t.avatar_permission = AvatarPermission::Everyone),
        ("allowExcessivelyViolentUsage", |t| t.allow_excessively_violent_usage = true),
        ("allowExcessivelySexualUsage", |t| t.allow_excessively_sexual_usage = true),
        ("allowPoliticalOrReligiousUsage", |t| t.allow_political_or_religious_usage = true),
        ("allowAntisocialOrHateUsage", |t| t.allow_antisocial_or_hate_usage = true),
        ("creditNotation", |t| t.credit_notation = CreditNotation::Unnecessary),
    ];

    /// A scene of the given source and commercial status, under the strictest terms except `open`.
    fn scene_with(source: Source, commercial: Commercial, open: Open) -> IoScene {
        let mut scene = model(false, Modification::Prohibited);
        scene.provenance = Some(Provenance { source, ..imported("LicenseRef-Unknown", commercial) });
        open(&mut scene.meta.terms);
        scene
    }

    fn exceed_findings(report: &Report) -> Vec<&Finding> {
        report.findings.iter().filter(|f| f.rule == "rights.terms_exceed_provenance").collect()
    }

    #[test]
    fn a_record_that_is_not_cleared_caps_every_permission_not_just_commercial_use() {
        // Non-commercial (a non-profit licence reads the same) and unknown, from every source but a file import.
        let sources = [Source::Authored, Source::Library { item: "hat@1".into() }];
        for source in sources {
            for commercial in [Commercial::No, Commercial::Unknown] {
                for (field, open) in OPEN {
                    let scene = scene_with(source.clone(), commercial, open);
                    // Even a private file is refused: it leaks.
                    for purpose in [Purpose::Private, Purpose::Publish] {
                        let report = run(&scene, purpose);
                        let found = exceed_findings(&report);
                        assert_eq!(found.len(), 1, "{field} {commercial:?} {purpose:?}: {report:?}");
                        let f = found[0];
                        assert_eq!(
                            (f.severity, f.location.as_deref()),
                            (Severity::Error, Some(&*format!("meta/{field}")))
                        );
                        assert!(
                            f.message.contains(field) && f.message.contains(&format!("{commercial:?}")),
                            "{}",
                            f.message
                        );
                        assert!(f.hint.as_deref().is_some_and(|h| h.contains("set it back")), "{:?}", f.hint);
                    }
                }
                // The strictest terms pass, and every open permission at once is named one by one.
                let strict = model_of(source.clone(), commercial);
                assert_eq!(exceed_findings(&run(&strict, Purpose::Private)).len(), 0);
                let mut all = strict;
                open_all(&mut all.meta.terms);
                assert_eq!(exceed_findings(&run(&all, Purpose::Private)).len(), OPEN.len());
            }
        }
    }

    fn open_all(terms: &mut UseTerms) {
        for (_, open) in OPEN {
            open(terms);
        }
    }

    fn model_of(source: Source, commercial: Commercial) -> IoScene {
        scene_with(source, commercial, |_| {})
    }

    #[test]
    fn private_test_material_caps_every_permission_even_when_it_is_cleared() {
        for commercial in [Commercial::Ok, Commercial::No] {
            for (field, open) in OPEN {
                let mut scene = scene_with(Source::Authored, commercial, open);
                scene.provenance.as_mut().unwrap().private_test = true;
                let report = run(&scene, Purpose::Private);
                let found = exceed_findings(&report);
                assert_eq!(found.len(), 1, "{field} {commercial:?}: {report:?}");
                assert_eq!(found[0].location.as_deref(), Some(&*format!("meta/{field}")));
                assert!(report.has_rule("rights.private_test_export"));
            }
        }
    }

    #[test]
    fn a_cleared_record_caps_nothing() {
        let mut scene = model_of(Source::Authored, Commercial::Ok);
        open_all(&mut scene.meta.terms);
        for purpose in [Purpose::Private, Purpose::Publish] {
            assert_eq!(rules(&run(&scene, purpose)), Vec::<&str>::new(), "{purpose:?}");
        }
    }

    #[test]
    fn an_imported_file_keeps_its_own_terms_and_only_commercial_use_is_capped() {
        // The creator's terms are the source's: a non-commercial file that lets people redistribute it says so.
        let imported_source = Source::Imported { file: "m.vrm".into(), origin: None };
        for (field, open) in OPEN {
            let scene = scene_with(imported_source.clone(), Commercial::No, open);
            let report = run(&scene, Purpose::Private);
            let found = exceed_findings(&report);
            if field == "commercialUsage" {
                assert_eq!(found.len(), 1, "{report:?}");
                assert_eq!(found[0].location.as_deref(), Some("meta/commercialUsage"));
            } else {
                assert!(found.is_empty(), "{field}: {report:?}");
            }
        }
    }

    #[test]
    fn a_missing_record_is_an_error() {
        let mut scene = model(true, Modification::AllowModificationRedistribution);
        scene.provenance = None;
        assert_eq!(rules(&run(&scene, Purpose::Private)), ["rights.provenance_missing"]);
    }
}
