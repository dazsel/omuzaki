//! Typed requests (#992, design D2): JSON round trips, the flat schema, unknown fields named, the accessors and every
//! `check` rule at its field path.

use std::collections::BTreeMap;

use omuzaki_connect_api::{
    Capability, ChatMessage, DccRequest, ImageRequest, LlmRequest, LoraUse, Model3dRequest, MotionRequest,
    MotionSource, MusicKind, MusicRequest, Quality, Request, Role, TextureRequest, VisionRequest, VoiceChoice,
    VoiceRequest,
};
use omuzaki_store_api::ContentHash;
use serde_json::json;

fn h(n: u8) -> ContentHash {
    ContentHash::of(&[n])
}

fn image() -> ImageRequest {
    ImageRequest { prompt: "chibi fox".into(), width: 1024, height: 1024, count: 4, ..ImageRequest::default() }
}

fn llm() -> LlmRequest {
    LlmRequest {
        system: Some("Be brief.".into()),
        messages: vec![
            ChatMessage { role: Role::User, text: "first".into() },
            ChatMessage { role: Role::Assistant, text: "reply".into() },
            ChatMessage { role: Role::User, text: "last".into() },
        ],
        max_tokens: 500,
        ..LlmRequest::default()
    }
}

/// One valid request of every capability.
fn all() -> Vec<Request> {
    vec![
        Request::Image(ImageRequest {
            negative: Some("blur".into()),
            seed: Some(3),
            references: vec![h(1), h(2)],
            model: Some("sd".into()),
            loras: vec![LoraUse { name: "ink".into(), weight: 0.8 }],
            params: BTreeMap::from([("steps".to_owned(), json!(30))]),
            ..image()
        }),
        Request::Texture(TextureRequest {
            prompt: "bricks".into(),
            width: 512,
            height: 512,
            tileable: true,
            count: 2,
            ..TextureRequest::default()
        }),
        Request::Model3d(Model3dRequest {
            images: vec![h(1), h(2), h(3)],
            quality: Quality::High,
            ..Model3dRequest::default()
        }),
        Request::Voice(VoiceRequest {
            text: "hello".into(),
            voice: VoiceChoice::clone_of(h(4), "Kit"),
            language: Some("en".into()),
            seed: None,
            params: BTreeMap::new(),
        }),
        Request::Music(MusicRequest {
            prompt: "lofi".into(),
            seconds: 30,
            kind: MusicKind::Sfx,
            looping: true,
            count: 2,
            ..MusicRequest::default()
        }),
        Request::Motion(MotionRequest {
            source: MotionSource::video(h(5)),
            seconds: Some(5),
            seed: None,
            params: BTreeMap::new(),
        }),
        Request::Llm(llm()),
        Request::Vision(VisionRequest {
            instruction: "describe".into(),
            images: vec![h(6), h(7)],
            max_tokens: 200,
            json_schema: Some(json!({"type": "object"})),
            model: None,
        }),
        Request::Dcc(DccRequest {
            tool: "blender".into(),
            action: "export".into(),
            asset: Some(h(8)),
            args: BTreeMap::new(),
        }),
    ]
}

#[test]
fn every_variant_round_trips_and_checks_clean() {
    for r in all() {
        let text = serde_json::to_string(&r).unwrap();
        assert_eq!(serde_json::from_str::<Request>(&text).unwrap(), r, "{text}");
        assert_eq!(r.check(), vec![], "{text}");
    }
}

#[test]
fn the_schema_is_one_flat_branch_per_capability() {
    let generator =
        schemars::generate::SchemaSettings::draft2020_12().with(|s| s.inline_subschemas = true).into_generator();
    let schema = serde_json::to_value(generator.into_root_schema_for::<Request>()).unwrap();
    assert_eq!(schema["oneOf"].as_array().unwrap().len(), Capability::ALL.len());
    assert!(!schema.to_string().contains("$ref"), "{schema}");
}

#[test]
fn unknown_fields_are_named() {
    let typo = json!({"capability": "image", "promt": "x", "width": 1, "height": 1, "count": 1});
    let error = serde_json::from_value::<Request>(typo).unwrap_err().to_string();
    assert!(error.contains("promt"), "{error}");
}

#[test]
fn capability_and_count() {
    let caps: Vec<Capability> = all().iter().map(Request::capability).collect();
    assert_eq!(caps, Capability::ALL);
    let counts: Vec<u32> = all().iter().map(Request::count).collect();
    assert_eq!(counts, [4, 2, 1, 1, 2, 1, 1, 1, 1]);
}

#[test]
fn input_assets_in_reading_order() {
    let assets: Vec<Vec<ContentHash>> = all().iter().map(Request::input_assets).collect();
    assert_eq!(assets[0], [h(1), h(2)]);
    assert_eq!(assets[2], [h(1), h(2), h(3)]);
    assert_eq!(assets[3], [h(4)]);
    assert_eq!(assets[5], [h(5)]);
    assert_eq!(assets[7], [h(6), h(7)]);
    assert_eq!(assets[8], [h(8)]);
    let no_asset =
        Request::Dcc(DccRequest { asset: None, tool: "b".into(), action: "a".into(), args: BTreeMap::new() });
    assert!(no_asset.input_assets().is_empty());
    let twice = Request::Image(ImageRequest { references: vec![h(1), h(1)], ..image() });
    assert_eq!(twice.input_assets().len(), 2, "duplicates are kept: the preview counts what is sent");
}

#[test]
fn seeds_prompts_and_params() {
    for (i, r) in all().into_iter().enumerate() {
        let seeded = r.clone().with_seed(9);
        match r.capability() {
            Capability::Llm | Capability::Vision | Capability::Dcc => assert_eq!(seeded, r, "{i}"),
            _ => assert_eq!(seeded.seed(), Some(9), "{i}"),
        }
    }
    let r = Request::Llm(llm());
    assert_eq!(r.prompt_text(), Some("last"));
    assert_eq!(r.text_bytes(), ("Be brief.".len() + "first".len() + "reply".len() + "last".len()) as u64);
    assert_eq!(all()[0].params().map(BTreeMap::len), Some(1));
    assert!(all()[8].params().is_some() && r.params().is_none());
}

/// The field each broken request names.
fn fields(r: &Request) -> Vec<String> {
    r.check().into_iter().map(|p| p.field).collect()
}

#[test]
fn each_check_rule_names_its_field() {
    let big = "x".repeat(16 * 1024 + 1);
    let cases: Vec<(Request, &str)> = vec![
        (Request::Image(ImageRequest { prompt: " ".into(), ..image() }), "prompt"),
        (Request::Image(ImageRequest { prompt: big.clone(), ..image() }), "prompt"),
        (Request::Image(ImageRequest { negative: Some(big.clone()), ..image() }), "negative"),
        (Request::Image(ImageRequest { count: 0, ..image() }), "count"),
        (Request::Image(ImageRequest { count: 9, ..image() }), "count"),
        (Request::Image(ImageRequest { width: 0, ..image() }), "width"),
        (Request::Image(ImageRequest { height: 4097, ..image() }), "height"),
        (Request::Image(ImageRequest { references: vec![h(0); 9], ..image() }), "references/8"),
        (
            Request::Image(ImageRequest { loras: vec![LoraUse { name: "a".into(), weight: 2.5 }], ..image() }),
            "loras/0/weight",
        ),
        (
            Request::Image(ImageRequest { loras: vec![LoraUse { name: "a".into(), weight: f32::NAN }], ..image() }),
            "loras/0/weight",
        ),
        (
            Request::Texture(TextureRequest {
                prompt: String::new(),
                width: 1,
                height: 1,
                count: 1,
                ..TextureRequest::default()
            }),
            "prompt",
        ),
        (Request::Model3d(Model3dRequest::default()), "prompt"),
        (Request::Model3d(Model3dRequest { images: vec![h(0); 5], ..Model3dRequest::default() }), "images"),
        (
            Request::Voice(VoiceRequest {
                text: "hi".into(),
                voice: VoiceChoice::clone_of(h(0), " "),
                language: None,
                seed: None,
                params: BTreeMap::new(),
            }),
            "voice/person",
        ),
        (
            Request::Music(MusicRequest { prompt: "a".into(), seconds: 0, count: 1, ..MusicRequest::default() }),
            "seconds",
        ),
        (
            Request::Music(MusicRequest { prompt: "a".into(), seconds: 301, count: 1, ..MusicRequest::default() }),
            "seconds",
        ),
        (
            Request::Motion(MotionRequest {
                source: MotionSource::prompt(""),
                seconds: None,
                seed: None,
                params: BTreeMap::new(),
            }),
            "source/text",
        ),
        (Request::Llm(LlmRequest { messages: vec![], ..llm() }), "messages"),
        (Request::Llm(LlmRequest { messages: vec![llm().messages[0].clone(); 65], ..llm() }), "messages"),
        (Request::Llm(LlmRequest { messages: llm().messages[..2].to_vec(), ..llm() }), "messages/1/role"),
        (Request::Llm(LlmRequest { max_tokens: 0, ..llm() }), "max_tokens"),
        (Request::Llm(LlmRequest { max_tokens: 64_001, ..llm() }), "max_tokens"),
        (Request::Llm(LlmRequest { temperature: Some(f32::INFINITY), ..llm() }), "temperature"),
        (
            Request::Vision(VisionRequest {
                instruction: "a".into(),
                images: vec![],
                max_tokens: 1,
                ..VisionRequest::default()
            }),
            "images",
        ),
        (
            Request::Vision(VisionRequest {
                instruction: "a".into(),
                images: vec![h(0); 9],
                max_tokens: 1,
                ..VisionRequest::default()
            }),
            "images",
        ),
        (Request::Dcc(DccRequest { tool: String::new(), action: "a".into(), ..DccRequest::default() }), "tool"),
        (Request::Dcc(DccRequest { tool: "b".into(), action: String::new(), ..DccRequest::default() }), "action"),
    ];
    for (i, (r, field)) in cases.iter().enumerate() {
        assert_eq!(fields(r), [*field], "case {i}");
    }
}

#[test]
fn four_broken_rules_give_four_problems() {
    let r = Request::Image(ImageRequest { prompt: String::new(), width: 0, height: 0, count: 0, ..image() });
    assert_eq!(fields(&r), ["prompt", "width", "height", "count"]);
}
