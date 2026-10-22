//! The studio as the app starts it (`main.rs`): the real shell over the real executor, opened on the project's
//! character, with the viewport on the GPU egui draws with. These tests are the UI's slice of "Omuzaki exists"
//! (CLAUDE.md): the character is in the viewport, a person and an agent edit the same character through one runner and
//! each sees the other's change at once, each undoes only their own, and a project folder reopens as it was left.
//!
//! The agent is connected as an MCP client is: the executor opens a session for it, and it runs ops as that session's
//! actor on the same `Arc<dyn Runner>` the shell holds, which is what the MCP server does for each tool call. Without a Vulkan adapter (no `mesa-vulkan-drivers`) the GPU tests say so and pass.

// Pixel arithmetic: window coordinates are small, positive and well inside `u32`.
#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]

mod support;

use std::path::Path;
use std::sync::Arc;

use eframe::egui_wgpu;
use egui::accesskit::Role;
use egui::{Key, Modifiers, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::wgpu::WgpuTestRenderer;
use omuzaki_char_api::{BuildServices, CharId, RegistryBuilder, paths};
use omuzaki_doc_api::DocPath;
use omuzaki_ops_api::{
    Actor, Budget, Grants, Limits, Mode, Request, Runner, Services, SessionSpec, Sessions, Transport,
};
use omuzaki_ui::panels::agent::Agents;
use omuzaki_ui::panels::edit::EditFocus;
use omuzaki_ui::panels::setup::FirstRun;
use omuzaki_ui::panels::viewport::ViewportServices;
use omuzaki_ui::shell::Shell;
use omuzaki_ui::studio;
use omuzaki_ui::wiring::{Core, Document, core_on};
use serde_json::json;

/// Runs frames until `seen` holds: the runner's events reach the shell through its own thread, so the frame they land
/// in is not fixed. Gives up after a bound that no healthy run comes near.
fn until(h: &mut Harness<'_, Studio>, what: &str, seen: impl Fn(&Harness<'_, Studio>) -> bool) {
    for _ in 0..500 {
        if seen(h) {
            return;
        }
        std::thread::yield_now();
        h.step();
    }
    panic!("never: {what}");
}

/// The head parameter both actors edit.
const WIDTH: &str = "cranium/half_width";

struct Studio {
    shell: Shell,
}

fn you() -> Actor {
    Actor::Person { name: "you".into() }
}

fn core(document: Document) -> Core {
    omuzaki_ui::link();
    core_on(document, Services::default()).unwrap_or_else(|e| panic!("{e}"))
}

/// An agent connected as an MCP client is: a session the executor opens for it, working for the person.
fn connect_agent(core: &Core) -> Actor {
    let spec = SessionSpec {
        transport: Transport::Stdio,
        client: "designer".into(),
        model: None,
        on_behalf_of: you(),
        grants: Grants::agent_default(),
        budget: Budget::default(),
        limits: Limits::default(),
        parent: None,
        role: None,
        fence: Vec::new(),
    };
    core.sessions.open(spec).unwrap_or_else(|e| panic!("{e}")).actor
}

/// A software GPU, or `None` without a Vulkan adapter.
fn gpu() -> Option<egui_wgpu::RenderState> {
    let state = std::panic::catch_unwind(support::render_state).ok()?;
    if state.adapter.get_info().backend != wgpu::Backend::Vulkan {
        eprintln!("no Vulkan adapter (install mesa-vulkan-drivers): the studio's GPU tests skip");
        return None;
    }
    Some(state)
}

/// The studio over `core` at `size`, as `main.rs` builds it: the project's character open, the viewport on `gpu`, the
/// agent sessions in the Agent panel.
fn studio(core: &Core, gpu: Option<egui_wgpu::RenderState>, size: egui::Vec2) -> Harness<'static, Studio> {
    studio_with(core, gpu, size, None)
}

/// [`studio`], with the setup flow opened as `main.rs` opens it when `first_run` is given and it has not run.
fn studio_with(
    core: &Core,
    gpu: Option<egui_wgpu::RenderState>,
    size: egui::Vec2,
    first_run: Option<FirstRun>,
) -> Harness<'static, Studio> {
    let sessions = Arc::clone(&core.sessions);
    studio_in(core, gpu, size, first_run, omuzaki_ui::ds::Settings::default(), sessions)
}

/// [`studio_with`], in `settings` (a theme), with the Agent panel over `sessions`.
fn studio_in(
    core: &Core,
    gpu: Option<egui_wgpu::RenderState>,
    size: egui::Vec2,
    first_run: Option<FirstRun>,
    settings: omuzaki_ui::ds::Settings,
    sessions: Arc<dyn Sessions>,
) -> Harness<'static, Studio> {
    let character = studio::open_character(core.runner.as_ref()).unwrap_or_else(|e| panic!("{e}"));
    let mut shell = Shell::new(Arc::clone(&core.runner), None);
    if let Some(first_run) = first_run {
        first_run.open_if_new(&shell.requests());
        shell.insert_view(first_run);
    }
    shell.insert_view(Agents(sessions));
    shell.insert_view(EditFocus::new(character));
    shell.insert_view(ViewportServices {
        gpu: gpu.clone(),
        builder: Arc::new(RegistryBuilder),
        build: BuildServices::default(),
    });
    let mut builder = Harness::builder().with_size(size).with_pixels_per_point(1.0).with_step_dt(1.0 / 60.0);
    if let Some(state) = gpu {
        builder = builder.renderer(WgpuTestRenderer::from_render_state(state));
    }
    let mut h = builder.build_ui_state(
        move |ui, s: &mut Studio| {
            omuzaki_ui::ds::apply_settings(ui.ctx(), settings);
            s.shell.ui(ui);
        },
        Studio { shell },
    );
    h.run_steps(4);
    h
}

fn width_path() -> DocPath {
    let kit = CharId::new(studio::SAMPLE_ID).unwrap_or_else(|e| panic!("{e}"));
    let head = omuzaki_char_api::PartId::new("head").unwrap_or_else(|e| panic!("{e}"));
    let mut path = paths::part_params(&kit, &head);
    for segment in WIDTH.split('/') {
        path = path.join(segment);
    }
    path
}

fn width(runner: &dyn Runner) -> f64 {
    let value = runner.get(&width_path(), &Grants::person()).unwrap_or_else(|e| panic!("{e}"));
    value.and_then(|v| v.as_f64()).unwrap_or_else(|| panic!("the head has a {WIDTH}"))
}

fn set_width(runner: &dyn Runner, actor: Actor, value: f64) {
    let grants = if matches!(actor, Actor::Agent { .. }) { Grants::agent_default() } else { Grants::person() };
    let request = Request {
        op: "part.set_param".into(),
        args: json!({"character": studio::SAMPLE_ID, "part": "head", "path": WIDTH, "value": value}),
        mode: Mode::Apply,
        actor,
        grants,
        cause: None,
        tx: None,
        base: None,
    };
    runner.run(request).unwrap_or_else(|e| panic!("part.set_param: {e}"));
}

/// How many pixels of the viewport differ from its backdrop (the colour in its top-left corner) by more than a little.
fn character_pixels(image: &image::RgbaImage, rect: egui::Rect) -> usize {
    let (x0, y0) = (rect.left().max(0.0) as u32 + 4, rect.top().max(0.0) as u32 + 4);
    let (x1, y1) = ((rect.right() as u32).min(image.width()), (rect.bottom() as u32).min(image.height()));
    let backdrop = *image.get_pixel(x0, y0);
    let far =
        |p: &image::Rgba<u8>| (0..3).map(|c| i32::from(p[c]).abs_diff(i32::from(backdrop[c]))).max().unwrap_or(0) > 24;
    (y0..y1).flat_map(|y| (x0..x1).map(move |x| (x, y))).filter(|&(x, y)| far(image.get_pixel(x, y))).count()
}

#[test]
fn the_studio_opens_on_the_kit_character_and_the_viewport_draws_it() {
    let Some(gpu) = gpu() else { return };
    let core = core(Document::empty());
    let mut h = studio(&core, Some(gpu), vec2(1920.0, 1080.0));
    assert_eq!(h.state().shell.active_workspace(), Some("build"));
    for part in ["body", "head", "hair"] {
        assert!(h.query_by_label_contains(part).is_some(), "the outliner lists the {part}");
    }
    // The tab and the panel both carry the name; the panel is the big one.
    let rect = h
        .query_all_by_label("Viewport")
        .map(|node| node.rect())
        .max_by(|a, b| a.area().total_cmp(&b.area()))
        .unwrap_or_else(|| panic!("a viewport"));
    let image = h.render().unwrap_or_else(|e| panic!("{e}"));
    let drawn = character_pixels(&image, rect);
    let area = (rect.width() * rect.height()) as usize;
    assert!(drawn * 50 > area, "the character covers at least 2% of the viewport: {drawn} of {area} pixels");
}

#[test]
fn an_agents_edit_reaches_the_studio_at_once_and_each_side_undoes_only_its_own() {
    let core = core(Document::empty());
    let runner = Arc::clone(&core.runner);
    let mut h = studio(&core, None, vec2(1920.0, 1080.0));
    let agent = connect_agent(&core);
    let start = width(runner.as_ref());
    h.get_by_role_and_label(Role::Button, "History").click();
    h.run_steps(2);

    // The agent's edit, made outside the UI as an MCP tool call is: the shell hears it and History lists it.
    set_width(runner.as_ref(), agent.clone(), start * 1.1);
    until(&mut h, "History shows the agent's op", |h| {
        h.query_all_by_label_contains(": part.set_param").next().is_some()
    });

    // The person's own edit, then Ctrl+Z in the studio: theirs is undone, the agent's stays.
    set_width(runner.as_ref(), you(), start * 1.2);
    until(&mut h, "History shows the person's op", |h| {
        h.query_all_by_label_contains("You: part.set_param").next().is_some()
    });
    h.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    h.run_steps(2);
    assert!((width(runner.as_ref()) - start * 1.1).abs() < 1e-6, "the person's undo took back only their own edit");
    assert!(h.query_by_label_contains("Undid").is_some(), "the status bar says what was undone");
    until(&mut h, "History marks the undone step", |h| h.query_all_by_label_contains("undone").next().is_some());
    assert!(h.query_all_by_label_contains("designer").next().is_some(), "History names the agent as the actor");
    assert!(h.query_all_by_label("You").next().is_some(), "and the person as You");

    // The agent undoes its own, as `history_undo` does over MCP.
    let undone = runner.undo(&agent, &Grants::agent_default()).unwrap_or_else(|e| panic!("{e}"));
    assert!(undone.is_some(), "the agent had a step to undo");
    assert!((width(runner.as_ref()) - start).abs() < 1e-6);
}

#[test]
fn a_project_folder_reopens_as_it_was_left() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let folder = dir.path().join("show");
    let edited = {
        let core = core(Document::project(&folder).unwrap_or_else(|e| panic!("{e}")));
        let runner = Arc::clone(&core.runner);
        let _h = studio(&core, None, vec2(1366.0, 768.0));
        let edited = width(runner.as_ref()) * 0.9;
        set_width(runner.as_ref(), you(), edited);
        edited
    };
    assert!(Path::new(&folder).join("project.toml").exists(), "the folder is a project");
    let core = core(Document::project(&folder).unwrap_or_else(|e| panic!("{e}")));
    let runner = Arc::clone(&core.runner);
    let h = studio(&core, None, vec2(1366.0, 768.0));
    assert_eq!(studio::characters(runner.as_ref()).unwrap_or_else(|e| panic!("{e}")).len(), 1, "no second sample");
    assert!((width(runner.as_ref()) - edited).abs() < 1e-6, "the edit was saved");
    assert!(h.query_by_label_contains("head").is_some());
}

/// The studio as a first-time user sees it, for the pull request's evidence (`evidence/studio.toml`).
#[test]
#[ignore = "writes images; run by the visual evidence job"]
fn studio_evidence() {
    let Ok(out) = std::env::var("OMUZAKI_EVIDENCE_OUT") else { panic!("set OMUZAKI_EVIDENCE_OUT to a folder") };
    for (w, ht) in [(1920.0, 1080.0), (1366.0, 768.0)] {
        let Some(gpu) = gpu() else { panic!("the evidence needs a Vulkan adapter") };
        let core = core(Document::empty());
        let runner = Arc::clone(&core.runner);
        let mut h = studio(&core, Some(gpu), vec2(w, ht));
        set_width(runner.as_ref(), connect_agent(&core), width(runner.as_ref()) * 1.08);
        h.run_steps(4);
        let image = h.render().unwrap_or_else(|e| panic!("{e}"));
        let path = format!("{out}/studio-{w}x{ht}.png");
        image.save(&path).unwrap_or_else(|e| panic!("writing {path}: {e}"));
    }
}

#[test]
fn the_agent_panel_shows_a_connected_agent_and_the_person_can_pause_it() {
    let core = core(Document::empty());
    let mut h = studio(&core, None, vec2(1920.0, 1080.0));
    assert!(
        h.query_all_by_label_contains("Connect an agent").next().is_some(),
        "with no agent it says how to connect one"
    );
    let agent = connect_agent(&core);
    h.run_steps(2);
    assert!(h.query_all_by_label_contains("designer").next().is_some(), "the session is listed by its client name");

    h.get_by_label("Pause").click();
    h.run_steps(2);
    let start = width(core.runner.as_ref());
    let request = Request {
        op: "part.set_param".into(),
        args: json!({"character": studio::SAMPLE_ID, "part": "head", "path": WIDTH, "value": start * 1.1}),
        mode: Mode::Apply,
        actor: agent,
        grants: Grants::agent_default(),
        cause: None,
        tx: None,
        base: None,
    };
    assert!(core.runner.run(request).is_err(), "a paused agent's request does not run");
    assert!((width(core.runner.as_ref()) - start).abs() < 1e-9);
    h.get_by_label("Resume").click();
    h.run_steps(2);
    assert!(h.query_by_label("Pause").is_some(), "resumed, it can be paused again");
}

#[test]
fn a_first_start_opens_setup_beside_the_stage_and_it_ends_where_the_person_chooses() {
    let core = core(Document::empty());
    let folder = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let mark = folder.path().join("setup-done");
    let size = egui::vec2(1366.0, 768.0);
    let mut h = studio_with(&core, None, size, Some(FirstRun::at(mark.clone())));
    assert_eq!(h.state().shell.active_workspace(), Some("perform"), "setup sits beside the live stage");
    assert!(h.query_by_label("Welcome to Omuzaki").is_some());

    // This core has no camera, performer or output ops: each step says so by offering to go on without them.
    for (button, next) in [
        ("Start", "Choose your character"),
        ("Use the sample", "Let the camera follow your face"),
        (
            "Skip for now",
            "Look at the camera with a relaxed face and press Calibrate. Hold still for about five seconds.",
        ),
        ("Skip for now", "Send the character to OBS"),
        ("Next", "You're set"),
    ] {
        h.get_by_role_and_label(Role::Button, button).click();
        h.run_steps(3);
        assert!(h.query_all_by_label(next).next().is_some(), "after {button}: no {next:?}");
    }
    assert!(h.query_by_label("Back").is_none(), "the last step has nothing to go back to");
    h.get_by_role_and_label(Role::Button, "Customise the character").click();
    h.run_steps(4);
    assert!(mark.exists(), "the flow is remembered");
    assert_eq!(h.state().shell.active_workspace(), Some("build"));
    assert!(h.query_by_label("You're set").is_none());
    drop(h);

    // The next start opens where the studio always does, with no setup.
    let h = studio_with(&core, None, size, Some(FirstRun::at(mark)));
    assert_ne!(h.state().shell.active_workspace(), Some("perform"));
    assert!(h.query_by_label("Welcome to Omuzaki").is_none());
}

#[test]
fn skip_setup_ends_the_flow_from_any_step() {
    let core = core(Document::empty());
    let folder = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let mark = folder.path().join("setup-done");
    let mut h = studio_with(&core, None, egui::vec2(1366.0, 768.0), Some(FirstRun::at(mark.clone())));
    h.get_by_role_and_label(Role::Button, "Start").click();
    h.run_steps(3);
    h.get_by_role_and_label(Role::Button, "Back").click();
    h.run_steps(3);
    assert!(h.query_by_label("Welcome to Omuzaki").is_some(), "Back returns to the welcome");
    h.get_by_role_and_label(Role::Button, "Skip setup").click();
    h.run_steps(4);
    assert!(mark.exists());
    assert!(h.query_by_label("Welcome to Omuzaki").is_none());
    assert_eq!(h.state().shell.active_workspace(), Some("perform"), "skipping leaves the person where they are");
}

/// The whole app as `main.rs` starts it on a first start, for the pull request's evidence: the live slice with the
/// project's character on the stage, and the shell on its executor with the setup flow open beside it.
#[test]
#[ignore = "writes images; run by the visual evidence job"]
fn first_run_evidence() {
    use omuzaki_render::WgpuRenderer;
    use omuzaki_ui::aux_window::Transparency;
    use omuzaki_ui::live::viewport::{NativePresenter, project_stage, published};
    use omuzaki_ui::live::wiring::{LiveApp, LiveScene, NoTracker, RealClock};

    struct App {
        shell: Shell,
        live: LiveApp,
    }

    let Ok(out) = std::env::var("OMUZAKI_EVIDENCE_OUT") else { panic!("set OMUZAKI_EVIDENCE_OUT to a folder") };
    let Some(gpu) = gpu() else { panic!("the evidence needs a Vulkan adapter") };
    omuzaki_ui::link();
    let folder = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let document = Document::project(&folder.path().join("project")).unwrap_or_else(|e| panic!("{e}"));
    let (id, def) = studio::character_to_stage(&document).unwrap_or_else(|e| panic!("{e}"));
    let mut renderer = WgpuRenderer::from_device(&gpu.adapter, gpu.device.clone(), gpu.queue.clone())
        .unwrap_or_else(|e| panic!("{e}"));
    let size = omuzaki_core::glam::UVec2::new(1920, 1080);
    let stage = project_stage(&mut renderer, &id, &def, size).unwrap_or_else(|e| panic!("{e}"));
    let base = LiveScene::base_pose(&stage);
    let mut live =
        LiveApp::start_on(document, Box::new(NoTracker), Box::new(RealClock::new()), Transparency::Fallback, base)
            .unwrap_or_else(|e| panic!("{e}"));
    live.attach_viewport(renderer, stage, size, Box::new(NativePresenter::new(gpu.clone())))
        .unwrap_or_else(|e| panic!("{e}"));
    let core = live.core();
    let character = studio::open_character(core.runner.as_ref()).unwrap_or_else(|e| panic!("{e}"));
    let mut shell = Shell::new(Arc::clone(&core.runner), None);
    shell.insert_view(Agents(Arc::clone(&core.sessions)));
    shell.insert_view(EditFocus::new(character));
    shell.insert_view(ViewportServices {
        gpu: Some(gpu.clone()),
        builder: Arc::new(RegistryBuilder),
        build: BuildServices::default(),
    });
    let first_run = FirstRun::at(folder.path().join("setup-done"));
    first_run.open_if_new(&shell.requests());
    shell.insert_view(first_run);
    let mut h = Harness::builder()
        .with_size(vec2(1366.0, 768.0))
        .with_pixels_per_point(1.0)
        .renderer(WgpuTestRenderer::from_render_state(gpu))
        .build_ui_state(
            |ui, app: &mut App| {
                omuzaki_ui::ds::apply_settings(ui.ctx(), omuzaki_ui::ds::Settings::default());
                app.live.show(ui.ctx());
                app.shell.ui(ui);
            },
            App { shell, live },
        );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while published(&h.ctx).is_none() && std::time::Instant::now() < deadline {
        h.step();
        // Paces the loop; what it waits for is the first published picture, not the time.
        std::thread::sleep(std::time::Duration::from_millis(8));
    }
    assert!(published(&h.ctx).is_some(), "the stage drew its first frame");
    h.run_steps(4);
    for (name, button) in [("welcome", None), ("character", Some("Start"))] {
        if let Some(button) = button {
            h.get_by_role_and_label(Role::Button, button).click();
            h.run_steps(4);
        }
        let image = h.render().unwrap_or_else(|e| panic!("{e}"));
        let path = format!("{out}/first-run-{name}-1366x768.png");
        image.save(&path).unwrap_or_else(|e| panic!("writing {path}: {e}"));
    }
    h.state_mut().live.shutdown().unwrap_or_else(|e| panic!("{e}"));
}

/// Presses Tab until the text field called `label` has the keyboard.
fn tab_to_text(h: &mut Harness<'_, Studio>, label: &str) {
    for _ in 0..12 {
        h.key_press(Key::Tab);
        h.run_steps(2);
        if h.query_by_role_and_label(Role::TextInput, label).is_some_and(|n| n.is_focused()) {
            return;
        }
    }
    panic!("Tab never reached {label}");
}

/// The palette over the real executor: Ctrl+K finds `character.rename`, Tab opens its form filled with the open
/// character, the live preview dry-runs it, and Enter renames the character as the person, one undo step.
fn rename_in_palette(h: &mut Harness<'_, Studio>, name: &str) {
    h.key_press_modifiers(Modifiers::COMMAND, Key::K);
    h.run_steps(2);
    h.event(egui::Event::Text(">character.rename".to_owned()));
    h.run_steps(2);
    h.key_press(Key::Tab);
    h.run_steps(2);
    tab_to_text(h, "name");
    h.event(egui::Event::Text(name.to_owned()));
    // The preview waits for the arguments to settle (120 ms), then dry-runs them.
    h.run_steps(20);
}

#[test]
fn the_palette_renames_the_open_character_with_a_live_preview_and_undo() {
    let core = core(Document::empty());
    let mut h = studio(&core, None, vec2(1366.0, 768.0));
    let name = || {
        let path = DocPath::parse(&format!("characters/{}/name", studio::SAMPLE_ID)).unwrap_or_else(|e| panic!("{e}"));
        core.runner.get(&path, &Grants::person()).unwrap_or_else(|e| panic!("{e}"))
    };
    let before = name();
    rename_in_palette(&mut h, "Mika");
    assert!(h.query_by_label_contains("Live preview").is_some());
    assert!(h.query_by_label_contains("Values it would change: 1").is_some(), "the dry run found the one change");
    assert_eq!(name(), before, "a preview changes nothing");
    assert!(h.query_by_label_contains("Same as MCP tool character_rename").is_some());
    h.key_press(Key::Enter);
    h.run_steps(2);
    assert_eq!(name(), Some(json!("Mika")));
    assert!(h.query_by_label_contains("character.rename: done. Values changed: 1").is_some());
    assert_eq!(h.state().shell.palette_recents(), ["character.rename"]);
    h.key_press(Key::Escape);
    h.run_steps(2);
    h.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    h.run_steps(2);
    assert_eq!(name(), before, "Ctrl+Z undoes the palette's run");
}

/// The palette at 1366x768 over the real studio: its results for "rename", then `character.rename`'s form with the
/// open character filled in, a name typed, and the live preview.
#[test]
#[ignore = "writes images; run by the visual evidence job"]
fn palette_evidence() {
    let Ok(out) = std::env::var("OMUZAKI_EVIDENCE_OUT") else { panic!("set OMUZAKI_EVIDENCE_OUT to a folder") };
    let Some(gpu) = gpu() else { panic!("the evidence needs a Vulkan adapter") };
    let core = core(Document::empty());
    let mut h = studio(&core, Some(gpu), vec2(1366.0, 768.0));
    h.key_press_modifiers(Modifiers::COMMAND, Key::K);
    h.run_steps(2);
    h.event(egui::Event::Text("rename".to_owned()));
    h.run_steps(4);
    let save = |h: &mut Harness<'_, Studio>, file: &str| {
        let image = h.render().unwrap_or_else(|e| panic!("{e}"));
        let path = format!("{out}/{file}");
        image.save(&path).unwrap_or_else(|e| panic!("writing {path}: {e}"));
    };
    save(&mut h, "palette-search-1366x768.png");
    h.key_press(Key::Escape);
    h.run_steps(2);
    rename_in_palette(&mut h, "Mika");
    save(&mut h, "palette-form-1366x768.png");
}

#[test]
fn history_shows_a_steps_details_and_adjust_reopens_it_in_the_palette() {
    let core = core(Document::empty());
    let runner = Arc::clone(&core.runner);
    let mut h = studio(&core, None, vec2(1920.0, 1080.0));
    h.get_by_role_and_label(Role::Button, "History").click();
    h.run_steps(2);
    let start = width(runner.as_ref());
    set_width(runner.as_ref(), you(), start * 1.2);
    until(&mut h, "the status bar names what Ctrl+Z would undo", |h| {
        h.query_by_label_contains("Ctrl+Z undoes part.set_param head").is_some()
    });
    let row = h
        .query_all_by_role(Role::Button)
        .find(|n| n.accesskit_node().label().is_some_and(|l| l.starts_with("You: part.set_param")))
        .unwrap_or_else(|| panic!("the person's step is listed"));
    row.click();
    h.run_steps(2);
    assert!(h.query_by_label_contains(&format!("{WIDTH}: ")).is_some(), "the details show the value that changed");
    // A new popover sizes itself on its first frame and settles on the next ones.
    h.run_steps(3);
    h.get_by_role_and_label(Role::Button, "Adjust").click();
    h.run_steps(3);
    assert!(h.state().shell.palette_open(), "Adjust opens the palette");
    assert!(h.query_by_label_contains("Same as MCP tool part_set_param").is_some(), "on the step's own op");
    let shown: Vec<String> = h.query_all_by_label("value").filter_map(|n| n.accesskit_node().value()).collect();
    let expected = format!("{:.2}", start * 1.2);
    assert!(shown.iter().any(|v| v.starts_with(&expected)), "filled with the step's arguments: {shown:?}, {expected}");
}

/// History at 1920x1080 over the real studio: the person's and an agent's steps, newest first, with the agent's step's
/// details open beside its row.
#[test]
#[ignore = "writes images; run by the visual evidence job"]
fn history_evidence() {
    let Ok(out) = std::env::var("OMUZAKI_EVIDENCE_OUT") else { panic!("set OMUZAKI_EVIDENCE_OUT to a folder") };
    let Some(gpu) = gpu() else { panic!("the evidence needs a Vulkan adapter") };
    let core = core(Document::empty());
    let runner = Arc::clone(&core.runner);
    let mut h = studio(&core, Some(gpu), vec2(1920.0, 1080.0));
    h.get_by_role_and_label(Role::Button, "History").click();
    h.run_steps(2);
    let agent = connect_agent(&core);
    let start = width(runner.as_ref());
    set_width(runner.as_ref(), you(), start * 1.05);
    set_width(runner.as_ref(), agent, start * 1.1);
    set_width(runner.as_ref(), you(), start * 1.15);
    until(&mut h, "three steps listed", |h| h.query_all_by_label_contains(": part.set_param").count() == 3);
    let row = h
        .query_all_by_role(Role::Button)
        .find(|n| n.accesskit_node().label().is_some_and(|l| l.starts_with("designer")))
        .unwrap_or_else(|| panic!("the agent's step is listed"));
    row.click();
    // Past the popover's fade-in.
    h.run_steps(30);
    let image = h.render().unwrap_or_else(|e| panic!("{e}"));
    let path = format!("{out}/history-1920x1080.png");
    image.save(&path).unwrap_or_else(|e| panic!("writing {path}: {e}"));
}

// ---- the Agent panel's evidence ----

/// The executor's session registry, with requests waiting and fences held staged on top: the executor queues no
/// approvals and holds no fences yet (they land with app core's executor work), so the evidence shows what the panel
/// does with them over the real sessions.
struct Staged {
    inner: Arc<dyn Sessions>,
    approvals: Vec<omuzaki_ops_api::PendingApproval>,
    fences: Vec<(omuzaki_ops_api::SessionId, Vec<DocPath>)>,
}

impl Sessions for Staged {
    fn open(&self, spec: SessionSpec) -> Result<omuzaki_ops_api::SessionInfo, omuzaki_ops_api::OpError> {
        self.inner.open(spec)
    }

    fn sessions(&self) -> Vec<omuzaki_ops_api::SessionInfo> {
        let mut all = self.inner.sessions();
        for session in &mut all {
            if let Some((_, fence)) = self.fences.iter().find(|(id, _)| *id == session.id) {
                session.fence.clone_from(fence);
            }
        }
        all
    }

    fn session(&self, id: omuzaki_ops_api::SessionId) -> Option<omuzaki_ops_api::SessionInfo> {
        self.sessions().into_iter().find(|s| s.id == id)
    }

    fn session_of(&self, actor: &Actor) -> Option<omuzaki_ops_api::SessionInfo> {
        self.sessions().into_iter().find(|s| s.actor == *actor)
    }

    fn pause(
        &self,
        id: omuzaki_ops_api::SessionId,
        by: &Actor,
    ) -> Result<omuzaki_ops_api::SessionInfo, omuzaki_ops_api::OpError> {
        self.inner.pause(id, by)
    }

    fn resume(
        &self,
        id: omuzaki_ops_api::SessionId,
        by: &Actor,
    ) -> Result<omuzaki_ops_api::SessionInfo, omuzaki_ops_api::OpError> {
        self.inner.resume(id, by)
    }

    fn set_grants(
        &self,
        id: omuzaki_ops_api::SessionId,
        grants: Grants,
        by: &Actor,
    ) -> Result<omuzaki_ops_api::SessionInfo, omuzaki_ops_api::OpError> {
        self.inner.set_grants(id, grants, by)
    }

    fn set_budget(
        &self,
        id: omuzaki_ops_api::SessionId,
        budget: Budget,
        by: &Actor,
    ) -> Result<omuzaki_ops_api::SessionInfo, omuzaki_ops_api::OpError> {
        self.inner.set_budget(id, budget, by)
    }

    fn charge(
        &self,
        id: omuzaki_ops_api::SessionId,
        spend: omuzaki_ops_api::Spend,
    ) -> Result<omuzaki_ops_api::SessionInfo, omuzaki_ops_api::OpError> {
        self.inner.charge(id, spend)
    }

    fn end(
        &self,
        id: omuzaki_ops_api::SessionId,
        reason: omuzaki_ops_api::EndReason,
    ) -> Result<omuzaki_ops_api::Ended, omuzaki_ops_api::OpError> {
        self.inner.end(id, reason)
    }

    fn undo_session(
        &self,
        id: omuzaki_ops_api::SessionId,
        by: &Actor,
    ) -> Result<omuzaki_ops_api::SessionUndo, omuzaki_ops_api::OpError> {
        self.inner.undo_session(id, by)
    }

    fn approvals(&self) -> Vec<omuzaki_ops_api::PendingApproval> {
        self.approvals.clone()
    }

    fn resolve(
        &self,
        id: omuzaki_ops_api::ApprovalId,
        decision: omuzaki_ops_api::Decision,
        by: &Actor,
    ) -> Result<omuzaki_ops_api::Resolution, omuzaki_ops_api::OpError> {
        self.inner.resolve(id, decision, by)
    }
}

/// A request from `session` to run `op`, needing `need`, previewing a rename of the kit character.
fn request(
    id: u64,
    session: &omuzaki_ops_api::SessionInfo,
    op: &str,
    need: omuzaki_ops_api::Need,
) -> omuzaki_ops_api::PendingApproval {
    let path = DocPath::parse("characters/kit/name").unwrap_or_else(|e| panic!("{e}"));
    let patch = omuzaki_doc_api::Patch::new().with(path, Some(json!("Kit")), Some(json!("Kit for the stream")));
    omuzaki_ops_api::PendingApproval {
        id: omuzaki_ops_api::ApprovalId(id),
        session: session.id,
        actor: session.actor.clone(),
        request: Some(omuzaki_ops_api::ApprovalRequest {
            op: op.to_owned(),
            args: json!({"character": "kit"}),
            cause: None,
            preview: omuzaki_ops_api::Plan::edit(patch),
        }),
        batch: format!("{}/{op}/{need:?}", session.id.0),
        need,
        requested_at: omuzaki_core::time::Seconds(0.0),
    }
}

/// Two agents on the real executor, three steps by one of them, four requests waiting (three alike), and the first
/// agent holding the kit's head.
fn staged(core: &Core) -> Arc<dyn Sessions> {
    // The sample character, as the studio opens it, so the agent has a head to edit.
    studio::open_character(core.runner.as_ref()).unwrap_or_else(|e| panic!("{e}"));
    let open = |client: &str, transport: Transport, model: Option<&str>| {
        let spec = SessionSpec {
            transport,
            client: client.into(),
            model: model.map(str::to_owned),
            on_behalf_of: you(),
            grants: Grants::agent_default(),
            budget: Budget::default(),
            limits: Limits::default(),
            parent: None,
            role: None,
            fence: Vec::new(),
        };
        core.sessions.open(spec).unwrap_or_else(|e| panic!("{e}"))
    };
    let designer = open("claude_code", Transport::Stdio, Some("assistant"));
    let bot = open("studio_bot", Transport::Http { token: "stream_deck".into() }, None);
    let budget = Budget { usd_micros: 2_000_000, ..Budget::default() };
    core.sessions.set_budget(bot.id, budget, &you()).unwrap_or_else(|e| panic!("{e}"));
    let start = width(core.runner.as_ref());
    for step in 1..=3 {
        set_width(core.runner.as_ref(), designer.actor.clone(), start * (1.0 + 0.02 * f64::from(step)));
    }
    let publish = omuzaki_ops_api::Need::Scope { scope: omuzaki_ops_api::Scope::Publish };
    let switch = omuzaki_ops_api::Need::Person {
        reason: "Switching the model sends your prompts to a different provider from now on.".into(),
    };
    let approvals = vec![
        request(1, &designer, "io.export", publish.clone()),
        request(2, &designer, "io.export", publish.clone()),
        request(3, &designer, "io.export", publish),
        request(4, &bot, "assistant.set_model", switch),
    ];
    let head = DocPath::parse("characters/kit/parts/head").unwrap_or_else(|e| panic!("{e}"));
    Arc::new(Staged { inner: Arc::clone(&core.sessions), approvals, fences: vec![(designer.id, vec![head])] })
}

fn save(h: &mut Harness<'_, Studio>, out: &str, name: &str) {
    // Past the fade-in of popovers and the stack.
    h.run_steps(30);
    let image = h.render().unwrap_or_else(|e| panic!("{e}"));
    let path = format!("{out}/{name}");
    image.save(&path).unwrap_or_else(|e| panic!("writing {path}: {e}"));
}

/// The Agent panel over the real studio (#390, #420, #969): in Build at 1920x1080, dark and light, with the head
/// selected so the inspector shows the fence note, the outliner its lock and the status bar its chip; the grants
/// popover; and in Perform, where no Agent panel is docked, the approval stack at the top right.
#[test]
#[ignore = "writes images; run by the visual evidence job"]
fn agents_evidence() {
    let Ok(out) = std::env::var("OMUZAKI_EVIDENCE_OUT") else { panic!("set OMUZAKI_EVIDENCE_OUT to a folder") };
    for (theme, name) in [(omuzaki_ui::ds::ThemeKind::Dark, "dark"), (omuzaki_ui::ds::ThemeKind::Light, "light")] {
        let Some(gpu) = gpu() else { panic!("the evidence needs a Vulkan adapter") };
        let core = core(Document::empty());
        let sessions = staged(&core);
        let settings = omuzaki_ui::ds::Settings { theme, ..omuzaki_ui::ds::Settings::default() };
        let mut h = studio_in(&core, Some(gpu), vec2(1920.0, 1080.0), None, settings, sessions);
        h.get_by_role_and_label(Role::Tab, "Build").click();
        h.run_steps(3);
        h.get_by_label("head").click();
        until(&mut h, "the fence note shows", |h| {
            h.query_all_by_label_contains("is working on this part").next().is_some()
        });
        save(&mut h, &out, &format!("agents-{name}-1920x1080.png"));
        if name == "dark" {
            // The session rows sit under the approvals in the docked panel: bring the row's button into view first.
            h.get_by_label("Grants for claude_code").scroll_to_me();
            h.run_steps(3);
            h.get_by_label("Grants for claude_code").click();
            until(&mut h, "the grants popover opens", |h| h.query_all_by_label_contains("Revoke all").next().is_some());
            save(&mut h, &out, "agents-grants-1920x1080.png");
            h.key_press(Key::Escape);
            h.get_by_role_and_label(Role::Tab, "Perform").click();
            until(&mut h, "the stack shows", |h| {
                h.query_all_by_label_contains("Approve all 3 similar requests").next().is_some()
            });
            save(&mut h, &out, "agents-stack-1920x1080.png");
        }
    }
}

/// Evidence from the team service for the review render: Ada approved, two threads open, this copy matches.
struct Desk;

impl omuzaki_ui::panels::agent::review::ReviewDesk for Desk {
    fn evidence(
        &self,
        _approval: &omuzaki_ops_api::PendingApproval,
    ) -> Option<omuzaki_ui::panels::agent::review::ReviewEvidence> {
        Some(omuzaki_ui::panels::agent::review::ReviewEvidence {
            own: false,
            requested: "b3:4f2a".into(),
            local: Some("b3:4f2a".into()),
            approved_by: vec!["Ada".into()],
            needed: 2,
            threads: vec!["the fringe clips the brow".into()],
            feedback: Vec::new(),
            licence: Some("passed".into()),
        })
    }

    fn export(&self, _approval: &omuzaki_ops_api::PendingApproval) -> Result<String, omuzaki_ops_api::OpError> {
        Ok(String::new())
    }
}

/// The review card over the real studio at 1366x768, dark and light (#1374): a publish a two-reviewer policy holds,
/// half approved, in the Perform workspace's approval stack.
#[test]
#[ignore = "writes images; run by the visual evidence job"]
fn review_evidence() {
    let Ok(out) = std::env::var("OMUZAKI_EVIDENCE_OUT") else { panic!("set OMUZAKI_EVIDENCE_OUT to a folder") };
    for (theme, name) in [(omuzaki_ui::ds::ThemeKind::Dark, "dark"), (omuzaki_ui::ds::ThemeKind::Light, "light")] {
        let Some(gpu) = gpu() else { panic!("the evidence needs a Vulkan adapter") };
        let core = core(Document::empty());
        studio::open_character(core.runner.as_ref()).unwrap_or_else(|e| panic!("{e}"));
        let spec = SessionSpec {
            transport: Transport::Stdio,
            client: "claude_code".into(),
            model: Some("assistant".into()),
            on_behalf_of: you(),
            grants: Grants::agent_default(),
            budget: Budget::default(),
            limits: Limits::default(),
            parent: None,
            role: None,
            fence: Vec::new(),
        };
        let designer = core.sessions.open(spec).unwrap_or_else(|e| panic!("{e}"));
        let need = omuzaki_ops_api::Need::Review { policy: "two_reviewers".into() };
        let approvals = vec![request(1, &designer, "io.export", need)];
        let sessions: Arc<dyn Sessions> =
            Arc::new(Staged { inner: Arc::clone(&core.sessions), approvals, fences: Vec::new() });
        let settings = omuzaki_ui::ds::Settings { theme, ..omuzaki_ui::ds::Settings::default() };
        let mut h = studio_in(&core, Some(gpu), vec2(1366.0, 768.0), None, settings, sessions);
        h.state_mut().shell.insert_view(omuzaki_ui::panels::agent::review::Reviews(Arc::new(Desk)));
        h.get_by_role_and_label(Role::Tab, "Perform").click();
        until(&mut h, "the review card shows", |h| h.query_all_by_label_contains("1 of 2 approvals").next().is_some());
        save(&mut h, &out, &format!("review-{name}-1366x768.png"));
    }
}
