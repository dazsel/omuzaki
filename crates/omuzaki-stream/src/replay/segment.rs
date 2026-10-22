//! The disk segment store behind a disk-backed replay buffer (`docs/design/record.md` D4).
//!
//! One segment file per keyframe group (about 1 s), appended as the group's chunks arrive. A chunk on disk is a
//! [`Slice`] of its group's file; the file is deleted, and its bytes given back to the meter, when the last slice
//! drops, so a save in progress keeps the files it reads alive after the ring evicted them. The meter counts the full
//! serialized record ([`SegmentStore::record_len`]), the same number the ring admitted, so the cap holds for file bytes.
//!
//! The folder is a cache, never a recovery source: it is a recording of the person nobody asked to keep. [`open`]
//! removes every `omuzaki-replay-*.seg` and `*.tmp` a crash left, and nothing else. File names never contain caller
//! data, and files are created with the platform's default private permissions.
//!
//! A record is the stream (one byte), `pts`, `dts` and `duration` (little-endian `i64`), the keyframe flag (one byte),
//! the side-data count (`u32`), per side datum its `BlockAdditional` id (`u64`), length (`u32`) and bytes, then the
//! data length (`u32`) and the data.
//!
//! [`open`]: SegmentStore::open

use std::fs::{File, OpenOptions};
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use bytes::Bytes;
use omuzaki_stream_api::error::StreamError;
use omuzaki_stream_api::packet::{Packet, SideData, SideKind, StreamId};

use super::ring::{Charge, Payload};

/// The start of every segment file's name.
const PREFIX: &str = "omuzaki-replay-";
/// Bytes of a record before its side data: stream, `pts`, `dts`, `duration`, keyframe flag, side-data count.
const HEAD: u64 = 1 + 8 + 8 + 8 + 1 + 4;
/// Bytes of a side datum before its bytes: id and length.
const SIDE_HEAD: u64 = 8 + 4;
/// Bytes of the data length.
const DATA_HEAD: u64 = 4;

fn io(path: &Path, e: &std::io::Error) -> StreamError {
    StreamError::Io(format!("{}: {e}", path.display()))
}

/// One segment file: deleted, and its charge given back, when the last slice of it drops.
#[derive(Debug)]
struct GroupFile {
    path: PathBuf,
    charge: Mutex<Option<Charge>>,
}

impl Drop for GroupFile {
    fn drop(&mut self) {
        // A cache file: if it is already gone there is nothing to do, and `open` sweeps anything left behind.
        #[allow(clippy::disallowed_methods, reason = "the replay cache's own file, not an op's file access (D4)")]
        let _ = std::fs::remove_file(&self.path);
    }
}

/// A chunk on disk: a range of its group's segment file. Cloning shares it.
#[derive(Clone, Debug)]
pub struct Slice {
    file: Arc<GroupFile>,
    offset: u64,
    len: u64,
}

impl Slice {
    /// The segment file this slice is in.
    pub fn path(&self) -> &Path {
        &self.file.path
    }

    /// Reads the packet back.
    ///
    /// # Errors
    /// [`StreamError::Io`] when the file can't be read, or the record is damaged.
    pub fn read(&self) -> Result<Packet, StreamError> {
        let path = &self.file.path;
        #[allow(clippy::disallowed_methods, reason = "the replay cache's own file, not an op's file access (D4)")]
        let mut file = File::open(path).map_err(|e| io(path, &e))?;
        file.seek(SeekFrom::Start(self.offset)).map_err(|e| io(path, &e))?;
        let mut record = vec![0u8; usize::try_from(self.len).unwrap_or(usize::MAX)];
        file.read_exact(&mut record).map_err(|e| io(path, &e))?;
        decode(&record).ok_or_else(|| StreamError::Io(format!("{}: a damaged replay record", path.display())))
    }
}

impl Payload for Slice {
    fn len(&self) -> u64 {
        self.len
    }
}

/// The group being written.
struct Open {
    group: Arc<GroupFile>,
    file: File,
    written: u64,
}

/// The segment store. Writes happen on the caller's thread (the encoder thread, never the audio thread).
pub struct SegmentStore {
    dir: PathBuf,
    next: AtomicU64,
    open: Mutex<Option<Open>>,
}

impl std::fmt::Debug for SegmentStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SegmentStore").field("dir", &self.dir).finish_non_exhaustive()
    }
}

/// Whether `name` is one of this store's files, or a temporary one a crash left.
fn ours(name: &str) -> bool {
    let extension = |wanted: &str| Path::new(name).extension().is_some_and(|e| e.eq_ignore_ascii_case(wanted));
    (name.starts_with(PREFIX) && extension("seg")) || extension("tmp")
}

impl SegmentStore {
    /// Opens the store in `dir`, creating it, and removes every segment and temporary file a crash left there.
    ///
    /// # Errors
    /// [`StreamError::Io`] when the folder can't be made or read.
    #[allow(clippy::disallowed_methods, reason = "the replay cache folder, not an op's file access (D4)")]
    pub fn open(dir: &Path) -> Result<Self, StreamError> {
        std::fs::create_dir_all(dir).map_err(|e| io(dir, &e))?;
        for entry in std::fs::read_dir(dir).map_err(|e| io(dir, &e))? {
            let entry = entry.map_err(|e| io(dir, &e))?;
            let is_file = entry.file_type().is_ok_and(|t| t.is_file());
            if is_file && entry.file_name().to_str().is_some_and(ours) {
                let path = entry.path();
                std::fs::remove_file(&path).map_err(|e| io(&path, &e))?;
            }
        }
        Ok(Self { dir: dir.to_owned(), next: AtomicU64::new(0), open: Mutex::new(None) })
    }

    /// The folder.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The bytes `packet` occupies on disk: its whole record.
    pub fn record_len(packet: &Packet) -> u64 {
        let side: u64 = packet.side_data.iter().map(|s| SIDE_HEAD + s.data.len() as u64).sum();
        HEAD + side + DATA_HEAD + packet.data.len() as u64
    }

    /// Appends `packet` under `charge` (sized by [`record_len`](Self::record_len)), starting a new segment file when
    /// `new_group` is set or none is open.
    ///
    /// # Errors
    /// [`StreamError::Io`] when the file can't be made or written; the charge is given back then.
    pub fn put(&self, packet: &Packet, charge: Charge, new_group: bool) -> Result<Slice, StreamError> {
        let record = encode(packet)?;
        let mut open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        if new_group || open.is_none() {
            *open = Some(self.start_group()?);
        }
        let Some(current) = open.as_mut() else { return Err(StreamError::Stopped) };
        let path = current.group.path.clone();
        if let Err(e) = current.file.write_all(&record) {
            // The file may now end in half a record; start a fresh one for the next chunk.
            *open = None;
            return Err(io(&path, &e));
        }
        let slice = Slice { file: Arc::clone(&current.group), offset: current.written, len: record.len() as u64 };
        current.written += slice.len;
        let mut held = current.group.charge.lock().unwrap_or_else(PoisonError::into_inner);
        match held.as_mut() {
            Some(total) => total.absorb(charge),
            None => *held = Some(charge),
        }
        drop(held);
        Ok(slice)
    }

    fn start_group(&self) -> Result<Open, StreamError> {
        let n = self.next.fetch_add(1, Ordering::Relaxed);
        let path = self.dir.join(format!("{PREFIX}{n}.seg"));
        #[allow(clippy::disallowed_methods, reason = "the replay cache's own file, not an op's file access (D4)")]
        let file = OpenOptions::new().write(true).create_new(true).open(&path).map_err(|e| io(&path, &e))?;
        Ok(Open { group: Arc::new(GroupFile { path, charge: Mutex::new(None) }), file, written: 0 })
    }

    /// Closes the segment being written; the next chunk starts a new file.
    pub fn close(&self) {
        *self.open.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

fn len32(n: usize) -> Result<[u8; 4], StreamError> {
    u32::try_from(n).map(u32::to_le_bytes).map_err(|_| StreamError::Io("a replay chunk over 4 GiB".into()))
}

fn encode(packet: &Packet) -> Result<Vec<u8>, StreamError> {
    let mut out = Vec::with_capacity(usize::try_from(SegmentStore::record_len(packet)).unwrap_or(0));
    out.push(packet.stream.0);
    out.extend_from_slice(&packet.pts.to_le_bytes());
    out.extend_from_slice(&packet.dts.to_le_bytes());
    out.extend_from_slice(&packet.duration.to_le_bytes());
    out.push(u8::from(packet.keyframe));
    out.extend_from_slice(&len32(packet.side_data.len())?);
    for side in &packet.side_data {
        let SideKind::BlockAdditional { id } = side.kind else {
            return Err(StreamError::Io("a side datum the replay buffer can't store".into()));
        };
        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&len32(side.data.len())?);
        out.extend_from_slice(&side.data);
    }
    out.extend_from_slice(&len32(packet.data.len())?);
    out.extend_from_slice(&packet.data);
    Ok(out)
}

/// Reads fixed-size fields off the front of a record.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (head, rest) = self.0.split_at_checked(n)?;
        self.0 = rest;
        Some(head)
    }

    fn array<const N: usize>(&mut self) -> Option<[u8; N]> {
        self.take(N)?.try_into().ok()
    }

    fn len(&mut self) -> Option<usize> {
        usize::try_from(u32::from_le_bytes(self.array()?)).ok()
    }
}

fn decode(record: &[u8]) -> Option<Packet> {
    let mut r = Reader(record);
    let [stream] = r.array()?;
    let pts = i64::from_le_bytes(r.array()?);
    let dts = i64::from_le_bytes(r.array()?);
    let duration = i64::from_le_bytes(r.array()?);
    let [key] = r.array()?;
    let sides = r.len()?;
    let mut side_data = Vec::with_capacity(sides.min(16));
    for _ in 0..sides {
        let id = u64::from_le_bytes(r.array()?);
        let n = r.len()?;
        side_data.push(SideData { kind: SideKind::BlockAdditional { id }, data: Bytes::copy_from_slice(r.take(n)?) });
    }
    let n = r.len()?;
    let data = Bytes::copy_from_slice(r.take(n)?);
    r.0.is_empty().then_some(Packet {
        stream: StreamId(stream),
        pts,
        dts,
        duration,
        keyframe: key == 1,
        data,
        side_data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_round_trips_with_its_side_data_and_its_length_is_exact() {
        let packet = Packet {
            stream: StreamId(2),
            pts: -3,
            dts: 7,
            duration: 1,
            keyframe: true,
            data: Bytes::from_static(b"colour"),
            side_data: vec![SideData { kind: SideKind::BlockAdditional { id: 1 }, data: Bytes::from_static(b"alpha") }],
        };
        let record = encode(&packet).ok();
        assert_eq!(record.as_ref().map(|r| r.len() as u64), Some(SegmentStore::record_len(&packet)));
        assert_eq!(record.as_deref().and_then(decode), Some(packet));
        assert_eq!(decode(b"short"), None);
    }
}
