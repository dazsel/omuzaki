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
    /// The wood.
    pub colour: Colour,
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
                                   full, the spines' colours and the wood.";

    fn validate(p: &ShelfParams) -> Result<(), String> {
        between("width", p.width, 0.4, 2.0)?;
        between("height", p.height, 0.6, 2.6)?;
        between("depth", p.depth, 0.2, 0.5)?;
        between("books", p.books, 0.0, 1.0)?;
        if !(1..=8).contains(&p.shelves) {
            return Err(format!("shelves is 1 to 8, got {}", p.shelves));
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
            fill_board(&mut books, &mut rng, &colours, p.books, (-w + side * 0.5, w - side * 0.5), y, gap - board, (back, d));
        }
        let mut parts = vec![(frame, painted(darker(wood, 0.6)))];
        if !books.is_empty() {
            parts.push((books, painted(Colour::hex(0x0022_1a18))));
        }
        Ok(output(parts))
    }
}

register_prop_kind!(Shelf);

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
