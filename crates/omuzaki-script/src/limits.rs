//! The sandbox limits (`docs/design/scripting.md` section 5).

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
