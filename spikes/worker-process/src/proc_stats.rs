//! The host's own CPU, memory and open-file cost, read from `/proc` (Linux).

use std::time::Instant;

use crate::Error;

/// One reading of this process.
#[derive(Clone, Copy, Debug)]
pub struct ProcSample {
    /// When it was taken.
    pub at: Instant,
    /// User + system CPU seconds this process has used.
    pub cpu_s: f64,
    /// CPU seconds used by children this process has waited for (the killed workers).
    pub child_cpu_s: f64,
    /// Resident set now, KiB.
    pub rss_kib: u64,
    /// Resident set high-water mark, KiB.
    pub peak_rss_kib: u64,
    /// Open file descriptors.
    pub fds: usize,
}

/// Clock ticks per second as the kernel reports CPU times (`getconf CLK_TCK`; 100 on every Linux build we meet).
#[must_use]
pub fn clock_ticks_per_second() -> f64 {
    std::process::Command::new("getconf")
        .arg("CLK_TCK")
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(100.0)
}

/// utime, stime, cutime, cstime from the text of `/proc/<pid>/stat`. The command name is in parentheses and may hold
/// spaces, so fields are counted from the last `)`.
fn parse_stat(text: &str) -> Option<[u64; 4]> {
    let after = &text[text.rfind(')')? + 1..];
    let fields: Vec<&str> = after.split_whitespace().collect();
    // After the name: state is field 3, so utime (14) is index 11.
    let at = |i: usize| fields.get(i)?.parse().ok();
    Some([at(11)?, at(12)?, at(13)?, at(14)?])
}

/// The value of a `Key:   123 kB` line of `/proc/<pid>/status`.
fn parse_status_kib(text: &str, key: &str) -> Option<u64> {
    text.lines().find_map(|line| line.strip_prefix(key)?.strip_prefix(':')?.split_whitespace().next()?.parse().ok())
}

/// Reads this process.
///
/// # Errors
/// Fails when `/proc` cannot be read or has an unexpected layout.
pub fn sample(ticks_per_second: f64) -> Result<ProcSample, Error> {
    let bad = |what: &str| Error::Config(format!("unexpected /proc layout: {what}"));
    let stat = std::fs::read_to_string("/proc/self/stat").map_err(Error::io("reading /proc/self/stat"))?;
    let status = std::fs::read_to_string("/proc/self/status").map_err(Error::io("reading /proc/self/status"))?;
    let [user, system, child_user, child_system] = parse_stat(&stat).ok_or_else(|| bad("stat"))?;
    let fds = std::fs::read_dir("/proc/self/fd").map_err(Error::io("listing /proc/self/fd"))?.count();
    #[allow(clippy::cast_precision_loss)] // tick counts far below 2^53
    let secs = |ticks: u64| ticks as f64 / ticks_per_second;
    Ok(ProcSample {
        at: Instant::now(),
        cpu_s: secs(user + system),
        child_cpu_s: secs(child_user + child_system),
        rss_kib: parse_status_kib(&status, "VmRSS").ok_or_else(|| bad("VmRSS"))?,
        peak_rss_kib: parse_status_kib(&status, "VmHWM").ok_or_else(|| bad("VmHWM"))?,
        fds,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_fields_are_counted_from_the_last_paren() {
        let line = "42 (wp spike) (x) S 1 2 3 4 5 6 7 8 9 10 111 222 333 444 20 0";
        assert_eq!(parse_stat(line), Some([111, 222, 333, 444]));
    }

    #[test]
    fn status_lines_parse() {
        let text = "Name:\tx\nVmHWM:\t  2048 kB\nVmRSS:\t  1024 kB\n";
        assert_eq!(parse_status_kib(text, "VmRSS"), Some(1024));
        assert_eq!(parse_status_kib(text, "VmHWM"), Some(2048));
        assert_eq!(parse_status_kib(text, "VmSwap"), None);
    }

    #[test]
    fn this_process_can_be_sampled() {
        let s = sample(clock_ticks_per_second()).unwrap();
        assert!(s.rss_kib > 0 && s.fds > 2);
    }
}
