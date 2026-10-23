//! History's pictures (#446, `shell.md` section 9, `design-system.md` section 9 rule 7): each row starts with the
//! character as it looked after the step. Only the rows on screen load theirs, at most two a frame, and a picture that
//! won't load is asked for once; over the real kit, an edit gets its picture from the stage's thumbnailer and its row
//! shows it.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::vec2;
use egui_kittest::Harness;
use omuzaki_core::time::Seconds;
use omuzaki_doc_api::{DocPath, DocVersion, Patch};
use omuzaki_ops_api::{Actor, Grants, Mode, OpRecord, Request, Runner, TxId};
use omuzaki_ui::ds::Settings;
use omuzaki_ui::ds::testing::harness_state;
use omuzaki_ui::shell::history::{History, HistoryLog};
use omuzaki_ui::shell::pictures::{HistoryPictures, StepPictures, UPLOADS_PER_FRAME};
use omuzaki_ui::shell::stub::NullRunner;
use omuzaki_ui_api::panel::{Panel as _, PanelCtx, ViewModels};
use serde_json::json;

/// Long enough for a debug build of the kit and its render on a slow runner; the test waits on the thumbnailer.
const WAIT: Duration = Duration::from_secs(120);

fn you() -> Actor {
    Actor::Person { name: "you".into() }
}

fn record(seq: u64) -> OpRecord {
    let path = DocPath::parse("characters/kit/name").unwrap_or_else(|e| panic!("{e}"));
    let change = Patch::new().with(path, Some(json!("Kit")), Some(json!(format!("Kit {seq}"))));
    let args = json!({"character": "kit", "name": format!("Kit {seq}")});
    OpRecord::new(
        seq,
        "character.rename",
        args,
        you(),
        TxId(seq),
        Seconds(f64::from(u32::try_from(seq).unwrap_or(u32::MAX))),
        change,
        DocVersion(seq),
    )
}

fn log(n: u64) -> HistoryLog {
    let records: Vec<OpRecord> = (1..=n).map(record).collect();
    let seen = records.iter().map(|r| (r.seq, Instant::now())).collect();
    HistoryLog { records, undone: HashSet::new(), seen, error: None, conflict: None }
}

/// A 16-pixel PNG of one colour.
fn png(rgb: [u8; 3]) -> Vec<u8> {
    let picture = image::RgbaImage::from_pixel(16, 16, image::Rgba([rgb[0], rgb[1], rgb[2], 255]));
    let mut out = std::io::Cursor::new(Vec::new());
    picture.write_to(&mut out, image::ImageFormat::Png).unwrap_or_else(|e| panic!("{e}"));
    out.into_inner()
}

/// Pictures for every step, or a broken one for every step, counting what History asks for.
struct Fake {
    broken: bool,
    asked: Mutex<HashMap<TxId, usize>>,
    calls: AtomicUsize,
}

impl Fake {
    fn new(broken: bool) -> Arc<Self> {
        Arc::new(Self { broken, asked: Mutex::new(HashMap::new()), calls: AtomicUsize::new(0) })
    }
}

impl StepPictures for Fake {
    fn png(&self, tx: TxId) -> Option<Result<Vec<u8>, String>> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        *self.asked.lock().unwrap_or_else(std::sync::PoisonError::into_inner).entry(tx).or_default() += 1;
        Some(if self.broken { Ok(b"not a png".to_vec()) } else { Ok(png([200, 120, 160])) })
    }
}

struct Rig {
    history: History,
    views: ViewModels,
}

/// History over `n` records; the pictures arrive after the first frames, as the app's do.
fn rig(n: u64, pictures: &Arc<Fake>) -> Harness<'static, Rig> {
    let mut views = ViewModels::new();
    views.insert(log(n));
    let mut h =
        harness_state(Settings::default(), vec2(360.0, 900.0), Rig { history: History::default(), views }, |ui, r| {
            let actor = you();
            let ctx = PanelCtx::new(&NullRunner, &actor).with_views(&r.views);
            r.history.ui(ui, &ctx);
        });
    h.state_mut().views.insert(HistoryPictures::new(pictures.clone()));
    h
}

#[test]
fn only_the_rows_on_screen_load_their_pictures_a_few_a_frame() {
    let pictures = Fake::new(false);
    let mut h = rig(50_000, &pictures);
    h.step();
    let first = h.state().history.pictures_kept();
    assert!(first > 0 && first <= UPLOADS_PER_FRAME, "the first frame uploads at most {UPLOADS_PER_FRAME}: {first}");
    h.run_steps(30);
    let kept = h.state().history.pictures_kept();
    // 900 pt of panel holds about a dozen 56 pt rows below the filters.
    assert!((8..=20).contains(&kept), "every row on screen has its picture, and no other: {kept}");
    let asked = pictures.asked.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_eq!(asked.len(), kept, "only rows on screen are asked for");
    assert!(asked.values().all(|&n| n == 1), "a loaded picture is never asked for again: {asked:?}");
    assert!(asked.keys().all(|tx| tx.0 > 50_000 - 20), "the newest rows, at the top: {:?}", asked.keys());
}

#[test]
fn a_picture_that_wont_load_is_asked_for_once_and_its_row_keeps_the_square() {
    let pictures = Fake::new(true);
    let mut h = rig(30, &pictures);
    h.run_steps(30);
    assert_eq!(h.state().history.pictures_kept(), 0);
    let calls = pictures.calls.load(Ordering::Relaxed);
    h.run_steps(10);
    assert_eq!(pictures.calls.load(Ordering::Relaxed), calls, "no row asks again");
    let asked = pictures.asked.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    assert!(asked.values().all(|&n| n == 1), "{asked:?}");
}

/// The studio's own wiring over a project folder: the kit, the stage's thumbnailer rendering with `renderer`, and
/// one edit by the person. Returns the runner, the pictures and the edit's transaction.
fn studio(
    dir: &std::path::Path,
    renderer: Box<dyn omuzaki_render_api::Renderer>,
) -> (Arc<dyn Runner>, HistoryPictures, Vec<TxId>) {
    omuzaki_ui::link();
    let document = omuzaki_ui::wiring::Document::project(dir).unwrap_or_else(|e| panic!("{e}"));
    let core =
        omuzaki_ui::wiring::core_on(document, omuzaki_ops_api::Services::default()).unwrap_or_else(|e| panic!("{e}"));
    let runner = core.runner;
    let pictures = omuzaki_ui::shell::pictures::taken(runner.clone(), renderer, dir).unwrap_or_else(|e| panic!("{e}"));
    omuzaki_ui::studio::open_character(runner.as_ref()).unwrap_or_else(|e| panic!("{e}"));
    let mut txs = Vec::new();
    for value in [0.85, 1.2] {
        let request = Request {
            op: "body.set_proportion".into(),
            args: json!({"character": "kit", "name": "leg_length", "value": value}),
            mode: Mode::Apply,
            actor: you(),
            grants: Grants::person(),
            cause: None,
            tx: None,
            base: None,
        };
        let outcome = runner.run(request).unwrap_or_else(|e| panic!("{e}"));
        let tx = outcome.applied.unwrap_or_else(|| panic!("the edit applied")).tx;
        // A burst shares one picture: wait for each edit's, so each row has its own.
        assert!(pictures.wait_for(tx, WAIT), "the thumbnailer took a picture of {tx:?}");
        txs.push(tx);
    }
    (runner, pictures, txs)
}

/// The op log of `runner`, as the shell keeps it.
fn read_log(runner: &dyn Runner) -> HistoryLog {
    let records = runner.log(0, 1000, &Grants::person()).unwrap_or_else(|e| panic!("{e}"));
    let seen = records.iter().map(|r| (r.seq, Instant::now())).collect();
    HistoryLog { records, undone: HashSet::new(), seen, error: None, conflict: None }
}

#[test]
fn an_edit_to_the_kit_gets_its_picture_and_its_row_shows_it() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let renderer = Box::new(omuzaki_render_mock::MockRenderer::new());
    let (runner, pictures, txs) = studio(dir.path(), renderer);
    let mut views = ViewModels::new();
    views.insert(read_log(runner.as_ref()));
    views.insert(pictures);
    let state = (History::default(), views, runner);
    let mut h = harness_state(Settings::default(), vec2(360.0, 600.0), state, |ui, s| {
        let actor = you();
        let ctx = PanelCtx::new(s.2.as_ref(), &actor).with_views(&s.1);
        s.0.ui(ui, &ctx);
    });
    h.run_steps(6);
    assert!(h.state().0.pictures_kept() >= txs.len(), "both edits' rows show their pictures");
    let index = dir.path().join(".omuzaki/history");
    assert!(index.is_dir(), "the pictures and their index live in the project's .omuzaki folder");
}

/// The rows with real renders, for the pull request: `OMUZAKI_EVIDENCE_OUT=<folder> cargo test -p omuzaki-ui --test
/// history_pictures -- --ignored`.
#[test]
#[ignore = "renders the kit on the GPU (lavapipe in CI) and writes evidence images"]
fn history_pictures_evidence() {
    use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};
    use omuzaki_ui::ds::{ThemeKind, apply_settings};
    let Ok(out) = std::env::var("OMUZAKI_EVIDENCE_OUT") else { panic!("set OMUZAKI_EVIDENCE_OUT to a folder") };
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let renderer = WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software })
        .unwrap_or_else(|e| panic!("{e}"));
    let (runner, pictures, _) = studio(dir.path(), Box::new(renderer));
    for (name, theme) in [("dark", ThemeKind::Dark), ("light", ThemeKind::Light)] {
        let mut views = ViewModels::new();
        views.insert(read_log(runner.as_ref()));
        views.insert(pictures.clone());
        let mut history = History::default();
        let runner = runner.clone();
        let mut h = Harness::builder().with_size(vec2(360.0, 420.0)).wgpu().build_ui(move |ui| {
            apply_settings(ui.ctx(), Settings { theme, ..Settings::default() });
            egui::CentralPanel::default().show(ui, |ui| {
                let actor = you();
                let ctx = PanelCtx::new(runner.as_ref(), &actor).with_views(&views);
                history.ui(ui, &ctx);
            });
        });
        h.run_steps(10);
        let image = h.render().unwrap_or_else(|e| panic!("{e}"));
        image.save(format!("{out}/history-pictures-{name}.png")).unwrap_or_else(|e| panic!("{e}"));
    }
}
