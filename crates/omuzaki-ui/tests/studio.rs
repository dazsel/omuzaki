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
use egui_kittest::kittest::Queryable as _;
use egui_kittest::wgpu::WgpuTestRenderer;
use omuzaki_char_api::{BuildServices, CharId, RegistryBuilder, paths};
use omuzaki_doc_api::DocPath;
use omuzaki_ops_api::{Actor, Budget, Grants, Limits, Mode, Request, Runner, Services, SessionSpec, Transport};
use omuzaki_ui::panels::edit::EditFocus;
use omuzaki_ui::panels::viewport::ViewportServices;
use omuzaki_ui::shell::Shell;
use omuzaki_ui::studio;
use omuzaki_ui::wiring::{Core, Document, core_on};
use serde_json::json;

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

fn runner(document: Document) -> Arc<dyn Runner> {
    core(document).runner
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

/// The studio over `runner` at `size`, as `main.rs` builds it: the project's character open, the viewport on `gpu`.
fn studio(runner: &Arc<dyn Runner>, gpu: Option<egui_wgpu::RenderState>, size: egui::Vec2) -> Harness<'static, Studio> {
    let character = studio::open_character(runner.as_ref()).unwrap_or_else(|e| panic!("{e}"));
    let mut shell = Shell::new(Arc::clone(runner), None);
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
        |ui, s: &mut Studio| {
            omuzaki_ui::ds::apply_settings(ui.ctx(), omuzaki_ui::ds::Settings::default());
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
    let runner = runner(Document::empty());
    let mut h = studio(&runner, Some(gpu), vec2(1920.0, 1080.0));
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
    let mut h = studio(&runner, None, vec2(1920.0, 1080.0));
    let agent = connect_agent(&core);
    let start = width(runner.as_ref());
    h.get_by_role_and_label(Role::Button, "History").click();
    h.run_steps(2);

    // The agent's edit, made outside the UI as an MCP tool call is: the shell hears it and History lists it.
    set_width(runner.as_ref(), agent.clone(), start * 1.1);
    h.run_steps(2);
    assert!(h.query_by_label_contains("part.set_param").is_some(), "History shows the agent's op");

    // The person's own edit, then Ctrl+Z in the studio: theirs is undone, the agent's stays.
    set_width(runner.as_ref(), you(), start * 1.2);
    h.run_steps(2);
    h.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    h.run_steps(2);
    assert!((width(runner.as_ref()) - start * 1.1).abs() < 1e-6, "the person's undo took back only their own edit");
    assert!(h.query_by_label_contains("Undid").is_some(), "the status bar says what was undone");

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
        let runner = runner(Document::project(&folder).unwrap_or_else(|e| panic!("{e}")));
        let _h = studio(&runner, None, vec2(1366.0, 768.0));
        let edited = width(runner.as_ref()) * 0.9;
        set_width(runner.as_ref(), you(), edited);
        edited
    };
    assert!(Path::new(&folder).join("project.toml").exists(), "the folder is a project");
    let runner = runner(Document::project(&folder).unwrap_or_else(|e| panic!("{e}")));
    let h = studio(&runner, None, vec2(1366.0, 768.0));
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
        let mut h = studio(&runner, Some(gpu), vec2(w, ht));
        set_width(runner.as_ref(), connect_agent(&core), width(runner.as_ref()) * 1.08);
        h.run_steps(4);
        let image = h.render().unwrap_or_else(|e| panic!("{e}"));
        let path = format!("{out}/studio-{w}x{ht}.png");
        image.save(&path).unwrap_or_else(|e| panic!("writing {path}: {e}"));
    }
}
