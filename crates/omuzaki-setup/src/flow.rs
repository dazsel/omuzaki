//! The state machine (design section 3). It decides which step is showing and what the host must do next.

use crate::probe::{Cameras, IPhone, LanguageOffer, OutputKind, Permission, Probes, TrackerModel};
use crate::progress::{Progress, Step, Timers};
use crate::request::{Ids, Op, Reply};
use std::collections::BTreeSet;

/// What the person picked on Character.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum CharacterChoice {
    /// The sample character.
    Sample,
    /// Create one from a sheet or an idea.
    FromSheetOrIdea,
    /// Import a VRM file.
    ImportVrm,
    /// Keep the open project (only when one is open).
    Keep,
}

/// The tracking source picked on Input.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum InputChoice {
    /// The webcam.
    Webcam,
    /// A paired iPhone.
    Iphone,
    /// No tracking; the avatar idles.
    LifeLayerOnly,
}

/// The host's settings as they are now, read before the flow starts. Setup never guesses them from the hardware: a
/// rerun keeps what is set, and a resume keeps what the person had chosen.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Current {
    /// The tracking source in use; `None` when none was ever chosen (a first run).
    pub input: Option<InputChoice>,
    /// The output in use; `None` when there is none.
    pub output: Option<OutputKind>,
}

/// What reaches the machine.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The host re-read the probes.
    Probed(Probes),
    /// Continue (on Calibrate: start the calibration).
    Next,
    /// Go to the previous shown step.
    Back,
    /// "Skip for now", "Use defaults" or "Later", by step.
    Skip,
    /// Leave setup from any step (Esc, after the panel's own cancels).
    SkipSetup,
    /// A Character card.
    ChooseCharacter(CharacterChoice),
    /// An Input card.
    ChooseInput(InputChoice),
    /// The host holds a stored calibration result from the run with this `request`; never sent by a caller's
    /// choice. A result for any other run (an earlier, abandoned one) is ignored.
    CalibrationDone {
        /// The `request` of the [`Effect::StartCalibration`] that ran.
        request: u32,
    },
    /// That calibration run was cancelled or failed. The step stays with a [`Note::CalibrationNotDone`]; Continue
    /// starts a new run and "Use defaults" is the other way on.
    CalibrationFailed {
        /// The `request` of the [`Effect::StartCalibration`] that ended.
        request: u32,
    },
    /// The output card.
    ChooseOutput(OutputKind),
    /// Finish from Go-live.
    Finish,
    /// The host carried out the operation of the effect with this `request` (an input selection, a character
    /// choice or an output with its test pattern). Only this lets the step move on or commit the choice, so a
    /// queued Confirm leaves the step as it is.
    Applied {
        /// The `request` of the effect that is answered.
        request: u32,
    },
    /// The host could not carry out that request (the person declined it, or the op failed). The step stays with
    /// a note and the choice can be made again; the same `request` is never answered twice.
    Refused {
        /// The `request` of the effect that is answered.
        request: u32,
    },
}

/// What the host must do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Show this step and move focus to its heading.
    Show(Step),
    /// Ask the OS for the camera, with the reason on screen.
    RequestCameraPermission,
    /// Apply this tracking source (`input.select`, which may wait for the person's approval). Sent for a choice, a
    /// skip that must drop the applied source, or a source only preselected when Continue is pressed; never for a
    /// source already in the host's settings. At most one request is outstanding: a later choice waits for the
    /// answer to this one, then the latest choice is requested. The host answers each `request` exactly once with
    /// [`Event::Applied`] or [`Event::Refused`].
    SelectInput {
        /// Numbers this request; the answer carries it back.
        request: u32,
        /// The source to apply.
        source: InputChoice,
    },
    /// Start the calibration overlay for this run; the host answers with [`Event::CalibrationDone`] or
    /// [`Event::CalibrationFailed`] carrying `request`.
    StartCalibration {
        /// Numbers this run.
        request: u32,
    },
    /// Add this output and show the test pattern on it. The host answers with [`Event::Applied`] or
    /// [`Event::Refused`]; the output counts as chosen only after the former.
    ShowTestPattern {
        /// Numbers this request.
        request: u32,
        /// The output to add.
        output: OutputKind,
    },
    /// Carry out this Character choice (create the sample project, start the sheet flow, import the file). The host
    /// answers with [`Event::Applied`] or [`Event::Refused`]; Character moves on only after the former. `Keep`
    /// writes nothing and sends no request.
    ApplyCharacter {
        /// Numbers this request.
        request: u32,
        /// The card chosen.
        choice: CharacterChoice,
    },
    /// Create the sample project and `main` scene if none is open.
    EnsureProject,
    /// Write this progress through to the session file.
    Persist(Progress),
    /// Setup ended.
    Finished {
        /// True when it ended through [`Event::SkipSetup`].
        skipped: bool,
        /// Wall time from the first event of this sitting, by the host's clock.
        total_ms: u64,
    },
}

/// A line the panel shows beside the step's controls.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Note {
    /// No camera: iPhone and life layer only.
    NoCamera,
    /// The OS refused the camera; the person fixes it in the OS settings.
    PermissionDenied,
    /// The tracker model is downloading, 0 to 100.
    TrackerDownloading(u8),
    /// The tracker model is not available.
    TrackerUnavailable,
    /// The iPhone source is chosen but no iPhone is paired.
    IphoneNotPaired,
    /// The host did not apply the chosen source; Continue tries again.
    InputNotApplied,
    /// The host did not add the chosen output; choosing it again tries again.
    OutputNotApplied,
    /// The host did not carry out the Character choice; choose again.
    CharacterNotApplied,
    /// The calibration was cancelled or failed; Continue starts it again, "Use defaults" moves on.
    CalibrationNotDone,
    /// The OS no longer offers an output, so the step stays only to say so; "Later" moves on.
    OutputUnavailable,
    /// No assistant is configured.
    NoAssistant,
    /// Offer Simplified Chinese.
    OfferSimplifiedChinese,
}

/// What the panel draws.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    /// The current step.
    pub step: Step,
    /// "n of m" over the shown steps after Welcome; `None` on Welcome.
    pub position: Option<(u8, u8)>,
    /// The steps in the list, always containing `step`.
    pub shown: Vec<Step>,
    /// Back is possible.
    pub can_back: bool,
    /// Input shows a camera picker.
    pub camera_picker: bool,
    /// The tracking source that is selected (preselected on Input), `None` before one is.
    pub input: Option<InputChoice>,
    /// The output that is selected, `None` before one is.
    pub output: Option<OutputKind>,
    /// Notes for this step.
    pub notes: Vec<Note>,
    /// Active time per step in this sitting, in [`Step::ALL`] order.
    pub elapsed_ms: [u64; 6],
}

/// What the person asked for on Input that waits for the host to apply the source.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Then {
    /// Continue.
    Continue,
    /// "Skip for now" (the life layer).
    Skip,
}

/// The first-run machine. Every state handles every event; one that makes no sense is ignored with no effects.
#[derive(Clone, Debug)]
pub struct SetupFlow {
    probes: Probes,
    step: Step,
    /// Steps skipped by the person; probe-made gaps are derived in [`Self::progress`].
    skipped: BTreeSet<Step>,
    finished: bool,
    /// The source the person selected (preselected on a first run until they choose).
    input: Option<InputChoice>,
    /// The host's side of `input.select`.
    input_op: Op<InputChoice>,
    ids: Ids,
    /// Move on when the host has applied the selected source.
    then: Option<Then>,
    /// The output the person selected.
    output: Option<OutputKind>,
    /// The host's side of adding the output with its test pattern.
    output_op: Op<OutputKind>,
    /// Continue was pressed on Output before the host answered.
    output_then: Option<Then>,
    /// The card the person chose on Character.
    character: Option<CharacterChoice>,
    /// The host's side of carrying that card out.
    character_op: Op<CharacterChoice>,
    /// The person asked for a calibration run (Continue on Calibrate) and has not given up on it.
    calibration_wanted: bool,
    /// The host's side of the calibration: a stored result of this sitting is "applied", a run is "in flight".
    calibration_op: Op<()>,
    permission_requested: bool,
    timers: Timers,
    last_ms: Option<u64>,
    started_ms: Option<u64>,
    persisted: Progress,
}

impl SetupFlow {
    /// Starts the flow, resuming at `resume` with the host's `current` settings. A resumed step the probes leave out
    /// moves forward to the next shown one; that move is not yet saved, so the first event persists it.
    pub fn new(probes: Probes, resume: Progress, current: Current) -> Self {
        let persisted = resume.clone();
        let Progress { step, skipped, finished } = resume;
        let skipped: BTreeSet<Step> =
            skipped.into_iter().filter(|s| matches!(s, Step::Input | Step::Calibrate | Step::Output)).collect();
        // The settings win; only a first visit to Input preselects the webcam when there is a camera to use.
        let skipped_input = skipped.contains(&Step::Input);
        let input = match current.input {
            Some(c) => Some(c),
            None if skipped.contains(&Step::Input) => Some(InputChoice::LifeLayerOnly),
            None if step <= Step::Input && probes.cameras.webcam_possible() => Some(InputChoice::Webcam),
            None => None,
        };
        let mut flow = SetupFlow {
            probes,
            step,
            skipped,
            finished,
            input,
            input_op: Op::new(current.input.or(skipped_input.then_some(InputChoice::LifeLayerOnly))),
            ids: Ids::default(),
            then: None,
            output: current.output,
            output_op: Op::new(current.output),
            output_then: None,
            character: None,
            character_op: Op::new(None),
            calibration_wanted: false,
            calibration_op: Op::new(None),
            permission_requested: false,
            timers: Timers::default(),
            last_ms: None,
            started_ms: None,
            persisted,
        };
        if flow.input.is_none() && flow.step > Step::Input {
            // Past Input without a saved source: the choice was lost, so ask for it again.
            flow.step = Step::Input;
        }
        if !flow.is_shown(flow.step) {
            flow.step = flow.next_shown().unwrap_or(Step::GoLive);
        }
        flow
    }

    /// Applies one event at the host's monotonic time and returns what the host must do.
    pub fn handle(&mut self, event: Event, now_ms: u64) -> Vec<Effect> {
        self.tick(now_ms);
        let mut fx = Vec::new();
        if self.finished {
            if let Event::Probed(p) = event {
                self.probes = p;
            }
            return fx;
        }
        match event {
            Event::Probed(p) => {
                self.probes = p;
                if p.cameras.permission() != Some(Permission::Undecided) {
                    self.permission_requested = false;
                }
            }
            Event::Next => self.next(&mut fx),
            Event::Applied { request } => self.answered(request, true, &mut fx),
            Event::Refused { request } => self.answered(request, false, &mut fx),
            Event::Back => {
                self.then = None;
                self.output_then = None;
                if let Some(prev) = self.prev_shown() {
                    self.go(prev, &mut fx);
                }
            }
            Event::Skip => self.skip(&mut fx),
            Event::SkipSetup => {
                fx.push(Effect::EnsureProject);
                self.end(true, &mut fx);
                return fx;
            }
            Event::ChooseCharacter(c) => self.choose_character(c, &mut fx),
            Event::ChooseInput(c) => self.choose_input(c, &mut fx),
            Event::CalibrationDone { request } => self.answered_calibration(request, true, &mut fx),
            Event::CalibrationFailed { request } => self.answered_calibration(request, false, &mut fx),
            Event::ChooseOutput(k) => self.choose_output(k, &mut fx),
            Event::Finish => {
                if self.step == Step::GoLive {
                    self.end(false, &mut fx);
                    return fx;
                }
            }
        }
        self.sync(&mut fx);
        fx
    }

    /// What the panel draws now.
    pub fn view(&self) -> View {
        let shown = self.shown();
        let position = (self.step != Step::Welcome).then(|| {
            let later = shown.iter().filter(|s| **s != Step::Welcome);
            let at = later.clone().position(|s| *s == self.step).map_or(0, |i| i + 1);
            let small = |n: usize| u8::try_from(n).unwrap_or(u8::MAX);
            (small(at), small(later.count()))
        });
        View {
            step: self.step,
            position,
            shown,
            can_back: self.prev_shown().is_some(),
            camera_picker: self.step == Step::Input && self.probes.cameras.needs_picker(),
            input: self.input,
            output: self.output,
            notes: self.notes(),
            elapsed_ms: self.timers.active_by_step(),
        }
    }

    /// Time per step in this sitting.
    pub fn timers(&self) -> &Timers {
        &self.timers
    }

    /// What would be saved now.
    pub fn progress(&self) -> Progress {
        let skipped = Step::ALL
            .into_iter()
            .filter(|s| {
                self.skipped.contains(s)
                    || (*s == Step::Output && !self.is_shown(Step::Output))
                    || (*s == Step::Calibrate
                        && !(self.input.is_none() && self.source_choosable())
                        && !self.calibrate_possible()
                        && self.calibration_op.applied().is_none())
            })
            .collect();
        Progress { step: self.step, skipped, finished: self.finished }
    }

    fn notes(&self) -> Vec<Note> {
        let mut notes = Vec::new();
        let p = &self.probes;
        match self.step {
            Step::Welcome => {
                if !p.assistant {
                    notes.push(Note::NoAssistant);
                }
                if p.language == LanguageOffer::SimplifiedChineseOnTraditionalSystem {
                    notes.push(Note::OfferSimplifiedChinese);
                }
            }
            Step::Input => {
                notes.extend(self.permission_note());
                let webcam_in_play = p.cameras != Cameras::None
                    && !matches!(self.input, Some(InputChoice::Iphone | InputChoice::LifeLayerOnly));
                if webcam_in_play {
                    notes.extend(self.tracker_note());
                }
                if self.input == Some(InputChoice::Iphone) && p.iphone != IPhone::Paired {
                    notes.push(Note::IphoneNotPaired);
                }
                if self.input_op.failed(self.input) {
                    notes.push(Note::InputNotApplied);
                }
            }
            Step::Character if self.character_op.failed(self.character) => notes.push(Note::CharacterNotApplied),
            Step::Output if self.output_op.failed(self.output) => {
                notes.push(Note::OutputNotApplied);
                if p.recommended_output.is_none() {
                    notes.push(Note::OutputUnavailable);
                }
            }
            // Likewise for Output when the recommendation went away while it is showing.
            Step::Output if p.recommended_output.is_none() => notes.push(Note::OutputUnavailable),
            // The step stays when the probes just made it pointless (no jump under the person); say why.
            Step::Calibrate if !self.calibrate_possible() => match self.input {
                Some(InputChoice::Webcam) => {
                    notes.extend(self.permission_note().or_else(|| self.tracker_note()));
                }
                Some(InputChoice::Iphone) => notes.push(Note::IphoneNotPaired),
                _ => {}
            },
            _ => {}
        }
        if self.step == Step::Calibrate && self.calibration_op.failed(self.calibration_want()) {
            notes.push(Note::CalibrationNotDone);
        }
        notes
    }

    /// The camera note: none there, or refused by the OS.
    fn permission_note(&self) -> Option<Note> {
        match self.probes.cameras.permission() {
            None => Some(Note::NoCamera),
            Some(Permission::Denied) => Some(Note::PermissionDenied),
            Some(_) => None,
        }
    }

    /// The webcam model note: downloading or unavailable.
    fn tracker_note(&self) -> Option<Note> {
        match self.probes.tracker {
            TrackerModel::Downloading { percent } => Some(Note::TrackerDownloading(percent.min(100))),
            TrackerModel::Unavailable => Some(Note::TrackerUnavailable),
            TrackerModel::Ready => None,
        }
    }

    fn tick(&mut self, now_ms: u64) {
        let last = self.last_ms.unwrap_or(now_ms);
        self.started_ms.get_or_insert(now_ms);
        self.last_ms = Some(now_ms.max(last));
        if !self.finished {
            self.timers.add(self.step, now_ms.saturating_sub(last), self.waiting());
        }
    }

    /// Waiting on the OS camera dialog or the webcam model, as opposed to the person.
    fn waiting(&self) -> bool {
        // Only while the webcam is the source and its step is showing: a source switched away from, or a later
        // step, is the person's time.
        let on_webcam = self.input == Some(InputChoice::Webcam) && matches!(self.step, Step::Input | Step::Calibrate);
        let dialog =
            on_webcam && self.permission_requested && self.probes.cameras.permission() == Some(Permission::Undecided);
        let download = on_webcam && matches!(self.probes.tracker, TrackerModel::Downloading { .. });
        dialog || download
    }

    fn is_shown(&self, step: Step) -> bool {
        match step {
            Step::Output => self.probes.recommended_output.is_some(),
            Step::Calibrate => self.calibrate_possible(),
            _ => true,
        }
    }

    /// Calibration can run on a webcam with a ready model, or on a paired iPhone whatever the webcam model is.
    fn calibrate_possible(&self) -> bool {
        let webcam = self.probes.cameras.webcam_possible() && self.probes.tracker == TrackerModel::Ready;
        let phone = self.probes.iphone == IPhone::Paired;
        match self.input {
            None => webcam || phone,
            Some(InputChoice::Webcam) => webcam,
            Some(InputChoice::Iphone) => phone,
            Some(InputChoice::LifeLayerOnly) => false,
        }
    }

    /// Some source could still be picked: a camera that works or may be allowed, or a paired iPhone.
    fn source_choosable(&self) -> bool {
        self.probes.cameras.webcam_possible() || self.probes.iphone == IPhone::Paired
    }

    /// The listed steps; the current one stays even when the probes just made it pointless (no jump under the person).
    fn shown(&self) -> Vec<Step> {
        Step::ALL.into_iter().filter(|s| *s == self.step || self.is_shown(*s)).collect()
    }

    fn next_shown(&self) -> Option<Step> {
        Step::ALL.into_iter().find(|s| *s > self.step && self.is_shown(*s))
    }

    fn prev_shown(&self) -> Option<Step> {
        Step::ALL.into_iter().rev().find(|s| *s < self.step && self.is_shown(*s))
    }

    /// Moves to `step`: persist first, then show, so a crash right after showing resumes at the shown step.
    fn go(&mut self, step: Step, fx: &mut Vec<Effect>) {
        if self.step == Step::Calibrate {
            // The overlay's cancel sends no event, so leaving the step is what ends a run.
            self.calibration_wanted = false;
            self.calibration_op.cancel();
        }
        self.step = step;
        self.sync(fx);
        fx.push(Effect::Show(step));
    }

    fn advance(&mut self, fx: &mut Vec<Effect>) {
        if let Some(next) = self.next_shown() {
            self.go(next, fx);
        }
    }

    fn next(&mut self, fx: &mut Vec<Effect>) {
        match self.step {
            Step::Welcome => self.advance(fx),
            Step::Input => self.continue_input(fx),
            Step::Calibrate => self.continue_calibrate(fx),
            Step::Output => self.continue_output(fx),
            _ => {}
        }
    }

    /// Numbers and sends what the person wants, for each kind of operation. Nothing is sent while a request of
    /// the kind is outstanding or the host already holds the wish; the answer to the outstanding one follows up.
    fn pump_input(&mut self, fx: &mut Vec<Effect>) {
        if let Some((request, source)) = self.input_op.ask(self.input, &mut self.ids) {
            fx.push(Effect::SelectInput { request, source });
        }
    }

    fn pump_output(&mut self, fx: &mut Vec<Effect>) {
        if let Some((request, output)) = self.output_op.ask(self.output, &mut self.ids) {
            fx.push(Effect::ShowTestPattern { request, output });
        }
    }

    fn pump_character(&mut self, fx: &mut Vec<Effect>) {
        if let Some((request, choice)) = self.character_op.ask(self.character, &mut self.ids) {
            fx.push(Effect::ApplyCharacter { request, choice });
        }
    }

    fn pump_calibration(&mut self, fx: &mut Vec<Effect>) {
        if let Some((request, ())) = self.calibration_op.ask(self.calibration_want(), &mut self.ids) {
            fx.push(Effect::StartCalibration { request });
        }
    }

    fn calibration_want(&self) -> Option<()> {
        self.calibration_wanted.then_some(())
    }

    /// The host answered a request. Stale or unknown answers change nothing.
    fn answered(&mut self, request: u32, ok: bool, fx: &mut Vec<Effect>) {
        if self.input_op.owns(request) {
            let reply = self.input_op.reply(ok, self.input);
            if reply == Some(Reply::Failed) {
                self.then = None;
            } else if self.input_op.settled(self.input) {
                self.finish_input(fx);
            } else {
                self.pump_input(fx);
            }
        } else if self.output_op.owns(request) {
            let reply = self.output_op.reply(ok, self.output);
            if reply == Some(Reply::Failed) {
                self.output_then = None;
            } else {
                if reply == Some(Reply::Applied) {
                    self.skipped.remove(&Step::Output);
                }
                if self.output_op.settled(self.output) {
                    if self.output_then.take().is_some() && self.step == Step::Output {
                        self.advance(fx);
                    }
                } else {
                    self.pump_output(fx);
                }
            }
        } else if self.character_op.owns(request) {
            let reply = self.character_op.reply(ok, self.character);
            if reply == Some(Reply::Failed) {
                return;
            }
            if reply == Some(Reply::Applied) {
                self.project_changed();
            }
            if !self.character_op.settled(self.character) {
                self.pump_character(fx);
            } else if self.step == Step::Character {
                self.advance(fx);
            }
        }
    }

    /// The host reports the calibration run `request`. Probes gate STARTING a run; a stored result of the active
    /// run counts whatever the probes say now.
    fn answered_calibration(&mut self, request: u32, ok: bool, fx: &mut Vec<Effect>) {
        if !self.calibration_op.owns(request) {
            return;
        }
        if self.calibration_op.reply(ok, self.calibration_want()) == Some(Reply::Failed) {
            self.calibration_wanted = false;
        } else if ok {
            self.calibration_wanted = false;
            self.skipped.remove(&Step::Calibrate);
            if self.step == Step::Calibrate {
                self.advance(fx);
            }
        }
    }

    /// A character card that made another project was carried out. Everything the old project held is gone with
    /// it: its output is not the new project's, so Output starts over.
    fn project_changed(&mut self) {
        self.output = None;
        self.output_op.forget();
        self.output_then = None;
        self.skipped.remove(&Step::Output);
    }

    /// Continue on Calibrate: a stored result moves on; otherwise a run is started if the probes allow one.
    fn continue_calibrate(&mut self, fx: &mut Vec<Effect>) {
        if self.calibration_op.applied().is_some() {
            self.advance(fx);
        } else if self.input.is_some() && self.calibrate_possible() {
            self.calibration_wanted = true;
            self.pump_calibration(fx);
        }
    }

    /// Continue on Input. A source that is only drawn as selected (or was refused) is sent to the host first, as
    /// choosing it would, and the step stays until the host reports it applied.
    fn continue_input(&mut self, fx: &mut Vec<Effect>) {
        let Some(choice) = self.input else { return };
        if choice == InputChoice::Webcam && matches!(self.probes.cameras.permission(), None | Some(Permission::Denied))
        {
            return;
        }
        if choice == InputChoice::Webcam {
            self.request_camera(fx);
        }
        if self.input_op.settled(self.input) {
            if self.input_ready() {
                self.advance(fx);
            }
        } else {
            self.then = Some(Then::Continue);
            self.pump_input(fx);
        }
    }

    /// Carries out what was waiting for the source to be applied, if the person is still on Input.
    fn finish_input(&mut self, fx: &mut Vec<Effect>) {
        let then = self.then.take();
        if self.step != Step::Input {
            return;
        }
        match then {
            Some(Then::Continue) if self.input_ready() => self.advance(fx),
            Some(Then::Skip) => {
                self.skipped.insert(Step::Input);
                self.advance(fx);
            }
            _ => {}
        }
    }

    /// A webcam choice waits for the OS to grant the camera, an iPhone for its pairing, and any source for the host to have applied it.
    fn input_ready(&self) -> bool {
        let source_ready = match self.input {
            None => false,
            Some(InputChoice::Webcam) => self.probes.cameras.permission() == Some(Permission::Granted),
            Some(InputChoice::Iphone) => self.probes.iphone == IPhone::Paired,
            Some(InputChoice::LifeLayerOnly) => true,
        };
        source_ready && self.input_op.settled(self.input)
    }

    fn choose_character(&mut self, choice: CharacterChoice, fx: &mut Vec<Effect>) {
        if self.step != Step::Character {
            return;
        }
        match choice {
            CharacterChoice::Keep if !self.probes.project_open => {}
            // Keeping the open project writes nothing, so there is nothing to wait for.
            CharacterChoice::Keep => self.advance(fx),
            _ => {
                self.character = Some(choice);
                self.pump_character(fx);
                if self.character_op.settled(self.character) {
                    self.advance(fx);
                }
            }
        }
    }

    fn choose_output(&mut self, kind: OutputKind, fx: &mut Vec<Effect>) {
        if self.step != Step::Output || self.probes.recommended_output != Some(kind) {
            return;
        }
        self.output = Some(kind);
        self.pump_output(fx);
        if self.output_op.settled(self.output) {
            self.skipped.remove(&Step::Output);
        }
    }

    /// Continue on Output: only an output the host has added moves on; otherwise ask and wait.
    fn continue_output(&mut self, fx: &mut Vec<Effect>) {
        if self.output.is_none() {
            return;
        }
        if self.output_op.settled(self.output) {
            self.skipped.remove(&Step::Output);
            self.advance(fx);
        } else {
            self.output_then = Some(Then::Continue);
            self.pump_output(fx);
        }
    }

    fn skip(&mut self, fx: &mut Vec<Effect>) {
        match self.step {
            Step::Input => {
                // "Skip for now" is the life layer chosen like any source: saved through the host, and the step waits.
                self.set_input(InputChoice::LifeLayerOnly);
                self.then = Some(Then::Skip);
                if self.input_op.settled(self.input) {
                    self.finish_input(fx);
                } else {
                    self.pump_input(fx);
                }
                return;
            }
            Step::Calibrate => {
                // "Use defaults" gives up any stored result of this sitting too.
                self.calibration_wanted = false;
                self.calibration_op.forget();
                self.skipped.insert(Step::Calibrate);
            }
            Step::Output => {
                self.output_then = None;
                // "Later" defers an output; one the host already holds has nothing left to defer.
                if !self.output_op.settled(self.output) {
                    self.skipped.insert(Step::Output);
                }
            }
            _ => return,
        }
        self.advance(fx);
    }

    fn choose_input(&mut self, choice: InputChoice, fx: &mut Vec<Effect>) {
        if self.step != Step::Input {
            return;
        }
        if choice == InputChoice::Webcam && matches!(self.probes.cameras.permission(), None | Some(Permission::Denied))
        {
            return;
        }
        if choice == InputChoice::Webcam {
            // Clicking the webcam starts the OS dialog whether or not it is already the selected source.
            self.request_camera(fx);
        }
        self.skipped.remove(&Step::Input);
        self.set_input(choice);
        self.pump_input(fx);
    }

    /// Every change of source goes through here: a calibration belongs to the source it ran on.
    fn set_input(&mut self, choice: InputChoice) {
        if self.input != Some(choice) {
            self.calibration_wanted = false;
            self.calibration_op.forget();
            self.then = None;
            if choice != InputChoice::Webcam {
                // A pending camera dialog belongs to the webcam source.
                self.permission_requested = false;
            }
        }
        self.input = Some(choice);
    }

    /// Asks the OS for the camera once, when it has not been asked.
    fn request_camera(&mut self, fx: &mut Vec<Effect>) {
        if self.probes.cameras.permission() == Some(Permission::Undecided) && !self.permission_requested {
            self.permission_requested = true;
            fx.push(Effect::RequestCameraPermission);
        }
    }

    /// Ends setup: persist first so a crash right after still counts as finished.
    fn end(&mut self, skipped: bool, fx: &mut Vec<Effect>) {
        self.finished = true;
        self.sync(fx);
        let total_ms = self.last_ms.unwrap_or(0).saturating_sub(self.started_ms.unwrap_or(0));
        fx.push(Effect::Finished { skipped, total_ms });
    }

    /// Persists when the saved fields changed, so every restart-worthy change is written and nothing else is.
    fn sync(&mut self, fx: &mut Vec<Effect>) {
        let progress = self.progress();
        if progress != self.persisted {
            self.persisted = progress.clone();
            fx.push(Effect::Persist(progress));
        }
    }
}

#[cfg(test)]
mod invariants;
