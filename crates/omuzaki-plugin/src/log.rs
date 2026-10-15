//! A plugin's log (design §7, "Log lines"): `host.log`, stdout and stderr land in one [`PluginLog`] per plugin.
//!
//! - **Caps:** a line is cut to [`LOG_LINE_MAX_BYTES`] at a character boundary; lines pass a token bucket of
//!   [`LOG_LINES_PER_SECOND`] with a burst of [`LOG_BURST`] on the runner's clock. A flood drops what the bucket
//!   refuses, and one "N lines dropped" line is written when lines flow again, not one per dropped line.
//! - **The ring:** the last [`LOG_RING_LINES`] lines, each with a sequence number that keeps rising, so a reader pages
//!   with `after` even once the oldest lines are gone.
//! - **The file:** a [`LogWriter`] thread hands kept lines to a [`LineSink`]; [`FileSink`] appends them to
//!   `<dir>/<id>.log` and rotates at [`LOG_FILE_MAX_BYTES`] to one `<id>.log.1`. Pushing a line only sends it to the
//!   thread, so a slow disk never blocks `host.log`.

use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;

use omuzaki_core::time::{Clock, Seconds};
use omuzaki_plugin_api::limits::{LOG_BURST, LOG_LINE_MAX_BYTES, LOG_LINES_PER_SECOND, LOG_RING_LINES};
use omuzaki_plugin_api::{LogLevel, LogLine, PluginId};

/// The size at which a plugin's log file rotates, in bytes.
pub const LOG_FILE_MAX_BYTES: u64 = 1024 * 1024;

/// `text` cut to at most [`LOG_LINE_MAX_BYTES`] bytes, never inside a character.
pub fn cut_line(text: &str) -> &str {
    let mut end = text.len().min(LOG_LINE_MAX_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[derive(Debug)]
struct State {
    lines: VecDeque<LogLine>,
    next_seq: u64,
    tokens: f64,
    at: Seconds,
    dropped: u64,
}

/// One plugin's log.
pub struct PluginLog {
    id: PluginId,
    clock: Arc<dyn Clock>,
    sink: Option<Sender<Message>>,
    state: Mutex<State>,
}

impl PluginLog {
    /// An empty log for `id` on `clock`, writing kept lines to `writer` when given.
    pub fn new(id: PluginId, clock: Arc<dyn Clock>, writer: Option<&LogWriter>) -> Self {
        let at = clock.now();
        let state = State { lines: VecDeque::new(), next_seq: 1, tokens: f64::from(LOG_BURST), at, dropped: 0 };
        Self { id, clock, sink: writer.and_then(LogWriter::sender), state: Mutex::new(state) }
    }

    /// Adds a line, cut to size, unless the bucket is empty; true when it was kept. A line written during a dry run
    /// is marked so.
    pub fn push(&self, level: LogLevel, text: &str, dry_run: bool) -> bool {
        let now = self.clock.now();
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let elapsed = (now.0 - state.at.0).max(0.0);
        state.tokens = (state.tokens + elapsed * f64::from(LOG_LINES_PER_SECOND)).min(f64::from(LOG_BURST));
        state.at = Seconds(state.at.0.max(now.0));
        if state.tokens < 1.0 {
            state.dropped += 1;
            return false;
        }
        state.tokens -= 1.0;
        if state.dropped > 0 {
            let notice = format!("{} lines dropped", state.dropped);
            state.dropped = 0;
            self.keep(&mut state, now, LogLevel::Warn, notice, false);
        }
        self.keep(&mut state, now, level, cut_line(text).to_owned(), dry_run);
        true
    }

    fn keep(&self, state: &mut State, at: Seconds, level: LogLevel, text: String, dry_run: bool) {
        let line = LogLine { seq: state.next_seq, at, level, text, dry_run };
        state.next_seq += 1;
        if let Some(sink) = &self.sink {
            // The writer outlives every log in normal use; once it has stopped, the ring still keeps the line.
            let _gone = sink.send(Message::Line(self.id.clone(), line.clone()));
        }
        state.lines.push_back(line);
        if state.lines.len() > LOG_RING_LINES {
            state.lines.pop_front();
        }
    }

    /// Writes the "N lines dropped" line now, if lines were dropped: when the plugin stops, so a flood that ended its
    /// run still says how much it lost.
    pub fn flush(&self) {
        let now = self.clock.now();
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.dropped > 0 {
            let notice = format!("{} lines dropped", state.dropped);
            state.dropped = 0;
            self.keep(&mut state, now, LogLevel::Warn, notice, false);
        }
    }

    /// Up to `limit` lines with a sequence number above `after`, oldest first.
    pub fn page(&self, after: u64, limit: usize) -> Vec<LogLine> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.lines.iter().filter(|l| l.seq > after).take(limit).cloned().collect()
    }
}

/// What the writer thread receives.
enum Message {
    /// A kept line of a plugin's log.
    Line(PluginId, LogLine),
    /// Write what came before, then stop.
    Stop,
}

/// Where the log writer thread puts kept lines.
pub trait LineSink: Send {
    /// Writes one line of `plugin`'s log.
    ///
    /// # Errors
    /// The I/O error; the writer counts it in [`LogWriter::failures`] and goes on.
    fn write(&mut self, plugin: &PluginId, line: &LogLine) -> io::Result<()>;
}

/// The thread that writes every plugin's kept lines to a [`LineSink`]. Dropping it stops and joins the thread.
pub struct LogWriter {
    tx: Option<Sender<Message>>,
    thread: Option<JoinHandle<()>>,
    failures: Arc<AtomicU64>,
}

impl LogWriter {
    /// Starts the thread over `sink`.
    ///
    /// # Errors
    /// The OS refused to start a thread.
    pub fn spawn(sink: Box<dyn LineSink>) -> io::Result<Self> {
        let (tx, rx) = channel::<Message>();
        let failures = Arc::new(AtomicU64::new(0));
        let counted = failures.clone();
        let thread =
            std::thread::Builder::new().name("plugin-log".into()).spawn(move || write_all(sink, &rx, &counted))?;
        Ok(Self { tx: Some(tx), thread: Some(thread), failures })
    }

    /// Lines the sink failed to write so far.
    pub fn failures(&self) -> u64 {
        self.failures.load(Ordering::Acquire)
    }

    fn sender(&self) -> Option<Sender<Message>> {
        self.tx.clone()
    }
}

fn write_all(mut sink: Box<dyn LineSink>, rx: &Receiver<Message>, failures: &AtomicU64) {
    for message in rx {
        let Message::Line(plugin, line) = message else { break };
        if sink.write(&plugin, &line).is_err() {
            failures.fetch_add(1, Ordering::AcqRel);
        }
    }
}

impl Drop for LogWriter {
    fn drop(&mut self) {
        // Logs may outlive the writer and still hold senders, so stop explicitly: the lines queued before it are
        // written, later ones stay in their rings only.
        if let Some(tx) = self.tx.take() {
            let _stopped = tx.send(Message::Stop);
        }
        if let Some(thread) = self.thread.take() {
            let _joined = thread.join();
        }
    }
}

/// Appends lines to `<dir>/<id>.log`, rotating to `<id>.log.1` (replacing the old one) before a line would take the
/// file past [`LOG_FILE_MAX_BYTES`].
pub struct FileSink {
    dir: PathBuf,
}

impl FileSink {
    /// A sink writing into `dir` (created on first write).
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }
}

/// One line as the file holds it: sequence number, clock, level, the dry-run mark and the text.
fn file_line(line: &LogLine) -> String {
    let level = match line.level {
        LogLevel::Trace => "trace",
        LogLevel::Debug => "debug",
        LogLevel::Info => "info",
        LogLevel::Warn => "warn",
        LogLevel::Error => "error",
    };
    let dry = if line.dry_run { " (dry run)" } else { "" };
    format!("{} {:.3} {level}{dry} {}\n", line.seq, line.at.0, line.text.replace('\n', " "))
}

impl LineSink for FileSink {
    fn write(&mut self, plugin: &PluginId, line: &LogLine) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let path = self.dir.join(format!("{plugin}.log"));
        let text = file_line(line);
        let size = fs::metadata(&path).map_or(0, |m| m.len());
        if size > 0 && size + text.len() as u64 > LOG_FILE_MAX_BYTES {
            fs::rename(&path, self.dir.join(format!("{plugin}.log.1")))?;
        }
        OpenOptions::new().create(true).append(true).open(&path)?.write_all(text.as_bytes())
    }
}
