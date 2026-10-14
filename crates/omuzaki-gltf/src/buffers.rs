//! Where the bytes come from: the GLB's binary chunk, `data:` URIs and sidecar files the host resolves.

use omuzaki_io_api::{IoError, Resolver};

use crate::schema::Document;
use crate::uri::{self, Target};

/// Reads sidecar files and `data:` URIs, and meters everything the importer copies out of the document's data (those
/// files, and images cut from buffers) against the input-size limit, so a file can't make the importer read or copy
/// without end: a hostile `.glb` can name one large buffer view as a thousand images.
pub struct Loader<'a> {
    resolver: &'a dyn Resolver,
    limit: u64,
    used: u64,
}

impl<'a> Loader<'a> {
    /// A loader that reads through `resolver` and allows `limit` bytes in all.
    pub fn new(resolver: &'a dyn Resolver, limit: u64) -> Self {
        Self { resolver, limit, used: 0 }
    }

    /// The bytes behind `uri`.
    ///
    /// # Errors
    /// [`IoError::Resource`] when the URI can't or must not be read, [`IoError::Limit`] when the loader add up to
    /// more than the input limit, or whatever the resolver returns.
    pub fn read(&mut self, uri: &str) -> Result<Vec<u8>, IoError> {
        let remaining = self.limit.saturating_sub(self.used);
        let bytes = match uri::classify(uri, remaining)? {
            Target::Data(bytes) => bytes,
            Target::Path(path) => self.resolver.read(&path)?,
            Target::Refused(reason) => {
                return Err(IoError::Resource { uri: uri.to_owned(), reason: reason.to_owned() });
            }
        };
        self.charge(bytes.len())?;
        Ok(bytes)
    }

    /// Counts `len` bytes against the limit.
    ///
    /// # Errors
    /// [`IoError::Limit`] when the total would pass the limit.
    pub fn charge(&mut self, len: usize) -> Result<(), IoError> {
        let got = self.used.saturating_add(len as u64);
        if got > self.limit {
            return Err(IoError::Limit { what: "total size of external data in bytes", limit: self.limit, got });
        }
        self.used = got;
        Ok(())
    }
}

enum Source<'a> {
    /// The GLB's binary chunk.
    Chunk(&'a [u8]),
    /// A `data:` URI or sidecar file.
    Owned(Vec<u8>),
}

/// The document's buffers, loaded. Each is cut to its declared `byteLength`, so everything past it (GLB padding) is
/// invisible to the accessors.
pub struct Buffers<'a> {
    items: Vec<Source<'a>>,
    lengths: Vec<usize>,
}

impl<'a> Buffers<'a> {
    /// Loads every buffer of `doc`.
    ///
    /// # Errors
    /// [`IoError::Malformed`] for a buffer with no source or fewer bytes than it declares; whatever
    /// [`Loader::read`] returns for the rest.
    pub fn load(doc: &Document, chunk: Option<&'a [u8]>, loader: &mut Loader<'_>) -> Result<Self, IoError> {
        let mut items = Vec::with_capacity(doc.buffers.len());
        let mut lengths = Vec::with_capacity(doc.buffers.len());
        for (i, def) in doc.buffers.iter().enumerate() {
            let source = match (&def.uri, chunk) {
                (Some(uri), _) => Source::Owned(loader.read(uri)?),
                (None, Some(chunk)) if i == 0 => Source::Chunk(chunk),
                (None, _) => {
                    return Err(IoError::Malformed {
                        message: format!("buffer {i} has no uri and isn't the GLB's binary chunk"),
                        at: Some(format!("/buffers/{i}")),
                    });
                }
            };
            let have = match &source {
                Source::Chunk(b) => b.len(),
                Source::Owned(b) => b.len(),
            };
            if have < def.byte_length {
                return Err(IoError::Malformed {
                    message: format!("buffer {i} declares {} bytes but has {have}", def.byte_length),
                    at: Some(format!("/buffers/{i}/byteLength")),
                });
            }
            items.push(source);
            lengths.push(def.byte_length);
        }
        Ok(Self { items, lengths })
    }

    /// Buffer `ix`, cut to its declared length.
    pub fn get(&self, ix: usize) -> Option<&[u8]> {
        let bytes = match self.items.get(ix)? {
            Source::Chunk(b) => b,
            Source::Owned(b) => b.as_slice(),
        };
        bytes.get(..*self.lengths.get(ix)?)
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_io_api::NoResolver;

    use super::*;
    use crate::schema::Buffer as BufferDef;

    struct One;
    impl Resolver for One {
        fn read(&self, uri: &str) -> Result<Vec<u8>, IoError> {
            if uri == "a b.bin" {
                Ok(vec![1, 2, 3, 4])
            } else {
                Err(IoError::Resource { uri: uri.into(), reason: "nope".into() })
            }
        }
    }

    fn doc(buffers: Vec<BufferDef>) -> Document {
        Document { buffers, ..Document::default() }
    }

    fn def(uri: Option<&str>, byte_length: usize) -> BufferDef {
        BufferDef { uri: uri.map(str::to_owned), byte_length, ..BufferDef::default() }
    }

    #[test]
    fn buffers_come_from_the_chunk_data_uris_and_the_resolver() {
        let d = doc(vec![def(None, 2), def(Some("data:;base64,AQID"), 3), def(Some("a%20b.bin"), 4)]);
        let mut side = Loader::new(&One, 100);
        let chunk = [9u8, 8, 7, 6];
        let b = Buffers::load(&d, Some(&chunk), &mut side).unwrap();
        assert_eq!(b.get(0), Some(&[9, 8][..]), "cut to the declared length");
        assert_eq!(b.get(1), Some(&[1, 2, 3][..]));
        assert_eq!(b.get(2), Some(&[1, 2, 3, 4][..]));
        assert_eq!(b.get(3), None);
    }

    #[test]
    fn short_or_unsourced_buffers_are_malformed_and_loader_are_capped() {
        let mut side = Loader::new(&NoResolver, 100);
        let short = doc(vec![def(None, 8)]);
        assert!(matches!(Buffers::load(&short, Some(&[0; 4]), &mut side), Err(IoError::Malformed { .. })));
        let no_chunk = doc(vec![def(None, 4)]);
        assert!(matches!(Buffers::load(&no_chunk, None, &mut side), Err(IoError::Malformed { .. })));
        let second_unsourced = doc(vec![def(None, 4), def(None, 4)]);
        assert!(matches!(Buffers::load(&second_unsourced, Some(&[0; 4]), &mut side), Err(IoError::Malformed { .. })));
        let mut tiny = Loader::new(&One, 3);
        assert!(matches!(tiny.read("a%20b.bin"), Err(IoError::Limit { .. })));
        let mut ok = Loader::new(&One, 100);
        assert!(matches!(ok.read("missing.bin"), Err(IoError::Resource { .. })));
        assert!(matches!(ok.read("https://x/y.bin"), Err(IoError::Resource { .. })));
    }
}
