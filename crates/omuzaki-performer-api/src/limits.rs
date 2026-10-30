//! The performer's limits, in one place (`ai-performer.md` §4 to §8). The loop, the parser, the chunker, chat intake
//! and the fakes all read these, so a limit changes here and nowhere else.
//!
//! Times are seconds unless the name says otherwise.

/// Characters in one persona text field (`personality`, `backstory`, `style`, one alias, ...): §4.
pub const PERSONA_FIELD_CHARS: usize = 1_000;
/// Items in one persona list (`aliases`, `languages`, `catchphrases`, each `boundaries` list): §4.
pub const PERSONA_LIST_ITEMS: usize = 16;
/// Few-shot samples in a persona: §4.
pub const PERSONA_SAMPLES: usize = 6;
/// Tokens in the rendered persona block, by the LLM seam's local estimate; `persona.set` refuses more: §4.
pub const PERSONA_BLOCK_TOKENS: u64 = 2_000;
/// Hex characters of a persona revision (the BLAKE3 of the canonical TOML, truncated): §4.
pub const PERSONA_REVISION_HEX: usize = 12;

/// A reply ready while the human holds the floor waits at most this long, then is dropped: §5.1.
pub const FLOOR_WAIT_S: f64 = 3.0;
/// A final transcript within this long after the performer's last reply ended (or was cut) is addressed: §5.1.
pub const FOLLOW_UP_WINDOW_S: f64 = 8.0;
/// Names at least this long match an utterance within edit distance [`NAME_MAX_EDITS`]: §5.1.
pub const NAME_FUZZY_MIN_LETTERS: usize = 5;
/// The edit distance a long name may be off by and still address the performer: §5.1.
pub const NAME_MAX_EDITS: usize = 1;
/// Speech per turn; the chunker stops at the sentence that crosses it: §5.1, §7.1.
pub const TURN_SPEECH_S: f64 = 20.0;
/// The window of the performer's talk share: §5.1.
pub const TALK_SHARE_WINDOW_S: f64 = 300.0;
/// The co-host's default talk share in [`TALK_SHARE_WINDOW_S`]; above it only addressed turns run: §5.1.
pub const TALK_SHARE_MAX: f32 = 0.4;

/// A first chunk may end at `, ; : —` once it has this many words: §5.2.
pub const FIRST_CHUNK_MIN_WORDS: usize = 8;
/// Any chunk is forced at this many characters, on the last space: §5.2.
pub const CHUNK_MAX_CHARS: usize = 220;

/// Consecutive speech frames (32 ms each) that make the speech host's `Held`: §5.4.
pub const BARGE_IN_FRAMES: u32 = 3;
/// Words in a final transcript after a cut that still count as a backchannel ("yeah", "mm-hm"): §5.4.
pub const BACKCHANNEL_MAX_WORDS: usize = 2;
/// A final transcript overlapping the performer's speech with at least this token Jaccard is echo: §5.5.
pub const ECHO_JACCARD: f32 = 0.8;

/// Silence on both mic and performer before a proactive turn: §5.7.
pub const PROACTIVE_SILENCE_S: f64 = 20.0;
/// At most one proactive turn per this long: §5.7.
pub const PROACTIVE_INTERVAL_S: f64 = 90.0;
/// At most one glance per turn every this long: §5.7.
pub const GLANCE_INTERVAL_S: f64 = 10.0;
/// The shortest glance interval a person may set: §5.7.
pub const GLANCE_INTERVAL_FLOOR_S: f64 = 3.0;
/// A glance's long side, in pixels: §5.7.
pub const GLANCE_MAX_PX: u32 = 768;

/// Characters between `[` and `]`, brackets included; a longer run is dropped: §6.2.
pub const TAG_MAX_CHARS: usize = 64;
/// Public turns in the prompt: §6.3.
pub const HISTORY_TURNS: usize = 12;
/// Tokens of public turns in the prompt; the oldest are dropped first: §6.3.
pub const HISTORY_TOKENS: u64 = 4_000;
/// Chat messages per turn: §6.3.
pub const CHAT_PER_TURN: usize = 5;
/// Characters of chat per turn: §6.3.
pub const CHAT_CHARS_PER_TURN: usize = 1_500;
/// A chat message longer than this is dropped: §6.3.
pub const CHAT_MESSAGE_CHARS: usize = 300;
/// A viewer's name is cut to this many characters: §6.3.
pub const CHAT_NAME_CHARS: usize = 25;
/// The same user and text within this long is a repeat: §6.3.
pub const CHAT_REPEAT_S: f64 = 60.0;

/// The default `emote` hold: §7.1.
pub const EMOTE_HOLD_DEFAULT_S: f32 = 1.5;
/// The shortest `emote` hold: §7.1.
pub const EMOTE_HOLD_MIN_S: f32 = 0.3;
/// The longest `emote` hold: §7.1.
pub const EMOTE_HOLD_MAX_S: f32 = 8.0;
/// The default `look` hold (the §10 example's).
pub const LOOK_HOLD_DEFAULT_S: f32 = 1.5;
/// The shortest `look` hold: §7.1.
pub const LOOK_HOLD_MIN_S: f32 = 0.5;
/// The longest `look` hold: §7.1.
pub const LOOK_HOLD_MAX_S: f32 = 10.0;
/// The longest clip a `clip` intent may play: §7.1.
pub const PLAY_CLIP_MAX_S: f32 = 30.0;
/// At most one `move` per this long: §7.1.
pub const MOVE_INTERVAL_S: f64 = 10.0;
/// The fastest a `move` walks, in metres per second: §7.1.
pub const MOVE_MAX_SPEED: f32 = 1.0;
/// The same emote at most once per this long: §7.1.
pub const SAME_EMOTE_COOLDOWN_S: f64 = 2.0;
/// Intents per second, sustained, per performer: §7.1.
pub const INTENTS_PER_S: u32 = 2;
/// Intents in one burst, per performer: §7.1.
pub const INTENT_BURST: u32 = 6;

/// Events a subscriber's queue holds; the oldest are dropped: §8.
pub const EVENT_QUEUE: usize = 256;

/// The default per-show money cap for a hosted model, in micro-dollars ($1.00): §3.
pub const SHOW_CAP_MICRO_USD: u64 = 1_000_000;

/// While the person talks, a listening face from their partial words at most once per this long (#2453).
pub const LISTEN_FACE_INTERVAL_S: f64 = 1.0;
/// A listener's nod at most once per this long (#2453).
pub const NOD_GAP_S: f64 = 2.0;
/// How strongly a backchannel nod dips the head, `0..=1` of a full nod.
pub const BACKCHANNEL_NOD: f32 = 0.6;
/// The decision layer's faces peak at most this strong, so a reaction never outshouts the model's own tags (#2453).
pub const REACTION_APEX: f32 = 0.85;
/// The decision layer's faces are this much as strong while the character speaks, under its words' own (#2453).
pub const SPEAKING_DAMP: f32 = 0.8;
