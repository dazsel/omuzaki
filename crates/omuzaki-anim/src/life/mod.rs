//! The life layer (spec PF-24), ported from the prototype's `life.js`: the procedural "in between" that keeps a
//! character alive when no human is driving it, and fills in when tracking drops.
//!
//! [`Life`] implements [`LifeLayer`]. The incoming bus is the **target**: springs chase the values already on it, and
//! blinks, gaze, couplings, laughs, breathing and noise ride on top. Everything is a function of the config (with its
//! seed) and the sequence of `(tick, cues, bus)` inputs, so a replay is bit-identical on one machine (decision 0009),
//! and a frame allocates nothing.
//!
//! ## What the layer does
//!
//! | Region | Needs it free | Behaviours |
//! |---|---|---|
//! | `EYES` | blinks, lids | blinks (never regular, 12% doubles), lid springs, the cheek push of a smile, the squint of a laugh, surprise widening, lid drift, ^^ arcs |
//! | `GAZE` | saccades | micro-saccades, glances, fixation drift, eyes leading a head turn |
//! | `BROWS` | kicks | a kick on each stressed syllable, per-side wobble |
//! | `MOUTH` | springs | jaw, smile, stretch, round and pucker springs, mouth-corner wobble, the smile that flushes the cheeks |
//! | `HEAD` | nods | soft head springs, speech nods, noise (more while talking), the laugh bob, lean |
//!
//! Breathing writes only the `breath` channel (and a hair of head pitch), so it runs whoever drives the rest.
//!
//! ## Rules it keeps (the contract, `LifeLayer::step`)
//!
//! - **No time, no change.** A zero, negative or non-finite `dt` leaves the bus and the layer untouched; `dt` above
//!   0.25 s counts as 0.25 s.
//! - **Humans are never overridden.** `write::Out` is the only writer and skips channels a person drives. A
//!   behaviour that needs a region is skipped while a person drives any channel of it, and the matching springs snap
//!   to the incoming values so handing back has no jump.
//! - **Amount.** `incoming + amount * (processed - incoming)` for every channel; `0` leaves the bus exactly alone.
//! - **Ownership.** Channels whose value changed are relabelled [`Source::Life`].
//! - **Seeded.** Four [`LifeStreams`] (blink, gaze, head, traits) and the noise lattice come from the seed alone.
//!
//! ## How the prototype's parameters became `ARKit` channels
//!
//! The prototype ran on the painter's own parameters; this layer runs on the 52 `ARKit` channels (spec FE-01), following
//! the port map in `docs/design/anim-api.md` section 11. Where one prototype parameter was a *net* of two opposed
//! channels (smile against frown, brow up against brow down, looking out against in, lid wide against squint), noise and
//! drift move the net: `Out::nudge` cancels the opposing channel before raising the other, so
//! half of a wobble is never clipped away at zero. Springs act on each channel separately, which is the same thing for
//! a linear spring. Differences from the prototype, all deliberate:
//!
//! - brows are their own region (a tracker that supplies only the eyes leaves the brows alive);
//! - the lower-lid raise of a smile goes to `cheekSquint` and the squint of a laugh to `eyeSquint`;
//! - there is no brow-tilt wobble (no single `ARKit` channel is a tilt; the painter derives it);
//! - breathing keeps whatever a score already shows (`max`) instead of overwriting it;
//! - laughter detected in the audio ([`Cues::laugh`]) is treated as extra loudness for the laugh logic only;
//! - the timers are `f64`, so a stream running for hours keeps its timing.

mod bank;
mod blink;
mod brows;
mod eyes;
mod frame;
mod gaze;
mod head;
mod laugh;
mod lead;
mod mouth;
mod noise;
mod spring;
mod write;

#[cfg(test)]
mod tests;

use omuzaki_anim_api::{Bus, Channel, Cues, LifeConfig, LifeLayer, LifeStreams, LifeTraits, Source, Tick};

use self::blink::{Blinker, already_shut};
use self::brows::Brows;
use self::eyes::Eyes;
use self::frame::{Ease, Frame, Gate};
use self::gaze::Wander;
use self::head::Head;
use self::laugh::Laugh;
use self::mouth::Mouth;
use self::noise::NoiseSeeds;
use self::spring::Substeps;
use self::write::Out;

/// A frame longer than this counts as this long, so a stall (a dragged window, a breakpoint) cannot make springs
/// and timers jump.
const MAX_DT: f32 = 0.25;

/// The real life layer. One instance per character; it owns its springs, timers and random streams.
#[derive(Clone, Debug)]
pub struct Life {
    config: LifeConfig,
    streams: LifeStreams,
    traits: LifeTraits,
    noise: NoiseSeeds,
    /// The layer's own timer: advances by `dt`, so a scrubbing caller clock cannot derail blinks and glances.
    clock: f64,
    mouth: Mouth,
    laugh: Laugh,
    brows: Brows,
    eyes: Eyes,
    head: Head,
    blink: Blinker,
    gaze: Wander,
    ease: Ease,
}

/// A cue value forced into `0..=1` (NaN counts as 0).
fn unit(x: f32) -> f32 {
    if x.is_nan() { 0.0 } else { x.clamp(0.0, 1.0) }
}

impl Life {
    /// A layer in the state a fresh construction with this config always has: the first blink, glance and saccade
    /// are already scheduled from the seed.
    pub fn new(config: LifeConfig) -> Self {
        let mut streams = LifeStreams::new(config.seed);
        let traits = LifeTraits::draw(&mut streams.traits, &config.params.face);
        let noise = NoiseSeeds::new(traits.noise_seed);
        let blink = Blinker::new(&config.params.blink, &mut streams.blink);
        let gaze = Wander::new(&config.params.gaze, &mut streams.gaze);
        let head = Head::new(&mut streams.head);
        Self {
            config,
            streams,
            traits,
            noise,
            clock: 0.0,
            mouth: Mouth::new(),
            laugh: Laugh::default(),
            brows: Brows::new(),
            eyes: Eyes::new(),
            head,
            blink,
            gaze,
            ease: Ease::FULL,
        }
    }

    /// The quirks this seed gave the character (which side leads, which lid lags).
    pub fn traits(&self) -> &LifeTraits {
        &self.traits
    }
}

impl LifeLayer for Life {
    fn step(&mut self, tick: Tick, cues: &Cues, bus: &mut Bus) {
        if !tick.dt.is_finite() || tick.dt <= 0.0 {
            return;
        }
        let dt = tick.dt.min(MAX_DT);
        self.clock += f64::from(dt);

        let before = bus.values;
        let mut src = before;
        src.sanitize();
        let taken = bus.owners.human_mask();
        let config = &self.config;
        let gate = Gate::new(taken);
        self.ease.advance(gate, dt, config.params.face.hand_back_s);
        let loud = unit(cues.loudness);
        let frame = Frame {
            params: &config.params,
            systems: config.systems,
            traits: &self.traits,
            noise: &self.noise,
            src: &src,
            taken,
            gate,
            ease: self.ease,
            laugh_arcs: config.laugh_arcs,
            dt,
            clock: self.clock,
            time: if tick.time.0.is_finite() { tick.time.0 } else { self.clock },
            sub: Substeps::plan(dt, &config.params.springs),
            loud,
            onset: cues.onset,
            laugh_loud: loud.max(unit(cues.laugh)),
        };
        let amount = if config.amount.is_finite() { config.amount.max(0.0) } else { 1.0 };
        let mut out = Out::new(&mut bus.values, &src, taken, amount);

        let mouth = self.mouth.step(&frame, &mut out);
        let smile = f32::midpoint(
            src[Channel::MOUTH_SMILE_LEFT] - src[Channel::MOUTH_FROWN_LEFT],
            src[Channel::MOUTH_SMILE_RIGHT] - src[Channel::MOUTH_FROWN_RIGHT],
        );
        let laugh = self.laugh.step(&frame, smile, src[Channel::JAW_OPEN], frame.laugh_loud);
        self.brows.step(&frame, &mut self.streams.head, &mut out);
        self.eyes.step(&frame, mouth, &laugh, &mut out);
        let lead = self.head.step(&frame, &laugh, &mut self.streams.head, &mut out);

        let blink = &config.params.blink;
        let blinking = frame.gate.eyes && frame.systems.blink;
        let shut = already_shut(blink, &src, laugh.arcs);
        if blinking {
            self.blink.update(&frame, &mut self.streams.blink, shut);
            if lead.started && self.streams.blink.next_f32() < blink.on_head_turn_chance {
                self.blink.extra(&frame, &mut self.streams.blink, shut);
            }
        } else {
            self.blink.hold_off(self.clock, blink.settle_s);
        }
        let gaze = self.gaze.step(&frame, &mut self.streams.gaze, lead.gaze, lead.active, &mut out);
        if blinking && gaze.glance_started && self.streams.blink.next_f32() < blink.on_glance_chance {
            self.blink.extra(&frame, &mut self.streams.blink, shut);
        }
        if frame.gate.eyes {
            self.blink.write(&frame, laugh.arcs, &mut out);
        }

        let changed = out.changed(&before);
        bus.owners.claim(Source::Life, changed);
    }

    fn config(&self) -> &LifeConfig {
        &self.config
    }

    fn set_config(&mut self, config: LifeConfig) {
        if config.seed == self.config.seed {
            // The state carries on. The traits depend on the face params (the side lag), so they are drawn again;
            // they come from the first draws of their own stream, so everything else about them is unchanged.
            self.traits = LifeTraits::draw(&mut LifeStreams::new(config.seed).traits, &config.params.face);
            self.config = config;
        } else {
            *self = Self::new(config);
        }
    }

    fn reset(&mut self) {
        *self = Self::new(self.config);
    }
}
