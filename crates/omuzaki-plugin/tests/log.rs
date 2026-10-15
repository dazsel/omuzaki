//! The plugin log (#1353, design §7 "Log lines"): the caps, the dropped-lines line, the ring and its paging, the
//! rotated file, and a slow file never blocking `host.log`.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_plugin::log::{FileSink, LOG_FILE_MAX_BYTES, LineSink, LogWriter, PluginLog};
use omuzaki_plugin_api::limits::{LOG_BURST, LOG_LINE_MAX_BYTES, LOG_LINES_PER_SECOND, LOG_RING_LINES};
use omuzaki_plugin_api::{LogLevel, LogLine, PluginId};

fn id() -> PluginId {
    PluginId::new("example").unwrap()
}

fn log_on(clock: &Arc<FakeClock>, writer: Option<&LogWriter>) -> PluginLog {
    PluginLog::new(id(), clock.clone(), writer)
}

#[test]
fn a_flood_keeps_the_burst_and_one_line_says_how_many_were_dropped() {
    let clock = Arc::new(FakeClock::new(Seconds(10.0)));
    let log = log_on(&clock, None);
    let kept = (0..1000).filter(|i| log.push(LogLevel::Info, &format!("line {i}"), false)).count();
    assert_eq!(kept, LOG_BURST as usize);
    clock.advance(Seconds(1.0 / f64::from(LOG_LINES_PER_SECOND)));
    assert!(log.push(LogLevel::Info, "after", false));
    let lines = log.page(0, usize::MAX);
    assert_eq!(lines.len(), LOG_BURST as usize + 2);
    let tail: Vec<&str> = lines[LOG_BURST as usize..].iter().map(|l| l.text.as_str()).collect();
    assert_eq!(tail, [format!("{} lines dropped", 1000 - LOG_BURST).as_str(), "after"], "one line per flood");
}

#[test]
fn a_long_line_is_cut_at_a_character_boundary() {
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    let log = log_on(&clock, None);
    log.push(LogLevel::Info, &"a".repeat(5 * 1024), false);
    // "é" is two bytes; placed across the cut, it must be dropped whole.
    let text = format!("{}é tail", "b".repeat(LOG_LINE_MAX_BYTES - 1));
    log.push(LogLevel::Info, &text, false);
    let lines = log.page(0, 10);
    assert_eq!(lines[0].text.len(), LOG_LINE_MAX_BYTES);
    assert_eq!(lines[1].text, "b".repeat(LOG_LINE_MAX_BYTES - 1));
}

#[test]
fn paging_survives_the_ring_wrapping() {
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    let log = log_on(&clock, None);
    let total = LOG_RING_LINES + 250;
    for i in 0..total {
        clock.advance(Seconds(1.0));
        log.push(LogLevel::Info, &format!("line {i}"), i % 2 == 0);
    }
    let all = log.page(0, usize::MAX);
    assert_eq!(all.len(), LOG_RING_LINES, "the oldest lines are gone");
    assert_eq!(all[0].seq, 251);
    assert!(all.windows(2).all(|w| w[1].seq == w[0].seq + 1));
    let page = log.page(1200, 5);
    assert_eq!(page.iter().map(|l| l.seq).collect::<Vec<_>>(), [1201, 1202, 1203, 1204, 1205]);
    assert_eq!(page[0].text, "line 1200");
    assert!(page[0].dry_run && !page[1].dry_run, "dry-run lines are marked");
}

#[test]
fn the_file_rotates_once_and_a_second_rotation_replaces_the_old_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut sink = FileSink::new(dir.path());
    let line = |seq: u64, dry_run: bool| LogLine {
        seq,
        at: Seconds(1.0),
        level: LogLevel::Info,
        text: "x".repeat(1000),
        dry_run,
    };
    let current = dir.path().join("example.log");
    let old = dir.path().join("example.log.1");
    let mut seq = 0;
    while !old.exists() {
        seq += 1;
        sink.write(&id(), &line(seq, seq == 1)).unwrap();
    }
    let first_old = std::fs::read_to_string(&old).unwrap();
    assert!(first_old.starts_with("1 1.000 info (dry run) "), "the dry-run mark is in the file too");
    assert!(first_old.len() as u64 <= LOG_FILE_MAX_BYTES);
    assert!(std::fs::metadata(&current).unwrap().len() < LOG_FILE_MAX_BYTES);
    let rotated_at = seq;
    while std::fs::read_to_string(&old).unwrap() == first_old {
        seq += 1;
        sink.write(&id(), &line(seq, false)).unwrap();
    }
    let second_old = std::fs::read_to_string(&old).unwrap();
    assert!(second_old.starts_with(&format!("{rotated_at} ")), "the second rotation replaced the first old file");
    let files: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
    assert_eq!(files.len(), 2, "one current file and one old file");
}

/// A sink that holds every line until the test releases it.
struct Blocked {
    release: Arc<Mutex<Receiver<()>>>,
    seen: Sender<String>,
}

impl LineSink for Blocked {
    fn write(&mut self, _: &PluginId, line: &LogLine) -> std::io::Result<()> {
        let _released = self.release.lock().unwrap().recv();
        self.seen.send(line.text.clone()).map_err(std::io::Error::other)
    }
}

#[test]
fn a_blocked_file_never_blocks_the_log() {
    let (release, wait) = channel();
    let (seen, lines) = channel();
    let writer = LogWriter::spawn(Box::new(Blocked { release: Arc::new(Mutex::new(wait)), seen })).unwrap();
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    let log = log_on(&clock, Some(&writer));
    let started = Instant::now();
    for i in 0..10 {
        assert!(log.push(LogLevel::Info, &format!("line {i}"), false));
    }
    assert!(started.elapsed() < Duration::from_millis(500), "pushing never waits for the file");
    assert_eq!(log.page(0, 100).len(), 10);
    for _ in 0..10 {
        release.send(()).unwrap();
    }
    let written: Vec<String> = (0..10).map(|_| lines.recv_timeout(Duration::from_secs(5)).unwrap()).collect();
    assert_eq!(written[9], "line 9");
    drop(writer);
    assert_eq!(log.page(0, 100).len(), 10, "the ring outlives the writer");
}
