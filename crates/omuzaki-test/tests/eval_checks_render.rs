//! The `render.*` checks on synthetic pictures: an identical pair, a pair differing by 3 levels in one region, and the
//! harness's trouble (no store, a missing render, an op that is not a query) kept apart from the agent's fail.
#![allow(clippy::unwrap_used)]

use std::io::Cursor;
use std::sync::Arc;

use image::{ImageFormat, Rgba, RgbaImage};
use omuzaki_doc_api::DocPath;
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Runner, Scope, Undo};
use omuzaki_ops_mock::MockRunner;
use omuzaki_store_api::{Commercial, ContentHash, Provenance, Source, Store};
use omuzaki_store_mock::MemStore;
use omuzaki_test::eval::check::{CheckResult, Checker, Outcome};
use omuzaki_test::eval::task::Task;
use omuzaki_test::eval::transcript::Transcript;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// A render op that answers with the address the document keeps under `shot`.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Shot {}
impl Op for Shot {
    fn meta() -> OpMeta {
        OpMeta {
            name: "evaltest.shot",
            summary: "Test op.",
            kind: OpKind::Query,
            scope: Scope::Render,
            undo: Undo::None,
        }
    }
    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let image = ctx.doc.get(&DocPath::parse("shot").unwrap()).cloned();
        Ok(Plan::answer(json!({ "image": image })))
    }
}
omuzaki_ops_api::register_op!(Shot);

/// A query that answers with the rect the document keeps under `rect`.
#[derive(Serialize, Deserialize, JsonSchema)]
struct RectOf {}
impl Op for RectOf {
    fn meta() -> OpMeta {
        OpMeta { name: "evaltest.rect", summary: "Test op.", kind: OpKind::Query, scope: Scope::Read, undo: Undo::None }
    }
    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::answer(json!({ "rect": ctx.doc.get(&DocPath::parse("rect").unwrap()).cloned() })))
    }
}
omuzaki_ops_api::register_op!(RectOf);

/// A job registered with the `render` scope: a check must not run it.
#[derive(Serialize, Deserialize, JsonSchema)]
struct ShotJob {}
impl Op for ShotJob {
    fn meta() -> OpMeta {
        OpMeta {
            name: "evaltest.shot_job",
            summary: "Test op.",
            kind: OpKind::Job,
            scope: Scope::Render,
            undo: Undo::None,
        }
    }
    fn plan(&self, _: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::answer(json!({ "image": null })))
    }
}
omuzaki_ops_api::register_op!(ShotJob);

const SIZE: u32 = 16;

fn grey(level: u8) -> RgbaImage {
    RgbaImage::from_pixel(SIZE, SIZE, Rgba([level, level, level, 255]))
}

/// `base` with the block at `x, y` of `side` pixels raised by `delta` levels.
fn with_block(base: &RgbaImage, x: u32, y: u32, side: u32, delta: u8) -> RgbaImage {
    let mut image = base.clone();
    for py in y..y + side {
        for px in x..x + side {
            let level = image.get_pixel(px, py).0[0] + delta;
            image.put_pixel(px, py, Rgba([level, level, level, 255]));
        }
    }
    image
}

fn provenance() -> Provenance {
    Provenance {
        source: Source::Authored,
        author: None,
        licence: "CC0-1.0".to_owned(),
        attribution: None,
        commercial: Commercial::Ok,
        consent: None,
        private_test: false,
    }
}

/// Stores `image` as a PNG and returns its render address.
fn store_png(store: &MemStore, image: &RgbaImage) -> String {
    let mut bytes = Cursor::new(Vec::new());
    image.write_to(&mut bytes, ImageFormat::Png).unwrap();
    let hash = store.put(bytes.get_ref(), provenance()).unwrap();
    format!("omuzaki://render/{hash}.png")
}

fn runner(doc: Value) -> Arc<dyn Runner> {
    Arc::new(MockRunner::new(MemDoc::from_value(doc)))
}

fn task(checks: &str) -> Task {
    let head = "id = 't'\nfeature = 'QG-03'\ntitle = 't'\nprompt = 'p'\nfixture = 'f'\nscopes = ['read']\n";
    toml::from_str(&format!("{head}{checks}")).unwrap()
}

/// Grades one check with a fixture picture and a result picture, with the rect `[4, 4, 4, 4]` in the result.
fn grade(before: &RgbaImage, after: &RgbaImage, body: &str) -> CheckResult {
    let store = Arc::new(MemStore::new());
    let (a, b) = (store_png(&store, before), store_png(&store, after));
    let checker = Checker::new(
        runner(json!({ "shot": a })),
        runner(json!({ "shot": b, "rect": [4, 4, 4, 4] })),
        Transcript::default(),
    )
    .with_store(store);
    checker.grade(&task(&format!("[[check]]\nid='c'\n{body}"))).results.remove(0)
}

const RENDER: &str = "render={ op='evaltest.shot' }\n";

#[test]
fn same_passes_on_identical_pictures_and_on_noise_within_two_levels() {
    let base = grey(100);
    assert_eq!(grade(&base, &base, &format!("kind='render.same'\n{RENDER}")).outcome, Outcome::Pass);
    let noisy = with_block(&base, 0, 0, SIZE, 2);
    assert_eq!(grade(&base, &noisy, &format!("kind='render.same'\n{RENDER}")).outcome, Outcome::Pass);
}

#[test]
fn same_fails_on_a_region_that_differs_by_three_levels() {
    let base = grey(100);
    let changed = with_block(&base, 4, 4, 4, 3);
    let result = grade(&base, &changed, &format!("kind='render.same'\n{RENDER}"));
    assert_eq!(result.outcome, Outcome::Fail);
    assert!(result.detail.contains("16 of 256"), "{}", result.detail);
}

#[test]
fn diff_passes_when_the_change_is_where_it_should_be() {
    let base = grey(100);
    let changed = with_block(&base, 4, 4, 4, 3);
    let body = format!(
        "kind='render.diff'\n{RENDER}region=[4.0, 4.0, 4.0, 4.0]\nexpect={{ changed_inside_at_least = 1.0, changed_fraction_at_least = 0.06, changed_fraction_at_most = 0.07 }}\n"
    );
    let result = grade(&base, &changed, &body);
    assert_eq!(result.outcome, Outcome::Pass, "{}", result.detail);
    assert_eq!(result.renders.len(), 2);
    assert_eq!(result.after.unwrap()["changed_inside"], json!(1.0));
}

#[test]
fn diff_fails_when_the_change_is_elsewhere_too_small_too_big_or_absent() {
    let base = grey(100);
    let elsewhere = with_block(&base, 10, 10, 4, 3);
    let inside = "kind='render.diff'\nrender={ op='evaltest.shot' }\nregion=[4.0, 4.0, 4.0, 4.0]\nexpect={ changed_inside_at_least = 0.9 }\n";
    assert_eq!(grade(&base, &elsewhere, inside).outcome, Outcome::Fail);
    // Half inside, half outside.
    let straddling = with_block(&base, 6, 6, 4, 3);
    assert_eq!(grade(&base, &straddling, inside).outcome, Outcome::Fail);
    let changed = with_block(&base, 4, 4, 4, 3);
    let too_small = "kind='render.diff'\nrender={ op='evaltest.shot' }\nregion=[4.0, 4.0, 4.0, 4.0]\nexpect={ changed_fraction_at_least = 0.1 }\n";
    assert_eq!(grade(&base, &changed, too_small).outcome, Outcome::Fail);
    let too_big = "kind='render.diff'\nrender={ op='evaltest.shot' }\nregion=[4.0, 4.0, 4.0, 4.0]\nexpect={ changed_fraction_at_most = 0.01 }\n";
    assert_eq!(grade(&base, &changed, too_big).outcome, Outcome::Fail);
    // Nothing changed, so none of it is inside.
    let nothing = grade(&base, &base, inside);
    assert_eq!(nothing.outcome, Outcome::Fail);
    assert!(nothing.detail.contains("nothing changed"), "{}", nothing.detail);
}

#[test]
fn diff_reads_its_region_from_an_op_on_the_result() {
    let base = grey(100);
    let changed = with_block(&base, 4, 4, 4, 3);
    let from_op = "kind='render.diff'\nrender={ op='evaltest.shot' }\nregion={ op='evaltest.rect', at='/rect' }\nexpect={ changed_inside_at_least = 1.0 }\n";
    assert_eq!(grade(&base, &changed, from_op).outcome, Outcome::Pass);
    // An op that answers something that is not a rect is the task's trouble.
    let not_a_rect = "kind='render.diff'\nrender={ op='evaltest.shot' }\nregion={ op='evaltest.rect', at='/rect/0' }\nexpect={ changed_inside_at_least = 1.0 }\n";
    assert_eq!(grade(&base, &changed, not_a_rect).outcome, Outcome::Error);
    // A region the agent's result no longer has is the agent's fail.
    let lost = "kind='render.diff'\nrender={ op='evaltest.shot' }\nregion={ op='evaltest.rect', at='/missing' }\nexpect={ changed_inside_at_least = 1.0 }\n";
    assert_eq!(grade(&base, &changed, lost).outcome, Outcome::Fail);
    // A rect outside the picture is a broken check.
    let outside = "kind='render.diff'\nrender={ op='evaltest.shot' }\nregion=[40.0, 40.0, 4.0, 4.0]\nexpect={ changed_inside_at_least = 1.0 }\n";
    assert_eq!(grade(&base, &changed, outside).outcome, Outcome::Error);
}

#[test]
fn renders_of_different_sizes_fail_the_result() {
    let base = grey(100);
    let small = RgbaImage::from_pixel(8, 8, Rgba([100, 100, 100, 255]));
    assert_eq!(grade(&base, &small, &format!("kind='render.same'\n{RENDER}")).outcome, Outcome::Fail);
}

#[test]
fn trouble_with_the_renders_is_the_harness_s_error() {
    let store = Arc::new(MemStore::new());
    let a = store_png(&store, &grey(100));
    let missing = format!("omuzaki://render/{}.png", ContentHash::of(b"never stored"));
    let fixture = || runner(json!({ "shot": a }));
    let grade_with = |result: Value, store: Option<Arc<MemStore>>, render: &str| {
        let mut checker = Checker::new(fixture(), runner(result), Transcript::default());
        if let Some(store) = store {
            checker = checker.with_store(store);
        }
        checker.grade(&task(&format!("[[check]]\nid='c'\nkind='render.same'\nrender={render}\n"))).results.remove(0)
    };
    let shot = "{ op='evaltest.shot' }";
    // No store, a render the store lacks, an address that is not one, an answer without an image.
    assert_eq!(grade_with(json!({ "shot": a }), None, shot).outcome, Outcome::Error);
    assert_eq!(grade_with(json!({ "shot": missing }), Some(store.clone()), shot).outcome, Outcome::Error);
    assert_eq!(grade_with(json!({ "shot": "file:///etc/passwd" }), Some(store.clone()), shot).outcome, Outcome::Error);
    assert_eq!(grade_with(json!({}), Some(store.clone()), shot).outcome, Outcome::Error);
    // Bytes that are not a PNG.
    let junk = store.put(b"not a png", provenance()).unwrap();
    let uri = format!("omuzaki://render/{junk}.png");
    assert_eq!(grade_with(json!({ "shot": uri }), Some(store.clone()), shot).outcome, Outcome::Error);
    // An op that is a job, or that is not registered, never runs.
    assert_eq!(
        grade_with(json!({ "shot": a }), Some(store.clone()), "{ op='evaltest.shot_job' }").outcome,
        Outcome::Error
    );
    assert_eq!(grade_with(json!({ "shot": a }), Some(store), "{ op='evaltest.nope' }").outcome, Outcome::Error);
}

#[test]
fn a_render_report_is_the_same_bytes_every_time() {
    let base = grey(100);
    let changed = with_block(&base, 4, 4, 4, 3);
    let body = format!(
        "kind='render.diff'\n{RENDER}region=[4.0, 4.0, 4.0, 4.0]\nexpect={{ changed_inside_at_least = 0.5 }}\n"
    );
    let json = |result: CheckResult| serde_json::to_string(&result).unwrap();
    assert_eq!(json(grade(&base, &changed, &body)), json(grade(&base, &changed, &body)));
}
