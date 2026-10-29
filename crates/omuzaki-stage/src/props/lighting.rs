//! Kinds that bring their own light (design D7: a practical light counts toward the scene's limit): `lamp`, whose
//! bulb is a point light, and `window`, a daylit pane with a spot light shining in through it.

use omuzaki_core::glam::Vec3;
use omuzaki_render_api::{Colour, Light, Material};
use omuzaki_stage_api::{PropKind, PropOutput, register_prop_kind};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::shape::{At, Shape, darker, glowing, mix, painted, painted_plain};
use super::{between, output};

/// What a lamp stands on.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LampStyle {
    /// A floor lamp on a heavy foot.
    #[default]
    Floor,
    /// A table lamp on a turned base.
    Table,
}

/// A lamp.
pub struct Lamp;

/// The lamp's parameters.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct LampParams {
    /// Floor or table.
    pub style: LampStyle,
    /// Metres to the top of the shade: 1.2 to 1.9 for a floor lamp, 0.3 to 0.8 for a table lamp.
    pub height: f32,
    /// The shade's cloth.
    pub shade: Colour,
    /// The stand.
    pub stand: Colour,
    /// Whether it is lit: the bulb glows and its point light joins the scene.
    pub on: bool,
    /// The light's colour.
    pub light: Colour,
    /// The point light's intensity (three.js units, inverse square), 0 to 20.
    pub intensity: f32,
    /// Metres where its light reaches zero, 0.5 to 15.
    pub range: f32,
}

impl Default for LampParams {
    fn default() -> Self {
        Self {
            style: LampStyle::Floor,
            height: 1.55,
            shade: Colour::hex(0x00f0_e2c8),
            stand: Colour::hex(0x003a_3532),
            on: true,
            light: Colour::hex(0x00ff_d29a),
            intensity: 2.2,
            range: 4.5,
        }
    }
}

impl PropKind for Lamp {
    type Params = LampParams;
    const ID: &'static str = "lamp";
    const SUMMARY: &'static str = "A floor or table lamp with a cloth shade; lit, its bulb is a warm point light \
                                   (colour, intensity, range) that joins the scene's lights.";

    fn validate(p: &LampParams) -> Result<(), String> {
        match p.style {
            LampStyle::Floor => between("height", p.height, 1.2, 1.9)?,
            LampStyle::Table => between("height", p.height, 0.3, 0.8)?,
        }
        between("intensity", p.intensity, 0.0, 20.0)?;
        between("range", p.range, 0.5, 15.0)
    }

    fn build(p: &LampParams) -> Result<PropOutput, String> {
        let h = p.height;
        let (shade_h, shade_bottom, shade_top) = match p.style {
            LampStyle::Floor => (0.3, 0.22, 0.15),
            LampStyle::Table => (h * 0.4, h * 0.36, h * 0.26),
        };
        let shade_y = h - shade_h;
        let mut stand = Shape::new();
        match p.style {
            LampStyle::Floor => {
                stand.frustum(At::xyz(0.0, 0.0, 0.0), 0.16, 0.13, 0.03, 32, (true, true), p.stand);
                stand.cylinder(At::xyz(0.0, 0.03, 0.0), 0.013, shade_y + shade_h * 0.4 - 0.03, 12, p.stand);
            }
            LampStyle::Table => {
                stand.frustum(At::xyz(0.0, 0.0, 0.0), h * 0.15, h * 0.12, h * 0.04, 28, (true, true), p.stand);
                stand.ellipsoid(At::xyz(0.0, h * 0.2, 0.0), Vec3::new(h * 0.12, h * 0.17, h * 0.12), 12, 20, p.stand);
                stand.cylinder(At::xyz(0.0, h * 0.3, 0.0), h * 0.02, shade_y + shade_h * 0.4 - h * 0.3, 10, p.stand);
            }
        }
        let mut shade = Shape::new();
        shade.vessel(At::xyz(0.0, shade_y, 0.0), shade_bottom, shade_top, shade_h, 0.004, false, 40, p.shade);
        let mut shade_material = painted(darker(p.shade, 0.5));
        if p.on {
            // A lit shade glows from inside: its own light, a little of the bulb's colour.
            if let omuzaki_render_api::Shading::Toon(toon) = &mut shade_material.shading {
                toon.emissive = mix(Colour::BLACK, p.light, 0.18);
            }
        }
        let bulb_at = Vec3::new(0.0, shade_y + shade_h * 0.45, 0.0);
        let mut bulb = Shape::new();
        bulb.ellipsoid(At::pos(bulb_at), Vec3::new(0.035, 0.045, 0.035), 10, 16, Colour::WHITE);
        let bulb_material = if p.on { lit_bulb(p.light) } else { painted_plain() };
        let mut out = output(vec![(stand, painted(darker(p.stand, 0.3))), (shade, shade_material), (bulb, bulb_material)]);
        if p.on && p.intensity > 0.0 {
            out.lights.push(Light::Point { position: bulb_at, colour: p.light, intensity: p.intensity, range: p.range });
        }
        Ok(out)
    }
}

fn lit_bulb(light: Colour) -> Material {
    let mut m = glowing(mix(light, Colour::WHITE, 0.6));
    m.vertex_colours = true;
    m
}

register_prop_kind!(Lamp);

/// A window.
pub struct Window;

/// The window's parameters.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct WindowParams {
    /// Metres, 0.4 to 3.0.
    pub width: f32,
    /// Metres, 0.4 to 2.6.
    pub height: f32,
    /// Panes across and down, 1 to 6 each.
    pub panes: [u8; 2],
    /// The frame.
    pub frame: Colour,
    /// The sky at the top of the glass.
    pub sky: Colour,
    /// The glass's colour at the bottom: the far view, lighter.
    pub horizon: Colour,
    /// Curtains hanging at both sides; none when absent.
    pub curtains: Option<Colour>,
    /// The daylight shining in: a spot light behind the glass, aimed down into the room.
    pub daylight: bool,
    /// The daylight's colour.
    pub light: Colour,
    /// The daylight's intensity (three.js units, inverse square from 1 m behind the glass), 0 to 40.
    pub intensity: f32,
    /// How steeply it falls into the room, degrees below level, 0 to 70.
    pub elevation: f32,
}

impl Default for WindowParams {
    fn default() -> Self {
        Self {
            width: 1.2,
            height: 1.4,
            panes: [2, 3],
            frame: Colour::hex(0x00f4_efe6),
            sky: Colour::hex(0x00a9_cdeb),
            horizon: Colour::hex(0x00ee_f2ea),
            curtains: Some(Colour::hex(0x00c9_a27a)),
            daylight: true,
            light: Colour::hex(0x00ff_f1dc),
            intensity: 9.0,
            elevation: 28.0,
        }
    }
}

impl PropKind for Window {
    type Params = WindowParams;
    const ID: &'static str = "window";
    const SUMMARY: &'static str = "A window to set on a wall, facing +Z, from y = 0 at its sill: a frame with panes \
                                   over a daylit sky, curtains if a colour is given, and daylight shining in as a spot \
                                   light (colour, intensity, elevation).";

    fn validate(p: &WindowParams) -> Result<(), String> {
        between("width", p.width, 0.4, 3.0)?;
        between("height", p.height, 0.4, 2.6)?;
        for (name, n) in [("panes[0]", p.panes[0]), ("panes[1]", p.panes[1])] {
            if !(1..=6).contains(&n) {
                return Err(format!("{name} is 1 to 6, got {n}"));
            }
        }
        between("intensity", p.intensity, 0.0, 40.0)?;
        between("elevation", p.elevation, 0.0, 70.0)
    }

    fn build(p: &WindowParams) -> Result<PropOutput, String> {
        let (w, h) = (p.width * 0.5, p.height);
        let (bar, depth) = (0.05, 0.07);
        let mut frame = Shape::new();
        // The outer frame, the sill below it and the bars between panes.
        frame.block(Vec3::new(-w - bar, 0.0, 0.0), Vec3::new(-w, h, depth), p.frame);
        frame.block(Vec3::new(w, 0.0, 0.0), Vec3::new(w + bar, h, depth), p.frame);
        frame.block(Vec3::new(-w - bar, h, 0.0), Vec3::new(w + bar, h + bar, depth), p.frame);
        frame.block(Vec3::new(-w - bar, -bar, 0.0), Vec3::new(w + bar, 0.0, depth), p.frame);
        frame.block(Vec3::new(-w - bar * 1.6, -bar * 1.5, 0.0), Vec3::new(w + bar * 1.6, -bar, depth + 0.07), p.frame);
        let [across, down] = p.panes.map(f32::from);
        let thin = bar * 0.4;
        for i in 1..p.panes[0] {
            let x = -w + p.width * f32::from(i) / across;
            frame.block(Vec3::new(x - thin * 0.5, 0.0, depth * 0.3), Vec3::new(x + thin * 0.5, h, depth * 0.7), p.frame);
        }
        for i in 1..p.panes[1] {
            let y = h * f32::from(i) / down;
            frame.block(Vec3::new(-w, y - thin * 0.5, depth * 0.3), Vec3::new(w, y + thin * 0.5, depth * 0.7), p.frame);
        }
        // The glass: a thin slab behind the bars, the sky graded from the top to the horizon.
        let mut glass = Shape::new();
        let rows = 8_u8;
        for r in 0..rows {
            let (y0, y1) = (h * f32::from(r) / f32::from(rows), h * f32::from(r + 1) / f32::from(rows));
            let t = 1.0 - (f32::from(r) + 0.5) / f32::from(rows);
            glass.block(Vec3::new(-w, y0, 0.008), Vec3::new(w, y1, 0.016), mix(p.horizon, p.sky, t));
        }
        let mut glass_material = glowing(Colour::WHITE);
        glass_material.vertex_colours = true;
        let mut parts = vec![(frame, painted(darker(p.frame, 0.45))), (glass, glass_material)];
        if let Some(cloth) = p.curtains {
            let mut curtains = Shape::new();
            for side in [-1.0_f32, 1.0] {
                // Folds: a run of slim upright slats staggered in depth, gathered at the window's sides.
                let folds = 7_u8;
                for f in 0..folds {
                    let t = f32::from(f) / f32::from(folds - 1);
                    let x = side * (w + bar + 0.02 + t * 0.26);
                    let z = depth + 0.05 + if f % 2 == 0 { 0.0 } else { 0.035 };
                    let shade = mix(cloth, darker(cloth, 0.3), if f % 2 == 0 { 0.0 } else { 1.0 });
                    curtains.bevelled(
                        At::xyz(x, f32::midpoint(h, bar) - 0.05, z),
                        Vec3::new(0.026, f32::midpoint(h, bar * 2.0) + 0.1, 0.014),
                        0.008,
                        shade,
                    );
                }
            }
            curtains.cylinder(At::xyz(-w - 0.4, h + bar + 0.1, depth + 0.07).roll(-90.0), 0.012, p.width + 0.8, 10, darker(p.frame, 0.6));
            parts.push((curtains, painted(darker(cloth, 0.5))));
        }
        let mut out = output(parts);
        if p.daylight && p.intensity > 0.0 {
            let down = p.elevation.to_radians();
            let direction = Vec3::new(0.0, -down.sin(), down.cos());
            out.lights.push(Light::Spot {
                position: Vec3::new(0.0, h * 0.5, 0.0) - direction * 1.0,
                direction,
                colour: p.light,
                intensity: p.intensity,
                range: 12.0,
                cone_outer_deg: 50.0,
                penumbra: 0.7,
                // It shines from behind the wall the window is set in: a shadow would be the wall's.
                casts_shadow: false,
            });
        }
        Ok(out)
    }
}

register_prop_kind!(Window);
