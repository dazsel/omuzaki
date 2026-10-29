//! Hand-assembled `Inochi2D` files for the tests: every byte is written here, from the format, with synthetic art.
#![allow(clippy::unwrap_used, clippy::expect_used, dead_code, reason = "test helpers; each test file uses some")]

use image::ImageEncoder;
use serde_json::{Value, json};

/// An INP1 file: the magic, the payload, and a texture section when `textures` is `Some`.
pub fn inp1(payload: &Value, textures: Option<&[(u8, Vec<u8>)]>) -> Vec<u8> {
    let json = serde_json::to_vec(payload).unwrap();
    let mut out = b"TRNSRTS\0".to_vec();
    out.extend_from_slice(&u32::try_from(json.len()).unwrap().to_be_bytes());
    out.extend_from_slice(&json);
    if let Some(textures) = textures {
        out.extend_from_slice(b"TEX_SECT");
        out.extend_from_slice(&u32::try_from(textures.len()).unwrap().to_be_bytes());
        for (encoding, data) in textures {
            out.extend_from_slice(&u32::try_from(data.len()).unwrap().to_be_bytes());
            out.push(*encoding);
            out.extend_from_slice(data);
        }
    }
    out
}

/// An `EXT_SECT` with the given entries.
pub fn ext_sect(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = b"EXT_SECT".to_vec();
    out.extend_from_slice(&u32::try_from(entries.len()).unwrap().to_be_bytes());
    for (name, data) in entries {
        out.extend_from_slice(&u32::try_from(name.len()).unwrap().to_be_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&u32::try_from(data.len()).unwrap().to_be_bytes());
        out.extend_from_slice(data);
    }
    out
}

/// A synthetic `w` x `h` RGBA image: a gradient with a soft alpha ramp, premultiplied.
pub fn pixels(w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let a = u8::try_from((x * 255) / w.max(2).saturating_sub(1).max(1)).unwrap_or(255);
            let c = |v: u32| u8::try_from((v.min(255) * u32::from(a)) / 255).unwrap();
            out.extend_from_slice(&[c(x * 40), c(y * 40), c(128), a]);
        }
    }
    out
}

/// `rgba` as PNG.
pub fn png(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out).write_image(rgba, w, h, image::ExtendedColorType::Rgba8).unwrap();
    out
}

/// `rgba` as TGA.
pub fn tga(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    image::codecs::tga::TgaEncoder::new(&mut out).write_image(rgba, w, h, image::ExtendedColorType::Rgba8).unwrap();
    out
}

/// A unit transform.
pub fn identity() -> Value {
    json!({ "trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0] })
}

/// A 10 x 10 quad centred on its node, as `Inochi2D` makes a part from a texture.
pub fn quad_mesh() -> Value {
    json!({
        "verts": [-5.0, -5.0, 5.0, -5.0, 5.0, 5.0, -5.0, 5.0],
        "uvs": [0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0],
        "indices": [0, 1, 2, 0, 2, 3],
        "origin": [0.0, 0.0],
    })
}

/// A part node.
pub fn part(uuid: u32, name: &str, texture: u32) -> Value {
    json!({
        "uuid": uuid, "name": name, "type": "Part", "enabled": true, "zsort": 0.0, "transform": identity(),
        "mesh": quad_mesh(), "textures": [texture, u32::MAX, u32::MAX], "blend_mode": "Normal",
        "tint": [1.0, 1.0, 1.0], "screenTint": [0.0, 0.0, 0.0], "mask_threshold": 0.5, "opacity": 1.0, "masks": [],
    })
}

/// A minimal puppet: a root, one part, one parameter with a deform binding, and metadata.
pub fn minimal() -> Value {
    json!({
        "meta": {
            "name": "Synthetic", "version": "1.0-alpha", "artist": "Omuzaki tests",
            "rights": { "allowCommercial": false, "allowRedistribution": true, "requireAttribution": true },
            "thumbnailId": u32::MAX, "preservePixels": false,
        },
        "physics": { "pixelsPerMeter": 1000.0, "gravity": 9.8 },
        "nodes": {
            "uuid": 1, "name": "Root", "type": "Node", "enabled": true, "zsort": 0.0, "transform": identity(),
            "children": [part(2, "Quad", 0)],
        },
        "param": [{
            "uuid": 100, "name": "Wave", "is_vec2": false, "min": [-1.0, 0.0], "max": [1.0, 0.0],
            "defaults": [0.0, 0.0], "axis_points": [[0.0, 0.5, 1.0], [0.0]],
            "bindings": [{
                "node": 2, "param_name": "deform", "interpolate_mode": "Linear",
                "values": [[[[0.0, -1.0], [0.0, -1.0], [0.0, 0.0], [0.0, 0.0]]],
                           [[[0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]]],
                           [[[0.0, 1.0], [0.0, 1.0], [0.0, 0.0], [0.0, 0.0]]]],
                "isSet": [[true], [true], [true]],
            }],
        }],
    })
}

/// The minimal puppet as an INP1 file with one 4 x 4 PNG texture.
pub fn minimal_file() -> Vec<u8> {
    inp1(&minimal(), Some(&[(0, png(4, 4, &pixels(4, 4)))]))
}

/// An INP2 tagged tree, written as the spec lays it out (little-endian, 4-byte aligned from the file start).
pub struct Inp2 {
    /// The file so far, magic included.
    pub buf: Vec<u8>,
}

impl Inp2 {
    /// A file with just the magic.
    pub fn new() -> Self {
        Self { buf: b"TRNSRTS2".to_vec() }
    }

    fn tag(&mut self, ty: u32, meta: u32) {
        self.buf.extend_from_slice(&(ty | (meta << 8)).to_le_bytes());
    }

    fn pad(&mut self) {
        while !self.buf.len().is_multiple_of(4) {
            self.buf.push(0);
        }
    }

    /// Opens an object of `count` entries.
    pub fn object(&mut self, count: u32) -> &mut Self {
        self.tag(0x12, count);
        self
    }

    /// Closes an object.
    pub fn end_object(&mut self) -> &mut Self {
        self.tag(0x13, 0);
        self
    }

    /// Opens an array of `count` items.
    pub fn array(&mut self, count: u32) -> &mut Self {
        self.tag(0x10, count);
        self
    }

    /// Closes an array.
    pub fn end_array(&mut self) -> &mut Self {
        self.tag(0x11, 0);
        self
    }

    /// An object key.
    pub fn key(&mut self, k: &str) -> &mut Self {
        self.tag(0xF0, u32::try_from(k.len()).unwrap());
        self.buf.extend_from_slice(k.as_bytes());
        self.pad();
        self
    }

    /// An unsigned integer.
    pub fn uint(&mut self, v: u32) -> &mut Self {
        self.tag(0x03, 0);
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    /// A string.
    pub fn string(&mut self, s: &str) -> &mut Self {
        self.tag(0x05, u32::try_from(s.len()).unwrap());
        self.buf.extend_from_slice(s.as_bytes());
        self.pad();
        self
    }

    /// A blob with its CRC-32.
    pub fn blob(&mut self, b: &[u8]) -> &mut Self {
        self.tag(0x06, u32::try_from(b.len()).unwrap());
        self.buf.extend_from_slice(b);
        self.pad();
        self.buf.extend_from_slice(&crc32fast::hash(b).to_le_bytes());
        self
    }
}

/// A small INP2 file: a payload object, one PNG texture and one vendor entry.
pub fn inp2_file() -> Vec<u8> {
    let tex = png(2, 2, &pixels(2, 2));
    let mut w = Inp2::new();
    w.object(3);
    w.key("INP_SECT").object(1).key("name").string("Synthetic").end_object();
    w.key("TEX_SECT").array(1).object(2).key("encoding").uint(0).key("data").blob(&tex).end_object().end_array();
    w.key("EXT_SECT").object(1).key("org.omuzaki.test").blob(b"vendor bytes").end_object();
    w.end_object();
    w.buf
}

/// A tiny seeded generator (xorshift64*), so the mutation tests are the same on every run.
pub struct Rng(pub u64);

impl Rng {
    /// The next 64 random bits.
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A number below `n` (`n > 0`).
    pub fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(n).unwrap()).unwrap()
    }
}
