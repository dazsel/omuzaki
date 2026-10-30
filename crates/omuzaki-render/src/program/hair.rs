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
//! | `keep-identity` | how much of the hair's own hue (its surface colour's, streaks included) survives coloured light | the `0.36` |
//! | `rim` | the rim's colour | the toon rim `0xffe2cc` |
//! | `rim-strength` | the rim's strength | its `0.12` |
//! | `rim-width` | how far in the rim reaches: the fresnel power is `1 / width` | its power 3 |
//! | `back-light` | how strongly lights behind the hair edge it in their own colour | (none) |
//! | `highlight` | the per-light highlight's colour (#2521) | (none) |
//! | `highlight-strength` | the primary lobe, in the light's colour | (none) |
//! | `highlight-width` | how wide both lobes are (the secondary twice this) | (none) |
//! | `highlight-shift` | the primary's tilt along the strand; the secondary sits 0.25 further toward the tip | (none) |
//! | `highlight-secondary` | the secondary lobe, tinted by the hair | (none) |
//! | `highlight-jitter` | how far each strand shifts the lobes, so their edge is combed | (none) |
//! | `highlight-cel` | `0` soft lobes, `1` crisp anime bands | (none) |
//! | `shade-deep` | the second, deeper shadow tone (#2525), a multiplier; white is none | (none) |
//! | `shade-deep-at` | the N.L below which it takes over (cast shadows take it too) | (none) |
//! | `shade-shift` | where the shadow starts, a cosine (`MToon`'s shade shift) | `shift` of the hair's `toon()` |
//! | `shade-softness` | half the width of the shadow's edge | `softness` of the hair's `toon()` |
//! | `ring-shape` | the painted ring's shape per lock: `0` band, `1` zig-zag, `2` streak, `3` crescent (#2525) | `0` |
//! | `ring-shape-depth` | how deep the teeth hang, the streaks run or the crescents curl | (none) |
//! | `keep-hue` | `keep-identity` keeps the surface's own hue (`1`) or the prototype's cream (`0`) | `0` |
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

/// The per-light highlight's colour (#2521): a warm white.
const HIGHLIGHT: u32 = 0x00ff_f6ec;
/// The primary highlight's strength.
const HIGHLIGHT_STRENGTH: f32 = 0.4;
/// The primary lobe's width: the cosine between strand and half vector at which it fades out.
const HIGHLIGHT_WIDTH: f32 = 0.14;
/// The primary lobe's tilt along the strand (toward the root below 0).
const HIGHLIGHT_SHIFT: f32 = -0.08;
/// The secondary, hair-tinted lobe's strength.
const HIGHLIGHT_SECONDARY: f32 = 0.2;
/// How far each strand of the texture shifts the lobes.
const HIGHLIGHT_JITTER: f32 = 0.25;
/// How crisp the lobes' edges are.
const HIGHLIGHT_CEL: f32 = 0.2;
/// The kit's terminator, as `MToon` hair's (the prototype's was -0.05 and 0.22): where the shadow starts (a cosine) and
/// half its edge's width.
const SHADE_SHIFT: f32 = -0.45;
const SHADE_SOFTNESS: f32 = 0.45;
/// The second shadow tone (#2525), a multiplier: white is none.
const SHADE_DEEP: u32 = 0x00f4_eef2;
/// The N.L below which the second tone takes over.
const SHADE_DEEP_AT: f32 = -0.25;
/// How deep a zig-zag's teeth hang, or how far a crescent curls or a streak runs.
const RING_SHAPE_DEPTH: f32 = 0.5;

/// The prototype's hair toon: a wide soft terminator with the shadow side tinted by the `shade` parameter and a warm rim.
fn toon(values: &Values) -> Toon {
    Toon {
        shade: values.colour("shade").unwrap_or(Colour::hex(0x00b0_8e88)),
        shift: values.float("shade-shift").unwrap_or(SHADE_SHIFT),
        softness: values.float("shade-softness").unwrap_or(SHADE_SOFTNESS),
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
            ParamSpec::float("strand-lines", 0.03, 0.0, 1.0),
            ParamSpec::float("edge-darkening", 0.03, 0.0, 1.0),
            ParamSpec::float("keep-identity", 0.36, 0.0, 1.0),
            ParamSpec::colour("rim", 0x00ff_e2cc),
            ParamSpec::float("rim-strength", 0.12, 0.0, 1.0),
            ParamSpec::float("rim-width", RIM_WIDTH, 0.1, 1.0),
            ParamSpec::float("back-light", 0.6, 0.0, 2.0),
            ParamSpec::colour("highlight", HIGHLIGHT),
            ParamSpec::float("highlight-strength", HIGHLIGHT_STRENGTH, 0.0, 2.0),
            ParamSpec::float("highlight-width", HIGHLIGHT_WIDTH, 0.02, 0.8),
            ParamSpec::float("highlight-shift", HIGHLIGHT_SHIFT, -0.6, 0.6),
            ParamSpec::float("highlight-secondary", HIGHLIGHT_SECONDARY, 0.0, 2.0),
            ParamSpec::float("highlight-jitter", HIGHLIGHT_JITTER, 0.0, 1.0),
            ParamSpec::float("highlight-cel", HIGHLIGHT_CEL, 0.0, 1.0),
            ParamSpec::colour("shade-deep", SHADE_DEEP),
            ParamSpec::float("shade-deep-at", SHADE_DEEP_AT, -1.0, 1.0),
            ParamSpec::float("ring-shape", 0.0, 0.0, 3.0),
            ParamSpec::float("ring-shape-depth", RING_SHAPE_DEPTH, 0.0, 1.0),
            ParamSpec::float("keep-hue", 1.0, 0.0, 1.0),
            ParamSpec::float("shade-shift", SHADE_SHIFT, -1.0, 1.0),
            ParamSpec::float("shade-softness", SHADE_SOFTNESS, 0.0, 1.0),
        ],
        attributes: &[
            AttributeSpec { name: "hair-uv", components: 2 },
            AttributeSpec { name: "hair-info", components: 3 }
        ],
        toon,
        wgsl: include_str!("hair.wgsl"),
    }
);
