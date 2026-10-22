//! The Channels panel allocates nothing per frame once warm (#347, `design-system.md` section 9, rule 4).
//!
//! Two measurements, counted only on the measuring thread so the harness's own threads can't make them flaky:
//!
//! - the panel's update (the names, the list on show, a sample of the perform thread's newest frame) allocates
//!   **nothing** over two seconds of 60 Hz frames while the mock performer publishes a new frame every one;
//! - drawing the panel with the 89 face meters costs no more allocations per frame than drawing the 3 body meters, so
//!   nothing it adds scales with the meters (egui itself allocates a fixed few times a frame).

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use egui::{Context, Pos2, RawInput, Rect, vec2};
use omuzaki_anim_api::{ChannelBuf, Cues, DeviceProfile, FrameSource as _, LifeConfig, Tick};
use omuzaki_anim_mock::performer::MockPerformer;
use omuzaki_anim_mock::retarget::MockRetarget;
use omuzaki_anim_mock::track::{SyntheticSpec, TrackReplay, synthetic_track};
use omuzaki_stage::perform::Tracking;
use omuzaki_ui::ds::{Settings, apply_settings};
use omuzaki_ui::live::channels::{ChannelFeed, ChannelsPanel, Group};
use omuzaki_ui::live::wiring::{FrameWriter, LiveFrame, frame_mailbox};

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

/// Allocations made by `f` on this thread.
fn count(f: impl FnOnce()) -> usize {
    let before = COUNT.with(Cell::get);
    MEASURING.with(|m| m.set(true));
    f();
    MEASURING.with(|m| m.set(false));
    COUNT.with(Cell::get) - before
}

/// The mock performer over a synthetic face, publishing each step into a mailbox as the perform thread does.
struct Show {
    performer: MockPerformer,
    replay: TrackReplay,
    writer: FrameWriter,
    index: u64,
}

impl Show {
    fn new() -> (Self, ChannelFeed) {
        let track = synthetic_track(&SyntheticSpec { seconds: 20.0, ..SyntheticSpec::default() });
        let performer =
            MockPerformer::new(MockRetarget::new(DeviceProfile::Webcam), LifeConfig::new(7), ChannelBuf::NEUTRAL);
        let (writer, reader) = frame_mailbox();
        (Self { performer, replay: TrackReplay::new(track).looping(), writer, index: 0 }, ChannelFeed::new(reader))
    }

    /// One 60 Hz perform step, published.
    fn step(&mut self) {
        let tick = Tick::at_rate(self.index, 60.0);
        self.index += 1;
        let source = self.replay.poll(tick.time);
        let out = self.performer.step(tick, source.as_ref(), &Cues::default());
        let frame = LiveFrame {
            tick,
            tracking: Tracking::Tracking,
            pose: *out.pose,
            channels: out.bus.values,
            owners: out.bus.owners,
            arrived: None,
        };
        self.writer.publish(&frame);
    }
}

#[test]
fn the_panels_update_allocates_nothing_while_the_performer_runs() {
    let ctx = Context::default();
    apply_settings(&ctx, Settings::default());
    let (mut show, feed) = Show::new();
    let mut panel = ChannelsPanel::new();
    let mut time = 0.0;
    for _ in 0..30 {
        show.step();
        time += 1.0 / 60.0;
        panel.update(&ctx, time, Some(&feed));
    }
    let warm = panel.samples();
    let mut allocations = 0;
    for _ in 0..120 {
        show.step();
        time += 1.0 / 60.0;
        allocations += count(|| panel.update(&ctx, time, Some(&feed)));
    }
    let sampled = panel.samples() - warm;
    eprintln!("120 frames at 60 Hz: {sampled} samples, {allocations} allocations");
    assert_eq!(allocations, 0, "the update allocated {allocations} times in 120 frames");
    assert!((55..=61).contains(&sampled), "two seconds sample about 60 times at 30 Hz, not every frame: {sampled}");
}

/// Allocations per frame, once warm, drawing the panel on `group` over a feed that holds one frame; and how many
/// meters it drew.
fn per_frame(group: Group) -> (f64, usize) {
    let ctx = Context::default();
    apply_settings(&ctx, Settings::default());
    let (mut show, feed) = Show::new();
    show.step();
    let mut panel = ChannelsPanel::new();
    panel.set_group(group);
    let mut time = 0.0;
    let mut frame = |panel: &mut ChannelsPanel| {
        time += 1.0 / 60.0;
        let input = RawInput {
            time: Some(time),
            predicted_dt: 1.0 / 60.0,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(320.0, 4000.0))),
            ..RawInput::default()
        };
        ctx.run_ui(input, |ui| panel.ui(ui, Some(&feed))).drop_without_applying_deltas();
    };
    // Warm up: fonts activate, glyphs rasterise, the text cache and egui's maps reach their working size.
    for _ in 0..40 {
        frame(&mut panel);
    }
    let frames = 30;
    let allocations = count(|| {
        for _ in 0..frames {
            frame(&mut panel);
        }
    });
    (allocations as f64 / f64::from(frames), panel.listed())
}

#[test]
fn drawing_every_face_meter_allocates_no_more_than_drawing_three() {
    let ((face, face_listed), (body, body_listed)) = (per_frame(Group::Face), per_frame(Group::Body));
    eprintln!("{face:.1} allocations/frame with {face_listed} meters, {body:.1} with {body_listed}");
    assert!(face_listed > 80 && body_listed == 3, "the groups: {face_listed} face meters, {body_listed} body meters");
    assert!(face - body <= 2.0, "the panel allocates per meter: {face} at {face_listed}, {body} at {body_listed}");
}
