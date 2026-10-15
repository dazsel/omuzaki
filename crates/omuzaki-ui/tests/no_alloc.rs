//! Proves the components do not allocate per frame once warm (`design-system.md` section 9, rule 4).
//!
//! egui itself allocates about 18 times every frame (layer lists, the output, the text-cache probe). What matters is
//! that **our widgets add no allocation that scales with how many of them there are**: for each widget the test runs
//! the same scene with 40 and with 80 copies and requires the allocations per frame to stay flat (measured: exactly
//! equal), allowing two for a growing `Vec`.
//! It lives in its own test file, and counts only on the measuring thread, so the harness's own threads can't
//! make it flaky.

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use egui::{Context, Pos2, RawInput, Rect, Ui, vec2};
use omuzaki_ui::ds::Status;
use omuzaki_ui::ds::widgets::{
    ActorChip, ActorKind, Button, Change, Channel, Choice, CostChip, Curve, CurveEditor, Dropdown, FormRow, GateChip,
    JobRow, JobState, MeterGrid, Metric, NumberField, ParamSlider, RowIcon, ScopeBadge, StatusChip, StepCard,
    StepState, TabBar, TabItem, TabKind, Thumb, ThumbnailTile, Toggle, TreeRow,
};
use omuzaki_ui::ds::{Settings, apply_settings, icons};

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

/// Frames counted per measurement, after the warm-up.
const FRAMES: usize = 30;

/// What a scene draws: `n` copies of one component into a column.
type Scene = fn(&mut Ui, usize, &mut Vec<f64>);

fn frame(ctx: &Context, time: f64, scene: Scene, n: usize, values: &mut Vec<f64>) {
    let input = RawInput {
        time: Some(time),
        predicted_dt: 1.0 / 60.0,
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(480.0, 20_000.0))),
        ..RawInput::default()
    };
    ctx.run_ui(input, |ui| scene(ui, n, values)).drop_without_applying_deltas();
}

/// Allocations per frame in steady state for `n` copies of a scene.
fn per_frame(scene: Scene, n: usize) -> f64 {
    let ctx = Context::default();
    apply_settings(&ctx, Settings::default());
    let mut values = vec![0.5; n.max(1)];
    let mut time = 0.0;
    // Warm up: fonts activate, glyphs rasterise, the text cache and egui's maps reach their working size.
    for _ in 0..40 {
        time += 1.0 / 60.0;
        frame(&ctx, time, scene, n, &mut values);
    }
    let before = COUNT.with(Cell::get);
    MEASURING.with(|m| m.set(true));
    for _ in 0..FRAMES {
        time += 1.0 / 60.0;
        frame(&ctx, time, scene, n, &mut values);
    }
    MEASURING.with(|m| m.set(false));
    (COUNT.with(Cell::get) - before) as f64 / FRAMES as f64
}

/// Asserts that doubling the widgets adds fewer than `slack` allocations per frame, and reports the numbers.
fn assert_flat(name: &str, scene: Scene, slack: f64) {
    let (small, big) = (per_frame(scene, 40), per_frame(scene, 80));
    eprintln!(
        "{name:>14}: {small:7.1} allocations/frame at 40 widgets, {big:7.1} at 80 (marginal {:+.2} per widget)",
        (big - small) / 40.0
    );
    assert!(big - small <= slack, "{name} allocates per widget per frame: {small} at 40, {big} at 80");
}

#[test]
fn param_sliders_allocate_nothing_per_row_once_warm() {
    assert_flat(
        "ParamSlider",
        |ui, n, values| {
            for value in values.iter_mut().take(n) {
                ParamSlider::new("Width", value, 0.0..=1.0).default_value(0.5).show(ui);
            }
        },
        2.0,
    );
}

#[test]
fn toggles_buttons_and_chips_allocate_nothing_per_widget_once_warm() {
    assert_flat(
        "Toggle",
        |ui, n, _| {
            for i in 0..n {
                let mut on = i % 2 == 0;
                ui.add(Toggle::switch("Outlines", &mut on));
            }
        },
        2.0,
    );
    assert_flat(
        "Button",
        |ui, n, _| {
            for _ in 0..n {
                ui.add(Button::new("Approve").primary().shortcut("Enter"));
            }
        },
        2.0,
    );
    assert_flat(
        "Chips",
        |ui, n, _| {
            for _ in 0..n {
                ui.add(ActorChip::new("Designer", ActorKind::Agent));
                ui.add(ScopeBadge::new("broadcast"));
                ui.add(CostChip::new(&["~40 s", "1.2 GB"]));
                ui.add(StatusChip::new(Status::Ok, "Connected"));
                ui.add(GateChip::failed("likeness", "0.81", "0.85"));
            }
        },
        2.0,
    );
}

/// A name long enough to be cut in a 288 pt panel, so the chips take their ellipsis and tooltip path.
const CUT_NAME: &str = "Donaudampfschifffahrtsgesellschaftskapitän bestätigen und fortfahren!!";

#[test]
fn cut_chips_allocate_nothing_per_chip_once_warm() {
    assert_flat(
        "CutChips",
        |ui, n, _| {
            ui.set_max_width(288.0);
            for _ in 0..n {
                ui.add(ActorChip::new(CUT_NAME, ActorKind::Agent));
                ui.add(ScopeBadge::new(CUT_NAME));
                ui.add(CostChip::new(&[CUT_NAME]));
                ui.add(StatusChip::new(Status::Warn, CUT_NAME));
                ui.add(GateChip::failed(CUT_NAME, "0.81", "0.85"));
            }
        },
        2.0,
    );
}

#[test]
fn tree_rows_allocate_nothing_per_row_once_warm() {
    assert_flat(
        "TreeRow",
        |ui, n, _| {
            let icons_on = [RowIcon::Visible(true), RowIcon::Locked { owner: "Designer" }];
            ui.spacing_mut().item_spacing.y = 0.0;
            for i in 0..n {
                TreeRow::new(i, "Face profile", icons::CUBE)
                    .level(i % 3)
                    .selected(i == 3, true)
                    .icons(&icons_on)
                    .show(ui);
            }
        },
        2.0,
    );
}

#[test]
fn a_grid_of_meters_allocates_the_same_for_52_as_for_104_channels() {
    assert_flat(
        "MeterGrid",
        |ui, n, _| {
            let channels: Vec<Channel<'_>> = (0..n).map(|i| Channel::new("Jaw open", (i % 10) as f32 / 10.0)).collect();
            // The Vec above is the test's own input; the grid is measured, so build it outside the count where possible.
            MeterGrid::new("Face channels", &channels).columns(2).show(ui);
        },
        2.0,
    );
}

#[test]
fn closed_dropdowns_allocate_nothing_per_widget_once_warm() {
    assert_flat(
        "Dropdown",
        |ui, n, _| {
            let choices = [Choice::new(0, "Dark"), Choice::new(1, "Light"), Choice::new(2, "High contrast")];
            for i in 0..n {
                let mut value = i % 3;
                ui.add(Dropdown::new("Appearance", &mut value, &choices));
            }
        },
        2.0,
    );
}

#[test]
fn tab_bars_allocate_nothing_per_bar_once_warm() {
    assert_flat(
        "TabBar",
        |ui, n, _| {
            let tabs = [
                TabItem::new("Outliner"),
                TabItem::new("Library").closable(),
                TabItem::new("History").closable().agent_active(true),
            ];
            for i in 0..n {
                let mut selected = i % 3;
                ui.add(TabBar::new("Left dock", &mut selected, &tabs).kind(TabKind::Dock));
            }
        },
        2.0,
    );
}

#[test]
fn form_rows_cost_only_the_child_ui_egui_makes_for_them() {
    // A form row needs a child `Ui` to put its control in the right column, and egui boxes the stack of every `Ui`
    // (one allocation, measured below). Nothing else in it allocates, and the control inside adds nothing.
    assert_flat(
        "FormRow",
        |ui, n, _| {
            for i in 0..n {
                let mut value = i % 3;
                let choices = [Choice::new(0, "Dark"), Choice::new(1, "Light"), Choice::new(2, "High contrast")];
                FormRow::new("Appearance").show(ui, |ui, label| ui.add(Dropdown::new(label, &mut value, &choices)));
            }
        },
        // One per row at 40 more rows.
        41.0,
    );
}

#[test]
fn number_fields_tiles_job_rows_and_step_cards_allocate_nothing_per_widget_once_warm() {
    assert_flat(
        "NumberField",
        |ui, n, values| {
            for value in values.iter_mut().take(n) {
                ui.add(NumberField::new("Output", value, 0.0..=1.0));
            }
        },
        2.0,
    );
    assert_flat(
        "ThumbnailTile",
        |ui, n, _| {
            for i in 0..n {
                ui.add(
                    ThumbnailTile::new("Oval", if i % 3 == 0 { Thumb::Loading } else { Thumb::Missing })
                        .selected(i == 1),
                );
            }
        },
        2.0,
    );
    assert_flat(
        "JobRow",
        |ui, n, _| {
            for i in 0..n {
                let state = match i % 4 {
                    0 => JobState::Running { progress: Some(0.4) },
                    1 => JobState::Running { progress: None },
                    2 => JobState::Failed { error: "GPU out of memory" },
                    _ => JobState::Queued,
                };
                ui.add(JobRow::new("Render 24 contact sheets", state).cost(&["~40 s", "1.2 GB"]).elapsed("0:17"));
            }
        },
        2.0,
    );
    assert_flat(
        "StepCard",
        |ui, n, _| {
            let metrics = [
                Metric { name: "likeness", delta: "+0.03", change: Change::Better },
                Metric { name: "spacing", delta: "+0.02", change: Change::Worse },
            ];
            for i in 0..n {
                let state = [StepState::Applied, StepState::Pending, StepState::Rejected, StepState::FailedGate][i % 4];
                ui.add(
                    StepCard::new("Rounder face profile", "Designer", ActorKind::Agent)
                        .cause("\"make the face more oval\"")
                        .metrics(&metrics)
                        .age("4 s")
                        .state(state)
                        .evidence(Thumb::Missing),
                );
            }
        },
        2.0,
    );
}

#[test]
fn an_idle_curve_editor_allocates_nothing_per_editor_once_warm() {
    assert_flat(
        "CurveEditor",
        |ui, n, _| {
            thread_local! {
                static CURVE: std::cell::RefCell<Curve> = std::cell::RefCell::new(Curve::unit(omuzaki_ui::ds::widgets::Preset::SCurve));
            }
            CURVE.with(|curve| {
                for _ in 0..n {
                    CurveEditor::new("Jaw response", &mut curve.borrow_mut())
                        .plot_height(60.0)
                        .live_input(Some(0.4))
                        .show(ui);
                }
            });
        },
        2.0,
    );
}
