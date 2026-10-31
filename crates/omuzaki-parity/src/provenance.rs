//! Recreate, never copy: the check that no reference pixel went into what is scored.
//!
//! A reference is only looked at and measured. The check refuses a score when:
//! - the document uses a reference asset anywhere outside `parity/` (as a texture, a layer, a part's image);
//! - an asset the document uses was generated from a reference (its provenance lists the reference as an input),
//!   which also catches the overlays the parity ops store, since they hold reference pixels;
//! - an image the document uses (on its own, or embedded in a model file), or an image file the caller names as
//!   built from, is a near-copy of a reference frame or of a tile of one (a 256-bit difference hash, at four grid
//!   sizes, within 24 bits).
//!
//! It also counts tiles of the render that match the reference to the byte where the reference has texture, as a
//! warning: on their own they are what parity looks like, so they do not refuse a score, but a high share before the
//! rest of the frame is close is a sign of a paste.

use std::collections::BTreeSet;

use omuzaki_store_api::{ContentHash, Source, Store};
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

use super::raster::LabImage;
use crate::image::{Rgba8, decode};

/// The hash side: 17 x 16 grey cells give 256 bits.
const HASH_SIDE: usize = 16;
/// At most this many of 256 bits apart counts as a copy.
pub const NEAR_COPY_BITS: u32 = 24;
/// The grids a frame is cut into for the tile hashes: whole, 2 x 2, 3 x 3, 4 x 4.
const GRIDS: [usize; 4] = [1, 2, 3, 4];
/// A tile flatter than this (standard deviation of its grey, 0..255) hashes to noise and is skipped.
const FLAT: f32 = 4.0;
/// The most embedded images read from one asset.
const MAX_EMBEDDED: usize = 64;

/// A 256-bit difference hash.
pub type Hash256 = [u64; 4];

/// A hashed tile of a reference frame.
#[derive(Clone, Debug)]
pub struct TileHash {
    /// Which frame and tile, for the message.
    pub label: String,
    /// The hash.
    pub hash: Hash256,
}

/// The grey (0..255) of straight RGBA8.
#[allow(clippy::cast_precision_loss)] // bytes
fn grey(image: &Rgba8) -> Vec<f32> {
    image
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| 0.299 * f32::from(p[0]) + 0.587 * f32::from(p[1]) + 0.114 * f32::from(p[2]))
        .collect()
}

/// The difference hash of the `[x, y, w, h]` part of a grey image `width` wide; `None` for a flat part.
#[allow(clippy::cast_precision_loss)] // cell counts
fn dhash(g: &[f32], width: usize, rect: [usize; 4]) -> Option<Hash256> {
    let [x0, y0, w, h] = rect;
    if w < 4 || h < 4 {
        return None;
    }
    // Area means on a 17 x 16 grid.
    let (cols, rows) = (HASH_SIDE + 1, HASH_SIDE);
    let mut cells = vec![0.0f32; cols * rows];
    let mut counts = vec![0u32; cols * rows];
    let (mut sum, mut sum_sq, mut n) = (0.0f64, 0.0f64, 0.0f64);
    for y in 0..h {
        let r = y * rows / h;
        for x in 0..w {
            let c = x * cols / w;
            let v = g[(y0 + y) * width + x0 + x];
            cells[r * cols + c] += v;
            counts[r * cols + c] += 1;
            sum += f64::from(v);
            sum_sq += f64::from(v) * f64::from(v);
            n += 1.0;
        }
    }
    let var = sum_sq / n - (sum / n).powi(2);
    if var.max(0.0).sqrt() < f64::from(FLAT) {
        return None;
    }
    for (c, k) in cells.iter_mut().zip(&counts) {
        *c /= (*k).max(1) as f32;
    }
    let mut out = [0u64; 4];
    let mut bit = 0;
    for r in 0..rows {
        for c in 0..HASH_SIDE {
            if cells[r * cols + c + 1] > cells[r * cols + c] {
                out[bit / 64] |= 1 << (bit % 64);
            }
            bit += 1;
        }
    }
    Some(out)
}

fn distance(a: &Hash256, b: &Hash256) -> u32 {
    a.iter().zip(b).map(|(x, y)| (x ^ y).count_ones()).sum()
}

/// The tile hashes of an image at every grid size, labelled `<name> tile r,c of n x n`.
#[must_use]
pub fn tile_hashes(image: &Rgba8, name: &str) -> Vec<TileHash> {
    let (w, h) = (image.width as usize, image.height as usize);
    let g = grey(image);
    let mut out = Vec::new();
    for n in GRIDS {
        for r in 0..n {
            for c in 0..n {
                let rect = [c * w / n, r * h / n, (c + 1) * w / n - c * w / n, (r + 1) * h / n - r * h / n];
                if let Some(hash) = dhash(&g, w, rect) {
                    let label = if n == 1 { name.to_owned() } else { format!("{name} tile {r},{c} of {n}x{n}") };
                    out.push(TileHash { label, hash });
                }
            }
        }
    }
    out
}

/// The reference tile a candidate image (whole or a tile of it) is a near-copy of, as a message.
#[must_use]
pub fn near_copy(candidate: &Rgba8, name: &str, reference: &[TileHash]) -> Option<String> {
    for tile in tile_hashes(candidate, name) {
        for r in reference {
            let bits = distance(&tile.hash, &r.hash);
            if bits <= NEAR_COPY_BITS {
                return Some(format!(
                    "{} is a near-copy of the reference's {} ({bits} of 256 bits apart)",
                    tile.label, r.label
                ));
            }
        }
    }
    None
}

/// The images inside a file's bytes: the file itself when it is a PNG or JPEG, else every embedded PNG (to its IEND)
/// and JPEG (to its EOI marker) that decodes, at most [`MAX_EMBEDDED`].
#[must_use]
pub fn embedded_images(bytes: &[u8]) -> Vec<Rgba8> {
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";
    const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF];
    let hash = ContentHash::of(bytes);
    if bytes.starts_with(PNG) || bytes.starts_with(JPEG) {
        return decode(bytes, hash, "asset").into_iter().collect();
    }
    let mut out = Vec::new();
    let mut at = 0;
    while at + 8 < bytes.len() && out.len() < MAX_EMBEDDED {
        let rest = &bytes[at..];
        let next_png = find(rest, PNG);
        let next_jpeg = find(rest, JPEG);
        let (start, is_png) = match (next_png, next_jpeg) {
            (Some(p), Some(j)) if p <= j => (p, true),
            (Some(p), None) => (p, true),
            (_, Some(j)) => (j, false),
            (None, None) => break,
        };
        let begin = at + start;
        let end = if is_png {
            find(&bytes[begin..], b"IEND").map(|e| begin + e + 8)
        } else {
            find(&bytes[begin + 2..], &[0xFF, 0xD9]).map(|e| begin + 2 + e + 2)
        };
        let Some(end) = end.map(|e| e.min(bytes.len())) else { break };
        if let Ok(image) = decode(&bytes[begin..end], hash, "asset") {
            out.push(image);
        }
        at = end.max(begin + 1);
    }
    out
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Every 64-hex-digit run in the document's strings, with the path it was found at, outside `parity/`.
#[must_use]
pub fn hashes_in(doc: &Value) -> Vec<(String, ContentHash)> {
    let mut out = Vec::new();
    walk(doc, "", &mut out);
    out
}

fn walk(v: &Value, path: &str, out: &mut Vec<(String, ContentHash)>) {
    match v {
        Value::Object(map) => {
            for (k, child) in map {
                if path.is_empty() && k == "parity" {
                    continue;
                }
                let p = if path.is_empty() { k.clone() } else { format!("{path}/{k}") };
                walk(child, &p, out);
            }
        }
        Value::Array(items) => {
            for (i, child) in items.iter().enumerate() {
                walk(child, &format!("{path}/{i}"), out);
            }
        }
        Value::String(s) => {
            let b = s.as_bytes();
            let mut i = 0;
            while i < b.len() {
                if b[i].is_ascii_hexdigit() {
                    let start = i;
                    while i < b.len() && b[i].is_ascii_hexdigit() {
                        i += 1;
                    }
                    if i - start == 64
                        && let Ok(h) = s[start..i].to_ascii_lowercase().parse()
                    {
                        out.push((path.to_owned(), h));
                    }
                } else {
                    i += 1;
                }
            }
        }
        _ => {}
    }
}

/// What the check found.
#[derive(Clone, Debug, Default, PartialEq, Serialize, JsonSchema)]
pub struct Provenance {
    /// True when nothing refuses the score.
    pub clean: bool,
    /// What refuses it, one line each.
    pub violations: Vec<String>,
    /// The assets of the document that were checked.
    pub assets_checked: usize,
    /// The images (on their own or embedded) that were hashed.
    pub images_checked: usize,
    /// The share of the render's textured tiles that match the reference to the byte (a warning, not a refusal).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exact_textured_tiles: Option<f64>,
    /// The rule, for the record.
    pub rule: &'static str,
}

/// The rule, as the reports state it.
pub const RULE: &str = "Recreate, never copy: the reference is only measured. No pixel of it may go into the render \
                        (no texture, layer, part or model cut, traced, warped or fitted from it). Draw and build with \
                        Omuzaki's own tools.";

/// Checks the document and the named image files against the reference frames (`reference_hashes` are their store
/// hashes, when they are assets; `reference_tiles` their tile hashes).
#[must_use]
pub fn check(
    doc: &Value,
    store: Option<&dyn Store>,
    reference_hashes: &BTreeSet<ContentHash>,
    reference_tiles: &[TileHash],
    built_from: &[(String, Rgba8)],
) -> Provenance {
    let mut p = Provenance { rule: RULE, ..Provenance::default() };
    let mut seen = BTreeSet::new();
    for (path, hash) in hashes_in(doc) {
        if reference_hashes.contains(&hash) {
            p.violations.push(format!("the document uses the reference image {hash} at {path}"));
            continue;
        }
        if !seen.insert(hash) {
            continue;
        }
        let Some(store) = store else { continue };
        if !store.contains(&hash) {
            continue;
        }
        p.assets_checked += 1;
        if let Ok(prov) = store.provenance(&hash)
            && let Source::Generated { inputs, tool, .. } = &prov.source
            && let Some(input) = inputs.iter().find(|i| reference_hashes.contains(i))
        {
            p.violations.push(format!("{path} uses asset {hash}, which {tool} made from the reference image {input}"));
            continue;
        }
        if store.size(&hash).is_ok_and(|s| s <= 64 << 20)
            && let Ok(bytes) = store.get(&hash)
        {
            for (k, image) in embedded_images(&bytes).iter().enumerate() {
                p.images_checked += 1;
                if let Some(m) = near_copy(image, &format!("{path} (asset {hash}, image {k})"), reference_tiles) {
                    p.violations.push(m);
                    break;
                }
            }
        }
    }
    for (name, image) in built_from {
        p.images_checked += 1;
        if let Some(m) = near_copy(image, name, reference_tiles) {
            p.violations.push(m);
        }
    }
    p.clean = p.violations.is_empty();
    p
}

/// The share of the render's 16-pixel tiles, among those where the reference has texture, that match the reference to
/// within one byte on 99% of their pixels. `None` when the reference has no textured tile.
#[must_use]
#[allow(clippy::cast_precision_loss)] // tile counts
pub fn exact_textured_tiles(ours: &LabImage, reference: &LabImage) -> Option<f64> {
    const SIDE: usize = 16;
    let (w, h) = (reference.w, reference.h);
    let (mut textured, mut exact) = (0usize, 0usize);
    for ty in (0..h.saturating_sub(SIDE - 1)).step_by(SIDE) {
        for tx in (0..w.saturating_sub(SIDE - 1)).step_by(SIDE) {
            let (mut s, mut s2, mut same) = (0.0f64, 0.0f64, 0usize);
            for y in ty..ty + SIDE {
                for x in tx..tx + SIDE {
                    let i = y * w + x;
                    let v = f64::from(reference.l.v[i]);
                    s += v;
                    s2 += v * v;
                    let (p, q) = (ours.rgb[i], reference.rgb[i]);
                    same += usize::from((0..3).all(|k| p[k].abs_diff(q[k]) <= 1));
                }
            }
            let n = (SIDE * SIDE) as f64;
            if (s2 / n - (s / n).powi(2)).max(0.0).sqrt() < 3.0 {
                continue;
            }
            textured += 1;
            exact += usize::from(same as f64 >= 0.99 * n);
        }
    }
    (textured > 0).then(|| exact as f64 / textured as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(w: u32, h: u32, seed: u32) -> Rgba8 {
        let mut s = seed;
        let pixels = (0..w * h)
            .flat_map(|_| {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let v = (s >> 24) as u8;
                [v, v.wrapping_mul(3), v / 2, 255]
            })
            .collect();
        Rgba8 { width: w, height: h, pixels, hash: ContentHash::of(&seed.to_le_bytes()) }
    }

    fn crop(image: &Rgba8, x: u32, y: u32, w: u32, h: u32) -> Rgba8 {
        let mut pixels = Vec::new();
        for row in y..y + h {
            let start = ((row * image.width + x) * 4) as usize;
            pixels.extend_from_slice(&image.pixels[start..start + (w * 4) as usize]);
        }
        Rgba8 { width: w, height: h, pixels, hash: ContentHash::of(b"crop") }
    }

    #[test]
    fn a_frame_and_a_quarter_cut_from_it_are_copies_and_other_noise_is_not() {
        let frame = noise(64, 64, 7);
        let tiles = tile_hashes(&frame, "frame 0");
        assert!(near_copy(&frame, "copy", &tiles).is_some());
        let quarter = crop(&frame, 32, 0, 32, 32);
        let found = near_copy(&quarter, "texture", &tiles).unwrap();
        assert!(found.contains("tile 0,1 of 2x2"), "{found}");
        assert!(near_copy(&noise(64, 64, 99), "other", &tiles).is_none());
    }

    #[test]
    fn hashes_are_found_in_strings_outside_parity() {
        let h = ContentHash::of(b"ref").to_string();
        let doc = serde_json::json!({
            "characters": { "a": { "texture": format!("omuzaki://asset/{h}") } },
            "parity": { "targets": { "t": { "frames": [h.clone()] } } },
        });
        let found = hashes_in(&doc);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, "characters/a/texture");
    }
}
