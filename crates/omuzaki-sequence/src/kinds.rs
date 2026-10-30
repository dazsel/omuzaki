//! The prop kinds a broadcast set and an outdoor cut are built from: `news-desk`, `skyline`, `cloud-bank` and `card`.
//! Each is procedural, in metres, toon with vertex colours, and registers itself with the stage's prop kinds, so a
//! scene, a sequence and `scene.add_prop` all place them.

use omuzaki_core::glam::Vec3;
use omuzaki_render_api::{Colour, Material};
use omuzaki_stage::props::shape::{At, Shape, darker, glowing, mix, painted, painted_plain};
use omuzaki_stage_api::{PropKind, PropOutput, PropPart, register_prop_kind};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// `value` is within `lo..=hi`, or the rule naming `field`.
fn between(field: &str, value: f32, lo: f32, hi: f32) -> Result<(), String> {
    if value.is_finite() && (lo..=hi).contains(&value) {
        Ok(())
    } else {
        Err(format!("{field} is {lo} to {hi}, got {value}"))
    }
}

/// The parts of a build, leaving out the shapes nothing was added to.
fn output(parts: Vec<(Shape, Material)>) -> PropOutput {
    let mut out = PropOutput::default();
    out.parts = parts
        .into_iter()
        .filter(|(shape, _)| !shape.is_empty())
        .map(|(shape, material)| PropPart::new(shape.into_mesh(), material))
        .collect();
    out
}

/// A seeded number in `0..1` for index `i` (a hash, so a kind's choices are the same on every machine).
fn unit(seed: u32, i: u32) -> f32 {
    let mut h = seed.wrapping_mul(0x9e37_79b9) ^ i.wrapping_mul(0x85eb_ca6b);
    h ^= h >> 16;
    h = h.wrapping_mul(0x7feb_352d);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846c_a68b);
    h ^= h >> 16;
    #[allow(clippy::cast_precision_loss, reason = "24 bits are exact")]
    let u = (h >> 8) as f32 / 16_777_216.0;
    u
}

/// A newsroom's anchor desk.
pub struct NewsDesk;

/// The news desk's parameters.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct NewsDeskParams {
    /// Across the front, metres, 1.0 to 4.0.
    pub width: f32,
    /// The top's height, metres, 0.7 to 1.2.
    pub height: f32,
    /// The top's depth, metres, 0.3 to 1.0.
    pub depth: f32,
    /// How far the desk wraps round the anchor, degrees of arc, 0 (straight) to 120.
    pub curve: f32,
    /// The front panel.
    pub colour: Colour,
    /// The top.
    pub top: Colour,
    /// The band across the front.
    pub accent: Colour,
    /// The band's height, metres, 0 to 0.4 (0: none).
    pub accent_height: f32,
    /// The band glows (a lit panel) rather than being painted.
    pub accent_glow: bool,
}

impl Default for NewsDeskParams {
    fn default() -> Self {
        Self {
            width: 2.0,
            height: 0.95,
            depth: 0.55,
            curve: 60.0,
            colour: Colour::hex(0x0028_2f5a),
            top: Colour::hex(0x00e8_ecf4),
            accent: Colour::hex(0x00ff_4f9a),
            accent_height: 0.12,
            accent_glow: false,
        }
    }
}

impl PropKind for NewsDesk {
    type Params = NewsDeskParams;
    const ID: &'static str = "news-desk";
    const SUMMARY: &'static str = "A newsroom's anchor desk: a front panel curved round the anchor, a top with a lip and a \
        band across the front, painted or lit. Width, height, depth, curve and the three colours.";

    fn validate(p: &NewsDeskParams) -> Result<(), String> {
        between("width", p.width, 1.0, 4.0)?;
        between("height", p.height, 0.7, 1.2)?;
        between("depth", p.depth, 0.3, 1.0)?;
        between("curve", p.curve, 0.0, 120.0)?;
        between("accent_height", p.accent_height, 0.0, 0.4)
    }

    #[allow(clippy::cast_precision_loss, reason = "a few dozen segments")]
    fn build(p: &NewsDeskParams) -> Result<PropOutput, String> {
        const SEGMENTS: u32 = 24;
        let (h, panel, lip) = (p.height, 0.05, 0.035);
        let arc = p.curve.to_radians();
        // Points along the front: a circular arc through the desk's width whose ends sit back, or a line.
        let radius = if arc > 1e-3 { p.width * 0.5 / (arc * 0.5).sin() } else { f32::INFINITY };
        let front = |u: f32| -> (Vec3, f32) {
            if radius.is_finite() {
                let a = (u - 0.5) * arc;
                (Vec3::new(radius * a.sin(), 0.0, radius * a.cos() - radius), -a.to_degrees())
            } else {
                (Vec3::new((u - 0.5) * p.width, 0.0, 0.0), 0.0)
            }
        };
        let seg_len = if radius.is_finite() { radius * arc / SEGMENTS as f32 } else { p.width / SEGMENTS as f32 };
        let mut body = Shape::new();
        let mut band = Shape::new();
        let top_colour = p.top;
        let side = darker(p.colour, 0.25);
        for i in 0..SEGMENTS {
            let u = (i as f32 + 0.5) / SEGMENTS as f32;
            let (at, yaw) = front(u);
            let half = seg_len * 0.5 + 0.004;
            // The front panel, set a hair back from the top's lip.
            body.cuboid(At::pos(at + Vec3::new(0.0, (h - lip) * 0.5, 0.0)).yaw(yaw), Vec3::new(half, (h - lip) * 0.5, panel * 0.5), p.colour);
            // The top, reaching back from the front.
            let back = Vec3::new(0.0, 0.0, -p.depth * 0.5 + 0.04);
            let turn = omuzaki_core::glam::Quat::from_rotation_y(yaw.to_radians());
            body.cuboid(
                At::pos(at + turn * back + Vec3::new(0.0, h - lip * 0.5, 0.0)).yaw(yaw),
                Vec3::new(half, lip * 0.5, p.depth * 0.5),
                top_colour,
            );
            // A plinth at the floor, darker and set back: the desk reads as standing on something.
            body.cuboid(
                At::pos(at + turn * Vec3::new(0.0, 0.0, -0.03) + Vec3::new(0.0, 0.03, 0.0)).yaw(yaw),
                Vec3::new(half, 0.03, panel * 0.5),
                side,
            );
            if p.accent_height > 0.0 {
                let y = h * 0.62;
                band.cuboid(
                    At::pos(at + turn * Vec3::new(0.0, 0.0, panel * 0.5 + 0.004) + Vec3::new(0.0, y, 0.0)).yaw(yaw),
                    Vec3::new(half, p.accent_height * 0.5, 0.006),
                    p.accent,
                );
            }
        }
        let line = darker(p.colour, 0.7);
        let band_material = if p.accent_glow { glowing(p.accent) } else { painted(line) };
        Ok(output(vec![(body, painted(line)), (band, band_material)]))
    }
}

register_prop_kind!(NewsDesk);

/// A city skyline.
pub struct Skyline;

/// The skyline's parameters.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct SkylineParams {
    /// Across, metres, 2 to 400.
    pub width: f32,
    /// Front to back of the blocks, metres, 0.5 to 60.
    pub depth: f32,
    /// How many buildings, 1 to 120.
    pub count: u32,
    /// The lowest building, metres, 1 to 300.
    pub min_height: f32,
    /// The tallest building, metres, 1 to 300.
    pub max_height: f32,
    /// The buildings' colour; each varies from it.
    pub colour: Colour,
    /// How much each building's shade varies, 0 to 1.
    pub variation: f32,
    /// The windows' colour.
    pub windows: Colour,
    /// Lit windows (glowing) rather than painted ones.
    pub lit: bool,
    /// Picks the heights, widths and shades.
    pub seed: u32,
}

impl Default for SkylineParams {
    fn default() -> Self {
        Self {
            width: 40.0,
            depth: 6.0,
            count: 18,
            min_height: 6.0,
            max_height: 26.0,
            colour: Colour::hex(0x007a_8fb8),
            variation: 0.35,
            windows: Colour::hex(0x00cf_e3ff),
            lit: false,
            seed: 7,
        }
    }
}

impl PropKind for Skyline {
    type Params = SkylineParams;
    const ID: &'static str = "skyline";
    const SUMMARY: &'static str = "A city skyline: a row of box buildings of seeded heights and widths with roof caps and \
        rows of windows, painted or lit. Width, depth, count, height range, colours and seed.";

    fn validate(p: &SkylineParams) -> Result<(), String> {
        between("width", p.width, 2.0, 400.0)?;
        between("depth", p.depth, 0.5, 60.0)?;
        if !(1..=120).contains(&p.count) {
            return Err(format!("count is 1 to 120, got {}", p.count));
        }
        between("min_height", p.min_height, 1.0, 300.0)?;
        between("max_height", p.max_height, p.min_height, 300.0)?;
        between("variation", p.variation, 0.0, 1.0)
    }

    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "small counts")]
    fn build(p: &SkylineParams) -> Result<PropOutput, String> {
        let mut blocks = Shape::new();
        let mut windows = Shape::new();
        let slot = p.width / p.count as f32;
        let dark = darker(p.colour, 0.45);
        for i in 0..p.count {
            let r = |k: u32| unit(p.seed, i * 8 + k);
            let w = slot * (0.55 + 0.4 * r(0));
            let d = p.depth * (0.5 + 0.5 * r(1));
            // Taller toward the middle of the row, with the seed's jitter: the silhouette has a peak.
            let centre = 1.0 - ((i as f32 + 0.5) / p.count as f32 - 0.5).abs() * 1.2;
            let height = p.min_height + (p.max_height - p.min_height) * (0.35 * centre + 0.65 * r(2)).clamp(0.0, 1.0);
            let x = -p.width * 0.5 + slot * (i as f32 + 0.5) + (r(3) - 0.5) * slot * 0.3;
            let z = (r(4) - 0.5) * p.depth * 0.6;
            let shade = mix(p.colour, if r(5) > 0.5 { Colour::WHITE } else { dark }, p.variation * r(6));
            blocks.block(Vec3::new(x - w * 0.5, 0.0, z - d * 0.5), Vec3::new(x + w * 0.5, height, z + d * 0.5), shade);
            // A roof cap a little narrower, on some.
            if r(7) > 0.45 {
                let cap = height * 0.06;
                blocks.block(
                    Vec3::new(x - w * 0.32, height, z - d * 0.32),
                    Vec3::new(x + w * 0.32, height + cap, z + d * 0.32),
                    darker(shade, 0.15),
                );
            }
            // Rows of windows on the front: a band a storey apart, up to 24 storeys.
            let storey = (height / 24.0).max(2.4 * (p.max_height / 80.0).max(0.25));
            let rows = ((height - storey) / storey).floor().clamp(0.0, 24.0) as u32;
            for row in 0..rows {
                let y = storey * (row as f32 + 0.75);
                windows.block(
                    Vec3::new(x - w * 0.38, y, z + d * 0.5),
                    Vec3::new(x + w * 0.38, y + storey * 0.38, z + d * 0.5 + 0.02 * slot),
                    p.windows,
                );
            }
        }
        let line = darker(p.colour, 0.75);
        let window_material = if p.lit {
            let mut m = glowing(p.windows);
            m.vertex_colours = true;
            m
        } else {
            painted_plain()
        };
        let mut block_material = painted(line);
        block_material.outline = block_material.outline.map(|mut o| {
            o.width = 0.006;
            o
        });
        Ok(output(vec![(blocks, block_material), (windows, window_material)]))
    }
}

register_prop_kind!(Skyline);

/// A bank of stylised clouds.
pub struct CloudBank;

/// The cloud bank's parameters.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct CloudBankParams {
    /// How many clouds, 1 to 40.
    pub count: u32,
    /// Across the bank, metres, 1 to 400.
    pub width: f32,
    /// Front to back, metres, 0 to 100.
    pub depth: f32,
    /// How high the bank floats spreads, metres, 0 to 100.
    pub spread: f32,
    /// One cloud's size, metres, 0.2 to 60.
    pub size: f32,
    /// The lit side.
    pub colour: Colour,
    /// The cloud's flat underside.
    pub shade: Colour,
    /// Picks the clouds.
    pub seed: u32,
}

impl Default for CloudBankParams {
    fn default() -> Self {
        Self {
            count: 6,
            width: 30.0,
            depth: 4.0,
            spread: 6.0,
            size: 4.0,
            colour: Colour::hex(0x00ff_ffff),
            shade: Colour::hex(0x00c4_d4ef),
            seed: 3,
        }
    }
}

impl PropKind for CloudBank {
    type Params = CloudBankParams;
    const ID: &'static str = "cloud-bank";
    const SUMMARY: &'static str = "Stylised clouds: clusters of round puffs on a flat, shaded underside, scattered across \
        a band of sky. Count, width, depth, height spread, size, the lit and shaded colours, and seed.";

    fn validate(p: &CloudBankParams) -> Result<(), String> {
        if !(1..=40).contains(&p.count) {
            return Err(format!("count is 1 to 40, got {}", p.count));
        }
        between("width", p.width, 1.0, 400.0)?;
        between("depth", p.depth, 0.0, 100.0)?;
        between("spread", p.spread, 0.0, 100.0)?;
        between("size", p.size, 0.2, 60.0)
    }

    #[allow(clippy::cast_precision_loss, reason = "small counts")]
    fn build(p: &CloudBankParams) -> Result<PropOutput, String> {
        let mut s = Shape::new();
        for i in 0..p.count {
            let r = |k: u32| unit(p.seed, i * 16 + k);
            let centre = Vec3::new(
                (i as f32 + 0.5) / p.count as f32 * p.width - p.width * 0.5 + (r(0) - 0.5) * p.width / p.count as f32,
                r(1) * p.spread,
                (r(2) - 0.5) * p.depth,
            );
            let size = p.size * (0.6 + 0.6 * r(3));
            // A flattened base the puffs sit on, in the shade colour, then puffs that climb toward the middle.
            s.ellipsoid(At::pos(centre), Vec3::new(size * 0.75, size * 0.16, size * 0.3), 6, 16, p.shade);
            let puffs = 4 + (r(4) * 3.0) as u32;
            for k in 0..puffs {
                let u = (k as f32 + 0.5) / puffs as f32 - 0.5;
                let radius = size * (0.22 + 0.16 * (1.0 - u.abs() * 2.0) + 0.08 * r(5 + k));
                let at = centre + Vec3::new(u * size * 1.1, radius * 0.55, (r(10 + k % 4) - 0.5) * size * 0.15);
                s.ellipsoid(At::pos(at), Vec3::splat(radius), 8, 14, p.colour);
            }
        }
        let mut material = painted(p.shade);
        if let omuzaki_render_api::Shading::Toon(toon) = &mut material.shading {
            toon.softness = 0.04;
            toon.shade = p.shade;
        }
        material.outline = material.outline.map(|mut o| {
            o.colour = darker(p.shade, 0.35);
            o
        });
        material.receives_shadows = false;
        let mut out = output(vec![(s, material)]);
        for part in &mut out.parts {
            part.casts_shadow = false;
        }
        Ok(out)
    }
}

register_prop_kind!(CloudBank);

/// A card held in a hand: a cue card, a ticket, a note.
pub struct Card;

/// The card's parameters.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct CardParams {
    /// Metres, 0.03 to 1.0.
    pub width: f32,
    /// Metres, 0.03 to 1.0.
    pub height: f32,
    /// Metres, 0.0005 to 0.02.
    pub thickness: f32,
    /// The card.
    pub colour: Colour,
    /// A band across its top (`colour` itself: none).
    pub band: Colour,
    /// Lines of print under the band, 0 to 12.
    pub lines: u32,
}

impl Default for CardParams {
    fn default() -> Self {
        Self {
            width: 0.15,
            height: 0.21,
            thickness: 0.002,
            colour: Colour::hex(0x00f7_f5ef),
            band: Colour::hex(0x00ff_4f9a),
            lines: 5,
        }
    }
}

impl PropKind for Card {
    type Params = CardParams;
    const ID: &'static str = "card";
    const SUMMARY: &'static str = "A card held in a hand (a cue card, a ticket): a thin sheet standing up from its bottom \
        edge's middle, with a coloured band and lines of print. Size, thickness, colours and line count.";

    fn validate(p: &CardParams) -> Result<(), String> {
        between("width", p.width, 0.03, 1.0)?;
        between("height", p.height, 0.03, 1.0)?;
        between("thickness", p.thickness, 0.0005, 0.02)?;
        if p.lines > 12 {
            return Err(format!("lines is 0 to 12, got {}", p.lines));
        }
        Ok(())
    }

    #[allow(clippy::cast_precision_loss, reason = "at most 12 lines")]
    fn build(p: &CardParams) -> Result<PropOutput, String> {
        let (w, h, t) = (p.width * 0.5, p.height, p.thickness * 0.5);
        let mut s = Shape::new();
        s.block(Vec3::new(-w, 0.0, -t), Vec3::new(w, h, t), p.colour);
        let ink = darker(p.colour, 0.6);
        let print = |s: &mut Shape, y0: f32, y1: f32, x0: f32, x1: f32, colour: Colour| {
            for z in [t, -t] {
                let (a, b) = if z > 0.0 { (z, z + 0.0006) } else { (z - 0.0006, z) };
                s.block(Vec3::new(x0, y0, a), Vec3::new(x1, y1, b), colour);
            }
        };
        if p.band != p.colour {
            print(&mut s, h * 0.8, h * 0.93, -w * 0.86, w * 0.86, p.band);
        }
        for i in 0..p.lines {
            let y = h * 0.7 - (i as f32) * h * 0.11;
            let end = if i + 1 == p.lines { 0.2 } else { 0.86 - 0.12 * ((i % 3) as f32) };
            print(&mut s, y - h * 0.025, y, -w * 0.86, w * end, ink);
        }
        Ok(output(vec![(s, painted(darker(p.colour, 0.7)))]))
    }
}

register_prop_kind!(Card);

#[cfg(test)]
mod tests {
    use super::*;

    fn builds<K: PropKind>() {
        let params = K::Params::default();
        K::validate(&params).unwrap();
        let out = K::build(&params).unwrap();
        assert!(!out.parts.is_empty(), "{} builds something", K::ID);
        for part in &out.parts {
            part.mesh.validate().unwrap();
        }
    }

    #[test]
    fn every_kind_builds_at_its_defaults() {
        builds::<NewsDesk>();
        builds::<Skyline>();
        builds::<CloudBank>();
        builds::<Card>();
    }

    #[test]
    fn a_straight_desk_builds_too() {
        let p = NewsDeskParams { curve: 0.0, ..NewsDeskParams::default() };
        NewsDesk::build(&p).unwrap();
    }

    #[test]
    fn the_registry_knows_them() {
        for id in ["news-desk", "skyline", "cloud-bank", "card"] {
            omuzaki_stage_api::prop_kind(id).unwrap();
        }
    }

    #[test]
    fn a_skyline_is_the_same_for_a_seed() {
        let a = Skyline::build(&SkylineParams::default()).unwrap();
        let b = Skyline::build(&SkylineParams::default()).unwrap();
        assert_eq!(a.parts[0].mesh.positions, b.parts[0].mesh.positions);
        let c = Skyline::build(&SkylineParams { seed: 8, ..SkylineParams::default() }).unwrap();
        assert_ne!(a.parts[0].mesh.positions, c.parts[0].mesh.positions);
    }
}
