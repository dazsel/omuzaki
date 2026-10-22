//! The Health panel (#1348) over the live-metrics snapshot: every reading from `MetricsReader`, an absent section
//! read as "not reporting" and never zero, dropped frames "n/a" until an output reports, every budget row marked
//! "live", each reading a Tab stop; the Live panel's health strip reads the same snapshot; the Perform workspace has the
//! panel in a tab beside the Channels.
//!
//! The evidence test renders the three states side by side on the software GPU (ignored; run by
//! hand or by the visual job).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use egui::{Key, Modifiers, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use omuzaki_core::time::Seconds;
use omuzaki_metrics_api::MetricsReader;
use omuzaki_metrics_api::sections::FrameMetrics;
use omuzaki_ops_api::Actor;
use omuzaki_render_api::{FrameReport, Pass, PassTimes};
use omuzaki_track_api::tracker::LatencyStats;
use omuzaki_ui::ds::Settings;
use omuzaki_ui::ds::testing::{all_strings, harness_state, press_for};
use omuzaki_ui::live::health::{self, HealthPanel, HealthPublisher, HealthSample, Rendered};
use omuzaki_ui::live::panel::LivePanel;

fn panel(reader: Option<MetricsReader>) -> Harness<'static, Option<MetricsReader>> {
    harness_state(Settings::default(), vec2(340.0, 640.0), reader, |ui, reader| {
        HealthPanel.ui(ui, reader.as_ref());
    })
}

fn says<S>(h: &Harness<'_, S>, words: &str) -> bool {
    all_strings(h).iter().any(|s| s == words)
}

#[test]
fn without_a_snapshot_the_panel_says_the_live_slice_is_not_running() {
    let h = panel(None);
    assert!(says(&h, "No live readings"), "{:#?}", all_strings(&h));
}

#[test]
fn an_absent_section_reads_not_reporting_never_zero() {
    let h = panel(Some(HealthSample::Idle.reader()));
    assert!(says(&h, "Frame rate: 60 fps"), "{:#?}", all_strings(&h));
    assert!(says(&h, "Frame time: 16.7 ms, worst 18 ms"));
    // The tracker has not published: its drops are not zero, they are unknown.
    assert!(says(&h, "Tracking drops: — not reporting"));
    assert!(says(&h, "Dropped frames: n/a"), "no output has reported");
    assert!(says(&h, "Latency: No face in use"));
    assert!(says(&h, "Audio underruns: 0"), "the audio engine reported none");
    assert!(says(&h, "No frames yet"));

    // Nothing at all published: every reading is unknown and there are no budget rows.
    let (_writers, reader) = omuzaki_metrics_api::metrics();
    let h = panel(Some(reader));
    assert!(says(&h, "Frame rate: — not reporting"));
    assert!(says(&h, "Audio underruns: — not reporting"));
    assert!(says(&h, "No budget readings yet"));
    assert!(says(&h, "Waiting for the performer"));
}

#[test]
fn every_budget_row_says_live_and_an_over_budget_row_says_so_in_words() {
    let h = panel(Some(HealthSample::OverBudget.reader()));
    for name in ["GPU frame", "Face paint", "Face tracking", "Voice effects", "Latency"] {
        let row = format!("{name} · live: Over budget:");
        assert!(all_strings(&h).iter().any(|s| s.starts_with(&row)), "{row}: {:#?}", all_strings(&h));
    }
    assert!(says(&h, "GPU frame · live: Over budget: 2.40 ms of 1.50 ms (160%)"), "{:#?}", all_strings(&h));
    assert!(all_strings(&h).iter().any(|s| s.starts_with("Latency · live") && s.contains("Partial")));
    assert!(says(&h, "Dropped frames: 18"));
    assert!(says(&h, "Late frames: 312"));
    assert!(says(&h, "Tracking drops: 44"));
    assert!(says(&h, "Audio underruns: 3"));
    assert!(says(&h, "Latency: 96 ms"));
}

#[test]
fn every_reading_is_a_tab_stop_in_order() {
    let mut h = panel(Some(HealthSample::TrackingLost.reader()));
    assert!(says(&h, "Face lost"));
    let order = [
        "Frame rate: 60 fps",
        "Frame time: 16.7 ms, worst 19 ms",
        "Latency: No face in use",
        "Late frames: 2",
        "Dropped frames: n/a",
        "Tracking drops: 3",
        "Audio underruns: 0",
    ];
    for label in order {
        press_for(&mut h, Modifiers::NONE, Key::Tab, 2);
        assert!(h.get_by_label(label).accesskit_node().is_focused(), "{label} has the focus after Tab");
    }
    press_for(&mut h, Modifiers::NONE, Key::Tab, 2);
    let focused = h.query_all_by_label_contains("· live").find(|n| n.accesskit_node().is_focused());
    assert!(focused.is_some(), "the first budget row is next");
}

#[test]
fn the_live_panels_health_strip_reads_the_snapshot() {
    omuzaki_ui::link();
    let runner = omuzaki_ui::shell::stub::NullRunner;
    let mut live = LivePanel::new(&runner);
    live.set_metrics(HealthSample::TrackingLost.reader());
    let h = Harness::builder().with_size(vec2(306.0, 400.0)).build_ui_state(
        |ui, (panel, runner): &mut (LivePanel, omuzaki_ui::shell::stub::NullRunner)| {
            omuzaki_ui::ds::apply_settings(ui.ctx(), Settings::default());
            panel.ui(ui, runner, &Actor::Person { name: "performer".into() });
        },
        (live, runner),
    );
    assert!(says(&h, "Face lost"), "the performer's state from the snapshot: {:#?}", all_strings(&h));
    assert!(says(&h, "29 fps"));
    assert!(says(&h, "Latency: —"), "no face in use");
}

#[test]
fn the_publisher_writes_what_the_panel_reads() {
    let (mut publisher, reader) = HealthPublisher::with_reader();
    let mut cpu = PassTimes::default();
    cpu.set(Pass::Opaque, Seconds(0.001));
    let report = FrameReport {
        frame: 1,
        render_size: omuzaki_core::glam::UVec2::new(1920, 1080),
        cpu,
        gpu: None,
        draw_calls: 12,
        triangles: 4000,
    };
    let mut t = 0.0;
    for _ in 0..30 {
        let rendered = Rendered { report, track_to_submit: Some(Duration::from_millis(41)) };
        publisher.frame(t, Some(rendered));
        t += 1.0 / 30.0;
    }
    let stats = LatencyStats::new(30, 1, Duration::from_millis(3), Duration::from_millis(4), Duration::from_millis(7));
    publisher.tracker(t, &stats);
    let live = reader.read();
    let frame: FrameMetrics = live.frame.expect("the stage drew");
    assert!(frame.gpu_ms.is_none(), "no GPU timing is never a zero GPU time");
    assert_eq!(frame.track_to_submit_ms.map(f32::round), Some(41.0));
    // Every frame at 30 Hz is late against the 60 Hz loop.
    assert_eq!(frame.late_frames, 29);
    let h = panel(Some(reader));
    assert!(says(&h, "Latency: 41 ms"), "{:#?}", all_strings(&h));
    assert!(says(&h, "Late frames: 29"));
    assert!(says(&h, "Tracking drops: 1"));
    assert!(all_strings(&h).iter().any(|s| s.starts_with("Face tracking · live: 4.0 ms of 5.0 ms (80%)")));
}

#[test]
fn the_perform_workspace_has_health_in_a_tab_beside_the_channels() {
    use omuzaki_ui_api::layout::Node;
    omuzaki_ui::link();
    let host = omuzaki_ui_mock::MockHost::new();
    let perform = host.registry().workspace("perform").expect("registered");
    let layout = (perform.default_layout)();
    let Node::Split { children, .. } = &layout else { panic!("a split") };
    assert!(children.contains(&Node::tabs(&["live-channels", "live-health", "input"])), "{layout:?}");
    assert!(host.registry().panel("live-health").is_some());
    assert!(host.registry().stories().iter().any(|story| story.id == "live-health"));
}

/// The gallery story: idle, over budget and tracking lost side by side, at 1366x768 and 1920x1080.
/// `OMUZAKI_EVIDENCE_OUT=<folder> cargo test -p omuzaki-ui --test live_health -- --ignored`.
#[test]
#[ignore = "renders on the GPU (lavapipe in CI) and writes evidence images"]
fn live_health_evidence() {
    let Ok(out) = std::env::var("OMUZAKI_EVIDENCE_OUT") else { panic!("set OMUZAKI_EVIDENCE_OUT to a folder") };
    let out = std::path::PathBuf::from(out);
    std::fs::create_dir_all(&out).unwrap();
    for (w, hgt) in [(1366.0, 768.0), (1920.0, 1080.0)] {
        let readers: Vec<_> = HealthSample::ALL.iter().map(|s| (*s, s.reader())).collect();
        let mut h = harness_state(Settings::default(), vec2(w, hgt), readers, |ui, readers| {
            ui.set_min_size(ui.available_size());
            // The three states side by side, each in a column as wide as the Perform workspace's right dock.
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 24.0;
                for (sample, reader) in readers.iter() {
                    ui.vertical(|ui| {
                        ui.set_width(320.0);
                        ui.label(egui::RichText::new(sample.title(ui.ctx())).strong());
                        ui.push_id(sample.slug(), |ui| health::show(ui, &reader.read()));
                    });
                }
            });
        });
        h.run_steps(3);
        h.render().unwrap().save(out.join(format!("live-health-{w}x{hgt}.png"))).unwrap();
    }
}
