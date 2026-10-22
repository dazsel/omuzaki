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
    Backdrop, ClickThrough, DesktopSettings, ModeSupport, OutputSurface, PaletteSource, SizePolicy, WindowCaps,
    WindowError, WindowHost, WindowMode, WindowPlan, WindowSettings, WindowState,
};
use serde_json::{Value, json};

const NAMES: [&str; 5] = [
    "output.window.open",
    "output.window.close",
    "output.window.status",
    "output.window.suggest_key",
    "output.window.set",
];

/// What `output.window.status` lists for a host that hasn't built the desktop mode (the trait's default).
fn default_modes() -> Value {
    json!([
        { "mode": "studio", "available": true },
        { "mode": "capture", "available": true },
        { "mode": "desktop", "available": false, "reason": "This build can't show the character on the desktop yet" },
    ])
}

struct FakeHost {
    caps: WindowCaps,
    surface: OutputSurface,
    refuse: bool,
    state: Mutex<WindowState>,
    opens: Mutex<Vec<(WindowPlan, WindowSettings)>>,
    /// Whether it shows the desktop mode; `None` keeps the trait's default list.
    desktop: Option<bool>,
    sets: Mutex<Vec<DesktopSettings>>,
}

impl FakeHost {
    fn new(transparent: bool) -> Arc<Self> {
        Arc::new(Self {
            caps: WindowCaps::new(transparent, true),
            surface: OutputSurface::new(UVec2::new(1920, 1080), 1.5),
            refuse: false,
            state: Mutex::new(WindowState::Closed),
            opens: Mutex::default(),
            desktop: None,
            sets: Mutex::default(),
        })
    }

    /// A transparent host that shows the desktop mode when `desktop`, and says why not otherwise.
    fn with_desktop(desktop: bool) -> Arc<Self> {
        let mut host = Arc::into_inner(Self::new(true)).unwrap();
        host.desktop = Some(desktop);
        Arc::new(host)
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
        *self.state.lock().unwrap() = WindowState::open(plan.physical, settings.backdrop).with_mode(settings);
        Ok(())
    }
    fn close(&self) {
        *self.state.lock().unwrap() = WindowState::Closed;
    }
    fn state(&self) -> WindowState {
        *self.state.lock().unwrap()
    }
    fn modes(&self) -> Vec<ModeSupport> {
        let Some(desktop) = self.desktop else {
            return vec![
                ModeSupport::available(WindowMode::Studio),
                ModeSupport::available(WindowMode::Capture),
                ModeSupport::unavailable(WindowMode::Desktop, "This build can't show the character on the desktop yet"),
            ];
        };
        let desktop = if desktop {
            ModeSupport::available(WindowMode::Desktop)
        } else {
            ModeSupport::unavailable(WindowMode::Desktop, "Click-through needs a compositor on this Linux session")
        };
        vec![ModeSupport::available(WindowMode::Studio), ModeSupport::available(WindowMode::Capture), desktop]
    }
    fn set_desktop(&self, desktop: &DesktopSettings) -> Result<(), WindowError> {
        self.sets.lock().unwrap().push(*desktop);
        let mut state = self.state.lock().unwrap();
        *state = state.with_desktop(*desktop);
        Ok(())
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
    assert_eq!(answer(NAMES[2], json!({}), &services), json!({ "state": "closed", "modes": default_modes() }));

    let opened = answer(NAMES[0], json!({}), &services);
    assert_eq!((opened["opened"].clone(), opened["fallback"].clone()), (json!(true), json!(false)));
    assert_eq!((opened["physical"].clone(), opened["logical"].clone()), (json!([1920, 1080]), json!([1280.0, 720.0])));
    assert_eq!(opened["title"], "Omuzaki Output");
    assert_eq!(
        answer(NAMES[2], json!({}), &services),
        json!({
            "state": "open",
            "physical": [1920, 1080],
            "backdrop": { "key": "#00b140" },
            "minimised": false,
            "mode": "capture",
            "modes": default_modes(),
        })
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
    assert_eq!(answer(NAMES[2], json!({}), &services), json!({ "state": "closed", "modes": default_modes() }));
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
fn an_agent_opens_the_window_without_it_becoming_the_persons_choice() {
    use omuzaki_settings_api::service::SettingsExt as _;
    use omuzaki_stream::ops::window::{Open, OutputWindow};
    let settings = Arc::new(omuzaki_settings_mock::MemorySettings::default());
    let agent = Actor::Agent { id: "assistant".into(), model: None };
    for dry_run in [false, true] {
        let done = open_as(&agent, None, dry_run, &settings).unwrap().output.unwrap();
        assert_eq!((done["opened"].clone(), done["saved"].clone()), (json!(!dry_run), json!(false)), "{dry_run}");
    }
    assert_eq!(settings.get::<OutputWindow>().unwrap(), Open::default(), "nothing was saved");

    // A person's confirmation makes it the person's choice, and a dry run still saves nothing.
    let dry = open_as(&agent, Some(&person()), true, &settings).unwrap().output.unwrap();
    assert_eq!(dry["saved"], false);
    let done = open_as(&agent, Some(&person()), false, &settings).unwrap().output.unwrap();
    assert_eq!(done["saved"], true);
    assert_eq!(settings.get::<OutputWindow>().unwrap().scale, Some(0.5));
}

fn agent() -> Actor {
    Actor::Agent { id: "assistant".into(), model: None }
}

/// Runs `op` with `args` as `actor` over `host`.
fn run_as(actor: &Actor, op: &str, args: Value, host: &Arc<FakeHost>, dry_run: bool) -> Result<Value, OpError> {
    let services = services(host, &green_palette());
    let op = (registered(op).unwrap().decode)(args)?;
    let (doc, clock) = (MemDoc::new(), FakeClock::new(Seconds(0.0)));
    let ctx = OpCtx::new(&doc, actor, &clock).with_dry_run(dry_run).with_services(&services);
    op.plan(&ctx).map(|p| p.output.unwrap_or(Value::Null))
}

#[test]
fn the_desktop_mode_opens_a_transparent_window_on_top_with_the_pointer_shown() {
    let host = FakeHost::with_desktop(true);
    let opened = run_as(&person(), NAMES[0], json!({ "mode": "desktop", "opacity": 0.75 }), &host, false).unwrap();
    assert_eq!(opened["mode"], "desktop");
    assert_eq!(opened["desktop"], json!({ "opacity": 0.75, "click_through": "transparent" }));
    assert_eq!(opened["physical"], json!([960, 540]), "half the output unless a scale is asked for");
    let (_, settings) = host.opens.lock().unwrap()[0].clone();
    assert_eq!(settings.mode, WindowMode::Desktop);
    assert_eq!(settings.backdrop, Backdrop::Transparent);
    assert!(settings.always_on_top && !settings.hide_cursor);
    assert_eq!(settings.size, SizePolicy::Scale(0.5));
    assert_eq!(settings.desktop, DesktopSettings::new(0.75, ClickThrough::Transparent));

    let status = run_as(&person(), NAMES[2], json!({}), &host, false).unwrap();
    assert_eq!((status["mode"].clone(), status["desktop"]["opacity"].clone()), (json!("desktop"), json!(0.75)));
    assert_eq!(status["modes"][2], json!({ "mode": "desktop", "available": true }));

    // Asking for a key colour or a capture tool's stacking changes nothing about a desktop window.
    let args = json!({ "mode": "desktop", "backdrop": "key", "always_on_top": false, "scale": 0.3 });
    run_as(&person(), NAMES[0], args, &host, false).unwrap();
    let (_, settings) = host.opens.lock().unwrap()[1].clone();
    assert_eq!((settings.backdrop, settings.always_on_top), (Backdrop::Transparent, true));
    assert_eq!(settings.size, SizePolicy::Scale(0.3));
}

#[test]
fn a_mode_this_machine_cant_show_is_refused_with_its_reason() {
    for host in [FakeHost::with_desktop(false), FakeHost::new(true)] {
        let error = run_as(&person(), NAMES[0], json!({ "mode": "desktop" }), &host, false).unwrap_err();
        assert_eq!(error.code, ErrorCode::Unsupported);
        let reason = run_as(&person(), NAMES[2], json!({}), &host, false).unwrap()["modes"][2]["reason"].clone();
        assert_eq!(json!(error.message), reason, "the refusal is the reason status lists");
        assert!(host.opens.lock().unwrap().is_empty());
    }
    // A host that lists the mode but can't make a transparent surface on top is refused too, never a key-coloured box.
    let mut flat = Arc::into_inner(FakeHost::with_desktop(true)).unwrap();
    flat.caps = WindowCaps::new(false, true);
    let flat = Arc::new(flat);
    assert_eq!(
        run_as(&person(), NAMES[0], json!({ "mode": "desktop" }), &flat, false).unwrap_err().code,
        ErrorCode::Unsupported
    );
}

#[test]
fn the_studio_mode_closes_the_window() {
    let host = FakeHost::with_desktop(true);
    run_as(&person(), NAMES[0], json!({}), &host, false).unwrap();
    let dry = run_as(&person(), NAMES[0], json!({ "mode": "studio" }), &host, true).unwrap();
    assert_eq!((dry["closed"].clone(), dry["was_open"].clone()), (json!(false), json!(true)));
    assert!(matches!(host.state(), WindowState::Open { .. }), "a dry run closes nothing");
    let done = run_as(&person(), NAMES[0], json!({ "mode": "studio" }), &host, false).unwrap();
    assert_eq!((done["mode"].clone(), done["closed"].clone()), (json!("studio"), json!(true)));
    assert_eq!(host.state(), WindowState::Closed);
}

#[test]
fn desktop_choices_are_refused_outside_the_desktop_mode_and_out_of_range() {
    let host = FakeHost::with_desktop(true);
    for args in [json!({ "opacity": 0.5 }), json!({ "mode": "capture", "click_through": "off" })] {
        let error = run_as(&person(), NAMES[0], args.clone(), &host, false).unwrap_err();
        assert_eq!(error.code, ErrorCode::Invalid, "{args}");
        assert!(error.hint.unwrap_or_default().contains("desktop"), "{args}");
    }
    for opacity in [0.05, 1.2] {
        let error = run_as(&person(), NAMES[0], json!({ "mode": "desktop", "opacity": opacity }), &host, false);
        let error = error.unwrap_err();
        assert_eq!(error.code, ErrorCode::Invalid, "{opacity}");
        assert!(error.message.contains("0.1 to 1"), "{}", error.message);
    }
    assert!(host.opens.lock().unwrap().is_empty());
}

#[test]
fn set_changes_an_open_desktop_window_live() {
    let host = FakeHost::with_desktop(true);
    let error = run_as(&person(), NAMES[4], json!({ "opacity": 0.5 }), &host, false).unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict, "no window: nothing to change");
    run_as(&person(), NAMES[0], json!({}), &host, false).unwrap();
    let error = run_as(&person(), NAMES[4], json!({ "opacity": 0.5 }), &host, false).unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict, "a capture window has no opacity");

    run_as(&person(), NAMES[0], json!({ "mode": "desktop" }), &host, false).unwrap();
    let dry = run_as(&agent(), NAMES[4], json!({ "opacity": 0.5 }), &host, true).unwrap();
    assert_eq!(dry["applied"], false);
    assert!(host.sets.lock().unwrap().is_empty(), "a dry run changes nothing");
    let done = run_as(&agent(), NAMES[4], json!({ "opacity": 0.5 }), &host, false).unwrap();
    assert_eq!(done["desktop"], json!({ "opacity": 0.5, "click_through": "transparent" }), "the rest is kept");
    run_as(&agent(), NAMES[4], json!({ "click_through": "off" }), &host, false).unwrap();
    assert_eq!(
        *host.sets.lock().unwrap(),
        [DesktopSettings::new(0.5, ClickThrough::Transparent), DesktopSettings::new(0.5, ClickThrough::Off),]
    );
    let status = run_as(&agent(), NAMES[2], json!({}), &host, false).unwrap();
    assert_eq!(status["desktop"], json!({ "opacity": 0.5, "click_through": "off" }));
    let error = run_as(&person(), NAMES[4], json!({ "opacity": 0.0 }), &host, false).unwrap_err();
    assert_eq!(error.code, ErrorCode::Invalid);
}

#[test]
fn only_a_person_lets_every_click_through() {
    let host = FakeHost::with_desktop(true);
    let needs_person =
        |e: &OpError| matches!(e.detail.as_deref(), Some(omuzaki_ops_api::ErrorDetail::NeedsPerson { .. }));
    for dry_run in [true, false] {
        let args = json!({ "mode": "desktop", "click_through": "full" });
        let error = run_as(&agent(), NAMES[0], args, &host, dry_run).unwrap_err();
        assert!(needs_person(&error), "{error:?}");
    }
    assert!(host.opens.lock().unwrap().is_empty());
    run_as(&agent(), NAMES[0], json!({ "mode": "desktop" }), &host, false).unwrap();
    let error = run_as(&agent(), NAMES[4], json!({ "click_through": "full" }), &host, false).unwrap_err();
    assert!(needs_person(&error), "{error:?}");
    assert!(host.sets.lock().unwrap().is_empty());
    let done = run_as(&person(), NAMES[4], json!({ "click_through": "full" }), &host, false).unwrap();
    assert_eq!(done["desktop"]["click_through"], "full");
}

#[test]
fn the_setting_keeps_the_mode_and_the_desktop_choices() {
    use omuzaki_settings_api::setting::Setting as _;
    use omuzaki_stream::ops::window::{Open, OutputWindow};
    let open = |args: Value| serde_json::from_value::<Open>(args).unwrap();
    let kept = open(json!({ "mode": "desktop", "opacity": 0.75, "click_through": "off" }));
    assert!(OutputWindow::check(&kept).is_ok());
    assert_eq!(serde_json::to_value(&kept).unwrap()["mode"], "desktop");
    assert!(OutputWindow::check(&open(json!({ "mode": "desktop", "opacity": 2.0 }))).is_err());
}
