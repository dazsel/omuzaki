//! The JPEG scan-completion check behind [`crate::fit_texture`] (#1144).
//!
//! JPEG decoders fill in zero bits when a scan's data stops early, so a truncated file decodes into a silently damaged
//! texture. Before a JPEG is decoded, [`check_scans`] walks the entropy-coded data of every scan against the frame
//! header: the Huffman tables, byte stuffing, restart markers and the number of MCUs each scan needs, for baseline and
//! extended sequential files and for progressive ones (spectral selection and successive approximation). It passes a
//! file only when every scan's data is complete before the marker that ends it and the file ends with its end-of-image
//! marker.
//!
//! It decodes no pixels. Its only allocations are per file (the frame's components and, for a progressive file, one
//! bit per coefficient of each block saying whether it is non-zero yet, which a refinement scan needs to know how many
//! correction bits to read). The bits it reads are bounded by the input; the blocks a scan visits are bounded by the
//! frame size, which the caller checks first, and the number of scans by [`MAX_SCANS`].

use omuzaki_io_api::IoError;

/// The most scans a file may have. Real progressive files have about ten; each scan visits every block of its
/// components, so a file of thousands of tiny scans would be a cheap way to spend time.
const MAX_SCANS: u64 = 256;

/// One component of the frame: its id and sampling factors, and its blocks.
struct Component {
    id: u8,
    h: u32,
    v: u32,
    /// Blocks across and down that hold the component's own samples (a single-component scan visits these).
    blocks_wide: u32,
    blocks_high: u32,
    /// Blocks across the MCU grid, padding included (an interleaved scan visits these); the row stride of `nonzero`.
    stride: u32,
    /// For a progressive frame, one bit per coefficient (zigzag order) of each block of the MCU grid: non-zero yet.
    nonzero: Vec<u64>,
}

/// The frame header (SOF).
struct Frame {
    progressive: bool,
    components: Vec<Component>,
    mcus_wide: u32,
    mcus_high: u32,
}

/// A Huffman table in the canonical form of JPEG's Annex F: for each code length, the largest code and the offset
/// from a code to its symbol.
struct Huffman {
    max_code: [i32; 17],
    offset: [i32; 17],
    symbols: [u8; 256],
}

/// Why the entropy-coded data stopped being read.
enum Stop {
    /// The data ran out: a marker or the end of the file came before the bits the scan needs.
    End,
    /// The data is wrong in a way that names itself.
    Bad(&'static str),
}

/// The width and height a JPEG's frame header declares, read without walking any scan.
///
/// # Errors
/// [`IoError::Malformed`] when there is no frame header before the first scan or the end; [`IoError::Unsupported`] for
/// a lossless, hierarchical or arithmetic-coded file, more than 8 bits per sample, or a height declared later (DNL).
pub(crate) fn frame_size(name: &str, bytes: &[u8]) -> Result<(u32, u32), IoError> {
    let mut at = soi(name, bytes)?;
    loop {
        let marker = marker(name, bytes, &mut at)?;
        if marker == 0xDA || marker == 0xD9 {
            return Err(malformed(name, "there is no frame header before the image data"));
        }
        if standalone(marker) {
            continue;
        }
        let (payload, end) = segment(name, bytes, at)?;
        if is_frame(marker) {
            let (width, height, _) = frame_header(name, marker, payload)?;
            return Ok((width, height));
        }
        at = end;
    }
}

/// Walks every scan of the JPEG in `bytes` and passes it only when each scan's data completes before its marker and
/// the file ends with EOI. `max_pixels` bounds the frame (the caller has checked the header against it already).
///
/// # Errors
/// [`IoError::Malformed`] naming the scan and the last complete MCU for data that stops early, or naming what is wrong;
/// [`IoError::Unsupported`] as [`frame_size`]; [`IoError::Limit`] for more than [`MAX_SCANS`] scans or a frame over
/// `max_pixels`.
pub(crate) fn check_scans(name: &str, bytes: &[u8], max_pixels: u64) -> Result<(), IoError> {
    let mut at = soi(name, bytes)?;
    let mut frame: Option<Frame> = None;
    let mut dc: [Option<Huffman>; 4] = Default::default();
    let mut ac: [Option<Huffman>; 4] = Default::default();
    let mut interval = 0_u32;
    let mut scans = 0_u64;
    loop {
        let marker = marker(name, bytes, &mut at)?;
        match marker {
            0xD9 if scans == 0 => return Err(malformed(name, "the file ends without image data")),
            0xD9 => return Ok(()),
            0xD8 => return Err(malformed(name, "a second start-of-image marker")),
            m if standalone(m) => continue,
            _ => {}
        }
        let (payload, end) = segment(name, bytes, at)?;
        at = end;
        match marker {
            0xC4 => tables(name, payload, &mut dc, &mut ac)?,
            0xDD => {
                let [hi, lo] = *payload else {
                    return Err(malformed(name, "the restart interval segment is not 2 bytes"));
                };
                interval = u32::from(u16::from_be_bytes([hi, lo]));
            }
            0xDC => return Err(unsupported(name, "a JPEG whose height is declared after its first scan (DNL)")),
            m if is_frame(m) => {
                if frame.is_some() {
                    return Err(malformed(name, "a second frame header"));
                }
                frame = Some(new_frame(name, m, payload, max_pixels)?);
            }
            0xDA => {
                let Some(frame) = frame.as_mut() else {
                    return Err(malformed(name, "a scan comes before the frame header"));
                };
                scans += 1;
                if scans > MAX_SCANS {
                    return Err(IoError::Limit { what: "JPEG scans", limit: MAX_SCANS, got: scans });
                }
                let scan = Scan::parse(name, payload, frame, &dc, &ac)?;
                at = scan.walk(name, bytes, at, frame, interval, scans)?;
            }
            _ => {}
        }
    }
}

/// The offset after the start-of-image marker, which must open the file.
fn soi(name: &str, bytes: &[u8]) -> Result<usize, IoError> {
    if bytes.starts_with(&[0xFF, 0xD8]) { Ok(2) } else { Err(malformed(name, "not a JPEG (no start-of-image marker)")) }
}

/// Reads the marker at `at` (after any fill bytes) and moves past it.
fn marker(name: &str, bytes: &[u8], at: &mut usize) -> Result<u8, IoError> {
    match bytes.get(*at) {
        None => return Err(malformed(name, "the file ends before its end-of-image marker")),
        Some(0xFF) => {}
        Some(_) => return Err(malformed(name, "a marker was expected between segments")),
    }
    while bytes.get(*at) == Some(&0xFF) {
        *at += 1;
    }
    let code = *bytes.get(*at).ok_or_else(|| malformed(name, "the file ends before its end-of-image marker"))?;
    *at += 1;
    Ok(code)
}

/// Markers with no segment after them: restarts and TEM.
fn standalone(marker: u8) -> bool {
    matches!(marker, 0xD0..=0xD7 | 0x01)
}

/// The start-of-frame markers (C0 to CF but DHT, JPG and DAC).
fn is_frame(marker: u8) -> bool {
    matches!(marker, 0xC0..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC)
}

/// The payload of the segment whose length field is at `at`, and the offset just past it.
fn segment<'a>(name: &str, bytes: &'a [u8], at: usize) -> Result<(&'a [u8], usize), IoError> {
    let length = match bytes.get(at..at + 2) {
        Some(&[hi, lo]) => usize::from(u16::from_be_bytes([hi, lo])),
        _ => return Err(malformed(name, "the file ends inside a segment's length")),
    };
    if length < 2 {
        return Err(malformed(name, "a segment's length is shorter than its own length field"));
    }
    let end = at + length;
    let payload = bytes.get(at + 2..end).ok_or_else(|| malformed(name, "a segment runs past the end of the file"))?;
    Ok((payload, end))
}

/// The width, height and component bytes of a frame header, with the kinds this build reads checked.
fn frame_header<'a>(name: &str, marker: u8, payload: &'a [u8]) -> Result<(u32, u32, &'a [u8]), IoError> {
    if !matches!(marker, 0xC0..=0xC2) {
        return Err(unsupported(name, "a lossless, hierarchical or arithmetic-coded JPEG"));
    }
    let [precision, h0, h1, w0, w1, count, ref rest @ ..] = *payload else {
        return Err(malformed(name, "the frame header is too short"));
    };
    if precision != 8 {
        return Err(unsupported(name, "a JPEG with more than 8 bits per sample"));
    }
    let (height, width) = (u32::from(u16::from_be_bytes([h0, h1])), u32::from(u16::from_be_bytes([w0, w1])));
    if height == 0 {
        return Err(unsupported(name, "a JPEG whose height is declared after its first scan (DNL)"));
    }
    if width == 0 {
        return Err(malformed(name, "the frame header declares a zero width"));
    }
    if !(1..=4).contains(&count) || rest.len() != usize::from(count) * 3 {
        return Err(malformed(name, "the frame header's component list is wrong"));
    }
    Ok((width, height, rest))
}

/// The frame, with each component's block grid.
fn new_frame(name: &str, marker: u8, payload: &[u8], max_pixels: u64) -> Result<Frame, IoError> {
    let (width, height, list) = frame_header(name, marker, payload)?;
    let pixels = u64::from(width) * u64::from(height);
    if pixels > max_pixels {
        return Err(IoError::Limit { what: "texture pixels", limit: max_pixels, got: pixels });
    }
    let mut sampling = Vec::with_capacity(4);
    for c in list.as_chunks::<3>().0 {
        let (h, v) = (u32::from(c[1] >> 4), u32::from(c[1] & 0x0F));
        if !(1..=4).contains(&h) || !(1..=4).contains(&v) {
            return Err(malformed(name, "a component's sampling factor is outside 1 to 4"));
        }
        if sampling.iter().any(|&(id, _, _)| id == c[0]) {
            return Err(malformed(name, "two components share an id"));
        }
        sampling.push((c[0], h, v));
    }
    let h_max = sampling.iter().map(|s| s.1).max().unwrap_or(1);
    let v_max = sampling.iter().map(|s| s.2).max().unwrap_or(1);
    let (mcus_wide, mcus_high) = (width.div_ceil(8 * h_max), height.div_ceil(8 * v_max));
    let progressive = marker == 0xC2;
    let components = sampling
        .into_iter()
        .map(|(id, h, v)| {
            let stride = mcus_wide * h;
            Component {
                id,
                h,
                v,
                blocks_wide: (width * h).div_ceil(h_max).div_ceil(8),
                blocks_high: (height * v).div_ceil(v_max).div_ceil(8),
                stride,
                // A progressive frame's refinement scans need it; at most a megabyte per component at the pixel cap.
                nonzero: if progressive { vec![0; (stride * mcus_high * v) as usize] } else { Vec::new() },
            }
        })
        .collect();
    Ok(Frame { progressive, components, mcus_wide, mcus_high })
}

/// Reads a DHT segment's tables into `dc` and `ac`.
fn tables(
    name: &str,
    mut payload: &[u8],
    dc: &mut [Option<Huffman>; 4],
    ac: &mut [Option<Huffman>; 4],
) -> Result<(), IoError> {
    while let [class_id, ref rest @ ..] = *payload {
        let (class, id) = (class_id >> 4, usize::from(class_id & 0x0F));
        let (Some(counts), true) = (rest.get(..16), class <= 1 && id < 4) else {
            return Err(malformed(name, "a Huffman table's header is wrong"));
        };
        let total: usize = counts.iter().map(|&n| usize::from(n)).sum();
        let symbols = rest.get(16..16 + total).ok_or_else(|| malformed(name, "a Huffman table is cut short"))?;
        let table =
            Huffman::new(counts, symbols).ok_or_else(|| malformed(name, "a Huffman table has too many codes"))?;
        let slot = if class == 0 { &mut dc[id] } else { &mut ac[id] };
        *slot = Some(table);
        payload = &rest[16 + total..];
    }
    Ok(())
}

impl Huffman {
    /// The table for `counts[l - 1]` codes of each length `l` and their `symbols`, or `None` when the counts need more
    /// codes than a length has.
    fn new(counts: &[u8], symbols: &[u8]) -> Option<Self> {
        let mut table = Huffman { max_code: [-1; 17], offset: [0; 17], symbols: [0; 256] };
        table.symbols.get_mut(..symbols.len())?.copy_from_slice(symbols);
        let (mut code, mut index) = (0_i32, 0_i32);
        for (length, &count) in (1..=16).zip(counts) {
            let count = i32::from(count);
            table.offset[length] = index - code;
            if count > 0 {
                code += count;
                if code > 1 << length {
                    return None;
                }
                table.max_code[length] = code - 1;
            }
            index += count;
            code <<= 1;
        }
        Some(table)
    }
}

/// Reads the entropy-coded data of one scan bit by bit, with byte stuffing undone. A marker ends the data.
struct Bits<'a> {
    bytes: &'a [u8],
    at: usize,
    byte: u8,
    left: u8,
}

impl Bits<'_> {
    fn bit(&mut self) -> Result<u32, Stop> {
        if self.left == 0 {
            let byte = *self.bytes.get(self.at).ok_or(Stop::End)?;
            if byte == 0xFF {
                // 0xFF 0x00 is a stuffed 0xFF; anything else after 0xFF is a marker, which ends the data.
                if self.bytes.get(self.at + 1) != Some(&0x00) {
                    return Err(Stop::End);
                }
                self.at += 1;
            }
            self.at += 1;
            self.byte = byte;
            self.left = 8;
        }
        self.left -= 1;
        Ok(u32::from(self.byte >> self.left) & 1)
    }

    fn bits(&mut self, count: u32) -> Result<u32, Stop> {
        let mut value = 0;
        for _ in 0..count {
            value = (value << 1) | self.bit()?;
        }
        Ok(value)
    }

    fn decode(&mut self, table: &Huffman) -> Result<u8, Stop> {
        let mut code = 0_i32;
        for length in 1..=16 {
            code = (code << 1) | i32::from(self.bit()? == 1);
            if code <= table.max_code[length] {
                let index =
                    usize::try_from(table.offset[length] + code).map_err(|_| Stop::Bad("a bad Huffman code"))?;
                return table.symbols.get(index).copied().ok_or(Stop::Bad("a bad Huffman code"));
            }
        }
        Err(Stop::Bad("a code that no Huffman table holds"))
    }

    /// Drops the bits left in the current byte and reads restart marker `n` (0 to 7), which must come next.
    fn restart(&mut self, n: u8) -> Result<(), Stop> {
        self.left = 0;
        if self.bytes.get(self.at) != Some(&0xFF) {
            return Err(Stop::Bad("a restart marker is missing"));
        }
        while self.bytes.get(self.at) == Some(&0xFF) {
            self.at += 1;
        }
        match self.bytes.get(self.at) {
            Some(&m) if m == 0xD0 + n => {
                self.at += 1;
                Ok(())
            }
            Some(0xD0..=0xD7) => Err(Stop::Bad("a restart marker is out of sequence")),
            Some(_) => Err(Stop::Bad("a restart marker is missing")),
            None => Err(Stop::End),
        }
    }
}

/// What a scan codes: the DC first pass, a DC refinement, an AC band's first pass or its refinement; sequential files
/// code whole blocks.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Pass {
    Sequential,
    DcFirst,
    DcRefine,
    AcFirst { start: u32, end: u32 },
    AcRefine { start: u32, end: u32 },
}

/// One scan's header: its components (frame index, DC and AC table) and what it codes.
struct Scan<'t> {
    components: Vec<(usize, Option<&'t Huffman>, Option<&'t Huffman>)>,
    pass: Pass,
}

impl<'t> Scan<'t> {
    fn parse(
        name: &str,
        payload: &[u8],
        frame: &Frame,
        dc: &'t [Option<Huffman>; 4],
        ac: &'t [Option<Huffman>; 4],
    ) -> Result<Self, IoError> {
        let [count, ref rest @ ..] = *payload else {
            return Err(malformed(name, "a scan header is empty"));
        };
        let count = usize::from(count);
        let ([.., ss, se, a], true) = (rest, (1..=4).contains(&count) && rest.len() == count * 2 + 3) else {
            return Err(malformed(name, "a scan header's component list is wrong"));
        };
        let (ss, se, ah, al) = (u32::from(*ss), u32::from(*se), a >> 4, a & 0x0F);
        let pass = match (frame.progressive, ss, ah) {
            (false, 0, 0) if se == 63 && al == 0 => Pass::Sequential,
            (false, ..) => return Err(malformed(name, "a sequential scan that is not one whole-block pass")),
            (true, 0, _) if se != 0 || al > 13 => return Err(malformed(name, "a DC scan that codes AC too")),
            (true, 0, 0) => Pass::DcFirst,
            (true, 0, _) => Pass::DcRefine,
            (true, ..) if se < ss || se > 63 || count != 1 || al > 13 => {
                return Err(malformed(name, "an AC scan with a wrong band or more than one component"));
            }
            (true, _, 0) => Pass::AcFirst { start: ss, end: se },
            (true, ..) => Pass::AcRefine { start: ss, end: se },
        };
        let mut components = Vec::with_capacity(count);
        for &[id, selectors] in rest[..count * 2].as_chunks::<2>().0 {
            let index = frame
                .components
                .iter()
                .position(|c| c.id == id)
                .ok_or_else(|| malformed(name, "a scan names a component the frame doesn't have"))?;
            if components.iter().any(|&(i, _, _)| i == index) {
                return Err(malformed(name, "a scan names a component twice"));
            }
            let dc_table = dc.get(usize::from(selectors >> 4)).and_then(Option::as_ref);
            let ac_table = ac.get(usize::from(selectors & 0x0F)).and_then(Option::as_ref);
            let missing = match pass {
                Pass::Sequential => dc_table.is_none() || ac_table.is_none(),
                Pass::DcFirst => dc_table.is_none(),
                Pass::DcRefine => false,
                Pass::AcFirst { .. } | Pass::AcRefine { .. } => ac_table.is_none(),
            };
            if missing {
                return Err(malformed(name, "a scan uses a Huffman table that isn't defined"));
            }
            components.push((index, dc_table, ac_table));
        }
        Ok(Scan { components, pass })
    }

    /// Walks the scan's data from `at` and returns the offset of the marker after it.
    fn walk(
        &self,
        name: &str,
        bytes: &[u8],
        at: usize,
        frame: &mut Frame,
        interval: u32,
        number: u64,
    ) -> Result<usize, IoError> {
        let mut bits = Bits { bytes, at, byte: 0, left: 0 };
        let (single, first) = (self.components.len() == 1, self.components[0].0);
        let (wide, high) = if single {
            let c = &frame.components[first];
            (c.blocks_wide, c.blocks_high)
        } else {
            (frame.mcus_wide, frame.mcus_high)
        };
        let total = u64::from(wide) * u64::from(high);
        let mut eob_run = 0_u32;
        let mut next_restart = 0_u8;
        let mut mcu = 0_u64;
        let result = (|| {
            for (row, column) in (0..high).flat_map(|row| (0..wide).map(move |column| (row, column))) {
                if interval > 0 && mcu > 0 && mcu.is_multiple_of(u64::from(interval)) {
                    bits.restart(next_restart)?;
                    next_restart = (next_restart + 1) % 8;
                    eob_run = 0;
                }
                for &(index, dc, ac) in &self.components {
                    let c = &mut frame.components[index];
                    let (h, v) = if single { (1, 1) } else { (c.h, c.v) };
                    for y in 0..v {
                        for x in 0..h {
                            let block = ((row * v + y) * c.stride + column * h + x) as usize;
                            self.block(&mut bits, dc, ac, c.nonzero.get_mut(block), &mut eob_run)?;
                        }
                    }
                }
                mcu += 1;
            }
            Ok(())
        })();
        match result {
            Ok(()) => {}
            Err(Stop::End) => {
                return Err(malformed(
                    name,
                    &format!("scan {number}'s data ends early: {mcu} of its {total} MCUs are complete"),
                ));
            }
            Err(Stop::Bad(what)) => {
                return Err(malformed(name, &format!("scan {number}, after {mcu} complete MCUs: {what}")));
            }
        }
        // Past the scan's last bits: padding, and any restart marker an encoder wrote after the last interval.
        let mut at = bits.at;
        loop {
            match (bytes.get(at), bytes.get(at + 1)) {
                (Some(0xFF), Some(0xD0..=0xD7 | 0x00)) => at += 2,
                (Some(0xFF), Some(_)) => return Ok(at),
                (None, _) | (Some(0xFF), None) => {
                    return Err(malformed(name, "the file ends before its end-of-image marker"));
                }
                (Some(_), _) => at += 1,
            }
        }
    }

    /// Reads one block of this scan's pass. `nonzero` is the block's bit set in a progressive frame.
    fn block(
        &self,
        bits: &mut Bits<'_>,
        dc: Option<&Huffman>,
        ac: Option<&Huffman>,
        nonzero: Option<&mut u64>,
        eob_run: &mut u32,
    ) -> Result<(), Stop> {
        match self.pass {
            Pass::Sequential => {
                dc_value(bits, table(dc)?)?;
                let ac = table(ac)?;
                let mut k = 1;
                while k < 64 {
                    let rs = bits.decode(ac)?;
                    let (run, size) = (u32::from(rs >> 4), u32::from(rs & 0x0F));
                    if size == 0 {
                        if run != 15 {
                            break;
                        }
                        k += 16;
                        continue;
                    }
                    k += run;
                    if k > 63 {
                        return Err(Stop::Bad("a run of zeros goes past the end of a block"));
                    }
                    bits.bits(size)?;
                    k += 1;
                }
            }
            Pass::DcFirst => dc_value(bits, table(dc)?)?,
            Pass::DcRefine => {
                bits.bit()?;
            }
            Pass::AcFirst { start, end } => {
                ac_first(
                    bits,
                    table(ac)?,
                    nonzero.ok_or(Stop::Bad("a block outside the frame"))?,
                    eob_run,
                    start,
                    end,
                )?;
            }
            Pass::AcRefine { start, end } => {
                ac_refine(bits, ac, nonzero.ok_or(Stop::Bad("a block outside the frame"))?, eob_run, start, end)?;
            }
        }
        Ok(())
    }
}

/// One block of an AC band's first pass: run-length coded values, or one block of an end-of-band run.
fn ac_first(
    bits: &mut Bits<'_>,
    ac: &Huffman,
    nonzero: &mut u64,
    eob_run: &mut u32,
    start: u32,
    end: u32,
) -> Result<(), Stop> {
    if *eob_run > 0 {
        *eob_run -= 1;
        return Ok(());
    }
    let mut k = start;
    while k <= end {
        let rs = bits.decode(ac)?;
        let (run, size) = (u32::from(rs >> 4), u32::from(rs & 0x0F));
        if size == 0 {
            if run < 15 {
                *eob_run = (1 << run) - 1 + bits.bits(run)?;
                break;
            }
            k += 16;
            continue;
        }
        k += run;
        if k > end {
            return Err(Stop::Bad("a run of zeros goes past the end of the band"));
        }
        bits.bits(size)?;
        *nonzero |= 1 << k;
        k += 1;
    }
    Ok(())
}

/// One block of an AC band's refinement: a correction bit for each coefficient that is already non-zero, and new
/// coefficients of one step, or the corrections alone inside an end-of-band run.
fn ac_refine(
    bits: &mut Bits<'_>,
    ac: Option<&Huffman>,
    nonzero: &mut u64,
    eob_run: &mut u32,
    start: u32,
    end: u32,
) -> Result<(), Stop> {
    let mut k = start;
    if *eob_run == 0 {
        let ac = table(ac)?;
        while k <= end {
            let rs = bits.decode(ac)?;
            let (mut run, size) = (i32::from(rs >> 4), rs & 0x0F);
            if size == 0 && run < 15 {
                *eob_run = (1 << run) + bits.bits(run.unsigned_abs())?;
                break;
            }
            if size > 1 {
                return Err(Stop::Bad("a refinement adds a coefficient larger than one step"));
            }
            if size == 1 {
                bits.bit()?;
            }
            // Past `run` coefficients that are still zero, reading a correction bit for each non-zero one on the way;
            // a new coefficient lands on the zero after them.
            while k <= end {
                if *nonzero & (1 << k) != 0 {
                    bits.bit()?;
                } else {
                    if run == 0 {
                        break;
                    }
                    run -= 1;
                }
                k += 1;
            }
            if size == 1 {
                if k > end {
                    return Err(Stop::Bad("a run of zeros goes past the end of the band"));
                }
                *nonzero |= 1 << k;
            }
            k += 1;
        }
    }
    if *eob_run > 0 {
        // The rest of the band carries only correction bits, one per coefficient that is non-zero.
        let band = if k > end { 0 } else { (u64::MAX >> (63 - end)) & (u64::MAX << k) };
        bits.bits((*nonzero & band).count_ones())?;
        *eob_run -= 1;
    }
    Ok(())
}

/// The table a pass needs; [`Scan::parse`] has checked it is there.
fn table(t: Option<&Huffman>) -> Result<&Huffman, Stop> {
    t.ok_or(Stop::Bad("a Huffman table is missing"))
}

/// Reads a DC difference: its size category, then that many bits.
fn dc_value(bits: &mut Bits<'_>, table: &Huffman) -> Result<(), Stop> {
    let size = bits.decode(table)?;
    if size > 11 {
        return Err(Stop::Bad("a DC difference larger than 8-bit samples allow"));
    }
    bits.bits(u32::from(size))?;
    Ok(())
}

fn malformed(name: &str, what: &str) -> IoError {
    IoError::Malformed { message: format!("image '{name}': {what}"), at: None }
}

fn unsupported(name: &str, what: &str) -> IoError {
    IoError::Unsupported { what: format!("image '{name}': {what}"), hint: Some("export the texture as PNG".to_owned()) }
}
