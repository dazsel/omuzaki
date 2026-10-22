//! The provenance the host writes for every output (D6): an adapter never builds one. A baseline from the manifest's
//! `licence.outputs`, then restrictions that only narrow, strictest wins (`no`, then `unknown`, then `ok`).

use omuzaki_connect_api::{Artifact, ConnectorConfig, ConnectorManifest, OutputsFrom, Request, join_commercial};
use omuzaki_store_api::{Commercial, Consent, Provenance, Source};

use crate::status::plan_allows;

/// The licence a user model gets when the person has not declared one (ADR 0016 rule 3).
pub const UNDECLARED_USER_MODEL: &str = "LicenseRef-User-Model-Undeclared";

/// What an output's rights depend on.
#[derive(Clone, Copy, Debug)]
pub struct RightsInputs<'a> {
    /// The connector's manifest.
    pub manifest: &'a ConnectorManifest,
    /// The person's settings for it (plan tier, declared model licences).
    pub config: &'a ConnectorConfig,
    /// The effective provenance of each input asset (`tray::input_rights`), in `Request::input_assets` order.
    pub inputs: &'a [&'a Provenance],
    /// Whether the request clones a voice.
    pub voice_clone: bool,
    /// The consent record for a cloned voice, as the host found it.
    pub consent: Option<&'a Consent>,
}

/// The commercial status and licence the outputs carry. `artifact` is `None` in the preview: a `user_model`
/// connector then reads the model the request names (`model`), and a `reported` one is `unknown` until an output
/// reports its model's rights.
pub fn output_rights(i: &RightsInputs<'_>, artifact: Option<&Artifact>, model: Option<&str>) -> (Commercial, String) {
    let policy = &i.manifest.licence;
    let (mut commercial, licence) = match policy.outputs {
        OutputsFrom::VendorTerms => (policy.commercial, policy.spdx.clone()),
        OutputsFrom::UserModel => {
            let model = artifact.map(|a| a.model.as_str()).or(model);
            match model.and_then(|m| i.config.models.get(m)) {
                Some(declared) => (declared.commercial, declared.spdx.clone()),
                None => (Commercial::Unknown, UNDECLARED_USER_MODEL.to_owned()),
            }
        }
        OutputsFrom::Reported => match artifact.and_then(|a| a.rights.as_ref()) {
            Some(r) => (r.commercial, r.spdx.clone()),
            None => (Commercial::Unknown, policy.spdx.clone()),
        },
    };
    if !plan_allows(&policy.requires_plan, i.config.plan.as_deref()) {
        commercial = join_commercial(commercial, Commercial::No);
    }
    for input in i.inputs {
        commercial = join_commercial(commercial, input.commercial);
    }
    if i.voice_clone && i.consent.is_none() {
        commercial = join_commercial(commercial, Commercial::Unknown);
    }
    (commercial, licence)
}

/// The record the store keeps for one output: `Source::Generated` with the connector, the artifact's model and seed,
/// the configured plan verbatim, the prompt and the input hashes; the licence and commercial status of
/// [`output_rights`]; the manifest's attribution; the consent found; private test material if any input is. Pure.
pub fn output_provenance(i: &RightsInputs<'_>, req: &Request, artifact: &Artifact) -> Provenance {
    let (commercial, licence) = output_rights(i, Some(artifact), None);
    Provenance {
        source: Source::Generated {
            tool: i.manifest.id.to_string(),
            model: artifact.model.clone(),
            plan: i.config.plan.clone(),
            prompt: req.prompt_text().map(str::to_owned),
            seed: artifact.seed,
            inputs: req.input_assets(),
        },
        author: None,
        licence,
        attribution: i.manifest.licence.attribution.clone(),
        commercial,
        consent: i.consent.cloned(),
        private_test: i.inputs.iter().any(|p| p.private_test),
    }
}

/// The model a request names, for a `user_model` connector's preview.
pub fn request_model(req: &Request) -> Option<&str> {
    match req {
        Request::Image(r) => r.model.as_deref(),
        Request::Llm(r) => r.model.as_deref(),
        Request::Vision(r) => r.model.as_deref(),
        _ => None,
    }
}
