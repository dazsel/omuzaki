//! Material and texture conversion: the renderer's toon materials against the file's `MToon` ones, and the kit's
//! pixels against PNG.
//!
//! The round-trip tests print their tables; run them with `--nocapture` to see the evidence the PR quotes.

// Test helpers may panic: a panic is a clear failure. The casts build random values from small integers.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
// `io::Toon` is `#[non_exhaustive]`: from outside its crate only assignment can fill it.
#![allow(clippy::field_reassign_with_default)]

use std::collections::BTreeSet;

use omuzaki_char_api::{FaceOverlay, MaterialSpec, TextureData, TextureName};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{UVec2, Vec2, Vec3, Vec4};
use omuzaki_core::rng::Rng;
use omuzaki_face_api::{FaceLayerId, PlanarWindow};
use omuzaki_io_api::{self as io, Blob, EncodedImage, ImageMime, Report, Severity};
use omuzaki_io_ops::convert::colour::{from_linear, to_linear};
use omuzaki_io_ops::convert::material::{SceneTexture, export_material, import_material};
use omuzaki_io_ops::convert::texture::{decode_png, encode_png, sampler_to_io, slot_colour_space};
use omuzaki_render_api as render;
use omuzaki_render_api::{AlphaEncoding, ColourSpace, TextureDesc};

// ---------------------------------------------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------------------------------------------

const CLOTH: &str = "cloth";

fn cloth() -> TextureName {
    TextureName::new(CLOTH).unwrap()
}

/// The scene texture the name `cloth` is, as the gather step would have added it.
fn scene_cloth() -> io::TexRef {
    io::TexRef::of(io::TextureIx(7))
}

fn textures(name: &TextureName) -> Option<SceneTexture> {
    (name.as_str() == CLOTH).then_some(SceneTexture { tex: scene_cloth(), colour_space: ColourSpace::Srgb })
}

fn names(texture: &io::TexRef) -> Option<TextureName> {
    (texture.texture == scene_cloth().texture).then(cloth)
}

fn export(spec: &MaterialSpec) -> (io::Material, Report) {
    let mut report = Report::new();
    let material = export_material("skin", spec, &textures, "materials/0", &mut report).unwrap();
    (material, report)
}

fn import(material: &io::Material) -> (MaterialSpec, Report) {
    let mut report = Report::new();
    let spec = import_material(material, &names, "materials/0", &mut report);
    (spec, report)
}

fn round_trip(spec: &MaterialSpec) -> MaterialSpec {
    import(&export(spec).0).0
}

fn toon_of(spec: &MaterialSpec) -> render::Toon {
    match &spec.shading {
        render::Shading::Toon(t) => *t,
        other => panic!("expected a toon, got {other:?}"),
    }
}

fn mtoon_of(material: &io::Material) -> &io::Toon {
    match &material.shading {
        io::Shading::Toon(t) => t,
        other => panic!("expected MToon, got {other:?}"),
    }
}

fn rules(report: &Report) -> Vec<&str> {
    report.findings.iter().map(|f| f.rule.as_str()).collect()
}

fn locations(report: &Report) -> Vec<&str> {
    report.findings.iter().map(|f| f.location.as_deref().unwrap()).collect()
}

/// A value on a 2^-10 grid in `lo..=hi`: what a slider or a JSON file holds, and exactly representable after `1 - x`.
fn grid(rng: &mut Rng, lo: f32, hi: f32) -> f32 {
    let steps = ((hi - lo) * 1024.0) as u64;
    lo + (rng.next_u64() % (steps + 1)) as f32 / 1024.0
}

fn byte(rng: &mut Rng) -> u8 {
    rng.next_u64().to_le_bytes()[0]
}

/// An opaque colour with every channel above 0 (see `shade` in the module docs of `material`).
fn colour(rng: &mut Rng) -> Colour {
    Colour::rgb(byte(rng).max(1), byte(rng).max(1), byte(rng).max(1))
}

fn opaque(rng: &mut Rng) -> Colour {
    Colour::rgb(byte(rng), byte(rng), byte(rng))
}

/// A random toon material using only what both sides have, with the rim strength set to `strength`.
fn random_toon(rng: &mut Rng, strength: f32) -> MaterialSpec {
    let mut base = colour(rng);
    base.a = byte(rng);
    let mut spec = MaterialSpec::toon(base);
    spec.shading = render::Shading::Toon(render::Toon {
        shade: opaque(rng),
        shift: grid(rng, -1.0, 1.0),
        softness: grid(rng, 0.0, 1.0),
        rim: render::Rim { colour: opaque(rng), power: grid(rng, 0.0, 12.0) + 1.0 / 1024.0, strength },
        emissive: opaque(rng),
        skin: None,
    });
    if rng.next_f32() < 0.5 {
        spec.outline = Some(render::Outline { colour: opaque(rng), width: grid(rng, 0.0, 0.05) });
    }
    spec.alpha = match rng.next_u64() % 3 {
        0 => render::AlphaMode::Opaque,
        1 => render::AlphaMode::Mask { cutoff: grid(rng, 0.0, 1.0) },
        _ => render::AlphaMode::Blend,
    };
    spec.double_sided = rng.next_f32() < 0.5;
    if rng.next_f32() < 0.5 {
        spec.base_texture = Some(cloth());
    }
    spec
}

// ---------------------------------------------------------------------------------------------------------------
// Toon -> MToon -> Toon
// ---------------------------------------------------------------------------------------------------------------

type Check = fn(&MaterialSpec, &MaterialSpec) -> bool;

/// Bit-for-bit equality: stricter than `==`, which is the claim the round trip makes.
fn same(a: f32, b: f32) -> bool {
    a.to_bits() == b.to_bits()
}

/// Every field both sides have, and how it is compared. `rim.colour` and `rim.strength` are not here: they fold into
/// one colour (see `rim_strength_is_folded_into_the_colour`), and only strength 1 returns exactly.
const FIELDS: [(&str, Check); 14] = [
    ("base (colour + alpha)", |a, b| a.base == b.base),
    ("base_texture", |a, b| a.base_texture == b.base_texture),
    ("shading.shade", |a, b| toon_of(a).shade == toon_of(b).shade),
    ("shading.shift", |a, b| same(toon_of(a).shift, toon_of(b).shift)),
    ("shading.softness", |a, b| same(toon_of(a).softness, toon_of(b).softness)),
    ("shading.rim.colour", |a, b| toon_of(a).rim.colour == toon_of(b).rim.colour),
    ("shading.rim.power", |a, b| same(toon_of(a).rim.power, toon_of(b).rim.power)),
    ("shading.rim.strength", |a, b| same(toon_of(a).rim.strength, toon_of(b).rim.strength)),
    ("shading.emissive", |a, b| toon_of(a).emissive == toon_of(b).emissive),
    ("outline.colour", |a, b| a.outline.map(|o| o.colour) == b.outline.map(|o| o.colour)),
    ("outline.width", |a, b| a.outline.map(|o| o.width) == b.outline.map(|o| o.width)),
    ("alpha (mode + cutoff)", |a, b| a.alpha == b.alpha),
    ("double_sided", |a, b| a.double_sided == b.double_sided),
    ("shading.skin (None)", |a, b| toon_of(a).skin == toon_of(b).skin),
];

#[test]
fn five_hundred_random_toon_materials_round_trip_field_by_field() {
    let mut rng = Rng::new(0x559);
    let mut mismatches = [0usize; FIELDS.len()];
    for n in 0..500 {
        // Strength 1 is the value an import produces, so it is the one that must come back; the folding of other
        // strengths is tested on its own.
        let spec = random_toon(&mut rng, 1.0);
        let (file, report) = export(&spec);
        assert!(report.findings.iter().all(|f| f.severity == Severity::Info), "material {n}: {:?}", report.findings);
        let back = import(&file).0;
        for (i, (field, same)) in FIELDS.iter().enumerate() {
            if !same(&spec, &back) {
                mismatches[i] += 1;
                eprintln!("material {n}: {field} differs: {spec:?} -> {back:?}");
            }
        }
        // Everything the renderer has and the file lacks keeps its default.
        assert!(back.overlays.is_empty() && back.receives_shadows && !back.vertex_colours);
    }
    println!("\n| field | materials | exact | differing |\n|---|---|---|---|");
    for ((field, _), bad) in FIELDS.iter().zip(mismatches) {
        println!("| {field} | 500 | {} | {bad} |", 500 - bad);
    }
    assert_eq!(mismatches, [0; FIELDS.len()], "every field both sides have must come back exactly");
}

#[test]
fn the_exported_mtoon_fields_are_what_other_viewers_read() {
    let mut spec = MaterialSpec::toon(Colour::hex(0x00f9_d3c5));
    spec.shading = render::Shading::Toon(render::Toon {
        shade: Colour::hex(0x00c0_a0b0),
        shift: 0.25,
        softness: 0.125,
        rim: render::Rim { colour: Colour::hex(0x00ff_ffff), power: 4.0, strength: 0.5 },
        emissive: Colour::hex(0x0020_1000),
        skin: None,
    });
    spec.outline = Some(render::Outline { colour: Colour::hex(0x0040_3030), width: 0.01 });
    spec.base_texture = Some(cloth());
    spec.alpha = render::AlphaMode::Mask { cutoff: 0.5 };
    let (file, _) = export(&spec);
    let m = mtoon_of(&file);

    let base = to_linear(spec.base);
    assert_eq!(file.base_color, base);
    let base = base.truncate();
    assert_eq!(file.base_color_texture, Some(scene_cloth()));
    assert_eq!(m.shade_color, Colour::hex(0x00c0_a0b0).to_linear() * base, "shade is absolute in a file");
    assert_eq!(m.shade_multiply_texture, Some(scene_cloth()), "the shaded side keeps the texture");
    assert_eq!((m.shading_shift, m.shading_toony), (-0.25, 0.875), "MToon adds the shift and 1 means a hard edge");
    assert_eq!(m.rim_color, Vec3::splat(0.5), "white at half strength");
    assert_eq!(m.rim_fresnel_power, 4.0);
    assert_eq!(m.outline_mode, io::OutlineMode::WorldCoordinates);
    assert_eq!((m.outline_width, m.outline_color), (0.01, Colour::hex(0x0040_3030).to_linear()));
    assert_eq!(file.emissive, Colour::hex(0x0020_1000).to_linear());
    assert_eq!(file.alpha, io::AlphaMode::Mask { cutoff: 0.5 });
    // What the renderer lacks takes the MToon defaults, written out.
    let defaults = io::Toon::default();
    assert_eq!((m.gi_equalization, m.matcap_color, m.rim_lighting_mix), (0.9, Vec3::ONE, 1.0));
    assert_eq!((m.uv_scroll_x, m.render_queue_offset, m.transparent_with_zwrite), (0.0, 0, false));
    assert_eq!(m.outline_lighting_mix, defaults.outline_lighting_mix);
}

#[test]
fn shade_round_trips_for_every_base_and_shade_byte() {
    // Each channel is converted on its own, so sweeping all pairs on all three channels at once covers every one.
    for base in 1..=255u8 {
        for shade in 0..=255u8 {
            let mut spec = MaterialSpec::toon(Colour::rgb(base, base, base));
            spec.shading = render::Shading::Toon(render::Toon {
                shade: Colour::rgb(shade, shade.wrapping_add(1), 255 - shade),
                ..render::Toon::default()
            });
            assert_eq!(toon_of(&round_trip(&spec)).shade, toon_of(&spec).shade, "base {base}, shade {shade}");
        }
    }
}

#[test]
fn shade_under_a_black_base_channel_is_invisible_and_imports_as_neutral() {
    let mut spec = MaterialSpec::toon(Colour::rgb(0, 200, 0));
    spec.shading = render::Shading::Toon(render::Toon { shade: Colour::rgb(10, 20, 30), ..render::Toon::default() });
    let shade = toon_of(&round_trip(&spec)).shade;
    assert_eq!((shade.r, shade.g, shade.b), (255, 20, 255), "channels over a black base are free; 1 is the neutral");
}

#[test]
fn a_shade_brighter_than_the_base_is_capped_and_reported() {
    let mut file = io::Material::new("m", io::Shading::Toon(Box::default()));
    file.base_color = Vec4::new(0.5, 0.5, 0.5, 1.0);
    let mut toon = io::Toon::default();
    toon.shade_color = Vec3::new(0.25, 0.75, 0.5);
    file.shading = io::Shading::Toon(Box::new(toon));
    let (spec, report) = import(&file);
    let shade = toon_of(&spec).shade;
    assert_eq!((shade.r, shade.g, shade.b), (from_linear(Vec4::new(0.5, 0.5, 0.5, 1.0)).r, 255, 255));
    assert_eq!(rules(&report), ["material.approximated"]);
    assert_eq!(locations(&report), ["materials/0/shading/shade_color"]);
}

#[test]
fn softness_and_shift_for_arbitrary_floats_return_within_half_an_ulp_of_one() {
    // `1 - (1 - x)` is exact on a coarse grid (tested above) but rounds once for a general f32: the error is at most
    // half an ulp of 1.0 below it, 2^-25.
    let mut rng = Rng::new(0x5f7);
    let mut worst = 0.0f32;
    for _ in 0..10_000 {
        // A product of two 24-bit fractions has a full mantissa at any scale, unlike a plain `next_f32`, whose values
        // are all multiples of 2^-24 and so subtract exactly.
        let (softness, shift) = (rng.next_f32() * rng.next_f32(), rng.range_f32(-1.0, 1.0));
        let mut spec = MaterialSpec::toon(Colour::WHITE);
        spec.shading = render::Shading::Toon(render::Toon { softness, shift, ..render::Toon::default() });
        let back = toon_of(&round_trip(&spec));
        assert_eq!(back.shift, shift, "negating twice is exact");
        worst = worst.max((back.softness - softness).abs());
    }
    println!("worst softness error over 10000 random f32: {worst:e} (limit {:e})", 2.0f32.powi(-25));
    assert!(worst <= 2.0f32.powi(-25));
}

#[test]
fn rim_strength_is_folded_into_the_colour() {
    let mut rng = Rng::new(0x817);
    // The effective rim is the linear colour times strength. Import rounds it to a byte, which moves a linear value by
    // at most half the widest step of the sRGB curve (254 -> 255: 0.0089).
    let half_step = 0.0045;
    let mut worst = 0.0f32;
    for _ in 0..500 {
        let strength = rng.next_f32();
        let spec = random_toon(&mut rng, strength);
        let (before, after) = (toon_of(&spec).rim, toon_of(&round_trip(&spec)).rim);
        assert_eq!(after.strength, 1.0, "an import always yields strength 1");
        assert_eq!(after.power, before.power);
        let error = (after.colour.to_linear() - before.colour.to_linear() * before.strength).abs().max_element();
        worst = worst.max(error);
    }
    println!("worst effective rim error over 500 random strengths: {worst:e} (limit {half_step})");
    assert!(worst <= half_step);
}

#[test]
fn a_rim_brighter_than_one_is_capped_and_reported() {
    let mut spec = MaterialSpec::toon(Colour::WHITE);
    spec.shading = render::Shading::Toon(render::Toon {
        rim: render::Rim { colour: Colour::WHITE, power: 3.0, strength: 2.0 },
        ..render::Toon::default()
    });
    let (file, report) = export(&spec);
    assert_eq!(mtoon_of(&file).rim_color, Vec3::ONE);
    assert_eq!(rules(&report), ["material.approximated"]);
    assert_eq!(locations(&report), ["materials/0/shading/rim_color"]);
}

#[test]
fn out_of_range_values_are_limited_and_reported() {
    let mut spec = MaterialSpec::toon(Colour::WHITE);
    spec.shading = render::Shading::Toon(render::Toon { shift: 3.0, softness: -1.0, ..render::Toon::default() });
    let (file, report) = export(&spec);
    let m = mtoon_of(&file);
    assert_eq!((m.shading_shift, m.shading_toony), (-1.0, 1.0));
    assert_eq!(locations(&report), ["materials/0/shading/shading_shift", "materials/0/shading/shading_toony"]);
}

#[test]
fn the_default_toon_exports_without_a_warning_and_keeps_its_look() {
    let spec = MaterialSpec::toon(Colour::hex(0x00f9_d3c5));
    let (file, report) = export(&spec);
    assert!(report.findings.is_empty(), "{:?}", report.findings);
    let (before, after) = (toon_of(&spec), toon_of(&import(&file).0));
    assert_eq!((after.shade, after.shift, after.emissive), (before.shade, before.shift, before.emissive));
    assert_eq!(after.rim.power, before.rim.power);
    // The default rim is a colour at strength 0.18; it comes back as the same effective colour at strength 1.
    let effective = before.rim.colour.to_linear() * before.rim.strength;
    assert!((after.rim.colour.to_linear() - effective).abs().max_element() < 0.0045);
}

// ---------------------------------------------------------------------------------------------------------------
// What the file cannot hold
// ---------------------------------------------------------------------------------------------------------------

fn program(name: &str) -> render::Shading {
    render::Shading::Program {
        name: omuzaki_core::id::Slug::new(name).unwrap(),
        params: std::collections::BTreeMap::new(),
    }
}

#[test]
fn a_program_exports_as_the_default_toon_look_with_a_finding_naming_it() {
    for name in ["hair", "sheen", "band-highlight"] {
        let mut spec = MaterialSpec::toon(Colour::hex(0x00e8_c070));
        spec.shading = program(name);
        spec.vertex_colours = true;
        spec.outline = Some(render::Outline { colour: Colour::BLACK, width: 0.004 });
        spec.double_sided = true;
        let (file, report) = export(&spec);

        let fallback =
            export(&MaterialSpec { shading: render::Shading::Toon(render::Toon::default()), ..spec.clone() }).0;
        assert_eq!(file.shading, fallback.shading, "{name}: the documented fallback is the default toon look");
        assert_eq!(file.base_color, to_linear(spec.base), "{name}: the base colour is kept");
        assert!(file.double_sided && mtoon_of(&file).outline_mode == io::OutlineMode::WorldCoordinates);
        let finding = report.findings.iter().find(|f| f.rule == "material.program_dropped").expect("a finding");
        assert_eq!(finding.severity, Severity::Warning);
        assert!(finding.message.contains(name), "{}", finding.message);
        assert_eq!(finding.location.as_deref(), Some("materials/0/shading"));
        assert!(
            report.has_rule("material.vertex_colours_ignored"),
            "the mesh keeps its colours, the file ignores them"
        );
    }
}

#[test]
fn the_kit_hair_exports_in_its_mean_strand_colour_with_its_toon() {
    let colour = |hex: u32| render::ParamValue::Colour(Colour::hex(hex));
    let slug = |name: &str| omuzaki_core::id::Slug::new(name).unwrap();
    let (root, mid, tip, shade) = (0x0030_2010, 0x0060_4020, 0x0090_6030, 0x0080_7060);
    let params = std::collections::BTreeMap::from([
        (slug("root"), colour(root)),
        (slug("mid"), colour(mid)),
        (slug("tip"), colour(tip)),
        (slug("shade"), colour(shade)),
        (slug("edge-darkening"), render::ParamValue::Float(0.0)),
    ]);
    let mut spec = MaterialSpec::toon(Colour::WHITE);
    spec.shading = render::Shading::Program { name: slug("hair"), params };
    let (file, report) = export(&spec);

    let linear = |hex: u32| to_linear(Colour::hex(hex)).truncate();
    let strand = linear(root) * 0.125 + linear(mid) * 0.65 + linear(tip) * 0.225;
    assert!((file.base_color.truncate() - strand).abs().max_element() < 1e-5, "{:?}", file.base_color);
    let toon = toon_of(&import(&file).0);
    assert!((toon.shift - -0.05).abs() < 1e-5 && (toon.softness - 0.22).abs() < 1e-5, "the program's terminator");
    assert!((toon.shade.to_linear() - linear(shade)).abs().max_element() < 0.0045, "the shade parameter");
    assert!(!report.has_rule("material.program_dropped"));
    let finding = report.findings.iter().find(|f| f.rule == "material.approximated").expect("a finding");
    assert_eq!(finding.location.as_deref(), Some("materials/0/shading"));
}

#[test]
fn skin_has_no_mtoon_field_and_says_so() {
    let mut spec = MaterialSpec::toon(Colour::hex(0x00f9_d3c5));
    spec.shading =
        render::Shading::Toon(render::Toon { skin: Some(render::Skin::default()), ..render::Toon::default() });
    let (file, report) = export(&spec);
    assert_eq!(rules(&report), ["material.skin_dropped"]);
    assert_eq!(locations(&report), ["materials/0/shading/skin"]);
    assert_eq!(toon_of(&import(&file).0).skin, None);
}

#[test]
fn everything_else_the_file_lacks_is_reported_once_each() {
    let mut spec = MaterialSpec::toon(Colour::WHITE);
    spec.vertex_colours = true;
    spec.receives_shadows = false;
    spec.overlays = vec![
        FaceOverlay {
            layer: FaceLayerId::Base,
            window: PlanarWindow { min: Vec2::ZERO, max: Vec2::ONE, fade_from: 0.0, fade_to: 0.1 },
            strength: 1.0,
        };
        2
    ];
    spec.outline = Some(render::Outline { colour: Colour::BLACK, width: 0.01 });
    let (_, report) = export(&spec);
    assert_eq!(
        rules(&report),
        ["material.not_exported", "material.not_exported", "material.vertex_colours_ignored", "material.not_exported"]
    );
    assert_eq!(
        locations(&report),
        ["materials/0/outline", "materials/0/overlays", "materials/0/vertex_colours", "materials/0/receives_shadows"]
    );
}

#[test]
fn an_unlit_material_stays_unlit_and_a_line_around_it_is_reported() {
    let mut spec = MaterialSpec::toon(Colour::hex(0x0080_c0ff));
    spec.shading = render::Shading::Unlit;
    spec.vertex_colours = true;
    let (file, report) = export(&spec);
    assert_eq!(file.shading, io::Shading::Unlit);
    assert!(report.findings.is_empty(), "vertex colours multiply an unlit material in glTF: {:?}", report.findings);
    assert_eq!(import(&file).0.shading, render::Shading::Unlit);

    spec.outline = Some(render::Outline { colour: Colour::BLACK, width: 0.01 });
    assert_eq!(rules(&export(&spec).1), ["material.outline_dropped"]);
}

#[test]
fn a_missing_texture_is_an_error_and_a_wrong_colour_space_is_a_warning() {
    let mut spec = MaterialSpec::toon(Colour::WHITE);
    spec.base_texture = Some(TextureName::new("nowhere").unwrap());
    let err = export_material("skin", &spec, &textures, "materials/0", &mut Report::new()).unwrap_err();
    assert_eq!(err.to_string(), "material 'skin' uses texture 'nowhere', which the scene doesn't have");

    spec.base_texture = Some(cloth());
    let linear = |_: &TextureName| Some(SceneTexture { tex: scene_cloth(), colour_space: ColourSpace::Linear });
    let mut report = Report::new();
    export_material("skin", &spec, &linear, "materials/0", &mut report).unwrap();
    assert_eq!(rules(&report), ["material.texture_colour_space"]);
}

// ---------------------------------------------------------------------------------------------------------------
// MToon -> Toon: what the renderer cannot draw
// ---------------------------------------------------------------------------------------------------------------

/// A texture reference to scene texture `n`, as a file's material slot would hold it.
fn slot(n: u32) -> io::TexRef {
    io::TexRef::of(io::TextureIx(n))
}

fn busy_mtoon() -> io::Material {
    let mut t = io::Toon::default();
    t.gi_equalization = 0.4;
    t.matcap_texture = Some(slot(1));
    t.uv_scroll_x = 0.5;
    t.uv_rotation = 0.25;
    t.render_queue_offset = -3;
    t.transparent_with_zwrite = true;
    t.shading_shift_texture = Some(io::ShiftTexture { texture: io::TexRef::of(io::TextureIx(2)), scale: 1.0 });
    t.rim_lift = 0.1;
    t.rim_lighting_mix = 0.5;
    t.rim_multiply_texture = Some(slot(3));
    t.outline_mode = io::OutlineMode::WorldCoordinates;
    t.outline_width_texture = Some(slot(4));
    t.outline_lighting_mix = 0.3;
    t.shade_multiply_texture = Some(slot(5));
    let mut m = io::Material::new("busy", io::Shading::Toon(Box::new(t)));
    m.alpha = io::AlphaMode::Blend;
    m
}

#[test]
fn every_field_the_renderer_cannot_draw_is_reported_once_with_its_path() {
    let (_, report) = import(&busy_mtoon());
    let expected = [
        "gi_equalization",
        "matcap_texture",
        "uv_animation",
        "render_queue_offset",
        "transparent_with_zwrite",
        "shading_shift_texture",
        "rim_lift",
        "rim_lighting_mix",
        "rim_multiply_texture",
        "outline_width_texture",
        "outline_lighting_mix",
        "shade_multiply_texture",
    ]
    .map(|field| format!("materials/0/shading/{field}"));
    assert_eq!(locations(&report), expected, "in a fixed order, one finding per field");
    assert!(report.findings.iter().all(|f| f.severity == Severity::Info && f.rule == "material.not_drawn"));
    let unique: BTreeSet<_> = locations(&report).into_iter().collect();
    assert_eq!(unique.len(), report.findings.len());
}

#[test]
fn a_plain_mtoon_material_imports_without_findings() {
    let (spec, report) = import(&io::Material::new("plain", io::Shading::Toon(Box::default())));
    assert!(report.findings.is_empty(), "{:?}", report.findings);
    // The spec's own defaults: shade 0 is a black multiplier over a white base, toony 0.9 is a 0.1 softness.
    let toon = toon_of(&spec);
    assert_eq!(toon.shade, Colour::BLACK);
    assert!((toon.softness - 0.1).abs() < 1e-6, "{}", toon.softness);
}

#[test]
fn screen_outlines_are_converted_to_metres_and_reported() {
    let mut t = io::Toon::default();
    t.outline_mode = io::OutlineMode::ScreenCoordinates;
    t.outline_width = 0.01;
    let (spec, report) = import(&io::Material::new("m", io::Shading::Toon(Box::new(t))));
    let width = spec.outline.unwrap().width;
    // 30 degree lens at 2 m: the screen is 2 * 2 * tan(15 deg) = 1.072 m tall.
    assert!((width - 0.010_717).abs() < 1e-5, "{width}");
    assert_eq!(rules(&report), ["material.approximated"]);
}

#[test]
fn pbr_and_textured_slots_import_as_the_default_look_with_findings() {
    let mut file = io::Material::new("metal", io::Shading::Pbr(io::Pbr::default()));
    file.base_color = Vec4::new(0.2, 0.4, 0.6, 0.5);
    file.base_color_texture = Some(io::TexRef { texture: io::TextureIx(7), uv_set: 1, transform: None });
    file.emissive = Vec3::new(4.0, 0.0, 0.0);
    file.emissive_texture = Some(slot(9));
    file.normal_texture = Some(io::NormalTexture { texture: io::TexRef::of(io::TextureIx(8)), scale: 1.0 });
    let (spec, report) = import(&file);
    assert_eq!(spec.base, from_linear(file.base_color));
    assert_eq!(spec.base_texture, Some(cloth()));
    assert_eq!(toon_of(&spec).emissive, Colour::rgb(255, 0, 0));
    assert_eq!(
        locations(&report),
        [
            "materials/0/base_color_texture",
            "materials/0/emissive_texture",
            "materials/0/normal_texture",
            "materials/0/emissive",
            "materials/0/shading/pbr",
        ]
    );
}

#[test]
fn an_unknown_scene_texture_is_dropped_with_a_warning() {
    let mut file = io::Material::new("m", io::Shading::Unlit);
    file.base_color_texture = Some(slot(99));
    let (spec, report) = import(&file);
    assert_eq!(spec.base_texture, None);
    assert_eq!(rules(&report), ["material.texture_missing"]);
}

// ---------------------------------------------------------------------------------------------------------------
// Texture slots and PNG
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn colour_slots_are_srgb_and_data_slots_are_linear() {
    // Fill every slot a material has, so a slot added to the scene without a decision here fails this test.
    let mut t = io::Toon::default();
    let any = Some(slot(0));
    t.shade_multiply_texture = any;
    t.matcap_texture = any;
    t.rim_multiply_texture = any;
    t.outline_width_texture = any;
    t.uv_animation_mask = any;
    t.shading_shift_texture = Some(io::ShiftTexture { texture: slot(0), scale: 1.0 });
    let mut m = io::Material::new("all", io::Shading::Toon(Box::new(t)));
    m.base_color_texture = any;
    m.emissive_texture = any;
    m.normal_texture = Some(io::NormalTexture { texture: slot(0), scale: 1.0 });
    let got: Vec<_> = m.texture_refs().into_iter().map(|(slot, _)| (slot, slot_colour_space(slot))).collect();
    let srgb = |s| (s, ColourSpace::Srgb);
    let linear = |s| (s, ColourSpace::Linear);
    assert_eq!(
        got,
        [
            srgb("baseColorTexture"),
            srgb("emissiveTexture"),
            srgb("shadeMultiplyTexture"),
            srgb("matcapTexture"),
            srgb("rimMultiplyTexture"),
            linear("outlineWidthMultiplyTexture"),
            linear("uvAnimationMaskTexture"),
            linear("shadingShiftTexture"),
            linear("normalTexture"),
        ]
    );
    assert_eq!(slot_colour_space("metallicRoughnessTexture"), ColourSpace::Linear);
}

fn random_texture(rng: &mut Rng, size: UVec2, colour_space: ColourSpace) -> TextureData {
    let len = (size.x * size.y * 4) as usize;
    let rgba8 = (0..len).map(|_| byte(rng)).collect();
    TextureData { desc: TextureDesc { colour_space, ..TextureDesc::colour(size) }, rgba8 }
}

fn png_round_trip(texture: &TextureData) -> (TextureData, Report) {
    let mut report = Report::new();
    let image = encode_png("t", texture, "images/0", &mut report).unwrap();
    assert_eq!(image.mime, ImageMime::Png);
    let sampler = sampler_to_io(&texture.desc);
    let back = decode_png(&image, texture.desc.colour_space, &sampler, "images/0", &mut report).unwrap();
    (back, report)
}

#[test]
fn rgba8_textures_survive_png_byte_for_byte() {
    let mut rng = Rng::new(0x9a9);
    println!(
        "\n| size | colour space | pixels equal | desc equal | PNG bytes | raw bytes |\n|---|---|---|---|---|---|"
    );
    for (size, space) in [
        (UVec2::new(1, 1), ColourSpace::Srgb),
        (UVec2::new(2, 3), ColourSpace::Linear),
        (UVec2::new(16, 16), ColourSpace::Srgb),
        (UVec2::new(64, 31), ColourSpace::Linear),
        (UVec2::new(255, 7), ColourSpace::Srgb),
        (UVec2::new(512, 512), ColourSpace::Srgb),
    ] {
        let texture = random_texture(&mut rng, size, space);
        let (back, report) = png_round_trip(&texture);
        assert_eq!(back.rgba8, texture.rgba8, "{size} {space:?}: pixels are bit-exact");
        assert_eq!(back.desc, texture.desc, "{size} {space:?}: the description comes back, flags included");
        assert!(report.findings.is_empty(), "{:?}", report.findings);
        let png = encode_png("t", &texture, "i", &mut Report::new()).unwrap().bytes.len();
        println!("| {size} | {space:?} | yes | yes | {png} | {} |", texture.rgba8.len());
    }
}

#[test]
fn patterns_with_alpha_edges_survive_png() {
    // Fully transparent pixels with non-zero colour, 0 and 255 alpha, and every byte value in every channel.
    let mut rgba8 = Vec::new();
    for v in 0..=255u8 {
        rgba8.extend_from_slice(&[v, 255 - v, v / 2, v]);
        rgba8.extend_from_slice(&[v, v, v, 0]);
        rgba8.extend_from_slice(&[v, v, v, 255]);
        rgba8.extend_from_slice(&[0, 0, 0, v]);
    }
    let texture = TextureData { desc: TextureDesc::colour(UVec2::new(32, 32)), rgba8 };
    assert_eq!(png_round_trip(&texture).0, texture);
}

#[test]
fn encoding_is_deterministic() {
    let texture = random_texture(&mut Rng::new(5), UVec2::new(40, 40), ColourSpace::Srgb);
    let encode = || encode_png("t", &texture, "i", &mut Report::new()).unwrap();
    assert_eq!(encode(), encode());
}

#[test]
fn the_sampler_comes_back_and_what_the_file_lacks_is_reported() {
    let mut texture = random_texture(&mut Rng::new(6), UVec2::new(4, 4), ColourSpace::Srgb);
    for (wrap, filter, mips) in [
        (render::Wrap::Clamp, render::Filter::Linear, true),
        (render::Wrap::Repeat, render::Filter::Nearest, false),
        (render::Wrap::Repeat, render::Filter::Linear, false),
        (render::Wrap::Clamp, render::Filter::Nearest, true),
    ] {
        texture.desc.sampler = render::Sampler { filter, wrap, anisotropy: 1 };
        texture.desc.mips = mips;
        assert_eq!(png_round_trip(&texture).0.desc, texture.desc);
    }

    texture.desc.sampler.anisotropy = 8;
    texture.desc.usage = render::TextureUsage::Dynamic;
    let (back, report) = png_round_trip(&texture);
    assert_eq!((back.desc.sampler.anisotropy, back.desc.usage), (1, render::TextureUsage::Static));
    assert_eq!(rules(&report), ["texture.anisotropy_dropped"]);
    assert_eq!(locations(&report), ["images/0/sampler/anisotropy"]);
}

#[test]
fn file_samplers_the_renderer_cannot_match_are_approximated_and_reported() {
    let texture = random_texture(&mut Rng::new(7), UVec2::new(2, 2), ColourSpace::Srgb);
    let image = encode_png("t", &texture, "i", &mut Report::new()).unwrap();
    let sampler = io::Sampler {
        wrap_u: io::Wrap::MirroredRepeat,
        wrap_v: io::Wrap::ClampToEdge,
        mag: io::Filter::Nearest,
        min: io::Filter::Linear,
        mipmaps: true,
    };
    let mut report = Report::new();
    let back = decode_png(&image, ColourSpace::Linear, &sampler, "images/3", &mut report).unwrap();
    assert_eq!(
        (back.desc.sampler.wrap, back.desc.sampler.filter, back.desc.colour_space),
        (render::Wrap::Repeat, render::Filter::Nearest, ColourSpace::Linear)
    );
    assert_eq!(
        locations(&report),
        ["images/3/sampler/wrap", "images/3/sampler/wrap", "images/3/sampler/filter"],
        "different axes, then mirroring, then different filters"
    );
}

#[test]
fn premultiplied_pixels_are_divided_out_and_reported() {
    let desc = TextureDesc { alpha: AlphaEncoding::Premultiplied, ..TextureDesc::colour(UVec2::new(2, 1)) };
    let texture = TextureData { desc, rgba8: vec![50, 0, 100, 100, 7, 7, 7, 0] };
    let (back, report) = png_round_trip(&texture);
    assert_eq!(back.rgba8, [128, 0, 255, 100, 0, 0, 0, 0]);
    assert_eq!(back.desc.alpha, AlphaEncoding::Straight);
    assert_eq!(rules(&report), ["texture.unpremultiplied"]);
}

#[test]
fn an_inconsistent_texture_is_refused() {
    let texture = TextureData { desc: TextureDesc::colour(UVec2::new(2, 2)), rgba8: vec![0; 15] };
    let err = encode_png("bad", &texture, "i", &mut Report::new()).unwrap_err();
    assert!(err.to_string().starts_with("texture 'bad' can't be written as PNG: 15 bytes"), "{err}");
}

#[test]
fn decoding_refuses_what_is_not_a_png_or_is_too_big() {
    let sampler = io::Sampler::default();
    let decode = |image: &EncodedImage| decode_png(image, ColourSpace::Srgb, &sampler, "i", &mut Report::new());

    let jpeg = EncodedImage::new("photo", ImageMime::Jpeg, Blob::new(vec![0xff, 0xd8, 0xff]));
    assert!(decode(&jpeg).unwrap_err().to_string().contains("only PNG is"));

    let junk = EncodedImage::new("junk", ImageMime::Png, Blob::new(vec![1, 2, 3, 4]));
    assert!(decode(&junk).unwrap_err().to_string().starts_with("image 'junk' can't be read"));

    let wide = TextureData { desc: TextureDesc::colour(UVec2::new(16_385, 1)), rgba8: vec![0; 16_385 * 4] };
    let image = encode_png("wide", &wide, "i", &mut Report::new()).unwrap();
    assert!(decode(&image).is_err(), "sides above 16384 are refused before the pixels are allocated");
}

#[test]
fn grey_palette_and_rgb_pngs_expand_to_rgba_without_findings() {
    use image::{DynamicImage, GrayImage, ImageBuffer, ImageFormat, Rgb};
    let write = |image: DynamicImage| {
        let mut bytes = Vec::new();
        image.write_to(&mut std::io::Cursor::new(&mut bytes), ImageFormat::Png).unwrap();
        EncodedImage::new("img", ImageMime::Png, Blob::new(bytes))
    };
    let sampler = io::Sampler::default();
    let mut report = Report::new();
    let grey = write(DynamicImage::ImageLuma8(GrayImage::from_raw(2, 1, vec![0, 200]).unwrap()));
    let data = decode_png(&grey, ColourSpace::Linear, &sampler, "i", &mut report).unwrap();
    assert_eq!(data.rgba8, [0, 0, 0, 255, 200, 200, 200, 255]);
    let rgb = write(DynamicImage::ImageRgb8(ImageBuffer::<Rgb<u8>, _>::from_raw(1, 1, vec![1, 2, 3]).unwrap()));
    let data = decode_png(&rgb, ColourSpace::Srgb, &sampler, "i", &mut report).unwrap();
    assert_eq!(data.rgba8, [1, 2, 3, 255]);
    assert_eq!(report, Report::new());
}
