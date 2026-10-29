//! Props on the stage (design D7, D10; ST-03): the kinds this crate registers, the solids they are made of, and the
//! props of a scene resolved, uploaded and drawn.
//!
//! - [`shape`]: boxes, frustums, vessels, ellipsoids, tubes and leaves, closed and painted, appended into one mesh.
//! - The interior kinds (#1166): [`room`] (`room`, `rug`), [`furniture`] (`desk`, `chair`, `cafe-table`, `bed`),
//!   [`shelf`], [`lighting`] (`lamp`, `window`) and [`small`] (`cup`, `plant`). Each is procedural, in metres, toon
//!   with vertex colours, and registers itself.

pub mod furniture;
pub mod lighting;
pub mod room;
pub mod shape;
pub mod shelf;
pub mod small;

use omuzaki_render_api::Material;
use omuzaki_stage_api::{PropOutput, PropPart};

use shape::Shape;

/// `value` is within `lo..=hi`, or the rule naming `field`.
pub(crate) fn between(field: &str, value: f32, lo: f32, hi: f32) -> Result<(), String> {
    if value.is_finite() && (lo..=hi).contains(&value) {
        Ok(())
    } else {
        Err(format!("{field} is {lo} to {hi}, got {value}"))
    }
}

/// The parts of a kind's build, leaving out the shapes nothing was added to.
pub(crate) fn output(parts: Vec<(Shape, Material)>) -> PropOutput {
    let mut out = PropOutput::default();
    out.parts = parts
        .into_iter()
        .filter(|(shape, _)| !shape.is_empty())
        .map(|(shape, material)| PropPart::new(shape.into_mesh(), material))
        .collect();
    out
}

/// A small seeded random sequence (PCG), so a kind's random choices are the same on every machine and every build.
#[derive(Clone, Debug)]
pub(crate) struct Seeded(u32);

impl Seeded {
    pub(crate) fn new(seed: u32) -> Self {
        Self(seed.wrapping_mul(747_796_405).wrapping_add(2_891_336_453))
    }

    /// The next number in `0..1`, 24 bits, exact in f32.
    pub(crate) fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(747_796_405).wrapping_add(2_891_336_453);
        let s = self.0;
        let w = ((s >> ((s >> 28) + 4)) ^ s).wrapping_mul(277_803_737);
        let h = (w >> 22) ^ w;
        #[allow(clippy::cast_precision_loss)] // 24 bits are exact
        let unit = (h >> 8) as f32 / 16_777_216.0;
        unit
    }

    /// A number in `lo..hi`.
    pub(crate) fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.next()
    }

    /// An index below `n` (0 for an empty range).
    pub(crate) fn index(&mut self, n: usize) -> usize {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // n is small
        let i = (self.next() * n as f32) as usize;
        i.min(n.saturating_sub(1))
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_stage_api::{MAX_PROPS, prop_kinds};
    use serde_json::{Value, json};

    use super::shape::tests::assert_solid;

    const KINDS: [&str; 11] =
        ["bed", "cafe-table", "chair", "cup", "desk", "lamp", "plant", "room", "rug", "shelf", "window"];

    /// Each kind's extreme parameters: what the contact sheet shows beside the default.
    fn extremes(kind: &str) -> Vec<Value> {
        match kind {
            "bed" => vec![json!({"width": 0.9, "length": 1.8, "pillows": 0}), json!({"width": 2.0, "length": 2.3, "pillows": 4})],
            "cafe-table" => vec![json!({"radius": 0.25, "height": 1.1}), json!({"radius": 0.6, "height": 0.6})],
            "chair" => vec![json!({"back": 0.0, "cushion": null}), json!({"seat_height": 0.6, "width": 0.6, "back": 0.7})],
            "cup" => vec![json!({"height": 0.05, "radius": 0.07, "handle": false, "drink": null}), json!({"height": 0.16, "radius": 0.025, "saucer": true})],
            "desk" => vec![json!({"width": 0.6, "depth": 0.4, "height": 0.6, "drawers": false}), json!({"width": 2.4, "depth": 1.0, "height": 1.1})],
            "lamp" => vec![json!({"style": "table", "height": 0.3}), json!({"style": "table", "height": 0.8, "on": false}), json!({"height": 1.9, "intensity": 20.0, "range": 15.0})],
            "plant" => vec![json!({"height": 0.2, "leaves": 4}), json!({"height": 2.0, "leaves": 60, "seed": 99})],
            "room" => vec![json!({"width": 2.0, "depth": 2.0, "height": 2.2, "wainscot": null, "planks": false}), json!({"width": 12.0, "depth": 12.0, "height": 5.0, "walls_up": [true, false, true]})],
            "rug" => vec![json!({"width": 0.6, "length": 0.4, "border": null}), json!({"width": 5.0, "length": 5.0})],
            "shelf" => vec![json!({"width": 0.4, "height": 0.6, "depth": 0.2, "shelves": 1, "books": 0.0}), json!({"width": 2.0, "height": 2.6, "depth": 0.5, "shelves": 8, "books": 1.0, "seed": 12})],
            "window" => vec![json!({"width": 0.4, "height": 0.4, "panes": [1, 1], "curtains": null, "daylight": false}), json!({"width": 3.0, "height": 2.6, "panes": [6, 6]})],
            _ => Vec::new(),
        }
    }

    #[test]
    fn the_interior_kinds_are_registered() {
        let ids: Vec<&str> = prop_kinds().iter().map(|k| k.id).filter(|id| KINDS.contains(id)).collect();
        assert_eq!(ids, KINDS);
    }

    /// #1166: each kind builds at its default and extreme parameters within the budget, its meshes valid and painted,
    /// and the solids closed (the plant's leaves and the window's open parts aside: a leaf is a sheet).
    #[test]
    fn every_kind_builds_at_its_default_and_extremes_within_the_budget() {
        let mut worst = (0, "");
        for kind in prop_kinds().into_iter().filter(|k| KINDS.contains(&k.id)) {
            for params in std::iter::once(json!({})).chain(extremes(kind.id)) {
                let checked = kind.check(&params).unwrap_or_else(|e| panic!("{}: {params}: {e}", kind.id));
                let built = kind.build(&checked).unwrap_or_else(|e| panic!("{}: {params}: {e}", kind.id));
                for (i, part) in built.parts.iter().enumerate() {
                    let sheet = kind.id == "plant" && i == 1;
                    assert_solid(&part.mesh, !sheet);
                }
                let triangles = built.triangles();
                eprintln!("{} {params}: {triangles} triangles, {} lights", kind.id, built.lights.len());
                if triangles > worst.0 {
                    worst = (triangles, kind.id);
                }
                assert_eq!(kind.build(&checked).unwrap(), built, "{}: a build is a function of its params", kind.id);
            }
        }
        // The budget the PR states: the largest prop stays under 20,000 triangles, so a room of a dozen props is a
        // fraction of a character.
        assert!(worst.0 < 20_000, "{} builds {} triangles", worst.1, worst.0);
        assert!(MAX_PROPS >= 64);
    }

    #[test]
    fn parameters_out_of_range_are_refused_by_name() {
        for (kind, params, field) in [
            ("desk", json!({"width": 9.0}), "width"),
            ("shelf", json!({"shelves": 0}), "shelves"),
            ("lamp", json!({"style": "table", "height": 1.5}), "height"),
            ("window", json!({"panes": [0, 2]}), "panes[0]"),
            ("plant", json!({"leaves": 2}), "leaves"),
        ] {
            let err = omuzaki_stage_api::prop_kind(kind).unwrap().check(&params).unwrap_err();
            assert!(err.to_string().contains(field), "{kind}: {err}");
        }
    }

    #[test]
    fn a_lit_lamp_and_a_daylit_window_bring_their_lights_and_an_unlit_one_none() {
        let build = |kind: &str, params: Value| omuzaki_stage_api::prop_kind(kind).unwrap().build(&params).unwrap();
        assert_eq!(build("lamp", json!({})).lights.len(), 1);
        assert_eq!(build("lamp", json!({"on": false})).lights.len(), 0);
        assert_eq!(build("window", json!({})).lights.len(), 1);
        assert_eq!(build("window", json!({"daylight": false})).lights.len(), 0);
    }

    #[test]
    fn the_seeded_sequence_is_fixed() {
        let mut a = super::Seeded::new(5);
        let first: Vec<f32> = (0..4).map(|_| a.next()).collect();
        let mut b = super::Seeded::new(5);
        assert_eq!(first, (0..4).map(|_| b.next()).collect::<Vec<_>>());
        assert!(first.iter().all(|v| (0.0..1.0).contains(v)));
    }
}
