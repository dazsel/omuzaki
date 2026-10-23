//! The AI mouth drive against `ai-performer-voice-face.md` §6.1 and §6.3 (#1311): "Hi chat!" from `FakeTts` through
//! `FakeVoiceToFace` (16 kHz, 20 ms blocks, queue 1), a fake play log of 5 ms engine blocks, the stage's
//! `PlayoutDelay` sampled chronologically. Keys, ticks, release times and sampled output to 3 decimals; `D` = 0 and
//! 200 ms give the same output at the same program times; cuts, the immediate resume, stalls (on time, descheduled,
//! 1,024-frame blocks), warm-up, missed blocks and the crossfade, a lead below `lead_min`, private speech.

#![allow(
    clippy::unwrap_used,
    clippy::float_cmp,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::similar_names
)]

use std::sync::{Arc, Mutex};

use omuzaki_ai_api::{StreamSpec, TensorBuf};
use omuzaki_ai_mock::{FakeVoiceToFace, InlineStream, fake_speech, jaw_open, voice_to_face_signature};
use omuzaki_anim_api::playout::PlayoutDelay;
use omuzaki_anim_api::{Channel, ChannelBuf, Layer};
use omuzaki_core::time::Seconds;
use omuzaki_performer_api::voice::{CutProbe, CutSource, CutState, PlayGap, Played};
use omuzaki_track_voice::ai_mouth::{EnergyVisemes, MouthConfig, MouthDrive, Visemes, lead, lead_min};

const FEED_RATE: u32 = 24_000;

fn ms(t: f64) -> Seconds {
    Seconds(t / 1000.0)
}

fn round3(x: f32) -> f32 {
    (x * 1000.0).round() / 1000.0
}

// ---- a fake play log ----

#[derive(Default)]
struct Feed {
    now: f64,
    /// Every record the engine will publish, in order; each is published at its start.
    records: Vec<Played>,
    raised: Option<f64>,
    /// (published at, silent from), ms.
    cut: Option<(f64, f64)>,
    /// Records the log keeps (0: all).
    ring: u64,
}

#[derive(Clone, Default)]
struct FakeFeed(Arc<Mutex<Feed>>);

impl FakeFeed {
    fn set_now(&self, t: f64) {
        self.0.lock().unwrap().now = t;
    }

    /// One record: `n` samples from `consumed`, playing from `start` to `end` (ms).
    fn record(&self, consumed: u64, n: u32, start: f64, end: f64) {
        let mut feed = self.0.lock().unwrap();
        let index = feed.records.len() as u64;
        feed.records.push(Played { index, consumed, n, start: ms(start), end: ms(end) });
    }

    /// Engine blocks of `block_ms` from `start`, consuming feed samples `from..to` at the feed rate, full blocks.
    fn play(&self, from: u64, to: u64, start: f64, block_ms: f64) -> f64 {
        let per = (block_ms * f64::from(FEED_RATE) / 1000.0).round() as u64;
        let (mut s, mut t) = (from, start);
        while s < to {
            let n = per.min(to - s);
            self.record(s, n as u32, t, t + n as f64 * 1000.0 / f64::from(FEED_RATE));
            s += n;
            t += block_ms;
        }
        t
    }

    fn cut(&self, raised: f64, published: f64, silent_from: f64) {
        let mut feed = self.0.lock().unwrap();
        feed.raised = Some(raised);
        feed.cut = Some((published, silent_from));
    }

    fn probe(&self) -> CutProbe {
        CutProbe::new(Arc::new(self.clone()))
    }
}

impl CutSource for FakeFeed {
    fn state(&self) -> CutState {
        let feed = self.0.lock().unwrap();
        match (feed.cut, feed.raised) {
            (Some((published, at)), _) if feed.now >= published - 1e-9 => CutState::cut(ms(at)),
            (_, Some(raised)) if feed.now >= raised - 1e-9 => CutState::Raised,
            _ => CutState::Live,
        }
    }

    fn head(&self) -> u64 {
        let feed = self.0.lock().unwrap();
        feed.records.iter().take_while(|r| r.start.0 * 1000.0 <= feed.now + 1e-9).count() as u64
    }

    fn record(&self, index: u64) -> Result<Played, PlayGap> {
        let head = self.head();
        let feed = self.0.lock().unwrap();
        if index >= head {
            return Err(PlayGap::NotYet);
        }
        if feed.ring > 0 && index + feed.ring < head {
            return Err(PlayGap::Lapped);
        }
        Ok(feed.records[index as usize])
    }
}

// ---- the rig ----

/// The fake's viseme fallback scaled: 0.8 makes a viseme key distinguishable from the model's.
struct Scaled(f32);

impl Visemes for Scaled {
    fn frame(&mut self, pcm: &[f32], _rate: u32, out: &mut ChannelBuf) {
        out.set(Channel::JAW_OPEN, self.0 * jaw_open(pcm));
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
struct Release {
    /// Program time of the poll that released it, ms.
    at: f64,
    /// Its stamp, ms.
    stamp: f64,
    jaw: f32,
    clear: bool,
}

struct Rig {
    drive: MouthDrive,
    feeds: Vec<FakeFeed>,
    playout: Box<PlayoutDelay<64>>,
    released: Vec<Release>,
    delay: f64,
}

struct Model {
    latency: u32,
    block: u32,
    queue: u32,
    late: fn(u64) -> bool,
    lead: Option<f64>,
    visemes: f32,
}

impl Default for Model {
    fn default() -> Self {
        Self { latency: 0, block: 320, queue: 1, late: |_| false, lead: None, visemes: 1.0 }
    }
}

fn spec(model: &Model) -> StreamSpec {
    StreamSpec { sample_rate: 16_000, block: model.block, latency: model.latency }
}

fn rig(model: &Model, delay: f64) -> Rig {
    let spec = spec(model);
    let face = FakeVoiceToFace::with_latency(model.latency);
    let (sink, source) = InlineStream::late_when(Box::new(face), spec, model.queue, 1_000, model.late);
    let visemes: Box<dyn Visemes> =
        if model.visemes == 1.0 { Box::new(EnergyVisemes) } else { Box::new(Scaled(model.visemes)) };
    let config = MouthConfig { delay: ms(delay), lead: model.lead.map(ms), ..MouthConfig::default() };
    let outputs = TensorBuf::for_outputs(&voice_to_face_signature());
    let drive = MouthDrive::new(sink, source, spec, outputs, visemes, config);
    Rig { drive, feeds: Vec::new(), playout: Box::new(PlayoutDelay::new()), released: Vec::new(), delay }
}

impl Rig {
    /// The reply "Hi chat!" on `feed`, attached at `attach` and pushed whole, predicted to play from `at0`.
    fn say(&mut self, feed: &FakeFeed, text: &str, attach: f64, at0: f64) {
        self.feeds.push(feed.clone());
        for f in &self.feeds {
            f.set_now(attach);
        }
        self.drive.attach(feed.probe());
        self.drive.push(ms(at0), 0, FEED_RATE, &fake_speech(text, 1.0), false);
    }

    fn poll(&mut self, t: f64) {
        for feed in &self.feeds {
            feed.set_now(t);
        }
        let (playout, released) = (&mut self.playout, &mut self.released);
        self.drive.poll(ms(t), &mut |stamp, layer: &Layer| {
            playout.push(stamp, *layer).unwrap();
            released.push(Release {
                at: t,
                stamp: stamp.0 * 1000.0,
                jaw: layer.values.get(Channel::JAW_OPEN),
                clear: layer.weight == 0.0,
            });
        });
    }

    /// The stage's mouth at program time `t`: the playout sampled at `t − D`.
    fn sample(&self, t: f64) -> f32 {
        self.playout.sample(ms(t), ms(self.delay)).map_or(0.0, |l| round3(l.values.get(Channel::JAW_OPEN)))
    }

    /// Polls at `polls` and samples at `samples`, merged in time order (a poll before a sample at the same time).
    fn run(&mut self, polls: &[f64], samples: &[f64]) -> Vec<f32> {
        let mut events: Vec<(f64, bool)> =
            polls.iter().map(|&t| (t, false)).chain(samples.iter().map(|&t| (t, true))).collect();
        events.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        let mut out = Vec::new();
        for (t, sample) in events {
            if sample {
                out.push(self.sample(t));
            } else {
                self.poll(t);
            }
        }
        out
    }

    /// Ticks released, as (program time of the stamp + D, value): the program time each depicts.
    fn ticks(&self) -> Vec<(f64, f32)> {
        self.released.iter().map(|r| (((r.stamp + self.delay) * 2.0).round() / 2.0, round3(r.jaw))).collect()
    }

    fn check_order(&self) {
        let d = self.delay;
        for w in self.released.windows(2) {
            assert!(w[1].stamp > w[0].stamp, "stamps rise: {w:?}");
        }
        for r in &self.released {
            assert!(r.at + 1e-6 >= r.stamp + d, "released before its time: {r:?}");
        }
        for w in self.released.windows(2) {
            assert!(w[1].at >= w[0].at, "released in order: {w:?}");
        }
        assert_eq!(self.drive.stats().refused, 0);
    }
}

fn grid(from: f64, to: f64, step: f64) -> Vec<f64> {
    let n = ((to - from) / step).round() as usize;
    (0..=n).map(|i| from + i as f64 * step).collect()
}

/// "Hi chat!" played from 830 in 5 ms blocks, to the end.
fn plays_whole(feed: &FakeFeed) {
    feed.play(0, 11_520, 830.0, 5.0);
}

// ---- §6.1 and §6.3 ----

#[test]
fn hi_chat_keys_ticks_and_release_are_the_golden() {
    for delay in [0.0, 200.0] {
        let mut rig = rig(&Model::default(), delay);
        assert_eq!(rig.drive.lead(), lead(&spec(&Model::default()), 1));
        assert!((rig.drive.lead().0 - 0.070).abs() < 1e-12, "lead = lead_min = 70 ms");
        let feed = FakeFeed::default();
        plays_whole(&feed);
        rig.say(&feed, "Hi chat!", 760.0, 830.0);
        let samples = grid(830.0, 1_400.0, 5.0);
        let out = rig.run(&grid(760.0, 1_400.0, 2.5), &samples);
        // §6.1's mouth column: keys every 20 ms.
        let keys: Vec<(f64, f32)> = (0..7)
            .map(|k| {
                let (v, values) = rig.drive.key(k).unwrap();
                ((830.0 + v.0 * 1000.0).round(), round3(values.get(Channel::JAW_OPEN)))
            })
            .collect();
        assert_eq!(
            keys,
            [(830.0, 0.141), (850.0, 0.141), (870.0, 0.141), (890.0, 0.53), (910.0, 0.53), (930.0, 0.53), (950.0, 0.0)]
        );
        // §6.3, fake A: the ticks around the first key boundary.
        let ticks = rig.ticks();
        let at = |t: f64| ticks.iter().find(|x| x.0 == t).unwrap().1;
        assert_eq!(
            [860.0, 870.0, 880.0, 890.0, 900.0, 910.0, 920.0].map(at),
            [0.141, 0.141, 0.336, 0.53, 0.53, 0.53, 0.53],
            "D = {delay}"
        );
        // Every tick from 830 to 1300, each released exactly when its audio played; then the mouth closes by 1370.
        assert_eq!(ticks.iter().filter(|t| t.0 <= 1_300.0).count(), 48);
        for r in rig.released.iter().filter(|r| r.stamp + delay <= 1_300.0) {
            assert!((r.at - r.stamp - delay).abs() < 1e-6, "released on time: {r:?}");
        }
        assert_eq!(ticks.last().unwrap(), &(1_370.0, 0.0));
        assert_eq!(rig.sample(1_400.0), 0.0);
        rig.check_order();
        assert_eq!(rig.drive.stats().late_keys, 0);
        assert!(out.contains(&0.53));
    }
}

#[test]
fn fake_b_with_10_ms_of_latency_is_the_golden_and_d_moves_nothing() {
    let model = Model { latency: 160, ..Model::default() };
    assert!((lead_min(&spec(&model), 1).0 - 0.080).abs() < 1e-12);
    let mut outputs = Vec::new();
    for delay in [0.0, 200.0] {
        let mut rig = rig(&model, delay);
        let feed = FakeFeed::default();
        plays_whole(&feed);
        rig.say(&feed, "Hi chat!", 750.0, 830.0);
        let samples = grid(830.0, 1_400.0, 5.0);
        let out = rig.run(&grid(750.0, 1_400.0, 2.5), &samples);
        assert!(rig.drive.has_head_key());
        assert_eq!(rig.drive.key(0), None, "block 0 depicts pre-roll and is dropped");
        let key = |k: u64| {
            let (v, values) = rig.drive.key(k).unwrap();
            (((830.0 + v.0 * 1000.0) * 16.0).round() / 16.0, round3(values.get(Channel::JAW_OPEN)))
        };
        assert_eq!(
            (1..8).map(key).collect::<Vec<_>>(),
            [
                (840.0, 0.141),
                (860.0, 0.141),
                (880.0, 0.388),
                (900.0, 0.53),
                (920.0, 0.53),
                (940.0, 0.375),
                (960.0, 0.0)
            ]
        );
        let ticks = rig.ticks();
        let at = |t: f64| ticks.iter().find(|x| x.0 == t).unwrap().1;
        assert_eq!(
            [830.0, 860.0, 870.0, 880.0, 890.0, 900.0, 910.0, 920.0].map(at),
            [0.141, 0.141, 0.265, 0.388, 0.459, 0.53, 0.53, 0.53]
        );
        // The stage holds the newest released tick: 0.459 at 890 and at 895, never the 0.388 of the key before.
        let sampled = |t: f64| out[samples.iter().position(|&s| s == t).unwrap()];
        assert_eq!((sampled(890.0), sampled(895.0)), (0.459, 0.459));
        rig.check_order();
        outputs.push(out);
    }
    assert_eq!(outputs[0], outputs[1], "identical at identical program times for D = 0 and 200 ms");
}

#[test]
fn warm_up_drops_the_pre_roll_and_a_head_key_fills_the_rest() {
    // (latency, head key, first retained block and its voice time in ms)
    for (latency, head, first, v) in [(320, false, 1, 0.0), (640, false, 2, 0.0), (480, true, 2, 10.0)] {
        let model = Model { latency, ..Model::default() };
        let mut rig = rig(&model, 0.0);
        let feed = FakeFeed::default();
        plays_whole(&feed);
        let lead_ms = lead_min(&spec(&model), 1).0 * 1000.0;
        rig.say(&feed, "Hi chat!", 830.0 - lead_ms, 830.0);
        rig.run(&grid(830.0 - lead_ms, 1_400.0, 2.5), &[]);
        assert_eq!(rig.drive.has_head_key(), head, "L = {latency}");
        for k in 0..first {
            assert_eq!(rig.drive.key(k), None, "L = {latency}: block {k} is pre-roll");
        }
        let (at, values) = rig.drive.key(first).unwrap();
        assert!((at.0 * 1000.0 - v).abs() < 1e-9, "L = {latency}: {at:?}");
        assert_eq!(round3(values.get(Channel::JAW_OPEN)), 0.141);
        assert_eq!(rig.ticks()[0], (830.0, 0.141), "L = {latency}");
        rig.check_order();
    }
}

#[test]
fn a_missed_block_is_its_own_viseme_and_the_model_fades_back_in_50_ms() {
    // Fake B, blocks 3 and 4 late; the fallback is 0.8 of the model so the two can be told apart.
    let model = Model { latency: 160, late: |seq| seq == 3 || seq == 4, visemes: 0.8, ..Model::default() };
    let mut rig = rig(&model, 0.0);
    let feed = FakeFeed::default();
    plays_whole(&feed);
    rig.say(&feed, "Hi chat!", 750.0, 830.0);
    rig.run(&grid(750.0, 1_400.0, 2.5), &[]);
    let jaw = |k: u64| round3(rig.drive.key(k).unwrap().1.get(Channel::JAW_OPEN));
    let at = |k: u64| (830.0 + rig.drive.key(k).unwrap().0.0 * 1000.0).round();
    // Block 3 (880) depicts half `H` and half `i`: the model says 0.388, the viseme 0.8 of it, at the same stamp.
    let near = |a: f32, b: f32| (a - b).abs() < 0.002;
    assert_eq!(at(3), 880.0);
    assert!(near(jaw(3), 0.8 * 0.388), "{}", jaw(3));
    // Block 4 too (w 0.6, then 0.2). Block 5 is 0.6 model and 0.4 viseme. The fake never ran blocks 3 and 4, so its
    // window for block 5 is 50 to 60 ms and 100 to 110 ms of the voice: 0.388, where the viseme of the right interval
    // is 0.424. Block 6 is the model again (w 1).
    assert!(near(jaw(4), 0.8 * 0.530), "{}", jaw(4));
    assert!(near(jaw(5), 0.6 * 0.388 + 0.4 * 0.8 * 0.530), "{}", jaw(5));
    assert!(near(jaw(6), 0.375), "{}", jaw(6));
    assert_eq!(rig.drive.stats().missed_keys, 2);
    rig.check_order();
}

// ---- cuts ----

/// Fake A's "Hi chat!" cut: raised at 893, the fade ends at 897.5 and the audio thread publishes it then; the block
/// at 895 played the fade.
fn cut_rig(model: &Model, delay: f64) -> Rig {
    let mut rig = rig(model, delay);
    let feed = FakeFeed::default();
    feed.play(0, 1_680, 830.0, 5.0);
    feed.cut(893.0, 897.5, 897.5);
    rig.say(&feed, "Hi chat!", 760.0, 830.0);
    rig
}

#[test]
fn a_cut_releases_what_played_and_closes_the_mouth_over_60_ms() {
    for delay in [0.0, 200.0] {
        let mut rig = cut_rig(&Model::default(), delay);
        let out = rig.run(&grid(760.0, 1_000.0, 2.5), &[900.0, 910.0, 957.5, 990.0]);
        let ticks = rig.ticks();
        assert!(ticks.iter().all(|t| t.0 <= 890.0 || t.0 >= 907.5), "nothing past the cut but the close: {ticks:?}");
        assert!(rig.released.iter().all(|r| !(893.0..897.5).contains(&r.at)), "nothing while raised");
        let close: Vec<(f64, f32)> = ticks.iter().filter(|t| t.0 > 897.5).copied().collect();
        assert_eq!(
            close,
            [(907.5, 0.442), (917.5, 0.353), (927.5, 0.265), (937.5, 0.177), (947.5, 0.088), (957.5, 0.0)],
            "D = {delay}"
        );
        assert_eq!(out, [0.53, 0.442, 0.0, 0.0]);
        assert!(rig.drive.stats().cut_ticks > 0);
        rig.check_order();
    }
}

#[test]
fn a_cut_exactly_on_a_tick_and_one_in_the_head_interval() {
    // On the tick at 900: the tick at 900 played (the block at 900 is the fade), the close starts after it.
    let mut rig = rig(&Model::default(), 0.0);
    let feed = FakeFeed::default();
    feed.play(0, 1_800, 830.0, 5.0);
    feed.cut(900.0, 905.0, 905.0);
    rig.say(&feed, "Hi chat!", 760.0, 830.0);
    rig.run(&grid(760.0, 1_000.0, 2.5), &[]);
    let ticks = rig.ticks();
    assert_eq!(ticks.iter().rfind(|t| t.0 <= 905.0).unwrap().0, 900.0);
    assert_eq!(ticks.iter().filter(|t| t.0 > 905.0).count(), 6);
    rig.check_order();

    // In the head interval of fake B (L = 160): cut 5 ms in.
    let mut rig = self::rig(&Model { latency: 160, ..Model::default() }, 200.0);
    let feed = FakeFeed::default();
    feed.play(0, 240, 830.0, 5.0);
    feed.cut(833.0, 840.0, 840.0);
    rig.say(&feed, "Hi chat!", 750.0, 830.0);
    rig.run(&grid(750.0, 1_000.0, 2.5), &[]);
    let ticks = rig.ticks();
    assert_eq!(ticks[0], (830.0, 0.141));
    assert_eq!(ticks.len(), 1 + 6, "the head tick and the close: {ticks:?}");
    rig.check_order();
}

#[test]
fn a_reply_right_after_a_cut_takes_over_the_close_and_wins_the_tie() {
    // 10 ms blocks, queue 2, L = 0: lead_min 50 ms. The next reply's first sample plays at 947.5.
    let model = Model { block: 160, queue: 2, ..Model::default() };
    assert!((lead_min(&spec(&model), 2).0 - 0.050).abs() < 1e-12);
    for delay in [0.0, 200.0] {
        let mut rig = cut_rig(&model, delay);
        let polls = grid(760.0, 1_100.0, 2.5);
        let (before, after): (Vec<f64>, Vec<f64>) = polls.iter().partition(|&&t| t < 897.5);
        rig.run(&before, &[]);
        let next = FakeFeed::default();
        next.play(0, 11_520, 947.5, 5.0);
        rig.say(&next, "Hi chat!", 897.5, 947.5);
        rig.run(&after, &[]);
        let ticks = rig.ticks();
        let close: Vec<(f64, f32)> = ticks.iter().filter(|t| t.0 > 897.5 && t.0 < 947.5).copied().collect();
        assert_eq!(close, [(907.5, 0.442), (917.5, 0.353), (927.5, 0.265), (937.5, 0.177)], "D = {delay}");
        assert_eq!(ticks.iter().find(|t| t.0 >= 947.5).unwrap(), &(947.5, 0.141), "the new reply wins the tie");
        assert!(!ticks.iter().any(|t| t.0 == 957.5 && t.1 == 0.0), "the close item at 957.5 is dropped");
        rig.check_order();
    }
}

// ---- stalls ----

/// Fake A's play log with the §6.3 underrun: the block at 890 consumes 60 samples, then nothing until 990.
fn stall_rig(delay: f64) -> Rig {
    let mut rig = rig(&Model::default(), delay);
    let feed = FakeFeed::default();
    feed.play(0, 1_440, 830.0, 5.0);
    feed.record(1_440, 60, 890.0, 892.5);
    feed.play(1_500, 11_520, 990.0, 5.0);
    rig.say(&feed, "Hi chat!", 760.0, 830.0);
    rig
}

const STALL_SAMPLES: [f64; 10] = [895.0, 897.5, 902.5, 910.0, 920.0, 950.0, 952.5, 989.0, 990.0, 997.5];

#[test]
fn a_stall_fades_the_mouth_as_each_item_falls_due_and_resumes_where_the_audio_did() {
    let mut outs = Vec::new();
    for delay in [0.0, 200.0] {
        let mut rig = stall_rig(delay);
        let out = rig.run(&grid(760.0, 1_100.0, 2.5), &STALL_SAMPLES);
        assert_eq!(out, [0.53, 0.53, 0.442, 0.442, 0.353, 0.088, 0.0, 0.0, 0.53, 0.53], "D = {delay}");
        let stamps: Vec<f64> = rig.ticks().iter().map(|t| t.0).filter(|t| (885.0..1_000.0).contains(t)).collect();
        assert_eq!(stamps, [890.0, 902.5, 912.5, 922.5, 932.5, 942.5, 952.5, 990.0, 997.5]);
        rig.check_order();
        outs.push(out);
    }
    assert_eq!(outs[0], outs[1]);
}

#[test]
fn a_descheduled_release_thread_holds_the_mouth_and_never_replays_the_fade() {
    let mut rig = stall_rig(0.0);
    let polls: Vec<f64> = grid(760.0, 1_100.0, 2.5).into_iter().filter(|&t| t <= 897.5 || t >= 1_010.0).collect();
    let samples = grid(890.0, 1_010.0, 5.0);
    let out = rig.run(&polls, &samples);
    assert!(out.iter().all(|&v| v == 0.53), "0.530 from 890 to 1,010: {out:?}");
    let at_1010: Vec<(f64, f32)> =
        rig.released.iter().filter(|r| r.at == 1_010.0).map(|r| (r.stamp, round3(r.jaw))).collect();
    let stamps: Vec<f64> = at_1010.iter().map(|r| (r.0 * 2.0).round() / 2.0).collect();
    assert_eq!(stamps, [990.0, 997.5, 1_007.5]);
    assert_eq!(rig.drive.stats().fade_overdue, 0);
    rig.check_order();
}

#[test]
fn with_1024_frame_blocks_overdue_fade_items_collapse_into_the_newest() {
    for delay in [0.0, 200.0] {
        let mut rig = rig(&Model::default(), delay);
        let feed = FakeFeed::default();
        let block = 1024.0 / 48.0;
        // Full blocks of 512 feed samples from 830; the third ends the audio at 894, which resumes at 990.
        let end = feed.play(0, 1_536, 830.0, block);
        assert!((end - 894.0).abs() < 1e-9);
        feed.play(1_536, 11_520, 990.0, block);
        rig.say(&feed, "Hi chat!", 760.0, 830.0);
        // The release thread polls at each block start and mid-block.
        let polls = grid(760.0 + 2.0 * block / 2.0 * 0.0, 1_100.0, 1.0)
            .into_iter()
            .filter(|t| ((t - 830.0) / (block / 2.0)).fract().abs() < 1e-6 || *t < 830.0)
            .collect::<Vec<_>>();
        let polls: Vec<f64> = (0..30).map(|i| 830.0 + f64::from(i) * block / 2.0).chain(polls).collect();
        let mut polls = polls;
        polls.sort_by(f64::total_cmp);
        polls.dedup();
        rig.run(&polls, &[]);
        let g0 = 894.0;
        let fades: Vec<(f64, f64, f32)> = rig
            .released
            .iter()
            .filter(|r| r.stamp + delay > g0 && r.stamp + delay < 990.0)
            .map(|r| (r.at, r.stamp + delay, round3(r.jaw)))
            .collect();
        // Detected at the first poll a block after g0 (915.333): 904 and 914 are both due, only 914 goes.
        let detect = 830.0 + 8.0 * block / 2.0;
        assert!((fades[0].0 - detect).abs() < 1e-6, "{fades:?}");
        assert_eq!(
            fades.iter().map(|f| (f.1.round(), f.2)).collect::<Vec<_>>(),
            [(914.0, 0.353), (924.0, 0.265), (934.0, 0.177), (944.0, 0.088), (954.0, 0.0)]
        );
        assert_eq!(rig.drive.stats().fade_overdue, 1);
        rig.check_order();
    }
}

// ---- lead, privacy ----

#[test]
fn a_lead_below_lead_min_keeps_ticks_on_time_and_uses_viseme_keys() {
    // 20 ms blocks, queue 4: lead_min 130 ms, the lead stays at 100.
    let model = Model { queue: 4, visemes: 0.8, ..Model::default() };
    assert!((lead_min(&spec(&model), 4).0 - 0.130).abs() < 1e-12);
    assert!((lead(&spec(&model), 4).0 - 0.100).abs() < 1e-12);
    let mut values = Vec::new();
    for delay in [0.0, 200.0] {
        let mut rig = rig(&model, delay);
        let feed = FakeFeed::default();
        plays_whole(&feed);
        rig.say(&feed, "Hi chat!", 700.0, 830.0);
        let samples = grid(830.0, 1_000.0, 5.0);
        values.push(rig.run(&grid(700.0, 1_400.0, 2.5), &samples));
        let stats = rig.drive.stats();
        assert!(stats.late_keys > 0 && stats.late_keys <= stats.ticks, "{stats:?}");
        for r in rig.released.iter().filter(|r| r.stamp + delay <= 1_300.0) {
            assert!((r.at - r.stamp - delay).abs() < 1e-6, "never delayed: {r:?}");
        }
        // Tick 50 between the key at 40 (the consonant) and the undecided key at 60 (its viseme, 0.8 of 0.530).
        let at = |t: f64| rig.ticks().iter().find(|x| x.0 == t).unwrap().1;
        assert_eq!(at(890.0), round3(0.8 * 0.530_33));
        rig.check_order();
    }
    assert_eq!(values[0], values[1]);
    // At lead_min nothing is late.
    let mut rig = rig(&Model { queue: 4, lead: Some(130.0), ..Model::default() }, 0.0);
    let feed = FakeFeed::default();
    plays_whole(&feed);
    rig.say(&feed, "Hi chat!", 690.0, 830.0);
    rig.run(&grid(690.0, 1_400.0, 2.5), &[]);
    assert_eq!(rig.drive.stats().late_keys, 0);
}

#[test]
fn private_speech_leaves_the_layer_clear() {
    let mut rig = rig(&Model::default(), 0.0);
    let feed = FakeFeed::default();
    plays_whole(&feed);
    rig.say(&feed, "Hi chat!", 760.0, 830.0);
    rig.run(&grid(760.0, 1_400.0, 2.5), &[]);
    rig.drive.push(ms(1_500.0), 0, FEED_RATE, &fake_speech("psst", 1.0), true);
    rig.run(&grid(1_402.5, 1_800.0, 2.5), &[]);
    let last = rig.released.last().unwrap();
    assert!(last.clear && last.stamp == 1_500.0, "{last:?}");
    assert_eq!(rig.playout.sample(ms(1_600.0), ms(0.0)).unwrap().weight, 0.0);
    rig.check_order();
}

// ---- the play log read late, lapped; a reply after a cut ----

#[test]
fn a_reader_that_polls_every_100_ms_stamps_every_tick_from_its_own_record() {
    let stamps = |every: f64, ring: u64| {
        let mut rig = stall_rig(0.0);
        rig.feeds[0].0.lock().unwrap().ring = ring;
        rig.run(&grid(760.0, 1_500.0, every), &[]);
        let ticks: Vec<f64> =
            rig.released.iter().map(|r| (r.stamp * 2.0).round() / 2.0).filter(|&s| s <= 1_300.0).collect();
        (ticks, rig.drive.stats())
    };
    let (every_block, _) = stamps(2.5, 0);
    let (sparse, stats) = stamps(100.0, 0);
    // The sparse reader never saw the stall while it happened, so it has no fade and no resume tick; every tick it
    // has carries the same stamp as the attentive reader's (890, 997.5, never extrapolated from a later record).
    assert!(sparse.iter().all(|s| every_block.contains(s)), "{sparse:?}");
    assert!(sparse.contains(&890.0) && sparse.contains(&997.5));
    assert_eq!(stats.history_lost, 0);
    // A log of 16 records read every 100 ms (20 blocks): the lost records' ticks are dropped and counted, the rest
    // keep their stamps, nothing is refused.
    let (lapped, stats) = stamps(100.0, 16);
    assert!(stats.history_lost > 0, "{stats:?}");
    assert!(lapped.iter().all(|s| every_block.contains(s)));
    assert_eq!(stats.refused, 0);
}

#[test]
fn a_reply_after_a_cut_starts_clean_on_its_own_origin() {
    let mut rig = cut_rig(&Model::default(), 0.0);
    let polls = grid(760.0, 1_600.0, 2.5);
    let (before, after): (Vec<f64>, Vec<f64>) = polls.iter().partition(|&&t| t < 905.0);
    rig.run(&before, &[]);
    let next = FakeFeed::default();
    next.play(0, 11_520, 1_000.0, 5.0);
    rig.say(&next, "Hi chat!", 905.0, 1_000.0);
    rig.run(&after, &[]);
    // A fresh drive saying the same reply from 830: the keys, as offsets from each reply's start, are the same.
    let fresh = rig_with_reply(&Model::default());
    for k in 0..24 {
        assert_eq!(rig.drive.key(k), fresh.drive.key(k), "key {k}");
    }
    let offsets = |r: &Rig, from: f64| -> Vec<(f64, f32)> {
        r.ticks().into_iter().filter(|t| t.0 >= from && t.0 < from + 470.0).map(|t| (t.0 - from, t.1)).collect()
    };
    assert_eq!(offsets(&rig, 1_000.0), offsets(&fresh, 830.0));
    rig.check_order();
    fresh.check_order();
}

fn rig_with_reply(model: &Model) -> Rig {
    let mut rig = rig(model, 0.0);
    let feed = FakeFeed::default();
    plays_whole(&feed);
    rig.say(&feed, "Hi chat!", 760.0, 830.0);
    rig.run(&grid(760.0, 1_400.0, 2.5), &[]);
    rig
}

#[test]
fn a_pump_that_stalls_across_a_cut_changes_nothing() {
    // The pump never says `cut` and pushes nothing for 300 ms: the probe alone decides. Published before and after
    // the pump comes back.
    for published in [897.5, 1_150.0] {
        let mut rig = rig(&Model::default(), 0.0);
        let feed = FakeFeed::default();
        feed.play(0, 1_680, 830.0, 5.0);
        feed.cut(893.0, published, 897.5);
        rig.say(&feed, "Hi chat!", 760.0, 830.0);
        rig.run(&grid(760.0, 1_300.0, 2.5), &[]);
        assert!(rig.released.iter().all(|r| r.stamp <= 890.0 + 1e-9 || r.stamp >= 897.5), "{:?}", rig.released);
        assert!(rig.released.iter().all(|r| !(893.0..published).contains(&r.at)), "nothing while raised");
        let close: Vec<(f64, f32)> = rig.ticks().into_iter().filter(|t| t.0 > 897.5).collect();
        if published < 900.0 {
            assert_eq!(close.len(), 6, "the close, item by item: {close:?}");
        } else {
            // Published long after the close's items fell due: they collapse into the newest, the mouth at rest.
            assert_eq!(close, [(957.5, 0.0)]);
            assert_eq!(rig.drive.stats().fade_overdue, 5);
        }
        rig.check_order();
    }
}
