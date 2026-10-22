//! The fake worker: a small process the supervisor tests launch, to exercise the watchdog (#1025).
//!
//! It writes one byte to stdout per beat, the heartbeat the supervisor's reader thread counts.
//!
//! - `--beat-ms N`: the beat period (default 33);
//! - `--startup-ms N`: silence before the first beat (default 0);
//! - `--crash-after-ms N`: exit with code 3 that long after starting;
//! - `--hang-after-ms N`: stop beating that long after starting, and stay alive.

use std::io::Write;
use std::time::{Duration, Instant};

/// The exit code of a scripted crash.
const CRASH_CODE: i32 = 3;

#[derive(Debug, Default)]
struct Script {
    beat: Duration,
    startup: Duration,
    crash_after: Option<Duration>,
    hang_after: Option<Duration>,
}

fn script() -> Result<Script, String> {
    let mut script = Script { beat: Duration::from_millis(33), ..Script::default() };
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let value = args.next().ok_or_else(|| format!("{flag} needs a number"))?;
        let ms: u64 = value.parse().map_err(|_| format!("{flag}: {value} isn't a whole number"))?;
        let n = Duration::from_millis(ms);
        match flag.as_str() {
            "--beat-ms" => script.beat = n.max(Duration::from_millis(1)),
            "--startup-ms" => script.startup = n,
            "--crash-after-ms" => script.crash_after = Some(n),
            "--hang-after-ms" => script.hang_after = Some(n),
            other => return Err(format!("unknown flag {other}")),
        }
    }
    Ok(script)
}

fn main() {
    let script = match script() {
        Ok(script) => script,
        Err(error) => {
            eprintln!("fake-worker: {error}");
            std::process::exit(2);
        }
    };
    let start = Instant::now();
    let after = |at: Option<Duration>| at.is_some_and(|at| start.elapsed() >= at);
    std::thread::sleep(script.startup);
    let mut out = std::io::stdout().lock();
    loop {
        if after(script.crash_after) {
            std::process::exit(CRASH_CODE);
        }
        if !after(script.hang_after) && (out.write_all(b".").is_err() || out.flush().is_err()) {
            // The supervisor closed the pipe: it is gone, so stop.
            return;
        }
        std::thread::sleep(script.beat);
    }
}
