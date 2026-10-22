//! The Health panel (#1348): how the live slice is doing right now, read from the live-metrics snapshot
//! (`omuzaki-metrics-api`; research note `2026-10-live-metrics-snapshot` §2, §3 and §5), and the [`HealthPublisher`]
//! the app's frame loop writes that snapshot with.
//!
//! The panel reads the typed [`MetricsReader`] the app puts in the shell's views. A section that has not published
//! reads "—" with the reason, never zero (§5); "Dropped frames" reads "n/a" until an output reports. Every budget row
//! carries the word "live": it is the value right now, not the §10 gate statistic (§2). Each reading is a stop on the
//! keyboard's Tab walk, so a screen reader or a keyboard user can reach every number.

use std::time::Duration;

use egui::{NumExt as _, RichText, Sense, Ui, WidgetInfo, WidgetType, vec2};
use omuzaki_audio_api::report::{AudioStatus, GraphReport, RouteEnd};
use omuzaki_core::time::Seconds;
use omuzaki_metrics_api::budget::{BudgetKey, BudgetUse};
use omuzaki_metrics_api::sections::{
    AudioMetrics, CalibrationPhase, FrameMetrics, PerformMetrics, TrackerMetrics, TrackingState,
};
use omuzaki_metrics_api::{LiveMetrics, MetricsReader, MetricsWriters, metrics};
use omuzaki_render_api::FrameReport;
use omuzaki_stage::perform::Tracking;
use omuzaki_stage::perform::host::{CalibrationState, PerformStatus};
use omuzaki_track_api::tracker::LatencyStats;

use super::text::{Word, say, text};
use crate::ds::theme::ds;
use crate::ds::tokens::{Status, space};
use crate::ds::widgets::{EmptyState, Segment, Segmented, StatusChip, Tip, TipExt as _};
use crate::ds::{a11y, icons};

/// The perform loop's step interval: a frame that takes over [`LATE_FACTOR`] of it is late.
const TARGET_INTERVAL_S: f64 = 1.0 / 60.0;
/// A frame is late past this many target intervals.
const LATE_FACTOR: f64 = 1.5;
/// Weight of the newest sample in the smoothed frame, CPU and GPU times.
const SMOOTHING: f32 = 0.1;
/// How often the slow sections (perform, tracker, audio) are sampled: 10 Hz.
const SLOW_PERIOD_S: f64 = 0.1;
/// Frame submits the one-second window holds: enough for a 500 Hz display.
const RING: usize = 512;
/// The audio graph's sample rate (`omuzaki-audio-api`).
const SAMPLE_RATE: f64 = 48_000.0;
/// The bus the `voice.fx` row reads.
const MIC_BUS: &str = "mic";
/// How often the panel reads the snapshot while it is shown.
const READ_PERIOD: Duration = Duration::from_millis(250);
/// The room before a budget bar for its status icon.
const BAR_ICON: f32 = 18.0;
/// A budget bar turns amber past this share of its budget.
const WARN_RATIO: f64 = 0.8;

/// One frame the stage drew, as the viewport hands it to the publisher.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Rendered {
    /// The renderer's report.
    pub report: FrameReport,
    /// From the tracker frame's capture to the end of this frame's submit, when a tracked face is in use.
    pub track_to_submit: Option<Duration>,
}

/// Writes the live-metrics snapshot from the app's frame loop: one call per UI frame with what the stage drew, and the
/// slow sections at 10 Hz. Allocates nothing after [`new`](Self::new).
#[derive(Debug)]
pub struct HealthPublisher {
    writers: MetricsWriters,
    /// The frame submits of the last second: when, and the interval before it (s).
    submits: Box<[(f64, f64); RING]>,
    /// The next slot in `submits`.
    head: usize,
    /// Slots in use.
    len: usize,
    /// The newest submit.
    last: Option<f64>,
    frame_ms: Option<f32>,
    cpu_ms: f32,
    gpu_ms: Option<f32>,
    face_paint_ms: Option<f32>,
    late: u64,
    report: Option<FrameReport>,
    track_to_submit_ms: Option<f32>,
    /// When the slow sections were last sampled.
    slow_at: Option<f64>,
}

impl HealthPublisher {
    /// A publisher writing through `writers`.
    pub fn new(writers: MetricsWriters) -> Self {
        Self {
            writers,
            submits: Box::new([(0.0, 0.0); RING]),
            head: 0,
            len: 0,
            last: None,
            frame_ms: None,
            cpu_ms: 0.0,
            gpu_ms: None,
            face_paint_ms: None,
            late: 0,
            report: None,
            track_to_submit_ms: None,
            slow_at: None,
        }
    }

    /// A publisher and the reader of what it writes.
    pub fn with_reader() -> (Self, MetricsReader) {
        let (writers, reader) = metrics();
        (Self::new(writers), reader)
    }

    /// One UI frame at `now` (seconds on the app's clock), with the stage's frame if it drew one. Publishes the frame
    /// section once the stage has drawn its first frame; before that the section stays absent.
    pub fn frame(&mut self, now: f64, rendered: Option<Rendered>) {
        if let Some(rendered) = rendered {
            self.submit(now, &rendered);
        }
        let Some(report) = self.report else { return };
        // The window's oldest entries fall out as time passes, so a stalled stage reads its true rate.
        let since = now - 1.0;
        let recent = || self.window().filter(move |(at, _)| *at > since);
        let fps = recent().count();
        let worst = recent().map(|(_, gap)| gap).fold(0.0_f64, f64::max);
        let mut m = FrameMetrics::new(Seconds(now));
        #[allow(clippy::cast_precision_loss)] // at most RING
        {
            m.fps = fps as f32;
        }
        m.frame_ms = self.frame_ms.unwrap_or(0.0);
        #[allow(clippy::cast_possible_truncation)] // milliseconds of one frame
        {
            m.frame_ms_worst_1s = (worst * 1000.0) as f32;
        }
        m.cpu_ms = self.cpu_ms;
        m.gpu_ms = self.gpu_ms;
        m.face_paint_ms = self.face_paint_ms;
        m.draw_calls = report.draw_calls;
        m.triangles = report.triangles;
        m.render_size = [report.render_size.x, report.render_size.y];
        m.late_frames = self.late;
        m.track_to_submit_ms = self.track_to_submit_ms;
        self.writers.frame.publish(&m);
    }

    /// Whether the slow sections are due at `now`; marks them sampled when they are.
    pub fn slow_due(&mut self, now: f64) -> bool {
        let due = self.slow_at.is_none_or(|at| now - at >= SLOW_PERIOD_S || now < at);
        if due {
            self.slow_at = Some(now);
        }
        due
    }

    /// The performer's state at `now`, with the steps its loop dropped so far.
    pub fn perform(&mut self, now: f64, status: &PerformStatus, skipped: u64) {
        let mut m = PerformMetrics::new(Seconds(now));
        m.source_fps = status.source_fps;
        m.tracking = match status.tracking {
            Tracking::Tracking => TrackingState::Tracking,
            Tracking::Lost => TrackingState::Lost,
            Tracking::NoFrames => TrackingState::NoFrames,
        };
        m.calibration = match status.calibration {
            CalibrationState::Capturing { .. } => CalibrationPhase::Capturing,
            CalibrationState::Done { .. } => CalibrationPhase::Done,
            CalibrationState::Failed { .. } => CalibrationPhase::Failed,
            CalibrationState::Idle => CalibrationPhase::Idle,
        };
        m.skipped_steps = skipped;
        self.writers.perform.publish(&m);
    }

    /// The tracker's latency at `now`. Nothing is published before its first frame: an idle tracker has no latency,
    /// and a zero would read as a perfect one.
    pub fn tracker(&mut self, now: f64, stats: &LatencyStats) {
        if stats.frames == 0 {
            return;
        }
        let mut m = TrackerMetrics::new(Seconds(now));
        m.track_ms_p50 = millis(stats.p50);
        m.track_ms_p95 = millis(stats.p95);
        m.track_ms_max = millis(stats.max);
        m.tracker_frames = stats.frames;
        m.tracker_dropped = stats.dropped;
        self.writers.tracker.publish(&m);
    }

    /// The audio engine's health at `now`, condensed to sums. Nothing is published while its graph has no `mic` bus:
    /// the `voice.fx` row would read zero.
    pub fn audio(&mut self, now: f64, status: &AudioStatus, report: &GraphReport) {
        let Some(mic) = report.buses.iter().find(|bus| bus.bus.as_str() == MIC_BUS) else { return };
        let mut m = AudioMetrics::new(Seconds(now));
        let samples = f64::from(mic.chain_samples) + f64::from(status.block_frames);
        #[allow(clippy::cast_possible_truncation)] // milliseconds of a voice chain
        {
            m.voice_fx_ms = (samples / SAMPLE_RATE * 1000.0) as f32;
        }
        // The heard route: the slowest declared route to an output device.
        let heard = report
            .routes
            .iter()
            .filter(|route| matches!(route.to, RouteEnd::Output { .. }))
            .max_by(|a, b| a.declared_total().0.total_cmp(&b.declared_total().0));
        if let Some(route) = heard {
            m.heard_latency_declared_ms = seconds_ms(route.declared_total());
            m.heard_latency_measured_ms = route.measured.map(seconds_ms);
        }
        m.underruns = status.bridges.iter().map(|bridge| bridge.underruns).sum();
        m.overruns = status.bridges.iter().map(|bridge| bridge.overruns).sum();
        m.tap_drops = status.tap_drops.iter().map(|tap| tap.dropped).sum();
        m.model_blocks_missed = status.model_blocks_missed;
        self.writers.audio.publish(&m);
    }

    fn submit(&mut self, now: f64, rendered: &Rendered) {
        let report = rendered.report;
        if let Some(last) = self.last {
            let gap = (now - last).max(0.0);
            if gap > TARGET_INTERVAL_S * LATE_FACTOR {
                self.late += 1;
            }
            #[allow(clippy::cast_possible_truncation)] // milliseconds of one frame
            let ms = (gap * 1000.0) as f32;
            self.frame_ms = Some(self.frame_ms.map_or(ms, |old| smooth(old, ms)));
            self.submits[self.head] = (now, gap);
            self.head = (self.head + 1) % RING;
            self.len = (self.len + 1).min(RING);
        }
        self.last = Some(now);
        let cpu = seconds_ms(report.cpu.total());
        self.cpu_ms = if self.report.is_some() { smooth(self.cpu_ms, cpu) } else { cpu };
        if let Some(gpu) = report.gpu {
            let painters = gpu.painters.map(seconds_ms);
            let passes = seconds_ms(gpu.passes.total()) - painters.unwrap_or(0.0);
            self.gpu_ms = Some(self.gpu_ms.map_or(passes, |old| smooth(old, passes.max(0.0))));
            if painters.is_some() {
                self.face_paint_ms = painters;
            }
        }
        self.track_to_submit_ms = rendered.track_to_submit.map(millis);
        self.report = Some(report);
    }

    /// The submits held, oldest first or not: the window only counts and takes the worst.
    fn window(&self) -> impl Iterator<Item = (f64, f64)> + '_ {
        self.submits.iter().take(self.len).copied()
    }
}

fn smooth(old: f32, new: f32) -> f32 {
    old + (new - old) * SMOOTHING
}

#[allow(clippy::cast_possible_truncation)] // milliseconds of a live reading
fn seconds_ms(s: Seconds) -> f32 {
    (s.0 * 1000.0) as f32
}

#[allow(clippy::cast_possible_truncation)] // milliseconds of a live reading
fn millis(d: Duration) -> f32 {
    (d.as_secs_f64() * 1000.0) as f32
}

/// The Health panel: tracking, the frame loop's numbers, drops and underruns, and every budget row against its budget.
#[derive(Debug, Default)]
pub struct HealthPanel;

impl HealthPanel {
    /// Draws the snapshot `reader` holds now; with no reader (the live slice is not running), says so.
    pub fn ui(&mut self, ui: &mut Ui, reader: Option<&MetricsReader>) {
        let Some(reader) = reader else {
            let ctx = ui.ctx().clone();
            let (headline, line) = (text(&ctx, Word::HealthOff), text(&ctx, Word::HealthOffLine));
            EmptyState::new(icons::GAUGE, &headline, &line).show(ui);
            return;
        };
        ui.ctx().request_repaint_after(READ_PERIOD);
        show(ui, &reader.read());
    }
}

/// Draws `live`: what the Health panel shows, for the panel, the gallery and the evidence.
pub fn show(ui: &mut Ui, live: &LiveMetrics) {
    let ctx = ui.ctx().clone();
    ui.spacing_mut().item_spacing.y = space::S8;
    tracking_line(ui, live.perform.as_ref());
    ui.add_space(space::S4);
    for (name, value) in readings(&ctx, live) {
        reading(ui, &name, &value);
    }
    ui.add_space(space::S8);
    ui.label(RichText::new(text(&ctx, Word::HealthBudgets)).strong());
    if live.budget.is_empty() {
        ui.label(RichText::new(text(&ctx, Word::HealthNoBudgets)).font(ds(&ctx).text.small.clone()));
    }
    for row in &live.budget {
        budget_bar(ui, row);
    }
}

/// The tracking chip, with the camera's rate while it sends frames.
fn tracking_line(ui: &mut Ui, perform: Option<&PerformMetrics>) {
    let ctx = ui.ctx().clone();
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = space::S8;
        let (status, word) = match perform.map(|p| p.tracking) {
            None => (Status::Info, Word::HealthUnknown),
            Some(TrackingState::Tracking) => (Status::Ok, Word::HealthTracking),
            Some(TrackingState::Lost) => (Status::Warn, Word::HealthLost),
            Some(_) => (Status::Info, Word::HealthNoFrames),
        };
        ui.add(StatusChip::new(status, &text(&ctx, word)));
        if let Some(p) = perform.filter(|p| p.tracking != TrackingState::NoFrames) {
            let fps = format!("{:.0}", p.source_fps);
            ui.label(say(&ctx, Word::HealthCameraFps, &[("fps", &fps)]));
        }
    });
    if perform.is_some_and(|p| p.tracking == TrackingState::Lost) {
        ui.label(RichText::new(text(&ctx, Word::HealthLostLine)).font(ds(&ctx).text.small.clone()));
    }
}

/// The readings, each a name and its value: "—" for a section that has not published, never zero.
fn readings(ctx: &egui::Context, live: &LiveMetrics) -> [(String, String); 7] {
    let off = || text(ctx, Word::HealthNotReporting).into_owned();
    let count = |n: u64| n.to_string();
    let frame = live.frame.as_ref();
    let fps = frame.map_or_else(off, |f| say(ctx, Word::HealthFps, &[("fps", &format!("{:.0}", f.fps))]));
    let time = frame.map_or_else(off, |f| {
        let (ms, worst) = (format!("{:.1}", f.frame_ms), format!("{:.0}", f.frame_ms_worst_1s));
        say(ctx, Word::HealthFrameTimeValue, &[("ms", &ms), ("worst", &worst)])
    });
    let latency = frame.map_or_else(off, |f| match f.track_to_submit_ms {
        Some(ms) => say(ctx, Word::HealthMs, &[("ms", &format!("{ms:.0}"))]),
        None => text(ctx, Word::HealthNoFace).into_owned(),
    });
    let late = frame.map_or_else(off, |f| count(f.late_frames));
    let dropped = frame.map_or_else(off, |f| {
        f.output_dropped.map_or_else(|| text(ctx, Word::HealthNotApplicable).into_owned(), count)
    });
    let tracking = live.tracker.as_ref().map_or_else(off, |t| count(t.tracker_dropped));
    let underruns = live.audio.as_ref().map_or_else(off, |a| count(a.underruns));
    let name = |word| text(ctx, word).into_owned();
    [
        (name(Word::HealthFrameRate), fps),
        (name(Word::HealthFrameTime), time),
        (name(Word::HealthLatency), latency),
        (name(Word::HealthLateFrames), late),
        (name(Word::HealthDroppedFrames), dropped),
        (name(Word::HealthTrackingDrops), tracking),
        (name(Word::HealthUnderruns), underruns),
    ]
}

/// One reading on a line of its own: the name, then the value at the right. A Tab stop, read as "name: value".
fn reading(ui: &mut Ui, name: &str, value: &str) {
    let ds = ds(ui.ctx());
    let height = ui.text_style_height(&egui::TextStyle::Body);
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::focusable_noninteractive());
    let spoken = format!("{name}: {value}");
    // A label carries its text as AccessKit's value; the reading's name is set too, so it is announced as a Tab stop.
    a11y::describe(
        &response,
        || WidgetInfo::labeled(WidgetType::Label, true, &spoken),
        |node| node.set_label(spoken.clone()),
    );
    let painter = ui.painter();
    let font = ds.text.body.clone();
    painter.text(rect.left_center(), egui::Align2::LEFT_CENTER, name, font.clone(), ds.theme.text2);
    painter.text(rect.right_center(), egui::Align2::RIGHT_CENTER, value, font, ds.theme.text);
    if response.has_focus() {
        painter.rect_stroke(rect.expand(2.0), 2.0, ui.visuals().selection.stroke, egui::StrokeKind::Outside);
    }
}

/// A budget row: its name with "live", the value against the budget, and a bar coloured by how much it uses.
fn budget_bar(ui: &mut Ui, row: &BudgetUse) {
    let ctx = ui.ctx().clone();
    let ds = ds(&ctx);
    let name = say(&ctx, Word::HealthBudgetLive, &[("name", &budget_name(&ctx, row.key))]);
    let (value, budget) = budget_numbers(&ctx, row);
    let percent = format!("{:.0}", row.ratio * 100.0);
    let mut detail =
        say(&ctx, Word::HealthBudgetValue, &[("value", &value), ("budget", &budget), ("percent", &percent)]);
    let over = row.ratio > 1.0;
    if over {
        detail = say(&ctx, Word::HealthOver, &[("line", &detail)]);
    }
    let status = if over {
        Status::Danger
    } else if row.ratio > WARN_RATIO {
        Status::Warn
    } else {
        Status::Ok
    };
    let colour = ds.theme.status(status);
    let small = ds.text.small.clone();
    let line = ui.text_style_height(&egui::TextStyle::Body);
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), line * 2.0 + 12.0), Sense::focusable_noninteractive());
    let mut spoken = format!("{name}: {detail}");
    if row.partial {
        spoken = format!("{spoken}. {}", text(&ctx, Word::HealthPartialDetail));
    }
    a11y::describe(&response, || WidgetInfo::labeled(WidgetType::ProgressIndicator, true, &spoken), |_| {});
    let painter = ui.painter();
    painter.text(rect.left_top(), egui::Align2::LEFT_TOP, &name, ds.text.body.clone(), ds.theme.text);
    painter.text(rect.right_top(), egui::Align2::RIGHT_TOP, &detail, small.clone(), ds.theme.text2);
    // The bar's status icon leads it, so its colour never says how it stands alone.
    let track = egui::Rect::from_min_size(
        rect.left_top() + vec2(BAR_ICON, line + 6.0),
        vec2((rect.width() - BAR_ICON).at_least(0.0), 6.0),
    );
    let at = egui::pos2(rect.left() + BAR_ICON / 2.0 - 2.0, track.center().y);
    crate::ds::paint::icon(painter, &ds, at, icons::status(status), 12.0, colour, true);
    painter.rect_filled(track, 3.0, ds.theme.bg4);
    #[allow(clippy::cast_possible_truncation)] // a share of one bar
    let share = row.ratio.clamp(0.0, 1.0) as f32;
    let fill = egui::Rect::from_min_size(track.min, vec2(track.width() * share, track.height()));
    painter.rect_filled(fill, 3.0, colour);
    if row.partial {
        let note = text(&ctx, Word::HealthPartial);
        painter.text(track.left_bottom() + vec2(0.0, 2.0), egui::Align2::LEFT_TOP, note, small, ds.theme.text3);
    }
    if response.has_focus() {
        painter.rect_stroke(rect.expand(2.0), 2.0, ui.visuals().selection.stroke, egui::StrokeKind::Outside);
    }
    let _ = response.tip(Tip::new(&name).detail(&budget_detail(&ctx, row)));
}

/// What a row measures, for its tooltip.
fn budget_detail(ctx: &egui::Context, row: &BudgetUse) -> String {
    let word = if row.partial { Word::HealthPartialDetail } else { Word::HealthLiveDetail };
    text(ctx, word).into_owned()
}

fn budget_name(ctx: &egui::Context, key: BudgetKey) -> String {
    let word = match key {
        BudgetKey::GpuFrame => Word::BudgetGpuFrame,
        BudgetKey::GpuFacePaint => Word::BudgetFacePaint,
        BudgetKey::TrackFace => Word::BudgetTrackFace,
        BudgetKey::VoiceFx => Word::BudgetVoiceFx,
        BudgetKey::LatencyLaptop => Word::BudgetLatency,
        BudgetKey::PhysicsStep => Word::BudgetPhysics,
        BudgetKey::CpuTotal => Word::BudgetCpu,
        BudgetKey::RamRuntime => Word::BudgetRam,
        _ => Word::BudgetOther,
    };
    text(ctx, word).into_owned()
}

/// The value and the budget in the row's unit: milliseconds, a share of the cores, or megabytes.
fn budget_numbers(ctx: &egui::Context, row: &BudgetUse) -> (String, String) {
    match row.key {
        BudgetKey::CpuTotal => {
            let pc = |v: f64| say(ctx, Word::HealthPercent, &[("n", &format!("{:.1}", v * 100.0))]);
            (pc(row.value), pc(row.budget))
        }
        BudgetKey::RamRuntime => {
            let mb = |v: f64| say(ctx, Word::HealthMb, &[("n", &format!("{v:.0}"))]);
            (mb(row.value), mb(row.budget))
        }
        _ => {
            let digits = if row.budget < 2.0 { 2 } else { 1 };
            let ms = |v: f64| say(ctx, Word::HealthMs, &[("ms", &format!("{v:.digits$}"))]);
            (ms(row.value), ms(row.budget))
        }
    }
}

/// The states the gallery and the evidence show the panel in.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum HealthSample {
    /// The studio just opened: the stage draws, no face yet, the audio idles.
    Idle,
    /// A heavy scene on a slow machine: every row over its budget.
    OverBudget,
    /// The camera lost the face: the life layer has the character.
    TrackingLost,
}

impl HealthSample {
    /// Every state, in the order the gallery shows them.
    pub const ALL: [Self; 3] = [Self::Idle, Self::OverBudget, Self::TrackingLost];

    /// The state's file-name slug.
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::OverBudget => "over-budget",
            Self::TrackingLost => "tracking-lost",
        }
    }

    /// A reader holding this state's snapshot.
    pub fn reader(self) -> MetricsReader {
        let (mut writers, reader) = metrics();
        let at = Seconds(12.0);
        let mut frame = FrameMetrics::new(at);
        frame.render_size = [1920, 1080];
        frame.draw_calls = 38;
        frame.triangles = 61_204;
        let mut perform = PerformMetrics::new(at);
        let mut tracker = TrackerMetrics::new(at);
        let mut audio = AudioMetrics::new(at);
        audio.voice_fx_ms = 6.3;
        match self {
            Self::Idle => {
                (frame.fps, frame.frame_ms, frame.frame_ms_worst_1s) = (60.0, 16.7, 18.0);
                (frame.cpu_ms, frame.gpu_ms) = (1.9, Some(0.9));
            }
            Self::OverBudget => {
                (frame.fps, frame.frame_ms, frame.frame_ms_worst_1s) = (41.0, 24.3, 48.0);
                (frame.cpu_ms, frame.gpu_ms, frame.face_paint_ms) = (6.8, Some(2.4), Some(0.9));
                (frame.late_frames, frame.output_dropped, frame.track_to_submit_ms) = (312, Some(18), Some(96.0));
                (perform.tracking, perform.source_fps, perform.calibration) =
                    (TrackingState::Tracking, 24.0, CalibrationPhase::Done);
                (tracker.track_ms_p50, tracker.track_ms_p95, tracker.track_ms_max) = (5.6, 7.8, 14.2);
                (tracker.tracker_frames, tracker.tracker_dropped) = (4_210, 44);
                (audio.voice_fx_ms, audio.underruns) = (13.1, 3);
            }
            Self::TrackingLost => {
                (frame.fps, frame.frame_ms, frame.frame_ms_worst_1s) = (60.0, 16.7, 19.0);
                (frame.cpu_ms, frame.gpu_ms, frame.face_paint_ms) = (2.1, Some(1.1), Some(0.3));
                frame.late_frames = 2;
                (perform.tracking, perform.source_fps, perform.calibration) =
                    (TrackingState::Lost, 29.0, CalibrationPhase::Done);
                (tracker.track_ms_p50, tracker.track_ms_p95, tracker.track_ms_max) = (2.2, 3.1, 6.0);
                (tracker.tracker_frames, tracker.tracker_dropped) = (9_120, 3);
            }
        }
        writers.frame.publish(&frame);
        writers.audio.publish(&audio);
        if self == Self::Idle {
            writers.perform.publish(&PerformMetrics::new(at));
        } else {
            writers.perform.publish(&perform);
            writers.tracker.publish(&tracker);
        }
        reader
    }
}

/// The gallery's Health story: one state at a time, picked with a segmented control.
pub struct HealthStory {
    shown: HealthSample,
    readers: Vec<(HealthSample, MetricsReader)>,
}

impl Default for HealthStory {
    fn default() -> Self {
        Self { shown: HealthSample::OverBudget, readers: HealthSample::ALL.iter().map(|s| (*s, s.reader())).collect() }
    }
}

impl std::fmt::Debug for HealthStory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HealthStory").field("shown", &self.shown).finish_non_exhaustive()
    }
}

impl HealthSample {
    /// What the gallery calls the state.
    pub fn title(self, ctx: &egui::Context) -> String {
        let word = match self {
            Self::Idle => Word::HealthStoryIdle,
            Self::OverBudget => Word::HealthStoryOver,
            Self::TrackingLost => Word::HealthStoryLost,
        };
        text(ctx, word).into_owned()
    }
}

/// Shows the story: the state picker, then the panel in a dock-wide column.
pub fn story(ui: &mut Ui, state: &mut HealthStory) {
    let ctx = ui.ctx().clone();
    let titles: Vec<String> = HealthSample::ALL.iter().map(|s| s.title(&ctx)).collect();
    let segments: Vec<Segment<'_, HealthSample>> =
        HealthSample::ALL.iter().zip(&titles).map(|(s, title)| Segment::new(*s, title)).collect();
    let label = text(&ctx, Word::HealthStoryState);
    ui.add(Segmented::new(&label, &mut state.shown, &segments).id_salt("live.health.story"));
    ui.add_space(space::S8);
    let Some((_, reader)) = state.readers.iter().find(|(s, _)| *s == state.shown) else { return };
    ui.vertical(|ui| {
        ui.set_max_width(320.0);
        show(ui, &reader.read());
    });
}

#[cfg(test)]
mod tests {
    use omuzaki_core::glam::UVec2;
    use omuzaki_render_api::{GpuTimings, Pass, PassTimes};

    use super::*;

    fn rendered(gpu_ms: Option<f64>) -> Rendered {
        let mut cpu = PassTimes::default();
        cpu.set(Pass::Opaque, Seconds(0.002));
        let gpu = gpu_ms.map(|ms| {
            let mut passes = PassTimes::default();
            passes.set(Pass::Opaque, Seconds(ms / 1000.0));
            GpuTimings { frame: 1, passes, painters: None }
        });
        let report =
            FrameReport { frame: 1, render_size: UVec2::new(640, 360), cpu, gpu, draw_calls: 9, triangles: 900 };
        Rendered { report, track_to_submit: None }
    }

    #[test]
    fn no_frame_section_before_the_stage_draws() {
        let (mut p, reader) = HealthPublisher::with_reader();
        p.frame(0.0, None);
        p.frame(0.1, None);
        assert!(reader.read().frame.is_none(), "an absent stage is not a zero");
    }

    #[test]
    fn rate_worst_interval_and_late_frames_come_from_the_submits() {
        let (mut p, reader) = HealthPublisher::with_reader();
        let mut t = 0.0;
        for _ in 0..60 {
            p.frame(t, Some(rendered(Some(1.0))));
            t += 1.0 / 60.0;
        }
        // One hitch of 50 ms: late, and the worst of the second.
        t += 0.05 - 1.0 / 60.0;
        p.frame(t, Some(rendered(Some(1.0))));
        let f = reader.read().frame.expect("published");
        assert!((58.0..=61.0).contains(&f.fps), "{}", f.fps);
        assert!((f.frame_ms_worst_1s - 50.0).abs() < 1.0, "{}", f.frame_ms_worst_1s);
        assert_eq!(f.late_frames, 1);
        assert_eq!(f.gpu_ms.map(|g| (g * 10.0).round()), Some(10.0));
        assert_eq!(f.render_size, [640, 360]);
        assert!(f.output_dropped.is_none(), "no output reported");

        // A stalled stage: the window empties as time passes.
        p.frame(t + 2.0, None);
        assert_eq!(reader.read().frame.expect("published").fps, 0.0);
    }

    #[test]
    fn an_idle_tracker_publishes_nothing_and_the_slow_sections_run_at_ten_hertz() {
        let (mut p, reader) = HealthPublisher::with_reader();
        p.tracker(0.0, &LatencyStats::default());
        assert!(reader.read().tracker.is_none());
        let stats =
            LatencyStats::new(30, 2, Duration::from_millis(3), Duration::from_millis(4), Duration::from_millis(9));
        p.tracker(0.0, &stats);
        let t = reader.read().tracker.expect("published");
        assert_eq!((t.tracker_frames, t.tracker_dropped), (30, 2));
        assert!((t.track_ms_p95 - 4.0).abs() < 1e-3);
        assert!(p.slow_due(0.0));
        assert!(!p.slow_due(0.05));
        assert!(p.slow_due(0.1));
    }

    #[test]
    fn the_over_budget_sample_is_over_every_budget_it_derives() {
        let live = HealthSample::OverBudget.reader().read();
        assert_ne!(live.budget, Vec::new());
        assert!(live.budget.iter().all(|row| row.ratio > 1.0), "{:?}", live.budget);
        assert!(live.budget.iter().any(|row| row.partial), "latency.laptop is partial");
        let idle = HealthSample::Idle.reader().read();
        assert!(idle.tracker.is_none(), "no face yet: the tracker has not published");
    }
}
