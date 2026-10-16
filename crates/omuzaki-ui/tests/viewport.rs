//! The viewport (`shell.md` section 5) on a software GPU: idle with a character open it asks for no frame, a camera
//! drag renders every frame, and its picture is the still `render.preview`'s path renders from the same camera
//! (`omuzaki_stage::ops::render::still::render_stills`), compared pixel by pixel through the real egui callback.
//!
//! egui (through `egui_kittest`'s wgpu renderer) and the viewport share one lavapipe device, as the app's eframe window
//! and the viewport do. Without a Vulkan adapter (no `mesa-vulkan-drivers`) the tests say so and pass.

use std::sync::{Arc, Mutex, PoisonError};

use eframe::egui_wgpu;
use egui::{Event, Modifiers, PointerButton, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable as _;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use omuzaki_char_api::{BuildOptions, BuildServices, CharId, CharacterBuilder, CharacterDef, Quality as BuildQuality};
use omuzaki_doc_api::Value;
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, Event as OpEvent, Grants, Runner};
use omuzaki_ops_mock::MockRunner;
use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};
use omuzaki_render_api::{Quality, Renderer};
use omuzaki_stage::ops::render::job::{DEFAULT_LIGHT_RIG, still_light};
use omuzaki_stage::ops::render::still::{Shot, StillSpec, render_stills};
use omuzaki_ui::panels::edit::EditFocus;
use omuzaki_ui::panels::io::Io;
use omuzaki_ui::panels::viewport::camera::OrbitCamera;
use omuzaki_ui::panels::viewport::gpu::Gpu;
use omuzaki_ui::panels::viewport::{Viewport, ViewportServices};
use serde_json::json;

/// How many pointer moves the drag test makes.
const MOVES: u64 = 6;
/// The window: a toolbar row over the viewport.
const SIZE: egui::Vec2 = vec2(320.0, 280.0);

struct Rig {
    viewport: Viewport,
    /// The device egui and the viewport share.
    gpu: egui_wgpu::RenderState,
    runner: MockRunner<MemDoc>,
    events: std::sync::mpsc::Receiver<OpEvent>,
    frame: Vec<OpEvent>,
    actor: Actor,
}

fn mock() -> CharId {
    CharId::new("mock").unwrap_or_else(|_| panic!("an id"))
}

fn node() -> Value {
    serde_json::from_str(omuzaki_char_mock::CHARACTER_JSON).unwrap_or(Value::Null)
}

/// A harness whose wgpu renderer shares its device with the viewport, or `None` without a Vulkan adapter.
fn rig() -> Option<Harness<'static, Rig>> {
    omuzaki_char::link();
    omuzaki_char_mock::link();
    let state =
        std::panic::catch_unwind(|| create_render_state(default_wgpu_setup(), egui_wgpu::RendererOptions::default()))
            .ok()?;
    if state.adapter.get_info().backend != wgpu::Backend::Vulkan {
        eprintln!("no Vulkan adapter (install mesa-vulkan-drivers): the viewport tests skip");
        return None;
    }
    let services = ViewportServices {
        gpu: Some(state.clone()),
        builder: Arc::new(omuzaki_char_api::RegistryBuilder),
        build: BuildServices::default(),
    };
    let runner = MockRunner::new(MemDoc::from_value(json!({"characters": {"mock": node()}})));
    let events = runner.subscribe(&Grants::person()).unwrap_or_else(|e| panic!("{e}"));
    let rig = Rig {
        viewport: Viewport::new(EditFocus::new(mock()), services),
        gpu: state.clone(),
        runner,
        events,
        frame: Vec::new(),
        actor: Actor::Person { name: "kit".to_owned() },
    };
    let mut h = Harness::builder()
        .with_size(SIZE)
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .renderer(WgpuTestRenderer::from_render_state(state))
        .build_ui_state(
            |ui, r: &mut Rig| {
                r.frame = r.events.try_iter().collect();
                r.viewport.ui(ui, Io::new(&r.runner, &r.actor, &r.frame));
            },
            rig,
        );
    // The first frames build and stage the character, and the callback runs only when the frame is rendered.
    h.run_steps(2);
    render(&mut h);
    Some(h)
}

/// Renders the frame through egui's wgpu renderer, which runs the viewport's callback.
fn render(h: &mut Harness<'_, Rig>) -> image::RgbaImage {
    h.render().unwrap_or_else(|e| panic!("the frame renders: {e}"))
}

fn renders(h: &Harness<'_, Rig>) -> u64 {
    h.state().viewport.renders()
}

fn viewport_rect(h: &Harness<'_, Rig>) -> egui::Rect {
    h.get_by_label("Viewport").rect()
}

#[test]
fn idle_with_a_character_open_it_asks_for_no_frame() {
    let Some(mut h) = rig() else { return };
    assert_eq!(h.state().viewport.problem(), None);
    let settled = h.try_run();
    assert!(settled.is_ok(), "an idle viewport keeps asking for frames: {settled:?}");
    render(&mut h);
    let before = renders(&h);
    assert!(before >= 1, "the character was drawn once");
    for _ in 0..5 {
        h.step();
        render(&mut h);
    }
    assert_eq!(renders(&h), before, "frames egui draws for other reasons copy the last picture, they don't render");
}

#[test]
fn a_camera_drag_renders_every_frame() {
    let Some(mut h) = rig() else { return };
    let centre = viewport_rect(&h).center();
    let before = renders(&h);
    let start = h.state().viewport.camera();
    h.event(Event::PointerMoved(centre));
    h.event(Event::PointerButton {
        pos: centre,
        button: PointerButton::Middle,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    h.step();
    render(&mut h);
    for i in 1..=MOVES {
        #[allow(clippy::cast_precision_loss, reason = "a small count")]
        h.event(Event::PointerMoved(centre + vec2(8.0 * i as f32, 0.0)));
        h.step();
        render(&mut h);
    }
    h.event(Event::PointerButton {
        pos: centre + vec2(48.0, 0.0),
        button: PointerButton::Middle,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    h.step();
    render(&mut h);
    assert_ne!(h.state().viewport.camera(), start, "a middle drag orbits");
    assert!(renders(&h) >= before + MOVES, "each frame of the drag rendered: {} after {before}", renders(&h));
}

#[test]
fn the_keys_frame_and_turn_the_camera() {
    let Some(mut h) = rig() else { return };
    h.get_by_label("Viewport").click();
    h.run();
    omuzaki_ui::ds::testing::press(&mut h, Modifiers::NONE, egui::Key::Num3);
    assert!((h.state().viewport.camera().yaw - 90.0).abs() < 1e-3, "3 is the side view");
    let far = h.state().viewport.camera().distance;
    let rect = viewport_rect(&h);
    h.event(Event::PointerMoved(rect.center()));
    h.event(Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: vec2(0.0, 200.0),
        phase: egui::TouchPhase::Move,
        modifiers: Modifiers::NONE,
    });
    // egui eases a wheel scroll over several frames.
    h.run_steps(30);
    assert!(h.state().viewport.camera().distance < far, "the wheel zooms in");
    omuzaki_ui::ds::testing::press(&mut h, Modifiers::NONE, egui::Key::Home);
    assert!((h.state().viewport.camera().distance - far).abs() < 1e-3, "Home frames the character again");
}

/// The still `render.preview`'s path renders for `camera` at `size`, on a renderer of its own.
fn still(camera: OrbitCamera, size: [u32; 2]) -> image::RgbaImage {
    let mut renderer = WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software })
        .unwrap_or_else(|e| panic!("a software renderer: {e}"));
    let def = CharacterDef::from_node(&node()).unwrap_or_else(|e| panic!("{e}"));
    let options = BuildOptions { variant: None, quality: BuildQuality::Live, rig_export: false };
    let built = omuzaki_char_api::RegistryBuilder
        .build(&mock(), &def, &options, &BuildServices::default())
        .unwrap_or_else(|e| panic!("the mock character builds: {e}"));
    let light = still_light(DEFAULT_LIGHT_RIG).unwrap_or_else(|e| panic!("{e}"));
    let spec = StillSpec::new(light, size.into(), Quality::live());
    let stills = render_stills(&mut renderer as &mut dyn Renderer, built, &spec, &[Shot::new(camera.camera())])
        .unwrap_or_else(|e| panic!("the still renders: {e}"));
    let image = &stills[0].image;
    image::RgbaImage::from_raw(image.size.x, image.size.y, image.rgba8.clone()).unwrap_or_default()
}

/// The largest difference of any channel of any pixel, and how many pixels differ at all.
fn compare(a: &image::RgbaImage, b: &image::RgbaImage) -> (u8, usize) {
    assert_eq!(a.dimensions(), b.dimensions());
    a.pixels().zip(b.pixels()).fold((0, 0), |(worst, count), (p, q)| {
        let d = p.0.iter().zip(q.0).map(|(x, y)| x.abs_diff(y)).max().unwrap_or(0);
        (worst.max(d), count + usize::from(d > 0))
    })
}

#[test]
fn the_picture_is_the_still_from_the_same_camera() {
    let Some(mut h) = rig() else { return };
    let camera =
        OrbitCamera { target: omuzaki_core::glam::Vec3::new(0.0, 0.85, 0.0), yaw: 30.0, pitch: 10.0, distance: 2.4 };
    h.state_mut().viewport.set_camera(camera);
    h.step();
    let frame = render(&mut h);
    let rect = viewport_rect(&h);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a rect inside a small window")]
    let (x, y, w, hgt) = (rect.left() as u32, rect.top() as u32, rect.width() as u32, rect.height() as u32);
    let expected = still(camera, [w, hgt]);

    // The stage's own target, read back: the still's pixels.
    let target = {
        let mut egui_renderer = h.state().gpu.renderer.write();
        let gpu = egui_renderer
            .callback_resources
            .get_mut::<Mutex<Gpu>>()
            .unwrap_or_else(|| panic!("the viewport installed its GPU side"));
        let gpu = gpu.get_mut().unwrap_or_else(PoisonError::into_inner);
        let id = gpu.target().unwrap_or_else(|| panic!("the viewport has a target"));
        let image = gpu.renderer().read_target(id).unwrap_or_else(|e| panic!("{e}"));
        image::RgbaImage::from_raw(image.size.x, image.size.y, image.rgba8).unwrap_or_default()
    };
    let (worst, differing) = compare(&target, &expected);
    eprintln!("viewport target vs still at {w}x{hgt}: largest channel difference {worst}, {differing} pixels differ");
    assert_eq!(worst, 0, "the stage's picture is the still's, exactly");

    // What egui put on screen, through the callback's blit.
    let shown = image::imageops::crop_imm(&frame, x, y, w, hgt).to_image();
    let (worst, differing) = compare(&shown, &expected);
    eprintln!("viewport on screen vs still: largest channel difference {worst}, {differing} pixels differ");
    assert!(worst <= 2, "the viewport's picture on screen is not the still's: a channel differs by {worst}");
}

// ---- evidence ----

/// The three edit panels as the Build workspace docks them (`shell.md` section 2: the outliner on the left, the
/// viewport in the centre, the inspector on the right), with the mock character open and its head selected.
struct Build {
    outliner: omuzaki_ui::panels::outliner::Outliner,
    viewport: Viewport,
    inspector: omuzaki_ui::panels::inspector::Inspector,
    runner: MockRunner<MemDoc>,
    actor: Actor,
}

/// Renders the Build workspace's edit panels at 1920×1080 and 1366×768 into the folder `OMUZAKI_EVIDENCE_OUT` names
/// (the `edit-panels` case of `evidence.toml`). Ignored in the gate: it writes files.
#[test]
#[ignore = "evidence: run by tools/ci/evidence.py with OMUZAKI_EVIDENCE_OUT set"]
fn edit_panels_evidence() {
    let Ok(out) = std::env::var("OMUZAKI_EVIDENCE_OUT") else { panic!("set OMUZAKI_EVIDENCE_OUT to a folder") };
    for (width, height) in [(1920.0, 1080.0), (1366.0, 768.0)] {
        omuzaki_char::link();
        omuzaki_char_mock::link();
        let state = create_render_state(default_wgpu_setup(), egui_wgpu::RendererOptions::default());
        let focus = EditFocus::new(mock());
        focus.select(omuzaki_char_api::PartId::new("head").unwrap_or_else(|_| panic!("an id")));
        let services = ViewportServices {
            gpu: Some(state.clone()),
            builder: Arc::new(omuzaki_char_api::RegistryBuilder),
            build: BuildServices::default(),
        };
        let build = Build {
            outliner: omuzaki_ui::panels::outliner::Outliner::new(focus.clone()),
            viewport: Viewport::new(focus.clone(), services),
            inspector: omuzaki_ui::panels::inspector::Inspector::new(focus),
            runner: MockRunner::new(MemDoc::from_value(json!({"characters": {"mock": node()}}))),
            actor: Actor::Person { name: "kit".to_owned() },
        };
        let mut h = Harness::builder()
            .with_size(vec2(width, height))
            .with_pixels_per_point(1.0)
            .renderer(WgpuTestRenderer::from_render_state(state))
            .build_ui_state(
                |ui, b: &mut Build| {
                    omuzaki_ui::ds::apply_settings(ui.ctx(), omuzaki_ui::ds::Settings::default());
                    let ds = omuzaki_ui::ds::ds(ui.ctx());
                    let full = ui.max_rect();
                    ui.painter().rect_filled(full, 0.0, ds.theme.bg0);
                    let io = Io::new(&b.runner, &b.actor, &[]);
                    let gap = 2.0;
                    let left = egui::Rect::from_min_size(full.min, vec2(280.0, full.height()));
                    let right = egui::Rect::from_min_max(egui::pos2(full.right() - 360.0, full.top()), full.max);
                    let centre = egui::Rect::from_min_max(
                        egui::pos2(left.right() + gap, full.top()),
                        egui::pos2(right.left() - gap, full.bottom()),
                    );
                    let mut dock = |rect: egui::Rect, draw: &mut dyn FnMut(&mut egui::Ui)| {
                        ui.painter().rect_filled(rect, 0.0, ds.theme.bg1);
                        let mut panel = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink(8.0)));
                        panel.set_clip_rect(rect);
                        draw(&mut panel);
                    };
                    dock(left, &mut |ui| b.outliner.ui(ui, io));
                    dock(centre, &mut |ui| b.viewport.ui(ui, io));
                    dock(right, &mut |ui| b.inspector.ui(ui, io));
                },
                build,
            );
        h.run_steps(4);
        let image = h.render().unwrap_or_else(|e| panic!("{e}"));
        let path = format!("{out}/build-{width}x{height}.png");
        image.save(&path).unwrap_or_else(|e| panic!("writing {path}: {e}"));
    }
}

#[test]
fn a_preview_shows_on_the_next_frame() {
    let Some(mut h) = rig() else { return };
    h.try_run().unwrap_or_else(|e| panic!("{e}"));
    let edit = |h: &mut Harness<'_, Rig>, value: f64| {
        let state = h.state();
        let args = json!({"character": "mock", "part": "vest", "path": "ease", "value": value});
        let request = omuzaki_ops_api::Request {
            op: "part.set_param".to_owned(),
            args,
            mode: omuzaki_ops_api::Mode::Apply,
            actor: state.actor.clone(),
            grants: Grants::person(),
            cause: None,
            tx: None,
            base: None,
        };
        state.runner.run(request).is_ok()
    };
    // A document that changes every frame (a slider dragged in the inspector): each edit is restaged and drawn by the
    // frame right after it, with no wait between, and the cost of that frame is what a drag pays per frame.
    let (mut frames, mut builds) = (Vec::new(), Vec::new());
    for i in 0..6 {
        let before = renders(&h);
        assert!(edit(&mut h, 0.001f64.mul_add(f64::from(i), 0.007)), "the mock garment takes an ease");
        let start = std::time::Instant::now();
        h.step();
        let built = start.elapsed().as_secs_f64() * 1000.0;
        render(&mut h);
        frames.push(start.elapsed().as_secs_f64() * 1000.0);
        builds.push(built);
        assert_eq!(renders(&h), before + 1, "edit {i} was drawn by the next frame");
    }
    frames.sort_by(f64::total_cmp);
    builds.sort_by(f64::total_cmp);
    eprintln!(
        "restage and draw on the frame after an edit: median {:.1} ms, worst {:.1} ms (UI frame with the character build {:.1} ms; the rest is the stage's render and the software readback)",
        frames[3], frames[5], builds[3]
    );
}

#[test]
fn the_gpu_side_is_reached_without_taking_a_lock() {
    let Some(h) = rig() else { return };
    // The renderer is a plain callback resource (no `Mutex` around it): reading it back takes egui's own lock only.
    let mut egui_renderer = h.state().gpu.renderer.write();
    let gpu = egui_renderer.callback_resources.get_mut::<Mutex<Gpu>>().map(|cell| cell.get_mut());
    assert!(
        gpu.is_some_and(|gpu| gpu.is_ok_and(|gpu| gpu.target().is_some())),
        "the viewport installed its GPU side, and it is reached without locking"
    );
}

#[test]
fn the_viewport_is_registered_and_passes_the_shells_conformance_check() {
    omuzaki_ui::link();
    let runner = MockRunner::new(MemDoc::new());
    let actor = omuzaki_ops_api::Actor::System;
    let ctx = omuzaki_ui_api::panel::PanelCtx::new(&runner, &actor);
    let host = omuzaki_ui_mock::MockHost::new();
    let problems = host.check("viewport", &ctx);
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn the_viewport_is_the_primary_panel_and_says_it_needs_a_gpu_when_the_shell_shares_none() {
    omuzaki_ui::link();
    let runner = MockRunner::new(MemDoc::new());
    let actor = omuzaki_ops_api::Actor::System;
    let mut views = omuzaki_ui_api::panel::ViewModels::new();
    views.insert(EditFocus::new(mock()));
    let ctx = omuzaki_ui_api::panel::PanelCtx::new(&runner, &actor).with_views(&views);
    let host = omuzaki_ui_mock::MockHost::new();
    let (def, mut panel) = host.open("viewport", &ctx).expect("the viewport is registered");
    assert_eq!(def.kind, omuzaki_ui_api::registry::PanelKind::Primary);
    let frame = host.frame(panel.as_mut(), def, &ctx, [640.0, 480.0]);
    assert!(frame.body, "a character is open, so the panel draws its body");
    assert!(frame.labels.iter().any(|t| t.contains("needs a GPU")), "{:?}", frame.labels);
}
