//! The pipe protocol between the registry and a model worker: frames of `[kind: u8][length: u32 LE][payload]`.
//!
//! - **Control** frames carry a JSON [`Control`] message: probing, loading, a native library the worker needs, and
//!   errors. They are cold, so their JSON may allocate.
//! - **Run**, **Ran** and **Reset** frames are the hot path, so they are binary and fixed: `Run` carries the number of
//!   input tensors that follow, `Ran` the number of output tensors that follow, `Reset` nothing.
//! - **Tensor** frames are `[dtype: u8][rank: u8][dims: u64 LE × rank][elements, little-endian]`.
//!
//! The worker first writes [`MAGIC`]; the registry skips whatever comes before it (a test binary standing in for the
//! worker prints its harness banner first), so the protocol never depends on a quiet start.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::path::PathBuf;

use omuzaki_ai_api::{
    DType, LoadError, MAX_RANK, ModelRef, Provider, RunError, RuntimeId, SessionOptions, Sha256, Signature, Stage,
    TensorBuf, TensorData, TensorDataMut, TensorRef,
};
use serde::{Deserialize, Serialize};

/// What a worker writes before anything else.
pub(crate) const MAGIC: &[u8; 8] = b"OMZWRK01";

/// The largest frame either side accepts: a 4K RGB frame in `f32` is about 100 MiB, and anything this big is a broken
/// or hostile peer, refused before it is allocated.
const MAX_FRAME: usize = 1 << 30;

/// What a frame holds.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A JSON [`Control`] message.
    Control = 1,
    /// One tensor.
    Tensor = 2,
    /// Registry to worker: run once on the input tensors that follow.
    Run = 3,
    /// Worker to registry: the run succeeded, its output tensors follow.
    Ran = 4,
    /// Registry to worker: reset the session; there is no answer.
    Reset = 5,
}

impl Kind {
    fn of(byte: u8) -> io::Result<Self> {
        Ok(match byte {
            1 => Self::Control,
            2 => Self::Tensor,
            3 => Self::Run,
            4 => Self::Ran,
            5 => Self::Reset,
            other => return Err(broken(format!("unknown frame kind {other}"))),
        })
    }
}

/// A protocol error: the peer sent something this side can't read.
pub(crate) fn broken(why: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why.into())
}

/// Writes one frame. The caller flushes.
pub(crate) fn write_frame(w: &mut impl Write, kind: Kind, payload: &[u8]) -> io::Result<()> {
    let len = u32::try_from(payload.len()).ok().filter(|&n| n as usize <= MAX_FRAME);
    let len = len.ok_or_else(|| broken(format!("a frame of {} bytes is over the limit", payload.len())))?;
    let mut header = [0u8; 5];
    header[0] = kind as u8;
    header[1..].copy_from_slice(&len.to_le_bytes());
    w.write_all(&header)?;
    w.write_all(payload)
}

/// Reads one frame into `buf` (grown only when a frame is larger than any before) and returns its kind.
/// `Ok(None)` at a clean end of the stream, before a frame starts.
pub(crate) fn read_frame(r: &mut impl Read, buf: &mut Vec<u8>) -> io::Result<Option<Kind>> {
    let mut header = [0u8; 5];
    let mut got = 0;
    while got < header.len() {
        match r.read(&mut header[got..]) {
            Ok(0) if got == 0 => return Ok(None),
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    let kind = Kind::of(header[0])?;
    let len = u32::from_le_bytes([header[1], header[2], header[3], header[4]]) as usize;
    if len > MAX_FRAME {
        return Err(broken(format!("a frame of {len} bytes is over the limit")));
    }
    buf.resize(len, 0);
    r.read_exact(buf)?;
    Ok(Some(kind))
}

/// Writes a [`Control`] frame.
pub(crate) fn write_control(w: &mut impl Write, message: &Control) -> io::Result<()> {
    let json = serde_json::to_vec(message).map_err(|e| broken(e.to_string()))?;
    write_frame(w, Kind::Control, &json)
}

/// Reads a [`Control`] from a frame's payload.
pub(crate) fn control(payload: &[u8]) -> io::Result<Control> {
    serde_json::from_slice(payload).map_err(|e| broken(format!("a control message that doesn't parse: {e}")))
}

/// A count frame's payload (`Run`, `Ran`).
pub(crate) fn count(n: usize) -> [u8; 4] {
    u32::try_from(n).unwrap_or(u32::MAX).to_le_bytes()
}

/// Reads a count frame's payload.
pub(crate) fn read_count(payload: &[u8]) -> io::Result<usize> {
    let bytes: [u8; 4] = payload.try_into().map_err(|_| broken("a count frame that isn't 4 bytes"))?;
    Ok(u32::from_le_bytes(bytes) as usize)
}

/// The model a worker probes or loads: its runtime and its verified files, by the manifest's names.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Job {
    pub model: ModelRef,
    pub runtime: RuntimeId,
    /// `(manifest name, verified path)`, main file first.
    pub files: Vec<(String, PathBuf)>,
    pub stages: Vec<Stage>,
}

/// The cold messages, both ways.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub(crate) enum Control {
    /// Registry to worker: read the model's signature without running it.
    Probe { job: Job },
    /// Registry to worker: load a session presenting `slot`; the worker holds one session at a time.
    Load { job: Job, slot: Signature, io: BTreeMap<String, String>, options: SessionOptions },
    /// Registry to worker: the answer to [`Control::Fetch`].
    Fetched { result: Result<PathBuf, String> },
    /// Worker to registry: what [`Control::Probe`] found.
    Probed { signature: Signature, opset: Option<u32> },
    /// Worker to registry: the session is loaded and runs on `provider`.
    Loaded { provider: Provider },
    /// Worker to registry: probing or loading failed.
    Failed { error: WireLoadError },
    /// Worker to registry, while probing or loading: the runtime needs this pinned native library. The registry
    /// fetches it through its own verified cache, so the worker needs no network.
    Fetch { urls: Vec<String>, sha256: Sha256, size: u64 },
    /// Worker to registry: the run produced nothing.
    RunFailed { error: WireRunError },
}

/// A [`LoadError`] on the wire.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "message", rename_all = "snake_case")]
pub(crate) enum WireLoadError {
    MissingFile(String),
    Format(String),
    Refused(String),
    Runtime(String),
}

impl From<&LoadError> for WireLoadError {
    fn from(e: &LoadError) -> Self {
        match e {
            LoadError::MissingFile(name) => Self::MissingFile(name.clone()),
            LoadError::Format(why) => Self::Format(why.clone()),
            LoadError::Refused(why) => Self::Refused(why.clone()),
            LoadError::Runtime(why) => Self::Runtime(why.clone()),
            // The library error's words survive; its type stays on the worker's side.
            other => Self::Runtime(other.to_string()),
        }
    }
}

impl From<WireLoadError> for LoadError {
    fn from(e: WireLoadError) -> Self {
        match e {
            WireLoadError::MissingFile(name) => LoadError::MissingFile(name),
            WireLoadError::Format(why) => LoadError::Format(why),
            WireLoadError::Refused(why) => LoadError::Refused(why),
            WireLoadError::Runtime(why) => LoadError::Runtime(why),
        }
    }
}

/// A [`RunError`] on the wire. A mismatch the registry's own checks let through is the worker's failure.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "message", rename_all = "snake_case")]
pub(crate) enum WireRunError {
    Unavailable,
    Crashed,
    Failed(String),
}

impl From<&RunError> for WireRunError {
    fn from(e: &RunError) -> Self {
        match e {
            RunError::Unavailable => Self::Unavailable,
            RunError::Crashed => Self::Crashed,
            RunError::Failed(why) => Self::Failed(why.clone()),
            other => Self::Failed(other.to_string()),
        }
    }
}

impl From<WireRunError> for RunError {
    fn from(e: WireRunError) -> Self {
        match e {
            WireRunError::Unavailable => RunError::Unavailable,
            WireRunError::Crashed => RunError::Crashed,
            WireRunError::Failed(why) => RunError::Failed(why),
        }
    }
}

fn dtype_code(dtype: DType) -> io::Result<u8> {
    Ok(match dtype {
        DType::F32 => 0,
        DType::F16 => 1,
        DType::I64 => 2,
        DType::I32 => 3,
        DType::U8 => 4,
        other => return Err(broken(format!("the sandbox can't carry {other:?} tensors"))),
    })
}

fn dtype_of(code: u8) -> io::Result<DType> {
    Ok(match code {
        0 => DType::F32,
        1 => DType::F16,
        2 => DType::I64,
        3 => DType::I32,
        4 => DType::U8,
        other => return Err(broken(format!("unknown tensor element type {other}"))),
    })
}

/// Encodes `tensor` into `out`, which is cleared first and grows only past its largest size so far.
pub(crate) fn encode_tensor(tensor: &TensorRef<'_>, out: &mut Vec<u8>) -> io::Result<()> {
    out.clear();
    out.push(dtype_code(tensor.dtype())?);
    out.push(u8::try_from(tensor.shape.len()).map_err(|_| broken("a tensor of too many dimensions"))?);
    for &d in tensor.shape {
        out.extend_from_slice(&(d as u64).to_le_bytes());
    }
    match tensor.data {
        TensorData::F32(d) => put(out, d, f32::to_le_bytes),
        TensorData::F16(d) => put(out, d, u16::to_le_bytes),
        TensorData::I64(d) => put(out, d, i64::to_le_bytes),
        TensorData::I32(d) => put(out, d, i32::to_le_bytes),
        TensorData::U8(d) => out.extend_from_slice(d),
    }
    Ok(())
}

/// Appends `data` as little-endian words: sized once, then filled in place, which the compiler turns into a copy on
/// little-endian machines.
fn put<T: Copy, const N: usize>(out: &mut Vec<u8>, data: &[T], bytes: fn(T) -> [u8; N]) {
    let start = out.len();
    out.resize(start + data.len() * N, 0);
    for (dst, v) in out[start..].as_chunks_mut::<N>().0.iter_mut().zip(data) {
        *dst = bytes(*v);
    }
}

/// Fills `dst` from little-endian words.
fn take<T, const N: usize>(dst: &mut [T], data: &[u8], value: fn([u8; N]) -> T) {
    for (v, word) in dst.iter_mut().zip(data.as_chunks::<N>().0) {
        *v = value(*word);
    }
}

/// The size in bytes of one element.
fn element_size(dtype: DType) -> usize {
    match dtype {
        DType::F16 => 2,
        DType::I64 => 8,
        DType::U8 => 1,
        _ => 4,
    }
}

/// Decodes a tensor frame into `buf`, whose element type it must have. Allocates only when `buf` grows; a frame whose
/// length disagrees with its shape is refused before `buf` changes.
pub(crate) fn decode_tensor(payload: &[u8], buf: &mut TensorBuf) -> io::Result<()> {
    let [code, rank, rest @ ..] = payload else { return Err(broken("a tensor frame without its header")) };
    let dtype = dtype_of(*code)?;
    if dtype != buf.dtype() {
        return Err(broken(format!("a {dtype:?} tensor where {:?} was expected", buf.dtype())));
    }
    let rank = usize::from(*rank);
    if rank > MAX_RANK || rest.len() < rank * 8 {
        return Err(broken("a tensor frame with a broken shape"));
    }
    let mut shape = [0usize; MAX_RANK];
    for (i, dim) in rest[..rank * 8].as_chunks::<8>().0.iter().enumerate() {
        shape[i] = usize::try_from(u64::from_le_bytes(*dim)).map_err(|_| broken("a dimension too large here"))?;
    }
    let data = &rest[rank * 8..];
    let elements = shape[..rank].iter().try_fold(1usize, |n, &d| n.checked_mul(d));
    if elements.and_then(|n| n.checked_mul(element_size(dtype))) != Some(data.len()) {
        return Err(broken("a tensor whose data doesn't fill its shape"));
    }
    match buf.resize(&shape[..rank]).map_err(|p| broken(p.to_string()))? {
        TensorDataMut::F32(d) => take(d, data, f32::from_le_bytes),
        TensorDataMut::F16(d) => take(d, data, u16::from_le_bytes),
        TensorDataMut::I64(d) => take(d, data, i64::from_le_bytes),
        TensorDataMut::I32(d) => take(d, data, i32::from_le_bytes),
        TensorDataMut::U8(d) => d.copy_from_slice(data),
    }
    Ok(())
}

/// The element type a tensor frame carries, for a side that keeps no buffer of it yet.
pub(crate) fn frame_dtype(payload: &[u8]) -> io::Result<DType> {
    dtype_of(*payload.first().ok_or_else(|| broken("an empty tensor frame"))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tensors_round_trip_bit_for_bit() {
        let mut wire = Vec::new();
        let data = [0.5f32, -0.0, f32::MAX, f32::MIN_POSITIVE, 3.25, 7.0];
        encode_tensor(&TensorRef::f32(&data, &[2, 3]), &mut wire).unwrap();
        let mut buf = TensorBuf::new(DType::F32);
        decode_tensor(&wire, &mut buf).unwrap();
        assert_eq!((buf.shape(), buf.as_f32().unwrap()), ([2, 3].as_slice(), data.as_slice()));
        let ints = [i64::MIN, 0, i64::MAX];
        encode_tensor(&TensorRef::i64(&ints, &[3]), &mut wire).unwrap();
        let mut buf = TensorBuf::new(DType::I64);
        decode_tensor(&wire, &mut buf).unwrap();
        assert_eq!(buf.data(), TensorData::I64(&ints));
    }

    #[test]
    fn a_frame_that_lies_about_its_tensor_is_refused() {
        let mut wire = Vec::new();
        encode_tensor(&TensorRef::f32(&[1.0, 2.0], &[2]), &mut wire).unwrap();
        let mut wrong_type = TensorBuf::new(DType::U8);
        assert!(decode_tensor(&wire, &mut wrong_type).is_err());
        wire.pop();
        assert!(decode_tensor(&wire, &mut TensorBuf::new(DType::F32)).is_err(), "short data");
        assert!(decode_tensor(&[0, 9], &mut TensorBuf::new(DType::F32)).is_err(), "rank over the limit");
    }

    #[test]
    fn frames_round_trip_and_a_huge_length_is_refused_before_it_is_read() {
        let mut pipe = Vec::new();
        write_frame(&mut pipe, Kind::Run, &count(3)).unwrap();
        write_control(&mut pipe, &Control::Loaded { provider: Provider::Cpu }).unwrap();
        let mut reader = pipe.as_slice();
        let mut buf = Vec::new();
        assert_eq!(read_frame(&mut reader, &mut buf).unwrap(), Some(Kind::Run));
        assert_eq!(read_count(&buf).unwrap(), 3);
        assert_eq!(read_frame(&mut reader, &mut buf).unwrap(), Some(Kind::Control));
        assert_eq!(control(&buf).unwrap(), Control::Loaded { provider: Provider::Cpu });
        assert_eq!(read_frame(&mut reader, &mut buf).unwrap(), None, "a clean end");
        let huge = [Kind::Tensor as u8, 0xff, 0xff, 0xff, 0xff];
        assert!(read_frame(&mut huge.as_slice(), &mut buf).is_err());
        assert!(read_frame(&mut [Kind::Run as u8, 4].as_slice(), &mut buf).is_err(), "cut off in the header");
    }
}
