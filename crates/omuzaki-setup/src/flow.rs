//! The state machine (design section 3). It decides which step is showing and what the host must do next.

use crate::probe::{Cameras, IPhone, LanguageOffer, OutputKind, Permission, Probes, TrackerModel};
use crate::progress::{Progress, Step, Timers};
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
    /// The host holds a stored calibration result for this sitting; never sent by a caller's choice.
    CalibrationDone,
    /// The output card.
    ChooseOutput(OutputKind),
    /// Finish from Go-live.
    Finish,
}

/// What the host must do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Show this step and move focus to its heading.
    Show(Step),
    /// Ask the OS for the camera, with the reason on screen.
    RequestCameraPermission,
    /// Start the calibration overlay.
    StartCalibration,
    /// Show the test pattern on this output.
    ShowTestPattern(OutputKind),
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

/// How far the calibration got in this sitting (not persisted).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Calibration {
    Idle,
    Running,
    Done,
}

/// The first-run machine. Every state handles every event; one that makes no sense is ignored with no effects.
#[derive(Clone, Debug)]
pub struct SetupFlow {
    probes: Probes,
    step: Step,
    /// Steps skipped by the person; probe-made gaps are derived in [`Self::progress`].
    skipped: BTreeSet<Step>,
    finished: bool,
    input: Option<InputChoice>,
    output: Option<OutputKind>,
    calibration: Calibration,
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
            output: current.output,
            calibration: Calibration::Idle,
            permission_requested: false,
            timers: Timers::default(),
            last_ms: None,
            started_ms: None,
            persisted,
        };
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
            Event::Back => {
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
            Event::ChooseCharacter(c) => {
                if self.step == Step::Character && (c != CharacterChoice::Keep || self.probes.project_open) {
                    self.advance(&mut fx);
                }
            }
            Event::ChooseInput(c) => self.choose_input(c, &mut fx),
            Event::CalibrationDone => {
                if self.step == Step::Calibrate && self.calibration == Calibration::Running && self.calibrate_possible()
                {
                    self.calibration = Calibration::Done;
                    self.skipped.remove(&Step::Calibrate);
                    self.advance(&mut fx);
                }
            }
            Event::ChooseOutput(k) => {
                if self.step == Step::Output && self.probes.recommended_output == Some(k) {
                    self.skipped.remove(&Step::Output);
                    if self.output != Some(k) {
                        self.output = Some(k);
                        fx.push(Effect::ShowTestPattern(k));
                    }
                }
            }
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
                        && self.calibration != Calibration::Done)
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
            }
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
        let dialog = self.permission_requested && self.probes.cameras.permission() == Some(Permission::Undecided);
        // A preselected webcam on an earlier step is not a wait: the person is not held up by the model yet.
        let download = self.step >= Step::Input
            && self.input == Some(InputChoice::Webcam)
            && matches!(self.probes.tracker, TrackerModel::Downloading { .. });
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
        if self.step == Step::Calibrate && self.calibration == Calibration::Running {
            // The overlay's cancel sends no event, so leaving the step is what ends a run.
            self.calibration = Calibration::Idle;
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
            Step::Input if self.input_ready() => self.advance(fx),
            // A preselected webcam asks the OS on Continue, as choosing it would.
            Step::Input if self.input == Some(InputChoice::Webcam) => self.request_camera(fx),
            // Back to an already calibrated step: Continue keeps the stored result.
            Step::Calibrate if self.calibration == Calibration::Done => self.advance(fx),
            Step::Calibrate if self.calibrate_possible() && self.calibration == Calibration::Idle => {
                self.calibration = Calibration::Running;
                fx.push(Effect::StartCalibration);
            }
            Step::Output if self.output.is_some() => self.advance(fx),
            _ => {}
        }
    }

    /// A webcam choice waits for the OS to grant the camera, an iPhone for its pairing; the life layer needs nothing.
    fn input_ready(&self) -> bool {
        match self.input {
            None => false,
            Some(InputChoice::Webcam) => self.probes.cameras.permission() == Some(Permission::Granted),
            Some(InputChoice::Iphone) => self.probes.iphone == IPhone::Paired,
            Some(InputChoice::LifeLayerOnly) => true,
        }
    }

    fn skip(&mut self, fx: &mut Vec<Effect>) {
        match self.step {
            Step::Input => {
                self.input = Some(InputChoice::LifeLayerOnly);
                self.skipped.insert(Step::Input);
            }
            Step::Calibrate => {
                self.skipped.insert(Step::Calibrate);
            }
            Step::Output => {
                self.skipped.insert(Step::Output);
            }
            _ => return,
        }
        self.advance(fx);
    }

    fn choose_input(&mut self, choice: InputChoice, fx: &mut Vec<Effect>) {
        if self.step != Step::Input {
            return;
        }
        if choice == InputChoice::Webcam {
            match self.probes.cameras.permission() {
                None | Some(Permission::Denied) => return,
                Some(_) => self.request_camera(fx),
            }
        }
        self.skipped.remove(&Step::Input);
        if self.input != Some(choice) {
            // A calibration belongs to the source it ran on.
            self.calibration = Calibration::Idle;
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
