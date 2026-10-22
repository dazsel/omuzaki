//! The preview (CN-04, D4 to D6): what a call would send, where, at what price and under which rights, before
//! anything runs. Pure and offline: it reads the manifest, the settings, the store and the consent records, and calls
//! no connector's `run` and no transport. The run builds it again from the same snapshot and refuses any widening.

use omuzaki_ai_api::{Likeness, Requester};
use omuzaki_connect_api::net::{Endpoint, Scheme};
use omuzaki_connect_api::{
    ConnectError, ConnectorConfig, ConnectorId, ConnectorManifest, InputRights, Outbound, Preview, PreviewError,
    Request, RequestProblem, Sends, Status, Target, Tri, VoiceChoice, check_params,
};
use omuzaki_store_api::{Commercial, Consent, ContentHash, Provenance};

use crate::host::{Host, Resolved};
use crate::rights::{RightsInputs, output_rights, request_model};
use crate::status::plan_allows;

impl Host {
    /// What `req` would send, where, at what price and under which rights; see
    /// [`Connectors::preview`](omuzaki_connect_api::Connectors::preview).
    ///
    /// # Errors
    /// A [`PreviewError`] saying what to fix.
    pub fn preview(
        &self,
        req: &Request,
        connector: Option<&ConnectorId>,
        who: Requester,
        inputs: &InputRights,
    ) -> Result<Preview, PreviewError> {
        let entry = self.choose(req.capability(), connector)?;
        self.preview_resolved(&self.resolve(entry), req, who, inputs).map(|(preview, _)| preview)
    }

    /// The preview of `req` on a connector resolved once, and the consent record a voice clone found. In order: the
    /// capability, the request and its params, enabled, the target, the key, the inputs' rights, consent, what is
    /// sent, the estimate, the outputs' rights, warnings and whether a person must confirm.
    pub(crate) fn preview_resolved(
        &self,
        r: &Resolved<'_>,
        req: &Request,
        who: Requester,
        inputs: &InputRights,
    ) -> Result<(Preview, Option<Consent>), PreviewError> {
        let m = &r.entry.manifest;
        let id = m.id.clone();
        let capability = req.capability();
        let decl = m.declares(capability).ok_or_else(|| PreviewError::Unsupported(format!("{id} does not make {capability}")))?;
        let mut problems = req.check();
        if req.count() > u32::from(decl.max_count) {
            problems.push(RequestProblem::new("count", format!("{id} makes at most {} at a time", decl.max_count)));
        }
        if let Some(params) = req.params() {
            problems.extend(check_params(&decl.params, params));
        }
        if !problems.is_empty() {
            return Err(PreviewError::Invalid(problems));
        }
        if !r.config.enabled {
            return Err(PreviewError::Disabled(id));
        }
        let target = r.target.as_ref().map_err(|e| PreviewError::Connector(e.clone()))?;
        match self.status_of(r) {
            Status::NeedsKey => return Err(PreviewError::NeedsKey(id)),
            Status::Unconfigured => {
                return Err(PreviewError::Connector(ConnectError::unavailable(format!(
                    "{id}'s settings are incomplete; finish them on the Connectors page"
                ))));
            }
            _ => {}
        }

        let assets = req.input_assets();
        let mut rights: Vec<&Provenance> = Vec::with_capacity(assets.len());
        for hash in &assets {
            let p = inputs.get(hash).ok_or(PreviewError::UnknownAsset(*hash))?;
            if target.leaves_machine && p.private_test {
                return Err(PreviewError::PrivateInput(*hash));
            }
            rights.push(p);
        }
        let mut consent = None;
        let mut voice_clone = false;
        if let Request::Voice(v) = req
            && let VoiceChoice::Clone { person, .. } = &v.voice
        {
            voice_clone = true;
            consent = Some(
                self.parts
                    .consent
                    .find(person, Likeness::VoiceClone)
                    .ok_or_else(|| PreviewError::needs_consent(person.clone()))?,
            );
        }

        let outbound = self.outbound(req, &assets)?;
        let estimate = r.entry.connector.estimate(m, target, req).map_err(PreviewError::Connector)?;
        let rights_in = RightsInputs {
            manifest: m,
            config: &r.config,
            inputs: &rights,
            voice_clone,
            consent: consent.as_ref(),
        };
        let (commercial, licence) = output_rights(&rights_in, None, request_model(req));

        let warnings = warnings(m, &r.config, target, commercial);
        let mut preview = Preview {
            connector: id,
            name: m.name.clone(),
            capability,
            leaves_machine: target.leaves_machine,
            destinations: target.destinations.clone(),
            endpoint: target.endpoint.clone(),
            outbound,
            needs_spend: estimate.usd_micros > 0,
            estimate,
            retention: m.privacy.retention.clone(),
            trains_on_inputs: m.privacy.trains_on_inputs,
            commercial,
            licence,
            plan: r.config.plan.clone(),
            secret: target.secret.clone(),
            needs_person: None,
            warnings,
        };
        let sends_assets = preview.outbound.iter().any(|o| o.what != Sends::Prompt);
        if who == Requester::Agent && preview.leaves_machine && sends_assets {
            preview.needs_person = Some(preview.english());
        }
        Ok((preview, consent))
    }

    /// What `req` sends, by kind: its text as one prompt, and each input asset by what its bytes are.
    fn outbound(&self, req: &Request, assets: &[ContentHash]) -> Result<Vec<Outbound>, PreviewError> {
        let mut outbound: Vec<Outbound> = Vec::new();
        let text = req.text_bytes();
        if text > 0 {
            outbound.push(Outbound { what: Sends::Prompt, count: 1, bytes: text });
        }
        for hash in assets {
            let bytes = self.parts.store.get(hash).map_err(|_| PreviewError::UnknownAsset(*hash))?;
            let what = sniff(&bytes).1;
            let len = bytes.len() as u64;
            match outbound.iter_mut().find(|o| o.what == what) {
                Some(row) => {
                    row.count = row.count.saturating_add(1);
                    row.bytes = row.bytes.saturating_add(len);
                }
                None => outbound.push(Outbound { what, count: 1, bytes: len }),
            }
        }
        Ok(outbound)
    }
}

/// Things to know before the call: another endpoint than the shipped one, an unencrypted one on another computer,
/// training terms, and outputs that can't be published (the plan tier, or their licence).
fn warnings(m: &ConnectorManifest, config: &ConnectorConfig, target: &Target, commercial: Commercial) -> Vec<String> {
    let mut warnings = Vec::new();
    if let Some(url) = &target.endpoint {
        if m.endpoint.as_ref() != Some(url) {
            warnings.push(format!("it uses the endpoint {url} from its settings"));
        }
        if let Some(e) = Endpoint::parse(url)
            && e.scheme == Scheme::Http
            && !e.is_loopback()
        {
            warnings.push(format!(
                "{} is another computer reached over plain http://: what is sent is not encrypted, and no key \
                 is ever sent over it",
                e.host
            ));
        }
    }
    if target.leaves_machine {
        match m.privacy.trains_on_inputs {
            Tri::No => {}
            Tri::Yes => warnings.push("the vendor may train on what it is sent".to_owned()),
            Tri::Unknown => {
                warnings.push("the vendor's terms don't say whether it trains on what it is sent".to_owned());
            }
        }
    }
    let plan_ok = plan_allows(&m.licence.requires_plan, config.plan.as_deref());
    if !plan_ok {
        warnings.push(format!(
            "on the plan {}, the outputs belong to the vendor and can't be published; it needs one of: {}",
            config.plan.as_deref().unwrap_or("(not set)"),
            m.licence.requires_plan.join(", ")
        ));
    } else if commercial != Commercial::Ok {
        warnings.push(format!(
            "the outputs will be {}: they can't be published until their licence allows commercial use",
            match commercial {
                Commercial::No => "non-commercial",
                _ => "of unknown commercial status",
            }
        ));
    }
    warnings
}

/// The media type of `bytes` from their magic numbers, and what the preview calls them; unknown bytes are a file.
pub(crate) fn sniff(bytes: &[u8]) -> (&'static str, Sends) {
    let at = |offset: usize, magic: &[u8]| bytes.get(offset..offset + magic.len()) == Some(magic);
    if at(0, b"\x89PNG\r\n\x1a\n") {
        ("image/png", Sends::Images)
    } else if at(0, &[0xFF, 0xD8, 0xFF]) {
        ("image/jpeg", Sends::Images)
    } else if at(0, b"RIFF") && at(8, b"WEBP") {
        ("image/webp", Sends::Images)
    } else if at(0, b"RIFF") && at(8, b"WAVE") {
        ("audio/wav", Sends::Audio)
    } else if at(0, b"OggS") {
        ("audio/ogg", Sends::Audio)
    } else if at(0, b"fLaC") {
        ("audio/flac", Sends::Audio)
    } else if at(0, b"ID3") {
        ("audio/mpeg", Sends::Audio)
    } else if at(0, b"glTF") {
        ("model/gltf-binary", Sends::Meshes)
    } else if at(4, b"ftyp") {
        ("video/mp4", Sends::Video)
    } else if at(0, b"HIERARCHY") {
        ("text/plain", Sends::Files)
    } else {
        ("application/octet-stream", Sends::Files)
    }
}
