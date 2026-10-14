//! The walking skeleton of the animation domain (spec §39 rule 7): the contract's stages wired in the order of spec §8,
//! with the mocks standing in for the real implementations. Replace a field with a real implementation and the rest
//! keeps working, which is exactly how parallel streams are meant to land.

use omuzaki_anim_api::{
    Animator, Bus, ChannelBuf, CompiledScore, Cues, Fade, HumanoidPose, LayerStack, LifeConfig, LifeLayer, Retarget,
    Tick, TrackFrame,
};
use omuzaki_core::time::Seconds;

use crate::animator::MockAnimator;
use crate::life::MockLife;
use crate::retarget::MockRetarget;

/// Seconds a score fades in and out when it starts or stops.
const SCORE_FADE_S: f32 = 0.25;

/// What one frame produced, borrowed from the performer.
#[derive(Copy, Clone, Debug)]
pub struct Frame<'a> {
    /// The final channels (sanitised) and who owns them: what the face painter reads.
    pub bus: &'a Bus,
    /// The animation pose: what the rig reads.
    pub pose: &'a HumanoidPose,
}

/// A whole animation pipeline made of mocks. Hot path: [`MockPerformer::step`] allocates nothing.
#[derive(Debug)]
pub struct MockPerformer {
    /// Tracker frames to channels.
    pub retarget: MockRetarget,
    /// Procedural life.
    pub life: MockLife,
    /// Channels to a humanoid pose.
    pub animator: MockAnimator,
    stack: LayerStack,
    bus: Bus,
    pose: HumanoidPose,
    base: HumanoidPose,
    score: Option<(CompiledScore, Seconds)>,
    score_fade: Fade,
}

impl MockPerformer {
    /// A performer showing `rest` when nothing drives it.
    pub fn new(retarget: MockRetarget, life: LifeConfig, rest: ChannelBuf) -> Self {
        Self {
            retarget,
            life: MockLife::new(life),
            animator: MockAnimator,
            stack: LayerStack::new(rest),
            bus: Bus::default(),
            pose: HumanoidPose::REST,
            base: HumanoidPose::REST,
            score: None,
            score_fade: Fade(0.0),
        }
    }

    /// Sets the base pose the animator layers onto each frame (the character's bind; the T-pose by default).
    pub fn set_base_pose(&mut self, base: HumanoidPose) {
        self.base = base;
    }

    /// Plays `score` from `start` (pipeline time) on the score layer.
    pub fn play(&mut self, score: CompiledScore, start: Seconds) {
        self.score = Some((score, start));
    }

    /// Runs one frame: retarget the tracker frame (if one arrived), sample the score, blend, add life, clean up, animate.
    pub fn step(&mut self, tick: Tick, tracker: Option<&TrackFrame>, cues: &Cues) -> Frame<'_> {
        if let Some(frame) = tracker {
            self.retarget.apply(frame, tick.dt, &mut self.stack.tracker);
        }
        self.sample_score(tick);
        self.stack.resolve(&mut self.bus);
        self.life.step(tick, cues, &mut self.bus);
        self.bus.values.sanitize();
        self.pose = self.base;
        self.animator.animate(tick, &self.bus, &[], &mut self.pose);
        Frame { bus: &self.bus, pose: &self.pose }
    }

    fn sample_score(&mut self, tick: Tick) {
        let layer = &mut self.stack.score;
        let Some((score, start)) = &self.score else {
            layer.clear();
            return;
        };
        let local = Seconds(tick.time.0 - start.0);
        let playing = local.0 >= 0.0 && local.0 <= score.duration().0;
        score.sample(local, layer);
        layer.weight = self.score_fade.toward(if playing { 1.0 } else { 0.0 }, tick.dt, SCORE_FADE_S);
        if !playing && layer.weight == 0.0 {
            layer.clear();
        }
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)] // exact values copied through; small loop counters
mod tests {
    use omuzaki_anim_api::{Channel, ChannelMask, DeviceProfile, FrameSource, Source};

    /// `WATCHED` at frames 100, 250, 400 and 550 of the 20 s synthetic track with life seed 7, recorded on Linux x86-64.
    const GOLDEN: [[f32; 5]; 4] = [
        [0.145_447_1, 0.211_883_02, 0.101_412_7, 0.116_666_675, 0.316_667_56],
        [0.060_590_133, 0.024_831_686, 0.103_540_7, 0.083_333_34, 0.183_330_9],
        [0.033_525_057, 0.171_286_57, 0.103_289_28, -0.016_666_666, 0.683_329_76],
        [0.116_178_5, 0.146_138_09, 0.104_157_14, -0.116_666_675, 0.816_671_85],
    ];

    use super::*;
    use crate::fixtures::demo_score;
    use crate::track::{RecordedTrack, SyntheticSpec, TrackReplay, synthetic_track};

    fn performer(seed: u64) -> MockPerformer {
        MockPerformer::new(MockRetarget::new(DeviceProfile::Arkit), LifeConfig::new(seed), ChannelBuf::NEUTRAL)
    }

    /// Plays a track through the pipeline at its own frame rate; calls `visit` with every frame's bus.
    fn play(track: &RecordedTrack, seed: u64, mut visit: impl FnMut(u64, &Bus)) {
        let mut replay = TrackReplay::new(track.clone());
        let mut p = performer(seed);
        for i in 0..track.frames.len() as u64 {
            let tick = Tick::at_rate(i, track.fps);
            let frame = replay.poll(tick.time);
            visit(i, p.step(tick, frame.as_ref(), &Cues::default()).bus);
        }
    }

    /// A digest of every bus of a run, for same-machine bit-identity (decision 0009, rule 1).
    fn digest(track: &RecordedTrack, seed: u64) -> u64 {
        let mut h = 0xcbf2_9ce4_8422_2325_u64;
        play(track, seed, |_, bus| h = (h ^ bus.values.checksum()).wrapping_mul(0x0000_0100_0000_01b3));
        h
    }

    /// Channel values at fixed frames, to compare against numbers recorded on another machine.
    const WATCHED: [Channel; 5] =
        [Channel::JAW_OPEN, Channel::MOUTH_SMILE_LEFT, Channel::BROW_INNER_UP, Channel::HEAD_YAW, Channel::BREATH];

    #[test]
    fn replaying_a_recorded_track_is_bit_identical_on_this_machine() {
        let track = synthetic_track(&SyntheticSpec { seconds: 20.0, ..SyntheticSpec::default() });
        let first = digest(&track, 7);
        assert_eq!(first, digest(&track, 7));
        assert_ne!(first, digest(&track, 8), "a different life seed gives a different performance");
        // Saving and reloading the track changes nothing (decision 0009, rule 3).
        let reloaded = RecordedTrack::from_json(&track.to_json().unwrap()).unwrap();
        assert_eq!(digest(&reloaded, 7), first);
    }

    #[test]
    fn the_performance_matches_recorded_values_within_the_cross_platform_tolerance() {
        // Decision 0009, rule 2: across machines and toolchains, channels agree within 1e-5. (The mock uses plain
        // arithmetic only, so it is in fact exact today; the test states the contract, not the luck.) If these
        // numbers change, the contract's meaning changed: update them deliberately, with the reason.
        let track = synthetic_track(&SyntheticSpec { seconds: 20.0, ..SyntheticSpec::default() });
        let mut seen = Vec::new();
        play(&track, 7, |i, bus| {
            if [100, 250, 400, 550].contains(&i) {
                seen.push(WATCHED.map(|c| bus.values[c]));
            }
        });
        let expected = GOLDEN;
        assert_eq!(seen.len(), expected.len());
        for (frame, (got, want)) in seen.iter().zip(expected).enumerate() {
            for (c, (g, w)) in got.iter().zip(want).enumerate() {
                assert!((g - w).abs() <= 1e-5, "sample {frame}, {}: got {g}, want {w}", WATCHED[c]);
            }
        }
    }

    #[test]
    fn a_tracked_face_is_owned_by_the_tracker_and_life_fills_in_when_it_is_lost() {
        let track = synthetic_track(&SyntheticSpec {
            seconds: 12.0,
            lost: Some((Seconds(4.0), Seconds(12.0))),
            ..SyntheticSpec::default()
        });
        let mut replay = TrackReplay::new(track.clone());
        let mut p = performer(3);
        let (mut owned_while_tracked, mut life_blinks_after_loss) = (true, 0);
        for i in 0..track.frames.len() as u64 {
            let tick = Tick::at_rate(i, track.fps);
            let frame = replay.poll(tick.time);
            let out = p.step(tick, frame.as_ref(), &Cues::default());
            let t = tick.time.0;
            if (1.0..3.9).contains(&t) {
                owned_while_tracked &= out.bus.owners.of(Channel::EYE_BLINK_LEFT) == Source::Tracker;
            }
            if t > 6.0 && out.bus.values[Channel::EYE_BLINK_LEFT] > 0.5 {
                life_blinks_after_loss += 1;
            }
        }
        assert!(owned_while_tracked, "the tracker owns the eyes while it sees a face");
        assert!(life_blinks_after_loss > 0, "after the face is lost the life layer blinks again");
    }

    #[test]
    fn a_score_fades_in_drives_its_channels_and_fades_out() {
        let mut p = performer(1);
        let score = demo_score().unwrap().compile().unwrap();
        p.play(score, Seconds(0.0));
        let mut smile_at = |t: f64| {
            let tick = Tick { index: 0, time: Seconds(t), dt: 1.0 / 30.0 };
            p.step(tick, None, &Cues::default()).bus.values[Channel::MOUTH_SMILE_LEFT]
        };
        let mut last = 0.0;
        for i in 0..200 {
            last = smile_at(f64::from(i) / 30.0);
        }
        assert!(last < 0.2, "after the smile ends at t=4 s it has faded back (got {last} at 6.6 s)");
        let mut p = performer(1);
        p.play(demo_score().unwrap().compile().unwrap(), Seconds(0.0));
        let mut mid = 0.0;
        for i in 0..60 {
            let tick = Tick { index: i, time: Seconds(i as f64 / 30.0), dt: 1.0 / 30.0 };
            mid = p.step(tick, None, &Cues::default()).bus.values[Channel::MOUTH_SMILE_LEFT];
        }
        assert!(mid > 0.5, "the score's smile shows at 2 s (got {mid})");
        let owner = p
            .step(Tick { index: 61, time: Seconds(2.04), dt: 1.0 / 30.0 }, None, &Cues::default())
            .bus
            .owners
            .of(Channel::MOUTH_SMILE_LEFT);
        assert_eq!(owner, Source::Score);
        assert!(ChannelMask::MOUTH.contains(Channel::MOUTH_SMILE_LEFT));
    }
}
