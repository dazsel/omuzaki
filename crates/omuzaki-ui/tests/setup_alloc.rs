//! What the first-run panel allocates per frame once it is idle (#1184), counted on the thread that draws.
//!
//! egui allocates on its own every frame (layer lists, the output, the text-cache probe), and every word the panel
//! draws comes from the message table as a new string, so the panel's count is not zero. What it must not do is grow:
//! the test draws an idle step for a warm-up, then counts the panel's frames against empty frames of the same context
//! and requires the difference to stay under a ceiling and the same from the first measured frames to the last.
//! Counted on the measuring thread only, so the harness's own threads can't make it flaky.

// A counting allocator is the only way to *prove* an allocation count; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use egui::{Context, Pos2, RawInput, Rect, vec2};
use omuzaki_ops_api::{Actor, Runner};
use omuzaki_setup::{
    Cameras, CharacterChoice, Current, Effect, Event, IPhone, InputChoice, LanguageOffer, OutputKind, Permission,
    Probes, Progress, Step, TrackerModel,
};
use omuzaki_stage::perform::Tracking;
use omuzaki_ui::ds::{Settings, apply_settings};
use omuzaki_ui::panels::setup::{FirstRun, HostStatus, SetupHost, SetupPanel, Unavailable};
use omuzaki_ui::shell::requests::ShellRequests;
use omuzaki_ui::shell::stub::NullRunner;
use omuzaki_ui_api::panel::{Panel as _, PanelCtx, ViewModels};

struct Counting;

thread_local! {
    // `const` init with no destructor: reading or writing them never allocates, so they are safe inside the
    // allocator. Both are per thread, so tests running in parallel can't count each other's allocations.
    static MEASURING: Cell<bool> = const { Cell::new(false) };
    static COUNT: Cell<usize> = const { Cell::new(0) };
}

// SAFETY: every method forwards to `System` unchanged; the counter is the only addition.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if MEASURING.try_with(Cell::get).unwrap_or(false) {
            let _ = COUNT.try_with(|c| c.set(c.get() + 1));
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// The most a warm, idle step may allocate per frame beyond an empty frame. Every drawn word is a string from the
/// message table and every card's description is laid out again. Measured: 49 (Welcome) to 152 (Input).
const CEILING: f64 = 250.0;

/// Frames per measurement, after the warm-up.
const FRAMES: usize = 60;

/// A host that answers every request at once and changes nothing on its own: the panel is idle.
struct Still;

impl SetupHost for Still {
    fn refresh(&mut self, _: &dyn Runner, _: &Actor) {}

    fn probes(&self) -> Probes {
        Probes {
            cameras: Cameras::One(Permission::Granted),
            tracker: TrackerModel::Ready,
            iphone: IPhone::NotPaired,
            recommended_output: Some(OutputKind::WindowCapture),
            assistant: false,
            language: LanguageOffer::None,
            project_open: false,
            privacy_claim_true: true,
        }
    }

    fn current(&self) -> Current {
        Current { input: Some(InputChoice::Webcam), output: None }
    }

    fn status(&self) -> HostStatus {
        HostStatus { camera: Some("Desk camera".into()), tracking: Some(Tracking::Tracking), ..HostStatus::default() }
    }

    fn character(&self, _: CharacterChoice) -> Result<(), Unavailable> {
        Ok(())
    }

    fn pick_camera(&mut self, _: &dyn Runner, _: &Actor, _: usize) {}

    fn stage_import(&mut self, _: String) {}

    fn run_effect(&mut self, _: &dyn Runner, _: &Actor, effect: &Effect) -> Vec<Event> {
        match *effect {
            Effect::SelectInput { request, .. }
            | Effect::ShowTestPattern { request, .. }
            | Effect::ApplyCharacter { request, .. } => vec![Event::Applied { request }],
            _ => Vec::new(),
        }
    }

    fn poll(&mut self) -> Vec<Event> {
        Vec::new()
    }
}

/// One frame of `ctx` at `time`, drawing the panel when there is one.
fn frame(ctx: &Context, time: f64, mut panel: Option<(&mut SetupPanel, &ViewModels)>) {
    let input = RawInput {
        time: Some(time),
        predicted_dt: 1.0 / 60.0,
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(320.0, 720.0))),
        ..RawInput::default()
    };
    let actor = Actor::Person { name: "kit".into() };
    ctx.run_ui(input, |ui| {
        if let Some((panel, views)) = panel.as_mut() {
            let pctx = PanelCtx::new(&NullRunner, &actor).with_views(views);
            panel.ui(ui, &pctx);
        }
    })
    .drop_without_applying_deltas();
}

/// Allocations of each of `FRAMES` frames.
fn count(ctx: &Context, time: &mut f64, mut panel: Option<(&mut SetupPanel, &ViewModels)>) -> Vec<usize> {
    let mut counts = Vec::with_capacity(FRAMES);
    for _ in 0..FRAMES {
        *time += 1.0 / 60.0;
        let before = COUNT.with(Cell::get);
        MEASURING.with(|m| m.set(true));
        frame(ctx, *time, panel.as_mut().map(|(p, v)| (&mut **p, *v)));
        MEASURING.with(|m| m.set(false));
        counts.push(COUNT.with(Cell::get) - before);
    }
    counts
}

fn mean(counts: &[usize]) -> f64 {
    counts.iter().sum::<usize>() as f64 / counts.len() as f64
}

#[test]
fn an_idle_setup_step_allocates_a_bounded_steady_amount_per_frame() {
    let ctx = Context::default();
    apply_settings(&ctx, Settings::default());
    let mut time = 0.0;
    for _ in 0..30 {
        time += 1.0 / 60.0;
        frame(&ctx, time, None);
    }
    let empty = mean(&count(&ctx, &mut time, None));
    for step in [Step::Welcome, Step::Character, Step::Input, Step::Output, Step::GoLive] {
        // A saved run resumes at its step, as after a quit.
        let folder = tempfile::tempdir().unwrap();
        let first_run = FirstRun::at(folder.path().join("setup-done"));
        first_run.save(&Progress { step, ..Progress::default() });
        let mut views = ViewModels::new();
        views.insert(ShellRequests::default());
        views.insert(first_run);
        let mut panel = SetupPanel::with_host(Box::new(Still));
        // Warm up: fonts activate, glyphs rasterise, the text cache and egui's maps reach their working size, and the
        // panel reads its host (every quarter second).
        for _ in 0..90 {
            time += 1.0 / 60.0;
            frame(&ctx, time, Some((&mut panel, &views)));
        }
        assert_eq!(panel.step(), step, "the panel resumed at the saved step");
        let first = mean(&count(&ctx, &mut time, Some((&mut panel, &views)))) - empty;
        let last = mean(&count(&ctx, &mut time, Some((&mut panel, &views)))) - empty;
        eprintln!("{step:?}: {first:.1} then {last:.1} allocations per idle frame beyond an empty frame");
        assert!(last <= CEILING, "{step:?} made {last:.1} allocations per idle frame (ceiling {CEILING})");
        assert!(last <= first + 2.0, "{step:?}: the count grew from {first:.1} to {last:.1} while idle");
    }
}
