//! A producer thread publishes 1e6 frames as fast as it can while a consumer polls: every frame the consumer sees
//! must be whole (no field from another frame) and newer than the one before, and the last frame must arrive.

#![allow(clippy::cast_precision_loss)]

use std::thread;

use omuzaki_anim_api::{ARKIT_COUNT, ChannelMask, DeviceProfile, FrameSource, TrackFrame};
use omuzaki_core::time::Seconds;
use omuzaki_track_api::mailbox;

const FRAMES: u64 = 1_000_000;

/// Frame `seq`, every field derived from `seq` so a torn read shows. Values stay exact in `f32` (below 2^24).
fn frame(seq: u64) -> TrackFrame {
    let mut f = TrackFrame::empty(seq, Seconds(seq as f64), DeviceProfile::Webcam);
    for (k, value) in f.blend.iter_mut().enumerate() {
        *value = (seq + k as u64) as f32;
    }
    f.present = ChannelMask::from_bits_truncate(u128::from(seq));
    f
}

#[test]
fn a_million_frames_arrive_whole_and_in_order() {
    let (mut tx, mut rx) = mailbox();
    let producer = thread::spawn(move || {
        for seq in 1..=FRAMES {
            tx.publish(&frame(seq));
        }
    });

    let (mut last, mut seen) = (0, 0_u64);
    while last < FRAMES {
        let Some(f) = rx.poll(Seconds(0.0)) else {
            std::hint::spin_loop();
            continue;
        };
        assert!(f.seq > last, "frame {} after {last}", f.seq);
        assert_eq!(f.time, Seconds(f.seq as f64), "time of frame {}", f.seq);
        assert_eq!(f.present, frame(f.seq).present, "mask of frame {}", f.seq);
        for k in 0..ARKIT_COUNT {
            assert_eq!(f.blend[k], (f.seq + k as u64) as f32, "blend {k} of frame {}", f.seq);
        }
        last = f.seq;
        seen += 1;
    }
    producer.join().unwrap();
    assert!(seen > 0 && rx.poll(Seconds(0.0)).is_none(), "the last frame was taken once");
}
