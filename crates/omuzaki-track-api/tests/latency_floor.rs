//! The mapped timestamp is the arrival floor (capture plus the smallest transport delay), so latency measured from it
//! to publish leaves a constant transport delay out, as the `LatencyStats` docs say. No real time: every instant is
//! computed, so the test is deterministic.

// Seconds from integer nanoseconds; the magnitudes stay far below f64's exact range.
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use omuzaki_core::time::Seconds;
use omuzaki_track_api::camera::DeviceTime;
use omuzaki_track_api::clock::ClockMap;

/// The constant best-case transport delay (USB or driver), in seconds.
const TRANSPORT: f64 = 0.020;
/// Time the tracker spends on a frame after it arrives, in seconds.
const PROCESSING: f64 = 0.005;
/// Tolerance well below a microsecond, far above the nanosecond rounding of the stamps.
const EPSILON: f64 = 1e-6;

#[test]
fn a_constant_transport_delay_is_left_out_of_the_reported_latency() {
    let mut clock = ClockMap::new();
    for i in 0..120_u64 {
        let captured_ns = i * 33_333_333;
        let captured = captured_ns as f64 * 1e-9;
        let arrived = captured + TRANSPORT;
        let published = arrived + PROCESSING;

        let mapped = clock.map(DeviceTime(captured_ns), Seconds(arrived)).unwrap().0;

        // The mapped timestamp is the arrival floor: capture plus the transport delay.
        assert!((mapped - (captured + TRANSPORT)).abs() < EPSILON, "frame {i}: mapped {mapped}, captured {captured}");

        // Latency as the docs define it: from the arrival floor to publish. It leaves the 20 ms out ...
        let reported = published - mapped;
        assert!((reported - PROCESSING).abs() < EPSILON, "frame {i}: reported {reported}");

        // ... so adding the transport delay back gives the capture-to-publish figure.
        let capture_to_publish = published - captured;
        assert!((capture_to_publish - reported - TRANSPORT).abs() < EPSILON, "frame {i}: {capture_to_publish}");
    }
}
