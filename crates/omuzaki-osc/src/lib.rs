//! An OSC 1.0 packet reader and writer (the transport VMC rides on, and OSC bindings, #469), borrowing from the
//! datagram and allocating nothing.
//!
//! A packet is a message or a bundle. A message is an address (`/VMC/Ext/Blend/Val`), a type-tag string (`,sf`) and
//! the arguments, each padded to four bytes and big-endian. A bundle is `#bundle`, an eight-byte time tag and elements
//! that are each a length and a packet, nested to any depth; [`for_each_message`] walks them to [`MAX_DEPTH`].

/// How deep bundles may nest before a packet is refused, so a hostile packet cannot recurse without bound.
pub const MAX_DEPTH: usize = 8;

/// Why a packet was refused.
#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum OscError {
    /// The packet ends inside a field.
    #[error("the OSC packet is truncated")]
    Truncated,
    /// A string is not UTF-8 or has no terminating zero.
    #[error("an OSC string is malformed")]
    BadString,
    /// The type tags do not start with a comma, or name a type this reader does not know.
    #[error("unknown OSC type tag {0:?}")]
    BadTag(char),
    /// Bundles nest deeper than [`MAX_DEPTH`].
    #[error("OSC bundles nest deeper than MAX_DEPTH ({MAX_DEPTH})")]
    TooDeep,
}

/// One argument of a message.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Arg<'a> {
    /// `i`: a 32-bit integer.
    Int(i32),
    /// `f`: a 32-bit float.
    Float(f32),
    /// `s` or `S`: a string.
    Str(&'a str),
    /// `b`: a blob.
    Blob(&'a [u8]),
    /// `h`: a 64-bit integer.
    Long(i64),
    /// `d`: a 64-bit float.
    Double(f64),
    /// `T` or `F`.
    Bool(bool),
    /// `N`, `I`, or a `t` time tag: carries no value used here.
    Nil,
}

impl Arg<'_> {
    /// The argument as an `f32`, for the numeric kinds.
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // senders put floats in doubles and ints
    pub fn as_f32(self) -> Option<f32> {
        match self {
            Self::Float(v) => Some(v),
            Self::Double(v) => Some(v as f32),
            Self::Int(v) => Some(v as f32),
            Self::Long(v) => Some(v as f32),
            _ => None,
        }
    }
}

/// One message: its address and its arguments, read lazily by [`Message::args`].
#[derive(Copy, Clone, Debug)]
pub struct Message<'a> {
    /// The address pattern, e.g. `/VMC/Ext/Blend/Val`.
    pub address: &'a str,
    tags: &'a [u8],
    data: &'a [u8],
}

impl<'a> Message<'a> {
    /// The arguments in order. A malformed argument ends the iteration with an error.
    pub fn args(&self) -> Args<'a> {
        Args { tags: self.tags, data: self.data }
    }

    /// The first `N` arguments, or an error when the message has fewer or one is malformed.
    ///
    /// # Errors
    /// [`OscError::Truncated`] when there are fewer than `N`, or the argument's own error.
    pub fn first<const N: usize>(&self) -> Result<[Arg<'a>; N], OscError> {
        let mut out = [Arg::Nil; N];
        let mut args = self.args();
        for slot in &mut out {
            *slot = args.next().ok_or(OscError::Truncated)??;
        }
        Ok(out)
    }
}

/// The arguments of a [`Message`].
#[derive(Clone, Debug)]
pub struct Args<'a> {
    tags: &'a [u8],
    data: &'a [u8],
}

impl<'a> Iterator for Args<'a> {
    type Item = Result<Arg<'a>, OscError>;

    fn next(&mut self) -> Option<Self::Item> {
        let (&tag, rest) = self.tags.split_first()?;
        self.tags = rest;
        Some(self.read(tag))
    }
}

impl<'a> Args<'a> {
    fn read(&mut self, tag: u8) -> Result<Arg<'a>, OscError> {
        Ok(match tag {
            b'i' => Arg::Int(i32::from_be_bytes(self.take::<4>()?)),
            b'f' => Arg::Float(f32::from_be_bytes(self.take::<4>()?)),
            b'h' => Arg::Long(i64::from_be_bytes(self.take::<8>()?)),
            b'd' => Arg::Double(f64::from_be_bytes(self.take::<8>()?)),
            b't' => {
                self.take::<8>()?;
                Arg::Nil
            }
            b's' | b'S' => {
                let (s, rest) = read_str(self.data)?;
                self.data = rest;
                Arg::Str(s)
            }
            b'b' => {
                let len = usize::try_from(u32::from_be_bytes(self.take::<4>()?)).map_err(|_| OscError::Truncated)?;
                let padded = pad4(len);
                if self.data.len() < padded {
                    return Err(OscError::Truncated);
                }
                let (blob, rest) = self.data.split_at(padded);
                self.data = rest;
                Arg::Blob(&blob[..len])
            }
            b'T' => Arg::Bool(true),
            b'F' => Arg::Bool(false),
            b'N' | b'I' => Arg::Nil,
            other => return Err(OscError::BadTag(char::from(other))),
        })
    }

    fn take<const N: usize>(&mut self) -> Result<[u8; N], OscError> {
        let (head, rest) = self.data.split_first_chunk::<N>().ok_or(OscError::Truncated)?;
        self.data = rest;
        Ok(*head)
    }
}

/// `n` rounded up to a multiple of four.
const fn pad4(n: usize) -> usize {
    (n + 3) & !3
}

/// A zero-terminated, four-byte-padded string at the start of `data`, and what follows it.
fn read_str(data: &[u8]) -> Result<(&str, &[u8]), OscError> {
    let end = data.iter().position(|&b| b == 0).ok_or(OscError::BadString)?;
    let s = std::str::from_utf8(&data[..end]).map_err(|_| OscError::BadString)?;
    let next = pad4(end + 1);
    if data.len() < next {
        return Err(OscError::Truncated);
    }
    Ok((s, &data[next..]))
}

/// The bundle marker, with its terminating zero.
const BUNDLE: &[u8; 8] = b"#bundle\0";

/// Calls `f` for every message in `packet`, depth first, in order.
///
/// # Errors
/// The first malformation found; the messages before it have been passed to `f`.
pub fn for_each_message<'a>(packet: &'a [u8], f: &mut impl FnMut(Message<'a>)) -> Result<(), OscError> {
    walk(packet, f, 0)
}

fn walk<'a>(packet: &'a [u8], f: &mut impl FnMut(Message<'a>), depth: usize) -> Result<(), OscError> {
    if let Some(mut rest) = packet.strip_prefix(BUNDLE) {
        if depth >= MAX_DEPTH {
            return Err(OscError::TooDeep);
        }
        rest = rest.get(8..).ok_or(OscError::Truncated)?; // the time tag: VMC senders do not rely on it
        while let Some((len, tail)) = rest.split_first_chunk::<4>() {
            let len = usize::try_from(u32::from_be_bytes(*len)).map_err(|_| OscError::Truncated)?;
            if tail.len() < len {
                return Err(OscError::Truncated);
            }
            let (element, after) = tail.split_at(len);
            walk(element, f, depth + 1)?;
            rest = after;
        }
        return if rest.is_empty() { Ok(()) } else { Err(OscError::Truncated) };
    }
    let (address, rest) = read_str(packet)?;
    // A message with no type-tag string at all is allowed by OSC 1.0 and has no arguments.
    if rest.is_empty() {
        f(Message { address, tags: &[], data: &[] });
        return Ok(());
    }
    let (tags, data) = read_str(rest)?;
    let tags = tags.as_bytes();
    let Some((b',', tags)) = tags.split_first() else {
        return Err(OscError::BadTag(tags.first().map_or('?', |&b| char::from(b))));
    };
    f(Message { address, tags, data });
    Ok(())
}

/// Builds OSC packets, for tests and for any sender (VMC out, PF-04) later. Allocates.
#[derive(Clone, Debug, Default)]
pub struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    /// A message at `address` with `args`.
    pub fn message(address: &str, args: &[Arg<'_>]) -> Vec<u8> {
        let mut w = Self::default();
        w.str(address);
        let mut tags = String::from(",");
        for arg in args {
            tags.push(match arg {
                Arg::Int(_) => 'i',
                Arg::Float(_) => 'f',
                Arg::Str(_) => 's',
                Arg::Blob(_) => 'b',
                Arg::Long(_) => 'h',
                Arg::Double(_) => 'd',
                Arg::Bool(true) => 'T',
                Arg::Bool(false) => 'F',
                Arg::Nil => 'N',
            });
        }
        w.str(&tags);
        for arg in args {
            match *arg {
                Arg::Int(v) => w.buf.extend_from_slice(&v.to_be_bytes()),
                Arg::Float(v) => w.buf.extend_from_slice(&v.to_be_bytes()),
                Arg::Str(s) => w.str(s),
                Arg::Blob(b) => {
                    w.buf.extend_from_slice(&u32::try_from(b.len()).unwrap_or(u32::MAX).to_be_bytes());
                    w.buf.extend_from_slice(b);
                    w.buf.resize(pad4(w.buf.len()), 0);
                }
                Arg::Long(v) => w.buf.extend_from_slice(&v.to_be_bytes()),
                Arg::Double(v) => w.buf.extend_from_slice(&v.to_be_bytes()),
                Arg::Bool(_) | Arg::Nil => {}
            }
        }
        w.buf
    }

    /// A bundle (time tag "immediately") of `elements`, each a message or bundle.
    pub fn bundle(elements: &[Vec<u8>]) -> Vec<u8> {
        let mut buf = BUNDLE.to_vec();
        buf.extend_from_slice(&1u64.to_be_bytes());
        for element in elements {
            buf.extend_from_slice(&u32::try_from(element.len()).unwrap_or(u32::MAX).to_be_bytes());
            buf.extend_from_slice(element);
        }
        buf
    }

    fn str(&mut self, s: &str) {
        self.buf.extend_from_slice(s.as_bytes());
        self.buf.push(0);
        self.buf.resize(pad4(self.buf.len()), 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(packet: &[u8]) -> Result<Vec<(String, Vec<String>)>, OscError> {
        let mut out = Vec::new();
        let mut failed = None;
        for_each_message(packet, &mut |m| match m.args().collect::<Result<Vec<_>, _>>() {
            Ok(args) => out.push((m.address.to_owned(), args.iter().map(|a| format!("{a:?}")).collect())),
            Err(e) => failed = Some(e),
        })?;
        failed.map_or(Ok(out), Err)
    }

    #[test]
    fn a_message_round_trips() {
        let packet = Writer::message("/VMC/Ext/Blend/Val", &[Arg::Str("JawOpen"), Arg::Float(0.5)]);
        assert_eq!(packet.len() % 4, 0);
        let got = collect(&packet).unwrap();
        assert_eq!(got, vec![("/VMC/Ext/Blend/Val".to_owned(), vec!["Str(\"JawOpen\")".into(), "Float(0.5)".into()])]);
    }

    #[test]
    fn nested_bundles_are_walked_in_order() {
        let a = Writer::message("/a", &[Arg::Int(1)]);
        let b = Writer::message("/b", &[Arg::Bool(true), Arg::Double(2.0), Arg::Blob(&[1, 2, 3])]);
        let inner = Writer::bundle(&[b]);
        let packet = Writer::bundle(&[a, inner, Writer::message("/c", &[])]);
        let got = collect(&packet).unwrap();
        let addresses: Vec<_> = got.iter().map(|(a, _)| a.as_str()).collect();
        assert_eq!(addresses, ["/a", "/b", "/c"]);
        assert_eq!(got[1].1, ["Bool(true)", "Double(2.0)", "Blob([1, 2, 3])"]);
    }

    #[test]
    fn malformed_packets_are_refused_not_panicked_on() {
        let good = Writer::message("/x", &[Arg::Float(1.0), Arg::Str("abc")]);
        // Every cut inside a field is refused; at 4 bytes the address stands alone, which OSC allows (no arguments).
        for cut in 0..good.len() {
            let result = collect(&good[..cut]);
            if cut == 4 {
                assert_eq!(result, Ok(vec![("/x".to_owned(), vec![])]));
            } else {
                assert!(result.is_err(), "a packet cut at {cut} was accepted: {result:?}");
            }
        }
        let mut deep = Writer::message("/deep", &[]);
        for _ in 0..=MAX_DEPTH {
            deep = Writer::bundle(&[deep]);
        }
        assert_eq!(collect(&deep), Err(OscError::TooDeep));
        assert!(matches!(collect(b"/x\0\0sf\0\0"), Err(OscError::BadTag('s'))));
    }

    proptest::proptest! {
        #[test]
        fn any_bytes_are_safe(bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..256)) {
            let _ = collect(&bytes);
        }
    }
}
