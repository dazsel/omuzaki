//! The rights rules an animation export passes through (`docs/design/io-api.md` D11, "Rights";
//! `docs/design/vrma-export.md` V12): the model preflight's rights part without the terms, since a VRMA carries no
//! permission flags and so cannot say more than its record does, plus the no-derivatives rule.

use omuzaki_store_api::Provenance;

use super::rights::gate;
use crate::{ExportOptions, Finding, IoAnimation, Purpose, Report};

/// The rights rules for an animation:
/// - a provenance record must exist (`rights.provenance_missing`);
/// - for [`Purpose::Publish`] the store's licence gate must pass (the same function and messages as a model);
/// - private test material is flagged (`rights.private_test_export`, info); only a private export allows it;
/// - a no-derivatives licence refuses a published export ([`licence_forbids_derivatives`]) and notes a private one
///   (`rights.nd_private_export`, info).
pub(crate) fn animation_rights(animation: &IoAnimation, options: &ExportOptions) -> Report {
    let mut r = Report::new();
    let Some(p) = &animation.provenance else {
        r.push(
            Finding::error("rights.provenance_missing", "the animation has no provenance record")
                .hint("the export op records the clip's own record"),
        );
        return r;
    };
    r.extend(gate(p, options));
    if p.private_test {
        r.push(Finding::info("rights.private_test_export", "this file contains private test material: never share it"));
    }
    if let Some(nd) = licence_forbids_derivatives(p) {
        match options.purpose {
            Purpose::Publish => r.push(nd),
            Purpose::Private => r.push(
                Finding::info(
                    "rights.nd_private_export",
                    format!(
                        "This motion's licence ({}) does not allow derivatives: local use only. The file carries the \
                         licence and the attribution.",
                        p.licence
                    ),
                )
                .at("provenance/licence"),
            ),
        }
    }
    r
}

/// The no-derivatives rule (vrma-export.md V12): an error `rights.licence_forbids_derivatives` when the record's
/// licence names an identifier with an `ND` segment (`CC-BY-ND-4.0`, `CC-BY-NC-ND-4.0`, a `LicenseRef-...-ND-...`).
///
/// The licence is split into identifiers on whitespace, parentheses and the `AND`, `OR` and `WITH` operators, and each
/// identifier into `-`-separated segments, so `ND` inside another word never matches. *Any* identifier counts: a joined
/// record's `A AND B` includes `B`'s terms, and for an `OR` there is no knowing which one the person took.
///
/// Resampling, retargeting and editing make an exported motion a derivative, so publishing one is refused. Library and
/// share uploads of a motion call this same function.
pub fn licence_forbids_derivatives(p: &Provenance) -> Option<Finding> {
    let nd = p
        .licence
        .split(|c: char| c.is_whitespace() || c == '(' || c == ')')
        .filter(|id| !matches!(*id, "" | "AND" | "OR" | "WITH"))
        .any(|id| id.split('-').any(|segment| segment == "ND"));
    nd.then(|| {
        Finding::error(
            "rights.licence_forbids_derivatives",
            format!(
                "This motion's licence ({}) does not allow derivatives. Resampling, retargeting and editing make an \
                 exported VRMA a derivative, so it cannot be published or uploaded.",
                p.licence
            ),
        )
        .at("provenance/licence")
        .hint("export it for your own use, or use a clip whose licence allows adaptation")
    })
}
