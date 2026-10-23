//! The `hair` program: root-to-tip colour with strand lines, darker clump edges and an angel-ring highlight, ported from
//! the prototype's `parts/hair.js` (`fragDiffuse` and `fragOut`; hooks in `hair.wgsl`).
//!
//! The parameters and attributes are the contract `omuzaki-char-api` fixes (`HairShading::shading`, `HAIR_UV`,
//! `HAIR_INFO`), and the defaults are `HairShading::default()`'s, so a hair material that sets nothing looks like the
//! character kit's default hair:
//!
//! | Parameter | Meaning | Prototype |
//! |---|---|---|
//! | `root`, `mid`, `tip` | the strand colour at `along` 0, 0.25 and 0.55..1 | `uRoot`, `uMid`, `uTip` |
//! | `shade` | the toon shadow multiplier | `shade` of the hair's `toon()` |
//! | `line` | strand-line colour | `uLine` |
//! | `ring` | angel-ring colour | `uRingCol` |
//! | `ring-strength` | ring brightness | the `0.22` in `fragOut` |
//! | `strand-lines` | how far strand lines tint the colour | the `0.10` of `lines * 0.10` |
//! | `edge-darkening` | how much darker a clump's edges are | `1 - 0.86` |
//! | `keep-identity` | how much of the hair's own colour survives coloured light | the `0.36` |
//!
//! Attributes: `hair-uv` (`across` in `-1..=1`, `along` in `0..=1`) and `hair-info` (strand seed, shade multiplier,
//! polar angle: the ring follows it). The toon base is the prototype's hair material: `shift -0.05`, `soft 0.22`, rim
//! `0xffe2cc` at `0.12`, power 3.

use omuzaki_core::colour::Colour;
use omuzaki_render_api::{Rim, Toon};

use super::{AttributeSpec, ParamSpec, ProgramSpec, Values, register_shading_program};

/// The prototype's hair toon: a wide soft terminator with the shadow side tinted by the `shade` parameter and a warm rim.
fn toon(values: &Values) -> Toon {
    Toon {
        shade: values.colour("shade").unwrap_or(Colour::hex(0x00b0_8e88)),
        shift: -0.05,
        softness: 0.22,
        rim: Rim { colour: Colour::hex(0x00ff_e2cc), power: 3.0, strength: 0.12 },
        ..Toon::default()
    }
}

register_shading_program!(
    "hair",
    ProgramSpec {
        name: "hair",
        params: &[
            ParamSpec::colour("root", 0x00c8_b09a),
            ParamSpec::colour("mid", 0x00e6_d2be),
            ParamSpec::colour("tip", 0x00dc_c4b0),
            ParamSpec::colour("shade", 0x00b0_8e88),
            ParamSpec::colour("line", 0x008c_685c),
            ParamSpec::colour("ring", 0x00ff_f4e8),
            ParamSpec::float("ring-strength", 0.22, 0.0, 1.0),
            ParamSpec::float("strand-lines", 0.10, 0.0, 1.0),
            ParamSpec::float("edge-darkening", 0.14, 0.0, 1.0),
            ParamSpec::float("keep-identity", 0.36, 0.0, 1.0),
        ],
        attributes: &[
            AttributeSpec { name: "hair-uv", components: 2 },
            AttributeSpec { name: "hair-info", components: 3 }
        ],
        toon,
        wgsl: include_str!("hair.wgsl"),
    }
);
