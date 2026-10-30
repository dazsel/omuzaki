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
//! | `ring-width` | the ring's half width in polar angle | the `0.11` of its `smoothstep` |
//! | `ring-wobble` | how far each lock's copy of the ring wanders | the `0.06` of its `sin` |
//! | `ring-teeth` | how much the comb teeth break the ring (`0` none) | always on |
//! | `ring-follow` | `0` paints the ring at a fixed polar angle; `1` makes it a lobe along each lock's strands | `0` |
//! | `ring-shift` | how far the lobe's strands tilt toward the normal, sliding the ring up or down the head | (none) |
//! | `strand-lines` | how far strand lines tint the colour | the `0.10` of `lines * 0.10` |
//! | `edge-darkening` | how much darker a clump's edges are | `1 - 0.86` |
//! | `keep-identity` | how much of the hair's own colour survives coloured light | the `0.36` |
//! | `rim` | the rim's colour | the toon rim `0xffe2cc` |
//! | `rim-strength` | the rim's strength | its `0.12` |
//! | `rim-width` | how far in the rim reaches: the fresnel power is `1 / width` | its power 3 |
//! | `back-light` | how strongly lights behind the hair edge it in their own colour | (none) |
//!
//! Attributes: `hair-uv` (`across` in `-1..=1`, `along` in `0..=1`) and `hair-info` (strand seed, shade multiplier,
//! polar angle: the ring follows it). The toon base is the prototype's hair material: `shift -0.05`, `soft 0.22`, rim
//! `0xffe2cc` at `0.12`, power 3; the rim's three numbers are parameters, so a close-up can thin and brighten it.
//!
//! `back-light` (0.6 by default) is the hair's own back rim: on the band where the rim lies, each spot, point or directional
//! light behind the hair, as the eye sees it, adds its colour, shadow and falloff included, so a stage's coloured
//! kickers outline the locks; a scene that turns on the back rim for every surface (`Scene::back_light_rim`) gives the
//! hair that one instead.

use omuzaki_core::colour::Colour;
use omuzaki_render_api::{Rim, Toon};

use super::{AttributeSpec, ParamSpec, ProgramSpec, Values, register_shading_program};

/// The default rim width: the prototype's fresnel power of 3.
const RIM_WIDTH: f32 = 1.0 / 3.0;
/// The ring's half width in polar angle (radians): the kit's thin arc (#2462); the prototype's is 0.11.
const RING_WIDTH: f32 = 0.035;
/// How far each lock's copy of the ring wanders (radians): the kit's steady arc; the prototype's is 0.06.
const RING_WOBBLE: f32 = 0.012;
/// How much the comb teeth break the ring, `0..=1`: barely, in the kit; the prototype's is 1.
const RING_TEETH: f32 = 0.25;
/// The kit's ring follows the strands (#2483); the prototype's is painted (0).
const RING_FOLLOW: f32 = 1.0;
/// The tilt that puts the strand-following ring where the painted one is, seen from the front: `tan(0.92) = -1 / shift`.
const RING_SHIFT: f32 = -0.75;

/// The prototype's hair toon: a wide soft terminator with the shadow side tinted by the `shade` parameter and a warm rim.
fn toon(values: &Values) -> Toon {
    Toon {
        shade: values.colour("shade").unwrap_or(Colour::hex(0x00b0_8e88)),
        shift: -0.05,
        softness: 0.22,
        rim: Rim {
            colour: values.colour("rim").unwrap_or(Colour::hex(0x00ff_e2cc)),
            power: 1.0 / values.float("rim-width").unwrap_or(RIM_WIDTH).max(0.1),
            strength: values.float("rim-strength").unwrap_or(0.12),
        },
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
            ParamSpec::float("ring-width", RING_WIDTH, 0.01, 0.3),
            ParamSpec::float("ring-wobble", RING_WOBBLE, 0.0, 0.2),
            ParamSpec::float("ring-teeth", RING_TEETH, 0.0, 1.0),
            ParamSpec::float("ring-follow", RING_FOLLOW, 0.0, 1.0),
            ParamSpec::float("ring-shift", RING_SHIFT, -2.0, 2.0),
            ParamSpec::float("strand-lines", 0.10, 0.0, 1.0),
            ParamSpec::float("edge-darkening", 0.14, 0.0, 1.0),
            ParamSpec::float("keep-identity", 0.36, 0.0, 1.0),
            ParamSpec::colour("rim", 0x00ff_e2cc),
            ParamSpec::float("rim-strength", 0.12, 0.0, 1.0),
            ParamSpec::float("rim-width", RIM_WIDTH, 0.1, 1.0),
            ParamSpec::float("back-light", 0.6, 0.0, 2.0),
        ],
        attributes: &[
            AttributeSpec { name: "hair-uv", components: 2 },
            AttributeSpec { name: "hair-info", components: 3 }
        ],
        toon,
        wgsl: include_str!("hair.wgsl"),
    }
);
