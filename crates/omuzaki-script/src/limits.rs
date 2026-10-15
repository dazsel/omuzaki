//! The sandbox limits (`docs/design/scripting.md` section 5).
//!
//! # The worker thread
//!
//! Rhai builds, sizes, clones and drops nested arrays and maps recursively, once per level, and a stack overflow aborts
//! the whole process. So every run happens on a dedicated worker thread whose stack is sized for the deepest nesting
//! the limits let a script build ([`Limits::worker_stack_bytes`]):
//!
//! - Rhai checks the size of every array and map a script builds, and it counts the items of the arrays and maps
//!   nested inside too, so a value can nest no deeper than [`Limits::array_len`] arrays plus [`Limits::map_len`] maps,
//!   whatever the operation limit is. A script that builds one level per step reaches the size error first.
//! - One level costs at most `STACK_PER_LEVEL` bytes of stack, while it is built, sized, cloned or dropped. Measured on
//!   Linux with a probe that nests `v = [v]`, `v = #{ a: v }` and both alternately until the size limit stops it, one
//!   level takes about 210 bytes of stack for an array and 345 for a map in a release build, and 1,300 and 2,200 in a
//!   debug build, whose frames are far larger. `STACK_PER_LEVEL` is 1 KiB for a release build (three times the
//!   largest) and 4 KiB for a debug build (nearly twice).
//! - The engine's own frames (parsing and evaluating within [`Limits::expr_depth`] and [`Limits::call_depth`]) need
//!   `STACK_BASE` more.
//!
//! The console and job limits (110,001 levels) come to about 123 MiB of stack in a release build and 446 MiB in a debug
//! build. It is reserved, not used: the operating system backs only the pages a script touches. A limit set that would
//! need more than [`Limits::MAX_WORKER_STACK_BYTES`] is refused (`ScriptErrorKind::Refused`) instead of run, so raising
//! `array_len` or `map_len` can't quietly lift the guarantee. The cap is above the debug figure on purpose: a lower cap
//! would refuse the shipped limits in a debug build, and the alternative the task allowed (a depth check) has no hook in
//! Rhai to stand on. On `wasm32` there are no threads, so a run happens on the caller's thread and a web player must
//! size its own stack.

/// Every limit a run is held to. The fields are public so a caller can tighten one; [`Limits::console`] and
/// [`Limits::job`] are the two shipped tables.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Longest source, in bytes, checked before parsing.
    pub source_bytes: usize,
    /// Rhai operations.
    pub operations: u64,
    /// Inner op calls.
    pub calls: u32,
    /// Script function call depth.
    pub call_depth: usize,
    /// Expression depth outside functions.
    pub expr_depth: usize,
    /// Expression depth inside functions.
    pub fn_expr_depth: usize,
    /// Longest string, in bytes.
    pub string_bytes: usize,
    /// Longest array.
    pub array_len: usize,
    /// Largest map.
    pub map_len: usize,
    /// Output lines kept before the "output truncated" line.
    pub output_lines: usize,
    /// Output bytes kept before the "output truncated" line.
    pub output_bytes: usize,
    /// Wall-clock seconds, read from the host's clock.
    pub seconds: u32,
    /// Longest result, as JSON bytes.
    pub result_bytes: usize,
    /// Deepest nesting of arrays and maps in a value that crosses the sandbox (a result, `params`, op arguments and
    /// outputs); 0 allows scalars only. Checked without recursion, so a deep value can't overflow the stack.
    pub value_depth: usize,
}

impl Limits {
    /// The most seconds a job may ask for.
    pub const MAX_JOB_SECONDS: u32 = 600;

    /// The most stack a run's worker thread may need, in bytes. Limits that need more are refused.
    pub const MAX_WORKER_STACK_BYTES: usize = 512 * 1024 * 1024;

    /// Stack for one level of nesting, in bytes, with the margin the module docs give.
    const STACK_PER_LEVEL: usize = if cfg!(debug_assertions) { 4096 } else { 1024 };

    /// Stack for the engine's own frames, in bytes.
    pub(crate) const STACK_BASE: usize = 16 * 1024 * 1024;

    /// The Rhai-enforced limits that Rhai reads 0 as "unlimited" for, so 0 would lift the limit instead of
    /// tightening it. A run with any of them at 0 is refused ([`Limits::zero_engine_limit`]).
    const ENGINE_FIELDS: [&'static str; 7] =
        ["operations", "call_depth", "expr_depth", "fn_expr_depth", "string_bytes", "array_len", "map_len"];

    /// The name of the first engine-enforced limit that is 0, if any: Rhai treats 0 there as no limit at all, which
    /// is the opposite of what a caller tightening a limit means. [`ScriptHost::run`](crate::ScriptHost::run) fails
    /// closed on it. The other limits (`calls`, `seconds`, the output and size caps) are ours and deny at 0.
    pub fn zero_engine_limit(&self) -> Option<&'static str> {
        let values = [
            self.operations,
            self.call_depth as u64,
            self.expr_depth as u64,
            self.fn_expr_depth as u64,
            self.string_bytes as u64,
            self.array_len as u64,
            self.map_len as u64,
        ];
        Self::ENGINE_FIELDS.into_iter().zip(values).find_map(|(name, value)| (value == 0).then_some(name))
    }

    /// The stack, in bytes, the worker thread of a run under these limits gets: enough for the deepest value the size
    /// limits let a script nest, plus the engine's own frames (see the module docs in `limits.rs`). `None` when that is more
    /// than [`MAX_WORKER_STACK_BYTES`](Self::MAX_WORKER_STACK_BYTES), in which case a run is refused.
    pub fn worker_stack_bytes(&self) -> Option<usize> {
        // Every array and map of a nested value counts toward the totals Rhai limits, and the outermost one is the
        // value itself, so this is the deepest chain of containers a script can hold.
        let levels = self.array_len.checked_add(self.map_len)?.checked_add(1)?;
        let bytes = levels.checked_mul(Self::STACK_PER_LEVEL)?.checked_add(Self::STACK_BASE)?;
        (bytes <= Self::MAX_WORKER_STACK_BYTES).then_some(bytes)
    }

    /// What a person in the console or the CLI gets.
    pub fn console() -> Self {
        Self {
            source_bytes: 64 * 1024,
            operations: 1_000_000,
            calls: 10_000,
            call_depth: 64,
            expr_depth: 64,
            fn_expr_depth: 32,
            string_bytes: 1024 * 1024,
            array_len: 100_000,
            map_len: 10_000,
            output_lines: 1_000,
            output_bytes: 64 * 1024,
            seconds: 10,
            result_bytes: 64 * 1024,
            value_depth: 64,
        }
    }

    /// What `script.run` gets: ten times the operations, and `seconds` of wall clock (120 by default, 600 at most).
    pub fn job(seconds: Option<u32>) -> Self {
        Self { operations: 10_000_000, seconds: seconds.unwrap_or(120).min(Self::MAX_JOB_SECONDS), ..Self::console() }
    }
}
