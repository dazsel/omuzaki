//! What the Setup panel asks of the studio: what the machine can do ([`Probes`]), and the [`Effect`]s the flow asks
//! for, carried out through the same ops the Live panel calls. [`LiveHost`] is the studio's; tests pass their own.

use omuzaki_ops_api::{Actor, Runner};
use omuzaki_setup::{
    CharacterChoice, Current, Effect, Event, IPhone, InputChoice, LanguageOffer, OutputKind, Permission, Probes,
    TrackerModel,
};
use omuzaki_stage::perform::Tracking;
use omuzaki_stage::perform::host::CalibrationState;
use omuzaki_stage::perform::input::{Permission as CameraPermission, SourceKind};

use crate::live::model::{Control, LiveModel};

/// What a step shows of the live studio: the camera, the face, the calibration and the output.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HostStatus {
    /// The camera the tracker reads, by the name the OS gives it.
    pub camera: Option<String>,
    /// The other cameras' names, for the picker (only when there is more than one).
    pub cameras: Vec<String>,
    /// Whether a face is found.
    pub tracking: Option<Tracking>,
    /// The calibration capture.
    pub calibration: Option<CalibrationState>,
    /// The output is on.
    pub output_on: bool,
    /// The model file imported on Character, by its file name.
    pub imported: Option<String>,
    /// The last op that failed, and why.
    pub failure: Option<(String, String)>,
}

/// The studio's side of the first-run flow.
pub trait SetupHost {
    /// Reads the studio again (cameras, tracking, calibration, the output).
    fn refresh(&mut self, runner: &dyn Runner, actor: &Actor);

    /// What the machine can do now.
    fn probes(&self) -> Probes;

    /// The settings as they are, before the flow starts.
    fn current(&self) -> Current;

    /// What the steps show.
    fn status(&self) -> HostStatus;

    /// Whether this build can carry out a Character card.
    ///
    /// # Errors
    /// Why it can't.
    fn character(&self, choice: CharacterChoice) -> Result<(), Unavailable>;

    /// Picks the camera at `index` of [`HostStatus::cameras`].
    fn pick_camera(&mut self, runner: &dyn Runner, actor: &Actor, index: usize);

    /// The file to import when the Import card's effect comes.
    fn stage_import(&mut self, path: String);

    /// Carries out `effect`. Answers known at once come back now; the rest come from [`poll`](Self::poll).
    fn apply(&mut self, runner: &dyn Runner, actor: &Actor, effect: &Effect) -> Vec<Event>;

    /// Answers that came later: a calibration run ending.
    fn poll(&mut self) -> Vec<Event>;
}

/// Why a Character card is off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unavailable {
    /// The ops behind it are not in this build.
    NotInBuild,
}

/// The studio's host: the Live panel's model over the running executor.
#[derive(Debug)]
pub struct LiveHost {
    model: LiveModel,
    /// A rerun: a project was set up before, so Character offers Keep.
    rerun: bool,
    /// The file the Import card brings in.
    import: Option<String>,
    /// The calibration run in flight: its request, and whether its capture was seen running.
    calibrating: Option<(u32, bool)>,
    /// The last op an effect ran that failed, and why: reads the model makes on its own are the Live panel's to show.
    failure: Option<(String, String)>,
}

impl LiveHost {
    /// A host over `runner`'s ops; `rerun` when setup ran here before.
    pub fn new(runner: &dyn Runner, rerun: bool) -> Self {
        Self::with_model(LiveModel::new(runner), rerun)
    }

    /// A host over `model` (tests give one with chosen ops).
    pub const fn with_model(model: LiveModel, rerun: bool) -> Self {
        Self { model, rerun, import: None, calibrating: None, failure: None }
    }

    /// The answer to `request`. A model call clears the model's failure when it succeeds and sets it when it fails, so
    /// `ran` (a model call was just made) asks the model too.
    fn answer(&self, request: u32, done: bool, ran: bool) -> Event {
        if done && !(ran && self.model.failure().is_some()) {
            Event::Applied { request }
        } else {
            Event::Refused { request }
        }
    }

    /// Carries out `effect` through the model; returns the answers known at once.
    fn carry_out(&mut self, runner: &dyn Runner, actor: &Actor, effect: &Effect) -> Vec<Event> {
        match *effect {
            Effect::RequestCameraPermission => {
                self.model.request_camera(runner, actor);
                Vec::new()
            }
            Effect::SelectInput { request, source } => {
                // The input ops answer only with the studio's input service; without it nothing tracks to stop.
                let live = self.model.ops().camera() == Control::Ready && self.model.input().is_some();
                let ran = live;
                let done = match source {
                    InputChoice::Webcam if live => {
                        let input = self.model.input().cloned();
                        if let Some(input) = input.filter(|i| i.camera.is_none()) {
                            let first = input.cameras.first().map(|c| c.id.0.clone());
                            self.model.bind_camera(runner, actor, first.as_deref());
                        }
                        self.model.set_tracking(runner, actor, true);
                        true
                    }
                    InputChoice::LifeLayerOnly => {
                        if live {
                            self.model.set_tracking(runner, actor, false);
                        }
                        true
                    }
                    // iPhone pairing is not in the studio yet; the flow notes it and offers the others.
                    InputChoice::Webcam | InputChoice::Iphone => false,
                };
                vec![self.answer(request, done, ran)]
            }
            Effect::StartCalibration { request } => {
                if self.model.ops().calibrate() != Control::Ready {
                    return vec![Event::CalibrationFailed { request }];
                }
                self.model.calibrate(runner, actor);
                if self.model.failure().is_some() {
                    return vec![Event::CalibrationFailed { request }];
                }
                self.calibrating = Some((request, false));
                Vec::new()
            }
            Effect::ShowTestPattern { request, output } => {
                let live = self.model.ops().output() == Control::Ready && self.model.window().is_some();
                let done = output == OutputKind::WindowCapture && live && {
                    self.model.set_window_open(runner, actor, true);
                    true
                };
                vec![self.answer(request, done, done)]
            }
            Effect::ApplyCharacter { request, choice } => {
                let ran = choice == CharacterChoice::ImportVrm;
                let done = match choice {
                    CharacterChoice::Sample => crate::studio::open_character(runner).is_ok(),
                    CharacterChoice::Keep => true,
                    CharacterChoice::ImportVrm => match self.import.take() {
                        Some(path) => {
                            self.model.import(runner, actor, &path);
                            self.model.imported().is_some()
                        }
                        None => false,
                    },
                    CharacterChoice::FromSheetOrIdea => false,
                };
                vec![self.answer(request, done, ran)]
            }
            // The studio opens the sample at start, so there is always a project; progress and the end are the
            // panel's.
            Effect::Show(_) | Effect::EnsureProject | Effect::Persist(_) | Effect::Finished { .. } => Vec::new(),
        }
    }
}

impl SetupHost for LiveHost {
    fn refresh(&mut self, runner: &dyn Runner, actor: &Actor) {
        self.model.refresh(runner, actor);
    }

    fn probes(&self) -> Probes {
        let ops = self.model.ops();
        let cameras = match self.model.input().filter(|_| ops.camera() == Control::Ready) {
            None => omuzaki_setup::Cameras::None,
            Some(input) => match (input.permission, input.cameras.len()) {
                (CameraPermission::Denied, _) => omuzaki_setup::Cameras::One(Permission::Denied),
                (_, 0) => omuzaki_setup::Cameras::None,
                (permission, 1) => omuzaki_setup::Cameras::One(match permission {
                    // A system that asks before a camera opens says Unknown until it has; one that never asks lists
                    // the camera, so it can be used.
                    CameraPermission::Unknown if ops.request_camera() == Control::Ready => Permission::Undecided,
                    _ => Permission::Granted,
                }),
                (_, n) => omuzaki_setup::Cameras::Several(u8::try_from(n).unwrap_or(u8::MAX)),
            },
        };
        Probes {
            cameras,
            // The tracker ships with the studio; the camera ops are the sign it is there.
            tracker: if ops.camera() == Control::Ready { TrackerModel::Ready } else { TrackerModel::Unavailable },
            iphone: IPhone::NotPaired,
            // The window ops answer only with the studio's window service: its status is the sign it is there.
            recommended_output: (ops.output() == Control::Ready && self.model.window().is_some())
                .then_some(OutputKind::WindowCapture),
            assistant: false,
            language: LanguageOffer::None,
            project_open: self.rerun,
            privacy_claim_true: true,
        }
    }

    fn current(&self) -> Current {
        let input = self.model.input().map(|input| match input.source {
            SourceKind::Webcam => InputChoice::Webcam,
            SourceKind::Iphone => InputChoice::Iphone,
            SourceKind::LifeLayer => InputChoice::LifeLayerOnly,
        });
        // A first run has chosen nothing: the life layer is where a fresh install starts, not a choice.
        let input = input.filter(|choice| self.rerun || *choice != InputChoice::LifeLayerOnly);
        Current { input, output: self.model.window_open().then_some(OutputKind::WindowCapture) }
    }

    fn status(&self) -> HostStatus {
        let input = self.model.input();
        let camera = input.and_then(|i| i.cameras.iter().find(|c| c.bound)).map(|c| c.name.clone());
        let cameras = input.map(|i| i.cameras.iter().map(|c| c.name.clone()).collect()).unwrap_or_default();
        HostStatus {
            camera,
            cameras,
            tracking: self.model.tracking(),
            calibration: self.model.calibration(),
            output_on: self.model.window_open(),
            imported: self.model.imported().map(str::to_owned),
            failure: self.failure.clone(),
        }
    }

    fn character(&self, choice: CharacterChoice) -> Result<(), Unavailable> {
        match choice {
            CharacterChoice::Sample | CharacterChoice::Keep => Ok(()),
            CharacterChoice::ImportVrm if self.model.ops().import() == Control::Ready => Ok(()),
            // The sheet flow (CB-90) is not in the studio yet.
            CharacterChoice::ImportVrm | CharacterChoice::FromSheetOrIdea => Err(Unavailable::NotInBuild),
        }
    }

    fn pick_camera(&mut self, runner: &dyn Runner, actor: &Actor, index: usize) {
        let id = self.model.input().and_then(|i| i.cameras.get(index)).map(|c| c.id.0.clone());
        if let Some(id) = id {
            self.model.bind_camera(runner, actor, Some(&id));
            self.model.set_tracking(runner, actor, true);
        }
    }

    fn stage_import(&mut self, path: String) {
        self.import = Some(path);
    }

    fn apply(&mut self, runner: &dyn Runner, actor: &Actor, effect: &Effect) -> Vec<Event> {
        let answers = self.carry_out(runner, actor, effect);
        if answers.iter().any(|a| matches!(a, Event::Refused { .. } | Event::CalibrationFailed { .. })) {
            self.failure = self.model.failure().map(|f| (f.op.to_owned(), f.message.clone()));
        } else if !answers.is_empty() {
            self.failure = None;
        }
        answers
    }

    fn poll(&mut self) -> Vec<Event> {
        let Some((request, seen)) = self.calibrating else { return Vec::new() };
        match self.model.calibration() {
            Some(CalibrationState::Capturing { .. }) => {
                self.calibrating = Some((request, true));
                Vec::new()
            }
            Some(CalibrationState::Done { .. }) if seen => {
                self.calibrating = None;
                vec![Event::CalibrationDone { request }]
            }
            Some(CalibrationState::Failed { .. }) if seen => {
                self.calibrating = None;
                vec![Event::CalibrationFailed { request }]
            }
            _ => Vec::new(),
        }
    }
}
