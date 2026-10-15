//! What a host run is asked to do, on either platform.

/// What to run.
#[derive(Clone, Debug)]
pub struct HostOptions {
    /// Kills to perform.
    pub kills: u32,
    /// Seed of the kill schedule.
    pub seed: u64,
    /// Frame size.
    pub size: (u32, u32),
    /// Output rate.
    pub fps: f64,
    /// Seconds of undisturbed running before the kills.
    pub baseline_s: u64,
    /// Workers started and stopped one at a time, for cold-start timings no kill can censor.
    pub cold_starts: u32,
    /// Earliest and latest kill, ms after a worker's spawn.
    pub kill_window_ms: (u64, u64),
    /// Extra environment for the worker (for example `MESA_SHADER_CACHE_DISABLE=true`).
    pub worker_env: Vec<(String, String)>,
}
