//! `OpCtx::require_scope` as an op sees it: it uses the same grant check as the executor (`checks::denied`), passes
//! in a dry-run, and never reveals the grants.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::expect_used)]

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, ErrorCode, Grants, OpCtx, Scope};

fn agent() -> Actor {
    Actor::Agent { id: "claude-code#1".to_owned(), model: None }
}

#[test]
fn passes_when_granted() {
    let (doc, clock, actor) = (MemDoc::default(), FakeClock::new(Seconds(0.0)), agent());
    let grants = Grants::of([Scope::Read, Scope::EditCharacter]);
    let ctx = OpCtx::new(&doc, &actor, &clock).with_grants(&grants);
    assert!(ctx.require_scope(Scope::EditCharacter).is_ok());
}

#[test]
fn refuses_when_not_granted_naming_the_scope_and_not_the_grants() {
    let (doc, clock, actor) = (MemDoc::default(), FakeClock::new(Seconds(0.0)), agent());
    let grants = Grants::of([Scope::Read, Scope::EditScene]);
    let ctx = OpCtx::new(&doc, &actor, &clock).with_grants(&grants);
    let err = ctx.require_scope(Scope::EditLibrary).err();
    let err = err.expect("edit:library is not granted");
    assert_eq!(err.code, ErrorCode::Denied);
    assert!(err.message.contains("edit:library"), "{}", err.message);
    assert!(!err.message.contains("edit:scene"), "the held grants stay hidden: {}", err.message);
}

#[test]
fn a_context_with_no_grants_refuses_everything() {
    let (doc, clock, actor) = (MemDoc::default(), FakeClock::new(Seconds(0.0)), agent());
    let ctx = OpCtx::new(&doc, &actor, &clock);
    assert!(Scope::ALL.into_iter().all(|s| ctx.require_scope(s).is_err()));
}

#[test]
fn passes_in_a_dry_run_whatever_the_grants() {
    let (doc, clock, actor) = (MemDoc::default(), FakeClock::new(Seconds(0.0)), agent());
    let grants = Grants::of([Scope::Read]);
    let ctx = OpCtx::new(&doc, &actor, &clock).with_grants(&grants).with_dry_run(true);
    assert!(ctx.require_scope(Scope::Publish).is_ok());
}
