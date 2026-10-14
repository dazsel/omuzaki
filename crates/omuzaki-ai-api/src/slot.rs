//! Slots: the versioned interfaces models plug into. The crate that owns a slot's data (speech in `omuzaki-speech`,
//! trackers in `omuzaki-track`, voice conversion in `omuzaki-audio`) implements [`Slot`] and registers it with
//! [`register_slot!`](crate::register_slot); nothing lists slots centrally (spec §39 rule 2).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{EvalEnv, EvalError, EvalLevel, EvalReport, Likeness, Session, Signature, SlotRef, StreamSpec};

/// How a slot's models are driven.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SlotMode {
    /// One [`Session::run`] per request (a camera frame, an utterance, a sentence to speak) on the caller's thread.
    Call {
        /// A run taking longer than this counts as a stall: a sandboxed model's worker is stopped and the slot falls
        /// back (ADR 0016 rule 4).
        stall_ms: u32,
    },
    /// Blocks fed from a real-time thread through [`BlockSink`](crate::BlockSink) and
    /// [`BlockSource`](crate::BlockSource); models declare their [`StreamSpec`].
    Stream {
        /// Blocks in flight: the model has this many block periods to answer each block.
        queue: u32,
        /// After this many missed blocks in a row the slot gives up on the model for the stream and falls back.
        stall_blocks: u32,
    },
}

/// What people, agents and the registry know about a slot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SlotInfo {
    /// The slot and its interface version.
    pub slot: SlotRef,
    /// One line: what models in this slot do.
    pub summary: String,
    /// What the app does when no model can run, in words a person reads on the health meter ("live captions are
    /// off", "the voice stays dry").
    pub without_model: String,
    /// Request/response or streaming, with its deadlines.
    pub mode: SlotMode,
    /// The default thread count for its sessions ([`SessionOptions::threads`](crate::SessionOptions::threads)).
    pub threads: u16,
    /// What of a real person a model in this slot reproduces when its card says it
    /// [`imitates`](crate::ModelCard::imitates) someone, and so what their consent must cover (SE-05). `None` for
    /// slots whose models can't imitate anyone (voice activity, speech to text); a model there that claims to is
    /// refused.
    pub likeness: Option<Likeness>,
}

/// A slot: a fixed interface (its [`Signature`]) and the evaluation every model in it must pass. Implemented by the
/// crate that owns the slot's data types and registered with [`register_slot!`](crate::register_slot).
///
/// A new interface version is a new slot registration (`asr@2` next to `asr@1`), never an edit of an old one: models
/// and recorded takes name the version they were made for.
pub trait Slot: Send + Sync {
    /// Its id, version, mode and words.
    fn info(&self) -> SlotInfo;

    /// The interface every model must present, ports named. A streaming slot leaves the block dimension `any`.
    fn signature(&self) -> Signature;

    /// A streaming slot's limits on a model's [`StreamSpec`], beyond [`StreamSpec::is_valid`]: voice conversion
    /// refuses blocks longer than 100 ms, which would make the delay useless live. The default accepts any valid spec.
    ///
    /// # Errors
    /// Why the spec doesn't suit this slot, in words a person reads.
    fn check_stream(&self, spec: &StreamSpec) -> Result<(), String> {
        let _ = spec;
        Ok(())
    }

    /// Measures `session` on this slot's test sets. [`EvalLevel::Smoke`] sets the floors a user model must meet
    /// before it is enabled (ADR 0016 rule 4) and must finish in seconds; [`EvalLevel::Full`] is the suite behind
    /// the model card and the gates.
    ///
    /// # Errors
    /// [`EvalError`] when no report could be made; a report whose checks fail is not an error.
    fn evaluate(&self, session: &mut dyn Session, env: &dyn EvalEnv, level: EvalLevel)
    -> Result<EvalReport, EvalError>;
}

/// One registered slot; build with [`SlotRegistration::of`] through [`register_slot!`](crate::register_slot).
pub struct SlotRegistration {
    /// Builds an instance.
    pub create: fn() -> Box<dyn Slot>,
}

fn make_slot<T: Slot + Default + 'static>() -> Box<dyn Slot> {
    Box::new(T::default())
}

impl SlotRegistration {
    /// The registration for `T`. A `const fn` so [`register_slot!`](crate::register_slot) works at link time.
    pub const fn of<T: Slot + Default + 'static>() -> Self {
        Self { create: make_slot::<T> }
    }
}

inventory::collect!(SlotRegistration);

/// Registers a slot type: `register_slot!(AsrSlotV1);`. Put it next to the type.
#[macro_export]
macro_rules! register_slot {
    ($ty:ty) => {
        $crate::__inventory::submit! { $crate::SlotRegistration::of::<$ty>() }
        $crate::__record_registration!("slot", $ty);
    };
}

/// Every registered slot, in link order.
pub fn slots() -> impl Iterator<Item = Box<dyn Slot>> {
    inventory::iter::<SlotRegistration>.into_iter().map(|r| (r.create)())
}

/// The slot registered for exactly this id and version.
pub fn find_slot(slot: &SlotRef) -> Option<Box<dyn Slot>> {
    slots().find(|s| &s.info().slot == slot)
}
