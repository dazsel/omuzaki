//! The `connect.*` ops on the job path, driven as the executor drives them: the design's worked example from preview
//! to accepted candidate, a paid run with its charges, confirmation, caps, cancel, the tray's decisions,
//! regeneration, reuse, and a key scan over every output.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

mod common;

use std::collections::BTreeMap;

use common::{KEY, Rig, Seen, Signal, agent, drive, dry, person, png_bytes};
use omuzaki_connect::ops::describe::ConnectDescribe;
use omuzaki_connect::ops::generate::ConnectGenerate;
use omuzaki_connect::ops::key_status::ConnectKeyStatus;
use omuzaki_connect::ops::list::ConnectList;
use omuzaki_connect::ops::preview::ConnectPreview;
use omuzaki_connect::ops::regenerate::{ConnectRegenerate, SeedChoice};
use omuzaki_connect::ops::test::ConnectTest;
use omuzaki_connect::ops::tray::{ConnectAccept, ConnectReject, ConnectTrayList, TrayState};
use omuzaki_connect_api::{
    CandidateId, CandidateState, ConnectorConfig, ConnectorId, DeclaredModel, Request, RunId, Usable, tray,
};
use omuzaki_core::rng::Rng;
use omuzaki_doc_api::DocWrite;
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{ErrorCode, ErrorDetail, OpError};
use omuzaki_store_api::{Commercial, Source, Store};
use serde_json::{Value, json};

fn cid(s: &str) -> CandidateId {
    CandidateId::new(s).unwrap()
}

fn connector(s: &str) -> ConnectorId {
    ConnectorId::new(s).unwrap()
}

fn generate(request: &Value, connector: Option<&str>) -> ConnectGenerate {
    serde_json::from_value(json!({ "request": request, "connector": connector })).unwrap()
}

fn images(count: u8) -> Value {
    json!({ "capability": "image", "prompt": "chibi fox mascot, front view", "width": 64, "height": 64, "count": count })
}

fn paid_rig() -> Rig {
    let rig = Rig::new();
    rig.enable("mock-image-paid", Some("paid"));
    rig.set_key("mock-image-paid");
    rig
}

/// The design's worked example (section 4), with its numbers: an agent's dry-run, the job as run 42, two pending
/// records, and the person accepting the first.
#[test]
fn the_worked_example_goes_from_preview_to_an_accepted_candidate() {
    let rig = Rig::new();
    let declared = DeclaredModel { spdx: "LicenseRef-OpenRAIL-M".into(), commercial: Commercial::Ok };
    rig.configure(
        "comfyui",
        ConnectorConfig {
            enabled: true,
            models: BTreeMap::from([("sd_xl_base_1.0.safetensors".to_owned(), declared)]),
            ..ConnectorConfig::default()
        },
    );
    let op: ConnectGenerate = serde_json::from_value(json!({
        "request": { "capability": "image", "prompt": "chibi fox mascot, front view", "negative": null,
                     "width": 1024, "height": 1024, "count": 2, "seed": null, "references": [],
                     "model": "sd_xl_base_1.0.safetensors", "loras": [], "params": { "steps": 30 } }
    }))
    .unwrap();
    let asker = agent("session-7");
    let mut doc = MemDoc::new();

    // 1-2. The dry-run is the plan: no connector called, nothing spent.
    let plan = dry(&rig, &doc, &op, &asker).unwrap();
    assert!(plan.patch.is_empty());
    assert_eq!(plan.cost.as_ref().and_then(|c| c.usd), Some(0.0));
    let out = plan.output.unwrap();
    let expected = json!({
        "connector": "comfyui", "name": "ComfyUI", "capability": "image", "leaves_machine": false,
        "destinations": ["127.0.0.1"], "endpoint": "http://127.0.0.1:8188",
        "outbound": [{ "what": "prompt", "count": 1, "bytes": 28 }],
        "estimate": { "usd_micros": 0, "exact": true, "basis": "free", "seconds": null },
        "retention": "Stays on this computer.", "trains_on_inputs": "no",
        "commercial": "ok", "licence": "LicenseRef-OpenRAIL-M", "plan": null, "secret": null,
        "needs_spend": false, "needs_person": null, "warnings": [],
        "english": "This stays on this computer and costs nothing.",
    });
    for (key, want) in expected.as_object().unwrap() {
        assert_eq!(&out[key], want, "{key}");
    }
    assert_eq!(rig.runs("comfyui"), 0);

    // 3. The job, as run 42: two pending records in one step.
    let driven = drive(&rig, &mut doc, &op, &asker, 42, &Signal::new(0, None), None).unwrap();
    assert_eq!(driven.output["candidates"], json!(["r42-0", "r42-1"]));
    let seed = Rng::new(42).next_u64();
    let first = tray::get(&doc, &cid("r42-0")).unwrap().unwrap();
    assert_eq!((first.connector.as_str(), first.connector_version.to_string().as_str()), ("comfyui", "1.0.0"));
    assert_eq!((first.usd_micros, first.request.seed(), first.state.clone()), (0, Some(seed), CandidateState::Pending));
    assert_eq!(first.requested_by, asker);
    let stored = rig.store.provenance(&first.asset).unwrap();
    let Source::Generated { tool, model, plan, prompt, seed: s, inputs } = &stored.source else { panic!() };
    assert_eq!(
        (tool.as_str(), model.as_str(), plan, prompt.as_deref(), *s, inputs.len()),
        ("comfyui", "sd_xl_base_1.0.safetensors", &None, Some("chibi fox mascot, front view"), Some(seed), 0)
    );
    assert_eq!((stored.licence.as_str(), stored.commercial), ("LicenseRef-OpenRAIL-M", Commercial::Ok));
    assert_eq!(tray::usable(&doc, &first.asset).unwrap(), Usable::Pending(cid("r42-0")));

    // 4. The person accepts r42-0; r42-1 stays pending.
    rig.clock.set(omuzaki_core::time::Seconds(1801.0));
    drive(&rig, &mut doc, &ConnectAccept { candidate: cid("r42-0") }, &person(), 0, &Signal::new(0, None), None)
        .unwrap();
    let accepted = tray::get(&doc, &cid("r42-0")).unwrap().unwrap();
    assert_eq!(accepted.state, CandidateState::accepted(person(), omuzaki_core::time::Seconds(1801.0)));
    assert_eq!(tray::usable(&doc, &first.asset).unwrap(), Usable::Yes);
    let second = tray::get(&doc, &cid("r42-1")).unwrap().unwrap();
    assert!(second.state.is_pending());
    println!("worked example | {}", serde_json::to_string(&accepted).unwrap());
}

#[test]
fn a_paid_run_charges_as_it_goes_and_undoes_in_one_step() {
    let rig = paid_rig();
    let op = generate(&images(4), Some("mock-image-paid"));
    let mut doc = MemDoc::new();
    let plan = dry(&rig, &doc, &op, &person()).unwrap();
    assert_eq!(plan.cost.as_ref().and_then(|c| c.usd), Some(0.16));
    assert_eq!(plan.output.as_ref().unwrap()["estimate"]["usd_micros"], 160_000);
    let signal = Signal::new(160_000, None);
    let driven = drive(&rig, &mut doc, &op, &person(), 7, &signal, None).unwrap();
    assert_eq!(*signal.charges.lock().unwrap(), vec![40_000; 4]);
    assert_eq!(driven.output["usage"]["usd_micros"], 160_000);
    let ids: Vec<String> = tray::read_all(&doc).unwrap().iter().map(|c| c.id.to_string()).collect();
    assert_eq!(ids, ["r7-0", "r7-1", "r7-2", "r7-3"]);
    assert_eq!(driven.output["candidates"], json!(ids));
    let hashes: Vec<_> = tray::read_all(&doc).unwrap().iter().map(|c| c.asset).collect();
    doc.apply(&driven.finish.unwrap().patch.inverse()).unwrap();
    assert!(tray::read_all(&doc).unwrap().is_empty(), "one undo step removes all four");
    assert!(hashes.iter().all(|h| rig.store.contains(h)), "the bytes stay in the store");
    println!("paid run | estimate 160000 | charges {:?} | candidates {ids:?}", signal.charges.lock().unwrap());
}

#[test]
fn an_agent_sending_assets_off_the_computer_needs_a_person() {
    let rig = paid_rig();
    let asset = rig.import(&png_bytes(b"reference"), Commercial::Ok, false);
    let mut with_ref = images(1);
    with_ref["references"] = json!([asset]);
    let op = generate(&with_ref, Some("mock-image-paid"));
    let mut doc = MemDoc::new();
    let e = drive(&rig, &mut doc, &op, &agent("a"), 1, &Signal::new(40_000, None), None).err().unwrap();
    let Some(ErrorDetail::NeedsPerson { reason }) = e.detail.as_deref() else { panic!("{e:?}") };
    assert!(reason.contains("an image") && reason.contains("mock.invalid"), "{reason}");
    assert_eq!(rig.runs("mock-image-paid"), 0);
    // Confirmed by a person, the same run proceeds.
    let ok = drive(&rig, &mut doc, &op, &agent("a"), 2, &Signal::new(40_000, None), Some(&person())).unwrap();
    assert_eq!(ok.output["candidates"], json!(["r2-0"]));
    // A prompt only needs no one.
    drive(
        &rig,
        &mut doc,
        &generate(&images(1), Some("mock-image-paid")),
        &agent("a"),
        3,
        &Signal::new(40_000, None),
        None,
    )
    .unwrap();
}

#[test]
fn caps_and_expectations_refuse_before_anything_runs() {
    let rig = paid_rig();
    let mut doc = MemDoc::new();
    let mut op = generate(&images(4), Some("mock-image-paid"));
    op.expect_usd_micros = Some(100_000);
    let e = dry(&rig, &doc, &op, &person()).unwrap_err();
    assert_eq!(e.code, ErrorCode::Conflict, "{e:?}");
    op.expect_usd_micros = None;
    op.max_usd_micros = Some(100_000);
    let e = drive(&rig, &mut doc, &op, &person(), 1, &Signal::new(160_000, None), None).err().unwrap();
    assert_eq!(e.code, ErrorCode::Limited, "{e:?}");
    assert_eq!(rig.runs("mock-image-paid"), 0);
}

#[test]
fn a_cancelled_job_lands_and_charges_what_was_delivered() {
    let rig = paid_rig();
    let mut doc = MemDoc::new();
    let signal = Signal::new(160_000, Some(2));
    let driven = drive(&rig, &mut doc, &generate(&images(4), None), &person(), 9, &signal, None).unwrap();
    assert_eq!(driven.output["candidates"], json!(["r9-0", "r9-1"]));
    assert_eq!(driven.output["stopped"], "the call was cancelled");
    assert_eq!(*signal.charges.lock().unwrap(), vec![40_000, 40_000]);
}

#[test]
fn private_material_is_refused_for_a_hosted_connector_in_the_dry_run_and_the_run() {
    let rig = paid_rig();
    let private = rig.import(&png_bytes(b"private"), Commercial::Ok, true);
    let mut req = images(1);
    req["references"] = json!([private]);
    let op = generate(&req, Some("mock-image-paid"));
    let mut doc = MemDoc::new();
    assert_eq!(dry(&rig, &doc, &op, &person()).unwrap_err().code, ErrorCode::Denied);
    let e = drive(&rig, &mut doc, &op, &person(), 1, &Signal::new(40_000, None), None).err().unwrap();
    assert_eq!(e.code, ErrorCode::Denied);
}

#[test]
fn list_describe_key_status_and_test() {
    let rig = Rig::new();
    let mut doc = MemDoc::new();
    let s = Signal::new(0, None);
    let list = drive(&rig, &mut doc, &ConnectList {}, &agent("a"), 0, &s, None).unwrap().output;
    let ids: Vec<&str> = list.as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap()).collect();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    assert_eq!(ids, sorted);
    let paid = list.as_array().unwrap().iter().find(|r| r["id"] == "mock-image-paid").unwrap();
    assert_eq!(
        (&paid["paid"], &paid["leaves_machine"], &paid["status"]),
        (&json!(true), &json!(true), &json!({"status": "disabled"}))
    );

    let described =
        drive(&rig, &mut doc, &ConnectDescribe { connector: connector("comfyui") }, &agent("a"), 0, &s, None).unwrap();
    let manifest = rig.host.set().get(&connector("comfyui")).unwrap().manifest.clone();
    assert_eq!(described.output, serde_json::to_value(manifest).unwrap());
    let e = drive(&rig, &mut doc, &ConnectDescribe { connector: connector("nope") }, &agent("a"), 0, &s, None)
        .err()
        .unwrap();
    assert!(e.code == ErrorCode::NotFound && e.hint.as_deref().unwrap_or("").contains("mock-image-paid"), "{e:?}");

    let key_status = |id: &str, doc: &mut MemDoc| {
        drive(&rig, doc, &ConnectKeyStatus { connector: connector(id) }, &agent("a"), 0, &s, None).unwrap().output
    };
    assert_eq!(key_status("mock-image-paid", &mut doc), json!({"present": false, "needs_key": true}));
    rig.set_key("mock-image-paid");
    assert_eq!(key_status("mock-image-paid", &mut doc), json!({"present": true, "needs_key": true}));
    assert_eq!(key_status("mock-image", &mut doc), json!({"present": false, "needs_key": false}));

    rig.enable("mock-image", None);
    let ok =
        drive(&rig, &mut doc, &ConnectTest { connector: connector("mock-image") }, &person(), 1, &s, None).unwrap();
    assert_eq!(ok.output, json!({"ok": true}));
    rig.enable("test-echo", None);
    rig.set_key("test-echo");
    let test = ConnectTest { connector: connector("test-echo") };
    let out = drive(&rig, &mut doc, &test, &person(), 2, &s, None).unwrap().output;
    assert_eq!(out, json!({"ok": true}));
    // An unreachable service answers `ok: false` with the reason.
    rig.enable("mock-music", None);
    let off = ConnectTest { connector: connector("comfyui") };
    let out = drive(&rig, &mut doc, &off, &person(), 3, &s, None).unwrap().output;
    assert_eq!(out["ok"], false);
    assert!(out["error"].as_str().unwrap().contains("disabled"), "{out}");
}

#[test]
#[should_panic(expected = "the key leaked")]
fn the_key_scan_finds_every_form() {
    let seen = Seen::default();
    seen.add(format!("{:?}", "c2stdGVzdC0wMTIzNDU2Nzg5YWJjZGVm"));
    seen.assert_no_key();
}

/// Runs a four-image paid run as `actor` with run id `run`, returning the document.
fn four(rig: &Rig, actor: &omuzaki_ops_api::Actor, run: u64, doc: &mut MemDoc) {
    drive(rig, doc, &generate(&images(4), Some("mock-image-paid")), actor, run, &Signal::new(160_000, None), None)
        .unwrap();
}

#[test]
fn the_tray_decides_once_and_only_the_asker_or_a_person_decides() {
    let rig = paid_rig();
    let mut doc = MemDoc::new();
    four(&rig, &person(), 1, &mut doc);
    let s = Signal::new(0, None);
    let p = person();
    drive(&rig, &mut doc, &ConnectAccept { candidate: cid("r1-0") }, &p, 0, &s, None).unwrap();
    drive(&rig, &mut doc, &ConnectAccept { candidate: cid("r1-1") }, &p, 0, &s, None).unwrap();
    let before = tray::get(&doc, &cid("r1-2")).unwrap().unwrap();
    let reject = ConnectReject { candidate: cid("r1-2"), reason: Some("too dark".into()) };
    let rejected = drive(&rig, &mut doc, &reject, &p, 0, &s, None).unwrap();
    let list = |state| {
        let op = ConnectTrayList { state: Some(state), capability: None };
        let out = drive(&rig, &mut MemDoc::from_value(doc.root().clone()), &op, &p, 0, &s, None).unwrap().output;
        out.as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap().to_owned()).collect::<Vec<_>>()
    };
    assert_eq!(list(TrayState::Accepted), ["r1-0", "r1-1"]);
    assert_eq!(list(TrayState::Pending), ["r1-3"]);
    let usable: Vec<Usable> =
        tray::read_all(&doc).unwrap().iter().map(|c| tray::usable(&doc, &c.asset).unwrap()).collect();
    assert_eq!(usable, [Usable::Yes, Usable::Yes, Usable::Rejected(cid("r1-2")), Usable::Pending(cid("r1-3"))]);
    let after = tray::get(&doc, &cid("r1-2")).unwrap().unwrap();
    assert_eq!(
        omuzaki_connect_api::Candidate { state: CandidateState::Pending, ..after.clone() },
        before,
        "the audit record"
    );
    assert!(matches!(after.state, CandidateState::Rejected { reason: Some(ref r), .. } if r == "too dark"));
    doc.apply(&rejected.plan.patch.inverse()).unwrap();
    assert!(tray::get(&doc, &cid("r1-2")).unwrap().unwrap().state.is_pending(), "undo returns it to pending");
    let e = drive(&rig, &mut doc, &ConnectAccept { candidate: cid("r1-0") }, &p, 0, &s, None).err().unwrap();
    assert_eq!(e.code, ErrorCode::Conflict);

    // Agents decide only what they asked for.
    let mut doc = MemDoc::new();
    four(&rig, &agent("a"), 2, &mut doc);
    drive(&rig, &mut doc, &ConnectAccept { candidate: cid("r2-0") }, &agent("a"), 0, &s, None).unwrap();
    let e = drive(&rig, &mut doc, &ConnectAccept { candidate: cid("r2-1") }, &agent("b"), 0, &s, None).err().unwrap();
    assert_eq!(e.code, ErrorCode::Denied);
    drive(&rig, &mut doc, &ConnectAccept { candidate: cid("r2-1") }, &p, 0, &s, None).unwrap();
    let missing = drive(&rig, &mut doc, &ConnectAccept { candidate: cid("r9-9") }, &p, 0, &s, None).err().unwrap();
    assert_eq!(missing.code, ErrorCode::NotFound);
}

#[test]
fn a_pending_candidate_is_not_an_input_until_accepted() {
    let rig = Rig::new();
    rig.enable("mock-image", None);
    let mut doc = MemDoc::new();
    let s = Signal::new(0, None);
    drive(&rig, &mut doc, &generate(&images(1), None), &person(), 1, &s, None).unwrap();
    let made = tray::get(&doc, &cid("r1-0")).unwrap().unwrap().asset;
    let mut req = images(1);
    req["references"] = json!([made]);
    let e = drive(&rig, &mut doc, &generate(&req, None), &person(), 2, &s, None).err().unwrap();
    assert!(e.code == ErrorCode::Conflict && e.hint.as_deref() == Some("accept it in the tray first"), "{e:?}");
    drive(&rig, &mut doc, &ConnectAccept { candidate: cid("r1-0") }, &person(), 0, &s, None).unwrap();
    drive(&rig, &mut doc, &generate(&req, None), &person(), 3, &s, None).unwrap();
}

#[test]
fn regenerate_draws_a_new_seed_or_keeps_it_and_takes_changes() {
    let rig = Rig::new();
    rig.enable("mock-image", None);
    let mut doc = MemDoc::new();
    let s = Signal::new(0, None);
    let p = person();
    drive(&rig, &mut doc, &generate(&images(1), None), &p, 1, &s, None).unwrap();
    let original = tray::get(&doc, &cid("r1-0")).unwrap().unwrap();
    let regen = |seed, changes: Option<Value>| ConnectRegenerate {
        candidate: cid("r1-0"),
        seed,
        changes,
        expect_usd_micros: None,
        max_usd_micros: None,
    };
    drive(&rig, &mut doc, &regen(SeedChoice::New, None), &p, 2, &s, None).unwrap();
    let new = tray::get(&doc, &cid("r2-0")).unwrap().unwrap();
    assert_eq!(new.regenerated_from, Some(cid("r1-0")));
    assert!(new.request.seed() != original.request.seed() && new.asset != original.asset);
    drive(&rig, &mut doc, &regen(SeedChoice::Same, None), &p, 3, &s, None).unwrap();
    let same = tray::get(&doc, &cid("r3-0")).unwrap().unwrap();
    assert_eq!((same.asset, same.request.seed()), (original.asset, original.request.seed()));
    drive(&rig, &mut doc, &regen(SeedChoice::New, Some(json!({"prompt": "x"}))), &p, 4, &s, None).unwrap();
    assert_eq!(tray::get(&doc, &cid("r4-0")).unwrap().unwrap().request.prompt_text(), Some("x"));
    let e = drive(&rig, &mut doc, &regen(SeedChoice::New, Some(json!({"count": 99}))), &p, 5, &s, None).err().unwrap();
    assert!(e.code == ErrorCode::Invalid && e.field.as_deref() == Some("changes/count"), "{e:?}");
    drive(&rig, &mut doc, &ConnectReject { candidate: cid("r1-0"), reason: None }, &p, 0, &s, None).unwrap();
    drive(&rig, &mut doc, &regen(SeedChoice::New, None), &p, 6, &s, None).unwrap();
    assert!(tray::get(&doc, &cid("r6-0")).unwrap().is_some(), "a rejected candidate regenerates");
}

#[test]
fn a_replay_reuses_a_matching_run_and_sends_nothing() {
    let rig = paid_rig();
    let mut doc = MemDoc::new();
    four(&rig, &person(), 1, &mut doc);
    let runs = rig.runs("mock-image-paid");
    let first = tray::of_run(&doc, RunId(1)).unwrap();
    let mut replay = images(4);
    replay["seed"] = json!(first[0].request.seed());
    let mut op = generate(&replay, Some("mock-image-paid"));
    op.reuse = Some(RunId(1));
    let plan = dry(&rig, &doc, &op, &person()).unwrap();
    assert_eq!(plan.cost.and_then(|c| c.usd), Some(0.0));
    let signal = Signal::new(0, None);
    let out = drive(&rig, &mut doc, &op, &person(), 2, &signal, None).unwrap().output;
    assert_eq!(out["reused"], 1);
    assert_eq!(rig.runs("mock-image-paid"), runs, "nothing sent");
    let again = tray::of_run(&doc, RunId(2)).unwrap();
    assert_eq!(again.iter().map(|c| c.asset).collect::<Vec<_>>(), first.iter().map(|c| c.asset).collect::<Vec<_>>());
    assert!(again.iter().all(|c| c.usd_micros == 0) && signal.charges.lock().unwrap().is_empty());
}

#[test]
fn preview_op_matches_the_host_and_prices_in_dollars() {
    let rig = paid_rig();
    let doc = MemDoc::new();
    let req: Request = serde_json::from_value(images(2)).unwrap();
    let op = ConnectPreview { request: req.clone(), connector: None };
    let plan = dry(&rig, &doc, &op, &person()).unwrap();
    let host = rig.host.preview(&req, None, omuzaki_ai_api::Requester::Person, &BTreeMap::default()).unwrap();
    let mut out = plan.output.unwrap();
    assert_eq!(out.as_object_mut().unwrap().remove("english"), Some(json!(host.english())));
    assert_eq!(out, serde_json::to_value(&host).unwrap());
    let usd = plan.cost.unwrap().usd.unwrap();
    assert_eq!((usd, (f64::from(usd).to_string().parse::<f64>().unwrap() * 1e6).round()), (0.08, 80_000.0));
}

/// The key scan over a whole session: every op's output, every error, the document and the store's records, with a
/// key in the keychain for two connectors.
#[test]
fn the_key_appears_in_no_output_error_record_or_document() {
    let rig = paid_rig();
    rig.enable("test-echo", None);
    rig.set_key("test-echo");
    let seen = Seen::default();
    let mut doc = MemDoc::new();
    let s = Signal::new(1_000_000, None);
    let p = person();
    let record = |r: Result<common::Driven, OpError>| match r {
        Ok(d) => {
            seen.json(&d.output);
            seen.json(&d.plan);
            seen.debug(&d.plan);
        }
        Err(e) => {
            seen.json(&e);
            seen.add(e.to_string());
        }
    };
    record(drive(&rig, &mut doc, &ConnectList {}, &p, 0, &s, None));
    for id in ["mock-image-paid", "test-echo", "comfyui"] {
        record(drive(&rig, &mut doc, &ConnectDescribe { connector: connector(id) }, &p, 0, &s, None));
        record(drive(&rig, &mut doc, &ConnectKeyStatus { connector: connector(id) }, &p, 0, &s, None));
        record(drive(&rig, &mut doc, &ConnectTest { connector: connector(id) }, &p, 1, &s, None));
    }
    record(drive(&rig, &mut doc, &generate(&images(2), Some("mock-image-paid")), &p, 2, &s, None));
    record(drive(&rig, &mut doc, &generate(&images(1), Some("test-echo")), &p, 3, &s, None));
    record(drive(&rig, &mut doc, &generate(&images(9), Some("test-echo")), &p, 4, &s, None));
    record(drive(&rig, &mut doc, &ConnectTrayList::default(), &p, 0, &s, None));
    record(drive(&rig, &mut doc, &ConnectAccept { candidate: cid("r3-0") }, &p, 0, &s, None));
    let regen = ConnectRegenerate {
        candidate: cid("r3-0"),
        seed: SeedChoice::New,
        changes: None,
        expect_usd_micros: None,
        max_usd_micros: None,
    };
    record(drive(&rig, &mut doc, &regen, &p, 5, &s, None));
    assert!(tray::get(&doc, &cid("r3-0")).unwrap().is_some(), "the hosted call ran");
    assert!(tray::get(&doc, &cid("r5-0")).unwrap().is_some(), "the regeneration ran");
    seen.json(doc.root());
    for c in tray::read_all(&doc).unwrap() {
        seen.json(&rig.store.provenance(&c.asset).unwrap());
        seen.add(String::from_utf8_lossy(&rig.store.get(&c.asset).unwrap()).into_owned());
    }
    seen.debug(rig.host.set());
    let texts = seen.0.lock().unwrap().len();
    seen.assert_no_key();
    // The key did go out, in the one place it belongs: the echo service's request header.
    let headers: Vec<_> = rig.http.calls().into_iter().flat_map(|c| c.headers).collect();
    assert!(headers.contains(&("authorization".to_owned(), format!("Bearer {KEY}"))));
    println!("key scan | {texts} texts searched for the key, its percent-encoded and base64 forms | 0 found");
}
