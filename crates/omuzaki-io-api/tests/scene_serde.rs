//! JSON contract of the material, texture and passthrough types (design `byom.md` §3 D8, "Materials are retained
//! exactly"), and proof that the JSON of the types that already had serde did not move.

#![allow(clippy::expect_used, clippy::cast_possible_truncation)]

use omuzaki_core::glam::{Vec2, Vec3, Vec4};
use omuzaki_io_api::*;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

fn to_json<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).expect("serialises")
}

fn round_trip<T: Serialize + DeserializeOwned>(v: &T) -> T {
    serde_json::from_str(&to_json(v)).expect("deserialises")
}

/// Strings recorded at `origin/main` before the serde derives were added.
#[test]
fn existing_json_is_unchanged() {
    let mut report = Report::new();
    report.push(Finding::warning("a.b", "msg").at("nodes/3").hint("fix"));
    assert_eq!(
        to_json(&VrmMeta::default()),
        r#"{"name":"","authors":[],"licenseUrl":"","avatarPermission":"onlyAuthor","allowExcessivelyViolentUsage":false,"allowExcessivelySexualUsage":false,"commercialUsage":"personalNonProfit","allowPoliticalOrReligiousUsage":false,"allowAntisocialOrHateUsage":false,"creditNotation":"required","allowRedistribution":false,"modification":"prohibited"}"#
    );
    assert_eq!(
        to_json(&report),
        r#"{"findings":[{"severity":"warning","rule":"a.b","message":"msg","location":"nodes/3","hint":"fix"}]}"#
    );
    assert_eq!(to_json(&ExpressionName::parse("jawOpen").expect("valid name")), r#""jawOpen""#);
    assert_eq!(to_json(&NodeIx(3)), "3");
    assert_eq!(to_json(&Purpose::Publish), r#""publish""#);
    assert_eq!(to_json(&Layout::Separate), r#""separate""#);
    assert_eq!(to_json(&Severity::Warning), r#""warning""#);
}

/// A tint with every colour field set to off-grid values that have no short exact decimal form.
fn tinted(shading: Shading) -> Material {
    let mut m = Material::new("m", shading);
    m.base_color = Vec4::new(0.5, 0.123, 0.9999, 0.123);
    m.emissive = Vec3::new(0.5, 0.123, 0.9999);
    m
}

fn toon_with_colours() -> Toon {
    let c = Vec3::new(0.5, 0.123, 0.9999);
    let mut t = Toon::default();
    (t.shade_color, t.matcap_color, t.rim_color, t.outline_color) = (c, c, c, c);
    t
}

fn colour_bits(m: &Material) -> Vec<u32> {
    let mut out: Vec<f32> = m.base_color.to_array().to_vec();
    out.extend(m.emissive.to_array());
    if let Shading::Toon(t) = &m.shading {
        for c in [t.shade_color, t.matcap_color, t.rim_color, t.outline_color] {
            out.extend(c.to_array());
        }
    }
    out.into_iter().map(f32::to_bits).collect()
}

#[test]
fn colours_round_trip_bit_for_bit() {
    let m = tinted(Shading::Toon(Box::new(toon_with_colours())));
    let back = round_trip(&m);
    assert_eq!(colour_bits(&back), colour_bits(&m));
    assert_eq!(back, m);
}

fn full_toon() -> Toon {
    let tex = |i| TexRef {
        texture: TextureIx(i),
        uv_set: 1,
        transform: Some(UvTransform { offset: Vec2::new(0.25, 0.5), scale: Vec2::new(2.0, 0.1) }),
    };
    let mut t = toon_with_colours();
    t.shade_multiply_texture = Some(tex(1));
    t.shading_shift_texture = Some(ShiftTexture { texture: tex(2), scale: 0.3 });
    t.matcap_texture = Some(TexRef::of(TextureIx(3)));
    t.rim_multiply_texture = Some(tex(4));
    t.outline_mode = OutlineMode::ScreenCoordinates;
    t.outline_width = 0.004;
    t.outline_width_texture = Some(tex(5));
    t.uv_animation_mask = Some(tex(6));
    t.transparent_with_zwrite = true;
    t.render_queue_offset = -3;
    t
}

#[test]
fn every_shading_case_round_trips() {
    let pbr = Pbr { metallic: 0.1, roughness: 0.7, metallic_roughness_texture: Some(TexRef::of(TextureIx(0))) };
    let mut cases =
        vec![tinted(Shading::Pbr(pbr)), tinted(Shading::Unlit), tinted(Shading::Toon(Box::new(full_toon())))];
    for (m, alpha) in cases.iter_mut().zip([AlphaMode::Opaque, AlphaMode::Mask { cutoff: 0.33 }, AlphaMode::Blend]) {
        m.alpha = alpha;
        m.double_sided = true;
        m.base_color_texture = Some(TexRef::of(TextureIx(7)));
        m.emissive_texture = Some(TexRef::of(TextureIx(8)));
        m.normal_texture = Some(NormalTexture { texture: TexRef::of(TextureIx(9)), scale: 0.8 });
    }
    for m in &cases {
        assert_eq!(&round_trip(m), m);
    }
}

#[test]
fn passthrough_with_nested_json_round_trips() {
    let mut p = Passthrough::default();
    p.extensions.insert("EXT_b".into(), json!({"index": 3, "nested": {"list": [1, 2.5, null, "x"], "f": 0.1}}));
    p.extensions.insert("KHR_materials_sheen".into(), json!({"sheenColorFactor": [0.5, 0.123, 0.9999]}));
    p.extras = Some(json!({"k": [true, {"deep": []}]}));
    assert_eq!(round_trip(&p), p);

    // An explicit `"extras": null` is a present value and must survive; an absent one stays absent.
    p.extras = Some(Value::Null);
    assert_eq!(round_trip(&p), p);
    assert_eq!(round_trip(&Passthrough::default()), Passthrough::default());

    let mut m = Material::new("m", Shading::Unlit);
    m.passthrough = p;
    assert_eq!(round_trip(&m), m);
}

#[test]
fn encoded_image_bytes_are_padded_base64() {
    let bytes: Vec<u8> = (0..1024u32).map(|i| (i * 7 + 3) as u8).collect();
    let img = EncodedImage::new("tex", ImageMime::Png, Blob::new(bytes.clone()));
    let v: Value = serde_json::to_value(&img).expect("serialises");
    let text = v["bytes"].as_str().expect("bytes is a string");
    assert_eq!(text.len(), 1368, "1 KiB is 1368 characters, not hex's 2048");
    assert!(text.ends_with('='), "1024 bytes leave one over, padded");
    // 0x03 0x0a 0x11 is "AwoR".
    assert_eq!(&text[..4], "AwoR");
    let back = round_trip(&img);
    assert_eq!(back, img);
    assert_eq!(back.bytes.as_slice(), &bytes[..]);
}

#[test]
fn blob_base64_matches_rfc_4648_and_round_trips_every_length() {
    // RFC 4648 §10's test vectors.
    for (plain, coded) in [
        ("", ""),
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("foob", "Zm9vYg=="),
        ("fooba", "Zm9vYmE="),
        ("foobar", "Zm9vYmFy"),
    ] {
        let blob = Blob::new(plain.as_bytes().to_vec());
        assert_eq!(serde_json::to_value(&blob).expect("serialises"), Value::from(coded), "{plain}");
        assert_eq!(round_trip(&blob), blob, "{plain}");
    }
    for len in 0..70 {
        let bytes: Vec<u8> = (0..len).map(|i: u32| (i * 37 + 250) as u8).collect();
        let blob = Blob::new(bytes);
        assert_eq!(round_trip(&blob), blob, "{len} bytes");
    }
    assert_eq!(
        serde_json::from_str::<Blob>(r#""+/+/""#).expect("the last two letters").as_slice(),
        &[0xfb, 0xff, 0xbf]
    );
}

#[test]
fn blob_rejects_malformed_base64() {
    for bad in [
        r#""abc""#,  // not a multiple of 4
        r#""ab=c""#, // padding inside
        r#""a===""#, // three '='
        r#""Zg-=""#, // URL-safe alphabet
        r#""Zh==""#, // bits past the last byte
        r#""Zm 9""#, // a space
        r#""=Zm9""#, // padding first
        "12",
        "null",
    ] {
        assert!(serde_json::from_str::<Blob>(bad).is_err(), "{bad}");
    }
    assert!(round_trip(&Blob::default()).is_empty());
}

#[test]
fn textures_and_samplers_round_trip() {
    let t = Texture {
        image: ImageIx(2),
        sampler: Sampler {
            wrap_u: Wrap::ClampToEdge,
            wrap_v: Wrap::MirroredRepeat,
            mag: Filter::Nearest,
            min: Filter::Linear,
            mipmaps: false,
        },
    };
    assert_eq!(round_trip(&t), t);
    assert_eq!(
        to_json(&Sampler::default()),
        r#"{"wrap_u":"repeat","wrap_v":"repeat","mag":"linear","min":"linear","mipmaps":true}"#
    );
    assert_eq!(to_json(&ImageMime::Jpeg), r#""jpeg""#);
}

/// Pins the new format: a change to this string is a change to saved files.
#[test]
fn default_unlit_material_golden() {
    let m = Material::new("m", Shading::Unlit);
    assert_eq!(to_json(&m), GOLDEN_UNLIT);
    assert_eq!(round_trip(&m), m);
}

const GOLDEN_UNLIT: &str = r#"{"name":"m","base_color":[1.0,1.0,1.0,1.0],"base_color_texture":null,"emissive":[0.0,0.0,0.0],"emissive_texture":null,"normal_texture":null,"alpha":{"kind":"opaque"},"double_sided":false,"shading":{"kind":"unlit"},"passthrough":{"extensions":{}}}"#;
