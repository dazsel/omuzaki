//! `shelf`: a bookcase, its boards filled with books. The books are seeded, so a scene draws the same shelf every time
//! and a new seed rearranges it.

use omuzaki_core::glam::Vec3;
use omuzaki_render_api::Colour;
use omuzaki_stage_api::{PropKind, PropOutput, register_prop_kind};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::furniture::WOOD;
use super::shape::{At, Shape, darker, mix, painted};
use super::{Seeded, between, output};

/// A bookcase.
pub struct Shelf;

/// The bookcase's parameters.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ShelfParams {
    /// Metres, 0.4 to 2.0.
    pub width: f32,
    /// Metres, 0.6 to 2.6.
    pub height: f32,
    /// Metres, 0.2 to 0.5.
    pub depth: f32,
    /// Boards, counting the bottom, 1 to 8.
    pub shelves: u8,
    /// How full the boards are, 0 (empty) to 1.
    pub books: f32,
    /// The spines' colours, picked at random per book; empty for the default leather and cloth.
    pub book_colours: Vec<Colour>,
    /// Picks the books.
    pub seed: u32,
    /// The wood (or its paint).
    pub colour: Colour,
    /// Gilded edging: a strip down the front of each side, along the boards' fronts, a crown on top and the doors'
    /// emblems; none for plain wood.
    pub trim: Option<Colour>,
    /// Boards closed by doors, counted from the bottom (a cabinet under open shelves), 0 to the number of boards.
    pub doors: u8,
    /// Square posts standing proud of each side, full height, with a capital and a base (in the trim, when there is
    /// one): a carved case rather than a plain one.
    pub pilasters: bool,
}

impl Default for ShelfParams {
    fn default() -> Self {
        Self {
            width: 0.9,
            height: 1.8,
            depth: 0.32,
            shelves: 5,
            books: 0.8,
            book_colours: Vec::new(),
            seed: 1,
            colour: WOOD,
            trim: None,
            doors: 0,
            pilasters: false,
        }
    }
}

/// Leather and cloth bindings: deep reds, greens, blues, browns and a few pale ones.
const BINDINGS: [u32; 12] = [
    0x008a_2c2c, 0x00a8_4a32, 0x0035_5e3b, 0x0024_4a6e, 0x005a_3a2a, 0x00c9_b48a, 0x006e_2a4e, 0x003c_3c48,
    0x00b8_8a3a, 0x0084_9a7a, 0x00d8_cfc0, 0x0046_2a20,
];

impl PropKind for Shelf {
    type Params = ShelfParams;
    const ID: &'static str = "shelf";
    const SUMMARY: &'static str = "A bookcase open toward +Z, its boards filled with seeded books. Size, boards, how \
                                   full, the spines' colours, the wood or paint, gilded trim and a cabinet of doors \
                                   under the open boards.";

    fn validate(p: &ShelfParams) -> Result<(), String> {
        between("width", p.width, 0.4, 2.0)?;
        between("height", p.height, 0.6, 2.6)?;
        between("depth", p.depth, 0.2, 0.5)?;
        between("books", p.books, 0.0, 1.0)?;
        if !(1..=8).contains(&p.shelves) {
            return Err(format!("shelves is 1 to 8, got {}", p.shelves));
        }
        if p.doors > p.shelves {
            return Err(format!("doors is 0 to the {} boards, got {}", p.shelves, p.doors));
        }
        if p.book_colours.len() > 16 {
            return Err(format!("book_colours holds at most 16, got {}", p.book_colours.len()));
        }
        Ok(())
    }

    fn build(p: &ShelfParams) -> Result<PropOutput, String> {
        let (w, h, d) = (p.width * 0.5, p.height, p.depth);
        let (side, board, back) = (0.022, 0.022, 0.008);
        let wood = p.colour;
        let inside = darker(wood, 0.28);
        let mut frame = Shape::new();
        for x in [-1.0, 1.0] {
            frame.block(Vec3::new(x * w - side * 0.5, 0.0, 0.0), Vec3::new(x * w + side * 0.5, h, d), wood);
        }
        frame.block(Vec3::new(-w, h - board, 0.0), Vec3::new(w, h, d), wood);
        frame.block(Vec3::new(-w + side * 0.5, 0.0, back), Vec3::new(w - side * 0.5, 0.07, d - 0.01), darker(wood, 0.1));
        frame.block(Vec3::new(-w + side * 0.5, 0.07, 0.0), Vec3::new(w - side * 0.5, h - board, back), inside);
        let gap = (h - 0.07 - board) / f32::from(p.shelves);
        let mut books = Shape::new();
        let colours: Vec<Colour> =
            if p.book_colours.is_empty() { BINDINGS.map(Colour::hex).to_vec() } else { p.book_colours.clone() };
        let mut rng = Seeded::new(p.seed);
        for i in 0..p.shelves {
            let y = 0.07 + gap * f32::from(i);
            if i > 0 {
                frame.block(Vec3::new(-w + side * 0.5, y - board, back), Vec3::new(w - side * 0.5, y, d), wood);
            }
            if i < p.doors {
                door_bay(&mut frame, wood, p.trim, (-w + side * 0.5, w - side * 0.5), (y, y + gap - board), d);
            } else {
                fill_board(&mut books, &mut rng, &colours, p.books, (-w + side * 0.5, w - side * 0.5), y, gap - board, (back, d));
            }
        }
        let mut gilt = Shape::new();
        if let Some(trim) = p.trim {
            // Down the front of each side, a lip on every board's front edge, and a crown over the top.
            for x in [-1.0, 1.0] {
                gilt.block(Vec3::new(x * w - side * 0.35, 0.0, d), Vec3::new(x * w + side * 0.35, h, d + 0.006), trim);
            }
            for i in p.doors.max(1)..p.shelves {
                let y = 0.07 + gap * f32::from(i);
                gilt.block(Vec3::new(-w + side * 0.5, y - board * 0.6, d), Vec3::new(w - side * 0.5, y - board * 0.2, d + 0.005), trim);
            }
            frame.block(Vec3::new(-w - 0.03, h, -0.01), Vec3::new(w + 0.03, h + 0.07, d + 0.035), darker(wood, 0.08));
            gilt.block(Vec3::new(-w - 0.032, h + 0.018, d + 0.035), Vec3::new(w + 0.032, h + 0.032, d + 0.04), trim);
            gilt.block(Vec3::new(-w - 0.035, h + 0.07, -0.012), Vec3::new(w + 0.035, h + 0.085, d + 0.045), trim);
        }
        if p.pilasters {
            let post = 0.035_f32.max(p.width * 0.04);
            let cap = p.trim.unwrap_or_else(|| darker(wood, 0.2));
            for x in [-1.0, 1.0] {
                let cx = x * (w + post * 0.2);
                frame.block(Vec3::new(cx - post, 0.0, d - post * 0.6), Vec3::new(cx + post, h, d + post * 0.9), mix(wood, Colour::WHITE, 0.05));
                for (y0, y1) in [(0.0, 0.08), (h - 0.1, h - 0.04)] {
                    gilt.block(Vec3::new(cx - post * 1.25, y0, d - post * 0.7), Vec3::new(cx + post * 1.25, y1, d + post * 1.15), cap);
                }
            }
        }
        let mut parts = vec![(frame, painted(darker(wood, 0.6)))];
        if !gilt.is_empty() {
            parts.push((gilt, painted(darker(p.trim.unwrap_or(wood), 0.55))));
        }
        if !books.is_empty() {
            parts.push((books, painted(Colour::hex(0x0022_1a18))));
        }
        Ok(output(parts))
    }
}

register_prop_kind!(Shelf);

/// Closes the bay between `x.0` and `x.1` from `y.0` to `y.1` with doors on the front (`z = d`): one door, or two
/// on a bay wider than 0.7 m, each with a raised panel and, with `trim`, a gilded diamond on it.
fn door_bay(frame: &mut Shape, wood: Colour, trim: Option<Colour>, x: (f32, f32), y: (f32, f32), d: f32) {
    let span = x.1 - x.0;
    let count: u8 = if span > 0.7 { 2 } else { 1 };
    let each = span / f32::from(count);
    for k in 0..count {
        let (x0, x1) = (x.0 + each * f32::from(k) + 0.004, x.0 + each * f32::from(k + 1) - 0.004);
        let (y0, y1) = (y.0 + 0.004, y.1 - 0.004);
        frame.block(Vec3::new(x0, y0, d - 0.018), Vec3::new(x1, y1, d), wood);
        let inset = (each * 0.14).min(0.06);
        let (px0, px1, py0, py1) = (x0 + inset, x1 - inset, y0 + inset, y1 - inset);
        if px1 > px0 && py1 > py0 {
            frame.block(Vec3::new(px0, py0, d), Vec3::new(px1, py1, d + 0.008), mix(wood, Colour::WHITE, 0.06));
            if let Some(trim) = trim {
                let centre = Vec3::new(f32::midpoint(px0, px1), f32::midpoint(py0, py1), d + 0.011);
                let size = ((px1 - px0).min(py1 - py0) * 0.1).max(0.008);
                frame.bevelled(At::pos(centre).roll(45.0), Vec3::new(size, size, 0.003), 0.001, trim);
            }
        }
    }
}

/// Stands books on one board from `x.0` to `x.1` at height `y` under a clearance of `room`: runs of upright books with
/// gaps, the odd one leaning on its neighbour, and now and then a short stack lying flat.
#[allow(clippy::too_many_arguments)] // a board is these numbers
fn fill_board(
    books: &mut Shape,
    rng: &mut Seeded,
    colours: &[Colour],
    fullness: f32,
    x: (f32, f32),
    y: f32,
    room: f32,
    z: (f32, f32),
) {
    if fullness <= 0.0 || room < 0.08 {
        return;
    }
    let span = x.1 - x.0;
    let used = span * fullness;
    // Where the books start: a gap at one end or the other.
    let mut at = x.0 + rng.range(0.0, (span - used).max(0.0));
    let end = at + used;
    let pick = |rng: &mut Seeded| {
        let c = colours[rng.index(colours.len())];
        mix(c, Colour::BLACK, rng.range(0.0, 0.18))
    };
    while at < end - 0.02 {
        let roll = rng.next();
        if roll < 0.06 && end - at > 0.2 {
            // A stack of two to four books lying flat, each pushed up to 1 cm along: never back past where it starts.
            let (len, depth) = (rng.range(0.16, 0.22), rng.range(0.13, (z.1 - z.0) * 0.85).max(0.12));
            if at + len + 0.01 > x.1 {
                break;
            }
            let mut top = y;
            for _ in 0..2 + rng.index(3) {
                let t = rng.range(0.025, 0.045);
                let c = pick(rng);
                books.bevelled(
                    At::xyz(at + len * 0.5 + rng.range(0.0, 0.01), top + t * 0.5, z.0 + 0.01 + depth * 0.5),
                    Vec3::new(len * 0.5, t * 0.5, depth * 0.5),
                    0.003,
                    c,
                );
                top += t;
            }
            at += len + 0.015;
            continue;
        }
        let thick = rng.range(0.018, 0.05);
        // Nothing stands past the side of the case.
        if at + thick > x.1 {
            break;
        }
        let tall = (room * rng.range(0.62, 0.92)).min(room - 0.01).max(0.06);
        let depth = rng.range(0.6, 0.9) * (z.1 - z.0);
        let c = pick(rng);
        if roll > 0.95 && at > x.0 + 0.05 {
            // Leaning left on the book before it, turned about its own lower left corner, which stands on the board
            // far enough along that its top meets that book.
            let lean = rng.range(10.0_f32, 22.0).to_radians();
            let (sin, cos) = lean.sin_cos();
            let pivot = at + tall * sin;
            if pivot + thick * cos > x.1 {
                break;
            }
            let centre = Vec3::new(pivot + thick * 0.5 * cos - tall * 0.5 * sin, y + thick * 0.5 * sin + tall * 0.5 * cos, z.0 + 0.01 + depth * 0.5);
            books.bevelled(
                At::pos(centre).roll(lean.to_degrees()),
                Vec3::new(thick * 0.5, tall * 0.5, depth * 0.5),
                0.003,
                c,
            );
            at = pivot + thick * cos + 0.01;
            continue;
        }
        books.bevelled(
            At::xyz(at + thick * 0.5, y + tall * 0.5, z.0 + 0.01 + depth * 0.5),
            Vec3::new(thick * 0.5, tall * 0.5, depth * 0.5),
            0.003,
            c,
        );
        at += thick + if rng.next() < 0.12 { rng.range(0.004, 0.02) } else { 0.0008 };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extent(out: &PropOutput) -> (Vec3, Vec3) {
        out.parts.iter().flat_map(|part| part.mesh.positions.iter().copied()).fold(
            (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)),
            |(lo, hi), p| (lo.min(p), hi.max(p)),
        )
    }

    #[test]
    fn trim_adds_a_gilded_part_and_a_crown_and_doors_close_the_lowest_bays() {
        let plain = Shelf::build(&ShelfParams::default()).expect("builds");
        let gilded = ShelfParams { trim: Some(Colour::hex(0x00c0_9058)), doors: 2, ..ShelfParams::default() };
        let built = Shelf::build(&gilded).expect("builds");
        assert_eq!(built.parts.len(), plain.parts.len() + 1, "the trim is its own part");
        assert!(extent(&built).1.y > extent(&plain).1.y + 0.05, "a crown stands over the top");
        // No book stands in the two closed bays: the lowest book is above them.
        let gap = (gilded.height - 0.07 - 0.022) / f32::from(gilded.shelves);
        // The frame, the gilding, then the books.
        let books = &built.parts[2];
        let lowest = books.mesh.positions.iter().map(|p| p.y).fold(f32::INFINITY, f32::min);
        assert!(lowest >= 0.07 + gap * 2.0 - 0.025, "the books start above the doors: {lowest}");
    }

    #[test]
    fn more_doors_than_boards_is_refused() {
        let p = ShelfParams { shelves: 3, doors: 4, ..ShelfParams::default() };
        assert!(Shelf::validate(&p).is_err());
    }
}
