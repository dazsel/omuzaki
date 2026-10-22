//! The run (D4, D6, D9, D10): the only code that calls `Connector::run`. It resolves the target once, builds the
//! preview again and refuses anything wider than the plan's, checks the cap, draws the seed from the run id, hands the
//! adapter a [`RunCx`] (inputs limited to the request's assets, the key attached by [`AuthedHttp`]), then stores each
//! output with the provenance the host builds and turns it into a pending candidate.

use std::collections::BTreeSet;

use omuzaki_ai_api::Progress;
use omuzaki_connect_api::{
    Call, Candidate, CandidateId, CandidateState, ConnectError, ConnectorId, InputAsset, Inputs, Meter, Output,
    Request, RunCx, RunEnv, RunError, RunOutput, RunResult, Target, Widening,
};
use omuzaki_core::rng::Rng;
use omuzaki_http_api::Cancel;
use omuzaki_secrets_api::prelude::SecretValue;
use omuzaki_store_api::{ContentHash, Provenance, Store};
use serde_json::Value;

use crate::auth::{AuthedHttp, Scrubber};
use crate::host::{Host, Resolved};
use crate::preview::sniff;
use crate::rights::{RightsInputs, output_provenance};

/// A connector's view of the store: only the assets its request names. Any other hash is refused and nothing is read.
struct StoreInputs<'a> {
    store: &'a dyn Store,
    allowed: BTreeSet<ContentHash>,
}

impl Inputs for StoreInputs<'_> {
    fn get(&self, hash: &ContentHash) -> Result<InputAsset, ConnectError> {
        if !self.allowed.contains(hash) {
            return Err(ConnectError::refused(format!("asset {hash} is not an input of this request")));
        }
        let bytes = self.store.get(hash).map_err(|e| ConnectError::failed(format!("reading asset {hash}: {e}")))?;
        let media_type = sniff(&bytes).0.to_owned();
        Ok(InputAsset { bytes, media_type })
    }
}

/// A stored output: its hash, media type, kind and provenance.
type Stored = (ContentHash, String, omuzaki_connect_api::MediaKind, Provenance);

/// The meter of a ping: nothing may be billed.
struct NoCharge;

impl Meter for NoCharge {
    fn charge(&self, usd_micros: u64) -> Result<(), ConnectError> {
        Err(ConnectError::cap_reached(0, usd_micros))
    }
}

/// A borrowed [`Progress`] as the owned cancel handle a ping's context needs: never cancelled after the call.
struct Never;

impl Cancel for Never {
    fn is_cancelled(&self) -> bool {
        false
    }
}

impl Host {
    /// Runs `call`; see [`Connectors::run`](omuzaki_connect_api::Connectors::run).
    ///
    /// # Errors
    /// A [`RunError`] when nothing came back.
    pub fn run(&self, call: &Call, env: &RunEnv<'_>) -> Result<RunResult, RunError> {
        let entry = self.choose(call.request.capability(), call.connector.as_ref()).map_err(RunError::Preview)?;
        let resolved = self.resolve(entry);
        let (preview, consent) =
            self.preview_resolved(&resolved, &call.request, call.requester, &call.inputs).map_err(RunError::Preview)?;
        if let Some(planned) = &call.planned {
            if planned.connector != preview.connector {
                return Err(RunError::preview_changed(Widening::destination(preview.connector.to_string())));
            }
            if let Some(widening) = preview.widens(planned) {
                return Err(RunError::preview_changed(widening));
            }
        }
        let now = preview.estimate.usd_micros;
        if let Some(shown) = call.expect_usd_micros
            && now > shown
        {
            return Err(RunError::preview_changed(Widening::estimate(shown, now)));
        }
        if now > call.max_usd_micros {
            return Err(RunError::cap_reached(call.max_usd_micros, now));
        }
        let target = resolved.target.as_ref().map_err(|e| RunError::Connector(e.clone()))?;
        let request = match call.request.seed() {
            Some(_) => call.request.clone(),
            None => call.request.clone().with_seed(Rng::new(call.run.0).next_u64()),
        };
        let key = self.key_for(target)?;
        let scrub = key.as_ref().map(|k| Scrubber::of(k.expose())).unwrap_or_default();

        env.progress.stage("send", 0.0);
        let output = self.call_connector(&resolved, target, &request, key.as_ref(), env, call.max_usd_micros);
        let output = output.map_err(|e| RunError::Connector(scrub_error(&scrub, &e)))?;

        env.progress.stage("store", 0.0);
        let rights: Vec<&Provenance> = request.input_assets().iter().filter_map(|h| call.inputs.get(h)).collect();
        let rights_in = RightsInputs {
            manifest: &entry.manifest,
            config: &resolved.config,
            inputs: &rights,
            voice_clone: matches!(&request, Request::Voice(v) if matches!(v.voice, omuzaki_connect_api::VoiceChoice::Clone { .. })),
            consent: consent.as_ref(),
        };
        let RunOutput { outputs, usage, mut stopped } = output;
        let mut stored = Vec::new();
        let mut answers = Vec::new();
        let mut store_failed = None;
        for out in outputs {
            match out {
                Output::Asset(artifact) => {
                    if !request.capability().allows_output(artifact.kind) {
                        stopped.get_or_insert_with(|| {
                            ConnectError::bad_response(format!(
                                "it returned {} for a {} request",
                                artifact.kind,
                                request.capability()
                            ))
                        });
                        continue;
                    }
                    let provenance = output_provenance(&rights_in, &request, &artifact);
                    match self.parts.store.put(&artifact.bytes, provenance.clone()) {
                        Ok(hash) => stored.push((hash, artifact.media_type, artifact.kind, provenance)),
                        Err(e) => {
                            stopped = Some(ConnectError::failed(format!("the asset store failed: {e}")));
                            store_failed = Some(e.to_string());
                            break;
                        }
                    }
                }
                Output::Text(text) => answers.push(Output::Text(scrub.text(&text))),
                Output::Json(json) => answers.push(Output::Json(scrub_json(&scrub, json))),
            }
        }
        if stored.is_empty() && answers.is_empty() {
            return Err(match (store_failed, stopped) {
                (Some(why), _) => RunError::Store(scrub.text(&why)),
                (None, Some(e)) => RunError::Connector(scrub_error(&scrub, &e)),
                (None, None) => RunError::Connector(ConnectError::bad_response("it returned nothing")),
            });
        }
        if usage.usd_micros > call.max_usd_micros {
            stopped = Some(ConnectError::failed(format!(
                "{} billed {} micro-dollars, above the call's cap of {}: a connector bug",
                entry.id(),
                usage.usd_micros,
                call.max_usd_micros
            )));
        }

        let candidates = self.candidates(call, entry, &request, stored, usage.usd_micros)?;
        env.progress.stage("store", 1.0);
        Ok(RunResult {
            connector: entry.id().clone(),
            candidates,
            answers,
            usage,
            stopped: stopped.map(|e| scrub_error(&scrub, &e)),
            inputs: call.inputs.clone(),
        })
    }

    /// A free reachability check of `id`: a zero cap, a meter that refuses every charge, nothing stored.
    ///
    /// # Errors
    /// What stops the connector from being reached.
    pub fn ping(&self, id: &ConnectorId, progress: &dyn Progress) -> Result<(), RunError> {
        let entry =
            self.parts.set.get(id).ok_or_else(|| RunError::Preview(omuzaki_connect_api::PreviewError::UnknownConnector(id.to_string())))?;
        let resolved = self.resolve(entry);
        if !resolved.config.enabled {
            return Err(RunError::Preview(omuzaki_connect_api::PreviewError::Disabled(id.clone())));
        }
        let target = resolved.target.as_ref().map_err(|e| RunError::Connector(e.clone()))?;
        let key = self.key_for(target)?;
        let scrub = key.as_ref().map(|k| Scrubber::of(k.expose())).unwrap_or_default();
        let http = AuthedHttp::new(&*self.parts.http, &entry.manifest, target, key.as_ref());
        let inputs = StoreInputs { store: &*self.parts.store, allowed: BTreeSet::new() };
        let env = RunEnv { progress, cancel: std::sync::Arc::new(Never), meter: &NoCharge };
        let cx = RunCx::new(target, &http, &inputs, &env, &*self.parts.pace, &*self.parts.clock, 0)
            .with_services(&self.parts.services);
        entry.connector.ping(&entry.manifest, &cx).map_err(|e| RunError::Connector(scrub_error(&scrub, &e)))
    }

    /// One pending candidate per stored output, `r<run>-<index>`, each with its share of the money.
    fn candidates(
        &self,
        call: &Call,
        entry: &crate::set::Entry,
        request: &Request,
        stored: Vec<Stored>,
        usd_micros: u64,
    ) -> Result<Vec<Candidate>, RunError> {
        let created_at = self.parts.clock.now();
        let shares = split(usd_micros, stored.len());
        let mut candidates = Vec::with_capacity(stored.len());
        for (index, ((asset, media_type, kind, provenance), usd_micros)) in stored.into_iter().zip(shares).enumerate() {
            let index = u32::try_from(index).unwrap_or(u32::MAX);
            let id = CandidateId::of_run(call.run, index)
                .map_err(|e| RunError::Connector(ConnectError::failed(format!("a candidate id: {e}"))))?;
            candidates.push(Candidate {
                id,
                run: call.run,
                connector: entry.id().clone(),
                connector_version: entry.manifest.version,
                request: request.clone(),
                asset,
                media_type,
                kind,
                provenance,
                usd_micros,
                requested_by: call.requested_by.clone(),
                created_at,
                state: CandidateState::Pending,
                regenerated_from: call.regenerated_from.clone(),
            });
        }
        Ok(candidates)
    }

    /// The key behind the target's label, if it has one.
    fn key_for(&self, target: &Target) -> Result<Option<SecretValue>, RunError> {
        target.secret.as_ref().map(|label| self.key(label)).transpose().map_err(RunError::Connector)
    }

    /// Builds the context and calls the adapter.
    fn call_connector(
        &self,
        r: &Resolved<'_>,
        target: &Target,
        request: &Request,
        key: Option<&SecretValue>,
        env: &RunEnv<'_>,
        cap: u64,
    ) -> Result<RunOutput, ConnectError> {
        let m = &r.entry.manifest;
        let http = AuthedHttp::new(&*self.parts.http, m, target, key);
        let inputs = StoreInputs { store: &*self.parts.store, allowed: request.input_assets().into_iter().collect() };
        let cx = RunCx::new(target, &http, &inputs, env, &*self.parts.pace, &*self.parts.clock, cap)
            .with_services(&self.parts.services);
        r.entry.connector.run(m, request, &cx)
    }
}

/// `total` split over `n` candidates: equal shares, the remainder on the first, so they sum to `total`.
fn split(total: u64, n: usize) -> Vec<u64> {
    let Some(count) = u64::try_from(n).ok().filter(|c| *c > 0) else {
        return Vec::new();
    };
    let share = total / count;
    let mut shares = vec![share; n];
    shares[0] += total - share * count;
    shares
}

/// `e` with every string scrubbed of the key: through its JSON form, so no variant's text is missed.
fn scrub_error(scrub: &Scrubber, e: &ConnectError) -> ConnectError {
    let Ok(json) = serde_json::to_value(e) else {
        return ConnectError::failed("the connector failed (its error could not be read)");
    };
    serde_json::from_value(scrub_json(scrub, json))
        .unwrap_or_else(|_| ConnectError::failed("the connector failed (its error could not be read)"))
}

/// `value` with every string scrubbed of the key.
fn scrub_json(scrub: &Scrubber, value: Value) -> Value {
    match value {
        Value::String(s) => Value::String(scrub.text(&s)),
        Value::Array(items) => Value::Array(items.into_iter().map(|v| scrub_json(scrub, v)).collect()),
        Value::Object(fields) => Value::Object(fields.into_iter().map(|(k, v)| (k, scrub_json(scrub, v))).collect()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shares_sum_to_the_total_with_the_remainder_first() {
        assert_eq!(split(160_000, 4), vec![40_000; 4]);
        assert_eq!(split(10, 3), vec![4, 3, 3]);
        assert_eq!(split(5, 0), Vec::<u64>::new());
    }

    #[test]
    fn errors_are_scrubbed_in_every_string() {
        let scrub = Scrubber::of("sk-test-0123456789");
        let e = ConnectError::unavailable("bad key sk-test-0123456789 at host");
        assert_eq!(scrub_error(&scrub, &e), ConnectError::unavailable("bad key [key] at host"));
    }
}
