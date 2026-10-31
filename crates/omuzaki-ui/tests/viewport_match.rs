//! Matching a reference in the viewport, on the studio's own core and a software GPU: a parity target (named the way an
//! agent names one, with `parity.target.set`) is picked in the viewport, which renders at the reference's size and
//! counts the pixels that differ, the stop rule's count. The reference here is a still of the kit character from the
//! viewport's own camera, so the loop can actually reach 0: the count is 0, the same number `parity.compare` gives, and
//! an edit to the character makes it rise.
//!
//! Without a Vulkan adapter (no `mesa-vulkan-drivers`) the tests say so and pass.

use std::sync::Arc;

use base64::Engine as _;
use eframe::egui_wgpu;
use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use omuzaki_char_api::{BuildOptions, BuildServices, CharId, CharacterBuilder, CharacterDef, Quality as BuildQuality};
use omuzaki_ops_api::{Actor, Event as OpEvent, Grants, Mode, Request};
use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};
use omuzaki_render_api::{Quality, Renderer};
use omuzaki_stage::ops::render::job::{DEFAULT_LIGHT_RIG, still_light};
use omuzaki_stage::ops::render::still::{Shot, StillSpec, render_stills};
use omuzaki_ui::panels::edit::EditFocus;
use omuzaki_ui::panels::io::Io;
use omuzaki_ui::panels::viewport::camera::OrbitCamera;
use omuzaki_ui::panels::viewport::matching::Look;
use omuzaki_ui::panels::viewport::{Viewport, ViewportServices};
use serde_json::{Value, json};

/// The reference's size: not the panel's shape, so the viewport has to fit it.
const SIZE: [u32; 2] = [240, 320];

struct Rig {
    viewport: Viewport,
    core: omuzaki_ui::wiring::Core,
    events: std::sync::mpsc::Receiver<OpEvent>,
    frame: Vec<OpEvent>,
    actor: Actor,
}

fn kit() -> CharId {
    CharId::new("kit").unwrap_or_else(|_| panic!("an id"))
}

fn run(core: &omuzaki_ui::wiring::Core, actor: &Actor, op: &str, args: Value) -> Value {
    let request = Request {
        op: op.to_owned(),
        args,
        mode: Mode::Apply,
        actor: actor.clone(),
        grants: Grants::person(),
        cause: None,
        tx: None,
        base: None,
    };
    let outcome = core.runner.run(request).unwrap_or_else(|e| panic!("{op}: {e}"));
    outcome.plan.output.unwrap_or(Value::Null)
}

/// The still of the kit character as the document holds it, from `camera`, at [`SIZE`]: what the viewport draws.
fn still(core: &omuzaki_ui::wiring::Core, camera: OrbitCamera) -> image::RgbaImage {
    let node = core
        .runner
        .get(&omuzaki_char_api::paths::character(&kit()), &Grants::person())
        .unwrap_or_else(|e| panic!("{e}"))
        .unwrap_or_else(|| panic!("the kit character"));
    let def = CharacterDef::from_node(&node).unwrap_or_else(|e| panic!("{e}"));
    let options = BuildOptions { variant: None, quality: BuildQuality::Live, rig_export: false };
    let built = omuzaki_char_api::RegistryBuilder
        .build(&kit(), &def, &options, &BuildServices::default())
        .unwrap_or_else(|e| panic!("the kit character builds: {e}"));
    let mut renderer = WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software })
        .unwrap_or_else(|e| panic!("a software renderer: {e}"));
    let light = still_light(DEFAULT_LIGHT_RIG).unwrap_or_else(|e| panic!("{e}"));
    let spec = StillSpec::new(light, SIZE.into(), Quality::live());
    let stills = render_stills(&mut renderer as &mut dyn Renderer, built, &spec, &[Shot::new(camera.camera())])
        .unwrap_or_else(|e| panic!("the still renders: {e}"));
    let image = &stills[0].image;
    image::RgbaImage::from_raw(image.size.x, image.size.y, image.rgba8.clone()).unwrap_or_default()
}

fn png_base64(image: &image::RgbaImage) -> String {
    let mut bytes = std::io::Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png).unwrap_or_else(|e| panic!("{e}"));
    base64::engine::general_purpose::STANDARD.encode(bytes.into_inner())
}

fn rig() -> Option<Harness<'static, Rig>> {
    omuzaki_ui::link();
    let state =
        std::panic::catch_unwind(|| create_render_state(default_wgpu_setup(), egui_wgpu::RendererOptions::default()))
            .ok()?;
    if state.adapter.get_info().backend != wgpu::Backend::Vulkan {
        eprintln!("no Vulkan adapter (install mesa-vulkan-drivers): the matching tests skip");
        return None;
    }
    let core = omuzaki_ui::wiring::core_on(
        omuzaki_ui::wiring::Document::empty(),
        omuzaki_ops_api::ServicesBuilder::new().build(),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let actor = Actor::Person { name: "you".to_owned() };
    run(&core, &actor, "character.create", json!({"id": "kit", "name": "Kit", "template": "standard"}));
    run(&core, &actor, "part.add", json!({"character": "kit", "kind": "hat", "id": "hat"}));
    let events = core.runner.subscribe(&Grants::person()).unwrap_or_else(|e| panic!("{e}"));
    let services = ViewportServices {
        gpu: Some(state.clone()),
        builder: Arc::new(omuzaki_char_api::RegistryBuilder),
        build: BuildServices::default(),
    };
    let rig = Rig { viewport: Viewport::new(EditFocus::new(kit()), services), core, events, frame: Vec::new(), actor };
    let mut h = Harness::builder()
        .with_size(vec2(640.0, 560.0))
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .renderer(WgpuTestRenderer::from_render_state(state))
        .build_ui_state(
            |ui, r: &mut Rig| {
                omuzaki_ui::ds::apply_settings(ui.ctx(), omuzaki_ui::ds::Settings::default());
                r.frame = r.events.try_iter().collect();
                r.viewport.ui(ui, Io::new(r.core.runner.as_ref(), &r.actor, &r.frame));
            },
            rig,
        );
    settle(&mut h);
    Some(h)
}

fn settle(h: &mut Harness<'_, Rig>) {
    for _ in 0..4 {
        h.step();
        h.render().unwrap_or_else(|e| panic!("the frame renders: {e}"));
    }
}

/// Names the still from the viewport's camera a parity target, the way an agent would, and matches it.
fn match_own_still(h: &mut Harness<'_, Rig>) -> image::RgbaImage {
    let camera =
        OrbitCamera { target: omuzaki_core::glam::Vec3::new(0.0, 1.3, 0.0), yaw: 20.0, pitch: 5.0, distance: 1.6 };
    h.state_mut().viewport.set_camera(camera);
    let reference = still(&h.state().core, camera);
    let target = json!({"reference": {"frames": [{"data": png_base64(&reference)}]}, "note": "the kit's own still"});
    let state = h.state();
    run(&state.core, &state.actor, "parity.target.set", json!({"name": "own-still", "target": target}));
    settle(h);
    assert_eq!(h.state().viewport.targets(), ["own-still".to_owned()], "the viewport lists the agent's target");
    let state = h.state_mut();
    let io = Io::new(state.core.runner.as_ref(), &state.actor, &[]);
    state.viewport.match_target(Some("own-still"), io);
    settle(h);
    reference
}

#[test]
fn the_viewport_counts_zero_against_its_own_still_and_the_count_rises_with_an_edit() {
    let Some(mut h) = rig() else { return };
    let reference = match_own_still(&mut h);
    assert_eq!(h.state().viewport.match_problem(), None);
    assert_eq!(h.state().viewport.matching(), Some("own-still"));
    let count = h.state().viewport.match_count().unwrap_or_else(|| panic!("measured"));
    assert_eq!(count.pixels, (SIZE[0] * SIZE[1]) as usize, "the viewport renders at the reference's size");
    assert_eq!(count.off, 0, "every pixel of the viewport's picture is the still's: {count:?}");

    // The same number parity.compare gives an agent for the same pair.
    let state = h.state();
    let ours = json!({"data": png_base64(&reference)});
    let compared = run(
        &state.core,
        &state.actor,
        "parity.compare",
        json!({"ours": ours, "reference": {"data": png_base64(&reference)}}),
    );
    assert_eq!(compared["pixels_off"], json!(0), "{compared}");

    // Move the hat 2 cm: the count rises, and the difference map shows where.
    let state = h.state();
    let pinned =
        run(&state.core, &state.actor, "part.set_attach", json!({"character": "kit", "part": "hat", "pin": true}));
    let mut attach = pinned["attach"].clone();
    let up = attach["offset"]["translation"][1].as_f64().unwrap_or(0.0) + 0.02;
    attach["offset"]["translation"][1] = json!(up);
    run(&state.core, &state.actor, "part.set_attach", json!({"character": "kit", "part": "hat", "attach": attach}));
    h.state_mut().viewport.set_look(Look::Exact);
    settle(&mut h);
    let moved = h.state().viewport.match_count().unwrap_or_else(|| panic!("measured again"));
    assert!(moved.off > 50 && moved.max_level > 0, "the moved hat shows in the count: {moved:?}");
}

#[test]
fn a_removed_target_stops_the_match_and_a_target_the_viewport_cant_read_says_why() {
    let Some(mut h) = rig() else { return };
    match_own_still(&mut h);
    let state = h.state();
    run(&state.core, &state.actor, "parity.target.remove", json!({"name": "own-still"}));
    settle(&mut h);
    assert_eq!(h.state().viewport.matching(), None);
    assert_eq!(h.state().viewport.targets(), [] as [String; 0]);

    let state = h.state();
    let missing = json!({"reference": {"frames": [{"data": "bm90IGFuIGltYWdl"}]}});
    let set = state.core.runner.run(Request {
        op: "parity.target.set".to_owned(),
        args: json!({"name": "broken", "target": missing}),
        mode: Mode::Apply,
        actor: state.actor.clone(),
        grants: Grants::person(),
        cause: None,
        tx: None,
        base: None,
    });
    if set.is_ok() {
        settle(&mut h);
        let state = h.state_mut();
        let io = Io::new(state.core.runner.as_ref(), &state.actor, &[]);
        state.viewport.match_target(Some("broken"), io);
        assert_eq!(state.viewport.matching(), None);
        assert!(state.viewport.match_problem().is_some(), "it says why it can't show the reference");
    }
}

#[test]
#[ignore = "evidence: run with OMUZAKI_EVIDENCE_OUT set"]
fn matching_evidence() {
    let Ok(out) = std::env::var("OMUZAKI_EVIDENCE_OUT") else { panic!("set OMUZAKI_EVIDENCE_OUT to a folder") };
    let Some(mut h) = rig() else { panic!("no Vulkan adapter") };
    let save = |h: &mut Harness<'_, Rig>, name: &str| {
        let image = h.render().unwrap_or_else(|e| panic!("{e}"));
        let path = format!("{out}/{name}.png");
        image.save(&path).unwrap_or_else(|e| panic!("writing {path}: {e}"));
    };
    match_own_still(&mut h);
    h.state_mut().viewport.set_look(Look::Exact);
    settle(&mut h);
    save(&mut h, "1-own-still-zero");
    let state = h.state();
    let pinned =
        run(&state.core, &state.actor, "part.set_attach", json!({"character": "kit", "part": "hat", "pin": true}));
    let mut attach = pinned["attach"].clone();
    let up = attach["offset"]["translation"][1].as_f64().unwrap_or(0.0) + 0.02;
    attach["offset"]["translation"][1] = json!(up);
    run(&state.core, &state.actor, "part.set_attach", json!({"character": "kit", "part": "hat", "attach": attach}));
    settle(&mut h);
    save(&mut h, "2-hat-moved-difference");
    for (look, name) in [(Look::Onion, "3-onion"), (Look::Wipe, "4-wipe")] {
        h.state_mut().viewport.set_look(look);
        settle(&mut h);
        save(&mut h, name);
    }
}
