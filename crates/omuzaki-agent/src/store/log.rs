//! The `log.jsonl` format (D7): a header line, then one timestamped entry per line.
//!
//! Every entry line is `{"at": "<rfc3339>", "kind": ..., ...}`: the [`Entry`] flattened beside its time, so retention
//! can age a conversation by its last entry rather than by the file's modification time (which a backup restore or a
//! copy would reset).

use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use omuzaki_agent_api::entry::Entry;
use omuzaki_agent_api::error::HostError;
use serde::{Deserialize, Serialize};

use super::io_err;

/// The log format this build writes and reads.
const FORMAT: u32 = 1;

/// The first line of a log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Header {
    /// Always [`FORMAT`]. Declared first so it serialises first.
    pub format: u32,
    /// The project the conversation belongs to.
    pub project: String,
    /// When it was created, RFC 3339.
    pub created: String,
    /// The model it started with.
    pub model: String,
}

impl Header {
    /// A header for a conversation created now.
    pub fn new(project: &str, created: &str, model: &str) -> Self {
        Self { format: FORMAT, project: project.into(), created: created.into(), model: model.into() }
    }
}

/// An entry and the time it was written.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Stamped {
    /// RFC 3339.
    pub at: String,
    /// The fact.
    pub entry: Entry,
}

/// A log read back: its header and every entry that survived.
#[derive(Debug)]
pub(super) struct Parsed {
    pub header: Header,
    pub entries: Vec<Stamped>,
}

impl Parsed {
    /// The entries without their times.
    pub fn into_entries(self) -> Vec<Entry> {
        self.entries.into_iter().map(|s| s.entry).collect()
    }

    /// When the conversation last changed: its last entry, or its creation if it has none.
    pub fn last_time(&self) -> &str {
        self.entries.last().map_or(self.header.created.as_str(), |s| s.at.as_str())
    }
}

/// How a line is written. Borrowed, so appending never clones the entry.
#[derive(Serialize)]
struct LineOut<'a> {
    at: &'a str,
    #[serde(flatten)]
    entry: &'a Entry,
}

/// How a line is read back.
#[derive(Deserialize)]
struct LineIn {
    at: String,
    #[serde(flatten)]
    entry: Entry,
}

/// The header as one line, newline included.
pub(super) fn header_line(header: &Header) -> Result<String, HostError> {
    let mut line = serde_json::to_string(header).map_err(|e| io_err("encode the log header", e))?;
    line.push('\n');
    Ok(line)
}

/// One entry as one line, newline included.
pub(super) fn entry_line(at: &str, entry: &Entry) -> Result<String, HostError> {
    let mut line = serde_json::to_string(&LineOut { at, entry }).map_err(|e| io_err("encode a log entry", e))?;
    line.push('\n');
    Ok(line)
}

/// Parses a whole log. A last line that does not parse is a torn write and is dropped; any other bad line is an
/// error naming its (1-based) number.
pub(super) fn parse(text: &str) -> Result<Parsed, HostError> {
    let mut lines = text.lines().enumerate();
    let header = match lines.next() {
        Some((_, first)) => serde_json::from_str::<Header>(first).map_err(|e| io_err("read the log header", e))?,
        None => return Err(HostError::Store("the log is empty".into())),
    };
    if header.format != FORMAT {
        return Err(HostError::Store(format!("unsupported log format {}", header.format)));
    }
    let count = text.lines().count();
    let mut entries = Vec::new();
    for (index, line) in lines {
        match serde_json::from_str::<LineIn>(line) {
            Ok(LineIn { at, entry }) => entries.push(Stamped { at, entry }),
            Err(_) if index + 1 == count => {} // a crash mid-write
            Err(e) => return Err(io_err(&format!("read log line {}", index + 1), e)),
        }
    }
    Ok(Parsed { header, entries })
}

/// Creates a log holding exactly `header_line`; fails if it already exists.
pub(super) fn create(path: &Path, header_line: &str) -> Result<(), HostError> {
    let mut file =
        OpenOptions::new().write(true).create_new(true).open(path).map_err(|e| io_err("create the log", e))?;
    file.write_all(header_line.as_bytes()).and_then(|()| file.flush()).map_err(|e| io_err("write the log header", e))
}

/// Appends `line` (one entry, newline included) in one write and flushes it. The log must exist.
pub(super) fn append(path: &Path, line: &str) -> Result<(), HostError> {
    let mut file = OpenOptions::new().read(true).write(true).open(path).map_err(|e| io_err("open the log", e))?;
    heal_tail(&mut file).map_err(|e| io_err("repair the log tail", e))?;
    file.seek(SeekFrom::End(0)).map_err(|e| io_err("seek the log", e))?;
    file.write_all(line.as_bytes()).and_then(|()| file.flush()).map_err(|e| io_err("append to the log", e))
}

/// After a crash the file can end mid-line. Appending to it would glue the new entry to the torn text and turn a
/// droppable last line into a corrupt middle one, so the tail is first completed (if it is a whole entry that only
/// lacks its newline) or cut off (if it is torn).
fn heal_tail(file: &mut std::fs::File) -> std::io::Result<()> {
    let len = file.metadata()?.len();
    if len == 0 {
        return Ok(());
    }
    file.seek(SeekFrom::End(-1))?;
    let mut last = [0u8; 1];
    file.read_exact(&mut last)?;
    if last[0] == b'\n' {
        return Ok(());
    }
    let mut bytes = Vec::new();
    file.seek(SeekFrom::Start(0))?;
    file.read_to_end(&mut bytes)?;
    let start = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |p| p + 1);
    if start == 0 {
        return Ok(()); // only a header, torn or not: nothing sensible to cut; reading reports it
    }
    let whole = std::str::from_utf8(&bytes[start..]).is_ok_and(|tail| serde_json::from_str::<LineIn>(tail).is_ok());
    if whole {
        file.seek(SeekFrom::End(0))?;
        file.write_all(b"\n")
    } else {
        file.set_len(start as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_header_has_the_documented_key_order() {
        let line = header_line(&Header::new("ab12", "2026-10-14T10:00:00Z", "m")).unwrap();
        assert_eq!(line, "{\"format\":1,\"project\":\"ab12\",\"created\":\"2026-10-14T10:00:00Z\",\"model\":\"m\"}\n");
    }

    #[test]
    fn an_entry_line_carries_its_time_first() {
        let line = entry_line("2026-10-14T10:00:01Z", &Entry::Stopped).unwrap();
        assert_eq!(line, "{\"at\":\"2026-10-14T10:00:01Z\",\"kind\":\"stopped\"}\n");
    }

    #[test]
    fn a_torn_last_line_goes_and_a_torn_middle_line_is_an_error() {
        let head = header_line(&Header::new("p", "2026-10-14T10:00:00Z", "m")).unwrap();
        let good = entry_line("2026-10-14T10:00:01Z", &Entry::Stopped).unwrap();
        let torn = parse(&format!("{head}{good}{{\"at\":\"2026-10-14T10:0")).unwrap();
        assert_eq!(torn.entries.len(), 1);
        let err = parse(&format!("{head}{{\"at\":\"x\n{good}")).unwrap_err();
        assert!(matches!(err, HostError::Store(m) if m.contains("line 2")));
    }
}
