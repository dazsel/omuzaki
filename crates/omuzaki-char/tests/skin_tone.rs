//! The skin-tone range (CB-04): objective gates over all 12 tones x 3 undertones, a render test through the
//! mock toon renderer under the prototype's three light rigs, the contact sheet a maintainer signs off, and the ops.
//!
//! The contact sheet is written only when `OMUZAKI_SKIN_SHEET=<directory>` is set (it renders either way, and the
//! picture's numbers are gated). Taste is judged on the picture, not on these numbers: see
//! `$OMUZAKI_ASSETS/renders/skin-tones/`.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::too_many_lines)]

use std::path::Path;

use omuzaki_char::kit::grid::{GridOptions, MeshBuilder};
use omuzaki_char::kit::loft::lathe_axis;
use omuzaki_char::palette::skin_tone::{
    Oklch, SkinSlots, SkinTone, SkinToneParams, Undertone, delta_e, hue_delta, skin_ramp, skin_tones,
};
use omuzaki_char_api::{CharacterDef, SkinToneId};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{DVec3, Mat4, UVec2, Vec2, Vec3};
use omuzaki_core::time::FakeClock;
use omuzaki_doc_api::{DocPath, DocRead, DocWrite};
use omuzaki_doc_mock::MemDoc;
use omuzaki_face_api::ColourRef;
use omuzaki_ops_api::{Actor, ErrorCode, OpCtx, OpError, OpKind, Plan, Scope, Undo, find};
use omuzaki_render_api::{
    AlphaEncoding, Background, Camera, Image, Instance, Light, Mapping, Material, MeshData, Outline, Overlay, Post,
    Quality, Renderer, RendererExt, Rim, Scene, Shading, TextureDesc, Tonemap, Toon,
};
use omuzaki_render_mock::MockRenderer;
use serde_json::{Value, json};

// ------------------------------------------------------------------------------------------------ the colour gates

fn ramps() -> Vec<(&'static SkinTone, Undertone, SkinSlots)> {
    let tones = skin_tones().unwrap();
    tones.iter().flat_map(|t| Undertone::ALL.map(|u| (t, u, skin_ramp(t, u)))).collect()
}

/// WCAG 2 contrast ratio between two sRGB colours.
fn contrast_ratio(a: Colour, b: Colour) -> f64 {
    let luminance = |c: Colour| {
        let [r, g, b] = [c.r, c.g, c.b].map(|v| {
            let v = f64::from(v) / 255.0;
            if v <= 0.040_45 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        });
        0.2126 * r + 0.7152 * g + 0.0722 * b
    };
    let (hi, lo) = (luminance(a).max(luminance(b)), luminance(a).min(luminance(b)));
    (hi + 0.05) / (lo + 0.05)
}

/// Collects every broken relationship so one run shows them all.
#[derive(Default)]
struct Gate(Vec<String>);

impl Gate {
    fn check(&mut self, ok: bool, what: impl FnOnce() -> String) {
        if !ok {
            self.0.push(what());
        }
    }

    fn finish(self) {
        assert!(self.0.is_empty(), "{} broken relationships:\n{}", self.0.len(), self.0.join("\n"));
    }
}

/// The gates the issue states, measured on the colours that ship (8-bit), for every tone and undertone. The bounds
/// are the issue's, plus the ones the rules make true with margin (the measured range is in the comment beside each).
#[test]
fn every_ramp_stays_inside_the_chroma_and_lightness_bounds() {
    let mut gate = Gate::default();
    for (tone, undertone, s) in ramps() {
        let who = format!("{} {}", tone.id, undertone.as_str());
        let [base, shade, line, rim, ear] =
            [s.skin, s.skin_shade, s.skin_line, s.skin_rim, s.ear_inner].map(Oklch::from_colour);

        // Shade: hue turned 4 to 18 degrees towards warm red (measured 5.9 to 13.2), chroma kept (>= 0.85; measured 1.2 to
        // 1.5, it is raised so shadows never grey), a visible but gentle step (measured delta-E 8.0 to 9.5).
        let turn = hue_delta(shade.h, base.h);
        gate.check((4.0..=18.0).contains(&turn), || format!("{who}: shade hue turn {turn:.1} degrees, want 4..=18"));
        gate.check(shade.c >= 0.85 * base.c, || format!("{who}: shade chroma {:.3} vs base {:.3}", shade.c, base.c));
        gate.check(shade.l < base.l, || format!("{who}: shade is not darker than the base"));
        let shade_step = delta_e(s.skin, s.skin_shade);
        gate.check((6.0..=12.0).contains(&shade_step), || format!("{who}: shade delta-E {shade_step:.1}, want 6..=12"));

        // Line: at least 0.08 darker than the shade, not black (L >= 0.12), more saturated (chroma per lightness).
        gate.check(shade.l - line.l >= 0.08, || format!("{who}: line only {:.3} darker than shade", shade.l - line.l));
        gate.check(line.l >= 0.12, || format!("{who}: line is black-ish (L {:.3})", line.l));
        gate.check(line.c / line.l > shade.c / shade.l, || format!("{who}: line is less saturated than the shade"));
        // Contrast against the skin it outlines (measured 1.8:1 to 2.5:1): the line cannot go much higher on deep skin
        // without turning black.
        let contrast = contrast_ratio(s.skin_line, s.skin);
        gate.check(contrast >= 1.7, || format!("{who}: line contrast {contrast:.2}:1, want >= 1.7:1"));

        // Rim: lighter by at least 0.04, chroma matched to the base (measured 0.60 to 1.0 of it: the lightest tone's rim sits
        // near white, where sRGB has little chroma to give).
        gate.check(rim.l - base.l >= 0.04, || format!("{who}: rim only {:.3} lighter", rim.l - base.l));
        gate.check((0.5..=1.25).contains(&(rim.c / base.c)), || {
            format!("{who}: rim chroma {:.2} of the base's", rim.c / base.c)
        });

        // Blush and lips read against the skin on every tone, the deepest included (measured 12.6 to 13.5 and 15.7 to 16.8).
        let blush = delta_e(s.skin, s.blush);
        gate.check(blush >= 12.0, || format!("{who}: blush delta-E {blush:.1}, want >= 12"));
        let lip = delta_e(s.skin, s.lip);
        gate.check(lip >= 12.0, || format!("{who}: lip delta-E {lip:.1}, want >= 12"));
        gate.check(delta_e(s.blush, s.lip) >= 3.0, || format!("{who}: blush and lip are the same colour"));

        // Ear: a little deeper and redder than the skin, never a different colour.
        let ear_step = delta_e(s.skin, s.ear_inner);
        gate.check((2.5..=10.0).contains(&ear_step), || format!("{who}: ear delta-E {ear_step:.1}, want 2.5..=10"));
        gate.check(ear.h < base.h && ear.c > base.c, || format!("{who}: the ear is not redder and richer"));
    }
    gate.finish();
}

/// The renderer multiplies the skin by `Toon::shade`, so the material must give it the ratio, not the shade colour:
/// skin x `toon_shade` shows the shade on every tone (the cap at 1 may only lose a hair of it).
#[test]
fn the_toon_shade_multiplier_gives_back_the_shade_colour() {
    let mut gate = Gate::default();
    for (tone, undertone, s) in ramps() {
        let shown = Colour::from_linear(s.skin.to_linear() * s.toon_shade().to_linear(), 1.0);
        let off = delta_e(shown, s.skin_shade);
        gate.check(off <= 2.0, || {
            format!("{} {}: skin x toon_shade is {off:.1} from the shade", tone.id, undertone.as_str())
        });
    }
    gate.finish();
}

#[test]
fn the_range_runs_from_very_light_to_very_dark_in_clear_steps() {
    let mut gate = Gate::default();
    for undertone in Undertone::ALL {
        let skins: Vec<Colour> = skin_tones().unwrap().iter().map(|t| skin_ramp(t, undertone).skin).collect();
        let ls: Vec<f64> = skins.iter().map(|c| Oklch::from_colour(*c).l).collect();
        gate.check(ls[0] > 0.88 && ls[11] < 0.42, || format!("{undertone:?}: range is {:.2} to {:.2}", ls[0], ls[11]));
        for i in 1..skins.len() {
            gate.check(ls[i] < ls[i - 1], || format!("{undertone:?}: tone {} is not darker than {}", i + 1, i));
            let step = delta_e(skins[i - 1], skins[i]);
            gate.check(step >= 4.0, || format!("{undertone:?}: tones {i} and {} are only {step:.1} apart", i + 1));
        }
    }
    gate.finish();
}

#[test]
fn undertones_turn_the_hue_the_same_way_on_every_tone() {
    let mut gate = Gate::default();
    for tone in skin_tones().unwrap() {
        let hue = |u| Oklch::from_colour(skin_ramp(tone, u).skin).h;
        let (warm, neutral, cool) = (hue(Undertone::Warm), hue(Undertone::Neutral), hue(Undertone::Cool));
        gate.check(warm > neutral && neutral > cool, || format!("{}: hues {warm:.0} {neutral:.0} {cool:.0}", tone.id));
        let apart = delta_e(skin_ramp(tone, Undertone::Warm).skin, skin_ramp(tone, Undertone::Cool).skin);
        gate.check(apart >= 1.2, || format!("{}: warm and cool are only {apart:.1} apart", tone.id));
    }
    gate.finish();
}

// ------------------------------------------------------------------------------------------------ the render rigs

/// One of the prototype's light presets (`LIGHTS` in its `js/viewer.js`). The candle's key sits at
/// the flame's offset from the origin; the flame's own point light is left out (the mock's falloff is not three.js's).
struct Rig {
    name: &'static str,
    exposure: f32,
    hemisphere: (u32, u32, f32),
    /// Key, rim and fill directional lights: colour, intensity, direction towards the light.
    lights: [(u32, f32, Vec3); 3],
}

const RIGS: [Rig; 3] = [
    Rig {
        name: "studio",
        exposure: 0.9,
        hemisphere: (0x00f1_eef8, 0x004a_3e36, 0.75),
        lights: [
            (0x00ff_f6ec, 1.7, Vec3::new(1.6, 3.0, 2.4)),
            (0x00df_e6ff, 0.8, Vec3::new(-2.2, 2.0, -2.5)),
            (0x00ff_f0e0, 0.45, Vec3::new(-2.4, 1.2, 2.0)),
        ],
    },
    Rig {
        name: "candle",
        exposure: 0.92,
        hemisphere: (0x008a_7f9a, 0x002a_1a10, 0.42),
        lights: [
            (0x00ff_c28a, 1.6, Vec3::new(1.65, 1.45, -0.55)),
            (0x008f_a8ff, 1.0, Vec3::new(-2.5, 2.2, -2.8)),
            (0x00ff_e2c4, 0.25, Vec3::new(-2.0, 1.0, 2.5)),
        ],
    },
    Rig {
        name: "moonlit",
        exposure: 1.0,
        hemisphere: (0x0040_507a, 0x0014_100e, 0.55),
        lights: [
            (0x009d_b6ff, 1.9, Vec3::new(-1.8, 3.2, -2.2)),
            (0x00bc_d0ff, 1.4, Vec3::new(2.4, 1.6, -2.6)),
            (0x006a_78a8, 0.35, Vec3::new(1.5, 1.0, 2.5)),
        ],
    },
];

impl Rig {
    fn lights(&self) -> Vec<Light> {
        let (sky, ground, intensity) = self.hemisphere;
        let mut lights = vec![Light::Hemisphere { sky: Colour::hex(sky), ground: Colour::hex(ground), intensity }];
        lights.extend(self.lights.iter().map(|&(colour, intensity, to_light)| Light::Directional {
            to_light,
            colour: Colour::hex(colour),
            intensity,
            casts_shadow: false,
        }));
        lights
    }

    fn key(&self) -> Vec3 {
        self.lights[0].2.normalize()
    }
}

/// The head's toon look in the prototype (`shift -0.28`, `soft 0.28`, rim 0.10): the terminator sits where the gates
/// measure it.
const SHIFT: f32 = -0.28;
const SOFTNESS: f32 = 0.28;
const RADIUS: f32 = 0.5;
const DISTANCE: f32 = 2.6;

/// A surface of revolution about Y through `profile` (height, radius), smooth-shaded, rigid.
fn lathe_mesh(profile: &[(f64, f64)], segments: usize) -> MeshData {
    let grid = lathe_axis(DVec3::ZERO, DVec3::Y, profile, segments).unwrap();
    let mut builder = MeshBuilder::new();
    let inside = |_: usize, _: usize| DVec3::ZERO;
    builder.add_grid(&grid, &GridOptions { wrap_j: true, inside: Some(&inside), ..GridOptions::default() }).unwrap();
    let mut mesh = builder.mesh().clone();
    mesh.joints.clear();
    mesh.weights.clear();
    mesh
}

/// The head proxy: a sphere.
fn head_mesh() -> MeshData {
    let r = f64::from(RADIUS);
    let profile: Vec<(f64, f64)> = (0..=48)
        .map(|i| {
            let a = std::f64::consts::PI * f64::from(i) / 48.0;
            (-r * a.cos(), r * a.sin())
        })
        .collect();
    lathe_mesh(&profile, 72)
}

/// The body proxy: a capsule, like a torso or a limb, 0.3 radius and 1.0 tall.
fn body_mesh() -> MeshData {
    let (r, half) = (0.3_f64, 0.35_f64);
    let cap = |sign: f64| {
        (0..=16).map(move |i| {
            let a = std::f64::consts::FRAC_PI_2 * f64::from(i) / 16.0;
            (sign * (half + r * a.sin()), r * a.cos())
        })
    };
    let mut profile: Vec<(f64, f64)> = cap(-1.0).collect();
    profile.reverse();
    profile.extend(cap(1.0));
    lathe_mesh(&profile, 72)
}

/// The skin material exactly as a head part would assemble it from the slots and the tone's params.
fn skin_material(slots: &SkinSlots, params: SkinToneParams) -> Material {
    let mut material = Material::toon(slots.skin);
    material.shading = Shading::Toon(Toon {
        shade: slots.toon_shade(),
        shift: SHIFT,
        softness: SOFTNESS,
        rim: Rim { colour: slots.skin_rim, power: 3.0, strength: 0.10 },
        emissive: Colour::BLACK,
        skin: Some(params.into()),
    });
    material.outline = Some(Outline { colour: slots.skin_line, width: 0.006 });
    material.receives_shadows = false;
    material
}

/// Blends `colour` over a straight-alpha RGBA texel with coverage `alpha`.
fn blend(texel: &mut [u8], colour: Colour, alpha: f32) {
    let below = f32::from(texel[3]) / 255.0;
    let out = alpha + below * (1.0 - alpha);
    for (channel, value) in texel.iter_mut().zip([colour.r, colour.g, colour.b]) {
        let mixed = (f32::from(value) * alpha + f32::from(*channel) * below * (1.0 - alpha)) / out.max(1e-6);
        *channel = mixed.round() as u8;
    }
    texel[3] = (out * 255.0).round() as u8;
}

/// A soft-edged ellipse of `colour` at `(x, y)` in the face rectangle (metres, y up), radii `(rx, ry)`.
fn ellipse(texture: &mut [u8], size: usize, at: Vec2, radii: Vec2, colour: Colour, softness: f32, peak: f32) {
    for py in 0..size {
        for px in 0..size {
            let x = (px as f32 + 0.5) / size as f32 * 0.64 - 0.32;
            let y = 0.32 - (py as f32 + 0.5) / size as f32 * 0.64;
            let d = (((x - at.x) / radii.x).powi(2) + ((y - at.y) / radii.y).powi(2)).sqrt();
            let alpha = peak * (1.0 - ((d - (1.0 - softness)) / softness).clamp(0.0, 1.0));
            if alpha > 0.0 {
                let at = (py * size + px) * 4;
                blend(&mut texture[at..at + 4], colour, alpha);
            }
        }
    }
}

/// The face proxy: blush, eyes and a mouth in the slot colours, painted on a transparent texture.
fn face_texture(slots: &SkinSlots, size: usize) -> Vec<u8> {
    let mut texture = vec![0u8; size * size * 4];
    for side in [-1.0, 1.0] {
        ellipse(&mut texture, size, Vec2::new(0.19 * side, -0.075), Vec2::new(0.085, 0.045), slots.blush, 0.9, 0.95);
        let eye = Vec2::new(0.13 * side, 0.03);
        ellipse(&mut texture, size, eye, Vec2::new(0.036, 0.052), Colour::hex(0x002b_1b1c), 0.15, 1.0);
        ellipse(
            &mut texture,
            size,
            eye - Vec2::new(0.0, 0.012),
            Vec2::new(0.024, 0.032),
            Colour::hex(0x0050_8a92),
            0.3,
            1.0,
        );
        ellipse(&mut texture, size, eye + Vec2::new(0.01, 0.022), Vec2::new(0.009, 0.009), Colour::WHITE, 0.3, 1.0);
    }
    ellipse(&mut texture, size, Vec2::new(0.0, -0.17), Vec2::new(0.05, 0.016), slots.lip, 0.4, 1.0);
    ellipse(&mut texture, size, Vec2::new(0.0, -0.1745), Vec2::new(0.052, 0.0045), slots.skin_line, 0.5, 0.9);
    texture
}

/// What is drawn: a head with the face proxy, or a plain head or body.
#[derive(Copy, Clone, PartialEq)]
enum Subject {
    PlainHead,
    FaceHead,
    Body,
}

struct Look {
    rig: &'static Rig,
    size: u32,
    /// `false` renders exact colours (no tone mapping, exposure 1) for the luminance-floor gate.
    graded: bool,
}

/// One skin subject under one rig, through the mock toon renderer.
fn render(slots: &SkinSlots, params: SkinToneParams, subject: Subject, look: &Look) -> Image {
    let mut renderer = MockRenderer::new();
    let mesh = renderer.create_mesh(&if subject == Subject::Body { body_mesh() } else { head_mesh() }).unwrap();
    let mut material = skin_material(slots, params);
    if subject == Subject::FaceHead {
        let desc = TextureDesc::colour(UVec2::splat(192));
        let texture = renderer.create_texture(&desc, Some(&face_texture(slots, 192))).unwrap();
        material.overlays.push(Overlay {
            texture,
            mapping: Mapping::Planar {
                min: Vec2::new(-0.32, -0.32),
                max: Vec2::new(0.32, 0.32),
                fade_from: 0.05,
                fade_to: 0.15,
            },
            strength: 1.0,
        });
    }
    let material = renderer.create_material(&material).unwrap();
    // The body is tilted so its lit and shadowed sides both show.
    let pose = if subject == Subject::Body { Mat4::from_rotation_z(-0.35) } else { Mat4::IDENTITY };
    let mut scene = Scene::new(Camera::look_at(
        Vec3::new(0.0, 0.0, DISTANCE + if subject == Subject::Body { 0.9 } else { 0.0 }),
        Vec3::ZERO,
        Vec3::Y,
        Camera::default().projection,
    ));
    scene.background = Background::Solid { colour: Colour::hex(0x001e_1c24) };
    scene.lights = look.rig.lights();
    scene.post = Post {
        exposure: if look.graded { look.rig.exposure } else { 1.0 },
        tonemap: if look.graded { Tonemap::Aces } else { Tonemap::None },
        ..Post::default()
    };
    scene.instances.push(Instance::new(mesh, material, pose));
    let quality = Quality { samples: 4, ..Quality::offline() };
    renderer.render_image(UVec2::splat(look.size), AlphaEncoding::Straight, &scene, &quality).unwrap()
}

// ------------------------------------------------------------------------------------------------ the render gates

/// The sphere's pixels grouped by how the key light meets them: fully lit, in the soft terminator band, in shade.
struct Regions {
    all: Vec<[u8; 3]>,
    lit: Vec<[u8; 3]>,
    terminator: Vec<[u8; 3]>,
}

/// Groups the plain head's pixels by their normal's angle to the key (the sphere's normal at a pixel follows from its
/// place in the silhouette; the outline ring and a rim of anti-aliasing are left out).
fn regions(image: &Image, rig: &Rig) -> Regions {
    let size = image.size.x as f32;
    let tan_half_fov = (15.0_f32).to_radians().tan();
    let radius_px = (RADIUS / (DISTANCE * DISTANCE - RADIUS * RADIUS).sqrt()) / tan_half_fov * size / 2.0;
    let key = rig.key();
    let mut out = Regions { all: Vec::new(), lit: Vec::new(), terminator: Vec::new() };
    for y in 0..image.size.y {
        for x in 0..image.size.x {
            let (dx, dy) = ((x as f32 + 0.5 - size / 2.0) / radius_px, -(y as f32 + 0.5 - size / 2.0) / radius_px);
            let r2 = dx * dx + dy * dy;
            if r2 > 0.88_f32.powi(2) {
                continue;
            }
            let normal = Vec3::new(dx, dy, (1.0 - r2).sqrt());
            let ndl = normal.dot(key);
            let [r, g, b, _] = image.pixel(x, y).unwrap();
            out.all.push([r, g, b]);
            if ndl > SHIFT + SOFTNESS + 0.1 {
                out.lit.push([r, g, b]);
            } else if (ndl - SHIFT).abs() < SOFTNESS * 0.6 {
                out.terminator.push([r, g, b]);
            }
        }
    }
    out
}

fn mean(pixels: &[[u8; 3]]) -> [f64; 3] {
    let sum = pixels
        .iter()
        .fold([0.0; 3], |acc, p| [acc[0] + f64::from(p[0]), acc[1] + f64::from(p[1]), acc[2] + f64::from(p[2])]);
    sum.map(|v| v / pixels.len() as f64)
}

/// HSV saturation of an sRGB colour (0 grey to 1 pure).
fn saturation([r, g, b]: [f64; 3]) -> f64 {
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    if max <= 0.0 { 0.0 } else { (max - min) / max }
}

fn linear_luminance([r, g, b]: [f64; 3]) -> f64 {
    let lin = |v: f64| {
        let v = v / 255.0;
        if v <= 0.040_45 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
}

fn rgb_of(c: Colour) -> [f64; 3] {
    [f64::from(c.r), f64::from(c.g), f64::from(c.b)]
}

/// Chroma per lightness of an average colour: how colourful it looks for how bright it is.
fn chromaticness(rgb: [f64; 3]) -> f64 {
    let c = Oklch::from_colour(Colour::rgb(rgb[0].round() as u8, rgb[1].round() as u8, rgb[2].round() as u8));
    c.c / c.l
}

/// A skin sphere under studio, candle and moonlight: the terminator keeps its saturation and its brightness, so
/// shading is never grey. 12 tones x 3 undertones x 3 rigs.
///
/// Measured over all 108 renders before the bounds were set: terminator saturation at worst 0.96 of the lit region's,
/// terminator chroma per lightness 0.91 of the lit side's, terminator luminance 0.99 x floor x albedo (8-bit
/// rounding), the whole sphere's chroma per lightness 0.45 of the skin's (palest tone under the studio's tone curve)
/// and its hue at most 36 degrees off (palest tone under the blue moonlight; at most 11 under white and warm keys).
#[test]
fn shading_never_goes_grey_under_studio_candle_or_moonlight() {
    let mut gate = Gate::default();
    for (tone, undertone, slots) in ramps() {
        for rig in &RIGS {
            let who = format!("{} {} under {}", tone.id, undertone.as_str(), rig.name);
            let graded = render(&slots, tone.params, Subject::PlainHead, &Look { rig, size: 128, graded: true });
            let exact = render(&slots, tone.params, Subject::PlainHead, &Look { rig, size: 128, graded: false });
            let (graded, exact) = (regions(&graded, rig), regions(&exact, rig));
            gate.check(graded.lit.len() > 40 && graded.terminator.len() > 40, || {
                format!(
                    "{who}: the sphere shows no lit or no terminator region ({} / {})",
                    graded.lit.len(),
                    graded.terminator.len()
                )
            });
            if graded.lit.len() <= 40 || graded.terminator.len() <= 40 {
                continue;
            }
            let (lit, term) = (mean(&graded.lit), mean(&graded.terminator));
            let kept = saturation(term) / saturation(lit);
            gate.check(kept >= 0.8, || {
                format!("{who}: terminator saturation {kept:.2} of the lit region's, want >= 0.8")
            });
            // Chroma per lightness (OKLCH) is the stricter reading of "saturation": tone mapping may pale the whole
            // sphere, but the terminator must not be greyer than the lit side.
            let retained = chromaticness(term) / chromaticness(lit);
            gate.check(retained >= 0.8, || {
                format!("{who}: terminator chroma per lightness {retained:.2} of the lit side's")
            });
            let albedo = rgb_of(slots.skin);
            // The whole sphere keeps most of the skin's colour (the filmic curve pales light skin, hence the bound) and
            // its hue: a white or warm key moves it a few degrees, a blue moonlight more on the palest skin.
            let sphere = mean(&graded.all);
            let whole = chromaticness(sphere) / chromaticness(albedo);
            gate.check(whole >= 0.4, || {
                format!("{who}: the sphere keeps only {whole:.2} of the skin's chroma per lightness")
            });
            let hue_of = |rgb: [f64; 3]| {
                Oklch::from_colour(Colour::rgb(rgb[0].round() as u8, rgb[1].round() as u8, rgb[2].round() as u8)).h
            };
            let drift = hue_delta(hue_of(albedo), hue_of(sphere)).abs();
            let allowed = if rig.name == "moonlit" { 40.0 } else { 15.0 };
            gate.check(drift <= allowed, || format!("{who}: the hue drifts {drift:.0} degrees, want <= {allowed}"));
            // Luminance is judged on exact colours: tone mapping lifts or squashes it, the floor is pre-tonemap.
            let floor = f64::from(tone.params.floor) * linear_luminance(albedo);
            let seen = linear_luminance(mean(&exact.terminator));
            gate.check(seen >= 0.98 * floor, || {
                format!("{who}: terminator luminance {seen:.4} below floor x albedo {floor:.4}")
            });
        }
    }
    gate.finish();
}

// ------------------------------------------------------------------------------------------------ the contact sheet

const CELL: u32 = 168;

/// A tiny RGBA canvas for the contact sheet.
struct Canvas {
    size: UVec2,
    rgba: Vec<u8>,
}

impl Canvas {
    fn new(size: UVec2, background: Colour) -> Self {
        let rgba = [background.r, background.g, background.b, 255].repeat((size.x * size.y) as usize);
        Self { size, rgba }
    }

    fn fill(&mut self, x: u32, y: u32, w: u32, h: u32, colour: Colour) {
        for yy in y..(y + h).min(self.size.y) {
            for xx in x..(x + w).min(self.size.x) {
                let at = ((yy * self.size.x + xx) * 4) as usize;
                self.rgba[at..at + 4].copy_from_slice(&[colour.r, colour.g, colour.b, 255]);
            }
        }
    }

    fn paste(&mut self, image: &Image, x: u32, y: u32) {
        for row in 0..image.size.y {
            let from = (row * image.size.x * 4) as usize;
            let to = (((y + row) * self.size.x + x) * 4) as usize;
            self.rgba[to..to + image.size.x as usize * 4]
                .copy_from_slice(&image.rgba8[from..from + image.size.x as usize * 4]);
        }
    }

    /// Draws `number` (two digits) in a 3x5 pixel font, each pixel `scale` square.
    fn number(&mut self, x: u32, y: u32, number: usize, scale: u32) {
        const DIGITS: [[u8; 5]; 10] = [
            [0b111, 0b101, 0b101, 0b101, 0b111],
            [0b010, 0b110, 0b010, 0b010, 0b111],
            [0b111, 0b001, 0b111, 0b100, 0b111],
            [0b111, 0b001, 0b111, 0b001, 0b111],
            [0b101, 0b101, 0b111, 0b001, 0b001],
            [0b111, 0b100, 0b111, 0b001, 0b111],
            [0b111, 0b100, 0b111, 0b101, 0b111],
            [0b111, 0b001, 0b001, 0b001, 0b001],
            [0b111, 0b101, 0b111, 0b101, 0b111],
            [0b111, 0b101, 0b111, 0b001, 0b111],
        ];
        for (i, digit) in [number / 10, number % 10].into_iter().enumerate() {
            for (row, bits) in DIGITS[digit].iter().enumerate() {
                for col in 0..3 {
                    if bits >> (2 - col) & 1 == 1 {
                        self.fill(
                            x + (i as u32 * 4 + col) * scale,
                            y + row as u32 * scale,
                            scale,
                            scale,
                            Colour::WHITE,
                        );
                    }
                }
            }
        }
    }
}

/// The seven slot colours as a column of chips.
fn chips(canvas: &mut Canvas, x: u32, y: u32, width: u32, slots: &SkinSlots) {
    let height = CELL / 7;
    for (i, (_, colour)) in slots.entries().into_iter().enumerate() {
        canvas.fill(x, y + i as u32 * height, width, height, colour);
    }
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 { 0xedb8_8320 ^ (crc >> 1) } else { crc >> 1 };
        }
    }
    !crc
}

/// An 8-bit RGBA PNG with stored (uncompressed) deflate blocks: no encoder dependency for a test picture.
fn png(canvas: &Canvas) -> Vec<u8> {
    let mut raw = Vec::with_capacity(canvas.rgba.len() + canvas.size.y as usize);
    for row in canvas.rgba.chunks((canvas.size.x * 4) as usize) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut zlib = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65_535).collect();
    for (i, block) in blocks.iter().enumerate() {
        zlib.push(u8::from(i + 1 == blocks.len()));
        zlib.extend_from_slice(&(block.len() as u16).to_le_bytes());
        zlib.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
        zlib.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for byte in &raw {
        a = (a + u32::from(*byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    zlib.extend_from_slice(&(b << 16 | a).to_be_bytes());

    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |kind: &[u8; 4], data: &[u8]| {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let body: Vec<u8> = kind.iter().chain(data).copied().collect();
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
    };
    let mut header = Vec::new();
    header.extend_from_slice(&canvas.size.x.to_be_bytes());
    header.extend_from_slice(&canvas.size.y.to_be_bytes());
    header.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(b"IHDR", &header);
    chunk(b"IDAT", &zlib);
    chunk(b"IEND", &[]);
    out
}

#[allow(clippy::disallowed_methods, reason = "test evidence, written to a folder the developer names; not an op")]
fn write_sheet(directory: &Path, name: &str, canvas: &Canvas) {
    std::fs::create_dir_all(directory).unwrap();
    std::fs::write(directory.join(name), png(canvas)).unwrap();
}

/// 12 tones x 3 rigs x a face and a body, in the neutral undertone, plus the undertones in the studio rig.
///
/// `contact-sheet.png`: one row per tone (01 at the top, 12 at the bottom). Left: the tone number and its seven palette
/// chips, top to bottom `skin`, `skin_shade`, `skin_line`, `skin_rim`, `blush`, `lip`, `ear_inner`. Then, for studio,
/// candle and moonlit in turn, the face proxy and the body proxy (the bar over each pair is the rig's key colour).
///
/// `undertones.png`: one row per tone; warm, neutral and cool in turn, each the studio face with its chips.
#[test]
fn contact_sheet_renders_every_tone_on_a_face_and_a_body() {
    let tones = skin_tones().unwrap();
    let (margin, header, chips_width) = (6, 22, 40);
    let width = margin + chips_width + 28 + 6 * CELL;
    let mut sheet = Canvas::new(UVec2::new(width, header + 12 * CELL), Colour::hex(0x0014_1218));
    for (row, tone) in tones.iter().enumerate() {
        let slots = skin_ramp(tone, Undertone::Neutral);
        let y = header + row as u32 * CELL;
        sheet.number(margin, y + 6, row + 1, 4);
        chips(&mut sheet, margin + 28, y, chips_width, &slots);
        for (r, rig) in RIGS.iter().enumerate() {
            let look = Look { rig, size: CELL, graded: true };
            for (s, subject) in [Subject::FaceHead, Subject::Body].into_iter().enumerate() {
                let image = render(&slots, tone.params, subject, &look);
                assert!(image.rgba8.iter().any(|v| *v != image.rgba8[0]), "{} is blank", tone.id);
                sheet.paste(&image, margin + 28 + chips_width + (r as u32 * 2 + s as u32) * CELL, y);
            }
            if row == 0 {
                sheet.fill(
                    margin + 28 + chips_width + r as u32 * 2 * CELL,
                    2,
                    2 * CELL - 4,
                    header - 6,
                    Colour::hex(rig.lights[0].0),
                );
            }
        }
    }

    let mut undertones =
        Canvas::new(UVec2::new(margin + 28 + 3 * (CELL + chips_width), 12 * CELL), Colour::hex(0x0014_1218));
    for (row, tone) in tones.iter().enumerate() {
        let y = row as u32 * CELL;
        undertones.number(margin, y + 6, row + 1, 4);
        for (u, undertone) in Undertone::ALL.into_iter().enumerate() {
            let slots = skin_ramp(tone, undertone);
            let x = margin + 28 + u as u32 * (CELL + chips_width);
            undertones.paste(
                &render(&slots, tone.params, Subject::FaceHead, &Look { rig: &RIGS[0], size: CELL, graded: true }),
                x,
                y,
            );
            chips(&mut undertones, x + CELL, y, chips_width, &slots);
        }
    }

    if let Some(directory) = std::env::var_os("OMUZAKI_SKIN_SHEET") {
        write_sheet(Path::new(&directory), "contact-sheet.png", &sheet);
        write_sheet(Path::new(&directory), "undertones.png", &undertones);
    }
}

// ------------------------------------------------------------------------------------------------ the ops

fn run(doc: &MemDoc, name: &str, args: Value) -> Result<Plan, OpError> {
    let registration = find(name).unwrap_or_else(|| panic!("{name} is not registered"));
    let op = (registration.decode)(args)?;
    let (actor, clock) = (Actor::Person { name: "tester".into() }, FakeClock::default());
    op.plan(&OpCtx::new(doc, &actor, &clock))
}

fn character(minimal: bool) -> Value {
    if minimal {
        // No palette, no face and no params on the parts: every missing parent has to be created whole.
        json!({ "name": "Mira", "template": "soft-sd", "parts": {
            "body": { "kind": "body" }, "head": { "kind": "head" }, "hair": { "kind": "hair" } } })
    } else {
        json!({ "name": "Mira", "template": "soft-sd", "palette": { "hair_root": "#112233", "skin": "#ffffff" },
            "parts": {
                "body": { "kind": "body", "params": { "skin": { "base": "#ffffff", "rim": "#abcdef" } } },
                "head": { "kind": "head", "params": { "skin": { "shade": "#000000" } } },
                "hair": { "kind": "hair", "params": { "keep": 1 } } },
            "face": { "cheeks": { "colour": "#f08c84" }, "mouth": { "lips": { "strength": 0.4 } } } })
    }
}

fn doc_with(character: &Value) -> MemDoc {
    MemDoc::from_value(json!({ "characters": { "mira": character } }))
}

fn set_tone_args(tone: &str, undertone: Option<&str>) -> Value {
    match undertone {
        Some(u) => json!({ "character": "mira", "tone": tone, "undertone": u }),
        None => json!({ "character": "mira", "tone": tone }),
    }
}

fn path(p: &str) -> DocPath {
    DocPath::parse(p).unwrap()
}

#[test]
fn set_skin_tone_writes_everything_in_one_patch_and_undo_restores_the_document() {
    for minimal in [false, true] {
        let mut doc = doc_with(&character(minimal));
        let before = doc.root().clone();
        let plan = run(&doc, "palette.set_skin_tone", set_tone_args("tone-07", Some("warm"))).unwrap();
        doc.apply(&plan.patch).unwrap();

        let tone = &skin_tones().unwrap()[6];
        let slots = skin_ramp(tone, Undertone::Warm);
        let def: CharacterDef = doc.get_as_character("mira");
        for (name, colour) in slots.entries() {
            assert_eq!(def.palette.0.get(&name.parse().unwrap()), Some(&colour), "slot {name} (minimal: {minimal})");
        }
        assert_eq!(def.skin_tone, Some(SkinToneId::new("tone-07-warm").unwrap()));
        for part in ["head", "body"] {
            let skin = &def.parts[&part.parse().unwrap()].params["skin"];
            assert_eq!(
                *skin,
                json!({ "base": "@skin", "shade": "@skin_shade", "line": "@skin_line", "rim": "@skin_rim" })
            );
        }
        let hair = &def.parts[&"hair".parse().unwrap()].params;
        assert!(hair.get("skin").is_none(), "other parts are left alone: {hair}");
        assert_eq!(def.face.cheeks.colour, ColourRef::parse("@blush").unwrap());
        assert_eq!(def.face.mouth.lips.colour, ColourRef::parse("@lip").unwrap());
        if !minimal {
            assert_eq!(
                def.palette.0.get(&"hair_root".parse().unwrap()),
                Some(&Colour::hex(0x0011_2233)),
                "other slots stay"
            );
            assert!((def.face.mouth.lips.strength - 0.4).abs() < 1e-9, "other face settings stay");
        }

        // The summary names what to render with, and applying it again changes nothing.
        let output = plan.output.unwrap();
        assert_eq!(output["skin_tone"], "tone-07-warm");
        assert_eq!(output["parts"], json!(["body", "head"]));
        assert!(run(&doc, "palette.set_skin_tone", set_tone_args("tone-07", Some("warm"))).unwrap().patch.is_empty());

        doc.apply(&plan.patch.inverse()).unwrap();
        assert_eq!(*doc.root(), before, "apply then undo gives an identical document (minimal: {minimal})");
    }
}

trait CharacterRead {
    fn get_as_character(&self, id: &str) -> CharacterDef;
}

impl CharacterRead for MemDoc {
    fn get_as_character(&self, id: &str) -> CharacterDef {
        CharacterDef::from_node(self.get(&path(&format!("characters/{id}"))).unwrap()).unwrap()
    }
}

/// A character whose node also holds the other files' sub-trees, as a project with a variant, a colourway, a rig and a
/// look does.
fn character_with_siblings() -> Value {
    let mut node = character(false);
    node["variants"] =
        json!({ "close-up": { "description": "face only", "overrides": { "parts/head/enabled": true } } });
    node["colourways"] = json!({ "winter": { "skin": "#eeeeff" } });
    node["rig"] = json!({ "extra_bones": [] });
    node["look"] = json!({ "light": "studio" });
    node["licence"] = json!({ "owner": "mira" });
    node
}

#[test]
fn set_skin_tone_works_on_a_character_that_has_a_variant_and_leaves_the_other_files_alone() {
    let mut doc = doc_with(&character_with_siblings());
    let before = doc.root().clone();
    let plan = run(&doc, "palette.set_skin_tone", set_tone_args("tone-07", Some("warm"))).unwrap();
    doc.apply(&plan.patch).unwrap();

    let def = doc.get_as_character("mira");
    assert_eq!(def.skin_tone, Some(SkinToneId::new("tone-07-warm").unwrap()));
    assert_eq!(plan.output.unwrap()["parts"], json!(["body", "head"]));
    for sibling in ["variants", "colourways", "rig", "look", "licence"] {
        assert_eq!(doc.root()["characters"]["mira"][sibling], before["characters"]["mira"][sibling], "{sibling}");
    }
    assert!(!doc.root().to_string().contains("null"), "no null reaches the project: {}", doc.root());
    doc.apply(&plan.patch.inverse()).unwrap();
    assert_eq!(*doc.root(), before, "undo restores the document");
}

#[test]
fn the_undertone_defaults_to_neutral_and_the_tone_must_exist() {
    let doc = doc_with(&character(false));
    let plan = run(&doc, "palette.set_skin_tone", set_tone_args("tone-01", None)).unwrap();
    assert_eq!(plan.output.unwrap()["skin_tone"], "tone-01-neutral");

    let e = run(&doc, "palette.set_skin_tone", set_tone_args("tone-13", None)).unwrap_err();
    assert_eq!((e.code, e.field.as_deref()), (ErrorCode::Invalid, Some("tone")));
    assert!(e.hint.unwrap().contains("tone-12"), "the hint lists the valid tones");

    let e = run(&doc, "palette.set_skin_tone", json!({ "character": "nobody", "tone": "tone-01" })).unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound);
    assert!(run(&doc, "palette.set_skin_tone", set_tone_args("tone-01", Some("golden"))).is_err());
}

#[test]
fn the_skin_tones_query_lists_the_range_and_changes_nothing() {
    let doc = doc_with(&character(false));
    let all = run(&doc, "palette.skin_tones", json!({})).unwrap();
    assert!(all.patch.is_empty());
    let listed = all.output.unwrap();
    assert_eq!(listed["tones"].as_array().unwrap().len(), 36);
    let cool = run(&doc, "palette.skin_tones", json!({ "undertone": "cool" })).unwrap().output.unwrap();
    let cool = cool["tones"].as_array().unwrap();
    assert_eq!(cool.len(), 12);
    assert_eq!(cool[11]["tone"], "tone-12");
    assert!((cool[0]["skin_params"]["floor"].as_f64().unwrap() - 0.32).abs() < 1e-6);
    assert!(cool[0]["slots"]["skin"].as_str().unwrap().starts_with('#'));
}

#[test]
fn both_ops_are_registered_with_the_right_scope_and_undo() {
    let meta = |name: &str| (find(name).unwrap().meta)();
    let set = meta("palette.set_skin_tone");
    assert_eq!((set.kind, set.scope, set.undo), (OpKind::Edit, Scope::EditCharacter, Undo::Auto));
    let list = meta("palette.skin_tones");
    assert_eq!((list.kind, list.scope, list.undo), (OpKind::Query, Scope::Read, Undo::None));
}

// --------------------------------------------------------------------------------------------- the treatment by colour

/// The lightness of a colour, as [`SkinToneParams::for_colour`] reads it.
fn lightness(c: Colour) -> f64 {
    Oklch::from_colour(c).l
}

#[test]
fn for_colour_returns_each_tones_tuned_values_exactly_at_its_colour() {
    for tone in skin_tones().unwrap() {
        let skin = skin_ramp(tone, Undertone::Neutral).skin;
        assert_eq!(SkinToneParams::for_colour(skin), tone.params, "{}", tone.id);
    }
}

#[test]
fn warm_and_cool_skins_get_their_tones_treatment_to_within_a_hair() {
    // The table is keyed on the neutral skin, so a warm or cool skin of the same tone lies a hair off its lightness.
    for tone in skin_tones().unwrap() {
        for undertone in [Undertone::Warm, Undertone::Cool] {
            let got = SkinToneParams::for_colour(skin_ramp(tone, undertone).skin);
            let off = (got.floor - tone.params.floor).abs().max((got.hue_keep - tone.params.hue_keep).abs());
            assert!(off < 0.005, "{} {}: {off}", tone.id, undertone.as_str());
        }
    }
}

#[test]
fn floor_never_rises_as_the_skin_gets_lighter() {
    // Sweep the lightness finely at a warm mid chroma. Order the samples by the lightness each colour really has
    // (8-bit rounding moves it a little), so the check is on the function, not on the sampling.
    let mut samples: Vec<(f64, SkinToneParams)> = (0..=700_u32)
        .map(|i| {
            let colour = Oklch { l: 0.30 + f64::from(i) * 0.001, c: 0.07, h: 48.0 }.to_colour();
            (lightness(colour), SkinToneParams::for_colour(colour))
        })
        .collect();
    samples.sort_by(|a, b| a.0.total_cmp(&b.0));
    for pair in samples.windows(2) {
        assert!(pair[1].1.floor <= pair[0].1.floor, "floor rises from {:?} to {:?}", pair[0], pair[1]);
    }
}

#[test]
fn a_colour_halfway_in_lightness_between_two_tones_gets_values_between_theirs() {
    let tones = skin_tones().unwrap();
    for pair in tones.windows(2) {
        let (light, dark) = (&pair[0], &pair[1]);
        let light_l = lightness(skin_ramp(light, Undertone::Neutral).skin);
        let dark_l = lightness(skin_ramp(dark, Undertone::Neutral).skin);
        let middle = Oklch { l: f64::midpoint(light_l, dark_l), c: 0.07, h: 48.0 }.to_colour();
        let got = SkinToneParams::for_colour(middle);

        let between = |v: f32, a: f32, b: f32| v >= a.min(b) && v <= a.max(b);
        assert!(between(got.floor, light.params.floor, dark.params.floor), "{}-{} floor {got:?}", light.id, dark.id);
        assert!(between(got.hue_keep, light.params.hue_keep, dark.params.hue_keep), "{} {got:?}", light.id);
        // Halfway in lightness is halfway in value, to the 8-bit rounding of the colour.
        let halfway_floor = f32::midpoint(light.params.floor, dark.params.floor);
        let halfway_keep = f32::midpoint(light.params.hue_keep, dark.params.hue_keep);
        assert!((got.floor - halfway_floor).abs() < 0.005, "{} {}: {got:?}", light.id, dark.id);
        assert!((got.hue_keep - halfway_keep).abs() < 0.005, "{} {}: {got:?}", light.id, dark.id);
    }
}

#[test]
fn a_colour_beyond_either_end_of_the_range_clamps_to_that_end_tone() {
    let tones = skin_tones().unwrap();
    let lightest = tones[0].params;
    let deepest = tones[11].params;
    let lightest_l = lightness(skin_ramp(&tones[0], Undertone::Neutral).skin);
    let deepest_l = lightness(skin_ramp(&tones[11], Undertone::Neutral).skin);

    for light in [Colour::rgb(255, 255, 255), Oklch { l: 0.98, c: 0.05, h: 47.0 }.to_colour()] {
        assert!(lightness(light) > lightest_l, "{light:?} is not lighter than the lightest tone");
        assert_eq!(SkinToneParams::for_colour(light), lightest);
    }
    for dark in [Colour::rgb(0, 0, 0), Oklch { l: 0.2, c: 0.02, h: 47.0 }.to_colour()] {
        assert!(lightness(dark) < deepest_l, "{dark:?} is not darker than the deepest tone");
        assert_eq!(SkinToneParams::for_colour(dark), deepest);
    }
}
