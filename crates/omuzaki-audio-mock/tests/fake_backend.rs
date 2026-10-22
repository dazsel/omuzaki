//! The scripted fake backend: devices that vanish, return, reorder, change id, drift and lag, all by sample index.

#![allow(clippy::unwrap_used)] // helpers outside `#[test]` fns: a panic is a clear test failure
#![allow(clippy::float_cmp)] // times and counts here are exact by construction

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use omuzaki_audio_api::backend::{AudioBackend, BackendError, CallbackTime, StreamConfig};
use omuzaki_audio_api::device::Direction;
use omuzaki_audio_mock::backend::{Event, FakeBackend, FakeDevice, InputSource, Script, fake_id};
use omuzaki_core::time::Seconds;

fn config(frames: u32) -> StreamConfig {
    StreamConfig { rate: 48_000, channels: 1, buffer_frames: frames }
}

fn host(script: Script) -> FakeBackend {
    FakeBackend::new(
        vec![
            FakeDevice::input("mic", "USB Mic", 1).default_device(),
            FakeDevice::input("line", "Line In", 2),
            FakeDevice::output("spk", "Speakers", 2).default_device().recording(),
        ],
        script,
    )
}

/// An input stream that counts its callbacks and the errors it was told about.
struct Probe {
    callbacks: Arc<AtomicU64>,
    errors: Arc<Mutex<Vec<BackendError>>>,
    times: Arc<Mutex<Vec<CallbackTime>>>,
    _stream: Box<dyn omuzaki_audio_api::backend::DeviceStream>,
}

fn open_probe(backend: &FakeBackend, name: &str, config: StreamConfig) -> Probe {
    let (callbacks, errors, times) =
        (Arc::new(AtomicU64::new(0)), Arc::new(Mutex::new(Vec::new())), Arc::new(Mutex::new(Vec::new())));
    let (c, e, t) = (callbacks.clone(), errors.clone(), times.clone());
    let stream = backend
        .open_input(
            &fake_id(name),
            config,
            Box::new(move |_, time| {
                c.fetch_add(1, Ordering::SeqCst);
                t.lock().unwrap().push(time);
            }),
            Box::new(move |err| e.lock().unwrap().push(err)),
        )
        .unwrap();
    Probe { callbacks, errors, times, _stream: stream }
}

#[test]
fn the_enumeration_lists_the_devices_and_the_defaults() {
    let backend = host(Script::new());
    let labels: Vec<String> = backend.devices().unwrap().into_iter().map(|d| d.label).collect();
    assert_eq!(labels, ["USB Mic", "Line In", "Speakers"]);
    assert_eq!(backend.default_device(Direction::Input).unwrap(), Some(fake_id("mic")));
    assert_eq!(backend.default_device(Direction::Output).unwrap(), Some(fake_id("spk")));
}

#[test]
fn callbacks_come_every_buffer_and_carry_exact_times() {
    let backend = host(Script::new());
    let probe = open_probe(&backend, "mic", config(480));
    backend.step(4800);
    // 480 frames at 48 kHz is 10 ms: callbacks at 10, 20, ... 40 ms; the one at 50 ms is not before the end of the step.
    assert_eq!(probe.callbacks.load(Ordering::SeqCst), 9);
    backend.step(480);
    assert_eq!(probe.callbacks.load(Ordering::SeqCst), 10);
    let times = probe.times.lock().unwrap();
    assert!((times[0].callback.0 - 0.010).abs() < 1e-12);
    assert!(
        (times[0].callback.0 - times[0].device.0 - 0.005).abs() < 1e-12,
        "an input's device time is one latency earlier"
    );
    assert_eq!(backend.now(), 5280);
}

#[test]
fn a_vanished_device_reports_once_stops_and_cannot_be_opened() {
    // The buffer is 256 frames: callbacks fall at 256 and 512, and the device vanishes before sample 480.
    let backend = host(Script::new().at(480, Event::Vanish(fake_id("mic"))));
    let probe = open_probe(&backend, "mic", config(256));
    backend.step(1000);
    assert_eq!(probe.callbacks.load(Ordering::SeqCst), 1, "only the callback at 256 ran");
    assert_eq!(*probe.errors.lock().unwrap(), [BackendError::DeviceNotAvailable]);
    assert_eq!(backend.open_streams(), 0);
    let labels: Vec<String> = backend.devices().unwrap().into_iter().map(|d| d.label).collect();
    assert_eq!(labels, ["Line In", "Speakers"]);
    assert_eq!(
        backend.default_device(Direction::Input).unwrap(),
        Some(fake_id("line")),
        "the next input becomes the default"
    );
    let refused = backend.open_input(&fake_id("mic"), config(256), Box::new(|_, _| {}), Box::new(|_| {}));
    assert!(matches!(refused, Err(BackendError::DeviceNotAvailable)));
}

#[test]
fn a_returned_device_needs_its_stream_opened_again() {
    let backend = host(Script::new().at(100, Event::Vanish(fake_id("mic"))).at(2000, Event::Return(fake_id("mic"))));
    let old = open_probe(&backend, "mic", config(256));
    backend.step(3000);
    assert_eq!(old.callbacks.load(Ordering::SeqCst), 0);
    assert_eq!(backend.devices().unwrap()[0].label, "USB Mic", "back in its place in the enumeration");
    let new = open_probe(&backend, "mic", config(256));
    backend.step(1000);
    assert_eq!(old.callbacks.load(Ordering::SeqCst), 0, "the dead stream stays dead");
    assert_eq!(new.callbacks.load(Ordering::SeqCst), 3);
}

#[test]
fn a_reorder_changes_only_the_order() {
    let backend = host(Script::new().at(10, Event::Reorder(vec![fake_id("spk"), fake_id("line"), fake_id("ghost")])));
    backend.step(10);
    let ids: Vec<String> = backend.devices().unwrap().into_iter().map(|d| d.id.id).collect();
    assert_eq!(ids, ["spk", "line", "mic"], "the listed devices first, the rest after, unknown ids ignored");
}

#[test]
fn a_device_can_come_back_with_the_same_label_and_a_new_id() {
    let backend = host(Script::new().at(300, Event::Reid { from: fake_id("mic"), to: fake_id("mic-2") }));
    let probe = open_probe(&backend, "mic", config(256));
    backend.step(1000);
    assert_eq!(probe.errors.lock().unwrap().len(), 1, "the stream on the old id died");
    let devices = backend.devices().unwrap();
    assert_eq!((devices[0].label.as_str(), devices[0].id.id.as_str()), ("USB Mic", "mic-2"));
    let old = backend.open_input(&fake_id("mic"), config(256), Box::new(|_, _| {}), Box::new(|_| {}));
    assert!(matches!(old, Err(BackendError::UnknownDevice(_))), "the old id is gone");
    assert!(backend.open_input(&fake_id("mic-2"), config(256), Box::new(|_, _| {}), Box::new(|_| {})).is_ok());
}

#[test]
fn drift_in_ppm_changes_how_many_frames_arrive() {
    // A minute at +200 ppm delivers 576 more frames than the nominal 2 880 000: about 2 more 256-frame callbacks.
    let backend = FakeBackend::new(
        vec![
            FakeDevice::input("fast", "Fast", 1).with_drift_ppm(200.0),
            FakeDevice::input("slow", "Slow", 1).with_drift_ppm(-200.0),
            FakeDevice::input("exact", "Exact", 1),
        ],
        Script::new(),
    );
    let (fast, slow, exact) = (
        open_probe(&backend, "fast", config(256)),
        open_probe(&backend, "slow", config(256)),
        open_probe(&backend, "exact", config(256)),
    );
    for _ in 0..60 {
        backend.step(48_000);
    }
    let frames = |p: &Probe| p.callbacks.load(Ordering::SeqCst) * 256;
    assert!(frames(&exact).abs_diff(2_880_000) <= 256, "{}", frames(&exact));
    assert!(frames(&fast).abs_diff(2_880_576) <= 256, "{}", frames(&fast));
    assert!(frames(&slow).abs_diff(2_879_424) <= 256, "{}", frames(&slow));
}

#[test]
fn a_drift_event_changes_the_rate_from_its_index() {
    let backend = FakeBackend::new(
        vec![FakeDevice::input("mic", "Mic", 1)],
        Script::new().at(480_000, Event::Drift { device: fake_id("mic"), ppm: 50_000.0 }),
    );
    let probe = open_probe(&backend, "mic", config(480));
    backend.step(480_000);
    let before = probe.callbacks.load(Ordering::SeqCst);
    assert_eq!(before, 999, "ten seconds of 10 ms callbacks, the last one at the step's end is not run yet");
    backend.step(480_000);
    // 5% fast: ten seconds of timeline hold 10.5 seconds of device time, 1050 callbacks, one more of them was carried.
    let after = probe.callbacks.load(Ordering::SeqCst) - before;
    assert!((1049..=1051).contains(&after), "{after}");
}

#[test]
fn latency_is_per_device_and_changes_on_script() {
    let backend = FakeBackend::new(
        vec![
            FakeDevice::input("a", "A", 1).with_latency(Seconds(0.020)),
            FakeDevice::input("b", "B", 1).with_latency(Seconds(0.002)),
            FakeDevice::output("o", "O", 1).with_latency(Seconds(0.030)),
        ],
        Script::new().at(2400, Event::Latency { device: fake_id("a"), latency: Seconds(0.040) }),
    );
    let (a, b) = (open_probe(&backend, "a", config(480)), open_probe(&backend, "b", config(480)));
    let out_times = Arc::new(Mutex::new(Vec::new()));
    let sink = out_times.clone();
    let _out = backend
        .open_output(
            &fake_id("o"),
            config(480),
            Box::new(move |_, time| sink.lock().unwrap().push(time)),
            Box::new(|_| {}),
        )
        .unwrap();
    backend.step(4800);
    let lag = |p: &Probe, i: usize| {
        let t = p.times.lock().unwrap()[i];
        t.callback.0 - t.device.0
    };
    assert!((lag(&a, 0) - 0.020).abs() < 1e-12 && (lag(&a, 7) - 0.040).abs() < 1e-12, "{} {}", lag(&a, 0), lag(&a, 7));
    assert!((lag(&b, 0) - 0.002).abs() < 1e-12);
    let t = out_times.lock().unwrap()[0];
    assert!((t.device.0 - t.callback.0 - 0.030).abs() < 1e-12, "an output is played one latency after its callback");
}

#[test]
fn inputs_carry_the_source_and_outputs_arrive_zeroed_and_are_recorded() {
    let backend = FakeBackend::new(
        vec![
            FakeDevice::input("mic", "Mic", 2).with_input(InputSource::samples(vec![1.0, 2.0, 3.0])),
            FakeDevice::output("spk", "Spk", 1).recording(),
        ],
        Script::new(),
    );
    let heard = Arc::new(Mutex::new(Vec::new()));
    let sink = heard.clone();
    let _mic = backend
        .open_input(
            &fake_id("mic"),
            StreamConfig { rate: 48_000, channels: 2, buffer_frames: 4 },
            Box::new(move |data, _| sink.lock().unwrap().extend_from_slice(data)),
            Box::new(|_| {}),
        )
        .unwrap();
    let was_zero = Arc::new(Mutex::new(Vec::new()));
    let seen = was_zero.clone();
    let _spk = backend
        .open_output(
            &fake_id("spk"),
            config(4),
            Box::new(move |data, _| {
                seen.lock().unwrap().push(data.iter().all(|s| *s == 0.0));
                data.fill(0.5);
            }),
            Box::new(|_| {}),
        )
        .unwrap();
    backend.step(9);
    // Two callbacks of 4 frames before sample 9: frames 0..8. The source ends after 3 samples, then silence.
    assert_eq!(
        *heard.lock().unwrap(),
        [1.0, 1.0, 2.0, 2.0, 3.0, 3.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]
    );
    assert_eq!(*was_zero.lock().unwrap(), [true, true], "the buffer arrives zeroed every time");
    assert_eq!(backend.played(&fake_id("spk")), [0.5; 8]);
}

#[test]
fn dropping_a_stream_stops_its_callbacks_and_a_stream_may_call_the_backend() {
    let backend = host(Script::new());
    let again = backend.clone();
    let count = Arc::new(AtomicU64::new(0));
    let c = count.clone();
    let stream = backend
        .open_input(
            &fake_id("mic"),
            config(100),
            Box::new(move |_, _| {
                // A callback can enumerate: no backend lock is held while it runs.
                assert_eq!(again.devices().unwrap().len(), 3);
                c.fetch_add(1, Ordering::SeqCst);
            }),
            Box::new(|_| {}),
        )
        .unwrap();
    backend.step(1000);
    assert_eq!(count.load(Ordering::SeqCst), 9);
    drop(stream);
    backend.step(1000);
    assert_eq!(count.load(Ordering::SeqCst), 9);
    assert_eq!(backend.open_streams(), 0);
}

#[test]
fn bad_streams_are_refused_and_the_real_buffer_size_is_reported() {
    let backend = FakeBackend::new(
        vec![FakeDevice::input("mic", "Mic", 1).with_callback_frames(480), FakeDevice::output("spk", "Spk", 2)],
        Script::new(),
    );
    let open = |name: &str, c: StreamConfig| {
        backend.open_input(&fake_id(name), c, Box::new(|_, _| {}), Box::new(|_| {})).map(|s| s.config())
    };
    assert_eq!(open("mic", config(256)).unwrap().buffer_frames, 480, "the host may differ from what was asked");
    assert!(matches!(open("ghost", config(256)), Err(BackendError::UnknownDevice(_))));
    assert!(matches!(open("spk", config(256)), Err(BackendError::Unsupported(_))), "an output is not an input");
    assert!(matches!(open("mic", StreamConfig { rate: 96_000, ..config(256) }), Err(BackendError::Unsupported(_))));
    assert!(matches!(open("mic", StreamConfig { channels: 2, ..config(256) }), Err(BackendError::Unsupported(_))));
    assert!(matches!(open("mic", config(0)), Err(BackendError::Unsupported(_))));
}

#[test]
fn a_clock_that_cannot_run_is_refused_or_fails_the_stream_instead_of_hanging_the_step() {
    let open = |device: FakeDevice| {
        let backend = FakeBackend::new(vec![device], Script::new());
        backend.open_input(&fake_id("mic"), config(256), Box::new(|_, _| {}), Box::new(|_| {})).map(|_| ())
    };
    assert!(matches!(
        open(FakeDevice::input("mic", "Mic", 1).with_callback_frames(0)),
        Err(BackendError::Unsupported(_))
    ));
    for ppm in [-1_000_000.0, -2_000_000.0, f64::NAN, f64::NEG_INFINITY] {
        let refused = open(FakeDevice::input("mic", "Mic", 1).with_drift_ppm(ppm));
        assert!(matches!(refused, Err(BackendError::Unsupported(_))), "{ppm}: {refused:?}");
    }
    // A script that stops the clock under a running stream: the stream fails, and the step ends.
    let backend = FakeBackend::new(
        vec![FakeDevice::input("mic", "Mic", 1)],
        Script::new().at(300, Event::Drift { device: fake_id("mic"), ppm: -2_000_000.0 }),
    );
    let probe = open_probe(&backend, "mic", config(100));
    backend.step(1000);
    assert_eq!(backend.now(), 1000);
    assert_eq!(probe.callbacks.load(Ordering::SeqCst), 2, "the callbacks before the drift event ran");
    assert!(matches!(probe.errors.lock().unwrap()[..], [BackendError::Other(_)]));
    assert_eq!(backend.open_streams(), 0);
}

#[test]
fn the_same_script_gives_the_same_run() {
    let run = || {
        let backend = host(
            Script::new()
                .at(700, Event::Vanish(fake_id("line")))
                .at(900, Event::Drift { device: fake_id("mic"), ppm: 1234.5 }),
        );
        let (a, b) = (open_probe(&backend, "mic", config(100)), open_probe(&backend, "line", config(64)));
        for _ in 0..20 {
            backend.step(333);
        }
        let times = a.times.lock().unwrap().iter().map(|t| t.callback.0.to_bits()).collect::<Vec<_>>();
        (times, b.callbacks.load(Ordering::SeqCst))
    };
    assert_eq!(run(), run());
}
