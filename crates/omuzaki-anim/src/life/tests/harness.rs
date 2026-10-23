//! Shared scenes, runners and small series helpers for the life layer's tests.

use omuzaki_anim_api::{Bus, Channel, ChannelBuf, Cues, LifeConfig, LifeLayer, Tick};

use crate::life::Life;

/// What a scene feeds the layer on one frame: the incoming bus (values and owners) and the audio cues.
pub(super) type Scene<'a> = &'a mut dyn FnMut(&Tick) -> (Bus, Cues);

/// A still, relaxed face: nothing driving anything.
pub(super) fn still(_: &Tick) -> (Bus, Cues) {
    (Bus::default(), Cues::default())
}

/// A bus whose values are set by `f`, owned by nobody.
pub(super) fn bus_of(f: impl FnOnce(&mut ChannelBuf)) -> Bus {
    let mut bus = Bus::default();
    f(&mut bus.values);
    bus
}

/// Runs `frames` frames at `fps`, handing `observe` each frame's incoming bus and the bus the layer left.
pub(super) fn simulate(
    life: &mut Life,
    fps: f32,
    frames: u64,
    scene: Scene<'_>,
    mut observe: impl FnMut(&Tick, &Bus, &Bus),
) {
    for i in 0..frames {
        let tick = Tick::at_rate(i, fps);
        let (incoming, cues) = scene(&tick);
        let mut bus = incoming;
        life.step(tick, &cues, &mut bus);
        observe(&tick, &incoming, &bus);
    }
}

/// A bus and cues with a bit of everything going on: a smile, talking, a head turn, a laugh.
pub(super) fn busy(tick: &Tick) -> (Bus, Cues) {
    let t = tick.time.0 as f32;
    let laugh = (12.0..14.0).contains(&t);
    let talk = t >= 15.0;
    let bus = bus_of(|v| {
        v[Channel::MOUTH_SMILE_LEFT] = if (5.0..8.0).contains(&t) || laugh { 1.0 } else { 0.15 };
        v[Channel::MOUTH_SMILE_RIGHT] = v[Channel::MOUTH_SMILE_LEFT];
        v[Channel::JAW_OPEN] = if laugh {
            0.6
        } else if talk {
            0.4
        } else {
            0.02
        };
        v[Channel::HEAD_YAW] = if t >= 9.0 { 0.35 } else { 0.0 };
    });
    let loudness = if laugh {
        0.8
    } else if talk {
        0.45 + 0.25 * (t * 7.0).sin()
    } else {
        0.0
    };
    (bus, Cues { loudness, onset: talk && tick.index.is_multiple_of(8), laugh: 0.0, ..Cues::default() })
}

/// Runs a scene and keeps one number per frame.
pub(super) fn series(
    config: LifeConfig,
    fps: f32,
    seconds: f32,
    scene: impl FnMut(&Tick) -> (Bus, Cues),
    read: impl Fn(&Bus) -> f32,
) -> Vec<f32> {
    let mut out = Vec::new();
    simulate(&mut Life::new(config), fps, (seconds * fps) as u64, &mut { scene }, |_, _, bus| out.push(read(bus)));
    out
}

pub(super) fn peak(values: &[f32]) -> f32 {
    values.iter().copied().fold(f32::MIN, f32::max)
}

pub(super) fn trough(values: &[f32]) -> f32 {
    values.iter().copied().fold(f32::MAX, f32::min)
}

/// Frame-by-frame difference of two series.
pub(super) fn minus(a: &[f32], b: &[f32]) -> Vec<f32> {
    a.iter().zip(b).map(|(x, y)| x - y).collect()
}

/// A broad smile with the jaw open by `jaw` at `loudness`: a laugh when both are big enough (closing the jaw makes the
/// same scene a non-laugh with the same loudness, so the two differ only by the laugh).
pub(super) fn laughing(loudness: f32, jaw: f32) -> impl FnMut(&Tick) -> (Bus, Cues) {
    move |_| {
        let bus = bus_of(|v| {
            for c in [Channel::MOUTH_SMILE_LEFT, Channel::MOUTH_SMILE_RIGHT] {
                v[c] = 1.0;
            }
            v[Channel::JAW_OPEN] = jaw;
        });
        (bus, Cues { loudness, ..Cues::default() })
    }
}
