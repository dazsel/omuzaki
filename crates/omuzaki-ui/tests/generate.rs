//! Generation in the studio over the real executor and the real connector host with the project's fake generators
//! (`omuzaki-connect-mock`), an in-memory keychain, settings and store:
//!
//! - #1185: the Generators page lists every generator with its status, price and where it sends; Enabled, the plan
//!   and the address arrive as `settings.set` of `connectors.entries` with the exact value, and the list follows at
//!   once; Test connection answers on its own thread.
//! - #1009: the key goes to the keychain under `connector/<id>` and nowhere else: the page's text, its widget labels,
//!   the accessibility tree and the op log hold no part of it; a short key is refused before the keychain is asked.
//! - #1010: the Generated tray shows the candidates with their chips and rights badge in words; Accept, Reject with
//!   a reason, batch decisions and Compare run the ops.
//! - #1011: a free local run shows no card; a paid hosted run shows the card with the exact price, Cancel sends
//!   nothing, Run passes `expect_usd_micros` equal to the price shown; a dearer run reopens the card with the new
//!   number and runs at that; a missing key is a refusal with the way to the Generators page.
//!
//! The evidence test renders the page and the tray on the software GPU (ignored; run by hand or by the visual job).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use egui::{Event, Key, Modifiers, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use omuzaki_ai_api::NoConsentRecords;
use omuzaki_connect::ConnectorSettings;
use omuzaki_connect::{ConnectorSet, Host, HostParts, SettingsConfig};
use omuzaki_connect_api::{ConnectorId, Connectors};
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::DocPath;
use omuzaki_ops_api::{
    Actor, Event as RunnerEvent, Grants, Mode, OpError, OpInfo, OpRecord, Outcome, Request, Runner, ServicesBuilder,
    TxId,
};
use omuzaki_secrets_api::prelude::{SecretLabel, SecretStore, SecretValue};
use omuzaki_secrets_mock::MemorySecrets;
use omuzaki_settings_api::service::{Layer, Settings as SettingsService, SettingsExt as _};
use omuzaki_settings_mock::MemorySettings;
use omuzaki_store_api::Store;
use omuzaki_store_mock::MemStore;
use omuzaki_ui::ds::Settings;
use omuzaki_ui::ds::testing::{all_strings, harness_state, press};
use omuzaki_ui::generate::Generation;
use omuzaki_ui::generate::tray::Tray;
use omuzaki_ui::settings::connectors::{Connector, Keychain};
use omuzaki_ui::settings::{Page, SettingsWindow};
use omuzaki_ui::wiring::{Core, Document, core_on};
use serde_json::{Value, json};

const KEY: &str = "test-only-key-0123456789";

fn you() -> Actor {
    Actor::Person { name: "you".into() }
}

/// The studio's core with the connector host over every fake generator.
struct Studio {
    core: Core,
    secrets: Arc<MemorySecrets>,
    settings: Arc<MemorySettings>,
    store: Arc<dyn Store>,
}

fn studio() -> Studio {
    omuzaki_ui::link();
    omuzaki_connect_mock::link();
    let secrets = Arc::new(MemorySecrets::new());
    let settings = Arc::new(MemorySettings::with_clock(Arc::new(FakeClock::new(Seconds(0.0)))));
    let store: Arc<dyn Store> = Arc::new(MemStore::new());
    let host = Arc::new(Host::new(HostParts {
        set: ConnectorSet::from_registered().unwrap_or_else(|e| panic!("{e:?}")),
        config: Arc::new(SettingsConfig::new(Arc::clone(&settings) as Arc<dyn SettingsService>)),
        secrets: Arc::clone(&secrets) as Arc<dyn SecretStore>,
        store: Arc::clone(&store),
        http: Arc::new(omuzaki_http_mock::FakeHttp::new()),
        pace: Arc::new(omuzaki_connect_mock::FakePace::default()),
        clock: Arc::new(FakeClock::new(Seconds(100.0))),
        consent: Arc::new(NoConsentRecords),
        services: omuzaki_ops_api::Services::default(),
    }));
    let services = ServicesBuilder::new()
        .insert::<dyn Store>(Arc::clone(&store))
        .insert::<dyn Connectors>(Arc::clone(&host) as Arc<dyn Connectors>)
        .insert::<Host>(host)
        .insert::<dyn SecretStore>(Arc::clone(&secrets) as Arc<dyn SecretStore>)
        .insert::<dyn SettingsService>(Arc::clone(&settings) as Arc<dyn SettingsService>)
        .build();
    let core = core_on(Document::empty(), services).unwrap();
    Studio { core, secrets, settings, store }
}

impl Studio {
    /// The person's settings for every generator, as the setting holds them.
    fn entries(&self) -> Value {
        serde_json::to_value(self.settings.get::<ConnectorSettings>().unwrap()).unwrap()
    }

    /// Sets `id`'s entry directly, as a person did earlier.
    fn set_entry(&self, id: &str, entry: Value) {
        let mut entries = self.settings.get::<ConnectorSettings>().unwrap();
        entries.insert(id.into(), serde_json::from_value(entry).unwrap());
        self.settings.set::<ConnectorSettings>(&entries, Layer::Auto, Mode::Apply, &you()).unwrap();
    }

    fn key(&self, id: &str) -> bool {
        self.secrets.contains(&SecretLabel::new(format!("connector/{id}")).unwrap()).unwrap()
    }

    /// Every op the person ran, with its arguments, as the log keeps them.
    fn log(&self) -> String {
        let records = self.core.runner.log(0, 10_000, &Grants::person()).unwrap();
        serde_json::to_string(&records).unwrap()
    }

    fn tray(&self) -> Vec<Value> {
        let out =
            omuzaki_ui::settings::connectors::run(self.core.runner.as_ref(), &you(), "connect.tray_list", json!({}));
        out.unwrap().as_array().cloned().unwrap()
    }
}

/// The Settings window on Connectors, Generators.
struct PageRig {
    studio: Studio,
    window: Option<SettingsWindow>,
}

fn page() -> Harness<'static, PageRig> {
    let studio = studio();
    let keychain = Keychain(Arc::clone(&studio.secrets) as Arc<dyn SecretStore>);
    let mut window = SettingsWindow::on(Page::Connectors).with_keychain(Some(keychain));
    window.connectors().show_connector(Connector::Generators);
    let rig = PageRig { studio, window: Some(window) };
    let mut h = harness_state(Settings::default(), vec2(1100.0, 1400.0), rig, |ui, r: &mut PageRig| {
        let runner = Arc::clone(&r.studio.core.runner);
        if let Some(window) = r.window.as_mut()
            && !window.show(ui.ctx(), &runner, &you())
        {
            r.window = None;
        }
    });
    h.run_steps(4);
    h
}

fn shows<S>(h: &Harness<'_, S>, text: &str) -> bool {
    all_strings(h).iter().any(|s| s.contains(text))
}

/// Runs frames, a second of egui's clock each round so the page reads again, until `done`.
fn until<S>(h: &mut Harness<'_, S>, what: &str, done: impl Fn(&Harness<'_, S>) -> bool) {
    let started = Instant::now();
    while !done(h) {
        assert!(started.elapsed() < Duration::from_secs(20), "{what} never happened: {:#?}", all_strings(h));
        std::thread::yield_now();
        h.run_steps(65);
    }
}

fn click<S>(h: &mut Harness<'_, S>, label: &str) {
    // As a person would: scroll it into view first, then press it.
    h.get_by_label(label).scroll_to_me();
    h.run();
    h.get_by_label(label).click();
    h.run_steps(3);
}

fn type_into<S>(h: &mut Harness<'_, S>, label: &str, text: &str) {
    h.get_by_label(label).scroll_to_me();
    h.run();
    h.get_by_label(label).focus();
    h.run();
    press(h, Modifiers::COMMAND, Key::A);
    h.event(Event::Text(text.to_owned()));
    h.run_steps(2);
}

#[test]
fn the_page_lists_every_generator_and_its_choices_arrive_as_the_setting() {
    let mut h = page();
    assert!(h.query_by_label("Mock images, Off").is_some(), "{:#?}", all_strings(&h));
    assert!(h.query_by_label("Mock paid images, Off").is_some());
    assert!(shows(&h, "Images \u{b7} Free"));
    assert!(shows(&h, "Stays on this computer"));
    assert!(shows(&h, "Sends to mock.invalid"), "the paid one names its host");

    click(&mut h, "Mock paid images, Off");
    assert!(shows(&h, "It goes to: mock.invalid"), "{:#?}", all_strings(&h));
    assert!(shows(&h, "What they keep: It keeps nothing: it is a test fake."));
    assert!(shows(&h, "They don't train on what you send."));
    assert!(shows(&h, "Results carry the licence LicenseRef-Omuzaki-Mock."));
    assert!(shows(&h, "Only these plans make publishable results: paid."));

    click(&mut h, "Enabled");
    assert_eq!(h.state().studio.entries()["mock-image-paid"]["enabled"], json!(true));
    until(&mut h, "the list follows", |h| h.query_by_label("Mock paid images, Needs key").is_some());

    // The plan: a dropdown of the plans the licence names.
    click(&mut h, "Your plan");
    click(&mut h, "paid");
    assert_eq!(h.state().studio.entries()["mock-image-paid"]["plan"], json!("paid"));
}

#[test]
fn the_key_goes_to_the_keychain_and_nowhere_else() {
    let mut h = page();
    click(&mut h, "Mock paid images, Off");
    assert!(shows(&h, "No key yet"));

    type_into(&mut h, "API key", "short12");
    click(&mut h, "Save key");
    assert!(shows(&h, "That doesn't look like a key"));
    assert!(!h.state().studio.key("mock-image-paid"), "a short key never reaches the keychain");

    type_into(&mut h, "API key", KEY);
    // Tab to Save keeps the key; Enter on Save stores it.
    press(&mut h, Modifiers::NONE, Key::Tab);
    h.run();
    press(&mut h, Modifiers::NONE, Key::Enter);
    h.run_steps(3);
    let studio = &h.state().studio;
    assert!(studio.key("mock-image-paid"), "the keychain holds it under connector/<id>");
    let stored = studio.secrets.get(&SecretLabel::new("connector/mock-image-paid").unwrap()).unwrap();
    assert_eq!(stored.map(|v| v.expose().to_owned()).as_deref(), Some(KEY));
    until(&mut h, "the chip", |h| shows(h, "Key saved"));

    // No part of the key anywhere a person, a screen reader or an agent could read it.
    let base64 = base64(KEY.as_bytes());
    let parts = [KEY, &KEY[5..15], base64.as_str()];
    let strings = all_strings(&h).join("\n");
    let tree: Vec<String> = h
        .root()
        .children_recursive()
        .map(|n| {
            let node = n.accesskit_node();
            format!("{:?} {:?} {:?}", node.label(), node.value(), node.description())
        })
        .collect();
    let log = h.state().studio.log();
    for part in parts {
        assert!(!strings.contains(part), "the page's text holds {part}");
        assert!(!tree.iter().any(|t| t.contains(part)), "the accessibility tree holds {part}");
        assert!(!log.contains(part), "the op log holds {part}");
    }
    let settings = serde_json::to_string(&h.state().studio.entries()).unwrap();
    assert!(!settings.contains(KEY), "the setting holds no key");

    click(&mut h, "Remove key");
    assert!(!h.state().studio.key("mock-image-paid"), "Remove deletes it");
}

#[test]
fn test_connection_answers_on_its_own_thread() {
    let mut h = page();
    click(&mut h, "Mock images, Off");
    click(&mut h, "Enabled");
    click(&mut h, "Test connection");
    until(&mut h, "the test", |h| shows(h, "It answered.") || shows(h, "It didn't answer"));
    assert!(shows(&h, "It answered."), "{:#?}", all_strings(&h));
}

/// The tray over the studio, with a ready generator or two.
struct TrayRig {
    studio: Studio,
    generation: Generation,
    tray: Tray,
}

fn tray(studio: Studio) -> Harness<'static, TrayRig> {
    let runner = Arc::clone(&studio.core.runner);
    tray_over(studio, runner)
}

/// The tray over `runner`, which may stand in front of the studio's own.
fn tray_over(studio: Studio, runner: Arc<dyn Runner>) -> Harness<'static, TrayRig> {
    let generation = Generation { runner, store: Some(Arc::clone(&studio.store)) };
    let rig = TrayRig { studio, generation, tray: Tray::default() };
    let mut h = harness_state(Settings::default(), vec2(1100.0, 1300.0), rig, |ui, r: &mut TrayRig| {
        r.tray.ui(ui, &r.generation, &you(), None);
    });
    h.run_steps(4);
    h
}

fn generate<S>(h: &mut Harness<'_, S>, prompt: &str) {
    type_into(h, "Describe it", prompt);
    click(h, "Generate");
}

#[test]
fn a_free_local_run_lands_in_the_tray_and_each_decision_is_an_op() {
    let studio = studio();
    studio.set_entry("mock-image", json!({ "enabled": true }));
    let mut h = tray(studio);
    assert!(shows(&h, "Nothing generated yet"));
    click(&mut h, "How many");
    click(&mut h, "3");
    generate(&mut h, "a soft fringe");
    assert!(!shows(&h, "Before it runs"), "a free local run shows no card");
    until(&mut h, "the run", |h| shows(h, "Made 3"));
    until(&mut h, "the tiles", |h| h.query_all_by_label_contains("a soft fringe, from mock-image").count() >= 3);
    assert_eq!(h.state().tray.confirm().last_expect(), Some(0));
    assert!(shows(&h, "Can be published"), "the rights badge in words");
    assert!(shows(&h, "mock-image \u{b7} mock-image@1 \u{b7} $0.00"), "{:#?}", all_strings(&h));

    let ids: Vec<String> = h.state().studio.tray().iter().map(|r| r["id"].as_str().unwrap().to_owned()).collect();
    h.get_all_by_label("Accept").next().unwrap().click();
    h.run_steps(3);
    let accepted =
        |h: &Harness<'_, TrayRig>| h.state().studio.tray().iter().filter(|r| r["state"] == "accepted").count();
    assert_eq!(accepted(&h), 1);

    // Reject with a reason.
    h.get_all_by_label("Reject").next().unwrap().click();
    h.run_steps(2);
    type_into(&mut h, "Why? (optional)", "too dark");
    click(&mut h, "Reject it");
    let rejected: Vec<Value> = h.state().studio.tray().into_iter().filter(|r| r["state"] == "rejected").collect();
    assert_eq!(rejected.len(), 1);
    assert_eq!(rejected[0]["reason"], json!("too dark"));

    // The rejected tile stays under its filter.
    click(&mut h, "Rejected");
    assert!(shows(&h, "Rejected: too dark"), "{:#?}", all_strings(&h));
    click(&mut h, "To decide");
    assert_eq!(ids.len(), 3);
}

#[test]
fn choosing_two_compares_them_and_batch_accept_sends_one_op_each() {
    let studio = studio();
    studio.set_entry("mock-image", json!({ "enabled": true }));
    let mut h = tray(studio);
    click(&mut h, "How many");
    click(&mut h, "2");
    generate(&mut h, "two eyes");
    until(&mut h, "the tiles", |h| h.query_all_by_label_contains("two eyes, from mock-image").count() == 2);
    let choose: Vec<String> = h
        .query_all_by_label_contains("two eyes, from mock-image")
        .map(|n| n.accesskit_node().label().unwrap_or_default())
        .collect();
    for label in &choose {
        h.get_all_by_label(label).next().unwrap().click();
        h.run_steps(2);
    }
    click(&mut h, "Compare");
    assert!(h.query_by_label("Close comparison").is_some());
    click(&mut h, "Close comparison");
    click(&mut h, "Accept 2");
    let log = h.state().studio.log();
    assert_eq!(log.matches("\"connect.accept\"").count(), 2, "one op per candidate");
    assert!(h.state().studio.tray().iter().all(|r| r["state"] == "accepted"));
}

#[test]
fn a_paid_hosted_run_asks_first_with_the_exact_price_and_run_passes_it() {
    let studio = studio();
    studio.set_entry("mock-image-paid", json!({ "enabled": true, "plan": "paid" }));
    studio.secrets.set(&SecretLabel::new("connector/mock-image-paid").unwrap(), SecretValue::new(KEY)).unwrap();
    let mut h = tray(studio);
    click(&mut h, "How many");
    click(&mut h, "2");
    generate(&mut h, "a sheet");
    assert!(shows(&h, "Before it runs"), "{:#?}", all_strings(&h));
    assert!(shows(&h, "This sends the prompt to mock.invalid and costs $0.08."), "{:#?}", all_strings(&h));
    assert!(shows(&h, "What they keep: It keeps nothing: it is a test fake."));

    click(&mut h, "Cancel");
    assert!(!shows(&h, "Before it runs"));
    assert!(!h.state().studio.log().contains("connect.generate"), "Cancel sends nothing");

    click(&mut h, "Generate");
    click(&mut h, "Run");
    until(&mut h, "the run", |h| shows(h, "Made 2"));
    assert_eq!(h.state().tray.confirm().last_expect(), Some(80_000), "Run passes the price shown");
    let log = h.state().studio.log();
    assert!(log.contains("\"expect_usd_micros\":80000"), "{log}");
    assert_eq!(h.state().studio.tray().len(), 2);
}

/// The studio's runner, except that the first `connect.generate` finds the estimate rose (the host's
/// `PreviewChanged`), and every preview after that says $0.12.
struct Dearer {
    inner: Arc<dyn Runner>,
    rose: AtomicBool,
}

impl Runner for Dearer {
    fn ops(&self) -> Vec<OpInfo> {
        self.inner.ops()
    }
    fn run(&self, req: Request) -> Result<Outcome, OpError> {
        match req.op.as_str() {
            "connect.generate" if !self.rose.swap(true, Ordering::SeqCst) => {
                Err(OpError::conflict("the estimate rose").hint("preview it again, then run that"))
            }
            "connect.preview" if self.rose.load(Ordering::SeqCst) => {
                let mut out = self.inner.run(req)?;
                if let Some(preview) = out.plan.output.as_mut() {
                    preview["estimate"]["usd_micros"] = json!(120_000);
                }
                Ok(out)
            }
            _ => self.inner.run(req),
        }
    }
    fn begin(&self, label: &str, actor: &Actor) -> TxId {
        self.inner.begin(label, actor)
    }
    fn commit(&self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        self.inner.commit(tx, actor)
    }
    fn rollback(&self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        self.inner.rollback(tx, actor)
    }
    fn undo(&self, actor: &Actor, grants: &Grants) -> Result<Option<String>, OpError> {
        self.inner.undo(actor, grants)
    }
    fn redo(&self, actor: &Actor, grants: &Grants) -> Result<Option<String>, OpError> {
        self.inner.redo(actor, grants)
    }
    fn subscribe(&self, grants: &Grants) -> Result<Receiver<RunnerEvent>, OpError> {
        self.inner.subscribe(grants)
    }
    fn get(&self, path: &DocPath, grants: &Grants) -> Result<Option<Value>, OpError> {
        self.inner.get(path, grants)
    }
    fn log(&self, after_seq: u64, limit: usize, grants: &Grants) -> Result<Vec<OpRecord>, OpError> {
        self.inner.log(after_seq, limit, grants)
    }
}

#[test]
fn a_dearer_run_reopens_the_card_with_the_new_price_and_runs_at_that() {
    let studio = studio();
    studio.set_entry("mock-image-paid", json!({ "enabled": true, "plan": "paid" }));
    studio.secrets.set(&SecretLabel::new("connector/mock-image-paid").unwrap(), SecretValue::new(KEY)).unwrap();
    let dearer: Arc<dyn Runner> =
        Arc::new(Dearer { inner: Arc::clone(&studio.core.runner), rose: AtomicBool::new(false) });
    let mut h = tray_over(studio, dearer);
    click(&mut h, "How many");
    click(&mut h, "2");
    generate(&mut h, "a sheet");
    assert!(shows(&h, "costs $0.08."), "{:#?}", all_strings(&h));
    click(&mut h, "Run");
    until(&mut h, "the card again", |h| shows(h, "The price changed before it ran."));
    assert!(shows(&h, "costs $0.12."), "the card shows the new number: {:#?}", all_strings(&h));
    assert!(!h.state().studio.log().contains("connect.generate"), "nothing ran at the old price");

    click(&mut h, "Run");
    until(&mut h, "the run", |h| shows(h, "Made 2"));
    assert_eq!(h.state().tray.confirm().last_expect(), Some(120_000), "Run passes the number shown, unrounded");
    assert!(h.state().studio.log().contains("\"expect_usd_micros\":120000"));
}

#[test]
fn a_missing_key_is_a_refusal_with_the_way_to_the_generators_page() {
    let studio = studio();
    studio.set_entry("mock-image-paid", json!({ "enabled": true, "plan": "paid" }));
    // Ready status needs the key; with none, the form offers no generator, so preview as the card would.
    let mut h = tray(studio);
    assert!(shows(&h, "Turn on a generator in Settings"), "{:#?}", all_strings(&h));
    let refused = omuzaki_ui::settings::connectors::run(
        h.state().studio.core.runner.as_ref(),
        &you(),
        "connect.preview",
        json!({ "request": { "capability": "image", "prompt": "x", "width": 64, "height": 64, "count": 1 }, "connector": "mock-image-paid" }),
    )
    .unwrap_err();
    let refusal =
        omuzaki_ui::generate::confirm::Refusal::of(&refused, &ConnectorId::new("mock-image-paid").unwrap(), None);
    assert_eq!(refusal, omuzaki_ui::generate::confirm::Refusal::NeedsKey);
    assert!(refusal.in_settings());
    h.run();
}

/// Standard base64, for searching for an encoded key.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The page and the tray for the pull request, on the software GPU at 1366x768 and 1920x1080: the paid generator's
/// detail with its key saved, and the tray with pending, accepted and rejected images and the cost card up.
/// `OMUZAKI_EVIDENCE_OUT=<folder> cargo test -p omuzaki-ui --test generate -- --ignored`.
#[test]
#[ignore = "renders on the GPU (lavapipe in CI) and writes evidence images"]
fn generate_evidence() {
    let Ok(out) = std::env::var("OMUZAKI_EVIDENCE_OUT") else { panic!("set OMUZAKI_EVIDENCE_OUT to a folder") };
    let studio = studio();
    studio.set_entry("mock-image", json!({ "enabled": true }));
    studio.set_entry("mock-image-paid", json!({ "enabled": true, "plan": "paid" }));
    studio.secrets.set(&SecretLabel::new("connector/mock-image-paid").unwrap(), SecretValue::new(KEY)).unwrap();
    let ask = |op: &str, args: Value| {
        omuzaki_ui::settings::connectors::run(studio.core.runner.as_ref(), &you(), op, args).unwrap()
    };
    let image = |prompt: &str, count: u8| json!({ "capability": "image", "prompt": prompt, "width": 64, "height": 64, "count": count });
    ask("connect.generate", json!({ "request": image("a soft fringe over the brows", 4), "connector": "mock-image" }));
    let paid = json!({ "request": image("a character sheet, front view", 2), "connector": "mock-image-paid", "expect_usd_micros": 80_000 });
    ask("connect.generate", paid);
    let rows = studio.tray();
    ask("connect.accept", json!({ "candidate": rows[0]["id"] }));
    ask("connect.reject", json!({ "candidate": rows[1]["id"], "reason": "too dark" }));

    for (w, ht) in [(1366.0, 768.0), (1920.0, 1080.0)] {
        let keychain = Keychain(Arc::clone(&studio.secrets) as Arc<dyn SecretStore>);
        let mut window = SettingsWindow::on(Page::Connectors).with_keychain(Some(keychain));
        window.connectors().show_connector(Connector::Generators);
        window.connectors().generators().select(omuzaki_connect_api::ConnectorId::new("mock-image-paid").unwrap());
        let runner = Arc::clone(&studio.core.runner);
        let mut h = Harness::builder().with_size(vec2(w, ht)).wgpu().build_ui_state(
            move |ui, window: &mut SettingsWindow| {
                omuzaki_ui::ds::apply_settings(ui.ctx(), Settings::default());
                egui::CentralPanel::default().show(ui, |_| {});
                window.show(ui.ctx(), &runner, &you());
            },
            window,
        );
        h.run_steps(10);
        h.render().unwrap().save(format!("{out}/generators-{w}x{ht}.png")).unwrap();

        let generation = Generation { runner: Arc::clone(&studio.core.runner), store: Some(Arc::clone(&studio.store)) };
        let mut h = Harness::builder().with_size(vec2(w, ht)).wgpu().build_ui_state(
            move |ui, tray: &mut Tray| {
                omuzaki_ui::ds::apply_settings(ui.ctx(), Settings::default());
                egui::CentralPanel::default().show(ui, |ui| tray.ui(ui, &generation, &you(), None));
            },
            Tray::default(),
        );
        h.run_steps(6);
        click(&mut h, "All");
        click(&mut h, "Generator");
        click(&mut h, "Mock paid images");
        type_into(&mut h, "Describe it", "a character sheet, side view");
        click(&mut h, "Generate");
        h.run_steps(12);
        assert!(shows(&h, "Before it runs"), "{:#?}", all_strings(&h));
        h.render().unwrap().save(format!("{out}/generated-{w}x{ht}.png")).unwrap();
    }
}
