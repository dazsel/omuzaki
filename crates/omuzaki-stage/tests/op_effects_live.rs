//! The live effect ops (particles.md D10 to D12; #812): `stage.effect.burst`, `.weather`, `.clear` and `.status` reach
//! the effect service, call nothing on a dry run, never touch the document or undo, and map the service's refusals
//! onto op errors. A `FakeEffects` stands in for the stage.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, ErrorCode, Op, OpCtx, OpError, OpKind, Plan, Scope, ServicesBuilder, Undo};
use omuzaki_stage::ops::effects::burst::EffectBurst;
use omuzaki_stage::ops::effects::clear::EffectClear;
use omuzaki_stage::ops::effects::status::EffectStatusOp;
use omuzaki_stage::ops::effects::weather::EffectWeather;
use omuzaki_stage_api::effects::{BurstAccepted, EffectStatus, WeatherSource};
use omuzaki_stage_api::{BurstRequest, EffectError, EffectEvent, EffectService, LiveWeather, PresetInfo};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

/// What the fake was asked.
#[derive(Debug, Clone, PartialEq)]
enum Call {
    Burst(BurstRequest),
    Weather(LiveWeather),
    Clear(f32),
}

/// An effect service that records its calls and answers with `refuse` when set.
#[derive(Default)]
struct FakeEffects {
    calls: Mutex<Vec<Call>>,
    refuse: Mutex<Option<EffectError>>,
}

impl FakeEffects {
    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    fn answer(&self, call: Call) -> Result<(), EffectError> {
        if let Some(error) = self.refuse.lock().unwrap().clone() {
            return Err(error);
        }
        self.calls.lock().unwrap().push(call);
        Ok(())
    }
}

impl EffectService for FakeEffects {
    fn presets(&self) -> &[PresetInfo] {
        omuzaki_stage::effects::presets::infos()
    }

    fn burst(&self, request: &BurstRequest) -> Result<BurstAccepted, EffectError> {
        self.answer(Call::Burst(request.clone()))?;
        Ok(BurstAccepted { preset: request.preset.clone(), count: request.count.unwrap_or(150), seed: 9 })
    }

    fn live_weather(&self, request: &LiveWeather) -> Result<(), EffectError> {
        self.answer(Call::Weather(request.clone()))
    }

    fn clear(&self, fade_s: f32) -> Result<(), EffectError> {
        self.answer(Call::Clear(fade_s))
    }

    fn status(&self) -> EffectStatus {
        EffectStatus {
            slots: 42,
            cap: 5_000,
            weather: WeatherSource::Scene,
            density: 1.0,
            drawn: true,
            ..EffectStatus::default()
        }
    }

    fn replay(&self, _: &EffectEvent) -> Result<(), EffectError> {
        Ok(())
    }
}

fn run<T: Op + DeserializeOwned>(fake: &Arc<FakeEffects>, args: Value, dry_run: bool) -> Result<Plan, OpError> {
    let doc = MemDoc::from_value(json!({}));
    let actor = Actor::Person { name: "test".to_owned() };
    let clock = FakeClock::new(Seconds(0.0));
    let service: Arc<dyn EffectService> = Arc::clone(fake) as Arc<dyn EffectService>;
    let services = ServicesBuilder::new().insert::<dyn EffectService>(service).build();
    let op: T = serde_json::from_value(args).expect("the arguments read");
    op.plan(&OpCtx::new(&doc, &actor, &clock).with_services(&services).with_dry_run(dry_run))
}

#[allow(clippy::needless_pass_by_value, reason = "every call site passes a json! literal")]
fn applied<T: Op + DeserializeOwned>(fake: &Arc<FakeEffects>, args: Value) -> Value {
    let plan = run::<T>(fake, args.clone(), false).unwrap_or_else(|e| panic!("{} {args}: {e:?}", T::meta().name));
    assert!(plan.patch.is_empty(), "{}: a live action leaves the document alone", T::meta().name);
    plan.output.unwrap_or(Value::Null)
}

#[test]
fn the_live_actions_are_perform_scoped_with_nothing_to_undo_and_status_is_a_query() {
    for meta in [EffectBurst::meta(), EffectWeather::meta(), EffectClear::meta()] {
        assert_eq!((meta.kind, meta.scope, meta.undo), (OpKind::Edit, Scope::Perform, Undo::None), "{}", meta.name);
    }
    let status = EffectStatusOp::meta();
    assert_eq!((status.kind, status.scope), (OpKind::Query, Scope::Read));
}

#[test]
fn burst_reaches_the_service_and_returns_what_it_accepted() {
    let fake = Arc::new(FakeEffects::default());
    let out = applied::<EffectBurst>(
        &fake,
        json!({ "preset": "confetti", "at": { "from": "subject", "point": "chest" }, "count": 200 }),
    );
    assert_eq!(out, json!({ "preset": "confetti", "count": 200, "seed": 9 }));
    let calls = fake.calls();
    let [Call::Burst(request)] = calls.as_slice() else { panic!("one burst: {calls:?}") };
    assert_eq!(request.count, Some(200));
    assert_eq!(request.at.as_ref().unwrap().point.as_deref(), Some("chest"));
}

#[test]
fn a_chat_messages_emote_spans_decode_as_emotes() {
    let fake = Arc::new(FakeEffects::default());
    // `chat.message`'s `EmoteSpan`s carry offsets too; a burst ignores them.
    let spans = json!([
        { "start": 0, "end": 4, "id": "25", "name": "Kappa", "image": "https://static-cdn.jtvnw.net/emoticons/v2/25/static/light/2.0" },
        { "start": 6, "end": 11, "id": "88", "name": "PogChamp" }
    ]);
    applied::<EffectBurst>(&fake, json!({ "preset": "emote-rain", "emotes": spans, "only_with_emotes": true }));
    let calls = fake.calls();
    let [Call::Burst(request)] = calls.as_slice() else { panic!("one burst") };
    assert_eq!(request.emotes.len(), 2);
    assert_eq!(request.emotes[1].name, "PogChamp");
    assert!(request.only_with_emotes);
}

#[test]
fn a_dry_run_checks_everything_and_calls_nothing() {
    let fake = Arc::new(FakeEffects::default());
    let plan = run::<EffectBurst>(&fake, json!({ "preset": "confetti" }), true).unwrap();
    assert_eq!(plan.output.unwrap()["applied"], false);
    run::<EffectWeather>(&fake, json!({ "preset": "rain", "for_s": 30 }), true).unwrap();
    run::<EffectClear>(&fake, json!({}), true).unwrap();
    assert!(fake.calls().is_empty(), "a dry run queues nothing: {:?}", fake.calls());
    let error = run::<EffectBurst>(&fake, json!({ "preset": "confeti" }), true).unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound, "a dry run still refuses a typo");
    let error = run::<EffectClear>(&fake, json!({ "fade_s": 9 }), true).unwrap_err();
    assert_eq!(error.field.as_deref(), Some("fade_s"));
}

#[test]
fn weather_takes_a_preset_none_or_scene_and_clear_defaults_its_fade() {
    let fake = Arc::new(FakeEffects::default());
    applied::<EffectWeather>(&fake, json!({ "preset": "snow", "intensity": 0.5, "for_s": 60 }));
    applied::<EffectWeather>(&fake, json!({ "preset": "none" }));
    applied::<EffectWeather>(&fake, json!({ "preset": "scene" }));
    assert_eq!(applied::<EffectClear>(&fake, json!({})), json!({ "fade_s": 0.3_f32 }));
    let calls = fake.calls();
    assert_eq!(
        calls[0],
        Call::Weather(LiveWeather::preset(omuzaki_core::id::Slug::new("snow").unwrap(), 0.5, Some(60.0)))
    );
    assert_eq!(calls[1], Call::Weather(LiveWeather::None));
    assert_eq!(calls[2], Call::Weather(LiveWeather::Scene));
    assert_eq!(calls[3], Call::Clear(0.3));
    let error = run::<EffectWeather>(&fake, json!({ "preset": "confetti" }), false).unwrap_err();
    assert_eq!(error.code, ErrorCode::Invalid, "a burst preset is no weather");
    assert!(error.message.contains("stage.effect.burst"), "{error:?}");
}

#[test]
fn the_services_refusals_map_onto_op_errors() {
    let fake = Arc::new(FakeEffects::default());
    let cases = [
        (EffectError::unknown_preset("sparkle", vec!["hit-stars".into()]), ErrorCode::NotFound),
        (EffectError::Limited, ErrorCode::Limited),
        (EffectError::Invalid("count must be in 1..=1000".into()), ErrorCode::Invalid),
        (EffectError::Unsupported, ErrorCode::Unsupported),
    ];
    for (refusal, code) in cases {
        *fake.refuse.lock().unwrap() = Some(refusal.clone());
        let error = run::<EffectBurst>(&fake, json!({ "preset": "confetti" }), false).unwrap_err();
        assert_eq!(error.code, code, "{refusal:?}");
        if code == ErrorCode::NotFound {
            assert!(error.hint.as_deref().is_some_and(|h| h.contains("'hit-stars'")), "{error:?}");
        }
    }
    *fake.refuse.lock().unwrap() = None;
    let error = run::<EffectBurst>(&fake, json!({ "preset": "snow" }), false).unwrap_err();
    assert_eq!(error.code, ErrorCode::Invalid, "a weather preset doesn't burst");
    let error = run::<EffectBurst>(&fake, json!({ "preset": "confetti", "scale": 9 }), false).unwrap_err();
    assert_eq!(error.code, ErrorCode::Invalid, "nothing out of range is clamped");
    let error = run::<EffectBurst>(
        &fake,
        json!({ "preset": "confetti", "at": { "from": "subject", "point": "rightHnad" } }),
        false,
    )
    .unwrap_err();
    assert!(error.message.contains("rightHand"), "a bone typo names the bone: {error:?}");
}

#[test]
fn status_reports_the_service_and_every_op_needs_the_service() {
    let fake = Arc::new(FakeEffects::default());
    let out = applied::<EffectStatusOp>(&fake, json!({}));
    assert_eq!(out["slots"], 42);
    assert_eq!(out["weather"], "scene");
    let doc = MemDoc::from_value(json!({}));
    let actor = Actor::Person { name: "test".to_owned() };
    let clock = FakeClock::new(Seconds(0.0));
    let op: EffectBurst = serde_json::from_value(json!({ "preset": "confetti" })).unwrap();
    let error = op.plan(&OpCtx::new(&doc, &actor, &clock)).unwrap_err();
    assert_eq!(error.code, ErrorCode::Unavailable);
}
