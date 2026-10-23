//! The `LifeLayer` contract: determinism, bad time, `amount`, and never fighting a person.

use omuzaki_anim_api::{Bus, Channel, ChannelMask, Cues, LifeConfig, LifeLayer, Source, Tick};

use crate::life::Life;

use super::harness::{bus_of, busy, simulate, still};

fn digest(config: LifeConfig, fps: f32, seconds: f32, scene: impl FnMut(&Tick) -> (Bus, Cues)) -> Vec<u64> {
    let mut life = Life::new(config);
    let mut sums = Vec::new();
    simulate(&mut life, fps, (seconds * fps) as u64, &mut { scene }, |_, _, bus| sums.push(bus.values.checksum()));
    sums
}

#[test]
fn the_same_seed_replays_bit_for_bit_and_other_seeds_differ() {
    let run = |seed| digest(LifeConfig::new(seed), 30.0, 20.0, busy);
    assert_eq!(run(1), run(1));
    assert_ne!(run(1), run(2));
}

#[test]
fn reset_and_clone_replay_exactly() {
    let first = digest(LifeConfig::new(5), 30.0, 20.0, busy);
    let mut life = Life::new(LifeConfig::new(5));
    let mut a = Vec::new();
    simulate(&mut life, 30.0, 300, &mut busy, |_, _, b| a.push(b.values.checksum()));
    let mut twin = life.clone();
    life.reset();
    let mut again = Vec::new();
    simulate(&mut life, 30.0, 600, &mut busy, |_, _, b| again.push(b.values.checksum()));
    assert_eq!(again, first, "reset returns to the state just after construction");

    // A clone made mid-run carries on exactly as the original does.
    let (mut left, mut right) = (twin.clone(), Vec::new());
    for i in 300..600 {
        let tick = Tick::at_rate(i, 30.0);
        let ((mut x, cues), (mut y, _)) = (busy(&tick), busy(&tick));
        left.step(tick, &cues, &mut x);
        twin.step(tick, &cues, &mut y);
        right.push((x.values.checksum(), y.values.checksum()));
    }
    assert!(right.iter().all(|(x, y)| x == y));
}

#[test]
fn blink_timing_ignores_the_callers_clock() {
    // Same seed and inputs at two caller clocks: the timers are the layer's own, so a shifted caller clock changes
    // only the noise, never when blinks happen.
    let blinks = |offset: f64| {
        let mut life = Life::new(LifeConfig::new(8));
        let mut times = Vec::new();
        for i in 0..1800u64 {
            let mut tick = Tick::at_rate(i, 30.0);
            tick.time.0 += offset;
            let mut bus = Bus::default();
            life.step(tick, &Cues::default(), &mut bus);
            if bus.values[Channel::EYE_BLINK_LEFT] > 0.3 {
                times.push(i);
            }
        }
        times
    };
    assert_eq!(blinks(0.0), blinks(1000.0));
}

#[test]
fn bad_time_changes_nothing_and_a_long_stall_counts_as_a_quarter_second() {
    let mut a = Life::new(LifeConfig::new(3));
    let mut b = a.clone();
    for i in 0..300 {
        let tick = Tick::at_rate(i, 30.0);
        for dt in [0.0, -1.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut bus = bus_of(|v| v[Channel::JAW_OPEN] = 0.3);
            let reference = bus;
            a.step(Tick { dt, ..tick }, &Cues::default(), &mut bus);
            assert_eq!(bus, reference, "dt = {dt}");
        }
        let (mut x, mut y) = (Bus::default(), Bus::default());
        a.step(tick, &Cues::default(), &mut x);
        b.step(tick, &Cues::default(), &mut y);
        assert_eq!(x, y, "the bad frames left no trace");
    }
    let (mut stalled, mut capped) = (Bus::default(), Bus::default());
    let mut long = Life::new(LifeConfig::new(3));
    let mut quarter = Life::new(LifeConfig::new(3));
    long.step(Tick { dt: 60.0, ..Tick::at_rate(0, 30.0) }, &Cues::default(), &mut stalled);
    quarter.step(Tick { dt: 0.25, ..Tick::at_rate(0, 30.0) }, &Cues::default(), &mut capped);
    assert_eq!(stalled, capped);
    assert_eq!(long.clock, 0.25);
}

#[test]
fn amount_zero_leaves_the_bus_exactly_alone_and_amount_scales_the_deviation() {
    let deviation = |amount: f32| {
        let mut config = LifeConfig::new(4);
        config.amount = amount;
        let mut life = Life::new(config);
        let mut total = 0.0;
        simulate(&mut life, 30.0, 600, &mut busy, |_, source, bus| {
            if amount == 0.0 {
                assert_eq!(bus, source, "amount 0 changes nothing, owners included");
            }
            total += ChannelMask::ALL.into_iter().map(|c| (bus.values[c] - source.values[c]).abs()).sum::<f32>();
        });
        total
    };
    let (none, half, full) = (deviation(0.0), deviation(0.5), deviation(1.0));
    assert_eq!(none, 0.0);
    assert!(half > 0.0 && half < full, "deviation {none} < {half} < {full}");
    assert!((half / full - 0.5).abs() < 0.15, "roughly proportional: {}", half / full);
}

#[test]
fn a_person_driving_everything_is_passed_through_untouched() {
    let mut life = Life::new(LifeConfig::new(3));
    let mut scene = |tick: &Tick| {
        let (mut bus, cues) = busy(tick);
        bus.owners.claim(Source::Tracker, ChannelMask::ALL);
        (bus, cues)
    };
    simulate(&mut life, 30.0, 900, &mut scene, |tick, expected, bus| {
        assert_eq!(bus, expected, "frame {}", tick.index);
    });
}

/// Each region's behaviours stop while a person drives any channel of it, and nothing else stops.
#[test]
fn each_region_is_left_alone_while_a_person_drives_it() {
    let regions: [(&str, Channel, ChannelMask); 5] = [
        ("eyes", Channel::EYE_BLINK_RIGHT, ChannelMask::EYES),
        ("gaze", Channel::EYE_LOOK_UP_LEFT, ChannelMask::GAZE),
        ("brows", Channel::BROW_INNER_UP, ChannelMask::BROWS),
        ("mouth", Channel::JAW_OPEN, ChannelMask::MOUTH),
        ("head", Channel::HEAD_ROLL, ChannelMask::HEAD),
    ];
    for (name, driven, region) in regions {
        let mut life = Life::new(LifeConfig::new(6));
        let mut moved = ChannelMask::EMPTY;
        let mut scene = |tick: &Tick| {
            let (mut bus, cues) = busy(tick);
            bus.owners.claim(Source::Tracker, ChannelMask::of(driven));
            (bus, cues)
        };
        simulate(&mut life, 30.0, 900, &mut scene, |_, source, bus| {
            for c in ChannelMask::ALL {
                if bus.values[c] != source.values[c] {
                    moved.insert(c);
                }
            }
        });
        assert!(!moved.intersects(region), "{name}: life moved {:?}", moved & region);
        let others = ChannelMask::ALL.without(region);
        assert!(moved.intersects(others), "{name}: the rest of the face is still alive");
    }
}

#[test]
fn blinks_resume_after_a_person_lets_go_of_the_eyes() {
    let mut life = Life::new(LifeConfig::new(5));
    let (mut owned_blinks, mut after) = (0, 0);
    let mut scene = |tick: &Tick| {
        let mut bus = Bus::default();
        if (2.0..14.0).contains(&(tick.time.0 as f32)) {
            bus.owners.claim(Source::Tracker, ChannelMask::EYES);
        }
        (bus, Cues::default())
    };
    simulate(&mut life, 30.0, 1200, &mut scene, |tick, _, bus| {
        let (t, blinking) = (tick.time.0, bus.values[Channel::EYE_BLINK_LEFT] > 0.0);
        if (2.0..14.0).contains(&t) {
            owned_blinks += usize::from(blinking);
        } else if t >= 14.0 && blinking {
            after += 1;
        }
    });
    assert_eq!(owned_blinks, 0);
    assert!(after > 0, "blinks come back");
}

#[test]
fn handing_back_a_region_has_no_jump() {
    // A person drives one channel of the region for three seconds, and the rest of it jumps a frame before they let
    // go. A spring that kept its old position would still be on its way; snapped springs are already at the source.
    // (driven channel, a spring-driven channel in the same region, how far it may differ: noise is written on top)
    let cases = [
        (Channel::EYE_BLINK_RIGHT, Channel::EYE_WIDE_LEFT, 0.01),
        (Channel::BROW_DOWN_RIGHT, Channel::BROW_INNER_UP, 0.0),
        (Channel::MOUTH_PRESS_LEFT, Channel::JAW_OPEN, 0.0),
        (Channel::HEAD_ROLL, Channel::HEAD_YAW, 0.01),
    ];
    for (driven, sprung, tolerance) in cases {
        let jump = if sprung == Channel::HEAD_YAW { 0.8 } else { 0.7 };
        let mut scene = |tick: &Tick| {
            let mut bus = bus_of(|v| v[sprung] = if tick.index >= 89 { jump } else { 0.1 });
            if tick.index < 90 {
                bus.owners.claim(Source::Tracker, ChannelMask::of(driven));
            }
            (bus, Cues::default())
        };
        let mut first_free = None;
        simulate(&mut Life::new(LifeConfig::new(2)), 30.0, 100, &mut scene, |tick, _, bus| {
            if tick.index == 90 {
                first_free = Some(bus.values[sprung]);
            }
        });
        let value = first_free.expect("frame 90 ran");
        assert!((value - jump).abs() <= tolerance, "{sprung}: {value} against the source {jump}");
    }
}

#[test]
fn handing_back_eases_the_couplings_in() {
    // A smile of 0.4 on the face while a person owns the eyes for four seconds: the cheek push (smile x 0.35 = +0.14
    // when free) must not land on the first free frame, and must be fully there once the ease is over.
    let mut scene = |tick: &Tick| {
        let mut bus = bus_of(|v| {
            v[Channel::MOUTH_SMILE_LEFT] = 0.4;
            v[Channel::MOUTH_SMILE_RIGHT] = 0.4;
        });
        if tick.index < 240 {
            bus.owners.claim(Source::Tracker, ChannelMask::of(Channel::EYE_BLINK_RIGHT));
        }
        (bus, Cues::default())
    };
    let mut cheek = Vec::new();
    simulate(&mut Life::new(LifeConfig::new(2)), 60.0, 330, &mut scene, |tick, _, bus| {
        if tick.index >= 239 {
            cheek.push(bus.values[Channel::CHEEK_SQUINT_LEFT]);
        }
    });
    assert_eq!(cheek[0], 0.0, "while owned the layer writes nothing to the eyes");
    assert!(cheek[1] < 0.03, "the first free frame stays near the source: {}", cheek[1]);
    assert!(cheek.windows(2).skip(1).all(|w| w[1] >= w[0] - 0.01), "and the push rises smoothly");
    let end = cheek[cheek.len() - 1];
    assert!((end - 0.14).abs() < 0.03, "then settles at smile x smile_to_lids: {end}");
}

#[test]
fn a_zero_hand_back_time_brings_everything_in_at_once() {
    let mut config = LifeConfig::new(2);
    config.params.face.hand_back_s = 0.0;
    let mut scene = |tick: &Tick| {
        let mut bus = bus_of(|v| {
            v[Channel::MOUTH_SMILE_LEFT] = 0.4;
            v[Channel::MOUTH_SMILE_RIGHT] = 0.4;
        });
        if tick.index < 60 {
            bus.owners.claim(Source::Tracker, ChannelMask::of(Channel::EYE_BLINK_RIGHT));
        }
        (bus, Cues::default())
    };
    let mut cheek = Vec::new();
    simulate(&mut Life::new(config), 60.0, 300, &mut scene, |_, _, bus| cheek.push(bus.values[Channel::CHEEK_SQUINT_LEFT]));
    assert!(cheek[299] > 0.1, "the push still arrives: {}", cheek[299]);
}

#[test]
fn a_person_driving_one_channel_in_another_region_is_still_respected() {
    // Lean, blush, breath and the ^^ arcs are written by behaviours of other regions; the writer guards them itself.
    let driven = [Channel::LEAN, Channel::BLUSH, Channel::BREATH, Channel::EYE_ARC_LEFT];
    let mut mask = ChannelMask::EMPTY;
    for c in driven {
        mask.insert(c);
    }
    let mut scene = |tick: &Tick| {
        let (mut bus, cues) = busy(tick);
        bus.values[Channel::LEAN] = 0.1;
        bus.owners.claim(Source::Tracker, mask);
        (bus, Cues { loudness: 0.95, ..cues })
    };
    simulate(&mut Life::new(LifeConfig::new(2)), 30.0, 900, &mut scene, |tick, source, bus| {
        for c in driven {
            assert_eq!(bus.values[c], source.values[c], "frame {}: {c}", tick.index);
        }
    });
}

#[test]
fn channels_the_layer_changes_are_relabelled_and_a_persons_never_are() {
    let mut life = Life::new(LifeConfig::new(3));
    let mut bus = Bus::default();
    bus.owners.claim(Source::Score, ChannelMask::ALL);
    bus.owners.claim(Source::Tracker, ChannelMask::of(Channel::JAW_OPEN));
    bus.owners.claim(Source::Trigger, ChannelMask::of(Channel::HEAD_YAW));
    let mut relabelled = ChannelMask::EMPTY;
    for i in 0..900 {
        let mut frame = bus;
        life.step(Tick::at_rate(i, 30.0), &Cues { loudness: 0.5, onset: i % 9 == 0, laugh: 0.0, ..Cues::default() }, &mut frame);
        relabelled |= frame.owners.mask_of(Source::Life);
        assert_eq!(frame.owners.of(Channel::JAW_OPEN), Source::Tracker);
        assert_eq!(frame.owners.of(Channel::HEAD_YAW), Source::Trigger);
        let total: u32 = Source::ALL.iter().map(|&s| frame.owners.mask_of(s).count()).sum();
        assert_eq!(total, ChannelMask::ALL.count(), "ownership still partitions the table");
    }
    assert!(relabelled.contains(Channel::EYE_BLINK_LEFT) && relabelled.contains(Channel::BREATH));
    assert!(!relabelled.contains(Channel::JAW_OPEN) && !relabelled.contains(Channel::HEAD_YAW));
}

#[test]
fn bad_values_on_the_bus_never_make_the_layer_produce_nan() {
    let mut life = Life::new(LifeConfig::new(9));
    let mut scene = |tick: &Tick| {
        let bad = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 40.0];
        let (mut bus, mut cues) = busy(tick);
        for (i, c) in [Channel::JAW_OPEN, Channel::HEAD_YAW, Channel::EYE_WIDE_LEFT, Channel::LEAN, Channel::BREATH]
            .into_iter()
            .enumerate()
        {
            bus.values[c] = bad[(tick.index as usize + i) % 4];
        }
        cues.loudness = f32::NAN;
        cues.laugh = f32::INFINITY;
        (bus, cues)
    };
    simulate(&mut life, 30.0, 900, &mut scene, |tick, _, bus| {
        for c in bus.owners.mask_of(Source::Life) {
            assert!(bus.values[c].is_finite(), "frame {}: {c} = {}", tick.index, bus.values[c]);
        }
    });
}

#[test]
fn set_config_keeps_state_unless_the_seed_changes() {
    let mut life = Life::new(LifeConfig::new(5));
    simulate(&mut life, 30.0, 150, &mut still, |_, _, _| {});
    let clock = life.clock;
    let mut tweaked = *life.config();
    tweaked.amount = 0.5;
    tweaked.params.face.side_lag_s = omuzaki_anim_api::Span::new(0.04, 0.04);
    life.set_config(tweaked);
    assert_eq!(life.clock, clock, "a knob turned mid-performance does not restart the layer");
    assert_eq!(life.traits().side_lag_s, 0.04, "the traits follow the new face params");
    tweaked.seed = 6;
    life.set_config(tweaked);
    assert_eq!(life.clock, 0.0);
    assert_eq!(life.traits(), Life::new(tweaked).traits());
}

#[test]
fn the_layer_never_lowers_what_the_source_already_shows() {
    let held = |_: &Tick| {
        (
            bus_of(|v| {
                v[Channel::EYE_BLINK_LEFT] = 1.0;
                v[Channel::EYE_BLINK_RIGHT] = 1.0;
                v[Channel::EYE_ARC_LEFT] = 1.0;
            }),
            Cues::default(),
        )
    };
    let mut life = Life::new(LifeConfig::new(5));
    simulate(&mut life, 30.0, 1800, &mut { held }, |_, _, bus| {
        assert_eq!(bus.values[Channel::EYE_BLINK_LEFT], 1.0);
        assert_eq!(bus.values[Channel::EYE_BLINK_RIGHT], 1.0);
        assert_eq!(bus.values[Channel::EYE_ARC_LEFT], 1.0);
    });
}
