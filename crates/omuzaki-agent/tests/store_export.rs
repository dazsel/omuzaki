//! Export, resume, the rationale for Explain this and the `Conversations` view of the store (#1112).
//!
//! The golden is the section 4 conversation of `docs/design/agent-runtime.md` exported. `OMUZAKI_BLESS=1 cargo test
//! -p omuzaki-agent --test store_export` rewrites `tests/goldens/store/` after an intended change.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::assert_is_empty)]

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use omuzaki_agent::sink::EntrySink;
use omuzaki_agent::store::{ConversationStore, cut_rationale};
use omuzaki_agent_api::conversations::{ConversationId, Conversations, RationaleSource};
use omuzaki_agent_api::entry::{Entry, SessionNote, StoredBlock};
use omuzaki_agent_api::error::HostError;
use omuzaki_agent_api::message::ContextChip;
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::DocPath;
use omuzaki_llm_api::message::{ContentBlock, ImageMime};
use omuzaki_llm_api::usage::Usage;
use omuzaki_ops_api::{Actor, ApprovalId, EndReason, Need, SessionId, Spend, TxId};
use serde_json::json;

/// 2026-10-14T10:00:00Z.
const START: f64 = 1_791_972_000.0;
const PROJECT: &str = "0123456789abcdef0123456789abcdef";
const LABEL: &str = "Assistant: make her face more oval";

const TURN_1: &str = "Let me look at her face from the front.";
const TURN_4: &str = "You widened the head while I worked; I'll keep your width.";
const TURN_6: &str = "Here she is: the jaw is narrower and\u{2026}";

struct Fixture {
    dir: tempfile::TempDir,
    clock: Arc<FakeClock>,
    store: ConversationStore,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(FakeClock::new(Seconds(START)));
    let store = ConversationStore::open(dir.path(), PROJECT, clock.clone());
    Fixture { dir, clock, store }
}

/// Appends `entries` one second apart, so the log is the same bytes on every run.
fn append_ticking(f: &Fixture, id: ConversationId, entries: &[Entry]) {
    let sink = f.store.sink(id);
    for entry in entries {
        f.clock.advance(Seconds(1.0));
        sink.append(entry).unwrap();
    }
}

fn charge(input: u64, output: u64, read: u64, write: u64, usd_micros: u64) -> Entry {
    Entry::charge(Usage::new(input, output).with_cache_read(read).with_cache_write(write), usd_micros)
}

/// A model turn that only calls a tool, and the row the panel shows for the call.
fn call(id: &str, tool: &str, summary: &str, input: serde_json::Value) -> [Entry; 2] {
    [Entry::assistant(vec![ContentBlock::tool_use(id, tool, input)]), Entry::tool_call(id, tool, summary)]
}

/// The six turns of section 4: charges 814, 143, 103, 354, 126, 152 micro-dollars, tx 31 stopped in turn 6.
fn section_4(f: &Fixture) -> ConversationId {
    let id = f.store.create("claude-haiku-5-5").unwrap();
    let front = f.store.put_blob(id, b"\x89PNG front render 1", ImageMime::Png).unwrap();
    let again = f.store.put_blob(id, b"\x89PNG front render 2", ImageMime::Png).unwrap();
    let head = DocPath::parse("characters/main/parts/head").unwrap();
    let mut log = vec![
        Entry::session(SessionId(1), SessionNote::Opened),
        Entry::user("make her face more oval", vec![ContextChip::Selection(vec![head])]),
        Entry::approval(
            ApprovalId(7),
            Need::Budget { needed: Spend { usd_micros: 4_293, tokens: 0, seconds: 0 }, left: Spend::default() },
            None,
        ),
    ];
    // Turn 1: look at her face.
    log.push(Entry::assistant(vec![
        ContentBlock::text(TURN_1),
        ContentBlock::tool_use("c1", "render_preview", json!({"view": "front", "size": 512})),
    ]));
    log.push(Entry::tool_call("c1", "render_preview", "Preview: front"));
    log.push(Entry::tool_result("c1", true, vec![front]));
    log.push(charge(4, 61, 0, 6_262, 814));
    // Turn 2: the person widened the head meanwhile, so the edit conflicts.
    log.extend(call("c2", "face_set_profile", "Face profile: egg-oval", json!({"profile": "egg-oval", "amount": 0.5})));
    log.push(Entry::tool_result(
        "c2",
        false,
        vec![StoredBlock::Text(
            "Conflict: /characters/main/parts/head/params/width changed by the person (version 43)".into(),
        )],
    ));
    log.push(charge(4, 48, 6_262, 447, 143));
    // Turn 3: read the character.
    log.extend(call("c3", "read_resource", "Read the character", json!({"uri": "omuzaki://character/main"})));
    log.push(Entry::tool_result("c3", true, vec![StoredBlock::Text("character main, version 43".into())]));
    log.push(charge(4, 41, 6_709, 118, 103));
    // Turn 4: keep the person's width, apply again.
    log.push(Entry::assistant(vec![
        ContentBlock::text(TURN_4),
        ContentBlock::tool_use("c4", "face_set_profile", json!({"profile": "egg-oval", "amount": 0.5})),
    ]));
    log.push(Entry::tool_call("c4", "face_set_profile", "Face profile: egg-oval"));
    log.push(Entry::tool_result("c4", true, vec![StoredBlock::Text("applied (version 44)".into())]));
    log.push(charge(4, 96, 6_827, 1_891, 354));
    // Turn 5: look again.
    log.extend(call("c5", "render_preview", "Preview: front", json!({"view": "front", "size": 512})));
    log.push(Entry::tool_result("c5", true, vec![again]));
    log.push(charge(4, 40, 8_718, 141, 126));
    // Turn 6: the person presses Stop after three deltas.
    log.push(Entry::assistant(vec![ContentBlock::text(TURN_6)]));
    log.push(Entry::Stopped);
    log.push(charge(4, 18, 8_859, 426, 152));
    let rationale = cut_rationale(&[TURN_1, TURN_4, TURN_6].join("\n\n"));
    log.push(Entry::step(TxId(31), format!("{LABEL} (stopped)"), rationale));
    append_ticking(f, id, &log);
    id
}

/// Every file directly under `dir`, by name.
fn tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            (e.file_name().into_string().unwrap(), fs::read(e.path()).unwrap())
        })
        .collect()
}

#[test]
fn the_section_4_transcript_matches_its_golden_with_images_beside_it() {
    let f = fixture();
    let id = section_4(&f);
    let out = f.dir.path().join("out");
    f.store.export(id, &out).unwrap();

    let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/goldens/store");
    if std::env::var_os("OMUZAKI_BLESS").is_some() {
        let _ = fs::remove_dir_all(&golden);
        fs::create_dir_all(&golden).unwrap();
        for (name, bytes) in tree(&out) {
            fs::write(golden.join(name), bytes).unwrap();
        }
    }
    assert!(golden.is_dir(), "tests/goldens/store/ is missing (OMUZAKI_BLESS=1 writes it)");
    let (got, want) = (tree(&out), tree(&golden));
    assert_eq!(got.keys().collect::<Vec<_>>(), want.keys().collect::<Vec<_>>(), "the exported files");
    for (name, bytes) in &got {
        assert!(bytes == &want[name], "{name} differs from its golden (OMUZAKI_BLESS=1 rewrites it)");
    }
}

#[test]
fn the_folder_has_two_images_a_markdown_file_and_the_raw_log_byte_for_byte() {
    let f = fixture();
    let id = section_4(&f);
    let out = f.dir.path().join("deep").join("out");
    f.store.export(id, &out).unwrap();

    let files = tree(&out);
    assert!(files.contains_key("conversation-1.md"));
    assert_eq!(
        files
            .keys()
            .filter(|n| std::path::Path::new(n.as_str()).extension().is_some_and(|e| e.eq_ignore_ascii_case("png")))
            .count(),
        2
    );
    let log = fs::read(f.dir.path().join("assistant").join(PROJECT).join("1").join("log.jsonl")).unwrap();
    assert_eq!(files["log.jsonl"], log);
    let markdown = String::from_utf8(files["conversation-1.md"].clone()).unwrap();
    for name in files
        .keys()
        .filter(|n| std::path::Path::new(n.as_str()).extension().is_some_and(|e| e.eq_ignore_ascii_case("png")))
    {
        assert!(markdown.contains(&format!("]({name})")), "{name} is linked relatively");
    }
}

#[test]
fn writing_the_folder_twice_gives_the_same_files_and_an_unknown_id_fails() {
    let f = fixture();
    let id = section_4(&f);
    let (a, b) = (f.dir.path().join("a"), f.dir.path().join("b"));
    f.store.export(id, &a).unwrap();
    f.store.export(id, &b).unwrap();
    assert_eq!(tree(&a), tree(&b));
    assert!(matches!(f.store.export(ConversationId(9), &a), Err(HostError::Store(_))));
}

#[test]
fn rationale_of_tx_31_is_the_text_of_turns_1_4_and_6() {
    let f = fixture();
    let id = section_4(&f);
    let found = f.store.rationale(TxId(31)).unwrap();
    assert_eq!(found.conversation, id);
    assert_eq!(found.text, format!("{TURN_1}\n\n{TURN_4}\n\n{TURN_6}"));
    assert!(found.text.chars().count() <= 2_000);
    assert_eq!(f.store.rationale(TxId(32)), None);
}

#[test]
fn a_rationale_is_cut_to_2000_characters_and_the_newest_conversation_answers_first() {
    let f = fixture();
    let (old, new) = (f.store.create("m").unwrap(), f.store.create("m").unwrap());
    append_ticking(&f, new, &[Entry::step(TxId(5), "new", "x".repeat(2_500))]);
    append_ticking(&f, old, &[Entry::step(TxId(5), "old", "old text")]);
    // `old` was written last, so it is the newest by its last entry.
    assert_eq!(f.store.rationale(TxId(5)).unwrap().conversation, old);
    f.store.delete(old).unwrap();
    let found = f.store.rationale(TxId(5)).unwrap();
    assert_eq!((found.conversation, found.text.chars().count()), (new, 2_000));
}

#[test]
fn a_corrupt_conversation_does_not_hide_a_good_one() {
    let f = fixture();
    let (good, bad) = (f.store.create("m").unwrap(), f.store.create("m").unwrap());
    append_ticking(&f, good, &[Entry::step(TxId(1), "l", "why")]);
    append_ticking(&f, bad, &[Entry::Stopped, Entry::Stopped]);
    let path = f.dir.path().join("assistant").join(PROJECT).join(bad.0.to_string()).join("log.jsonl");
    let text = fs::read_to_string(&path).unwrap();
    fs::write(&path, text.replacen("{\"at\"", "{", 1)).unwrap();
    assert!(f.store.read(bad).is_err(), "the fixture really is corrupt");
    assert_eq!(f.store.rationale(TxId(1)).unwrap().conversation, good);
}

fn resumed_interrupted(tail: Vec<Entry>) -> bool {
    let f = fixture();
    let id = f.store.create("m").unwrap();
    let mut entries = vec![Entry::user("go", Vec::new()), Entry::assistant(vec![ContentBlock::text("on it")])];
    entries.extend(tail);
    append_ticking(&f, id, &entries);
    let resumed = f.store.resume(id).unwrap();
    assert_eq!(resumed.entries, entries);
    resumed.interrupted
}

#[test]
fn a_log_that_ends_mid_answer_resumes_interrupted() {
    let tool_use = || ContentBlock::tool_use("c", "t", json!({}));
    assert!(resumed_interrupted(vec![Entry::assistant(vec![tool_use()]), Entry::tool_call("c", "t", "s")]));
    assert!(resumed_interrupted(vec![
        Entry::assistant(vec![tool_use()]),
        Entry::tool_call("c", "t", "s"),
        Entry::tool_result("c", true, vec![StoredBlock::Text("ok".into())]),
    ]));
    // A charge after a tool-use turn is not the end of the answer.
    assert!(resumed_interrupted(vec![Entry::assistant(vec![tool_use()]), charge(1, 1, 0, 0, 1)]));
    assert!(resumed_interrupted(vec![Entry::user("again", Vec::new())]));
}

#[test]
fn a_log_that_ends_after_a_final_text_resumes_closed() {
    assert!(!resumed_interrupted(vec![charge(1, 1, 0, 0, 1)]));
    assert!(!resumed_interrupted(vec![charge(1, 1, 0, 0, 1), Entry::step(TxId(1), "l", "r")]));
    assert!(!resumed_interrupted(vec![Entry::Stopped]));
    assert!(!resumed_interrupted(vec![Entry::compacted("summary")]));
}

#[test]
fn a_finished_section_4_conversation_resumes_closed_and_a_new_one_is_not_interrupted() {
    let f = fixture();
    let id = section_4(&f);
    assert!(!f.store.resume(id).unwrap().interrupted);
    let empty = f.store.create("m").unwrap();
    assert!(!f.store.resume(empty).unwrap().interrupted);
}

#[test]
fn the_conversations_trait_lists_opens_deletes_and_writes_like_the_store() {
    let f = fixture();
    let view: &dyn Conversations = &f.store;
    let other = ConversationStore::open(f.dir.path(), "other", f.clock.clone());
    let kept = other.create("m").unwrap();
    other.sink(kept).append(&Entry::Stopped).unwrap();

    let quiet = f.store.create("m1").unwrap();
    append_ticking(&f, quiet, &[Entry::user("a".repeat(120), Vec::new())]);
    let revoked = f.store.create("m2").unwrap();
    let ended = SessionNote::Ended(EndReason::Revoked { by: Actor::Person { name: "sam".into() }, credential: true });
    append_ticking(
        &f,
        revoked,
        &[
            Entry::user("second\nline", Vec::new()),
            Entry::session(SessionId(1), SessionNote::Opened),
            Entry::session(SessionId(1), ended),
        ],
    );

    let rows = view.list().unwrap();
    assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), vec![revoked, quiet]);
    assert_eq!((rows[0].first_line.as_str(), rows[0].revoked), ("second", true));
    assert_eq!((rows[1].first_line.chars().count(), rows[1].revoked), (80, false));

    assert_eq!(view.open(revoked).unwrap(), f.store.resume(revoked).unwrap());

    let (a, b) = (f.dir.path().join("via-trait"), f.dir.path().join("via-store"));
    view.export(quiet, &a).unwrap();
    f.store.export(quiet, &b).unwrap();
    assert_eq!(tree(&a), tree(&b));

    view.delete(quiet).unwrap();
    assert_eq!(view.list().unwrap().len(), 1);
    assert!(matches!(view.open(quiet), Err(HostError::Store(_))));
    view.delete_all().unwrap();
    assert!(view.list().unwrap().is_empty());
    assert_eq!(other.read(kept).unwrap(), vec![Entry::Stopped]);
}
