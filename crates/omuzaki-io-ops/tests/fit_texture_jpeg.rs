//! JPEG input to the texture resizer (#2009) and the scan-completion check in front of the decoder (#1144).
//!
//! Every JPEG is made here: by the image crate's baseline encoder, or by the small coefficient-level encoder below,
//! which writes the same coefficients as a baseline file (with or without restart markers) and as a progressive one
//! (spectral selection and successive approximation, with end-of-band runs), so the two must decode to the same pixels.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    reason = "test fixtures: a panic is a clear failure, and the casts are on small bounded values"
)]

use std::io::Cursor;
use std::panic::{AssertUnwindSafe, catch_unwind};

use image::codecs::jpeg::JpegEncoder;
use image::{ExtendedColorType, ImageFormat, ImageReader, RgbaImage};
use omuzaki_core::rng::Rng;
use omuzaki_io_api::{Blob, EncodedImage, ImageMime, IoError};
use omuzaki_io_ops::fit_texture::{TextureUse, downscale_to, resize};

fn jpeg(bytes: Vec<u8>) -> EncodedImage {
    EncodedImage::new("photo", ImageMime::Jpeg, Blob::new(bytes))
}

fn decode_png(image: &EncodedImage) -> RgbaImage {
    assert_eq!(image.mime, ImageMime::Png, "the output is a PNG");
    ImageReader::with_format(Cursor::new(image.bytes.as_slice()), ImageFormat::Png).decode().unwrap().into_rgba8()
}

/// A 64 by 48 RGB gradient through the image crate's baseline encoder.
fn from_the_image_crate() -> Vec<u8> {
    let pixels: Vec<u8> = (0..48_u32)
        .flat_map(|y| (0..64_u32).flat_map(move |x| [(x * 4) as u8, (y * 5) as u8, ((x + y) * 2) as u8]))
        .collect();
    let mut bytes = Vec::new();
    JpegEncoder::new_with_quality(&mut bytes, 90).encode(&pixels, 64, 48, ExtendedColorType::Rgb8).unwrap();
    bytes
}

/// How a file made by [`Encoder`] codes its coefficients.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Coding {
    Baseline,
    Progressive,
}

/// One component: id and sampling factors.
#[derive(Copy, Clone)]
struct Comp {
    id: u8,
    h: u32,
    v: u32,
}

/// A coefficient-level JPEG writer. Quantisation is all ones, so the coefficients are what is coded; the Huffman tables
/// give every DC size a 4-bit code and every AC symbol used here an 8-bit one.
struct Encoder {
    width: u32,
    height: u32,
    comps: Vec<Comp>,
    restart: u32,
}

/// The AC symbols the encoder uses, in code order: end of band (and end-of-band runs), sixteen zeros, then every run and
/// size pair.
fn ac_symbols() -> Vec<u8> {
    let mut symbols = vec![0x00, 0xF0];
    symbols.extend((1..=14).map(|r| r << 4));
    symbols.extend((0..16_u8).flat_map(|r| (1..=10).map(move |s| (r << 4) | s)));
    symbols
}

/// Writes bits most significant first, stuffing a zero after every 0xFF.
#[derive(Default)]
struct BitWriter {
    out: Vec<u8>,
    acc: u32,
    count: u32,
}

impl BitWriter {
    fn put(&mut self, value: u32, length: u32) {
        for i in (0..length).rev() {
            self.acc = (self.acc << 1) | ((value >> i) & 1);
            self.count += 1;
            if self.count == 8 {
                let byte = self.acc as u8;
                self.out.push(byte);
                if byte == 0xFF {
                    self.out.push(0);
                }
                self.acc = 0;
                self.count = 0;
            }
        }
    }

    /// Pads the last byte with ones.
    fn flush(&mut self) {
        while self.count != 0 {
            self.put(1, 1);
        }
    }
}

/// The size category of `v` and its bits as JPEG codes them (negative values one's complement).
fn magnitude(v: i32) -> (u32, u32) {
    let size = 32 - v.unsigned_abs().leading_zeros();
    let bits = if v < 0 { (v + (1 << size) - 1) as u32 } else { v as u32 };
    (size, bits)
}

/// The coefficients (zigzag order) of block (`row`, `column`) of component `comp`: a DC level that varies between
/// blocks, and a sparse scatter of AC values from -6 to 6.
fn coefficients(comp: usize, row: u32, column: u32) -> [i32; 64] {
    let mut c = [0; 64];
    c[0] = ((row * 7 + column * 13 + comp as u32 * 5) % 32) as i32 * 8 - 128;
    for k in 1..64_u32 {
        let h = (row * 31 + column * 17 + k * 7 + comp as u32 * 3) % 23;
        if h < 3 {
            c[k as usize] = (k % 13) as i32 - 6;
        }
    }
    c
}

/// A scan of a progressive file: components (indices), band, and the bit positions before and after.
struct ScanSpec {
    comps: Vec<usize>,
    ss: u32,
    se: u32,
    ah: u32,
    al: u32,
}

/// The state one scan carries from block to block.
struct ScanState {
    w: BitWriter,
    predictions: Vec<i32>,
    eob_run: u32,
    /// Correction bits of the blocks in the current end-of-band run (refinement scans).
    pending: Vec<u32>,
}

impl ScanState {
    fn symbol(&mut self, symbol: u8, ac: bool) {
        if ac {
            let code = ac_symbols().iter().position(|&s| s == symbol).unwrap();
            self.w.put(code as u32, 8);
        } else {
            self.w.put(u32::from(symbol), 4);
        }
    }

    fn dc(&mut self, comp: usize, value: i32) {
        let (size, bits) = magnitude(value - self.predictions[comp]);
        self.predictions[comp] = value;
        self.symbol(size as u8, false);
        self.w.put(bits, size);
    }

    fn flush_eob_run(&mut self) {
        if self.eob_run > 0 {
            let r = self.eob_run.ilog2();
            self.symbol((r as u8) << 4, true);
            self.w.put(self.eob_run - (1 << r), r);
            for bit in std::mem::take(&mut self.pending) {
                self.w.put(bit, 1);
            }
            self.eob_run = 0;
        }
    }

    fn end_band(&mut self) {
        self.eob_run += 1;
        if self.eob_run == 0x7FFF {
            self.flush_eob_run();
        }
    }

    fn sequential(&mut self, comp: usize, c: &[i32; 64]) {
        self.dc(comp, c[0]);
        let mut run = 0;
        for &v in &c[1..] {
            if v == 0 {
                run += 1;
                continue;
            }
            while run > 15 {
                self.symbol(0xF0, true);
                run -= 16;
            }
            let (size, bits) = magnitude(v);
            self.symbol(((run << 4) | size) as u8, true);
            self.w.put(bits, size);
            run = 0;
        }
        if run > 0 {
            self.symbol(0x00, true);
        }
    }

    fn ac_first(&mut self, c: &[i32; 64], ss: u32, se: u32, al: u32) {
        let shifted = |k: u32| {
            let v = c[k as usize];
            if v < 0 { -((-v) >> al) } else { v >> al }
        };
        if (ss..=se).all(|k| shifted(k) == 0) {
            self.end_band();
            return;
        }
        self.flush_eob_run();
        let mut run = 0;
        for k in ss..=se {
            let v = shifted(k);
            if v == 0 {
                run += 1;
                continue;
            }
            while run > 15 {
                self.symbol(0xF0, true);
                run -= 16;
            }
            let (size, bits) = magnitude(v);
            self.symbol(((run << 4) | size) as u8, true);
            self.w.put(bits, size);
            run = 0;
        }
        if run > 0 {
            self.end_band();
        }
    }

    fn ac_refine(&mut self, c: &[i32; 64], ss: u32, se: u32, al: u32) {
        let abs = |k: u32| c[k as usize].unsigned_abs() >> al;
        let Some(last_new) = (ss..=se).rev().find(|&k| abs(k) == 1) else {
            self.pending.extend((ss..=se).filter(|&k| abs(k) > 1).map(|k| abs(k) & 1));
            self.end_band();
            return;
        };
        self.flush_eob_run();
        let (mut run, mut corrections) = (0, Vec::new());
        for k in ss..=se {
            let a = abs(k);
            if a == 0 {
                run += 1;
                continue;
            }
            while run > 15 && k <= last_new {
                self.symbol(0xF0, true);
                for bit in corrections.drain(..) {
                    self.w.put(bit, 1);
                }
                run -= 16;
            }
            if a > 1 {
                corrections.push(a & 1);
                continue;
            }
            self.symbol(((run << 4) | 1) as u8, true);
            self.w.put(u32::from(c[k as usize] > 0), 1);
            for bit in corrections.drain(..) {
                self.w.put(bit, 1);
            }
            run = 0;
        }
        if run > 0 || !corrections.is_empty() {
            self.pending.extend(corrections);
            self.end_band();
        }
    }
}

fn segment(out: &mut Vec<u8>, marker: u8, payload: &[u8]) {
    out.extend([0xFF, marker]);
    out.extend(((payload.len() + 2) as u16).to_be_bytes());
    out.extend(payload);
}

impl Encoder {
    fn grey(width: u32, height: u32) -> Self {
        Encoder { width, height, comps: vec![Comp { id: 1, h: 1, v: 1 }], restart: 0 }
    }

    /// Luma at 2 by 2 and two chroma components at 1 by 1 (4:2:0).
    fn colour(width: u32, height: u32) -> Self {
        let comps = vec![Comp { id: 1, h: 2, v: 2 }, Comp { id: 2, h: 1, v: 1 }, Comp { id: 3, h: 1, v: 1 }];
        Encoder { width, height, comps, restart: 0 }
    }

    fn h_max(&self) -> u32 {
        self.comps.iter().map(|c| c.h).max().unwrap()
    }

    fn v_max(&self) -> u32 {
        self.comps.iter().map(|c| c.v).max().unwrap()
    }

    /// The blocks a scan of `comps` visits, MCU by MCU: (component, row, column).
    fn mcus(&self, comps: &[usize]) -> Vec<Vec<(usize, u32, u32)>> {
        if let [comp] = *comps {
            let c = self.comps[comp];
            let wide = (self.width * c.h).div_ceil(self.h_max()).div_ceil(8);
            let high = (self.height * c.v).div_ceil(self.v_max()).div_ceil(8);
            return (0..high).flat_map(|r| (0..wide).map(move |col| vec![(comp, r, col)])).collect();
        }
        let wide = self.width.div_ceil(8 * self.h_max());
        let high = self.height.div_ceil(8 * self.v_max());
        (0..high)
            .flat_map(|my| {
                (0..wide).map(move |mx| {
                    comps
                        .iter()
                        .flat_map(|&i| {
                            let c = self.comps[i];
                            (0..c.v).flat_map(move |y| (0..c.h).map(move |x| (i, my * c.v + y, mx * c.h + x)))
                        })
                        .collect()
                })
            })
            .collect()
    }

    fn scan(&self, out: &mut Vec<u8>, spec: &ScanSpec, coding: Coding) {
        let mut header = vec![spec.comps.len() as u8];
        for &i in &spec.comps {
            header.extend([self.comps[i].id, 0x00]);
        }
        header.extend([spec.ss as u8, spec.se as u8, ((spec.ah << 4) | spec.al) as u8]);
        segment(out, 0xDA, &header);
        let fresh = || ScanState {
            w: BitWriter::default(),
            predictions: vec![0; self.comps.len()],
            eob_run: 0,
            pending: Vec::new(),
        };
        let mut state = fresh();
        let mcus = self.mcus(&spec.comps);
        for (n, mcu) in mcus.iter().enumerate() {
            if self.restart > 0 && n > 0 && (n as u32).is_multiple_of(self.restart) {
                state.flush_eob_run();
                state.w.flush();
                out.extend(std::mem::take(&mut state.w.out));
                out.extend([0xFF, 0xD0 + ((n as u32 / self.restart - 1) % 8) as u8]);
                state = fresh();
            }
            for &(comp, row, column) in mcu {
                let c = coefficients(comp, row, column);
                match (coding, spec.ss, spec.ah) {
                    (Coding::Baseline, ..) => state.sequential(comp, &c),
                    (Coding::Progressive, 0, 0) => state.dc(comp, c[0] >> spec.al),
                    (Coding::Progressive, 0, _) => state.w.put(((c[0] >> spec.al) & 1) as u32, 1),
                    (Coding::Progressive, _, 0) => state.ac_first(&c, spec.ss, spec.se, spec.al),
                    (Coding::Progressive, ..) => state.ac_refine(&c, spec.ss, spec.se, spec.al),
                }
            }
        }
        state.flush_eob_run();
        state.w.flush();
        out.extend(state.w.out);
    }

    fn encode(&self, coding: Coding) -> Vec<u8> {
        let mut out = vec![0xFF, 0xD8];
        segment(&mut out, 0xDB, &[[0_u8].as_slice(), &[1; 64]].concat());
        let mut frame = vec![8];
        frame.extend((self.height as u16).to_be_bytes());
        frame.extend((self.width as u16).to_be_bytes());
        frame.push(self.comps.len() as u8);
        for c in &self.comps {
            frame.extend([c.id, ((c.h << 4) | c.v) as u8, 0]);
        }
        segment(&mut out, if coding == Coding::Baseline { 0xC0 } else { 0xC2 }, &frame);
        let mut dht = vec![0x00];
        dht.extend([0, 0, 0, 12, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        dht.extend(0..12);
        let ac = ac_symbols();
        dht.push(0x10);
        dht.extend([0, 0, 0, 0, 0, 0, 0, ac.len() as u8, 0, 0, 0, 0, 0, 0, 0, 0]);
        dht.extend(&ac);
        segment(&mut out, 0xC4, &dht);
        if self.restart > 0 {
            segment(&mut out, 0xDD, &(self.restart as u16).to_be_bytes());
        }
        let all: Vec<usize> = (0..self.comps.len()).collect();
        let specs = match coding {
            Coding::Baseline => vec![ScanSpec { comps: all, ss: 0, se: 63, ah: 0, al: 0 }],
            Coding::Progressive => {
                let mut specs = vec![ScanSpec { comps: all.clone(), ss: 0, se: 0, ah: 0, al: 1 }];
                for &i in &all {
                    specs.push(ScanSpec { comps: vec![i], ss: 1, se: 5, ah: 0, al: 2 });
                    specs.push(ScanSpec { comps: vec![i], ss: 6, se: 63, ah: 0, al: 1 });
                }
                specs.push(ScanSpec { comps: all.clone(), ss: 0, se: 0, ah: 1, al: 0 });
                for &i in &all {
                    specs.push(ScanSpec { comps: vec![i], ss: 1, se: 5, ah: 2, al: 1 });
                    specs.push(ScanSpec { comps: vec![i], ss: 1, se: 5, ah: 1, al: 0 });
                    specs.push(ScanSpec { comps: vec![i], ss: 6, se: 63, ah: 1, al: 0 });
                }
                specs
            }
        };
        for spec in &specs {
            self.scan(&mut out, spec, coding);
        }
        out.extend([0xFF, 0xD9]);
        out
    }
}

/// The byte ranges of each scan's entropy-coded data: from after its header to the next marker.
fn scan_data(bytes: &[u8]) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut at = 2;
    while at + 4 <= bytes.len() {
        let marker = bytes[at + 1];
        if marker == 0xD9 {
            break;
        }
        let length = usize::from(u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]));
        at += 2 + length;
        if marker == 0xDA {
            let start = at;
            while bytes[at] != 0xFF || matches!(bytes[at + 1], 0x00 | 0xD0..=0xD7) {
                at += 1;
            }
            ranges.push((start, at));
        }
    }
    ranges
}

/// Every fixture: the image crate's baseline file, and the coefficient encoder's grey and colour files in each coding,
/// with and without restart markers.
fn fixtures() -> Vec<(String, Vec<u8>)> {
    let mut all = vec![("image crate baseline".to_owned(), from_the_image_crate())];
    for (shape, mut encoder) in [("grey", Encoder::grey(40, 24)), ("colour", Encoder::colour(40, 40))] {
        for restart in [0, 2] {
            encoder.restart = restart;
            for (coding, label) in [(Coding::Baseline, "baseline"), (Coding::Progressive, "progressive")] {
                all.push((format!("{shape} {label} restart {restart}"), encoder.encode(coding)));
            }
        }
    }
    all
}

fn same_size(bytes: &[u8]) -> Result<EncodedImage, IoError> {
    let size = image::load_from_memory_with_format(bytes, ImageFormat::Jpeg).map(|i| (i.width(), i.height()));
    let (width, height) = size.unwrap_or((8, 8));
    resize(&jpeg(bytes.to_vec()), width, height, TextureUse::Colour)
}

#[test]
fn a_baseline_jpeg_resizes_to_a_png() {
    let out = resize(&jpeg(from_the_image_crate()), 32, 24, TextureUse::Colour).expect("a JPEG input resizes");
    let png = decode_png(&out);
    assert_eq!(png.dimensions(), (32, 24));
    // The gradient's red ramp: x * 4 at the source, so the output's last column is near 248.
    let red = png.get_pixel(31, 12)[0];
    assert!(red > 230, "red at the right edge was {red}");
    assert!(png.pixels().all(|p| p[3] == 255), "a JPEG is opaque");
}

#[test]
fn progressive_and_restart_files_decode_to_the_same_pixels_as_baseline() {
    for encoder in [Encoder::grey(40, 24), Encoder::colour(40, 40)] {
        let mut encoder = encoder;
        let baseline = decode_png(&same_size(&encoder.encode(Coding::Baseline)).expect("baseline"));
        let progressive = decode_png(&same_size(&encoder.encode(Coding::Progressive)).expect("progressive"));
        encoder.restart = 2;
        let restarts = decode_png(&same_size(&encoder.encode(Coding::Progressive)).expect("restart intervals"));
        assert_eq!(baseline.dimensions(), (encoder.width, encoder.height));
        assert!(baseline == progressive, "progressive matches baseline");
        assert!(baseline == restarts, "restart intervals change nothing");
    }
}

#[test]
fn a_jpeg_that_fits_is_kept_and_one_that_doesnt_comes_back_as_png() {
    let bytes = from_the_image_crate();
    assert!(downscale_to(&jpeg(bytes.clone()), 64, TextureUse::Colour).unwrap().is_none());
    let out = downscale_to(&jpeg(bytes), 16, TextureUse::Colour).unwrap().expect("shrunk");
    assert_eq!(decode_png(&out).dimensions(), (16, 12));
}

#[test]
fn every_scan_cut_at_each_sixteenth_is_malformed_and_the_whole_file_passes() {
    for (name, bytes) in fixtures() {
        assert!(same_size(&bytes).is_ok(), "{name}: the whole file passes");
        let scans = scan_data(&bytes);
        assert!(!scans.is_empty(), "{name}");
        for (n, &(start, end)) in scans.iter().enumerate() {
            for sixteenth in 0..16 {
                let cut = start + (end - start) * sixteenth / 16;
                let result = same_size(&bytes[..cut]);
                assert!(matches!(result, Err(IoError::Malformed { .. })), "{name}: scan {n} cut at {cut}: {result:?}");
            }
        }
        // Cut inside the end-of-image marker too.
        assert!(matches!(same_size(&bytes[..bytes.len() - 1]), Err(IoError::Malformed { .. })), "{name}");
    }
}

#[test]
fn a_cut_refinement_scan_followed_by_the_rest_of_the_file_is_malformed_naming_the_scan() {
    let bytes = Encoder::colour(40, 40).encode(Coding::Progressive);
    let scans = scan_data(&bytes);
    // The last scan is a refinement of the last chroma component's whole band.
    let (start, end) = scans[scans.len() - 1];
    let spliced = [&bytes[..start + (end - start) / 2], &bytes[end..]].concat();
    let Err(IoError::Malformed { message, .. }) = same_size(&spliced) else {
        panic!("a cut refinement scan must be malformed");
    };
    assert!(message.contains(&format!("scan {}", scans.len())), "{message}");
    assert!(message.contains("MCUs are complete"), "{message}");
}

#[test]
fn a_missing_or_misordered_restart_marker_is_refused() {
    let mut encoder = Encoder::grey(40, 24);
    encoder.restart = 2;
    let bytes = encoder.encode(Coding::Baseline);
    let at = bytes.windows(2).position(|w| w == [0xFF, 0xD1]).expect("a second restart marker");
    let missing = [&bytes[..at], &bytes[at + 2..]].concat();
    let message = match same_size(&missing) {
        Err(IoError::Malformed { message, .. }) => message,
        other => panic!("a missing restart marker must be malformed: {other:?}"),
    };
    assert!(message.contains("restart marker"), "{message}");
    let mut misordered = bytes.clone();
    misordered[at + 1] = 0xD3;
    assert!(matches!(same_size(&misordered), Err(IoError::Malformed { .. })));
}

#[test]
fn seeded_byte_flips_never_panic() {
    let mut rng = Rng::new(0x1144_2009);
    for (name, bytes) in fixtures() {
        for _ in 0..512 {
            let mut flipped = bytes.clone();
            let at = (rng.next_u64() % flipped.len() as u64) as usize;
            flipped[at] = (rng.next_u64() & 0xFF) as u8;
            let result = catch_unwind(AssertUnwindSafe(|| {
                resize(&jpeg(flipped.clone()), 7, 5, TextureUse::Colour).map(|out| decode_png(&out).dimensions())
            }));
            match result {
                Ok(Ok(size)) => assert_eq!(size, (7, 5), "{name}: byte {at}"),
                Ok(Err(_)) => {}
                Err(panic) => {
                    eprintln!("{name}: byte {at} set to {:#x} panicked", flipped[at]);
                    std::panic::resume_unwind(panic);
                }
            }
        }
    }
}

#[test]
fn a_header_declaring_an_oversized_jpeg_is_a_limit_before_any_decoding() {
    let mut bytes = Encoder::grey(8, 8).encode(Coding::Baseline);
    let sof = bytes.windows(2).position(|w| w == [0xFF, 0xC0]).unwrap();
    bytes[sof + 5..sof + 9].copy_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
    let result = resize(&jpeg(bytes), 8, 8, TextureUse::Colour);
    assert!(matches!(result, Err(IoError::Limit { what: "texture side", .. })), "{result:?}");
}

#[test]
fn lossless_and_arithmetic_jpegs_are_unsupported() {
    for marker in [0xC3, 0xC9, 0xCA] {
        let mut bytes = Encoder::grey(8, 8).encode(Coding::Baseline);
        let sof = bytes.windows(2).position(|w| w == [0xFF, 0xC0]).unwrap();
        bytes[sof + 1] = marker;
        let result = resize(&jpeg(bytes), 8, 8, TextureUse::Colour);
        assert!(matches!(result, Err(IoError::Unsupported { .. })), "{marker:#x}: {result:?}");
    }
}
