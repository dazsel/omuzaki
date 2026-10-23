//! The Generators half of the Connectors page (#1185, CN-05, `connectors.md` sections 8 and 12): the image, texture,
//! voice, music and model services Omuzaki can call, each with what it makes, whether it can run, whether it costs
//! money and whether what it is sent leaves this computer.
//!
//! Choosing one shows its privacy notes and the licence of its results in plain words, and what the person decides:
//! Enabled (every generator starts off), the address of a local service (an address on another computer says that
//! data now leaves this one), the plan tier where the licence depends on it, the model licences the person declared,
//! Test connection, and the key ([`KeyField`], straight to the keychain).
//!
//! Like the other pages it holds nothing of its own: it reads `connect.list`, `connect.describe`, `connect.key_status`
//! and the `connectors.entries` setting once a second, and each change is a `settings.set` of that setting run as the
//! person (agents can't write it), so the list shows it at once.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use egui::{RichText, ScrollArea, Sense, Ui, WidgetInfo, WidgetType};
use omuzaki_connect::KeyStatus;
use omuzaki_connect::ops::list::Listed;
use omuzaki_connect_api::{
    AuthSpec, ConnectorConfig, ConnectorId, ConnectorManifest, OutputsFrom, Sends, Transport, Tri,
};
use omuzaki_ops_api::{Actor, OpError, Runner};
use omuzaki_store_api::Commercial;
use serde_json::{Value, json};

use super::{Keychain, run};
use crate::ds::icons;
use crate::ds::paint::{self, RingPlacement};
use crate::ds::theme::{Ds, ds};
use crate::ds::tokens::space;
use crate::ds::widgets::{Button, Choice, Dropdown, EmptyState, FormRow, Section, StatusChip, TextField, Toggle};
use crate::generate::key_field::KeyField;
use crate::generate::{capability, is_loopback, status, text};
use crate::panels::text::say;

/// Seconds between reads while the page is open.
const REFRESH_S: f64 = 1.0;
/// The setting every change writes.
const KEY: &str = omuzaki_connect::settings::KEY;
/// The width of the list column.
const LIST_WIDTH: f32 = 240.0;

/// The chosen generator, as the ops describe it.
struct Detail {
    id: ConnectorId,
    manifest: ConnectorManifest,
    key: Option<KeyStatus>,
}

/// A connection test: running on its own thread, or its answer.
enum Testing {
    Running(ConnectorId, Receiver<Result<Value, OpError>>),
    Done(ConnectorId, Result<(), String>),
}

/// What the person asked for this frame.
enum Action {
    Select(ConnectorId),
    Enable(bool),
    Endpoint(Option<String>),
    Plan(Option<String>),
    Test,
    KeyChanged,
}

/// The page's state, kept while the Settings window is open.
#[derive(Default)]
pub struct GeneratorsPage {
    keychain: Option<Keychain>,
    listed: Option<Result<Vec<Listed>, String>>,
    /// Every generator's manifest, read once.
    manifests: BTreeMap<ConnectorId, ConnectorManifest>,
    entries: BTreeMap<String, ConnectorConfig>,
    read_at: Option<f64>,
    selected: Option<ConnectorId>,
    detail: Option<Detail>,
    /// The address as typed, and the generator it was typed for.
    endpoint: String,
    endpoint_for: Option<ConnectorId>,
    key: KeyField,
    testing: Option<Testing>,
    refused: Option<String>,
}

impl GeneratorsPage {
    /// The page, keeping keys in `keychain` (without one, the key field says it can't be saved).
    pub fn new(keychain: Option<Keychain>) -> Self {
        Self { keychain, ..Self::default() }
    }

    /// The generators as `connect.list` last answered.
    pub fn listed(&self) -> &[Listed] {
        self.listed.as_ref().and_then(|l| l.as_ref().ok()).map_or(&[], Vec::as_slice)
    }

    /// The generator on show.
    pub fn selected(&self) -> Option<&ConnectorId> {
        self.selected.as_ref()
    }

    /// Shows `id`.
    pub fn select(&mut self, id: ConnectorId) {
        self.selected = Some(id);
        self.read_at = None;
    }

    /// Whether a connection test is running.
    pub fn testing(&self) -> bool {
        matches!(self.testing, Some(Testing::Running(..)))
    }

    /// Draws the page; `now` is egui's clock.
    pub fn show(&mut self, ui: &mut Ui, runner: &Arc<dyn Runner>, actor: &Actor, now: f64) {
        let ds = ds(ui.ctx());
        self.poll_test(ui);
        if self.read_at.is_none_or(|at| now - at >= REFRESH_S) {
            self.read(runner.as_ref(), actor, now);
        }
        let mut actions = Vec::new();
        if let Some(reason) = &self.refused {
            ui.label(RichText::new(say(&ds, text::NOT_SAVED, &[("reason", reason)])).color(ds.theme.danger));
        }
        match &self.listed {
            None => {}
            Some(Err(reason)) => {
                let headline = say(&ds, text::UNAVAILABLE, &[]);
                let line = say(&ds, text::UNAVAILABLE_WHY, &[("reason", reason)]);
                EmptyState::new(icons::GEAR, &headline, &line).show(ui);
            }
            Some(Ok(listed)) if listed.is_empty() => {
                let headline = say(&ds, text::NO_GENERATORS, &[]);
                let line = say(&ds, text::NO_GENERATORS_HOW, &[]);
                EmptyState::new(icons::SPARKLE, &headline, &line).show(ui);
            }
            Some(Ok(listed)) => {
                let listed = listed.clone();
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(LIST_WIDTH);
                        ScrollArea::vertical().id_salt("settings.generators.list").auto_shrink([false, true]).show(
                            ui,
                            |ui| {
                                for row in &listed {
                                    self.row(ui, &ds, row, &mut actions);
                                }
                            },
                        );
                    });
                    ui.separator();
                    ui.vertical(|ui| {
                        ScrollArea::vertical()
                            .id_salt("settings.generators.detail")
                            .auto_shrink([false, true])
                            .show(ui, |ui| self.detail(ui, &ds, &listed, &mut actions));
                    });
                });
            }
        }
        for action in actions {
            self.act(runner, actor, action, now);
        }
    }

    /// One generator in the list: its name, status, what it makes, whether it is paid and where it sends.
    fn row(&self, ui: &mut Ui, ds: &Ds, row: &Listed, actions: &mut Vec<Action>) {
        let selected = self.selected.as_ref() == Some(&row.id);
        let (tone, word) = status(ds, &row.status);
        let frame = egui::Frame::new().inner_margin(space::S8).corner_radius(6.0).fill(if selected {
            ds.theme.bg3
        } else {
            ds.theme.bg2
        });
        let shown = frame.show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(&row.name).strong().color(ds.theme.text));
                ui.add(StatusChip::new(tone, &word));
            });
            let made: Vec<String> = row.capabilities.iter().map(|c| capability(ds, *c)).collect();
            let price = say(ds, if row.paid { text::PAID } else { text::FREE }, &[]);
            let line = format!("{} \u{b7} {price}", made.join(", "));
            ui.label(RichText::new(line).font(ds.text.small.clone()).color(ds.theme.text2));
            let place = self.place(ds, row);
            ui.label(RichText::new(place).font(ds.text.small.clone()).color(ds.theme.text2));
        });
        let name = say(ds, text::ROW_NAME, &[("name", &row.name), ("status", &word)]).into_owned();
        let response = ui.interact(shown.response.rect, ui.id().with(("generator", row.id.as_str())), Sense::click());
        response.widget_info(|| WidgetInfo::selected(WidgetType::SelectableLabel, true, selected, &name));
        paint::focus_ring(ui.painter(), &ds.theme, &response, shown.response.rect, 6, RingPlacement::Inside);
        if response.clicked() {
            actions.push(Action::Select(row.id.clone()));
        }
        ui.add_space(space::S4);
    }

    /// "Sends to <host>" or "Stays on this computer", from the person's address when they set one.
    fn place(&self, ds: &Ds, row: &Listed) -> String {
        let endpoint = self.entries.get(row.id.as_str()).and_then(|c| c.endpoint.clone());
        let declared = self.manifests.get(&row.id).and_then(|m| m.privacy.destinations.first().cloned());
        let host = endpoint.as_deref().map(host_of).or(declared);
        let leaves = endpoint.as_deref().map_or(row.leaves_machine, |e| !is_loopback(e));
        if leaves {
            let host = host.unwrap_or_else(|| "?".to_owned());
            say(ds, text::SENDS_TO, &[("host", &host)]).into_owned()
        } else {
            say(ds, text::STAYS, &[]).into_owned()
        }
    }

    /// The chosen generator: its notes, its licence and the person's choices.
    #[allow(clippy::too_many_lines, reason = "one form, top to bottom")]
    fn detail(&mut self, ui: &mut Ui, ds: &Ds, listed: &[Listed], actions: &mut Vec<Action>) {
        let Some(detail) = &self.detail else { return };
        let Some(row) = listed.iter().find(|l| l.id == detail.id) else { return };
        let manifest = &detail.manifest;
        let config = self.entries.get(detail.id.as_str()).cloned().unwrap_or_default();
        let (tone, word) = status(ds, &row.status);
        ui.horizontal(|ui| {
            ui.label(RichText::new(&manifest.name).font(ds.text.section.clone()).color(ds.theme.text));
            ui.add(StatusChip::new(tone, &word));
        });
        ui.add_space(space::S8);
        let mut enabled = config.enabled;
        let label = say(ds, text::ENABLED, &[]);
        if ui.add(Toggle::switch(&label, &mut enabled).id_salt(("generator.enabled", detail.id.as_str()))).changed() {
            actions.push(Action::Enable(enabled));
        }
        ui.add_space(space::S8);

        let privacy = say(ds, text::PRIVACY, &[]);
        Section::new(&privacy).default_open(true).id_salt(("generator.privacy", detail.id.as_str())).show(ui, |ui| {
            let notes = &manifest.privacy;
            let small = |ui: &mut Ui, line: &str| {
                ui.label(RichText::new(line).color(ds.theme.text2));
            };
            if notes.sends.is_empty() {
                small(ui, &say(ds, text::SENDS_NOTHING, &[]));
            } else {
                let what: Vec<String> = notes.sends.iter().map(|s| sends(ds, *s)).collect();
                small(ui, &say(ds, text::SENDS, &[("what", &what.join(", "))]));
            }
            let hosts = config.endpoint.as_deref().map_or_else(|| notes.destinations.join(", "), host_of);
            if !hosts.is_empty() {
                small(ui, &say(ds, text::GOES_TO, &[("hosts", &hosts)]));
            }
            if !notes.retention.trim().is_empty() {
                small(ui, &say(ds, text::RETENTION, &[("words", &notes.retention)]));
            }
            let trains = match notes.trains_on_inputs {
                Tri::Yes => text::TRAINS_YES,
                Tri::No => text::TRAINS_NO,
                Tri::Unknown => text::TRAINS_UNKNOWN,
            };
            small(ui, &say(ds, trains, &[]));
        });

        let licence = say(ds, text::LICENCE, &[]);
        Section::new(&licence).default_open(true).id_salt(("generator.licence", detail.id.as_str())).show(ui, |ui| {
            let policy = &manifest.licence;
            if policy.outputs == OutputsFrom::UserModel {
                ui.label(RichText::new(say(ds, text::LICENCE_FROM_MODEL, &[])).color(ds.theme.text2));
            } else {
                let line = say(ds, text::LICENCE_LINE, &[("licence", &policy.spdx)]);
                ui.label(RichText::new(line).color(ds.theme.text2));
                ui.label(RichText::new(say(ds, commercial(policy.commercial), &[])).color(ds.theme.text2));
            }
            if !policy.requires_plan.is_empty() {
                let line = say(ds, text::PLAN_RULE, &[("plans", &policy.requires_plan.join(", "))]);
                ui.label(RichText::new(line).color(ds.theme.text2));
            }
            if policy.outputs == OutputsFrom::UserModel {
                ui.add_space(space::S6);
                ui.label(RichText::new(say(ds, text::MODELS, &[])).strong());
                if config.models.is_empty() {
                    ui.label(RichText::new(say(ds, text::NO_MODELS, &[])).color(ds.theme.text2));
                }
                for (model, declared) in &config.models {
                    let word = match declared.commercial {
                        Commercial::Ok => text::MODEL_COMMERCIAL,
                        Commercial::No => text::MODEL_NONCOMMERCIAL,
                        Commercial::Unknown => text::MODEL_UNKNOWN,
                    };
                    let line = say(
                        ds,
                        text::MODEL_LINE,
                        &[("model", model), ("licence", &declared.spdx), ("commercial", &say(ds, word, &[]))],
                    );
                    ui.label(RichText::new(line).color(ds.theme.text2));
                }
            }
        });

        let addressed = matches!(manifest.transport, Transport::Http | Transport::Mcp) && manifest.endpoint.is_some();
        if addressed {
            ui.add_space(space::S8);
            let name = say(ds, text::ENDPOINT, &[]);
            let hint = say(ds, text::ENDPOINT_HINT, &[]);
            FormRow::new(&name).show(ui, |ui, name| {
                ui.horizontal(|ui| {
                    let width = (ui.available_width() - 120.0).clamp(140.0, 280.0);
                    let field = TextField::new(name, &mut self.endpoint).hint(&hint).width(width);
                    let field = ui.add(field.id_salt(("generator.endpoint", detail.id.as_str())));
                    let save = say(ds, text::ENDPOINT_SAVE, &[]);
                    let entered = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.add(Button::new(&save).op("settings.set")).clicked() || entered {
                        let typed = self.endpoint.trim().to_owned();
                        actions.push(Action::Endpoint((!typed.is_empty()).then_some(typed)));
                    }
                    if config.endpoint.is_some() {
                        let reset = say(ds, text::ENDPOINT_RESET, &[]);
                        if ui.add(Button::new(&reset).ghost()).clicked() {
                            actions.push(Action::Endpoint(None));
                        }
                    }
                });
            });
            let shown = if self.endpoint.trim().is_empty() {
                manifest.endpoint.clone().unwrap_or_default()
            } else {
                self.endpoint.trim().to_owned()
            };
            let (word, colour) = if is_loopback(&shown) {
                (text::ENDPOINT_LOCAL, ds.theme.text2)
            } else {
                (text::ENDPOINT_LEAVES, ds.theme.warn)
            };
            ui.label(RichText::new(say(ds, word, &[])).font(ds.text.small.clone()).color(colour));
        }

        let plans = &manifest.licence.requires_plan;
        if !plans.is_empty() {
            ui.add_space(space::S8);
            let none = say(ds, text::PLAN_NONE, &[]);
            let mut options: Vec<Option<String>> = vec![None];
            options.extend(plans.iter().cloned().map(Some));
            if let Some(current) = &config.plan
                && !plans.contains(current)
            {
                options.push(Some(current.clone()));
            }
            let words: Vec<&str> = options.iter().map(|o| o.as_deref().unwrap_or(&none)).collect();
            let choices: Vec<Choice<'_, usize>> = words.iter().enumerate().map(|(n, w)| Choice::new(n, w)).collect();
            let mut at = options.iter().position(|o| *o == config.plan).unwrap_or(0);
            let label = say(ds, text::PLAN, &[]);
            let changed = FormRow::new(&label).show(ui, |ui, label| {
                let dropdown = Dropdown::new(label, &mut at, &choices).id_salt(("generator.plan", detail.id.as_str()));
                ui.add(dropdown).changed()
            });
            if changed.inner {
                actions.push(Action::Plan(options.get(at).cloned().flatten()));
            }
        }

        if let AuthSpec::ApiKey { secret: Some(label), .. } = &manifest.auth {
            ui.add_space(space::S8);
            let present = detail.key.as_ref().map(|k| k.present);
            if self.key.show(ui, self.keychain.as_ref(), label, present) {
                actions.push(Action::KeyChanged);
            }
        }

        ui.add_space(space::S8);
        ui.horizontal(|ui| {
            let test = say(ds, text::TEST, &[]);
            let running = self.testing();
            let busy = say(ds, text::TESTING, &[]);
            if ui.add(Button::new(&test).op("connect.test").disabled_if(running, &busy)).clicked() {
                actions.push(Action::Test);
            }
            match &self.testing {
                Some(Testing::Running(id, _)) if *id == detail.id => {
                    ui.label(RichText::new(say(ds, text::TESTING, &[])).color(ds.theme.text2));
                }
                Some(Testing::Done(id, Ok(()))) if *id == detail.id => {
                    ui.label(RichText::new(say(ds, text::TEST_OK, &[])).color(ds.theme.ok));
                }
                Some(Testing::Done(id, Err(reason))) if *id == detail.id => {
                    let line = say(ds, text::TEST_FAILED, &[("reason", reason)]);
                    ui.label(RichText::new(line).color(ds.theme.danger));
                }
                _ => {}
            }
        });
    }

    fn act(&mut self, runner: &Arc<dyn Runner>, actor: &Actor, action: Action, now: f64) {
        let Some(id) = (match &action {
            Action::Select(id) => Some(id.clone()),
            _ => self.selected.clone(),
        }) else {
            return;
        };
        let result = match action {
            Action::Select(_) => {
                self.select(id);
                Ok(())
            }
            Action::Enable(on) => self.change(runner.as_ref(), actor, &id, |c| c.enabled = on),
            Action::Endpoint(endpoint) => {
                let done = self.change(runner.as_ref(), actor, &id, |c| c.endpoint.clone_from(&endpoint));
                if done.is_ok() {
                    self.endpoint = endpoint.unwrap_or_default();
                }
                done
            }
            Action::Plan(plan) => self.change(runner.as_ref(), actor, &id, |c| c.plan = plan),
            Action::Test => {
                self.test(runner, actor, id);
                Ok(())
            }
            Action::KeyChanged => Ok(()),
        };
        self.refused = result.err().map(|e| e.message);
        self.read(runner.as_ref(), actor, now);
    }

    /// Writes `connectors.entries` with `id`'s entry changed by `edit`.
    fn change(
        &mut self,
        runner: &dyn Runner,
        actor: &Actor,
        id: &ConnectorId,
        edit: impl FnOnce(&mut ConnectorConfig),
    ) -> Result<(), OpError> {
        let mut entries = self.entries.clone();
        edit(entries.entry(id.to_string()).or_default());
        let value = serde_json::to_value(&entries).map_err(|e| OpError::internal(e.to_string()))?;
        run(runner, actor, "settings.set", json!({ "key": KEY, "value": value }))?;
        self.entries = entries;
        Ok(())
    }

    /// Starts `connect.test` on a thread of its own: it reaches the service, which can take a while.
    fn test(&mut self, runner: &Arc<dyn Runner>, actor: &Actor, id: ConnectorId) {
        let (tx, rx) = channel();
        let (runner, actor, args) = (Arc::clone(runner), actor.clone(), json!({ "connector": id }));
        let spawned = std::thread::Builder::new().name("connect-test".into()).spawn(move || {
            // The page may have closed by the time the test ends; nobody is listening then, and that is fine.
            let _ = tx.send(run(runner.as_ref(), &actor, "connect.test", args));
        });
        self.testing = Some(match spawned {
            Ok(_) => Testing::Running(id, rx),
            Err(e) => Testing::Done(id, Err(e.to_string())),
        });
    }

    /// Takes a finished test's answer.
    fn poll_test(&mut self, ui: &Ui) {
        let Some(Testing::Running(id, rx)) = &self.testing else { return };
        let answer = match rx.try_recv() {
            Ok(Ok(answer)) if answer["ok"].as_bool() == Some(true) => Ok(()),
            Ok(Ok(answer)) => Err(answer["error"].as_str().unwrap_or_default().to_owned()),
            Ok(Err(e)) => Err(e.message),
            Err(TryRecvError::Empty) => {
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
                return;
            }
            Err(TryRecvError::Disconnected) => Err("the test stopped".to_owned()),
        };
        self.testing = Some(Testing::Done(id.clone(), answer));
    }

    /// Reads the list, the setting, and the chosen generator's manifest and key status.
    fn read(&mut self, runner: &dyn Runner, actor: &Actor, now: f64) {
        self.read_at = Some(now);
        let listed = run(runner, actor, "connect.list", json!({})).and_then(|v| {
            serde_json::from_value::<Vec<Listed>>(v).map_err(|e| OpError::internal(format!("connect.list: {e}")))
        });
        self.listed = Some(listed.map_err(|e| e.message));
        if let Ok(answer) = run(runner, actor, "settings.get", json!({ "key": KEY })) {
            self.entries = serde_json::from_value(answer["value"].clone()).unwrap_or_default();
        }
        if self.selected.is_none() {
            self.selected = self.listed().first().map(|l| l.id.clone());
        }
        let Some(id) = self.selected.clone() else { return };
        if self.endpoint_for.as_ref() != Some(&id) {
            self.endpoint = self.entries.get(id.as_str()).and_then(|c| c.endpoint.clone()).unwrap_or_default();
            self.endpoint_for = Some(id.clone());
        }
        let unread: Vec<ConnectorId> =
            self.listed().iter().map(|l| l.id.clone()).filter(|id| !self.manifests.contains_key(id)).collect();
        for id in unread {
            let manifest = run(runner, actor, "connect.describe", json!({ "connector": id }))
                .ok()
                .and_then(|v| serde_json::from_value::<ConnectorManifest>(v).ok());
            if let Some(manifest) = manifest {
                self.manifests.insert(id, manifest);
            }
        }
        let manifest = self.manifests.get(&id).cloned();
        let key = run(runner, actor, "connect.key_status", json!({ "connector": id }))
            .ok()
            .and_then(|v| serde_json::from_value::<KeyStatus>(v).ok());
        self.detail = manifest.map(|manifest| Detail { id, manifest, key });
    }
}

/// What is sent, in words.
fn sends(ds: &Ds, what: Sends) -> String {
    let word = match what {
        Sends::Prompt => text::SENDS_PROMPT,
        Sends::Images => text::SENDS_IMAGES,
        Sends::Audio => text::SENDS_AUDIO,
        Sends::Meshes => text::SENDS_MESHES,
        Sends::Video => text::SENDS_VIDEO,
        Sends::Files => text::SENDS_FILES,
    };
    say(ds, word, &[]).into_owned()
}

/// The commercial line for a licence.
pub(crate) const fn commercial(commercial: Commercial) -> &'static str {
    match commercial {
        Commercial::Ok => text::COMMERCIAL_OK,
        Commercial::No => text::COMMERCIAL_NO,
        Commercial::Unknown => text::COMMERCIAL_UNKNOWN,
    }
}

/// The host of an address, for "Sends to".
fn host_of(address: &str) -> String {
    let rest = address.split_once("://").map_or(address, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    authority.rsplit_once('@').map_or(authority, |(_, host)| host).to_owned()
}
