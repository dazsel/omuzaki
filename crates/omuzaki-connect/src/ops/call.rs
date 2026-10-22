//! The job path `connect.generate` and `connect.regenerate` share (D4): `plan` is the preview (and the dry-run),
//! `work` runs exactly what the plan priced with the rights read when it starts, and `finish` joins the rights read
//! again into the candidates and lands them in one step.

use omuzaki_ai_api::Version;
use omuzaki_connect_api::{
    Call, Candidate, CandidateId, CandidateState, ConnectorId, Connectors, Preview, Request, RunEnv, RunError, RunId,
    RunResult, Usable, join_commercial, tray,
};
use omuzaki_doc_api::DocRead;
use omuzaki_ops_api::{CostHint, JobCtx, OpCtx, OpError, Plan};
use omuzaki_store_api::Store;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{JobCancel, JobMeter, JobProgress};

/// What a call asks for, besides its request.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Limits {
    /// The connector the caller named.
    pub(crate) connector: Option<ConnectorId>,
    /// What the person was shown.
    pub(crate) expect_usd_micros: Option<u64>,
    /// The most it may bill.
    pub(crate) max_usd_micros: Option<u64>,
    /// A run whose candidates to reuse, when they still match (TH-12).
    pub(crate) reuse: Option<RunId>,
    /// The candidate this regenerates.
    pub(crate) regenerated_from: Option<CandidateId>,
}

/// What `plan` hands `work` through the job's plan output, besides the preview's own fields.
#[derive(Debug, Deserialize)]
struct Planned {
    /// The effective request.
    request: Request,
    /// The run reused instead of calling the connector.
    #[serde(default)]
    reuse: Option<RunId>,
}

/// The preview as JSON with its English sentence, the request it was made for, and the run it reuses.
pub(crate) fn with_english(preview: &Preview, extra: Option<(&Request, Option<RunId>)>) -> Result<Value, OpError> {
    let mut out = serde_json::to_value(preview).map_err(|e| OpError::internal(format!("writing the preview: {e}")))?;
    if let Value::Object(fields) = &mut out {
        fields.insert("english".into(), preview.english().into());
        if let Some((request, reuse)) = extra {
            let request =
                serde_json::to_value(request).map_err(|e| OpError::internal(format!("writing the request: {e}")))?;
            fields.insert("request".into(), request);
            if let Some(run) = reuse {
                fields.insert("reuse".into(), run.0.into());
            }
        }
    }
    Ok(out)
}

/// Refuses an input asset a person has not accepted in the tray (D7).
fn inputs_usable(doc: &dyn DocRead, request: &Request) -> Result<(), OpError> {
    for hash in request.input_assets() {
        match tray::usable(doc, &hash).map_err(super::tray_error)? {
            Usable::Yes => {}
            Usable::Pending(id) => {
                return Err(OpError::conflict(format!("asset {hash} is candidate {id}, still pending"))
                    .hint("accept it in the tray first"));
            }
            Usable::Rejected(id) => {
                return Err(OpError::conflict(format!("asset {hash} is candidate {id}, which was rejected"))
                    .hint("regenerate it, or use another asset"));
            }
        }
    }
    Ok(())
}

/// `plan` of a call: the preview, an agent's confirmation when assets would leave, the cap, and reuse. An empty patch;
/// the output is the preview (with the request); the cost is the estimate, or nothing when a run is reused.
pub(crate) fn plan_call(ctx: &OpCtx<'_>, request: &Request, limits: &Limits) -> Result<Plan, OpError> {
    let host = super::connectors(ctx)?;
    let store = ctx.service::<dyn Store>()?;
    inputs_usable(ctx.doc, request)?;
    let rights = tray::input_rights(ctx.doc, store, request).map_err(super::tray_error)?;
    let preview = host
        .preview(request, limits.connector.as_ref(), super::requester(ctx.actor), &rights)
        .map_err(super::preview_error)?;
    let estimate = preview.estimate.usd_micros;
    if let Some(shown) = limits.expect_usd_micros
        && estimate > shown
    {
        return Err(super::run_error(RunError::preview_changed(omuzaki_connect_api::Widening::estimate(
            shown, estimate,
        ))));
    }
    if let Some(max) = limits.max_usd_micros
        && estimate > max
    {
        return Err(super::run_error(RunError::cap_reached(max, estimate)));
    }
    if let Some(sentence) = &preview.needs_person {
        ctx.require_person(sentence.clone())?;
    }
    let version = host.describe(&preview.connector).map(|m| m.version);
    let reuse = match (limits.reuse, &version) {
        (Some(run), Some(version)) => reused(ctx.doc, store, run, &preview.connector, version, request)?.map(|_| run),
        _ => None,
    };
    let cost = CostHint {
        usd: Some(if reuse.is_some() { 0.0 } else { super::dollars(estimate) }),
        seconds: preview.estimate.seconds,
        gpu_mib: None,
    };
    Ok(Plan { output: Some(with_english(&preview, Some((request, reuse)))?), cost: Some(cost), ..Plan::default() })
}

/// `work` of a call: the planned connector (never chosen again), the rights read from the live document now, the cap
/// (the call's, at most the job's reservation), then the run. Returns the [`RunResult`] as JSON, or the reused run.
pub(crate) fn work_call(job: &JobCtx<'_>, limits: &Limits) -> Result<Value, OpError> {
    let output = job.plan_output().ok_or_else(|| OpError::internal("the job has no planned preview"))?;
    let planned: Preview = serde_json::from_value(output.clone())
        .map_err(|e| OpError::internal(format!("reading the planned preview: {e}")))?;
    let Planned { request, reuse } = serde_json::from_value(output.clone())
        .map_err(|e| OpError::internal(format!("reading the planned request: {e}")))?;
    if let Some(run) = reuse {
        return Ok(json!({ "reuse": run.0, "run": job.id().0 }));
    }
    let host = job.service::<dyn Connectors>()?;
    let store = job.service::<dyn Store>()?;
    let inputs = job.read(|doc| tray::input_rights(doc, store, &request)).map_err(super::tray_error)?;
    let cap = limits.max_usd_micros.unwrap_or(planned.estimate.usd_micros).min(job.reserved_usd_micros());
    let call = Call {
        connector: Some(planned.connector.clone()),
        request,
        max_usd_micros: cap,
        expect_usd_micros: limits.expect_usd_micros,
        run: RunId(job.id().0),
        requested_by: job.actor().clone(),
        requester: super::requester(job.actor()),
        regenerated_from: limits.regenerated_from.clone(),
        inputs,
        planned: Some(planned),
    };
    let (progress, meter) = (JobProgress(job), JobMeter::new(job));
    let env = RunEnv { progress: &progress, cancel: JobCancel::of(job), meter: &meter };
    let result = host.run(&call, &env).map_err(super::run_error)?;
    serde_json::to_value(result).map_err(|e| OpError::internal(format!("writing the result: {e}")))
}

/// `finish` of a call, under the lock against the document as it is now: each candidate's rights joined with the
/// inputs' rights read again (strictest wins, so a stricter record that arrived during the call is not lost), then
/// every candidate landed as one patch, one undo step.
pub(crate) fn finish_call(ctx: &OpCtx<'_>, result: Value, limits: &Limits) -> Result<Plan, OpError> {
    if let (Some(old), Some(new)) =
        (result.get("reuse").and_then(Value::as_u64), result.get("run").and_then(Value::as_u64))
    {
        return finish_reuse(ctx, RunId(old), RunId(new), limits);
    }
    let mut r: RunResult =
        serde_json::from_value(result).map_err(|e| OpError::internal(format!("reading the run's result: {e}")))?;
    let store = ctx.service::<dyn Store>()?;
    if let Some(request) = r.candidates.first().map(|c| c.request.clone()) {
        // An asset gone from the store since the run keeps the rights the run used.
        let now = tray::input_rights(ctx.doc, store, &request).unwrap_or_default();
        for c in &mut r.candidates {
            for hash in request.input_assets() {
                for p in [now.get(&hash), r.inputs.get(&hash)].into_iter().flatten() {
                    c.provenance.commercial = join_commercial(c.provenance.commercial, p.commercial);
                    c.provenance.private_test |= p.private_test;
                }
            }
        }
    }
    let patch = tray::land(ctx.doc, &r.candidates).map_err(super::tray_error)?;
    let output = json!({
        "connector": r.connector,
        "candidates": r.candidates.iter().map(|c| c.id.to_string()).collect::<Vec<_>>(),
        "answers": r.answers,
        "usage": r.usage,
        "stopped": r.stopped.as_ref().map(ToString::to_string),
    });
    Ok(Plan { patch, output: Some(output), ..Plan::default() })
}

/// The run's records, when every one is still in the document, from the same connector and version, for the same
/// request (the seed compared only when the new request names one), and every hash is in the store (D10).
fn reused(
    doc: &dyn DocRead,
    store: &dyn Store,
    run: RunId,
    connector: &ConnectorId,
    version: &Version,
    request: &Request,
) -> Result<Option<Vec<Candidate>>, OpError> {
    let old = tray::of_run(doc, run).map_err(super::tray_error)?;
    let same = |c: &Candidate| {
        let compared = match (request.seed(), c.request.seed()) {
            (None, Some(seed)) => request.clone().with_seed(seed),
            _ => request.clone(),
        };
        c.connector == *connector
            && c.connector_version == *version
            && c.request == compared
            && store.contains(&c.asset)
    };
    Ok((!old.is_empty() && old.iter().all(same)).then_some(old))
}

/// `finish` of a reused run: new pending records for this job's run naming the same bytes; nothing was sent or billed.
fn finish_reuse(ctx: &OpCtx<'_>, run: RunId, job_run: RunId, limits: &Limits) -> Result<Plan, OpError> {
    let old = tray::of_run(ctx.doc, run).map_err(super::tray_error)?;
    if old.is_empty() {
        return Err(OpError::conflict(format!("run {run}'s candidates are gone")).hint("run the call again"));
    }
    let now = ctx.clock.now();
    let mut fresh = Vec::with_capacity(old.len());
    for (index, c) in old.into_iter().enumerate() {
        let index = u32::try_from(index).unwrap_or(u32::MAX);
        let id = CandidateId::of_run(job_run, index).map_err(|e| OpError::internal(format!("a candidate id: {e}")))?;
        fresh.push(Candidate {
            id,
            run: job_run,
            usd_micros: 0,
            requested_by: ctx.actor.clone(),
            created_at: now,
            state: CandidateState::Pending,
            regenerated_from: limits.regenerated_from.clone(),
            ..c
        });
    }
    let patch = tray::land(ctx.doc, &fresh).map_err(super::tray_error)?;
    let output = json!({
        "reused": run.0,
        "candidates": fresh.iter().map(|c| c.id.to_string()).collect::<Vec<_>>(),
        "answers": [],
        "usage": omuzaki_connect_api::Usage::default(),
        "stopped": null,
    });
    Ok(Plan { patch, output: Some(output), ..Plan::default() })
}
