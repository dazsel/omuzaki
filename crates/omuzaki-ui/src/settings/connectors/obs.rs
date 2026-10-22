//! The OBS Studio half of the Connectors page (#1030, BR-19): where the link to OBS's WebSocket server stands and what
//! to do next, the setup steps while it isn't connected, the port and Connect, the password, a remote OBS behind a
//! disclosure with its warning, and, once connected, OBS's scenes with the one on program (read only: switching is
//! `obs.scene.set`, from the palette, an agent or a show's rule). The footer says Omuzaki is not affiliated with the OBS
//! Project, and the page draws no logo or image of theirs: OBS Studio is plain text.
//!
//! It reads `obs.status` and `obs.scenes` and the `obs.*` settings (when it opens, after each of its own changes, and once
//! a second); every change is an op run as the person (`obs.connect` on a thread of its own, since a connection takes
//! as long as OBS takes to answer; `obs.disconnect`; `settings.set` for the port, the host and remote links), except
//! the password, which no op carries (#784): Save writes it to the keychain under `obs/websocket` and empties the
//! field, Forget deletes it, and the page never reads it back. Nothing reconnects after a wrong password; the person
//! connects again.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use egui::{RichText, ScrollArea, Ui};
use omuzaki_obs::ops::connection::DEFAULT_PASSWORD_LABEL;
use omuzaki_ops_api::{Actor, OpError, Runner};
use omuzaki_secrets_api::prelude::{SecretLabel, SecretValue};
use serde_json::{Value, json};

use super::{Keychain, run};
use crate::ds::icons;
use crate::ds::theme::ds;
use crate::ds::tokens::{Status, space};
use crate::ds::widgets::{Button, EmptyState, Section, StatusChip, TextField, Toggle};
use crate::settings::text::{Word, say};

/// Seconds between reads while the page is open.
const REFRESH_S: f64 = 1.0;

/// What the person asked for this frame.
#[derive(Clone, Copy)]
enum Action {
    Connect,
    Disconnect,
    SavePassword,
    ForgetPassword,
    AllowRemote(bool),
}

/// The `obs.*` settings as the page last read them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Where {
    host: String,
    port: u16,
    allow_remote: bool,
}

/// The OBS half's state.
#[derive(Default)]
pub struct ObsPage {
    keychain: Option<Keychain>,
    status: Option<Value>,
    /// `obs.scenes`, while connected.
    scenes: Option<Value>,
    read_at: Option<f64>,
    /// Why `obs.status` failed (no bridge in this build, for one).
    unreadable: Option<String>,
    /// Why the last change was refused.
    refused: Option<String>,
    /// The settings, once read; the fields start from them.
    saved: Option<Where>,
    port: String,
    host: String,
    allow_remote: bool,
    /// What is typed in the password field; emptied by Save.
    password: String,
    /// Whether the keychain holds a password, as last read (never the password).
    stored: bool,
    /// The connect running on its own thread.
    connecting: Option<Receiver<Result<Value, OpError>>>,
}

impl ObsPage {
    /// The page, keeping the password in `keychain`.
    pub fn new(keychain: Option<Keychain>) -> Self {
        Self { keychain, ..Self::default() }
    }

    /// The state as `obs.status` last answered (`disconnected`, `connecting`, `connected`, `retrying`, `auth_failed`
    /// or `failed`).
    pub fn state(&self) -> Option<&str> {
        self.status.as_ref().and_then(|s| s["state"].as_str())
    }

    /// Whether a connect is running.
    pub const fn connecting(&self) -> bool {
        self.connecting.is_some()
    }

    /// Draws the page; `now` is egui's clock.
    pub fn show(&mut self, ui: &mut Ui, runner: &Arc<dyn Runner>, actor: &Actor, now: f64) {
        let ctx = ui.ctx().clone();
        self.finish_connect();
        if self.read_at.is_none_or(|at| now - at >= REFRESH_S) {
            self.read(runner.as_ref(), actor, now);
        }
        if self.connecting.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let mut actions = Vec::new();
        ScrollArea::vertical().id_salt("settings.obs.scroll").auto_shrink([false, false]).show(ui, |ui| {
            if let Some(reason) = &self.refused {
                ui.label(RichText::new(say(&ctx, Word::NotSaved, &[("reason", reason)])).color(ds(&ctx).theme.danger));
            }
            match self.status.clone() {
                None => {
                    let reason = self.unreadable.clone().unwrap_or_default();
                    let headline = say(&ctx, Word::ObsUnavailable, &[]);
                    let line = say(&ctx, Word::UnavailableLine, &[("reason", &reason)]);
                    EmptyState::new(icons::GEAR, &headline, &line).show(ui);
                }
                Some(status) => self.draw(ui, &status, &mut actions),
            }
            ui.add_space(space::S12);
            ui.label(
                RichText::new(say(&ctx, Word::ObsNotAffiliated, &[]))
                    .font(ds(&ctx).text.small.clone())
                    .color(ds(&ctx).theme.text2),
            );
        });
        for action in actions {
            self.act(runner, actor, action, now);
        }
    }

    fn draw(&mut self, ui: &mut Ui, status: &Value, actions: &mut Vec<Action>) {
        let ctx = ui.ctx().clone();
        let ds = ds(&ctx);
        let state = if self.connecting.is_some() { "connecting" } else { status["state"].as_str().unwrap_or("") };
        let text = |key: &str| status[key].as_str().unwrap_or_default().to_owned();
        let (chip, line) = match state {
            "connected" => {
                let chip = say(&ctx, Word::ObsConnected, &[("version", &text("websocket_version"))]);
                let port = status["port"].as_u64().unwrap_or_default().to_string();
                ((Status::Ok, chip), Some(say(&ctx, Word::ObsWhere, &[("host", &text("host")), ("port", &port)])))
            }
            "connecting" => ((Status::Info, say(&ctx, Word::ObsConnecting, &[])), None),
            "retrying" => ((Status::Warn, say(&ctx, Word::ObsRetrying, &[("reason", &text("reason"))])), None),
            "auth_failed" => {
                ((Status::Danger, say(&ctx, Word::ObsAuthFailed, &[])), Some(say(&ctx, Word::ObsAuthFailedLine, &[])))
            }
            "failed" => ((Status::Danger, say(&ctx, Word::ObsFailed, &[("reason", &text("reason"))])), None),
            _ => ((Status::Info, say(&ctx, Word::ObsDisconnected, &[])), None),
        };
        ui.add(StatusChip::new(chip.0, &chip.1));
        if let Some(line) = line {
            ui.label(RichText::new(line).font(ds.text.small.clone()).color(ds.theme.text2));
        }
        if state == "connected" && status["unencrypted"].as_bool() == Some(true) {
            ui.add(StatusChip::new(Status::Warn, &say(&ctx, Word::ObsUnencrypted, &[])));
        }
        ui.add_space(space::S6);
        let connected = state == "connected" || state == "retrying";
        if connected {
            if ui.add(Button::new(&say(&ctx, Word::ObsDisconnect, &[])).op("obs.disconnect")).clicked() {
                actions.push(Action::Disconnect);
            }
            if state == "connected" {
                ui.add_space(space::S12);
                self.draw_scenes(ui);
            }
        } else {
            self.draw_setup(ui, state == "connecting", actions);
        }
        ui.add_space(space::S12);
        self.draw_remote(ui, actions);
    }

    /// The port and Connect, the password, and the steps in OBS.
    fn draw_setup(&mut self, ui: &mut Ui, connecting: bool, actions: &mut Vec<Action>) {
        let ctx = ui.ctx().clone();
        let ds = ds(&ctx);
        ui.horizontal(|ui| {
            let (port, hint) = (say(&ctx, Word::ObsPort, &[]), say(&ctx, Word::ObsPortHint, &[]));
            ui.add(
                TextField::new(&port, &mut self.port)
                    .hint(&hint)
                    .width(120.0)
                    .clearable(false)
                    .id_salt("settings.obs.port"),
            );
            let waiting = say(&ctx, Word::ObsConnecting, &[]);
            let connect_word = say(&ctx, Word::ObsConnect, &[]);
            let connect = Button::new(&connect_word).primary().op("obs.connect");
            if ui.add(connect.disabled_if(connecting, &waiting)).clicked() {
                actions.push(Action::Connect);
            }
        });
        ui.add_space(space::S8);
        let label = say(&ctx, Word::ObsPassword, &[]);
        if self.keychain.is_some() {
            ui.horizontal(|ui| {
                let hint = say(&ctx, Word::ObsPasswordHint, &[]);
                let field = TextField::new(&label, &mut self.password).password().hint(&hint).width(220.0);
                ui.add(field.id_salt("settings.obs.password"));
                let empty = say(&ctx, Word::ObsPasswordEmpty, &[]);
                let save_word = say(&ctx, Word::ObsSavePassword, &[]);
                let save = Button::new(&save_word);
                if ui.add(save.disabled_if(self.password.is_empty(), &empty)).clicked() {
                    actions.push(Action::SavePassword);
                }
            });
            if self.stored {
                ui.horizontal(|ui| {
                    ui.add(StatusChip::new(Status::Ok, &say(&ctx, Word::ObsPasswordSaved, &[])));
                    if ui.add(Button::new(&say(&ctx, Word::ObsForgetPassword, &[])).ghost()).clicked() {
                        actions.push(Action::ForgetPassword);
                    }
                });
            }
        } else {
            let line = say(&ctx, Word::ObsNoKeychain, &[]);
            ui.label(RichText::new(line).font(ds.text.small.clone()).color(ds.theme.text2));
        }
        ui.add_space(space::S8);
        let steps = [Word::ObsStep1, Word::ObsStep2, Word::ObsStep3, Word::ObsStep4];
        for (n, step) in steps.into_iter().enumerate() {
            let number = (n + 1).to_string();
            ui.label(say(&ctx, Word::ObsStep, &[("n", &number), ("step", &say(&ctx, step, &[]))]));
        }
    }

    /// OBS's scenes, the one on program marked.
    fn draw_scenes(&self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let ds = ds(&ctx);
        ui.label(RichText::new(say(&ctx, Word::ObsScenes, &[])).font(ds.text.strong.clone()));
        let Some(scenes) = &self.scenes else { return };
        let program = scenes["program"].as_str();
        let on_program = say(&ctx, Word::ObsOnProgram, &[]);
        for name in scenes["scenes"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            ui.horizontal(|ui| {
                ui.label(name);
                if Some(name) == program {
                    ui.add(StatusChip::new(Status::Ok, &on_program));
                }
            });
        }
    }

    /// OBS on another computer: the warning, the switch and the host.
    fn draw_remote(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let ctx = ui.ctx().clone();
        let ds = ds(&ctx);
        Section::new(&say(&ctx, Word::ObsRemote, &[])).id_salt("settings.obs.remote").show(ui, |ui| {
            ui.label(RichText::new(say(&ctx, Word::ObsRemoteWarning, &[])).color(ds.theme.warn));
            ui.add_space(space::S4);
            let mut allow = self.allow_remote;
            let switch = say(&ctx, Word::ObsAllowRemote, &[]);
            if ui.add(Toggle::switch(&switch, &mut allow).id_salt("settings.obs.allow-remote")).changed() {
                actions.push(Action::AllowRemote(allow));
            }
            if self.allow_remote {
                let (host, hint) = (say(&ctx, Word::ObsHost, &[]), say(&ctx, Word::ObsHostHint, &[]));
                ui.add(TextField::new(&host, &mut self.host).hint(&hint).width(220.0).id_salt("settings.obs.host"));
            }
        });
    }

    fn act(&mut self, runner: &Arc<dyn Runner>, actor: &Actor, action: Action, now: f64) {
        let result = match action {
            Action::Connect => self.connect(runner, actor),
            Action::Disconnect => run(runner.as_ref(), actor, "obs.disconnect", json!({})).map(drop),
            Action::SavePassword => self.save_password(),
            Action::ForgetPassword => self.forget_password(),
            Action::AllowRemote(allow) => {
                let answer =
                    run(runner.as_ref(), actor, "settings.set", json!({ "key": "obs.allow_remote", "value": allow }));
                if answer.is_ok() {
                    self.allow_remote = allow;
                }
                answer.map(drop)
            }
        };
        self.refused = result.err().map(|error| error.message);
        self.read(runner.as_ref(), actor, now);
    }

    /// Keeps the port and the host the person typed (as settings, so the next start finds OBS where they said), then
    /// runs `obs.connect` on its own thread.
    fn connect(&mut self, runner: &Arc<dyn Runner>, actor: &Actor) -> Result<(), OpError> {
        let mut args = json!({});
        let port = self.port.trim();
        if !port.is_empty() {
            let port: u16 = port.parse().map_err(|_| OpError::invalid("port", "a port from 1 to 65535, like 4455"))?;
            if self.saved.as_ref().is_none_or(|saved| saved.port != port) {
                run(runner.as_ref(), actor, "settings.set", json!({ "key": "obs.port", "value": port }))?;
            }
            args["port"] = json!(port);
        }
        let host = self.host.trim();
        if self.allow_remote && !host.is_empty() {
            if self.saved.as_ref().is_none_or(|saved| saved.host != host) {
                run(runner.as_ref(), actor, "settings.set", json!({ "key": "obs.host", "value": host }))?;
            }
            args["host"] = json!(host);
        }
        let (tx, rx) = channel();
        let (runner, actor) = (Arc::clone(runner), actor.clone());
        std::thread::Builder::new()
            .name("obs-connect".into())
            .spawn(move || {
                // The page may have closed by the time OBS answers; nobody is listening then, and that is fine.
                let _ = tx.send(run(runner.as_ref(), &actor, "obs.connect", args));
            })
            .map_err(|e| OpError::internal(format!("the connect thread did not start: {e}")))?;
        self.connecting = Some(rx);
        Ok(())
    }

    /// Takes the connect's answer once it is in.
    fn finish_connect(&mut self) {
        let Some(rx) = &self.connecting else { return };
        match rx.try_recv() {
            Ok(answer) => {
                self.connecting = None;
                self.refused = answer.err().map(|error| error.message);
                self.read_at = None;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.connecting = None;
                self.read_at = None;
            }
        }
    }

    /// Writes the typed password to the keychain and empties the field.
    fn save_password(&mut self) -> Result<(), OpError> {
        let password = std::mem::take(&mut self.password);
        let (store, label) = self.keychain_label()?;
        // Moved, not copied: the keychain's value zeroes the one buffer the field held.
        store.0.set(&label, SecretValue::new(password)).map_err(|e| OpError::internal(e.to_string()))?;
        self.stored = true;
        Ok(())
    }

    /// Deletes the stored password.
    fn forget_password(&mut self) -> Result<(), OpError> {
        let (store, label) = self.keychain_label()?;
        store.0.delete(&label).map_err(|e| OpError::internal(e.to_string()))?;
        self.stored = false;
        Ok(())
    }

    fn keychain_label(&self) -> Result<(Keychain, SecretLabel), OpError> {
        let store = self.keychain.clone().ok_or_else(|| OpError::unsupported("no keychain in this build"))?;
        let label = SecretLabel::new(DEFAULT_PASSWORD_LABEL).map_err(|e| OpError::internal(e.to_string()))?;
        Ok((store, label))
    }

    /// Reads `obs.status`, `obs.scenes` while connected, the settings and whether a password is stored.
    fn read(&mut self, runner: &dyn Runner, actor: &Actor, now: f64) {
        self.read_at = Some(now);
        match run(runner, actor, "obs.status", json!({})) {
            Ok(status) => {
                self.scenes = if status["connected"].as_bool() == Some(true) {
                    run(runner, actor, "obs.scenes", json!({})).ok()
                } else {
                    None
                };
                self.status = Some(status);
                self.unreadable = None;
            }
            Err(error) => {
                (self.status, self.scenes) = (None, None);
                self.unreadable = Some(error.message);
            }
        }
        let setting =
            |key: &str| run(runner, actor, "settings.get", json!({ "key": key })).ok().map(|v| v["value"].clone());
        let read = Where {
            host: setting("obs.host").and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default(),
            port: setting("obs.port").and_then(|v| v.as_u64()).and_then(|p| u16::try_from(p).ok()).unwrap_or_default(),
            allow_remote: setting("obs.allow_remote").and_then(|v| v.as_bool()).unwrap_or(false),
        };
        // The fields start from the settings, and follow them while the person hasn't typed something else.
        let before = self.saved.clone().unwrap_or_default();
        if self.port.is_empty() || self.port == before.port.to_string() {
            self.port = if read.port == 0 { String::new() } else { read.port.to_string() };
        }
        if self.host.is_empty() || self.host == before.host {
            self.host.clone_from(&read.host);
        }
        self.allow_remote = read.allow_remote;
        self.saved = Some(read);
        if let Ok((store, label)) = self.keychain_label() {
            self.stored = store.0.contains(&label).unwrap_or(false);
        }
    }
}
