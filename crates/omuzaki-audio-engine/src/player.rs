//! The player source (#1160): the sound bank and the players that play it into the buses, with their duck gates.
//!
//! Everything here runs on the audio thread and is made on the control thread: the bank's slots and each player's
//! voices and buffers are fixed. A sound's `Arc` reaches the bank through a command and leaves it the same way, handed
//! back to be dropped off the audio thread, so a sound is never freed here. Each slot's `held` flag, shared with the
//! control side, says whether the audio thread still has a sound there, so a slot is never reused too early.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use omuzaki_audio_api::ids::PlayerId;
use omuzaki_audio_api::node::MAX_BLOCK;
use omuzaki_audio_api::player::{Sound, VOICE_FADE, VOICES};
use omuzaki_audio_api::semantics::MONO_TO_STEREO_GAIN;

/// How many sounds the bank holds at once.
pub(crate) const BANK: usize = 256;

/// The most players an engine runs.
pub(crate) const MAX_PLAYERS: usize = 16;

/// Voice slots per player: the live voices, plus as many fading out after a steal.
const SLOTS: usize = 2 * VOICES;

/// The decoded sounds, by slot.
pub(crate) struct Bank {
    slots: Vec<Option<Arc<Sound>>>,
    /// Slots whose sound was unloaded: handed back once no voice plays them.
    retiring: Vec<bool>,
    /// Per slot, whether the audio thread holds a sound there. The control side sets it when it loads one; the audio
    /// thread clears it when it hands one back.
    held: Arc<[AtomicBool]>,
}

/// The flags that say which bank slots the audio thread holds.
pub(crate) fn held_flags() -> Arc<[AtomicBool]> {
    (0..BANK).map(|_| AtomicBool::new(false)).collect()
}

impl Bank {
    pub(crate) fn new(held: Arc<[AtomicBool]>) -> Self {
        Self { slots: (0..BANK).map(|_| None).collect(), retiring: vec![false; BANK], held }
    }

    /// Puts `sound` in `slot`; returns what was there, to be dropped off the audio thread.
    pub(crate) fn load(&mut self, slot: usize, sound: Arc<Sound>) -> Option<Arc<Sound>> {
        let at = self.slots.get_mut(slot)?;
        self.retiring[slot] = false;
        at.replace(sound)
    }

    /// Marks `slot` to be let go once nothing plays it (the players fade its voices out first).
    pub(crate) fn unload(&mut self, slot: usize) {
        if let Some(retiring) = self.retiring.get_mut(slot) {
            *retiring = true;
        }
    }

    fn get(&self, slot: usize) -> Option<&Sound> {
        self.slots.get(slot).and_then(Option::as_deref)
    }

    /// Takes out an unloaded sound no voice plays any more, to be dropped off the audio thread; `None` when there is
    /// none left.
    pub(crate) fn take_released(&mut self, players: &[Box<Player>]) -> Option<Arc<Sound>> {
        let slot = (0..BANK).find(|&slot| self.retiring[slot] && !players.iter().any(|p| p.uses(slot)))?;
        self.retiring[slot] = false;
        let sound = self.slots[slot].take();
        self.held[slot].store(false, Ordering::Release);
        sound
    }
}

/// One sound playing.
#[derive(Copy, Clone, Debug)]
struct Voice {
    slot: usize,
    position: usize,
    gain: f32,
    ducks: bool,
    /// Samples of fade-out left, once stopped or stolen.
    fade: Option<u32>,
    /// When it started, for stealing the oldest.
    serial: u64,
}

/// A player: its voices, its output block and its gate.
pub(crate) struct Player {
    pub id: PlayerId,
    voices: [Option<Voice>; SLOTS],
    serial: u64,
    /// The last block, stereo.
    pub out: [Vec<f32>; 2],
    /// The gate per sample of the last block: up while a voice that ducks sounds.
    pub gate: Vec<bool>,
}

impl Player {
    pub(crate) fn new(id: PlayerId) -> Self {
        Self {
            id,
            voices: [None; SLOTS],
            serial: 0,
            out: [vec![0.0; MAX_BLOCK], vec![0.0; MAX_BLOCK]],
            gate: vec![false; MAX_BLOCK],
        }
    }

    /// Starts the sound in `slot` at the linear `gain`. With [`VOICES`] already sounding, the oldest starts fading
    /// out; with every slot taken by voices still fading, the oldest of those is cut.
    pub(crate) fn play(&mut self, slot: usize, gain: f32, ducks: bool) {
        let live = self.voices.iter().flatten().filter(|v| v.fade.is_none()).count();
        if live >= VOICES
            && let Some(oldest) = self.voices.iter_mut().flatten().filter(|v| v.fade.is_none()).min_by_key(|v| v.serial)
        {
            oldest.fade = Some(VOICE_FADE);
        }
        let free = self.voices.iter().position(Option::is_none).or_else(|| {
            (0..SLOTS)
                .filter(|&i| self.voices[i].is_some_and(|v| v.fade.is_some()))
                .min_by_key(|&i| self.voices[i].map_or(u64::MAX, |v| v.serial))
        });
        if let Some(at) = free {
            self.voices[at] = Some(Voice { slot, position: 0, gain, ducks, fade: None, serial: self.serial });
            self.serial += 1;
        }
    }

    /// Fades out the voices playing `slot`, or every voice with `None`.
    pub(crate) fn stop(&mut self, slot: Option<usize>) {
        for voice in self.voices.iter_mut().flatten() {
            if voice.fade.is_none() && slot.is_none_or(|s| s == voice.slot) {
                voice.fade = Some(VOICE_FADE);
            }
        }
    }

    /// Whether a voice plays `slot`.
    pub(crate) fn uses(&self, slot: usize) -> bool {
        self.voices.iter().flatten().any(|v| v.slot == slot)
    }

    /// Makes the next `n` frames into [`out`](Self::out) and [`gate`](Self::gate).
    pub(crate) fn render(&mut self, n: usize, bank: &Bank) {
        let n = n.min(MAX_BLOCK);
        self.out[0][..n].fill(0.0);
        self.out[1][..n].fill(0.0);
        self.gate[..n].fill(false);
        let [left, right] = &mut self.out;
        for entry in &mut self.voices {
            let Some(voice) = entry.as_mut() else { continue };
            let Some(sound) = bank.get(voice.slot) else {
                *entry = None;
                continue;
            };
            let side = if sound.channel_count() == 1 { MONO_TO_STEREO_GAIN } else { 1.0 };
            let (l, r) = (sound.channel(0), sound.channel(1));
            let mut ended = false;
            for i in 0..n {
                if voice.position >= sound.frames() {
                    ended = true;
                    break;
                }
                let mut gain = voice.gain * side;
                if let Some(left_to_fade) = voice.fade.as_mut() {
                    *left_to_fade -= 1;
                    gain *= fraction(*left_to_fade);
                }
                left[i] += l[voice.position] * gain;
                right[i] += r[voice.position] * gain;
                if voice.ducks {
                    self.gate[i] = true;
                }
                voice.position += 1;
                if voice.fade == Some(0) {
                    ended = true;
                    break;
                }
            }
            if ended || voice.position >= sound.frames() {
                *entry = None;
            }
        }
    }
}

/// `k / VOICE_FADE` as a gain.
fn fraction(k: u32) -> f32 {
    f32::from(u16::try_from(k).unwrap_or(0)) / f32::from(u16::try_from(VOICE_FADE).unwrap_or(1))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::cast_precision_loss)]
mod tests {
    use std::collections::BTreeMap;

    use omuzaki_audio_api::graph::{BusSpec, Channels, DuckRule, DuckTrigger, GraphSpec, MicMode, SourceRef};
    use omuzaki_audio_api::ids::{BusId, DeviceRole, GateId};
    use omuzaki_audio_api::player::gate_of;
    use omuzaki_audio_api::semantics::{TRIGGER_DELAY, db_to_gain};
    use omuzaki_audio_mock::conformance::allocations_during;

    use super::*;
    use crate::mix::{Inputs, Mix, Points, Stage};

    fn dc(level: f32, frames: usize) -> Arc<Sound> {
        Arc::new(Sound::new(vec![vec![level; frames]]).unwrap())
    }

    fn stereo_dc(level: f32, frames: usize) -> Arc<Sound> {
        Arc::new(Sound::new(vec![vec![level; frames], vec![level; frames]]).unwrap())
    }

    fn player() -> Player {
        Player::new(PlayerId::new("board").unwrap())
    }

    /// Renders `blocks` blocks of `n` and returns the left channel and the gate.
    fn render(p: &mut Player, bank: &Bank, n: usize, blocks: usize) -> (Vec<f32>, Vec<bool>) {
        let (mut out, mut gate) = (Vec::new(), Vec::new());
        for _ in 0..blocks {
            p.render(n, bank);
            out.extend_from_slice(&p.out[0][..n]);
            gate.extend_from_slice(&p.gate[..n]);
        }
        (out, gate)
    }

    #[test]
    fn voices_mix_deterministically_whatever_the_blocks() {
        let mut bank = Bank::new(held_flags());
        bank.load(0, stereo_dc(0.25, 3_000));
        bank.load(1, Arc::new(Sound::new(vec![(0..2_000).map(|i| (i as f32 * 0.01).sin()).collect()]).unwrap()));
        let run = |n: usize| {
            let mut p = player();
            p.play(0, 0.5, false);
            p.play(1, 1.0, false);
            render(&mut p, &bank, n, 4_096 / n).0
        };
        let whole = run(1_024);
        assert_eq!(whole, run(64));
        assert_eq!(whole, run(1));
        // The stereo sound at its gain, plus the mono one equal-power on each side.
        let expected = 0.25 * 0.5 + (10.0f32 * 0.01).sin() * MONO_TO_STEREO_GAIN;
        assert!((whole[10] - expected).abs() < 1e-7, "{} vs {expected}", whole[10]);
        // After both end, silence.
        assert!(whole[3_000..].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn a_seventeenth_sound_steals_the_oldest_which_fades_out_over_240_samples() {
        let mut bank = Bank::new(held_flags());
        bank.load(0, stereo_dc(100.0, 48_000));
        bank.load(1, stereo_dc(1.0, 48_000));
        let mut p = player();
        p.play(0, 1.0, false);
        for _ in 1..VOICES {
            p.play(1, 1.0, false);
        }
        let (before, _) = render(&mut p, &bank, 64, 1);
        assert!(before.iter().all(|&s| (s - 115.0).abs() < 1e-4), "{}", before[0]);
        p.play(1, 1.0, false);
        let (after, _) = render(&mut p, &bank, 480, 1);
        for (i, &s) in after.iter().enumerate() {
            // The oldest (the loud one) at (239 - i) / 240, the other 16 at full.
            let fading = if i < 240 { 100.0 * (239 - i) as f32 / 240.0 } else { 0.0 };
            assert!((s - (16.0 + fading)).abs() < 1e-3, "sample {i}: {s}");
        }
        assert!(!p.uses(0), "the stolen voice is gone");
    }

    #[test]
    fn stopping_fades_out_over_exactly_240_samples() {
        let mut bank = Bank::new(held_flags());
        bank.load(3, stereo_dc(1.0, 48_000));
        let mut p = player();
        p.play(3, 1.0, false);
        render(&mut p, &bank, 100, 1);
        p.stop(Some(3));
        let (out, _) = render(&mut p, &bank, 37, 10);
        for (i, &s) in out.iter().enumerate() {
            let expected = if i < 240 { (239 - i) as f32 / 240.0 } else { 0.0 };
            assert!((s - expected).abs() < 1e-6, "sample {i}: {s}");
        }
        assert!(!p.uses(3));
    }

    #[test]
    fn the_gate_is_up_exactly_while_a_ducking_sound_sounds() {
        let mut bank = Bank::new(held_flags());
        bank.load(0, dc(0.5, 1_000));
        bank.load(1, dc(0.5, 5_000));
        let mut p = player();
        p.play(1, 1.0, false);
        p.play(0, 1.0, true);
        let (_, gate) = render(&mut p, &bank, 480, 4);
        assert!(gate[..1_000].iter().all(|&g| g), "up while it plays");
        assert!(gate[1_000..].iter().all(|&g| !g), "down after, though a sound that doesn't duck still plays");
        // Stopped: up through its 240-sample fade, then down.
        p.play(0, 1.0, true);
        render(&mut p, &bank, 100, 1);
        p.stop(None);
        let (_, gate) = render(&mut p, &bank, 480, 1);
        assert!(gate[..240].iter().all(|&g| g) && gate[240..].iter().all(|&g| !g));
    }

    #[test]
    fn playing_stealing_and_unloading_neither_allocate_nor_free() {
        let mut bank = Bank::new(held_flags());
        let sounds: Vec<Arc<Sound>> = (0..4).map(|k| dc(0.1 * k as f32, 700 + 300 * k)).collect();
        let mut players = vec![Box::new(player()), Box::new(Player::new(PlayerId::new("music").unwrap()))];
        // What the audio thread hands back, for the control side to drop.
        let mut returned: Vec<Arc<Sound>> = Vec::with_capacity(1_024);
        let mut k = 0usize;
        let count = allocations_during(&mut || {
            for round in 0..400 {
                if round % 50 == 0 {
                    for (slot, sound) in sounds.iter().enumerate() {
                        returned.extend(bank.load(slot, Arc::clone(sound)));
                    }
                }
                for p in &mut players {
                    p.play(k % 4, 0.5, k.is_multiple_of(3));
                    if k.is_multiple_of(7) {
                        p.stop(Some(k % 4));
                    }
                    k += 1;
                }
                if round % 50 == 25 {
                    bank.unload(round % 4);
                    for p in &mut players {
                        p.stop(Some(round % 4));
                    }
                }
                for p in &mut players {
                    p.render([1usize, 64, 480, 1_024][round % 4], &bank);
                }
                while let Some(sound) = bank.take_released(&players) {
                    returned.push(sound);
                }
            }
        });
        assert_eq!(count, 0, "allocator calls while playing");
        assert!(!returned.is_empty(), "unloaded sounds came back");
        drop(returned);
        drop(bank);
        assert!(sounds.iter().all(|s| Arc::strong_count(s) == 1), "nothing kept a sound");
    }

    /// The player as the mixer's source and gate, and a steady 1.0 on every device.
    struct Studio<'a> {
        player: &'a Player,
        ones: Vec<f32>,
    }

    impl Inputs for Studio<'_> {
        fn source(&self, source: &SourceRef, n: usize) -> Option<(usize, [&[f32]; 2])> {
            match source {
                SourceRef::Player { .. } => Some((2, [&self.player.out[0][..n], &self.player.out[1][..n]])),
                _ => Some((1, [&self.ones[..n], &self.ones[..n]])),
            }
        }

        fn gate(&self, gate: &GateId, n: usize) -> Option<&[bool]> {
            (gate.as_str() == self.player.id.as_str()).then(|| &self.player.gate[..n])
        }
    }

    #[derive(Default)]
    struct Capture(BTreeMap<String, Vec<f32>>);

    impl Points for Capture {
        fn reached(&mut self, stage: Stage, bus: &BusId, _: usize, block: [&[f32]; 2]) {
            if stage == Stage::PostFader {
                self.0.entry(bus.as_str().to_owned()).or_default().extend_from_slice(block[0]);
            }
        }
    }

    #[test]
    fn a_ducking_sound_lowers_the_music_through_the_gate() {
        omuzaki_audio_mock::link();
        let board = PlayerId::new("board").unwrap();
        let bus = |id: &str, sources| BusSpec {
            id: BusId::new(id).unwrap(),
            channels: Channels::Stereo,
            sources,
            chain: Vec::new(),
            gain_db: 0.0,
            muted: false,
            sends: Vec::new(),
            outputs: vec![DeviceRole::new("main_out").unwrap()],
        };
        let graph = GraphSpec {
            buses: vec![
                bus("sfx", vec![SourceRef::Player { id: board.clone() }]),
                bus("music", vec![SourceRef::Device { role: DeviceRole::new("music_in").unwrap() }]),
            ],
            ducks: vec![DuckRule {
                trigger: DuckTrigger::Gate { id: gate_of(&board) },
                target: BusId::new("music").unwrap(),
                depth_db: 12.0,
                threshold_db: -30.0,
                attack_ms: 1.0,
                hold_ms: 0.0,
                release_ms: 1.0,
            }],
            ..GraphSpec::default()
        };
        let (mut mix, _) = Mix::compile(&graph, 1, MicMode::Live, None).unwrap();
        let mut bank = Bank::new(held_flags());
        bank.load(0, dc(0.5, 4_800));
        let mut p = Player::new(board);
        let mut capture = Capture::default();
        let mut first = 0u64;
        for k in 0..20 {
            if k == 2 {
                p.play(0, 1.0, true);
            }
            p.render(480, &bank);
            mix.process(480, first, &Studio { player: &p, ones: vec![1.0; MAX_BLOCK] }, &mut capture);
            first += 480;
        }
        let music = &capture.0["music"];
        let start = 960 + TRIGGER_DELAY;
        // The mono input reaches the stereo bus equal-power.
        let level = MONO_TO_STEREO_GAIN;
        assert_eq!(music[start - 1], level, "untouched until the gate's delay has passed");
        let ducked = music[start + 100] / level;
        assert!((ducked - db_to_gain(-12.0)).abs() < 1e-6, "ducked: {ducked}");
        // The sound ends after 4,800 samples; the gate drops then, and the rule releases 1,024 samples later.
        assert!((music[960 + 4_800 + TRIGGER_DELAY + 100] - level).abs() < 1e-6);
    }
}
