//! Branches in the studio (#440, #442, #445; `docs/design/branches.md` 5, 6.4, 7.8, 8.3 and 9) over a real project's app
//! core and the kit, with a fake painter for the pictures: the chip's menu makes, switches, deletes and restores
//! branches as the person; an agent's ready work shows as a review card that can be sent back; compare shows branches
//! at one shared camera with what each changes; the merge view asks for a choice where both sides changed a part,
//! merges as one undo step, and leaves out what the person unticks.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use egui::accesskit::Role;
use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use omuzaki_char_api::{CharId, PartId, paths};
use omuzaki_doc_api::{DocPath, Value};
use omuzaki_history_api::branch::{BranchId, BranchInfo, BranchName, NewBranch, ReviewState};
use omuzaki_ops_api::{Actor, Grants, Mode, Request, Runner, Services};
use omuzaki_stage::ops::render::preview::Preview;
use omuzaki_ui::ds::Settings;
use omuzaki_ui::ds::testing::{all_strings, harness_state};
use omuzaki_ui::panels::edit::EditFocus;
use omuzaki_ui::shell::branches::merge::Phase;
use omuzaki_ui::shell::branches::pictures::Painter;
use omuzaki_ui::shell::branches::{BranchUi, Branching, StatePictures};
use omuzaki_ui::studio;
use omuzaki_ui::wiring::{Core, Document, core_on};
use serde_json::json;

fn you() -> Actor {
    Actor::Person { name: "you".into() }
}

/// What the painter was asked for: the camera, the size, and the head's width in the state drawn.
#[derive(Clone, Debug, PartialEq)]
struct Shot {
    yaw: Option<f32>,
    pitch: Option<f32>,
    size: [u32; 2],
    width: Option<f64>,
}

/// Paints a disc as wide as the head, and remembers every shot.
struct Discs(Arc<Mutex<Vec<Shot>>>);

impl Painter for Discs {
    fn paint(&self, tree: Arc<Value>, shot: &Preview) -> Result<Vec<u8>, String> {
        let width = omuzaki_doc_api::lookup(&tree, &width_path()).and_then(Value::as_f64);
        self.0.lock().unwrap().push(Shot { yaw: shot.yaw, pitch: shot.pitch, size: shot.size, width });
        #[allow(clippy::cast_possible_truncation)]
        let radius = (width.unwrap_or(0.2) * 300.0) as f32;
        let picture = image::RgbaImage::from_fn(shot.size[0], shot.size[1], |x, y| {
            #[allow(clippy::cast_precision_loss)]
            let (dx, dy) = (x as f32 - shot.size[0] as f32 / 2.0, y as f32 - shot.size[1] as f32 / 2.0);
            if (dx / radius).powi(2) + (dy / 90.0).powi(2) <= 1.0 {
                image::Rgba([240, 205, 190, 255])
            } else {
                image::Rgba([0, 0, 0, 0])
            }
        });
        let mut png = Vec::new();
        picture.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).map_err(|e| e.to_string())?;
        Ok(png)
    }
}

fn kit() -> CharId {
    CharId::new(studio::SAMPLE_ID).unwrap()
}

fn width_path() -> DocPath {
    paths::part_params(&kit(), &PartId::new("head").unwrap()).join("cranium").join("half_width")
}

/// The unit the legs' change is in: the one the merge offers besides the head.
fn legs_unit(h: &Harness<'_, Rig>) -> String {
    let preview = h.state().ui.merge().unwrap().previewed().unwrap().clone();
    let unit = preview.units.iter().find(|u| u.offers && u.key != HEAD).unwrap_or_else(|| panic!("{preview:#?}"));
    assert!(!unit.needs_choice, "only theirs changed the legs: {unit:#?}");
    unit.key.clone()
}

fn at(runner: &dyn Runner, key: &str) -> Option<Value> {
    runner.get(&DocPath::parse(key).unwrap(), &Grants::person()).unwrap()
}

const HEAD: &str = "characters/kit/parts/head";

fn run(runner: &dyn Runner, op: &str, args: Value) {
    let request = Request {
        op: op.into(),
        args,
        mode: Mode::Apply,
        actor: you(),
        grants: Grants::person(),
        cause: None,
        tx: None,
        base: None,
    };
    runner.run(request).unwrap_or_else(|e| panic!("{op}: {e}"));
}

fn set_width(runner: &dyn Runner, value: f64) {
    run(
        runner,
        "part.set_param",
        json!({"character": "kit", "part": "head", "path": "cranium/half_width", "value": value}),
    );
}

fn set_legs(runner: &dyn Runner, value: f64) {
    run(runner, "body.set_proportion", json!({"character": "kit", "name": "leg_length", "value": value}));
}

fn width(runner: &dyn Runner) -> Option<f64> {
    runner.get(&width_path(), &Grants::person()).unwrap().and_then(|v| v.as_f64())
}

struct Rig {
    _dir: tempfile::TempDir,
    core: Core,
    branching: Branching,
    pictures: StatePictures,
    shots: Arc<Mutex<Vec<Shot>>>,
    ui: BranchUi,
    focus: EditFocus,
    switched: bool,
}

impl Rig {
    fn branches(&self) -> Vec<BranchInfo> {
        self.branching.branches.list()
    }

    fn named(&self, name: &str) -> BranchInfo {
        self.branches().into_iter().find(|b| b.name.as_str() == name).unwrap_or_else(|| panic!("no branch {name}"))
    }

    fn runner(&self) -> &dyn Runner {
        self.core.runner.as_ref()
    }

    /// The branch's own runner, as an agent working on it has.
    fn lane(&self, id: &BranchId) -> Arc<dyn Runner> {
        self.branching.branches.lane(id).unwrap().runner
    }

    /// Makes `name` from the folder branch's current state, as the person.
    fn branch(&self, name: &str) -> BranchInfo {
        let new = NewBranch::new(BranchName::new(name).unwrap(), self.branching.branches.folder());
        self.branching.branches.create(new, &you()).unwrap()
    }
}

/// A project with the kit open, the branch windows over its app core, and a painter that draws discs.
fn rig() -> Harness<'static, Rig> {
    omuzaki_ui::link();
    let dir = tempfile::tempdir().unwrap();
    let core = core_on(Document::project(dir.path()).unwrap(), Services::default()).unwrap();
    let character = studio::open_character(core.runner.as_ref()).unwrap();
    let shots = Arc::new(Mutex::new(Vec::new()));
    let pictures = StatePictures::start(Box::new(Discs(Arc::clone(&shots)))).unwrap();
    let branching = core.branching(Some(pictures.clone())).expect("a project has branches");
    let focus = EditFocus::new(character);
    let rig = Rig { _dir: dir, core, branching, pictures, shots, ui: BranchUi::default(), focus, switched: false };
    let mut h = harness_state(Settings::default(), vec2(1280.0, 900.0), rig, |ui, r: &mut Rig| {
        r.ui.menu.chip(ui, &r.branching);
        let runner = Arc::clone(&r.core.runner);
        r.switched |= r.ui.show(ui.ctx(), &r.branching, runner.as_ref(), Some(&r.focus));
    });
    h.run_steps(2);
    h
}

fn says(h: &Harness<'_, Rig>, words: &str) -> bool {
    all_strings(h).iter().any(|s| s.contains(words))
}

/// The first button labelled `label`, or, failing that, whose label holds it (the chip's carries an icon).
fn button<'h>(h: &'h Harness<'_, Rig>, label: &'h str) -> egui_kittest::Node<'h> {
    let exact = h.query_all_by_label(label).find(|n| n.accesskit_node().role() == Role::Button);
    exact
        .or_else(|| h.query_all_by_label_contains(label).find(|n| n.accesskit_node().role() == Role::Button))
        .unwrap_or_else(|| panic!("a button {label:?}: {:#?}", all_strings(h)))
}

fn press(h: &mut Harness<'_, Rig>, label: &str) {
    button(h, label).click();
    h.run_steps(3);
}

fn enabled(h: &Harness<'_, Rig>, label: &str) -> bool {
    !button(h, label).accesskit_node().is_disabled()
}

fn type_into(h: &mut Harness<'_, Rig>, label: &str, text: &str) {
    h.get_by_label(label).click();
    h.run_steps(1);
    h.get_by_label(label).type_text(text);
    h.run_steps(2);
}

#[test]
fn the_chip_names_the_branch_and_its_menu_makes_switches_deletes_and_restores_branches() {
    let mut h = rig();
    assert!(says(&h, "Branch: main"), "{:#?}", all_strings(&h));
    press(&mut h, "Branch: main");
    assert!(h.state().ui.menu.is_open());
    assert!(says(&h, "You're working on main"));

    // A taken name is refused in words, with what to do about it; a fresh one is made from the folder branch.
    press(&mut h, "New branch");
    type_into(&mut h, "Name", "main");
    press(&mut h, "Make branch");
    assert!(says(&h, "the name main is taken. pick another name"), "{:#?}", all_strings(&h));
    h.get_by_label("Name").click();
    h.run_steps(1);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.get_by_label("Name").type_text("oval");
    h.run_steps(2);
    type_into(&mut h, "Title, optional", "A rounder face");
    press(&mut h, "Make branch");
    let oval = h.state().named("oval");
    assert_eq!(oval.title.as_deref(), Some("A rounder face"));
    assert!(says(&h, "A rounder face"), "the new branch is listed under Yours");

    // Switch moves the folder and asks the shell to read it again; the chip follows.
    press(&mut h, "Switch");
    assert!(h.state().switched, "the shell is asked to follow the folder");
    assert_eq!(h.state().branching.branches.folder(), oval.id);
    assert!(!h.state().ui.menu.is_open(), "the menu closes on a switch");
    h.run_steps(2);
    assert!(says(&h, "Branch: oval"), "{:#?}", all_strings(&h));
    set_width(h.state().runner(), 0.31);
    assert_eq!(width(h.state().lane(&oval.id).as_ref()), Some(0.31), "the person's edits land on the folder branch");

    // A branch deleted goes to the bin, and comes back from it.
    let spare = h.state().branch("spare");
    press(&mut h, "Branch: oval");
    assert!(says(&h, "spare"));
    let deletes: Vec<_> =
        h.query_all_by_label("Delete").filter(|n| n.accesskit_node().role() == Role::Button).collect();
    assert_eq!(deletes.len(), 2, "main and spare can be deleted, the folder branch can't");
    drop(deletes);
    let names: Vec<String> = h.state().branches().iter().map(|b| b.name.to_string()).collect();
    // Rows are listed by name: main, then spare.
    assert!(names.contains(&"spare".to_owned()));
    h.query_all_by_label("Delete").filter(|n| n.accesskit_node().role() == Role::Button).nth(1).unwrap().click();
    h.run_steps(3);
    assert!(h.state().named("spare").deleted.is_some(), "spare is in the bin");
    assert!(h.state().named("main").deleted.is_none(), "main is untouched");
    press(&mut h, "Restore");
    assert!(h.state().named("spare").deleted.is_none(), "spare is back");
    assert_eq!(h.state().named("spare").id, spare.id);
}

#[test]
fn an_agents_ready_work_is_a_review_card_that_can_be_sent_back_with_a_comment() {
    let mut h = rig();
    let wide = h.state().branch("wide");
    set_width(h.state().lane(&wide.id).as_ref(), 0.3);
    let point = h
        .state()
        .branching
        .branches
        .checkpoint(&wide.id, "wider head", Some("Wider, as asked".into()), &you())
        .unwrap();
    h.run_steps(2);
    assert!(says(&h, "1 ready"), "the chip counts the work waiting: {:#?}", all_strings(&h));
    h.state_mut().ui.menu.toggle();
    h.run_steps(3);
    assert!(says(&h, "you: wide is ready"), "{:#?}", all_strings(&h));
    assert!(says(&h, "Wider, as asked"));

    // Merge on the card opens the merge view at the reviewed point.
    press(&mut h, "Merge into main");
    assert!(h.state().ui.merge().is_some(), "the merge view opens");
    h.state_mut().ui = BranchUi::default();
    h.state_mut().ui.menu.toggle();
    h.run_steps(3);

    press(&mut h, "Send back");
    assert!(!enabled(&h, "Send back"), "a send-back needs a comment");
    type_into(&mut h, "What to change", "Less wide, please");
    press(&mut h, "Send back");
    let review = h.state().named("wide").review.expect("the review stays on the branch");
    assert_eq!(review.state, ReviewState::SentBack);
    assert_eq!(review.point, point);
    assert!(!says(&h, "you: wide is ready"), "the card is gone");
}

#[test]
fn a_part_both_sides_changed_needs_a_choice_merges_as_one_step_and_undoes() {
    let mut h = rig();
    let wide = h.state().branch("wide");
    let lane = h.state().lane(&wide.id);
    set_width(lane.as_ref(), 0.3);
    set_legs(lane.as_ref(), 1.15);
    h.state().branching.branches.checkpoint(&wide.id, "work", None, &you()).unwrap();
    set_width(h.state().runner(), 0.25);

    let branching = h.state().branching.clone();
    let runner = Arc::clone(&h.state().core.runner);
    h.state_mut().ui.open_merge(&wide.id, None, &branching, runner.as_ref());
    h.run_steps(3);
    let view = h.state().ui.merge().unwrap();
    let preview = view.previewed().unwrap();
    let head = preview.units.iter().find(|u| u.key == HEAD).expect("the head is a unit");
    assert!(head.needs_choice, "both sides changed the head's width: {head:#?}");
    let change = head.changes.iter().find(|c| c.path.ends_with("half_width")).unwrap();
    assert_eq!((change.mine.clone(), change.theirs.clone()), (Some(json!(0.25)), Some(json!(0.3))));
    let legs = legs_unit(&h);
    let legs_before = at(h.state().runner(), &legs);
    assert!(says(&h, "Needs a choice"));
    assert!(!enabled(&h, "Merge"), "Merge waits for the choice");

    // Both sides are drawn at one camera.
    h.state().pictures.settle();
    let widths: Vec<Option<f64>> = h.state().shots.lock().unwrap().iter().map(|s| s.width).collect();
    assert!(widths.contains(&Some(0.25)) && widths.contains(&Some(0.3)), "mine and theirs are drawn: {widths:?}");

    // The result is drawn as the choices change: mine until the head is chosen, theirs once taken.
    let result_width = |h: &Harness<'_, Rig>| {
        let tree = h.state().ui.merge().unwrap().result().cloned().expect("a result").unwrap();
        omuzaki_doc_api::lookup(&tree, &width_path()).and_then(Value::as_f64)
    };
    assert_eq!(result_width(&h), Some(0.25));
    press(&mut h, "Take theirs");
    assert!(enabled(&h, "Merge"));
    assert_eq!(result_width(&h), Some(0.3), "the result follows the choice");
    h.state().pictures.settle();
    let drawn = h.state().shots.lock().unwrap().len();
    press(&mut h, "Show the base");
    h.state().pictures.settle();
    let base_width = h.state().shots.lock().unwrap()[drawn..].iter().map(|s| s.width).collect::<Vec<_>>();
    assert!(base_width.iter().any(|w| w.is_some_and(|w| w != 0.25 && w != 0.3)), "the base is drawn: {base_width:?}");
    press(&mut h, "Merge");
    assert_eq!(h.state().ui.merge().unwrap().phase(), &Phase::Merged);
    assert_eq!(width(h.state().runner()), Some(0.3), "theirs was taken");
    assert_ne!(at(h.state().runner(), &legs), legs_before, "the clean unit came too");
    assert!(says(&h, "Merged wide into main"), "{:#?}", all_strings(&h));

    // One undo takes the whole merge back.
    press(&mut h, "Undo the merge");
    assert_eq!(h.state().ui.merge().unwrap().phase(), &Phase::Undone);
    assert_eq!(width(h.state().runner()), Some(0.25));
    assert_eq!(at(h.state().runner(), &legs), legs_before);
}

#[test]
fn a_clean_unit_unticked_is_left_out_and_a_stale_preview_is_refused() {
    let mut h = rig();
    let wide = h.state().branch("wide");
    let lane = h.state().lane(&wide.id);
    set_width(lane.as_ref(), 0.3);
    set_legs(lane.as_ref(), 1.15);
    h.state().branching.branches.checkpoint(&wide.id, "work", None, &you()).unwrap();

    let branching = h.state().branching.clone();
    let runner = Arc::clone(&h.state().core.runner);
    h.state_mut().ui.open_merge(&wide.id, None, &branching, runner.as_ref());
    h.run_steps(3);
    assert!(h.state().ui.merge().unwrap().ready(), "nothing conflicts");
    let legs = legs_unit(&h);
    let legs_before = at(h.state().runner(), &legs);

    // The folder branch moves on under the preview: the merge is refused as stale, and previewing again works.
    set_width(h.state().runner(), 0.22);
    press(&mut h, "Merge");
    assert!(matches!(h.state().ui.merge().unwrap().phase(), Phase::Stale(_)), "{:?}", h.state().ui.merge());
    press(&mut h, "Preview again");
    assert_eq!(h.state().ui.merge().unwrap().phase(), &Phase::Choosing);

    // The head now conflicts; keep mine and leave the legs out.
    press(&mut h, "Keep mine");
    let merge = h.state_mut().ui.merge_mut().unwrap();
    merge.leave_out(&legs, true);
    assert_eq!(merge.args()["exclude"], json!([legs]));
    h.run_steps(2);
    press(&mut h, "Merge selected");
    assert_eq!(h.state().ui.merge().unwrap().phase(), &Phase::Merged);
    assert_eq!(width(h.state().runner()), Some(0.22), "mine kept");
    assert_eq!(at(h.state().runner(), &legs), legs_before, "the legs were left out");
}

#[test]
fn compare_draws_every_branch_at_one_camera_and_lists_what_each_changes() {
    let mut h = rig();
    let wide = h.state().branch("wide");
    let lane = h.state().lane(&wide.id);
    set_width(lane.as_ref(), 0.3);
    h.state().branching.branches.checkpoint(&wide.id, "work", None, &you()).unwrap();
    let narrow = h.state().branch("narrow");
    set_width(h.state().lane(&narrow.id).as_ref(), 0.18);
    h.state().branching.branches.checkpoint(&narrow.id, "work", None, &you()).unwrap();

    let branching = h.state().branching.clone();
    let runner = Arc::clone(&h.state().core.runner);
    let ids = [BranchId::main(), wide.id.clone(), narrow.id.clone()];
    h.state_mut().ui.open_compare(&ids, &branching, runner.as_ref());
    h.run_steps(3);
    h.state().pictures.settle();
    h.run_steps(3);

    let compare = h.state().ui.compare().unwrap();
    assert_eq!(compare.branches().len(), 3);
    assert_eq!(compare.reference(), 0, "the folder branch is the reference");
    let groups = compare.groups(1).unwrap().unwrap();
    let head = groups.iter().find(|g| g.path == HEAD).expect("wide changes the head");
    assert_eq!(head.changes.iter().find(|c| c.path.ends_with("half_width")).unwrap().after, Some(json!(0.3)));
    assert!(says(&h, "1 differ"), "{:#?}", all_strings(&h));
    assert!(compare.groups(0).is_none(), "the reference has nothing to differ from");

    // Every state is drawn with the same shot; only the state differs.
    let shots = h.state().shots.lock().unwrap().clone();
    assert!(shots.len() >= 3, "{shots:#?}");
    assert!(shots.iter().all(|s| (s.yaw, s.pitch, s.size) == (shots[0].yaw, shots[0].pitch, shots[0].size)));
    let widths: Vec<Option<f64>> = shots.iter().map(|s| s.width).collect();
    for w in [0.3, 0.18] {
        assert!(widths.contains(&Some(w)), "{w} is drawn: {widths:?}");
    }

    // Dragging one picture turns them all.
    let before = h.state().ui.compare().unwrap().camera();
    let picture = h.get_by_label("wide, drag to turn").rect().center();
    h.drag_at(picture);
    h.run_steps(1);
    h.hover_at(picture + vec2(60.0, 0.0));
    h.run_steps(1);
    h.drop_at(picture + vec2(60.0, 0.0));
    h.run_steps(3);
    let after = h.state().ui.compare().unwrap().camera();
    assert!((after.yaw - before.yaw).abs() > 5.0, "{before:?} -> {after:?}");
    h.state().pictures.settle();
    h.run_steps(2);
    let turned: Vec<Shot> =
        h.state().shots.lock().unwrap().iter().filter(|s| s.yaw == Some(after.yaw)).cloned().collect();
    assert!(turned.len() >= 3, "every column is drawn again at the new camera: {turned:#?}");

    // A branch's later work shows after Refresh.
    set_width(lane.as_ref(), 0.33);
    let refresh: Vec<_> =
        h.query_all_by_label("Refresh").filter(|n| n.accesskit_node().role() == Role::Button).collect();
    assert_eq!(refresh.len(), 2, "both branches can be brought up to date");
    drop(refresh);
    h.query_all_by_label("Refresh").find(|n| n.accesskit_node().role() == Role::Button).unwrap().click();
    h.run_steps(3);
    let compare = h.state().ui.compare().unwrap();
    let groups = compare.groups(1).unwrap().unwrap();
    let head = groups.iter().find(|g| g.path == HEAD).unwrap();
    assert_eq!(head.changes.iter().find(|c| c.path.ends_with("half_width")).unwrap().after, Some(json!(0.33)));
}
