//! The `output.window.*` ops against a fake host: what they ask of it, what they answer, and who may call them.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use omuzaki_core::colour::Colour;
use omuzaki_core::glam::UVec2;
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{
    Actor, ErrorCode, Grants, OpCtx, OpError, OpKind, Plan, Scope, Services, ServicesBuilder, Undo, find,
};
use omuzaki_stream_api::window::{
    Backdrop, OutputSurface, PaletteSource, WindowCaps, WindowError, WindowHost, WindowPlan, WindowSettings,
    WindowState,
};
use serde_json::{Value, json};

const NAMES: [&str; 4] =
    ["output.window.open", "output.window.close", "output.window.status", "output.window.suggest_key"];

struct FakeHost {
    caps: WindowCaps,
    surface: OutputSurface,
    refuse: bool,
    state: Mutex<WindowState>,
    opens: Mutex<Vec<(WindowPlan, WindowSettings)>>,
}

impl FakeHost {
    fn new(transparent: bool) -> Arc<Self> {
        Arc::new(Self {
            caps: WindowCaps::new(transparent, true),
            surface: OutputSurface::new(UVec2::new(1920, 1080), 1.5),
            refuse: false,
            state: Mutex::new(WindowState::Closed),
            opens: Mutex::default(),
        })
    }
}

impl WindowHost for FakeHost {
    fn capabilities(&self) -> WindowCaps {
        self.caps
    }
    fn surface(&self) -> OutputSurface {
        self.surface
    }
    fn open(&self, plan: &WindowPlan, settings: &WindowSettings) -> Result<(), WindowError> {
        if self.refuse {
            return Err(WindowError::Host("no display".into()));
        }
        self.opens.lock().unwrap().push((*plan, settings.clone()));
        *self.state.lock().unwrap() = WindowState::open(plan.physical, settings.backdrop);
        Ok(())
    }
    fn close(&self) {
        *self.state.lock().unwrap() = WindowState::Closed;
    }
    fn state(&self) -> WindowState {
        *self.state.lock().unwrap()
    }
}

/// Answers with the same palette for every scene and remembers which scene it was asked about.
struct FakePalette(Vec<Colour>, Mutex<Vec<Option<String>>>);

impl PaletteSource for FakePalette {
    fn colours(&self, scene: Option<&str>) -> Vec<Colour> {
        self.1.lock().unwrap().push(scene.map(str::to_owned));
        self.0.clone()
    }
}

fn services(host: &Arc<FakeHost>, palette: &Arc<FakePalette>) -> Services {
    ServicesBuilder::new().insert::<dyn WindowHost>(host.clone()).insert::<dyn PaletteSource>(palette.clone()).build()
}

fn green_palette() -> Arc<FakePalette> {
    Arc::new(FakePalette(vec!["#00b140".parse().unwrap()], Mutex::default()))
}

fn person() -> Actor {
    Actor::Person { name: "host".into() }
}

/// Looks an op up after linking this crate's registrations into the test binary (ADR 0015).
fn registered(name: &str) -> Option<&'static omuzaki_ops_api::OpRegistration> {
    omuzaki_stream::link();
    find(name)
}

fn call(op: &str, args: Value, services: Option<&Services>, dry_run: bool) -> Result<Plan, OpError> {
    let op = (registered(op).expect("the op registers itself").decode)(args)?;
    let (doc, clock, actor) = (MemDoc::new(), FakeClock::new(Seconds(0.0)), person());
    let ctx = OpCtx::new(&doc, &actor, &clock).with_dry_run(dry_run);
    match services {
        Some(services) => op.plan(&ctx.with_services(services)),
        None => op.plan(&ctx),
    }
}

fn answer(op: &str, args: Value, services: &Services) -> Value {
    call(op, args, Some(services), false).unwrap().output.expect("the op answers")
}

#[test]
fn the_ops_declare_their_kind_scope_and_undo() {
    for (name, kind, scope) in [
        (NAMES[0], OpKind::Edit, Scope::Perform),
        (NAMES[1], OpKind::Edit, Scope::Perform),
        (NAMES[2], OpKind::Query, Scope::Read),
        (NAMES[3], OpKind::Query, Scope::Read),
    ] {
        let meta = (registered(name).expect("registered").meta)();
        assert_eq!((meta.name, meta.kind, meta.scope, meta.undo), (name, kind, scope, Undo::None));
    }
}

#[test]
fn open_then_status_shows_the_planned_size() {
    let (host, palette) = (FakeHost::new(true), green_palette());
    let services = services(&host, &palette);
    assert_eq!(answer(NAMES[2], json!({}), &services), json!({ "state": "closed" }));

    let opened = answer(NAMES[0], json!({}), &services);
    assert_eq!((opened["opened"].clone(), opened["fallback"].clone()), (json!(true), json!(false)));
    assert_eq!((opened["physical"].clone(), opened["logical"].clone()), (json!([1920, 1080]), json!([1280.0, 720.0])));
    assert_eq!(opened["title"], "Omuzaki Output");
    assert_eq!(
        answer(NAMES[2], json!({}), &services),
        json!({ "state": "open", "physical": [1920, 1080], "backdrop": { "key": "#00b140" }, "minimised": false })
    );
    let (planned, settings) = host.opens.lock().unwrap()[0].clone();
    assert_eq!((planned.physical, settings.hide_cursor), (UVec2::new(1920, 1080), true));
    assert!(!settings.always_on_top, "the window is not kept on top unless asked");
    assert_eq!(opened["always_on_top"], false);
}

#[test]
fn always_on_top_is_asked_of_a_host_that_can_and_answered_false_by_one_that_cannot() {
    let (host, palette) = (FakeHost::new(true), green_palette());
    let opened = answer(NAMES[0], json!({ "always_on_top": true }), &services(&host, &palette));
    assert_eq!(opened["always_on_top"], true);
    assert!(host.opens.lock().unwrap()[0].1.always_on_top);

    let flat =
        Arc::new(FakeHost { caps: WindowCaps::new(true, false), ..Arc::try_unwrap(FakeHost::new(true)).ok().unwrap() });
    let opened = answer(NAMES[0], json!({ "always_on_top": true }), &services(&flat, &palette));
    assert_eq!(opened["always_on_top"], false, "the host said it cannot, so it is not asked");
    assert!(!flat.opens.lock().unwrap()[0].1.always_on_top);
}

#[test]
fn status_says_when_the_window_is_minimised() {
    let (host, palette) = (FakeHost::new(true), green_palette());
    let services = services(&host, &palette);
    answer(NAMES[0], json!({}), &services);
    let open = host.state();
    *host.state.lock().unwrap() = open.with_minimised(true);
    assert_eq!(answer(NAMES[2], json!({}), &services)["minimised"], true);
    assert_eq!(
        WindowState::Closed.with_minimised(true),
        WindowState::Closed,
        "a closed window has nothing to minimise"
    );
}

#[test]
fn every_declared_example_decodes() {
    for name in NAMES {
        let reg = registered(name).expect("registered");
        let examples = reg.examples();
        assert!(!examples.is_empty(), "{name} declares examples");
        for args in examples {
            assert!((reg.decode)(args.clone()).is_ok(), "{name}: {args}");
        }
    }
}

#[test]
fn a_scale_title_and_key_reach_the_host() {
    let (host, palette) = (FakeHost::new(true), green_palette());
    let services = services(&host, &palette);
    answer(NAMES[0], json!({ "scale": 0.5, "title": "Cast", "key": "#ff00ff" }), &services);
    let (planned, settings) = host.opens.lock().unwrap()[0].clone();
    assert_eq!(planned.physical, UVec2::new(960, 540));
    assert_eq!((settings.title.as_str(), settings.backdrop), ("Cast", Backdrop::Key("#ff00ff".parse().unwrap())));
}

#[test]
fn transparent_is_kept_on_a_host_that_has_it() {
    let (host, palette) = (FakeHost::new(true), green_palette());
    let opened = answer(NAMES[0], json!({ "backdrop": "transparent" }), &services(&host, &palette));
    assert_eq!((opened["fallback"].clone(), opened["backdrop"].clone()), (json!(false), json!("transparent")));
    assert_eq!(host.state(), WindowState::open(UVec2::new(1920, 1080), Backdrop::Transparent));
}

#[test]
fn transparent_on_a_host_without_it_falls_back_to_the_key_and_says_so() {
    let (host, palette) = (FakeHost::new(false), green_palette());
    let services = services(&host, &palette);
    let opened = answer(NAMES[0], json!({ "backdrop": "transparent", "key": "#0047bb" }), &services);
    assert_eq!((opened["fallback"].clone(), opened["backdrop"].clone()), (json!(true), json!({ "key": "#0047bb" })));
    let blue = Backdrop::Key("#0047bb".parse().unwrap());
    assert_eq!(host.state(), WindowState::open(UVec2::new(1920, 1080), blue));
    // Without a key of its own the fallback is the default chroma green.
    let opened = answer(NAMES[0], json!({ "backdrop": "transparent" }), &services);
    assert_eq!(opened["backdrop"], json!({ "key": "#00b140" }));
}

#[test]
fn close_then_status_shows_closed() {
    let (host, palette) = (FakeHost::new(true), green_palette());
    let services = services(&host, &palette);
    assert_eq!(answer(NAMES[1], json!({}), &services), json!({ "closed": true, "was_open": false }));
    answer(NAMES[0], json!({}), &services);
    assert_eq!(answer(NAMES[1], json!({}), &services), json!({ "closed": true, "was_open": true }));
    assert_eq!(answer(NAMES[2], json!({}), &services), json!({ "state": "closed" }));
}

#[test]
fn a_dry_run_changes_nothing() {
    let (host, palette) = (FakeHost::new(true), green_palette());
    let services = services(&host, &palette);
    let planned = call(NAMES[0], json!({}), Some(&services), true).unwrap().output.unwrap();
    assert_eq!((planned["opened"].clone(), planned["physical"].clone()), (json!(false), json!([1920, 1080])));
    assert_eq!(host.state(), WindowState::Closed);
    answer(NAMES[0], json!({}), &services);
    call(NAMES[1], json!({}), Some(&services), true).unwrap();
    assert_ne!(host.state(), WindowState::Closed, "a dry-run close must leave the window open");
}

#[test]
fn suggest_key_answers_with_the_key_and_margin() {
    let (host, palette) = (FakeHost::new(true), green_palette());
    let services = services(&host, &palette);
    let suggestion = answer(NAMES[3], json!({ "scene": "stage" }), &services);
    assert_eq!(suggestion["key"], "#ff00ff");
    assert!((suggestion["margin"].as_f64().unwrap() - 615.5).abs() < 0.1);
    assert_eq!(suggestion["collides"], false);
    assert_eq!(palette.1.lock().unwrap()[0].as_deref(), Some("stage"));

    let empty = Arc::new(FakePalette(Vec::new(), Mutex::default()));
    let suggestion = answer(NAMES[3], json!({}), &self::services(&host, &empty));
    assert_eq!(suggestion, json!({ "key": "#00b140", "margin": null, "collides": false }));
    assert_eq!(empty.1.lock().unwrap()[0], None, "no scene asks for the scene on air");
}

#[test]
fn refusals_say_which_field_to_fix() {
    let (host, palette) = (FakeHost::new(true), green_palette());
    let services = services(&host, &palette);
    for (args, field) in
        [(json!({ "scale": 0.0 }), "scale"), (json!({ "title": "" }), "title"), (json!({ "title": "a\nb" }), "title")]
    {
        let error = call(NAMES[0], args, Some(&services), false).unwrap_err();
        assert_eq!(error.code, ErrorCode::Invalid);
        assert!(format!("{error:?}").contains(field), "{error:?}");
    }
    assert!(call(NAMES[0], json!({ "scale": "big" }), Some(&services), false).is_err());
    assert!(call(NAMES[0], json!({ "colour": "#fff" }), Some(&services), false).is_err(), "unknown fields are refused");
    assert_eq!(host.opens.lock().unwrap().len(), 0);
}

#[test]
fn a_host_that_cannot_open_is_a_conflict_with_a_hint() {
    let host = Arc::new(FakeHost { refuse: true, ..Arc::try_unwrap(FakeHost::new(true)).ok().unwrap() });
    let error = call(NAMES[0], json!({}), Some(&services(&host, &green_palette())), false).unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict);
    assert!(error.hint.is_some());
}

#[test]
fn an_unusable_host_surface_is_a_conflict_not_a_bad_argument() {
    for surface in [OutputSurface::new(UVec2::new(0, 1080), 1.0), OutputSurface::new(UVec2::new(1920, 1080), 9.0)] {
        let host = Arc::new(FakeHost { surface, ..Arc::try_unwrap(FakeHost::new(true)).ok().unwrap() });
        let services = services(&host, &green_palette());
        for args in [json!({}), json!({ "scale": 0.5 })] {
            let error = call(NAMES[0], args, Some(&services), false).unwrap_err();
            assert_eq!(error.code, ErrorCode::Conflict, "{surface:?}: {error:?}");
            assert!(error.hint.is_some());
        }
        assert_eq!(host.opens.lock().unwrap().len(), 0);
    }
}

#[test]
fn without_the_services_every_op_is_unavailable() {
    for name in NAMES {
        assert_eq!(call(name, json!({}), None, false).unwrap_err().code, ErrorCode::Unavailable, "{name}");
    }
}

#[test]
fn actor_matrix_a_person_may_do_all_and_an_agent_only_the_queries() {
    let (person, agent) = (Grants::person(), Grants::agent_default());
    for name in NAMES {
        let scope = (registered(name).expect("registered").meta)().scope;
        let query = name.ends_with("status") || name.ends_with("suggest_key");
        assert!(person.allows(scope), "{name}: a person is allowed");
        assert_eq!(agent.allows(scope), query, "{name}: an agent is denied the actions and allowed the queries");
    }
    assert!(agent.with(Scope::Perform).allows(Scope::Perform), "a session grant opens the actions to an agent");
}

fn with_settings(host: &Arc<FakeHost>, settings: &Arc<omuzaki_settings_mock::MemorySettings>) -> Services {
    use omuzaki_settings_api::service::Settings;
    ServicesBuilder::new()
        .insert::<dyn WindowHost>(host.clone())
        .insert::<dyn PaletteSource>(green_palette())
        .insert::<dyn Settings>(settings.clone())
        .build()
}

#[test]
fn open_keeps_the_request_in_the_output_window_setting_and_a_dry_run_keeps_nothing() {
    use omuzaki_settings_api::service::SettingsExt as _;
    use omuzaki_stream::ops::window::OutputWindow;
    omuzaki_stream::link();
    let (host, settings) = (FakeHost::new(true), Arc::new(omuzaki_settings_mock::MemorySettings::default()));
    let kept = with_settings(&host, &settings);
    let request = json!({ "backdrop": "transparent", "key": "#ff00ff", "scale": 0.5, "always_on_top": true });

    let planned = call(NAMES[0], request.clone(), Some(&kept), true).unwrap().output.unwrap();
    assert_eq!(planned["saved"], false, "a dry run saves nothing");
    assert_eq!(settings.get::<OutputWindow>().unwrap(), omuzaki_stream::ops::window::Open::default());

    let opened = answer(NAMES[0], request.clone(), &kept);
    assert_eq!(opened["saved"], true);
    let asked: omuzaki_stream::ops::window::Open = serde_json::from_value(request).unwrap();
    assert_eq!(settings.get::<OutputWindow>().unwrap(), asked, "the next start shows the same choices");
    // Without a settings service the window still opens; the choice lasts until the app closes.
    assert_eq!(answer(NAMES[0], json!({}), &services(&host, &green_palette()))["saved"], false);
}

#[test]
fn the_output_window_setting_refuses_what_the_op_refuses() {
    use omuzaki_settings_api::setting::Setting as _;
    use omuzaki_stream::ops::window::{Open, OutputWindow};
    let open = |args: Value| serde_json::from_value::<Open>(args).unwrap();
    assert!(OutputWindow::check(&open(json!({ "title": "Cast", "scale": 2.0 }))).is_ok());
    assert!(OutputWindow::check(&open(json!({ "title": "" }))).is_err());
    assert!(OutputWindow::check(&open(json!({ "scale": 2.5 }))).is_err());
    assert_eq!(OutputWindow::META.key, "output.window");
}

/// Runs `output.window.open` with `actor` (and `confirmed_by`, when given) against a settings service.
fn open_as(
    actor: &Actor,
    confirmed_by: Option<&Actor>,
    dry_run: bool,
    settings: &Arc<omuzaki_settings_mock::MemorySettings>,
) -> Result<Plan, OpError> {
    let host = FakeHost::new(true);
    let services = with_settings(&host, settings);
    let op = (registered(NAMES[0]).unwrap().decode)(json!({ "scale": 0.5 }))?;
    let (doc, clock) = (MemDoc::new(), FakeClock::new(Seconds(0.0)));
    let mut ctx = OpCtx::new(&doc, actor, &clock).with_dry_run(dry_run).with_services(&services);
    if let Some(person) = confirmed_by {
        ctx = ctx.with_confirmation(person);
    }
    op.plan(&ctx)
}

#[test]
fn an_agent_cannot_save_the_output_window_setting_without_a_person() {
    use omuzaki_settings_api::service::SettingsExt as _;
    use omuzaki_stream::ops::window::{Open, OutputWindow};
    let settings = Arc::new(omuzaki_settings_mock::MemorySettings::default());
    let agent = Actor::Agent { id: "assistant".into(), model: None };
    for dry_run in [false, true] {
        let err = open_as(&agent, None, dry_run, &settings).unwrap_err();
        assert!(
            matches!(err.detail.as_deref(), Some(omuzaki_ops_api::ErrorDetail::NeedsPerson { .. })),
            "dry_run {dry_run}: {err:?}"
        );
    }
    assert_eq!(settings.get::<OutputWindow>().unwrap(), Open::default(), "nothing was saved");

    // A person's confirmation lets the same request through, and a dry run still saves nothing.
    let dry = open_as(&agent, Some(&person()), true, &settings).unwrap().output.unwrap();
    assert_eq!(dry["saved"], false);
    let done = open_as(&agent, Some(&person()), false, &settings).unwrap().output.unwrap();
    assert_eq!(done["saved"], true);
    assert_eq!(settings.get::<OutputWindow>().unwrap().scale, Some(0.5));
}
