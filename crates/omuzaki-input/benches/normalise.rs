//! `normalise` plus `match` per key event (design §13 IN-3B: at most 1 µs on `Dev`): a full table of 64 key
//! bindings, keys pressed and released through the raw injector, the rings drained between batches.

use std::sync::Arc;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_input::{Hub, HubConfig, TestBackend};
use omuzaki_input_api::binding::{BindingAction, BindingId};
use omuzaki_input_api::capture::IndicatorHandle;
use omuzaki_input_api::event::{DeviceClass, DeviceMask};
use omuzaki_input_api::feed::{InputHub, IntentFrame, MotionFrame};

fn bench(c: &mut Criterion) {
    let clock = Arc::new(FakeClock::new(Seconds(1.0)));
    let keyboard = TestBackend { name: "test.keyboard", classes: DeviceMask::of(&[DeviceClass::Keyboard]) };
    let Ok(hub) = Hub::new(HubConfig::new(clock, None).with_test_backends(&[keyboard]).manual()) else { return };
    let control = hub.control();
    let mut ids: Vec<BindingId> = Vec::new();
    for slot in 0..64u8 {
        if let Ok(id) = control.add_binding("bench", DeviceClass::Keyboard, BindingAction::Trigger(slot)) {
            ids.push(id);
        }
    }
    for (id, usage) in ids.iter().zip(0x04u16..) {
        let _bound = hub.bind_key(*id, usage, 0);
    }
    let indicator: IndicatorHandle = hub.indicator_host().install();
    indicator.beat();
    let _started = control.start(DeviceMask::ALL);
    let Some((mut motion, mut intents, mut bindings)) = hub.take_feeds() else { return };
    let mut injector = hub.injector(0);
    injector.device_up(1, DeviceClass::Keyboard);
    let (mut motion_frame, mut intent_frame) = (MotionFrame::new(), IntentFrame::new());
    let mut usage = 0x04u16;
    c.bench_function("normalise and match a key down and up (two events)", |b| {
        b.iter(|| {
            injector.key(1, usage, true);
            injector.key(1, usage, false);
            usage = if usage >= 0x43 { 0x04 } else { usage + 1 };
            if usage == 0x04 {
                motion.drain(Seconds(f64::MAX), &mut motion_frame);
                intents.drain(Seconds(f64::MAX), &mut intent_frame);
                while bindings.poll().is_some() {}
            }
        });
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
