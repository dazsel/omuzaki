//! Cloth quality modes (#198): what `Quality::Live` and `Quality::Offline` ask of the sheet and the solver, the key
//! that identifies a runtime's inputs, and the settle cache that lets a timeline scrub without re-simulating from the
//! start.
//!
//! # Live and Offline
//!
//! - **Live** steps the sheets as the spec gives them with the solver's live preset ([`SolverConfig::live`]: 3
//!   sub-steps of 2 sweeps per 1/120 s), so turning quality on for renders changes nothing about live cost.
//! - **Offline** builds every level twice as fine (each row and column of cells split in two: `2r - 1` rows, `2c`
//!   columns around a wrapped garment or `2c - 1` across an open one, clamped to the render grid) with the same pins
//!   (mapped by parameter) and runs [`OFFLINE_SUBSTEPS`] sub-steps of [`OFFLINE_ITERATIONS`] sweeps. The solver's
//!   compliances are resolution independent, so Offline adds detail, not different behaviour: the hem stays within
//!   1.5 cm of Live over a walk (`tests/cloth_quality.rs`).
//!
//! # Frame-exact replay (SH-05, decision 0009)
//!
//! An offline render clocks the cloth in whole sub-steps, never in frame times:
//! [`ClothRuntime::advance_to`](super::runtime::ClothRuntime::advance_to) runs exactly the sub-steps up to frame `k`'s
//! ([`ClothRuntime::substep_at`](super::runtime::ClothRuntime::substep_at)), so a render replayed from frame 0 with the
//! same poses is byte-identical whatever order the frames were requested in.
//!
//! # The settle cache
//!
//! A shot starts from a settled sheet (reset to the first frame's pose, then [`ClothRuntime::settle`]), and scrubbing
//! back should not redo it. [`SettleCache`] keeps snapshots ([`ClothSnapshot`]): the settled state under its key (the
//! runtime's [`key`](super::runtime::ClothRuntime::key), a hash of the settle pose and the duration) and checkpoints
//! along the shot by sub-step. Restoring one and advancing to frame `N` gives exactly the sequential result, because a
//! snapshot holds everything the runtime evolves. A cache belongs to one shot: [`SettleCache::clear`] it when the
//! shot's animation changes.

use omuzaki_char_api::{PoseSource, Quality};
use omuzaki_core::glam::{Vec3, Vec3A};

use super::runtime::{ClothGrid, ClothRuntime, ClothRuntimeError, ClothSnapshot, ClothSpec, MAX_LEVELS, SheetSize};
use super::solver::SolverConfig;

/// Sub-steps per 1/120 s physics step at Offline quality (a 1440 Hz sub-step).
pub const OFFLINE_SUBSTEPS: u32 = 12;

/// Gauss-Seidel sweeps per sub-step at Offline quality.
pub const OFFLINE_ITERATIONS: u32 = 4;

/// The solver configuration for `quality`.
pub fn solver_config(quality: Quality) -> SolverConfig {
    match quality {
        Quality::Live => SolverConfig::live(),
        Quality::Offline => {
            SolverConfig { substeps: OFFLINE_SUBSTEPS, iterations: OFFLINE_ITERATIONS, ..SolverConfig::quality() }
        }
    }
}

/// The sheet of `size` at `quality` over a grid of `grid_rows x grid_cols` (`wrap` around): as given at Live, twice as
/// fine at Offline (clamped to the grid).
pub fn sheet_size(quality: Quality, size: SheetSize, grid_rows: usize, grid_cols: usize, wrap: bool) -> SheetSize {
    match quality {
        Quality::Live => size,
        Quality::Offline => {
            let cols = if wrap { size.cols * 2 } else { (size.cols * 2).saturating_sub(1) };
            SheetSize { cols: cols.min(grid_cols), rows: (size.rows * 2).saturating_sub(1).min(grid_rows) }
        }
    }
}

/// The levels `spec` asks for at its quality, checked: 1 to [`MAX_LEVELS`], none finer than the grid, particle counts
/// not increasing.
///
/// # Errors
/// [`ClothRuntimeError::Spec`] naming what is wrong.
pub fn levels(spec: &ClothSpec, grid: &ClothGrid) -> Result<Vec<SheetSize>, ClothRuntimeError> {
    if spec.levels.is_empty() || spec.levels.len() > MAX_LEVELS {
        return Err(ClothRuntimeError::Spec("a cloth spec needs 1 to 4 levels"));
    }
    if spec.levels.iter().any(|s| s.rows > grid.rows || s.cols > grid.cols) {
        return Err(ClothRuntimeError::Spec("a level may not be finer than its grid"));
    }
    if !spec.levels.windows(2).all(|w| w[1].rows * w[1].cols <= w[0].rows * w[0].cols) {
        return Err(ClothRuntimeError::Spec("levels go from the finest to the coarsest"));
    }
    Ok(spec.levels.iter().map(|&s| sheet_size(spec.quality, s, grid.rows, grid.cols, grid.wrap)).collect())
}

/// A deterministic 64-bit hash (FNV-1a over little-endian bytes): the same inputs give the same key on every platform
/// and every run, which `std`'s hasher does not promise.
#[derive(Copy, Clone, Debug)]
pub struct KeyHasher(u64);

impl Default for KeyHasher {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyHasher {
    /// An empty hash.
    pub const fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    /// Adds bytes.
    pub fn bytes(&mut self, bytes: &[u8]) -> &mut Self {
        for &b in bytes {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        self
    }

    /// Adds an integer.
    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.bytes(&v.to_le_bytes())
    }

    /// Adds a size.
    pub fn usize(&mut self, v: usize) -> &mut Self {
        self.u64(v as u64)
    }

    /// Adds a flag.
    pub fn bool(&mut self, v: bool) -> &mut Self {
        self.bytes(&[u8::from(v)])
    }

    /// Adds a float's exact bits.
    pub fn f32(&mut self, v: f32) -> &mut Self {
        self.bytes(&v.to_bits().to_le_bytes())
    }

    /// Adds a vector's exact bits.
    pub fn vec3(&mut self, v: Vec3) -> &mut Self {
        self.f32(v.x).f32(v.y).f32(v.z)
    }

    /// Adds a SIMD vector's exact bits.
    pub fn vec3a(&mut self, v: Vec3A) -> &mut Self {
        self.vec3(v.into())
    }

    /// Adds a string (length first, so `"ab", "c"` and `"a", "bc"` differ).
    pub fn str(&mut self, s: &str) -> &mut Self {
        self.usize(s.len()).bytes(s.as_bytes())
    }

    /// The hash.
    pub fn finish(&self) -> u64 {
        self.0
    }
}

/// Snapshots of one shot's cloth, by key and sub-step (see the [module docs](self)). Holds at most `capacity`
/// checkpoints; the oldest inserted goes first.
#[derive(Clone, Debug)]
pub struct SettleCache {
    settled: Vec<(u64, ClothSnapshot)>,
    checkpoints: Vec<ClothSnapshot>,
    capacity: usize,
}

impl SettleCache {
    /// An empty cache keeping up to `capacity` checkpoints (and one settled state per runtime and settle pose).
    pub fn new(capacity: usize) -> Self {
        Self { settled: Vec::new(), checkpoints: Vec::new(), capacity }
    }

    /// Forgets everything (the shot's animation changed).
    pub fn clear(&mut self) {
        self.settled.clear();
        self.checkpoints.clear();
    }

    /// Checkpoints held.
    pub fn len(&self) -> usize {
        self.checkpoints.len()
    }

    /// Whether no checkpoint is held.
    pub fn is_empty(&self) -> bool {
        self.checkpoints.is_empty()
    }

    /// Resets `runtime` to `pose` and settles it for `seconds`, or restores the state a previous call with the same
    /// runtime key, pose and duration stored. Returns whether it was a hit. The result is identical either way.
    pub fn settle(&mut self, runtime: &mut ClothRuntime, pose: &dyn PoseSource, seconds: f32) -> bool {
        let key = settle_key(runtime, pose, seconds);
        if let Some((_, snapshot)) = self.settled.iter().find(|(k, _)| *k == key)
            && runtime.restore(snapshot).is_ok()
        {
            return true;
        }
        omuzaki_char_api::PartRuntime::reset(runtime, pose);
        runtime.settle(pose, seconds);
        self.settled.push((key, runtime.snapshot()));
        false
    }

    /// Keeps a checkpoint of `runtime` at its current clock (replacing one at the same clock).
    pub fn checkpoint(&mut self, runtime: &ClothRuntime) {
        let snapshot = runtime.snapshot();
        let (key, clock) = (snapshot.key(), snapshot.clock());
        self.checkpoints.retain(|s| !(s.key() == key && s.clock() == clock));
        if self.capacity == 0 {
            return;
        }
        if self.checkpoints.len() >= self.capacity {
            self.checkpoints.remove(0);
        }
        self.checkpoints.push(snapshot);
    }

    /// Restores the latest checkpoint of `runtime` at or before sub-step `substep` and returns its clock, or `None`
    /// (the runtime is unchanged) when there is none: settle again then.
    pub fn scrub_to(&self, runtime: &mut ClothRuntime, substep: u64) -> Option<u64> {
        let key = runtime.key();
        let best =
            self.checkpoints.iter().filter(|s| s.key() == key && s.clock() <= substep).max_by_key(|s| s.clock())?;
        runtime.restore(best).ok()?;
        Some(best.clock())
    }
}

/// The settled state's key: the runtime, the pose its joints and root are in, and the duration.
fn settle_key(runtime: &ClothRuntime, pose: &dyn PoseSource, seconds: f32) -> u64 {
    let mut h = KeyHasher::new();
    h.u64(runtime.key()).f32(seconds);
    for joint in runtime.joints() {
        match pose.skin_matrix(joint) {
            Some(m) => {
                for v in m.to_cols_array() {
                    h.f32(v);
                }
            }
            None => {
                h.bool(false);
            }
        }
    }
    for v in pose.root().to_cols_array() {
        h.f32(v);
    }
    h.finish()
}
