//! The mode, subtype and error tables (they build and run on every platform).

use omuzaki_camera_mf::modes::{
    E_ACCESSDENIED, MF_E_HW_MFT_FAILED_START_STREAMING, MF_E_VIDEO_RECORDING_DEVICE_INVALIDATED, MJPG, ModeInfo, NV12,
    RGB32, YUY2, choose_mode, copy_top_down, frame_span, map_hresult, subtype_format,
};
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use omuzaki_camera_mf::modes::{
    Capacity, CaptureClock, ClockSource, FrameSpan, NativeSizes, RowFlip, SeqTracker, Signal, SlotFailure, Stamp, Take,
    TokenSlot, is_progressive, reap, take_after_retype,
};
use omuzaki_track_api::camera::{CameraFormat, CameraFormatRequest, CameraId, PixelFormat};
use omuzaki_track_api::error::TrackError;

const fn mode(format: PixelFormat, width: u32, height: u32, fps: u32) -> ModeInfo {
    ModeInfo { format, width, height, fps_num: fps, fps_den: 1 }
}

/// Twelve modes: three layouts at 1280x720 (30 and 60 fps), 640x480 and 1920x1080.
fn table() -> Vec<ModeInfo> {
    use PixelFormat::{Mjpg, Nv12, Yuy2};
    vec![
        mode(Mjpg, 1280, 720, 30),
        mode(Yuy2, 1280, 720, 30),
        mode(Nv12, 1280, 720, 30),
        mode(Mjpg, 1280, 720, 60),
        mode(Yuy2, 1280, 720, 60),
        mode(Nv12, 1280, 720, 60),
        mode(Mjpg, 640, 480, 30),
        mode(Yuy2, 640, 480, 30),
        mode(Nv12, 640, 480, 30),
        mode(Mjpg, 1920, 1080, 30),
        mode(Yuy2, 1920, 1080, 30),
        mode(Nv12, 1920, 1080, 30),
    ]
}

#[test]
fn nv12_beats_yuy2_beats_mjpg_at_the_requested_size() {
    let request = CameraFormatRequest::new(1280, 720, 30);
    let mut modes = table();
    assert_eq!(choose_mode(&modes, &request), Some(mode(PixelFormat::Nv12, 1280, 720, 30)));
    modes.retain(|m| m.format != PixelFormat::Nv12);
    assert_eq!(choose_mode(&modes, &request), Some(mode(PixelFormat::Yuy2, 1280, 720, 30)));
    modes.retain(|m| m.format != PixelFormat::Yuy2);
    assert_eq!(choose_mode(&modes, &request), Some(mode(PixelFormat::Mjpg, 1280, 720, 30)));
}

#[test]
fn sixty_fps_is_not_chosen_over_thirty_for_a_thirty_request() {
    let picked = choose_mode(&table(), &CameraFormatRequest::new(1280, 720, 30)).unwrap();
    assert_eq!(picked.to_format().fps, 30);
    let picked = choose_mode(&table(), &CameraFormatRequest::new(1280, 720, 60)).unwrap();
    assert_eq!(picked.to_format().fps, 60);
}

#[test]
fn a_camera_with_one_size_gets_it() {
    let only = [mode(PixelFormat::Yuy2, 640, 480, 30)];
    assert_eq!(choose_mode(&only, &CameraFormatRequest::new(1280, 720, 30)), Some(only[0]));
}

#[test]
fn no_modes_or_no_matching_layout_picks_nothing() {
    let request = CameraFormatRequest::new(1280, 720, 30);
    assert_eq!(choose_mode(&[], &request), None);
    let request = request.with_pixel(Some(PixelFormat::Bgra8));
    assert_eq!(choose_mode(&table(), &request), None);
}

#[test]
fn fractional_rates_round_to_whole_frames() {
    let ntsc = ModeInfo { format: PixelFormat::Nv12, width: 1280, height: 720, fps_num: 30_000, fps_den: 1001 };
    assert_eq!(ntsc.to_format(), CameraFormat::new(1280, 720, 30, PixelFormat::Nv12));
    let none = ModeInfo { fps_den: 0, ..ntsc };
    assert_eq!(none.to_format().fps, 0);
}

#[test]
fn subtypes_map_to_layouts() {
    assert_eq!(subtype_format(NV12), Some(PixelFormat::Nv12));
    assert_eq!(subtype_format(YUY2), Some(PixelFormat::Yuy2));
    assert_eq!(subtype_format(MJPG), Some(PixelFormat::Mjpg));
    assert_eq!(subtype_format(RGB32), Some(PixelFormat::Bgra8));
    assert_eq!(subtype_format(20), None, "RGB24 is B, G, R in Media Foundation; Rgb8 is R, G, B");
    assert_eq!(subtype_format(u32::from_le_bytes(*b"H264")), None);
    assert_eq!(NV12, 0x3231_564E);
}

#[test]
fn hresults_map_to_track_errors() {
    let id = CameraId("cam".into());
    assert_eq!(map_hresult(E_ACCESSDENIED, &id), TrackError::PermissionDenied);
    assert_eq!(map_hresult(MF_E_VIDEO_RECORDING_DEVICE_INVALIDATED, &id), TrackError::Lost);
    assert_eq!(map_hresult(MF_E_HW_MFT_FAILED_START_STREAMING, &id), TrackError::Busy(id.clone()));
    assert_eq!(
        map_hresult(0x8000_4005_u32.cast_signed(), &id),
        TrackError::Backend("Media Foundation error 0x80004005".into())
    );
}

/// Three rows of two pixels' worth of bytes (`a`, `b`), padded to a pitch of 4.
const BLOCK: [u8; 12] = [1, 1, 0, 0, 2, 2, 0, 0, 3, 3, 0, 0];

#[test]
fn top_first_rows_copy_in_order_without_padding() {
    let mut dst = [9; 6];
    copy_top_down(&BLOCK, 4, 2, 3, &mut dst).unwrap();
    assert_eq!(dst, [1, 1, 2, 2, 3, 3]);
}

#[test]
fn bottom_up_rows_are_read_from_the_end() {
    let mut dst = [9; 6];
    copy_top_down(&BLOCK, -4, 2, 3, &mut dst).unwrap();
    assert_eq!(dst, [3, 3, 2, 2, 1, 1]);
}

#[test]
fn bad_sizes_are_errors_and_leave_dst_alone() {
    let mut dst = [9; 6];
    assert!(copy_top_down(&BLOCK, 1, 2, 3, &mut dst).is_err(), "pitch below a row");
    assert!(copy_top_down(&BLOCK, 0, 0, 3, &mut dst).is_err(), "zero pitch");
    assert!(copy_top_down(&BLOCK[..10], 4, 2, 3, &mut dst).is_err(), "short source");
    assert!(copy_top_down(&BLOCK, 4, 2, 3, &mut dst[..5]).is_err(), "short destination");
    assert_eq!(dst, [9; 6]);
}

#[test]
fn nv12_frames_include_the_chroma_plane() {
    let span = frame_span(PixelFormat::Nv12, 1280, 720, 1280, 0, 1_382_400).unwrap();
    assert_eq!(span, FrameSpan { start: 0, len: 1_382_400 });
    assert!(frame_span(PixelFormat::Nv12, 1280, 720, 1280, 0, 921_600).is_err(), "Y plane alone is too short");
    assert!(frame_span(PixelFormat::Nv12, 1280, 720, -1280, 1_382_400, 2_000_000).is_err());
}

#[test]
fn padded_rows_are_judged_by_their_real_extent() {
    // 1280x720 BGRA with 64 bytes of padding per row is larger than the packed 3,686,400 bytes.
    let pitch: usize = 4 * 1280 + 64;
    let whole = 720 * pitch;
    assert_eq!(frame_span(PixelFormat::Bgra8, 1280, 720, pitch.cast_signed(), 0, whole).unwrap().len, whole);
    assert!(frame_span(PixelFormat::Bgra8, 1280, 720, pitch.cast_signed(), 0, whole - 1).is_err());
    // YUY2 at 640x480 with 128 padding bytes per row, the top row 256 bytes into the allocation.
    let pitch: usize = 2 * 640 + 128;
    let span = frame_span(PixelFormat::Yuy2, 640, 480, pitch.cast_signed(), 256, 256 + 480 * pitch).unwrap();
    assert_eq!(span, FrameSpan { start: 256, len: 480 * pitch });
}

#[test]
fn bottom_up_frames_start_below_the_top_row() {
    let pitch: usize = 4 * 4;
    // 4x3 BGRA, top row last in memory: scanline0 is the third row.
    let span = frame_span(PixelFormat::Bgra8, 4, 3, -(pitch.cast_signed()), 2 * pitch, 3 * pitch).unwrap();
    assert_eq!(span, FrameSpan { start: 0, len: 3 * pitch });
    assert!(frame_span(PixelFormat::Bgra8, 4, 3, -(pitch.cast_signed()), pitch, 3 * pitch).is_err(), "starts before");
}

#[test]
fn degenerate_frames_are_refused() {
    assert!(frame_span(PixelFormat::Mjpg, 4, 4, 0, 0, 100).is_err());
    assert!(frame_span(PixelFormat::Bgra8, 4, 4, 0, 0, 100).is_err());
    assert!(frame_span(PixelFormat::Bgra8, 4, 4, 8, 0, 1000).is_err(), "pitch below a row");
    assert!(frame_span(PixelFormat::Bgra8, 4, 0, 16, 0, 1000).is_err());
    assert!(frame_span(PixelFormat::Bgra8, u32::MAX, u32::MAX, isize::MAX, usize::MAX, usize::MAX).is_err());
}

#[test]
fn sequence_numbers_show_skipped_captures() {
    let mut seq = SeqTracker::new(30, 1);
    let tick = 333_333;
    let times = [0, tick, 2 * tick + 40, 3 * tick, 6 * tick, 7 * tick - 90];
    let got: Vec<u64> = times.iter().map(|&t| seq.next(t, false)).collect();
    assert_eq!(got, [0, 1, 2, 3, 6, 7], "two frames missing between 3 and 6");
}

#[test]
fn sequence_ignores_a_slightly_fast_clock_and_honours_discontinuities() {
    let mut seq = SeqTracker::new(30_000, 1001);
    let tick = 333_667;
    let mut last = 0;
    for i in 0..1000_i64 {
        last = seq.next(i * (tick - 300), false); // 0.1% fast: never a gap
    }
    assert_eq!(last, 999);
    assert_eq!(seq.next(1000 * (tick - 300), true), 1001, "the driver says one was lost");
    let mut none = SeqTracker::new(0, 0);
    assert_eq!((none.next(5, false), none.next(5, false), none.next(1, false)), (0, 1, 2));
}

/// A latched signal for the tests: a flag and a condition variable.
#[derive(Default)]
struct Flag {
    set: Mutex<bool>,
    cond: Condvar,
}

impl Signal for Flag {
    fn notify(&self) {
        *self.set.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.cond.notify_all();
    }

    fn wait(&self, timeout: Duration) {
        let guard = self.set.lock().unwrap_or_else(PoisonError::into_inner);
        let (mut guard, _) =
            self.cond.wait_timeout_while(guard, timeout, |set| !*set).unwrap_or_else(PoisonError::into_inner);
        *guard = false;
    }
}

/// A token that is never dereferenced: the slot only moves pointers.
fn token(n: usize) -> NonNull<c_void> {
    NonNull::new(std::ptr::without_provenance_mut(n + 1)).unwrap_or(NonNull::dangling())
}

#[test]
fn a_slot_times_out_then_delivers_the_newest() {
    let slot = TokenSlot::new(Flag::default());
    let mut released = Vec::new();
    assert_eq!(slot.take(Duration::ZERO), Take::Timeout);
    let started = Instant::now();
    assert_eq!(slot.take(Duration::from_millis(30)), Take::Timeout);
    assert!(started.elapsed() >= Duration::from_millis(30));
    slot.put(token(1), |t| released.push(t));
    slot.put(token(2), |t| released.push(t));
    assert_eq!(released, [token(1)], "the older frame is handed back to be released");
    assert_eq!(slot.take(Duration::ZERO), Take::Item(token(2)));
}

/// A read that timed out delivers late; the capture keeps going and a newer frame replaces it.
#[test]
fn a_late_frame_is_replaced_by_a_newer_one() {
    let slot = TokenSlot::new(Flag::default());
    let mut released = Vec::new();
    assert_eq!(slot.take(Duration::from_millis(5)), Take::Timeout, "the caller gave up");
    slot.put(token(10), |t| released.push(t)); // the late delivery
    slot.put(token(11), |t| released.push(t)); // the next capture, armed by the callback
    assert_eq!(slot.take(Duration::ZERO), Take::Item(token(11)));
    assert_eq!(released, [token(10)]);
    assert_eq!(slot.take(Duration::ZERO), Take::Timeout, "and nothing stale is left behind");
}

/// A media-type change outlives the frame it came with: the taker retypes whichever frame is in the slot.
#[test]
fn a_type_change_survives_frame_replacement() {
    let slot = TokenSlot::new(Flag::default());
    let mut released = Vec::new();
    slot.set_retype(); // the callback saw the flag on the first frame of the new type
    slot.put(token(1), |t| released.push(t));
    slot.put(token(2), |t| released.push(t)); // an ordinary frame replaces it
    assert_eq!(slot.take(Duration::ZERO), Take::Retype(Some(token(2))), "still retypes, and hands the frame back");
    assert_eq!(slot.take(Duration::ZERO), Take::Timeout, "handled once");
    slot.put(token(3), |_| {});
    assert_eq!(slot.take(Duration::ZERO), Take::Item(token(3)), "frames after it are ordinary");
    slot.set_retype();
    assert_eq!(slot.take(Duration::from_secs(10)), Take::Retype(None), "a change with no frame still wakes the taker");
}

#[test]
fn a_waiting_taker_wakes_for_a_put_from_another_thread() {
    let slot = Arc::new(TokenSlot::new(Flag::default()));
    let producer = Arc::clone(&slot);
    let thread = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(20));
        producer.put(token(7), |_| {});
    });
    assert_eq!(slot.take(Duration::from_secs(10)), Take::Item(token(7)));
    thread.join().unwrap();
}

#[test]
fn a_put_before_the_wait_is_not_lost() {
    let slot = TokenSlot::new(Flag::default());
    slot.put(token(4), |_| {});
    assert_eq!(slot.take(Duration::from_secs(10)), Take::Item(token(4)));
}

#[test]
fn failures_and_closing_end_the_wait() {
    let slot = TokenSlot::new(Flag::default());
    slot.put(token(1), |_| {});
    slot.fail(SlotFailure::Hresult(-5));
    slot.fail(SlotFailure::Lost);
    assert_eq!(slot.take(Duration::ZERO), Take::Item(token(1)), "a frame that arrived comes first");
    assert_eq!(slot.take(Duration::ZERO), Take::Failed(SlotFailure::Hresult(-5)), "the first failure stays");
    assert_eq!(slot.take(Duration::ZERO), Take::Failed(SlotFailure::Hresult(-5)));

    let slot = TokenSlot::new(Flag::default());
    slot.fail(SlotFailure::Lost);
    assert_eq!(slot.take(Duration::ZERO), Take::Failed(SlotFailure::Lost));

    let slot = TokenSlot::new(Flag::default());
    let mut released = Vec::new();
    slot.put(token(8), |_| {});
    slot.close();
    assert_eq!(slot.drain(), Some(token(8)), "the owner reclaims what was left");
    assert_eq!(slot.take(Duration::from_secs(10)), Take::Closed);
    slot.put(token(3), |t| released.push(t));
    assert_eq!(released, [token(3)], "a late callback's frame goes straight back");
    assert_eq!(slot.drain(), None);
}

#[test]
fn a_flush_can_be_waited_for() {
    let slot = TokenSlot::new(Flag::default());
    assert!(!slot.wait_flushed(Duration::from_millis(10)));
    slot.mark_flushed();
    assert!(slot.wait_flushed(Duration::ZERO));
}

#[test]
fn an_unrepresentable_timeout_does_not_panic() {
    let slot = Arc::new(TokenSlot::new(Flag::default()));
    let producer = Arc::clone(&slot);
    let thread = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(10));
        producer.put(token(1), |_| {});
    });
    assert_eq!(slot.take(Duration::MAX), Take::Item(token(1)));
    thread.join().unwrap();
}

/// Every token is released exactly once however a producer and a closing taker interleave.
#[test]
fn racing_puts_and_a_close_leak_nothing() {
    for _ in 0..200 {
        let slot = Arc::new(TokenSlot::new(Flag::default()));
        let live = Arc::new(AtomicUsize::new(0));
        let (producer, count) = (Arc::clone(&slot), Arc::clone(&live));
        let thread = std::thread::spawn(move || {
            for n in 1..=50 {
                count.fetch_add(1, Ordering::SeqCst);
                producer.put(token(n), |_| {
                    count.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
        slot.close();
        if slot.drain().is_some() {
            live.fetch_sub(1, Ordering::SeqCst);
        }
        thread.join().unwrap();
        if slot.drain().is_some() {
            live.fetch_sub(1, Ordering::SeqCst);
        }
        assert_eq!(live.load(Ordering::SeqCst), 0, "every token was released once");
    }
}

#[test]
fn a_renegotiated_rate_keeps_the_numbering() {
    let mut seq = SeqTracker::new(30, 1);
    assert_eq!((seq.next(0, false), seq.next(333_333, false)), (0, 1));
    seq.set_rate(60, 1);
    assert_eq!(seq.next(333_333 + 166_667, false), 2);
    assert_eq!(seq.next(333_333 + 3 * 166_667, false), 4, "at 60 fps one capture is missing");
}

fn bgra(width: u32, height: u32) -> ModeInfo {
    mode(PixelFormat::Bgra8, width, height, 30)
}

#[test]
fn capacity_covers_the_largest_offered_mode_and_refuses_beyond() {
    let offered = [
        bgra(640, 480),
        mode(PixelFormat::Nv12, 1920, 1080, 30),
        bgra(1280, 720),
        mode(PixelFormat::Mjpg, 3840, 2160, 30),
    ];
    let capacity = Capacity::for_modes(&offered);
    assert_eq!(capacity.flip, 4 * 1280 * 720, "only BGRA is flipped");
    assert!(capacity.gather >= 2 * 3840 * 2160, "MJPG at 4K");
    for m in &offered {
        assert_eq!(capacity.admits(m), Ok(()));
    }
    assert!(capacity.admits(&bgra(1920, 1080)).is_err(), "a BGRA mode that was never offered");
    assert!(capacity.admits(&mode(PixelFormat::Nv12, 7680, 4320, 30)).is_err());
    assert_eq!(Capacity::for_modes(&[]), Capacity { gather: 0, flip: 0 });
}

#[test]
fn row_flip_changes_mode_without_growing() {
    let mut flip = RowFlip::new(4 * 4 * 3);
    assert!(flip.is_empty());
    flip.set_len(4 * 4 * 3).unwrap();
    let block: Vec<u8> = (0..48).collect();
    let top_down = flip.fill(&block, -16, 16, 3).unwrap().to_vec();
    assert_eq!(&top_down[..16], &block[32..48], "the last row in memory is the top row");
    flip.set_len(4 * 2 * 2).unwrap(); // a smaller mode: only the length changes
    assert_eq!(flip.len(), 16);
    assert!(flip.fill(&block, -8, 8, 3).is_err(), "the size must match the mode");
    assert!(flip.set_len(4 * 4 * 3 + 1).is_err(), "never grows");
}

/// Runs frames through the clock and the sequence the way the stream does; returns the times and the numbers.
fn run(frames: &[(Option<u64>, i64)]) -> (Vec<Stamp>, Vec<u64>) {
    let (mut clock, mut seq) = (CaptureClock::new(), SeqTracker::new(30, 1));
    let stamps: Vec<Stamp> = frames.iter().map(|&(device, sample)| clock.stamp(device, sample)).collect();
    let numbers = stamps.iter().map(|s| seq.next(i64::try_from(s.ticks).unwrap_or(i64::MAX), false)).collect();
    (stamps, numbers)
}

const TICK: i64 = 333_333;
const QPC_EPOCH: u64 = 90_000_000_000_000;

#[test]
fn a_stream_that_starts_with_the_device_timestamp_stays_on_its_epoch() {
    let frames = [
        (Some(QPC_EPOCH), 0),
        (Some(QPC_EPOCH + TICK as u64), TICK),
        (None, 2 * TICK), // the attribute disappears
        (None, 3 * TICK),
        (Some(QPC_EPOCH + 4 * TICK as u64), 4 * TICK), // and comes back
    ];
    let (stamps, numbers) = run(&frames);
    assert_eq!(stamps[0].source, ClockSource::Device);
    assert!(stamps[0].first_of_source);
    assert_eq!(stamps[2].source, ClockSource::SampleAligned);
    assert!(stamps[2].first_of_source && !stamps[3].first_of_source);
    let ticks: Vec<u64> = stamps.iter().map(|s| s.ticks).collect();
    assert_eq!(ticks, (0..5).map(|n| QPC_EPOCH + n * TICK as u64).collect::<Vec<_>>(), "one epoch, no jump");
    assert_eq!(numbers, [0, 1, 2, 3, 4], "nothing counted as dropped");
}

#[test]
fn a_stream_that_starts_without_it_ignores_a_timestamp_that_appears_later() {
    let frames = [(None, 0), (None, TICK), (Some(QPC_EPOCH), 2 * TICK), (Some(QPC_EPOCH + TICK as u64), 3 * TICK)];
    let (stamps, numbers) = run(&frames);
    assert!(stamps.iter().all(|s| s.source == ClockSource::Sample));
    let ticks: Vec<i64> = stamps.iter().map(|s| i64::try_from(s.ticks).unwrap()).collect();
    assert_eq!(ticks, [0, TICK, 2 * TICK, 3 * TICK], "the sample epoch throughout; captured never jumps");
    assert_eq!(numbers, [0, 1, 2, 3], "dropped does not move");
}

#[test]
fn capture_times_never_go_negative() {
    let mut clock = CaptureClock::new();
    assert_eq!(clock.stamp(None, -7).ticks, 0);
}

#[test]
fn only_progressive_modes_are_accepted() {
    assert!(is_progressive(None), "most UVC webcams omit the attribute");
    assert!(is_progressive(Some(0)), "unknown");
    assert!(is_progressive(Some(2)), "progressive");
    for explicit in [1, 3, 4, 5, 6, 7, 8, u32::MAX] {
        assert!(!is_progressive(Some(explicit)), "interlace mode {explicit}");
    }
}

#[test]
fn gather_capacity_follows_the_native_types() {
    // 1000x500 BGRA: tight 4000 bytes a row. A driver pads rows to 5000, beyond any 256-byte alignment (4096, 4352).
    let padded = bgra(1000, 500);
    let sizes = NativeSizes { sample_size: None, default_stride: Some(-5000) };
    let capacity = Capacity::for_native(&[(padded, sizes)]);
    assert_eq!(capacity.gather, 5000 * 500, "the declared stride, sign ignored");
    // A declared sample size wins when it is the larger.
    let declared = NativeSizes { sample_size: Some(3_000_000), default_stride: None };
    assert_eq!(Capacity::for_native(&[(padded, declared)]).gather, 3_000_000);
    // Nothing declared: the mode's own tight size, no assumed padding.
    assert_eq!(Capacity::for_native(&[(padded, NativeSizes::default())]).gather, 4000 * 500);
    // The largest of several native types.
    let nv12 = mode(PixelFormat::Nv12, 1920, 1080, 30);
    let both = Capacity::for_native(&[(padded, sizes), (nv12, NativeSizes::default())]);
    assert_eq!(both.gather, 1920 * 1080 * 3 / 2);
    // A renegotiated mode that fits what the native types allowed is admitted.
    assert_eq!(both.admits(&padded), Ok(()));
}

#[test]
fn reaping_returns_at_once_even_when_the_work_stalls() {
    let (done, finished) = std::sync::mpsc::channel();
    let started = Instant::now();
    // The work stands in for a flush that a stalled driver takes 300 ms to answer.
    reap(done, |done| {
        std::thread::sleep(Duration::from_millis(300));
        let _ = done.send(());
    });
    assert!(started.elapsed() < Duration::from_millis(100), "stop must not wait for the driver");
    assert!(finished.recv_timeout(Duration::from_secs(10)).is_ok(), "and the work still runs to completion");
}

#[test]
fn a_retype_followed_by_a_frame_returns_that_frame() {
    let slot = Arc::new(TokenSlot::new(Flag::default()));
    let producer = Arc::clone(&slot);
    let thread = std::thread::spawn(move || {
        producer.set_retype();
        std::thread::sleep(Duration::from_millis(20));
        producer.put(token(2), |_| {}); // the first frame in the new format
    });
    let mut retyped = 0;
    let got = take_after_retype(&slot, Duration::from_secs(10), |_| -> Result<(), ()> {
        retyped += 1;
        Ok(())
    });
    thread.join().unwrap();
    assert_eq!(retyped, 1, "retyped once");
    assert_eq!(got, Ok(Take::Item(token(2))), "and the wait went on to the next frame");
}

#[test]
fn a_retype_without_a_frame_waits_out_the_deadline() {
    let slot = TokenSlot::new(Flag::default());
    slot.set_retype();
    let started = Instant::now();
    let mut calls = 0;
    let got = take_after_retype(&slot, Duration::from_millis(40), |_| -> Result<(), ()> {
        calls += 1;
        Ok(())
    });
    assert_eq!((got, calls), (Ok(Take::Timeout), 1));
    assert!(started.elapsed() >= Duration::from_millis(40), "the change did not cut the wait short");
    let failing = TokenSlot::new(Flag::default());
    failing.set_retype();
    assert_eq!(take_after_retype(&failing, Duration::ZERO, |_| Err("unsupported")), Err("unsupported"));
}
