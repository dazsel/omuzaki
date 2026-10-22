//! The studio's look as settings (#949): `ui.theme`, `ui.density`, `ui.scale` and `ui.reduce_motion` are registered
//! well formed, every key the app links is well formed beside them, and a `settings.set` through the op layer, by a
//! person or an agent, changes the look on the next frame; a frame with no change applies nothing.

use std::sync::Arc;

use egui::{Context, RawInput};
use omuzaki_ops_api::{Actor, Budget, Grants, Limits, Mode, Request, Runner, SessionSpec, Transport};
use omuzaki_settings_api::service::Settings as SettingsService;
use omuzaki_ui::ds::settings_keys::SettingsLink;
use omuzaki_ui::ds::{Density, Scale, Settings, ThemeKind, ds};

omuzaki_settings_api::check_crate!(omuzaki_ui);
omuzaki_settings_api::check_app!(omuzaki_ui);

#[allow(clippy::needless_pass_by_value)] // Called with `json!` literals.
fn set(runner: &dyn Runner, actor: &Actor, key: &str, value: serde_json::Value) {
    let grants = if matches!(actor, Actor::Person { .. }) { Grants::person() } else { Grants::agent_default() };
    let request = Request {
        op: "settings.set".to_owned(),
        args: serde_json::json!({ "key": key, "value": value }),
        mode: Mode::Apply,
        actor: actor.clone(),
        grants,
        cause: None,
        tx: None,
        base: None,
    };
    runner.run(request).unwrap_or_else(|e| panic!("settings.set {key}: {e}"));
}

/// One frame of `ctx` with the link synced at its start, as the app does, and the frame after it, which is the first
/// drawn at a new zoom (egui takes a zoom set during a frame from the next one).
fn frame(ctx: &Context, link: &mut SettingsLink) {
    for _ in 0..2 {
        ctx.run_ui(RawInput::default(), |ui| link.sync(ui.ctx())).drop_without_applying_deltas();
    }
}

#[test]
fn a_set_through_the_op_layer_changes_the_look_on_the_next_frame() {
    omuzaki_ui::link();
    let folder = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let paths = omuzaki_settings::Paths::new(folder.path().join("config"), folder.path().join("data"));
    let store: Arc<dyn SettingsService> = Arc::new(omuzaki_settings::Store::open(&paths));
    let services = omuzaki_ops_api::ServicesBuilder::new().insert::<dyn SettingsService>(Arc::clone(&store)).build();
    let core =
        omuzaki_ui::wiring::core_on(omuzaki_ui::wiring::Document::empty(), services).unwrap_or_else(|e| panic!("{e}"));
    let runner = Arc::clone(&core.runner);
    let ctx = Context::default();
    let mut link = SettingsLink::new(Arc::clone(&store));
    frame(&ctx, &mut link);
    assert_eq!(link.applied(), Some(Settings::default()), "the defaults are the design system's");
    assert!((ctx.zoom_factor() - 1.0).abs() < 1e-6);

    // An agent connected as an MCP client is: a session the executor opens for it, working for the person.
    let spec = SessionSpec {
        transport: Transport::Stdio,
        client: "designer".into(),
        model: None,
        on_behalf_of: Actor::Person { name: "kit".into() },
        grants: Grants::agent_default(),
        budget: Budget::default(),
        limits: Limits::default(),
        parent: None,
        role: None,
        fence: Vec::new(),
    };
    let agent = core.sessions.open(spec).unwrap_or_else(|e| panic!("{e}")).actor;
    set(runner.as_ref(), &agent, "ui.scale", serde_json::json!(150));
    frame(&ctx, &mut link);
    assert!(
        (ctx.zoom_factor() - 1.5).abs() < 1e-6,
        "an agent's scale applies on the next frame: {}",
        ctx.zoom_factor()
    );

    let person = Actor::Person { name: "kit".into() };
    set(runner.as_ref(), &person, "ui.theme", serde_json::json!("light"));
    set(runner.as_ref(), &person, "ui.density", serde_json::json!("compact"));
    frame(&ctx, &mut link);
    assert_eq!(ds(&ctx).settings.theme, ThemeKind::Light);
    assert_eq!(ds(&ctx).settings.density, Density::Compact);
    assert_eq!(ds(&ctx).settings.scale, Scale::nearest(150));

    // A change to another area's key reaches the link but changes nothing here.
    let before = link.applied();
    set(runner.as_ref(), &person, "obs.port", serde_json::json!(4456));
    frame(&ctx, &mut link);
    assert_eq!(link.applied(), before);

    // The store keeps them: a second store over the same folders, as after a restart, reads the same look.
    drop(runner);
    drop(core);
    drop(link);
    drop(store);
    let reopened = omuzaki_settings::Store::open(&paths);
    let look = Settings::from_snapshot(&reopened.snapshot());
    assert_eq!((look.theme, look.density, look.scale.percent()), (ThemeKind::Light, Density::Compact, 150));
}

#[test]
fn a_scale_outside_the_range_is_refused_and_the_look_stays() {
    omuzaki_ui::link();
    let store: Arc<dyn SettingsService> = Arc::new(omuzaki_settings_mock::MemorySettings::with_clock(Arc::new(
        omuzaki_core::time::FakeClock::new(omuzaki_core::time::Seconds(0.0)),
    )));
    let services = omuzaki_ops_api::ServicesBuilder::new().insert::<dyn SettingsService>(Arc::clone(&store)).build();
    let runner = omuzaki_ui::wiring::runner(services).unwrap_or_else(|e| panic!("{e}"));
    let request = Request {
        op: "settings.set".to_owned(),
        args: serde_json::json!({ "key": "ui.scale", "value": 400 }),
        mode: Mode::Apply,
        actor: Actor::Person { name: "kit".into() },
        grants: Grants::person(),
        cause: None,
        tx: None,
        base: None,
    };
    let refused = runner.run(request).err().unwrap_or_else(|| panic!("400% is out of range"));
    assert_eq!(refused.field.as_deref(), Some("value"), "{refused:?}");
    let ctx = Context::default();
    let mut link = SettingsLink::new(store);
    frame(&ctx, &mut link);
    assert_eq!(link.applied().map(|s| s.scale), Some(Scale::NORMAL));
}

#[test]
fn a_language_set_through_the_op_layer_swaps_the_words_on_the_next_frame() {
    use omuzaki_l10n_api::Locale;
    omuzaki_ui::link();
    let store: Arc<dyn SettingsService> = Arc::new(omuzaki_settings_mock::MemorySettings::with_clock(Arc::new(
        omuzaki_core::time::FakeClock::new(omuzaki_core::time::Seconds(0.0)),
    )));
    let services = omuzaki_ops_api::ServicesBuilder::new().insert::<dyn SettingsService>(Arc::clone(&store)).build();
    let runner = omuzaki_ui::wiring::runner(services).unwrap_or_else(|e| panic!("{e}"));
    let ctx = Context::default();
    let mut link = SettingsLink::new(Arc::clone(&store));
    frame(&ctx, &mut link);
    assert!(link.speaks().is_some(), "the computer's language is applied on the first frame");

    let person = Actor::Person { name: "kit".into() };
    set(runner.as_ref(), &person, "ui.language", serde_json::json!("ja"));
    frame(&ctx, &mut link);
    assert_eq!(link.speaks(), Some(&Locale::JA));
    assert_eq!(ds(&ctx).strings.locale(), "ja");
    let look = link.applied();
    set(runner.as_ref(), &person, "ui.language", serde_json::json!("en"));
    frame(&ctx, &mut link);
    assert_eq!(ds(&ctx).strings.locale(), "en");
    assert_eq!(link.applied(), look, "a new language leaves the look as it was");

    let request = Request {
        op: "settings.set".to_owned(),
        args: serde_json::json!({ "key": "ui.language", "value": "zh-Hant" }),
        mode: Mode::Apply,
        actor: person,
        grants: Grants::person(),
        cause: None,
        tx: None,
        base: None,
    };
    let refused = runner.run(request).err().unwrap_or_else(|| panic!("zh-Hant isn't shipped yet"));
    assert_eq!(refused.field.as_deref(), Some("value"), "{refused:?}");
}

#[test]
fn a_translated_word_is_said_in_its_language_and_an_untranslated_one_in_english() {
    use omuzaki_l10n_api::{Catalog, Locale, Resource};
    use omuzaki_ui::ds::strings::CatalogStrings;
    use omuzaki_ui::shell::text::{Word, say, title};
    omuzaki_ui::link();
    // A fixture translation: two of the shell's words, one panel title and one component word.
    let english =
        Resource { package: "omuzaki-ui", locale: "en", source: include_str!("../locales/en/omuzaki-ui.ftl") };
    let japanese = Resource {
        package: "omuzaki-ui",
        locale: "ja",
        source: "ui-shell-menu-settings = 設定\nui-shell-status-can-undo = Ctrl+Z で { $label } を元に戻す\n\
                 ui-panels-outliner = アウトライナー\nds-status-ok = 正常\n",
    };
    let catalog = Catalog::from_sources(Locale::JA, &[], &[english, japanese]);
    let ctx = Context::default();
    omuzaki_ui::ds::set_strings(&ctx, Arc::new(CatalogStrings::new(catalog)));
    assert_eq!(say(&ctx, Word::MenuSettings, &[]), "設定");
    assert_eq!(say(&ctx, Word::StatusCanUndo, &[("label", "Hair colour")]), "Ctrl+Z で Hair colour を元に戻す");
    assert_eq!(title(&ctx, omuzaki_ui::panels::text::OUTLINER), "アウトライナー");
    assert_eq!(ds(&ctx).status_word(omuzaki_ui::ds::tokens::Status::Ok), "正常");
    assert_eq!(say(&ctx, Word::MenuHelp, &[]), "Help", "an untranslated word falls back to English");
}

/// A font source that hands out a bundled Latin face in place of the pinned file and counts what it is asked for.
#[derive(Default)]
struct FakeFonts(std::sync::Mutex<Vec<String>>);

impl omuzaki_stage::sheet::layout::FontSource for FakeFonts {
    fn load(
        &self,
        spec: &omuzaki_stage::sheet::layout::FontSpec,
    ) -> Result<Vec<u8>, omuzaki_stage::sheet::layout::FontError> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(spec.file_name.to_owned());
        Ok(include_bytes!("../src/ds/fonts/JetBrainsMono.ttf").to_vec())
    }
}

#[test]
fn a_cjk_language_fetches_its_face_once_and_english_fetches_none() {
    use omuzaki_stage::sheet::layout::Family;
    use omuzaki_ui::ds::script_fonts::ScriptFonts;
    omuzaki_ui::link();
    let store: Arc<dyn SettingsService> = Arc::new(omuzaki_settings_mock::MemorySettings::with_clock(Arc::new(
        omuzaki_core::time::FakeClock::new(omuzaki_core::time::Seconds(0.0)),
    )));
    let services = omuzaki_ops_api::ServicesBuilder::new().insert::<dyn SettingsService>(Arc::clone(&store)).build();
    let runner = omuzaki_ui::wiring::runner(services).unwrap_or_else(|e| panic!("{e}"));
    let source = Arc::new(FakeFonts::default());
    let fonts = ScriptFonts::new(Arc::clone(&source) as Arc<dyn omuzaki_stage::sheet::layout::FontSource>);
    let ctx = Context::default();
    let mut link = SettingsLink::new(Arc::clone(&store)).with_fonts(fonts);
    let person = Actor::Person { name: "kit".into() };
    set(runner.as_ref(), &person, "ui.language", serde_json::json!("en"));
    frame(&ctx, &mut link);
    let fonts = link.fonts().unwrap_or_else(|| panic!("the link fetches faces"));
    assert!(!fonts.loading() && fonts.installed().is_empty(), "English needs no face");

    set(runner.as_ref(), &person, "ui.language", serde_json::json!("ja"));
    frame(&ctx, &mut link);
    let fonts = link.fonts().unwrap_or_else(|| panic!("the link fetches faces"));
    assert!(fonts.finish(&ctx), "the Japanese face arrived and went into the fonts");
    assert_eq!(fonts.installed(), vec![Family::Jp]);
    set(runner.as_ref(), &person, "ui.language", serde_json::json!("en"));
    frame(&ctx, &mut link);
    set(runner.as_ref(), &person, "ui.language", serde_json::json!("ja"));
    frame(&ctx, &mut link);
    let asked = source.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    assert_eq!(asked, vec!["NotoSansJP-Regular.otf".to_owned()], "fetched once, for Japanese only");
}
