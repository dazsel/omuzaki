//! What recovery costs (journal.md 13): a 5,000-record tail over a 500-file project, from the crash to a core that
//! accepts edits. The design's budget is 1 s in a release build on a local SSD; the ceiling here is wide enough for a
//! debug build on a shared CI runner, and the measured number goes in the pull request.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::disallowed_methods, reason = "tests set up their fixtures on disk; the fence is for ops")]

use std::time::{Duration, Instant};

use omuzaki_app::{AppCore, AppOptions};
use omuzaki_doc::journal::JournalOptions;
use omuzaki_doc_api::{DocPath, Patch};
use omuzaki_history_api::JournalSink;
use omuzaki_ops_api::{Actor, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

const FILES: usize = 500;
const RECORDS: u64 = 5_000;

/// Sets one part's index.
#[derive(Serialize, Deserialize, JsonSchema)]
struct SetIndex {
    part: usize,
    index: u64,
}

impl Op for SetIndex {
    fn meta() -> OpMeta {
        OpMeta {
            name: "test.set_index",
            summary: "Set a part's index.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let at = part_index(self.part);
        Ok(Plan::edit(Patch::new().with(at.clone(), ctx.doc.get(&at).cloned(), Some(json!(self.index)))))
    }
}
register_op!(SetIndex);

fn part_index(part: usize) -> DocPath {
    DocPath::parse(&format!("characters/c{}/part{part}/index", part % 25)).unwrap()
}

/// No snapshot and no save during the session: the whole tail is the journal's.
fn options() -> AppOptions {
    AppOptions::headless(Actor::Person { name: "pat".into() }).with_journal(
        JournalOptions::default()
            .with_save_delays(Duration::from_secs(3600), Duration::from_secs(3600))
            .with_snapshot_cadence(Duration::from_secs(3600), u64::MAX),
    )
}

#[test]
fn a_5000_record_tail_over_a_500_file_project_recovers_quickly() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    AppCore::create(dir, "bench", options()).unwrap().close().unwrap();
    for i in 0..FILES - 1 {
        let folder = dir.join(format!("characters/c{}", i % 25));
        std::fs::create_dir_all(&folder).unwrap();
        let text = format!("kind = \"lock\"\nindex = {i}\ncurve = [0.1, 0.2, 0.3, 0.4]\ncolour = \"#a0b0c0\"\n");
        std::fs::write(folder.join(format!("part{i}.toml")), text).unwrap();
    }

    let core = AppCore::open(dir, options()).unwrap();
    // Each op is its own step (no transaction), so the tail is RECORDS op records.
    for n in 0..RECORDS {
        let part = usize::try_from(n % 400).unwrap();
        core.run("test.set_index", json!({ "part": part, "index": 10_000 + n })).unwrap();
    }
    core.journal().sync(Duration::from_secs(30)).unwrap();
    core.abandon();

    let started = Instant::now();
    let core = AppCore::open(dir, options()).unwrap();
    let took = started.elapsed();
    let report = core.recovery().clone();
    assert!(report.unclean, "{report:?}");
    assert!(report.replayed >= usize::try_from(RECORDS).unwrap(), "{report:?}");
    for part in [0, 199, 399] {
        let last = (0..RECORDS).rev().find(|n| usize::try_from(n % 400).unwrap() == part).unwrap();
        assert_eq!(core.get(&part_index(part)), Some(json!(10_000 + last)));
    }
    println!("recovery of a {} record tail ({RECORDS} ops) over {FILES} files: {took:?}", report.replayed);
    core.close().unwrap();
    assert!(took < Duration::from_secs(20), "{took:?}");
}
