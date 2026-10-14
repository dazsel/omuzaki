//! A deterministic stand-in for the life layer: seeded blinks, micro-saccades and breathing, honouring the ownership
//! rules of the contract. It has no springs, couplings, laugh or head motion (that is the real layer's job), and it
//! draws blink gaps uniformly instead of with the real Erlang shape, so its output depends only on plain arithmetic
//! (no libm), which keeps it far inside decision 0009's cross-platform tolerance.

use omuzaki_anim_api::{
    Bus, Channel, ChannelMask, Cues, LifeConfig, LifeLayer, LifeStreams, LifeTraits, Source, Span, Tick,
};

#[derive(Copy, Clone, Debug)]
struct ActiveBlink {
    start: f32,
    close: f32,
    open: f32,
    depth: [f32; 2],
    lag: [f32; 2],
}

/// See the module docs.
#[derive(Clone, Debug)]
pub struct MockLife {
    config: LifeConfig,
    streams: LifeStreams,
    traits: LifeTraits,
    /// Our own timer: advances by `dt`, so a scrubbing caller clock cannot derail it.
    clock: f32,
    next_blink: f32,
    blink: Option<ActiveBlink>,
    next_saccade: f32,
    gaze: (f32, f32),
    breath: f32,
}

impl MockLife {
    /// A layer in the state a fresh construction with this config always has.
    pub fn new(config: LifeConfig) -> Self {
        let mut streams = LifeStreams::new(config.seed);
        let traits = LifeTraits::draw(&mut streams.traits, &config.params.face);
        let next_blink = Self::gap(&config, &mut streams);
        let next_saccade = config.params.gaze.saccade_gap_s.sample(streams.gaze.next_f32());
        Self {
            config,
            streams,
            traits,
            clock: 0.0,
            next_blink,
            blink: None,
            next_saccade,
            gaze: (0.0, 0.0),
            breath: 0.0,
        }
    }

    /// The quirks this seed gave the character.
    pub fn traits(&self) -> &LifeTraits {
        &self.traits
    }

    fn gap(config: &LifeConfig, streams: &mut LifeStreams) -> f32 {
        let b = &config.params.blink;
        Span::new(b.gap_floor_s, b.gap_cap_s).sample(streams.blink.next_f32())
    }

    fn start_blink(&mut self) {
        let (p, rng) = (&self.config.params.blink, &mut self.streams.blink);
        let lag = p.eye_lag_s.sample(rng.next_f32());
        let (lag_left, lag_right) = if self.traits.right_lid_lags { (0.0, lag) } else { (lag, 0.0) };
        self.blink = Some(ActiveBlink {
            start: self.clock,
            close: p.close_s.sample(rng.next_f32()),
            open: p.open_s.sample(rng.next_f32()),
            depth: [p.depth.sample(rng.next_f32()), p.depth.sample(rng.next_f32())],
            lag: [lag_left, lag_right],
        });
        self.next_blink = self.clock + Self::gap(&self.config, &mut self.streams);
    }

    /// Lids close fast and open slower: a linear down-and-up shape in `0..=1`.
    fn lid(b: &ActiveBlink, since: f32) -> f32 {
        if since <= 0.0 {
            0.0
        } else if since < b.close {
            since / b.close
        } else if since < b.close + b.open {
            1.0 - (since - b.close) / b.open
        } else {
            0.0
        }
    }

    fn blinks(&mut self, bus: &mut Bus, amount: f32) -> ChannelMask {
        if self.blink.is_none() && self.clock >= self.next_blink {
            self.start_blink();
        }
        let Some(b) = self.blink else { return ChannelMask::EMPTY };
        let since = self.clock - b.start;
        if since > b.close + b.open + b.lag[0].max(b.lag[1]) {
            self.blink = None;
            return ChannelMask::EMPTY;
        }
        let sides = [(Channel::EYE_BLINK_LEFT, 0), (Channel::EYE_BLINK_RIGHT, 1)];
        for (channel, side) in sides {
            let shut = b.depth[side] * Self::lid(&b, since - b.lag[side]);
            let incoming = bus.values[channel];
            bus.values[channel] = incoming + amount * (incoming.max(shut) - incoming);
        }
        ChannelMask::of(Channel::EYE_BLINK_LEFT) | ChannelMask::of(Channel::EYE_BLINK_RIGHT)
    }

    fn saccades(&mut self, bus: &mut Bus, amount: f32) -> ChannelMask {
        let g = &self.config.params.gaze;
        if self.clock >= self.next_saccade {
            let rng = &mut self.streams.gaze;
            let size = g.saccade_size.sample(rng.next_f32());
            let (dx, dy) = (rng.range_f32(-1.0, 1.0), rng.range_f32(-1.0, 1.0));
            let (x, y) = (self.gaze.0 + size * dx, self.gaze.1 + size * dy);
            // Steer back toward centre rather than wandering off.
            self.gaze = if x * x + y * y > g.saccade_radius * g.saccade_radius {
                (0.5 * self.gaze.0, 0.5 * self.gaze.1)
            } else {
                (x, y)
            };
            self.next_saccade = self.clock + g.saccade_gap_s.sample(rng.next_f32());
        }
        let (x, y) = bus.values.gaze();
        bus.values.set_gaze(x + amount * self.gaze.0, y + amount * self.gaze.1);
        ChannelMask::GAZE
    }

    fn breathe(&mut self, bus: &mut Bus, amount: f32, dt: f32) -> ChannelMask {
        self.breath = (self.breath + self.config.params.head.breath_hz * dt).fract();
        let wave = 1.0 - (2.0 * self.breath - 1.0).abs();
        bus.values[Channel::BREATH] = amount * wave;
        ChannelMask::of(Channel::BREATH)
    }
}

impl LifeLayer for MockLife {
    fn step(&mut self, tick: Tick, _cues: &Cues, bus: &mut Bus) {
        if !tick.dt.is_finite() || tick.dt <= 0.0 {
            return;
        }
        let dt = tick.dt.min(0.25);
        self.clock += dt;
        let taken = bus.owners.human_mask();
        let (systems, amount) = (self.config.systems, self.config.amount);
        let mut changed = ChannelMask::EMPTY;
        if systems.blink && !taken.intersects(ChannelMask::EYES) {
            changed |= self.blinks(bus, amount);
        } else {
            // A person has the eyes: forget any blink in flight and wait a beat before the next, so hand-back is calm.
            self.blink = None;
            self.next_blink = self.next_blink.max(self.clock + 0.3);
        }
        if systems.gaze && !taken.intersects(ChannelMask::GAZE) {
            changed |= self.saccades(bus, amount);
        }
        if systems.breathing && !taken.intersects(ChannelMask::BODY) {
            changed |= self.breathe(bus, amount, dt);
        }
        bus.owners.claim(Source::Life, changed.without(taken));
    }

    fn config(&self) -> &LifeConfig {
        &self.config
    }

    fn set_config(&mut self, config: LifeConfig) {
        if config.seed == self.config.seed {
            self.config = config;
        } else {
            *self = Self::new(config);
        }
    }

    fn reset(&mut self) {
        *self = Self::new(self.config);
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)] // exact values copied through; small loop counters
mod tests {
    use omuzaki_anim_api::{ChannelBuf, Ownership};

    use super::*;

    fn run(life: &mut MockLife, bus: &mut Bus, seconds: f32) -> Vec<f32> {
        let mut blink = Vec::new();
        for i in 0..(seconds * 60.0) as u64 {
            life.step(Tick::at_rate(i, 60.0), &Cues::default(), bus);
            blink.push(bus.values[Channel::EYE_BLINK_LEFT]);
            bus.values = ChannelBuf::NEUTRAL; // the next frame's incoming bus
        }
        blink
    }

    fn bus_with_owner(source: Source, mask: ChannelMask) -> Bus {
        let mut bus = Bus::default();
        bus.owners.claim(source, mask);
        bus
    }

    #[test]
    fn same_seed_same_blinks_and_other_seeds_differ() {
        let go = |seed| run(&mut MockLife::new(LifeConfig::new(seed)), &mut Bus::default(), 30.0);
        assert_eq!(go(5), go(5));
        assert_ne!(go(5), go(6));
        let blinks = go(5).windows(2).filter(|w| w[0] == 0.0 && w[1] > 0.0).count();
        assert!((5..=30).contains(&blinks), "{blinks} blinks in 30 s");
    }

    #[test]
    fn reset_replays_from_the_start() {
        let mut life = MockLife::new(LifeConfig::new(3));
        let first = run(&mut life, &mut Bus::default(), 10.0);
        life.reset();
        assert_eq!(run(&mut life, &mut Bus::default(), 10.0), first);
    }

    #[test]
    fn a_human_owning_the_eyes_stops_blinks_but_not_breathing() {
        let mut life = MockLife::new(LifeConfig::new(5));
        let mut bus = bus_with_owner(Source::Tracker, ChannelMask::of(Channel::EYE_BLINK_RIGHT));
        let blinks = run(&mut life, &mut bus, 30.0);
        assert!(blinks.iter().all(|&v| v == 0.0), "never writes an eye channel while a human drives the eyes");
        let mut still = bus_with_owner(Source::Tracker, ChannelMask::of(Channel::EYE_BLINK_RIGHT));
        life.step(Tick::at_rate(0, 60.0), &Cues::default(), &mut still);
        assert!(still.values[Channel::BREATH] >= 0.0 && still.owners.of(Channel::BREATH) == Source::Life);
        assert_eq!(still.owners.of(Channel::EYE_BLINK_RIGHT), Source::Tracker, "ownership of human channels is kept");
    }

    #[test]
    fn it_never_lowers_what_the_source_already_shows() {
        let mut life = MockLife::new(LifeConfig::new(5));
        let mut bus = Bus::default();
        bus.values[Channel::EYE_BLINK_LEFT] = 1.0; // a score holds the eye shut
        for i in 0..600 {
            life.step(Tick::at_rate(i, 60.0), &Cues::default(), &mut bus);
            assert_eq!(bus.values[Channel::EYE_BLINK_LEFT], 1.0);
        }
    }

    #[test]
    fn paused_or_bad_time_changes_nothing() {
        let mut life = MockLife::new(LifeConfig::new(5));
        for dt in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            let mut bus = Bus::default();
            life.step(Tick { index: 0, time: omuzaki_core::time::Seconds(0.0), dt }, &Cues::default(), &mut bus);
            assert_eq!(bus, Bus::default(), "dt = {dt}");
        }
    }

    #[test]
    fn amount_zero_leaves_the_bus_alone_and_systems_can_be_switched_off() {
        let mut off = LifeConfig::new(5);
        off.amount = 0.0;
        let mut life = MockLife::new(off);
        let mut bus = Bus::default();
        for i in 0..600 {
            life.step(Tick::at_rate(i, 60.0), &Cues::default(), &mut bus);
        }
        assert_eq!(bus.values, ChannelBuf::NEUTRAL);

        let mut no_blink = LifeConfig::new(5);
        no_blink.systems.blink = false;
        let silent = run(&mut MockLife::new(no_blink), &mut Bus::default(), 30.0);
        assert!(silent.iter().all(|&v| v == 0.0));
        assert_eq!(Bus::default().owners, Ownership::all_rest());
    }

    #[test]
    fn changing_the_seed_resets_but_other_knobs_keep_state() {
        let mut life = MockLife::new(LifeConfig::new(5));
        run(&mut life, &mut Bus::default(), 5.0);
        let clock = life.clock;
        let mut tweak = *life.config();
        tweak.amount = 0.5;
        life.set_config(tweak);
        assert_eq!(life.clock, clock);
        tweak.seed = 6;
        life.set_config(tweak);
        assert_eq!(life.clock, 0.0);
    }
}
