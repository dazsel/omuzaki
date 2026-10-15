//! The conversation store's write path, read path and retention (#1299), against a `FakeClock` and a temp folder.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::assert_is_empty)]

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use omuzaki_agent::sink::EntrySink;
use omuzaki_agent::store::ConversationStore;
use omuzaki_agent_api::conversations::ConversationId;
use omuzaki_agent_api::entry::{Entry, SessionNote, StoredBlock};
use omuzaki_agent_api::error::HostError;
use omuzaki_agent_api::event::QuestionId;
use omuzaki_agent_api::message::ContextChip;
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_llm_api::message::{ContentBlock, ImageMime};
use omuzaki_llm_api::usage::Usage;
use omuzaki_ops_api::{Actor, ApprovalId, EndReason, Need, Resolution, Resolved, SessionId, TxId};
use serde_json::json;

/// 2026-10-14T10:00:00Z.
const START: f64 = 1_791_972_000.0;
const DAY: f64 = 86_400.0;
const PROJECT: &str = "0123456789abcdef0123456789abcdef";

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

impl Fixture {
    fn project(&self, project: &str) -> ConversationStore {
        ConversationStore::open(self.dir.path(), project, self.clock.clone())
    }

    fn folder(&self, project: &str) -> PathBuf {
        self.dir.path().join("assistant").join(project)
    }

    fn log(&self, id: ConversationId) -> PathBuf {
        self.folder(PROJECT).join(id.0.to_string()).join("log.jsonl")
    }

    fn blob_files(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.folder(PROJECT).join("blobs"))
            .map(|rd| rd.map(|e| e.unwrap().file_name().into_string().unwrap()).collect())
            .unwrap_or_default();
        names.sort();
        names
    }

    fn advance(&self, secs: f64) {
        self.clock.advance(Seconds(secs));
    }
}

fn every_entry(blob: StoredBlock) -> Vec<Entry> {
    let resolution = Resolution {
        id: ApprovalId(7),
        by: Some(Actor::Person { name: "sam".into() }),
        result: Resolved::Rejected { comment: Some("no".into()) },
        at: Seconds(3.25),
    };
    vec![
        Entry::user(
            "make her face more oval",
            vec![ContextChip::Viewport, ContextChip::Resource("omuzaki://x".into())],
        ),
        Entry::assistant(vec![
            ContentBlock::text("Let me look."),
            ContentBlock::opaque("anthropic", json!({"type": "thinking", "signature": "abc", "n": 0.1})),
            ContentBlock::tool_use("c1", "render_preview", json!({"view": "front"})),
        ]),
        Entry::tool_call("c1", "render_preview", "Preview: front"),
        Entry::tool_result("c1", true, vec![StoredBlock::Text("ok".into()), blob]),
        Entry::approval(ApprovalId(7), Need::Person { reason: "model switch".into() }, Some(resolution)),
        Entry::approval(ApprovalId(8), Need::Person { reason: "again".into() }, None),
        Entry::question(QuestionId(1), "Which?", vec!["a".into(), "b".into()]),
        Entry::answer(QuestionId(1), "a"),
        Entry::charge(Usage::new(4, 61).with_cache_write(6_262), 814),
        Entry::step(TxId(31), "Assistant: make her face more oval", "the jaw reads round because"),
        Entry::session(SessionId(2), SessionNote::Opened),
        Entry::session(
            SessionId(2),
            SessionNote::Ended(EndReason::Revoked { by: Actor::Person { name: "sam".into() }, credential: true }),
        ),
        Entry::compacted("Summary: ..."),
        Entry::Stopped,
    ]
}

fn append_all(store: &ConversationStore, id: ConversationId, entries: &[Entry]) {
    let sink = store.sink(id);
    for entry in entries {
        sink.append(entry).unwrap();
    }
}

// Takes the call's result as it comes, so each assertion reads `is_store_error(store.call())`.
#[allow(clippy::needless_pass_by_value)]
fn is_store_error(result: Result<impl std::fmt::Debug, HostError>) -> bool {
    matches!(result, Err(HostError::Store(_)))
}

#[test]
fn every_entry_kind_round_trips_through_append_and_read() {
    let f = fixture();
    let id = f.store.create("claude-haiku-5-5").unwrap();
    let blob = f.store.put_blob(id, b"png bytes", ImageMime::Png).unwrap();
    let entries = every_entry(blob);
    append_all(&f.store, id, &entries);
    assert_eq!(f.store.read(id).unwrap(), entries);
}

#[test]
fn the_first_line_is_the_header_and_every_entry_carries_its_time() {
    let f = fixture();
    let id = f.store.create("claude-haiku-5-5").unwrap();
    f.advance(61.0);
    f.store.sink(id).append(&Entry::Stopped).unwrap();
    let text = fs::read_to_string(f.log(id)).unwrap();
    let mut lines = text.lines();
    assert_eq!(
        lines.next().unwrap(),
        format!(r#"{{"format":1,"project":"{PROJECT}","created":"2026-10-14T10:00:00Z","model":"claude-haiku-5-5"}}"#)
    );
    assert_eq!(lines.next().unwrap(), r#"{"at":"2026-10-14T10:01:01Z","kind":"stopped"}"#);
    assert_eq!(lines.next(), None);
}

#[test]
fn ids_count_up_from_one_and_the_layout_is_the_documented_one() {
    let f = fixture();
    assert_eq!(f.store.create("m").unwrap(), ConversationId(1));
    assert_eq!(f.store.create("m").unwrap(), ConversationId(2));
    f.store.delete(ConversationId(1)).unwrap();
    assert_eq!(f.store.create("m").unwrap(), ConversationId(3));
    assert!(f.folder(PROJECT).join("3").join("log.jsonl").is_file());
}

#[test]
fn a_torn_last_line_is_dropped_and_the_earlier_entries_are_intact() {
    let f = fixture();
    let id = f.store.create("m").unwrap();
    let entries = [Entry::user("hi", Vec::new()), Entry::Stopped];
    append_all(&f.store, id, &entries);
    let mut text = fs::read_to_string(f.log(id)).unwrap();
    text.push_str(r#"{"at":"2026-10-14T10:00:0"#);
    fs::write(f.log(id), text).unwrap();
    assert_eq!(f.store.read(id).unwrap(), entries);
}

#[test]
fn appending_after_a_torn_line_cuts_it_off_instead_of_gluing_to_it() {
    let f = fixture();
    let id = f.store.create("m").unwrap();
    f.store.sink(id).append(&Entry::user("hi", Vec::new())).unwrap();
    let mut text = fs::read_to_string(f.log(id)).unwrap();
    text.push_str(r#"{"at":"2026-10-14T10:00:0"#);
    fs::write(f.log(id), text).unwrap();
    f.store.sink(id).append(&Entry::Stopped).unwrap();
    assert_eq!(f.store.read(id).unwrap(), vec![Entry::user("hi", Vec::new()), Entry::Stopped]);
}

#[test]
fn a_whole_last_entry_that_only_lacks_its_newline_is_kept() {
    let f = fixture();
    let id = f.store.create("m").unwrap();
    f.store.sink(id).append(&Entry::user("hi", Vec::new())).unwrap();
    let text = fs::read_to_string(f.log(id)).unwrap();
    fs::write(f.log(id), text.trim_end_matches('\n')).unwrap();
    f.store.sink(id).append(&Entry::Stopped).unwrap();
    assert_eq!(f.store.read(id).unwrap(), vec![Entry::user("hi", Vec::new()), Entry::Stopped]);
}

#[test]
fn a_corrupt_line_before_the_last_names_its_number() {
    let f = fixture();
    let id = f.store.create("m").unwrap();
    append_all(&f.store, id, &[Entry::Stopped, Entry::Stopped]);
    let text = fs::read_to_string(f.log(id)).unwrap();
    let mut lines: Vec<&str> = text.lines().collect();
    lines[1] = "not json";
    fs::write(f.log(id), lines.join("\n") + "\n").unwrap();
    match f.store.read(id) {
        Err(HostError::Store(why)) => assert!(why.contains("line 2"), "{why}"),
        other => panic!("expected a store error, got {other:?}"),
    }
}

#[test]
fn an_unknown_conversation_is_a_store_error() {
    let f = fixture();
    assert!(is_store_error(f.store.read(ConversationId(9))));
    assert!(is_store_error(f.store.delete(ConversationId(9))));
    assert!(is_store_error(f.store.sink(ConversationId(9)).append(&Entry::Stopped).map(|()| 0)));
    assert!(is_store_error(f.store.put_blob(ConversationId(9), b"x", ImageMime::Png)));
}

#[test]
fn a_project_id_that_could_leave_the_root_is_refused() {
    let f = fixture();
    for bad in ["", "..", "../x", "a/b", "a\\b", "a.b", "a b"] {
        let store = f.project(bad);
        assert!(is_store_error(store.create("m")), "{bad:?}");
        assert!(is_store_error(store.list()), "{bad:?}");
        assert!(is_store_error(store.delete_all()), "{bad:?}");
    }
    assert!(!f.dir.path().join("x").exists());
}

#[test]
fn purge_keeps_exactly_90_days_and_deletes_one_second_more() {
    let f = fixture();
    let kept = f.store.create("m").unwrap();
    let gone = f.store.create("m").unwrap();
    let blob = f.store.put_blob(gone, b"only in the old one", ImageMime::Png).unwrap();
    f.store.sink(gone).append(&Entry::tool_result("c", true, vec![blob])).unwrap();
    f.store.sink(kept).append(&Entry::Stopped).unwrap();
    assert_eq!(f.blob_files().len(), 1);

    f.advance(90.0 * DAY);
    assert_eq!(f.store.purge(90).unwrap(), vec![]);
    assert_eq!(f.store.list().unwrap().len(), 2);

    f.advance(1.0);
    assert_eq!(f.store.purge(90).unwrap(), vec![kept, gone]);
    assert!(f.store.list().unwrap().is_empty());
    assert!(f.blob_files().is_empty());
}

#[test]
fn purge_ages_a_conversation_by_its_last_entry_not_its_creation() {
    let f = fixture();
    let id = f.store.create("m").unwrap();
    f.advance(89.0 * DAY);
    f.store.sink(id).append(&Entry::Stopped).unwrap();
    f.advance(2.0 * DAY);
    assert_eq!(f.store.purge(90).unwrap(), vec![]);
    f.advance(89.0 * DAY);
    assert_eq!(f.store.purge(90).unwrap(), vec![id]);
}

#[test]
fn a_blob_shared_by_two_conversations_survives_until_the_second_is_deleted() {
    let f = fixture();
    let (a, b) = (f.store.create("m").unwrap(), f.store.create("m").unwrap());
    let shared = f.store.put_blob(a, b"the same render", ImageMime::Png).unwrap();
    assert_eq!(shared, f.store.put_blob(b, b"the same render", ImageMime::Png).unwrap());
    let only_b = f.store.put_blob(b, b"another render", ImageMime::Jpeg).unwrap();
    f.store.sink(a).append(&Entry::tool_result("c", true, vec![shared.clone()])).unwrap();
    f.store.sink(b).append(&Entry::tool_result("c", true, vec![shared, only_b])).unwrap();
    assert_eq!(f.blob_files().len(), 2);

    f.store.delete(a).unwrap();
    assert_eq!(f.blob_files().len(), 2, "b still names both");
    f.store.delete(b).unwrap();
    assert!(f.blob_files().is_empty());
}

#[test]
fn a_blob_is_named_by_its_hash_and_encoding() {
    let f = fixture();
    let id = f.store.create("m").unwrap();
    let block = f.store.put_blob(id, b"bytes", ImageMime::Webp).unwrap();
    let hash = blake3::hash(b"bytes").to_hex().to_string();
    assert_eq!(block, StoredBlock::image(ImageMime::Webp, hash.clone()));
    assert_eq!(fs::read(f.folder(PROJECT).join("blobs").join(format!("{hash}.webp"))).unwrap(), b"bytes");
}

#[test]
fn delete_all_removes_only_this_projects_conversations() {
    let f = fixture();
    let other = f.project("another-project");
    let other_id = other.create("m").unwrap();
    other.sink(other_id).append(&Entry::Stopped).unwrap();
    let mine = f.store.create("m").unwrap();
    let blob = f.store.put_blob(mine, b"img", ImageMime::Png).unwrap();
    f.store.sink(mine).append(&Entry::tool_result("c", true, vec![blob])).unwrap();
    f.store.create("m").unwrap();

    f.store.delete_all().unwrap();
    assert!(f.store.list().unwrap().is_empty());
    assert!(f.blob_files().is_empty());
    assert_eq!(other.read(other_id).unwrap(), vec![Entry::Stopped]);
    f.store.delete_all().unwrap(); // nothing left is not an error
}

#[test]
fn list_is_newest_first_with_a_cut_first_line_and_the_revoked_flag() {
    let f = fixture();
    let old = f.store.create("m1").unwrap();
    f.store.sink(old).append(&Entry::user(format!("{}\nsecond line", "é".repeat(100)), Vec::new())).unwrap();
    f.advance(10.0);
    let new = f.store.create("m2").unwrap();
    let revoked = Entry::session(
        SessionId(1),
        SessionNote::Ended(EndReason::Revoked { by: Actor::Person { name: "sam".into() }, credential: false }),
    );
    append_all(
        &f.store,
        new,
        &[Entry::user("hello", Vec::new()), revoked, Entry::session(SessionId(2), SessionNote::Opened)],
    );
    f.advance(10.0);
    f.store.sink(old).append(&Entry::Stopped).unwrap();

    let rows = f.store.list().unwrap();
    assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), vec![old, new], "old was written last");
    assert_eq!(rows[0].first_line, "é".repeat(80));
    assert_eq!(
        (rows[0].created.as_str(), rows[0].last_entry.as_str()),
        ("2026-10-14T10:00:00Z", "2026-10-14T10:00:20Z")
    );
    assert_eq!(rows[0].model, "m1");
    assert!(!rows[0].revoked);
    assert_eq!(rows[1].first_line, "hello");
    assert!(!rows[1].revoked, "the last session note is Opened");
}

#[test]
fn appends_from_clones_on_many_threads_never_interleave() {
    let f = fixture();
    let id = f.store.create("m").unwrap();
    let threads: Vec<_> = (0..8)
        .map(|t| {
            let sink = f.store.clone().sink(id);
            std::thread::spawn(move || {
                for n in 0..25 {
                    sink.append(&Entry::user(format!("t{t} n{n}"), Vec::new())).unwrap();
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    assert_eq!(f.store.read(id).unwrap().len(), 200);
}

#[test]
fn store_errors_carry_no_paths() {
    let f = fixture();
    let root = f.dir.path().to_string_lossy().into_owned();
    let Err(HostError::Store(why)) = f.store.read(ConversationId(5)) else { panic!("expected an error") };
    assert!(!why.contains(&root), "{why}");
}
