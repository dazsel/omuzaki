//! Blinks and the ^^ arcs of a laugh: timed events on top of the lid springs.
//!
//! Gaps between blinks are Erlang-3 on top of a floor (never regular, rarely long; about 17 a minute with the extra
//! blinks that glances and head turns earn). A blink closes fast and opens slower, each lid a hair different in
//! depth and one a few milliseconds late. 12% are doubles.

use std::f32::consts::FRAC_PI_2;

use omuzaki_anim_api::{BlinkParams, Channel, ChannelBuf};
use omuzaki_core::rng::Rng;

use super::frame::{Frame, smoothstep};
use super::write::Out;


/// The state of the blink timer and the blink in flight.
#[derive(Clone, Debug)]
pub(super) struct Blinker {
    /// When the next ordinary blink starts (the layer's own clock).
    next: f64,
    active: bool,
    start: f64,
    close_s: f32,
    open_s: f32,
    lag_s: f32,
    depth: [f32; 2],
    /// When the second blink of a double starts, or infinity.
    again: f64,
    last: f64,
}

/// A lid's closure at `since` seconds after its blink began: a quarter-sine down, then a smoothstep up.
fn lid(since: f32, close_s: f32, open_s: f32) -> f32 {
    if since <= 0.0 {
        0.0
    } else if since < close_s {
        (since / close_s * FRAC_PI_2).sin()
    } else {
        let u = (since - close_s) / open_s;
        if u >= 1.0 { 0.0 } else { 1.0 - smoothstep(u) }
    }
}

/// True when the source's eyes are already closed (a score's blink, a squeeze, ^^), so a life blink would be noise.
pub(super) fn already_shut(params: &BlinkParams, src: &ChannelBuf, laugh_arcs: f32) -> bool {
    let both = |a: Channel, b: Channel, at: f32| src[a] >= at && src[b] >= at;
    let arcs = f32::midpoint(src[Channel::EYE_ARC_LEFT], src[Channel::EYE_ARC_RIGHT]).max(laugh_arcs);
    both(Channel::EYE_BLINK_LEFT, Channel::EYE_BLINK_RIGHT, params.shut_blink)
        || both(Channel::EYE_SQUINT_LEFT, Channel::EYE_SQUINT_RIGHT, params.shut_squint)
        || src[Channel::EYES_SQUEEZE] >= params.shut_arc
        || arcs >= params.shut_arc
}

impl Blinker {
    /// The first blink's gap is drawn from `rng` (the blink stream).
    pub fn new(params: &BlinkParams, rng: &mut Rng) -> Self {
        Self {
            next: f64::from(params.gap(rng)),
            active: false,
            start: 0.0,
            close_s: 0.07,
            open_s: 0.15,
            lag_s: 0.01,
            depth: [1.0; 2],
            again: f64::INFINITY,
            last: -9.0,
        }
    }

    /// A person has the eyes (or blinks are switched off): forget the blink in flight and wait a beat before the
    /// next, so handing back is calm.
    pub fn hold_off(&mut self, clock: f64, settle_s: f32) {
        self.active = false;
        self.again = f64::INFINITY;
        self.next = self.next.max(clock + f64::from(settle_s));
    }

    /// Starts a blink at `at` (which may be a little in the past, when the timer fell inside the last frame).
    fn begin(&mut self, params: &BlinkParams, rng: &mut Rng, at: f64, first: bool) {
        self.close_s = params.close_s.sample(rng.next_f32());
        self.open_s = params.open_s.sample(rng.next_f32());
        self.lag_s = params.eye_lag_s.sample(rng.next_f32());
        self.depth = [params.depth.sample(rng.next_f32()), params.depth.sample(rng.next_f32())];
        self.start = at;
        self.last = at;
        self.active = true;
        self.again = if first && rng.next_f32() < params.double_chance {
            at + f64::from(params.double_delay_s.sample(rng.next_f32()))
        } else {
            f64::INFINITY
        };
    }

    /// Runs the ordinary timers: the second blink of a double, then the next scheduled blink.
    pub fn update(&mut self, frame: &Frame<'_>, rng: &mut Rng, shut: bool) {
        let params = &frame.params.blink;
        let clock = frame.clock;
        if self.again.is_finite() && clock >= self.again {
            if !self.active && !shut {
                self.begin(params, rng, self.again, false);
            } else {
                self.again = f64::INFINITY;
            }
        }
        if clock >= self.next && !self.active {
            if shut {
                self.next = clock + f64::from(params.retry_s.sample(rng.next_f32()));
            } else {
                let at = self.next.max(clock - f64::from(frame.dt));
                self.begin(params, rng, at, true);
                self.next = at + f64::from(params.gap(rng));
            }
        }
    }

    /// An extra blink earned by a glance or a fast head turn, if one is not already under way or just done.
    pub fn extra(&mut self, frame: &Frame<'_>, rng: &mut Rng, shut: bool) {
        if !self.active && !shut && frame.clock - self.last > f64::from(frame.params.blink.extra_cooldown_s) {
            self.begin(&frame.params.blink, rng, frame.clock, true);
            self.next = frame.clock + f64::from(frame.params.blink.gap(rng));
        }
    }

    /// Writes the lids and, in a loud laugh, the arcs. Blinks never lower what the source already shows.
    pub fn write(&mut self, frame: &Frame<'_>, laugh_arcs: f32, out: &mut Out<'_>) {
        let src = frame.src;
        if self.active {
            #[allow(clippy::cast_possible_truncation)] // seconds within one blink
            let since = (frame.clock - self.start) as f32;
            let lagging = usize::from(frame.traits.right_lid_lags);
            for (side, channel) in [Channel::EYE_BLINK_LEFT, Channel::EYE_BLINK_RIGHT].into_iter().enumerate() {
                let lag = if side == lagging { self.lag_s } else { 0.0 };
                let closure = self.depth[side] * lid(since - lag, self.close_s, self.open_s);
                out.put(channel, src[channel].max(closure));
            }
            if since > self.lag_s + self.close_s + self.open_s {
                self.active = false;
            }
        }
        if frame.systems.laugh && frame.laugh_arcs {
            for channel in [Channel::EYE_ARC_LEFT, Channel::EYE_ARC_RIGHT] {
                out.put(channel, src[channel].max(laugh_arcs));
            }
        }
    }
}
