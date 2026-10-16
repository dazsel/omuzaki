//! The contract's own conformance checks: what every implementation of a seam must do, whoever wrote it. The fakes in
//! `omuzaki-performer-mock` pass them in its tests; the speech host, the perform pipeline and the core's planner run
//! the same functions against theirs.
//!
//! Each check returns the first failure as `"<case>: <what went wrong>"`.

use omuzaki_anim_api::blend::{BlendMode, Layer};
use omuzaki_anim_api::frame::Tick;
use omuzaki_anim_api::score::ExpressionId;
use omuzaki_core::rng::Rng;
use omuzaki_core::time::Seconds;
use omuzaki_stage_api::ids::MemberId;

use crate::ears::Heard;
use crate::ids::VoiceId;
use crate::intent::{GazeTarget, Intent};
use crate::limits::CHUNK_MAX_CHARS;
use crate::moves::{Addressee, BigMovePlanner, Floor, MoveContext, OWNED_CHANNELS, StyleKnobs, TalkState};
use crate::speech::{SpeakHow, SpeechHandle, SpeechState, Voice, VoiceError};
use crate::stage::{Refusal, Stage, StageError};
use crate::voice::{VoiceCatalog, VoiceProvider};

/// A [`Voice`] under test with the clock it plays on.
pub trait VoiceRig {
    /// The voice.
    fn voice(&mut self) -> &mut dyn Voice;
    /// A voice id it knows.
    fn voice_id(&self) -> VoiceId;
    /// Moves its clock forward and lets it play.
    fn advance(&mut self, by: Seconds);
}

/// Builds a fresh rig for each case.
pub type MakeVoice<'a> = &'a mut dyn FnMut() -> Box<dyn VoiceRig>;

/// The clock step the checks advance by: one engine block's worth of slack for "at once".
const STEP: Seconds = Seconds(0.01);
/// How long a check lets a chunk play before calling it stuck.
const PATIENCE_S: f64 = 60.0;
/// Float slack for comparing program times.
const EPS: f64 = 1e-6;
/// The most silence allowed between a chunk ending and the next queued one starting: one [`STEP`], the check's own
/// resolution. Anything longer is an audible pause.
const GAP_MAX: Seconds = STEP;

/// Runs every voice case.
///
/// # Errors
/// The first failing case.
pub fn voice(make: MakeVoice<'_>) -> Result<(), String> {
    type Case = fn(&mut dyn VoiceRig) -> Result<(), String>;
    let cases: [(&str, Case); 5] = [
        ("plays to the end", plays_to_the_end),
        ("chunks play back to back", back_to_back),
        ("a cut is at once and final", cut_while_playing),
        ("a cut before playback speaks nothing", cut_before_playing),
        ("bad chunks are refused", bad_chunks),
    ];
    for (name, case) in cases {
        let mut rig = make();
        case(rig.as_mut()).map_err(|why| format!("{name}: {why}"))?;
    }
    Ok(())
}

fn speak(rig: &mut dyn VoiceRig, text: &str) -> Result<SpeechHandle, String> {
    let id = rig.voice_id();
    rig.voice().speak(text, &id, &SpeakHow::default()).map_err(|e| format!("speak {text:?}: {e}"))
}

/// Advances until `done(state)` holds, returning the state; fails after [`PATIENCE_S`].
fn run_until(rig: &mut dyn VoiceRig, h: &SpeechHandle, done: fn(&SpeechState) -> bool) -> Result<SpeechState, String> {
    let mut waited = 0.0;
    loop {
        let s = h.state();
        if done(&s) {
            return Ok(s);
        }
        if waited > PATIENCE_S {
            return Err(format!("stuck in {s:?}"));
        }
        rig.advance(STEP);
        waited += STEP.0;
    }
}

fn plays_to_the_end(rig: &mut dyn VoiceRig) -> Result<(), String> {
    let h = speak(rig, "Hi chat!")?;
    if h.state().is_final() {
        return Err(format!("final before any time passed: {:?}", h.state()));
    }
    let SpeechState::Playing { from, duration } = run_until(rig, &h, |s| matches!(s, SpeechState::Playing { .. }))?
    else {
        return Err("never played".into());
    };
    if duration.0 <= 0.0 {
        return Err(format!("playing with duration {}", duration.0));
    }
    match run_until(rig, &h, SpeechState::is_final)? {
        SpeechState::Done { at } if at.0 + EPS >= from.0 + duration.0 => Ok(()),
        other => Err(format!("ended as {other:?} after playing from {} for {}", from.0, duration.0)),
    }
}

fn back_to_back(rig: &mut dyn VoiceRig) -> Result<(), String> {
    let a = speak(rig, "Hi chat!")?;
    let b = speak(rig, "Fold it and see!")?;
    let SpeechState::Done { at: a_done } = run_until(rig, &a, SpeechState::is_final)? else {
        return Err(format!("the first chunk ended as {:?}", a.state()));
    };
    match run_until(rig, &b, |s| matches!(s, SpeechState::Playing { .. } | SpeechState::Done { .. }))? {
        SpeechState::Playing { from, .. } if from.0 + EPS >= a_done.0 && from.0 <= a_done.0 + GAP_MAX.0 + EPS => Ok(()),
        other => Err(format!(
            "the second chunk was {other:?} with the first done at {}; it must start within {} s",
            a_done.0, GAP_MAX.0
        )),
    }
}

fn cut_while_playing(rig: &mut dyn VoiceRig) -> Result<(), String> {
    let text = "It took me a whole week to learn and I still fold it wrong.";
    let h = speak(rig, text)?;
    run_until(rig, &h, |s| matches!(s, SpeechState::Playing { .. }))?;
    rig.advance(Seconds(0.5));
    h.cut();
    rig.advance(STEP);
    let cut = h.state();
    let SpeechState::Cut { spoken_chars, .. } = cut else {
        return Err(format!("one step after cut(): {cut:?}"));
    };
    if spoken_chars as usize > text.chars().count() {
        return Err(format!("spoke {spoken_chars} of {} chars", text.chars().count()));
    }
    rig.advance(Seconds(5.0));
    h.cut();
    if h.state() == cut { Ok(()) } else { Err(format!("a cut chunk changed to {:?}", h.state())) }
}

fn cut_before_playing(rig: &mut dyn VoiceRig) -> Result<(), String> {
    let h = speak(rig, "Hi chat!")?;
    h.cut();
    match run_until(rig, &h, SpeechState::is_final)? {
        SpeechState::Cut { spoken_chars: 0, .. } => Ok(()),
        other => Err(format!("ended as {other:?}")),
    }
}

fn bad_chunks(rig: &mut dyn VoiceRig) -> Result<(), String> {
    let id = rig.voice_id();
    for text in [String::new(), "a".repeat(CHUNK_MAX_CHARS + 1)] {
        match rig.voice().speak(&text, &id, &SpeakHow::default()) {
            Err(VoiceError::BadChunk { .. }) => {}
            other => return Err(format!("a chunk of {} chars gave {other:?}", text.len())),
        }
    }
    Ok(())
}

/// Checks a sequence an [`Ears`](crate::ears::Ears) produced: times never go back, every utterance is `Onset`, at most
/// one `Held`, then `Final` or `Rejected`; `Final`s are numbered 0, 1, 2… with no gaps (a `Rejected` takes no number)
/// and end after they start; nothing but `Muted(false)` follows `Muted(true)`.
///
/// # Errors
/// The first event that breaks a rule, with its index.
pub fn heard_sequence(events: &[Heard]) -> Result<(), String> {
    let mut last = f64::NEG_INFINITY;
    let (mut open, mut held, mut muted) = (false, false, false);
    let mut next_utterance: u64 = 0;
    for (i, e) in events.iter().enumerate() {
        let fail = |why: &str| Err(format!("event {i} ({e:?}): {why}"));
        if let Some(t) = e.time() {
            if t.0 < last {
                return fail("time went back");
            }
            last = t.0;
        }
        if muted && *e != Heard::Muted(false) {
            return fail("heard while muted");
        }
        match e {
            Heard::Muted(m) => {
                if *m && open {
                    return fail("muted mid-utterance");
                }
                muted = *m;
            }
            Heard::Onset { .. } if open => return fail("onset inside an utterance"),
            Heard::Onset { .. } => (open, held) = (true, false),
            Heard::Held { .. } if !open || held => return fail("held outside an utterance or twice"),
            Heard::Held { .. } => held = true,
            Heard::Rejected { .. } | Heard::Final { .. } if !open => return fail("ends an utterance that never began"),
            Heard::Rejected { .. } => open = false,
            Heard::Final { utterance, start, end, .. } => {
                if end.0 < start.0 {
                    return fail("ends before it starts");
                }
                if *utterance != next_utterance {
                    return Err(format!("event {i} ({e:?}): utterance {next_utterance} was next"));
                }
                let Some(next) = next_utterance.checked_add(1) else {
                    return fail("no utterance can follow u64::MAX");
                };
                next_utterance = next;
                open = false;
            }
        }
    }
    Ok(())
}

/// Checks a [`Stage`] with `member` in its scene: it reports an inventory, refuses an unknown member and a name
/// outside the inventory, takes a look at the camera (which every inventory allows) and an expression from the
/// inventory when it has one, and releases without failing.
///
/// # Errors
/// The first failing case.
pub fn stage(stage: &dyn Stage, member: &MemberId) -> Result<(), String> {
    let inventory = stage.inventory(member).map_err(|e| format!("inventory: {e}"))?;
    let nobody = MemberId::new("conformance-nobody").map_err(|e| e.to_string())?;
    if !matches!(stage.inventory(&nobody), Err(StageError::UnknownMember { .. })) {
        return Err("inventory: an unknown member was not refused".into());
    }
    let missing = ExpressionId::new("conformance-missing").map_err(|e| e.to_string())?;
    match stage.apply(member, &Intent::emote(missing, 1.0, 1.5), Seconds(0.0)) {
        Err(StageError::Refused(Refusal::UnknownExpression)) => {}
        other => return Err(format!("apply: an unknown expression gave {other:?}")),
    }
    let valid = std::iter::once(Intent::look_at(GazeTarget::Camera, 1.0))
        .chain(inventory.expressions.first().map(|known| Intent::emote(known.clone(), 1.0, 1.5)));
    for intent in valid {
        stage.apply(member, &intent, Seconds(0.0)).map_err(|e| format!("apply: {intent:?} was refused: {e}"))?;
    }
    stage.release(member, Seconds(0.0));
    Ok(())
}

/// Checks a [`VoiceCatalog`] for `provider`: it lists voices, each once, each one the provider can offer.
///
/// # Errors
/// The first failing voice.
pub fn catalog(catalog: &dyn VoiceCatalog, provider: &VoiceProvider) -> Result<(), String> {
    let voices = catalog.voices(provider).map_err(|e| format!("voices: {e}"))?;
    for (i, v) in voices.iter().enumerate() {
        if voices[..i].iter().any(|w| w.id == v.id) {
            return Err(format!("{} is listed twice", v.id));
        }
        if !v.offered_by(provider) {
            return Err(format!("{} is not from {provider:?}", v.id));
        }
    }
    Ok(())
}

/// Frames in the planner script: 10 s at 60 fps.
const PLANNER_FRAMES: u64 = 600;

/// The floor and speech at frame `i` of the planner script: idle, thinking, a question, listening to a co-host.
#[allow(clippy::cast_precision_loss)] // frame counts stay tiny
fn script(i: u64, private: bool) -> (MoveContext, TalkState) {
    let s = i as f64 / 60.0;
    let (floor, addressee, talk) = match s {
        s if s < 1.0 => (Floor::Idle, Addressee::Camera, TalkState::default()),
        s if s < 4.0 => (Floor::Thinking, Addressee::Camera, TalkState::default()),
        s if s < 7.0 => (Floor::Speaking, Addressee::Camera, TalkState::new(true, true, false, Seconds(s - 4.0))),
        s => (Floor::Listening, Addressee::member(0.5), TalkState::new(false, false, false, Seconds(s - 7.0))),
    };
    let ctx = MoveContext::new(floor, addressee);
    if private { (ctx.private(), TalkState::default()) } else { (ctx, talk) }
}

/// Runs the planner script, returning every frame's layer.
fn run_planner(planner: &mut dyn BigMovePlanner, seed: u64, private: bool) -> Vec<Layer> {
    let mut rng = Rng::new(seed);
    let knobs = StyleKnobs::default();
    (0..PLANNER_FRAMES)
        .map(|i| {
            let (ctx, talk) = script(i, private);
            let mut out = Layer::absent(BlendMode::Override);
            planner.plan(Tick::at_rate(i, 60.0), &ctx, &talk, &knobs, &mut rng, &mut out);
            out
        })
        .collect()
}

fn same_bits(a: &Layer, b: &Layer) -> bool {
    a.mask == b.mask
        && a.weight.to_bits() == b.weight.to_bits()
        && a.values.as_slice().iter().zip(b.values.as_slice()).all(|(x, y)| x.to_bits() == y.to_bits())
}

/// Checks a [`BigMovePlanner`] (§5, §7): over a 10 s script it writes only [`OWNED_CHANNELS`], gives the same bits for
/// the same seed, and stays inactive (no weight or an empty mask) on a private turn, since even a layer of rest values
/// would take its channels from the life layer.
///
/// # Errors
/// The first failing frame.
pub fn planner(make: &dyn Fn() -> Box<dyn BigMovePlanner>) -> Result<(), String> {
    let first = run_planner(make().as_mut(), 7, false);
    for (i, layer) in first.iter().enumerate() {
        if layer.is_active() && !layer.mask.without(OWNED_CHANNELS).is_empty() {
            return Err(format!(
                "frame {i}: writes channels it does not own: {:?}",
                layer.mask.without(OWNED_CHANNELS)
            ));
        }
    }
    let again = run_planner(make().as_mut(), 7, false);
    if let Some(i) = (0..first.len()).find(|&i| !same_bits(&first[i], &again[i])) {
        return Err(format!("frame {i}: the same seed gave different bits"));
    }
    for (i, layer) in run_planner(make().as_mut(), 7, true).iter().enumerate() {
        if layer.is_active() {
            return Err(format!("frame {i}: the layer is active on a private turn (weight {})", layer.weight));
        }
    }
    Ok(())
}
