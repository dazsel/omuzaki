//! Selecting and placing in the viewport (#2590), on the studio's own core and a software GPU: a click on a part
//! selects it, and a drag of a move handle places it through `part.set_attach` (pinning it first, since the kit put it
//! there by itself), as one undo step named for the drag.
//!
//! Without a Vulkan adapter (no `mesa-vulkan-drivers`) the tests say so and pass.

use std::sync::Arc;

use eframe::egui_wgpu;
use egui::{Event, Modifiers, PointerButton, Pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use omuzaki_char_api::{BuildServices, CharId, PartId};
use omuzaki_ops_api::{Actor, Event as OpEvent, Grants, Mode, Request};
use omuzaki_ui::panels::edit::EditFocus;
use omuzaki_ui::panels::io::Io;
use omuzaki_ui::panels::viewport::gizmo::Tool;
use omuzaki_ui::panels::viewport::{Viewport, ViewportServices};
use serde_json::{Value, json};

struct Rig {
    viewport: Viewport,
    focus: EditFocus,
    core: omuzaki_ui::wiring::Core,
    events: std::sync::mpsc::Receiver<OpEvent>,
    frame: Vec<OpEvent>,
    actor: Actor,
}

fn id(s: &str) -> PartId {
    PartId::new(s).unwrap_or_else(|_| panic!("an id"))
}

fn kit() -> CharId {
    CharId::new("kit").unwrap_or_else(|_| panic!("an id"))
}

/// Runs `op` as the person, outside the viewport (the test's own setup).
fn run(core: &omuzaki_ui::wiring::Core, actor: &Actor, op: &str, args: Value) {
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
    core.runner.run(request).unwrap_or_else(|e| panic!("{op}: {e}"));
}

/// The kit character with a hat, in a viewport of `size` on the studio's core, or `None` without a Vulkan adapter.
fn rig(size: egui::Vec2) -> Option<Harness<'static, Rig>> {
    omuzaki_ui::link();
    let state =
        std::panic::catch_unwind(|| create_render_state(default_wgpu_setup(), egui_wgpu::RendererOptions::default()))
            .ok()?;
    if state.adapter.get_info().backend != wgpu::Backend::Vulkan {
        eprintln!("no Vulkan adapter (install mesa-vulkan-drivers): the placing tests skip");
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
    let focus = EditFocus::new(kit());
    let rig = Rig { viewport: Viewport::new(focus.clone(), services), focus, core, events, frame: Vec::new(), actor };
    let mut h = Harness::builder()
        .with_size(size)
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

/// A few frames, each rendered, so the callback stages what changed and the picking mesh follows.
fn settle(h: &mut Harness<'_, Rig>) {
    for _ in 0..3 {
        h.step();
        h.render().unwrap_or_else(|e| panic!("the frame renders: {e}"));
    }
}

/// A point on screen where the hat shows (it sits behind the fringe, so its middle may be hair): the first point of a
/// grid over its box whose ray hits the hat.
fn hat_on_screen(h: &Harness<'_, Rig>) -> Pos2 {
    let viewport = &h.state().viewport;
    let rect = viewport.rect().unwrap_or_else(|| panic!("drawn"));
    let bounds = viewport.part_bounds(&id("hat")).unwrap_or_else(|| panic!("the hat has a mesh"));
    let [x0, y0, x1, y1] = viewport
        .camera()
        .project_bounds(bounds, [rect.width(), rect.height()])
        .unwrap_or_else(|| panic!("the hat is in view"));
    for j in 0..=12 {
        for i in 0..=12 {
            #[allow(clippy::cast_precision_loss, reason = "a small grid")]
            let at = vec2(x0 + (x1 - x0) * i as f32 / 12.0, y0 + (y1 - y0) * j as f32 / 12.0);
            if viewport.part_at(at).is_some_and(|hit| hit.part == id("hat")) {
                return rect.min + at;
            }
        }
    }
    panic!("the hat shows nowhere in its box")
}

fn press(h: &mut Harness<'_, Rig>, at: Pos2, pressed: bool) {
    h.event(Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
}

fn hat_attach(h: &Harness<'_, Rig>) -> Option<Value> {
    let path = omuzaki_char_api::paths::part(&kit(), &id("hat")).join("attach");
    h.state().core.runner.get(&path, &Grants::person()).unwrap_or_else(|e| panic!("{e}"))
}

/// Drags the up arrow of the move handles up by `points`.
fn drag_up(h: &mut Harness<'_, Rig>, points: f32) {
    let screen = h.state().viewport.handles().unwrap_or_else(|| panic!("the selected hat has handles"));
    let rect = h.state().viewport.rect().unwrap_or_else(|| panic!("drawn"));
    let tip = screen.tips[1].unwrap_or_else(|| panic!("the up arrow is seen"));
    let start = rect.min + (screen.centre + (tip - screen.centre) * 0.6).to_vec2();
    h.event(Event::PointerMoved(start));
    h.step();
    press(h, start, true);
    h.step();
    for i in 1..=6 {
        #[allow(clippy::cast_precision_loss, reason = "six steps")]
        let at = start - vec2(0.0, points * i as f32 / 6.0);
        h.event(Event::PointerMoved(at));
        h.step();
    }
    press(h, start - vec2(0.0, points), false);
    settle(h);
}

#[test]
fn a_click_selects_the_part_under_the_pointer_and_a_click_on_nothing_clears() {
    let Some(mut h) = rig(vec2(640.0, 720.0)) else { return };
    let at = hat_on_screen(&h);
    h.event(Event::PointerMoved(at));
    press(&mut h, at, true);
    press(&mut h, at, false);
    settle(&mut h);
    assert_eq!(h.state().focus.selected(), vec![id("hat")]);
    let corner = h.state().viewport.rect().unwrap_or(egui::Rect::ZERO).min + vec2(6.0, 60.0);
    h.event(Event::PointerMoved(corner));
    press(&mut h, corner, true);
    press(&mut h, corner, false);
    settle(&mut h);
    assert!(h.state().focus.selected().is_empty(), "a click on nothing clears the selection");
}

#[test]
fn dragging_a_move_handle_places_the_part_as_one_undo_step() {
    let Some(mut h) = rig(vec2(640.0, 720.0)) else { return };
    h.state().focus.select(id("hat"));
    settle(&mut h);
    assert_eq!(h.state().viewport.tool(), Tool::Move);
    assert_eq!(hat_attach(&h), None, "the kit places the hat by itself");
    let before = h.state().viewport.part_bounds(&id("hat")).unwrap_or_else(|| panic!("a mesh")).centre();
    drag_up(&mut h, 40.0);
    assert_eq!(h.state().viewport.refused(), None);
    let attach = hat_attach(&h).unwrap_or_else(|| panic!("the drag wrote an attachment"));
    assert_eq!(attach["to"], "bone", "{attach}");
    let after = h.state().viewport.part_bounds(&id("hat")).unwrap_or_else(|| panic!("a mesh")).centre();
    assert!(after.y - before.y > 0.01, "the hat moved up: {before} to {after}");
    assert!((after.x - before.x).abs() < 0.005 && (after.z - before.z).abs() < 0.005, "only up: {before} to {after}");
    assert_eq!(h.state().focus.selected(), vec![id("hat")], "a handle drag doesn't change the selection");

    // One step: undo takes the move and the pin back together.
    let state = h.state();
    let undone = state.core.runner.undo(&state.actor, &Grants::person()).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(undone.as_deref(), Some("Move hat"));
    settle(&mut h);
    assert_eq!(hat_attach(&h), None, "undo gives the kit's own place back");
    let back = h.state().viewport.part_bounds(&id("hat")).unwrap_or_else(|| panic!("a mesh")).centre();
    assert!(back.distance(before) < 1e-4, "back where it was: {before} vs {back}");
}

#[test]
fn escape_puts_a_drag_back_and_a_part_that_cant_be_placed_has_no_handles() {
    let Some(mut h) = rig(vec2(640.0, 720.0)) else { return };
    h.state().focus.select(id("hair"));
    settle(&mut h);
    assert!(h.state().viewport.handles().is_none(), "hair, bent by its bones and simulated, has no handles");
    assert_eq!(h.state().viewport.refused(), None);
    let hair = omuzaki_char_api::paths::part(&kit(), &id("hair")).join("attach");
    assert!(h.state().core.runner.get(&hair, &Grants::person()).unwrap_or_else(|e| panic!("{e}")).is_none());

    h.state().focus.select(id("hat"));
    settle(&mut h);
    let screen = h.state().viewport.handles().unwrap_or_else(|| panic!("handles"));
    let rect = h.state().viewport.rect().unwrap_or_else(|| panic!("drawn"));
    let tip = screen.tips[1].unwrap_or_else(|| panic!("the up arrow"));
    let start = rect.min + (screen.centre + (tip - screen.centre) * 0.6).to_vec2();
    h.event(Event::PointerMoved(start));
    h.step();
    press(&mut h, start, true);
    h.step();
    h.event(Event::PointerMoved(start - vec2(0.0, 30.0)));
    h.step();
    assert!(hat_attach(&h).is_some(), "the drag shows as it goes");
    h.key_press(egui::Key::Escape);
    h.step();
    press(&mut h, start - vec2(0.0, 30.0), false);
    settle(&mut h);
    assert_eq!(hat_attach(&h), None, "Escape put the hat back, pin and all");
    // Neither drag left a step: what undo takes back is the setup's own last edit.
    let state = h.state();
    let undone = state.core.runner.undo(&state.actor, &Grants::person()).unwrap_or_else(|e| panic!("{e}"));
    assert!(undone.as_deref().is_some_and(|label| !label.contains("hat") || !label.starts_with("Move")), "{undone:?}");
}

/// Drags from `start` to `end` in six moves, with the primary button.
fn drag(h: &mut Harness<'_, Rig>, start: Pos2, end: Pos2) {
    h.event(Event::PointerMoved(start));
    h.step();
    press(h, start, true);
    h.step();
    for i in 1..=6 {
        #[allow(clippy::cast_precision_loss, reason = "six steps")]
        h.event(Event::PointerMoved(start + (end - start) * (i as f32 / 6.0)));
        h.step();
    }
    press(h, end, false);
    settle(h);
}

#[test]
#[ignore = "evidence: run with OMUZAKI_EVIDENCE_OUT set"]
fn placing_evidence() {
    let Ok(out) = std::env::var("OMUZAKI_EVIDENCE_OUT") else { panic!("set OMUZAKI_EVIDENCE_OUT to a folder") };
    let Some(mut h) = rig(vec2(900.0, 760.0)) else { panic!("no Vulkan adapter") };
    let save = |h: &mut Harness<'_, Rig>, name: &str| {
        let image = h.render().unwrap_or_else(|e| panic!("{e}"));
        let path = format!("{out}/{name}.png");
        image.save(&path).unwrap_or_else(|e| panic!("writing {path}: {e}"));
    };
    let at = hat_on_screen(&h);
    h.event(Event::PointerMoved(at));
    press(&mut h, at, true);
    press(&mut h, at, false);
    settle(&mut h);
    save(&mut h, "1-click-selects-the-hat");
    drag_up(&mut h, 40.0);
    save(&mut h, "2-moved-up");
    let mut camera = h.state().viewport.camera();
    camera.look_from(omuzaki_ui::panels::viewport::camera::View::ThreeQuarter);
    h.state_mut().viewport.set_camera(camera);
    h.state_mut().viewport.set_tool(Tool::Rotate);
    settle(&mut h);
    save(&mut h, "3-rotate-handles");
    let screen = h.state().viewport.handles().unwrap_or_else(|| panic!("handles"));
    let rect = h.state().viewport.rect().unwrap_or_else(|| panic!("drawn"));
    let ring = &screen.rings[2];
    let (from, to) = (ring[ring.len() / 4], ring[ring.len() / 4 + 4]);
    drag(&mut h, rect.min + from.to_vec2(), rect.min + to.to_vec2());
    save(&mut h, "4-tilted");
    h.state_mut().viewport.set_tool(Tool::Scale);
    settle(&mut h);
    let screen = h.state().viewport.handles().unwrap_or_else(|| panic!("handles"));
    let centre = rect.min + screen.centre.to_vec2();
    drag(&mut h, centre, centre + vec2(50.0, -20.0));
    save(&mut h, "5-scaled");
}
