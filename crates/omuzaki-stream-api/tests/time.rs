//! Timebase rescaling and the byte meter.

use bytes::Bytes;
use omuzaki_stream_api::packet::*;
use omuzaki_stream_api::time::Timebase;

const fn tb(num: u32, den: u32) -> Timebase {
    Timebase { num, den }
}

#[test]
fn rescale_rounds_to_nearest_ties_away_from_zero() {
    assert_eq!(tb(1, 30).rescale(1, tb(1, 48000)), Ok(1600));
    assert_eq!(tb(1, 30).rescale(3, tb(1, 48000)), Ok(4800));
    let down = |v| tb(1, 48000).rescale(v, tb(1, 30));
    assert_eq!((down(1599), down(799), down(800)), (Ok(1), Ok(0), Ok(1)));
    assert_eq!(down(-800), Ok(-1));
}

#[test]
fn rescale_is_exact_for_huge_values_and_rejects_zero() {
    let big = i64::MAX / 2;
    assert_eq!(tb(1, 1000).rescale(big, tb(1, 1000)), Ok(big));
    assert!(tb(1, 1).rescale(i64::MAX, tb(1, 1000)).is_err(), "overflow is an error");
    assert!(tb(1, 0).rescale(1, tb(1, 30)).is_err());
    assert!(tb(1, 30).rescale(1, tb(0, 30)).is_err());
}

#[test]
fn byte_meter_balances_across_threads() {
    let meter = ByteMeter::new();
    std::thread::scope(|s| {
        for _ in 0..4 {
            let m = meter.clone();
            s.spawn(move || {
                for _ in 0..1000 {
                    m.add(3);
                    m.sub(3);
                }
            });
        }
    });
    meter.add(5);
    meter.sub(10);
    assert_eq!(meter.get(), 0, "sub stops at zero");
}

#[test]
fn cloning_a_packet_shares_its_bytes() {
    let side = SideData { kind: SideKind::BlockAdditional { id: 1 }, data: Bytes::from(vec![7; 64]) };
    let data = Bytes::from(vec![1; 64]);
    let p = Packet { stream: StreamId(0), pts: 0, dts: 0, duration: 1, keyframe: true, data, side_data: vec![side] };
    let q = p.clone();
    assert_eq!(p.data.as_ptr(), q.data.as_ptr());
    assert_eq!(p.side_data[0].data.as_ptr(), q.side_data[0].data.as_ptr());
}
