//! The cost and privacy card before a run (#1011, CN-04, `connectors.md` sections 5 and 6): before a `connect.generate`
//! started from the studio, `connect.preview` says what the run would send, where, at what price and under which
//! licence. A run that leaves the computer or costs money shows the card first: "This sends 4 images and the prompt to
//! api.example.com and costs $0.16", what the service keeps and whether it trains on it, and what the results may be
//! used for. **Run passes `expect_usd_micros` equal to the number shown**, so a dearer run fails with a conflict
//! before anything is sent, and the card opens again with the new price. A free run that stays on the computer shows
//! no card, only "Stays on this computer".
//!
//! A preview that is refused (private test material, a missing key or plan, a likeness without consent) shows what to
//! do and a button to the Generators page. The card's words come from the preview's fields, in the person's
//! language; [`sentence`] is the same sentence for anything else that asks the person.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use egui::{RichText, Ui};
use omuzaki_connect_api::{ConnectorId, Preview, PreviewError, Sends, Status, Tri};
use omuzaki_ops_api::{Actor, ErrorCode, OpError, Runner};
use serde_json::{Value, json};

use super::text;
use crate::ds::theme::{Ds, ds};
use crate::ds::tokens::{Status as Tone, space};
use crate::ds::widgets::{Button, StatusChip};
use crate::panels::text::say;
use crate::settings::connectors::run;

/// The card as the preview makes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CardState {
    /// Free and on this computer: no card, the run starts, and this line says so.
    Local {
        /// "Stays on this computer".
        line: String,
    },
    /// It leaves the computer or costs money: ask first.
    Ask {
        /// What is sent where and what it costs.
        sentence: String,
        /// The lines under it: what the service keeps, whether it trains on it, the results' licence, warnings.
        notes: Vec<String>,
        /// The price shown, which Run passes as `expect_usd_micros`.
        usd_micros: u64,
    },
}

/// Why a run can't start, as the card says it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// An input is private test material.
    PrivateInput,
    /// The generator's key is missing.
    NeedsKey,
    /// The plan tier is missing or not one that makes publishable results.
    NeedsPlan,
    /// A real person's likeness or voice without consent on record.
    NeedsConsent,
    /// Anything else, in the op's words.
    Other(String),
}

impl Refusal {
    /// What a refused `connect.preview` for `connector` means, given its status from `connect.list`. Read from the
    /// error's code and the host's own refusal, never from words in the message.
    pub fn of(error: &OpError, connector: &ConnectorId, status: Option<&Status>) -> Self {
        let no_key = PreviewError::NeedsKey(connector.clone()).to_string();
        match (error.code, status) {
            // The host refuses private test material without a hint, and a likeness without consent with one.
            (ErrorCode::Denied, _) if error.hint.is_none() => Self::PrivateInput,
            (ErrorCode::Denied, _) => Self::NeedsConsent,
            (_, Some(Status::NeedsKey)) => Self::NeedsKey,
            (_, Some(Status::NeedsPlan { .. })) => Self::NeedsPlan,
            _ if error.message == no_key => Self::NeedsKey,
            _ => Self::Other(error.message.clone()),
        }
    }

    /// The line the card shows.
    pub fn line(&self, ds: &Ds) -> String {
        match self {
            Self::PrivateInput => say(ds, text::REFUSED_PRIVATE, &[]).into_owned(),
            Self::NeedsKey => say(ds, text::REFUSED_KEY, &[]).into_owned(),
            Self::NeedsPlan => say(ds, text::REFUSED_PLAN, &[]).into_owned(),
            Self::NeedsConsent => say(ds, text::REFUSED_CONSENT, &[]).into_owned(),
            Self::Other(reason) => say(ds, text::REFUSED_OTHER, &[("reason", reason)]).into_owned(),
        }
    }

    /// Whether the Generators page is where the person fixes it.
    pub const fn in_settings(&self) -> bool {
        matches!(self, Self::NeedsKey | Self::NeedsPlan | Self::Other(_))
    }
}

/// The card for `preview`.
pub fn card(ds: &Ds, preview: &Preview) -> CardState {
    if !preview.leaves_machine && preview.estimate.usd_micros == 0 && !preview.needs_spend {
        return CardState::Local { line: say(ds, text::LOCAL_LINE, &[]).into_owned() };
    }
    let mut notes = Vec::new();
    if preview.leaves_machine {
        if !preview.retention.trim().is_empty() {
            notes.push(say(ds, text::RETENTION, &[("words", &preview.retention)]).into_owned());
        }
        let trains = match preview.trains_on_inputs {
            Tri::Yes => text::TRAINS_YES,
            Tri::No => text::TRAINS_NO,
            Tri::Unknown => text::TRAINS_UNKNOWN,
        };
        notes.push(say(ds, trains, &[]).into_owned());
    }
    let commercial = say(ds, crate::settings::connectors::generators::commercial(preview.commercial), &[]);
    notes.push(say(ds, text::CARD_LICENCE, &[("licence", &preview.licence), ("commercial", &commercial)]).into_owned());
    notes.extend(preview.warnings.iter().cloned());
    CardState::Ask { sentence: sentence(ds, preview), notes, usd_micros: preview.estimate.usd_micros }
}

/// "This sends 4 images and the prompt to api.example.com and costs $0.16", from the preview's fields: the price is
/// exactly the preview's, never rounded, and an upper bound says "up to".
pub fn sentence(ds: &Ds, preview: &Preview) -> String {
    let price = omuzaki_connect_api::usd(preview.estimate.usd_micros);
    let about = if preview.estimate.exact { String::new() } else { say(ds, text::ABOUT, &[]).into_owned() };
    if !preview.leaves_machine {
        return say(ds, text::CARD_PAID_LOCAL, &[("about", &about), ("usd", &price)]).into_owned();
    }
    let mut parts: Vec<String> = Vec::new();
    for out in &preview.outbound {
        let word = match out.what {
            Sends::Prompt => {
                parts.push(say(ds, text::SENDS_PROMPT, &[]).into_owned());
                continue;
            }
            Sends::Images => text::SENDS_IMAGES,
            Sends::Audio => text::SENDS_AUDIO,
            Sends::Meshes => text::SENDS_MESHES,
            Sends::Video => text::SENDS_VIDEO,
            Sends::Files => text::SENDS_FILES,
        };
        let count = out.count.to_string();
        parts.push(say(ds, text::COUNTED, &[("count", &count), ("what", &say(ds, word, &[]))]).into_owned());
    }
    if parts.is_empty() {
        parts.push(say(ds, text::SENDS_PROMPT, &[]).into_owned());
    }
    let what = parts.join(&say(ds, text::AND, &[]));
    let hosts = preview.endpoint.as_deref().map_or_else(|| preview.destinations.join(", "), host_of);
    if preview.estimate.usd_micros == 0 {
        say(ds, text::CARD_SENDS_FREE, &[("what", &what), ("hosts", &hosts)]).into_owned()
    } else {
        say(ds, text::CARD_SENDS_PAID, &[("what", &what), ("hosts", &hosts), ("about", &about), ("usd", &price)])
            .into_owned()
    }
}

/// An address's host.
fn host_of(address: &str) -> String {
    let rest = address.split_once("://").map_or(address, |(_, rest)| rest);
    rest.split(['/', '?', '#']).next().unwrap_or(rest).to_owned()
}

/// Where a run started from the studio stands.
#[derive(Default)]
enum Stage {
    #[default]
    Idle,
    /// The card is up.
    Asking { connector: ConnectorId, request: Value, preview: Box<Preview>, changed: bool },
    /// The preview was refused.
    Refused(Refusal),
    /// Running on its own thread.
    Running { name: String, answer: Receiver<Result<Value, OpError>>, connector: ConnectorId, request: Value },
    /// Finished: how many files it made, or why it failed.
    Done(Result<usize, String>),
}

/// A run from preview to result.
#[derive(Default)]
pub struct Confirm {
    stage: Stage,
    /// The `expect_usd_micros` of the last run started, for tests and the log.
    last_expect: Option<u64>,
}

impl std::fmt::Debug for Confirm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Confirm").field("last_expect", &self.last_expect).finish_non_exhaustive()
    }
}

/// What the card asked the host to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardOutput {
    /// Nothing.
    Nothing,
    /// Open the Generators page.
    OpenSettings,
    /// A run finished: the tray reads again.
    Finished,
}

impl Confirm {
    /// Whether the card is waiting for the person.
    pub const fn asking(&self) -> bool {
        matches!(self.stage, Stage::Asking { .. })
    }

    /// Whether a run is going on.
    pub const fn running(&self) -> bool {
        matches!(self.stage, Stage::Running { .. })
    }

    /// The `expect_usd_micros` the last run passed.
    pub const fn last_expect(&self) -> Option<u64> {
        self.last_expect
    }

    /// Previews `request` on `connector` and asks, or runs at once when it is free and local. `status` is the
    /// generator's status from `connect.list`, for the refusal's words.
    pub fn start(
        &mut self,
        ui: &Ui,
        runner: &Arc<dyn Runner>,
        actor: &Actor,
        connector: ConnectorId,
        request: Value,
        status: Option<&Status>,
    ) {
        self.preview(ui, runner, actor, connector, request, status, false);
    }

    #[allow(clippy::too_many_arguments, reason = "the card's whole input")]
    fn preview(
        &mut self,
        ui: &Ui,
        runner: &Arc<dyn Runner>,
        actor: &Actor,
        connector: ConnectorId,
        request: Value,
        status: Option<&Status>,
        changed: bool,
    ) {
        let asked =
            run(runner.as_ref(), actor, "connect.preview", json!({ "request": request, "connector": connector }))
                .and_then(|v| {
                    serde_json::from_value::<Preview>(v).map_err(|e| OpError::internal(format!("connect.preview: {e}")))
                });
        let preview = match asked {
            Ok(preview) => preview,
            Err(error) => {
                self.stage = Stage::Refused(Refusal::of(&error, &connector, status));
                return;
            }
        };
        match card(&ds(ui.ctx()), &preview) {
            CardState::Local { .. } => self.run(runner, actor, connector, request, &preview),
            CardState::Ask { .. } => {
                self.stage = Stage::Asking { connector, request, preview: Box::new(preview), changed };
            }
        }
    }

    /// Runs `connect.generate` with the price the person saw, on a thread of its own.
    fn run(&mut self, runner: &Arc<dyn Runner>, actor: &Actor, connector: ConnectorId, request: Value, seen: &Preview) {
        let expect = seen.estimate.usd_micros;
        self.last_expect = Some(expect);
        let args = json!({ "request": request, "connector": connector, "expect_usd_micros": expect });
        let (tx, rx) = channel();
        let (thread_runner, thread_actor) = (Arc::clone(runner), actor.clone());
        let spawned = std::thread::Builder::new().name("connect-generate".into()).spawn(move || {
            // The panel may have closed by the time the run ends; nobody is listening then, and that is fine.
            let _ = tx.send(run(thread_runner.as_ref(), &thread_actor, "connect.generate", args));
        });
        self.stage = match spawned {
            Ok(_) => Stage::Running { name: seen.name.clone(), answer: rx, connector, request },
            Err(e) => Stage::Done(Err(e.to_string())),
        };
    }

    /// Draws the card, the running line or the result; `status` is as for [`start`](Self::start).
    pub fn show(
        &mut self,
        ui: &mut Ui,
        runner: &Arc<dyn Runner>,
        actor: &Actor,
        status: Option<&Status>,
    ) -> CardOutput {
        let ds = ds(ui.ctx());
        let mut out = CardOutput::Nothing;
        if let Stage::Running { answer, .. } = &self.stage {
            match answer.try_recv() {
                Err(TryRecvError::Empty) => {
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
                }
                Err(TryRecvError::Disconnected) => self.stage = Stage::Done(Err("the run stopped".to_owned())),
                Ok(answer) => {
                    out = CardOutput::Finished;
                    let Stage::Running { connector, request, .. } = std::mem::take(&mut self.stage) else {
                        return out;
                    };
                    match answer {
                        Ok(answer) => {
                            let made = answer["candidates"].as_array().map_or(0, Vec::len);
                            self.stage = Stage::Done(Ok(made));
                        }
                        // The price went up between the card and the run: ask again with the new one.
                        Err(e) if e.code == ErrorCode::Conflict => {
                            self.preview(ui, runner, actor, connector, request, status, true);
                        }
                        Err(e) => self.stage = Stage::Done(Err(e.message)),
                    }
                }
            }
        }
        match &self.stage {
            Stage::Idle => {}
            Stage::Running { name, .. } => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new(say(&ds, text::MAKING, &[("name", name)])).color(ds.theme.text2));
                });
            }
            Stage::Done(Ok(made)) => {
                let line = say(&ds, text::RUN_DONE, &[("count", &made.to_string())]);
                ui.label(RichText::new(line).color(ds.theme.ok));
            }
            Stage::Done(Err(reason)) => {
                ui.label(RichText::new(say(&ds, text::RUN_FAILED, &[("reason", reason)])).color(ds.theme.danger));
            }
            Stage::Refused(refusal) => {
                let mut open = false;
                card_frame(ui, &ds, |ui| {
                    ui.add(StatusChip::new(Tone::Warn, &say(&ds, text::CARD_TITLE, &[])));
                    ui.label(RichText::new(refusal.line(&ds)).color(ds.theme.text));
                    if refusal.in_settings() {
                        let label = say(&ds, text::OPEN_SETTINGS, &[]);
                        open = ui.add(Button::new(&label)).clicked();
                    }
                });
                if open {
                    out = CardOutput::OpenSettings;
                    self.stage = Stage::Idle;
                }
            }
            Stage::Asking { preview, changed, .. } => {
                let state = card(&ds, preview);
                let CardState::Ask { sentence, notes, .. } = &state else { return out };
                let (mut go, mut cancel) = (false, false);
                card_frame(ui, &ds, |ui| {
                    ui.label(RichText::new(say(&ds, text::CARD_TITLE, &[])).font(ds.text.strong.clone()));
                    if *changed {
                        ui.label(RichText::new(say(&ds, text::PRICE_CHANGED, &[])).color(ds.theme.warn));
                    }
                    ui.label(RichText::new(sentence).color(ds.theme.text));
                    for note in notes {
                        ui.label(RichText::new(note).font(ds.text.small.clone()).color(ds.theme.text2));
                    }
                    ui.add_space(space::S6);
                    ui.horizontal(|ui| {
                        let run_label = say(&ds, text::RUN, &[]);
                        go = ui.add(Button::new(&run_label).primary().op("connect.generate")).clicked();
                        let cancel_label = say(&ds, text::CANCEL, &[]);
                        cancel = ui.add(Button::new(&cancel_label).ghost()).clicked();
                    });
                });
                if cancel {
                    self.stage = Stage::Idle;
                } else if go && let Stage::Asking { connector, request, preview, .. } = std::mem::take(&mut self.stage)
                {
                    self.run(runner, actor, connector, request, &preview);
                }
            }
        }
        out
    }
}

/// The card's frame: a raised box with a warning edge.
fn card_frame(ui: &mut Ui, ds: &Ds, body: impl FnOnce(&mut Ui)) {
    egui::Frame::new()
        .fill(ds.theme.bg2)
        .stroke(egui::Stroke::new(1.0, ds.theme.warn))
        .corner_radius(6.0)
        .inner_margin(space::S12)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            body(ui);
        });
}

#[cfg(test)]
mod tests {
    use omuzaki_connect_api::{Capability, Estimate, Outbound};
    use omuzaki_store_api::Commercial;

    use super::*;

    fn preview(leaves: bool, usd_micros: u64) -> Preview {
        serde_json::from_value(json!({
            "connector": "mock-image-paid",
            "name": "Mock paid images",
            "capability": Capability::Image,
            "leaves_machine": leaves,
            "destinations": ["api.example.com"],
            "outbound": [Outbound { what: Sends::Images, count: 3, bytes: 900 }, Outbound { what: Sends::Prompt, count: 1, bytes: 20 }],
            "estimate": Estimate { usd_micros, exact: true, basis: "3 images x $0.04".into(), seconds: None },
            "retention": "30 days",
            "trains_on_inputs": "no",
            "commercial": Commercial::Ok,
            "licence": "LicenseRef-Vendor",
            "plan": null,
            "secret": null,
            "needs_spend": usd_micros > 0,
            "needs_person": null,
            "warnings": [],
        }))
        .unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn the_card_says_what_leaves_where_and_its_exact_price() {
        let ds = ds(&egui::Context::default());
        let CardState::Ask { sentence, notes, usd_micros } = card(&ds, &preview(true, 123_457)) else {
            panic!("a paid hosted run asks")
        };
        assert_eq!(usd_micros, 123_457);
        assert_eq!(sentence, "This sends 3 images and the prompt to api.example.com and costs $0.123457.");
        assert!(notes.iter().any(|n| n == "What they keep: 30 days"), "{notes:?}");
        assert!(notes.iter().any(|n| n == "They don't train on what you send."));
        let free = card(&ds, &preview(true, 0));
        assert!(matches!(free, CardState::Ask { ref sentence, .. } if sentence.ends_with("It costs nothing.")));
        assert_eq!(card(&ds, &preview(false, 0)), CardState::Local { line: "Stays on this computer".into() });
    }
}
