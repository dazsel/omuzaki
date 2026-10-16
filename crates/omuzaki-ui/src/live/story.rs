//! The Live workspace's gallery story, and the [`StoryRunner`] that lets it (and the evidence pictures) show the
//! workspace in each state without a performer.

use std::sync::mpsc::Receiver;

use egui::Ui;
use omuzaki_anim::TriggerMode;
use omuzaki_anim_api::RetargetConfig;
use omuzaki_core::glam::UVec2;
use omuzaki_doc_api::{DocPath, Value};
use omuzaki_face_api::ExpressionId;
use omuzaki_ops_api::checks::op_info;
use omuzaki_ops_api::{Actor, Event, Grants, OpError, OpInfo, OpRecord, Outcome, Plan, Request, Runner, TxId, find};
use omuzaki_stage::perform::Tracking;
use omuzaki_stage::perform::host::{CalibrationState, PerformStatus};
use omuzaki_stage::perform::triggers::{TriggerBinding, TriggerInfo};
use omuzaki_stream_api::window::{Backdrop, DEFAULT_KEY, WindowState};

use super::input::{self, Camera, InputView, Permission, Preview, Source};
use super::model::{
    LiveModel, OP_CALIBRATE, OP_SET_RETARGET, OP_TRIGGER_BIND, OP_TRIGGER_LIST, OP_TRIGGER_UNBIND, OP_WINDOW_CLOSE,
    OP_WINDOW_OPEN, OP_WINDOW_STATUS,
};
use super::panel::LivePanel;
use crate::ds::gallery::Group;
use crate::register_story;

/// The states the story and the evidence show.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LiveState {
    /// The app just opened: no frames yet, not calibrated.
    Idle,
    /// A calibration is running over a tracked face.
    Calibrating,
    /// The face was lost; the life layer has it.
    TrackingLost,
}

impl LiveState {
    /// Every state, in the order the evidence shows them.
    pub const ALL: [Self; 3] = [Self::Idle, Self::Calibrating, Self::TrackingLost];

    /// The state's file-name slug (`tracking-lost`).
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Calibrating => "calibrating",
            Self::TrackingLost => "tracking-lost",
        }
    }

    /// What `perform.status` answers in this state.
    pub fn status(self) -> PerformStatus {
        let (tracking, calibration, source_fps) = match self {
            Self::Idle => (Tracking::NoFrames, CalibrationState::Idle, 0.0),
            Self::Calibrating => (Tracking::Tracking, CalibrationState::Capturing { frames: 112, of: 300 }, 30.0),
            Self::TrackingLost => (Tracking::Lost, CalibrationState::Done { frames: 300 }, 29.0),
        };
        let retarget = RetargetConfig { smoothing: 0.35, ..RetargetConfig::default() };
        PerformStatus { tracking, calibration, retarget, retarget_seq: 1, source_fps }
    }

    /// What `trigger.list` answers in this state: `F1` on Happy and `F2` on Surprised, with Happy showing while the
    /// face is tracked.
    pub fn triggers(self) -> Vec<TriggerInfo> {
        let info = |key: &str, expression: &str, active: bool| TriggerInfo {
            binding: TriggerBinding {
                key: key.to_owned(),
                expression: ExpressionId::new(expression).unwrap_or_else(|_| unreachable!("a starter id")),
                mode: TriggerMode::Toggle,
            },
            active,
            missing: false,
        };
        vec![info("F1", "happy", self == Self::Calibrating), info("F2", "surprised", false)]
    }

    /// What `input.state` answers in this state: two cameras; none bound when idle, the first tracking while
    /// calibrating, and the first unplugged once the face is lost.
    pub fn input(self) -> InputView {
        let camera = |id: &str, name: &str, bound: bool| Camera { id: id.to_owned(), name: name.to_owned(), bound };
        let bound = self != Self::Idle;
        // Device names are the OS's, shown verbatim; capitals keep them out of the pseudo-locale's plain-English check.
        let cameras = vec![camera("usb:0", "C920 PRO HD", bound), camera("usb:1", "HD WEBCAM", false)];
        let (source, preview, present) = match self {
            Self::Idle => (Source::LifeLayer, Preview::Off, false),
            Self::Calibrating => (Source::Webcam, Preview::On { camera: "usb:0".to_owned() }, true),
            Self::TrackingLost => (Source::Webcam, Preview::On { camera: "usb:0".to_owned() }, false),
        };
        // An unplugged camera is no longer listed.
        let cameras = if present || !bound { cameras } else { cameras.into_iter().skip(1).collect() };
        InputView {
            source,
            camera: bound.then(|| "usb:0".to_owned()),
            camera_present: present,
            preview,
            permission: Permission::Granted,
            cameras,
            tracking: self.status().tracking,
        }
    }

    /// What `output.window.status` answers in this state: closed when idle, open over the key otherwise, and
    /// minimised once the face is lost (so its warning shows).
    pub fn window(self) -> WindowState {
        let open = WindowState::open(UVec2::new(1920, 1080), Backdrop::Key(DEFAULT_KEY));
        match self {
            Self::Idle => WindowState::Closed,
            Self::Calibrating => open,
            Self::TrackingLost => open.with_minimised(true),
        }
    }

    /// The workspace on `runner`, showing this state.
    pub fn panel(self, runner: &dyn Runner) -> LivePanel {
        let mut model = LiveModel::new(runner);
        model.set_status(self.status());
        model.set_triggers(self.triggers());
        LivePanel::with_model(model)
    }
}

/// A runner with no document that offers `perform.calibrate`, `perform.set_retarget`, the trigger ops, the output
/// window's ops and the `input.*` surface, and accepts the actions without doing anything; `trigger.list`,
/// `input.state` and `output.window.status` answer with its state's ([`LiveState`]). It has no `perform.status`, so
/// the workspace shows whatever status its model is given ([`LiveModel::set_status`]): the gallery and the evidence
/// pictures pick a [`LiveState`] that way.
#[derive(Clone, Copy, Debug)]
pub struct StoryRunner(pub LiveState);

impl Default for StoryRunner {
    fn default() -> Self {
        Self(LiveState::Calibrating)
    }
}

impl Runner for StoryRunner {
    fn ops(&self) -> Vec<OpInfo> {
        let registered = [
            OP_CALIBRATE,
            OP_SET_RETARGET,
            OP_TRIGGER_BIND,
            OP_TRIGGER_UNBIND,
            OP_TRIGGER_LIST,
            OP_WINDOW_OPEN,
            OP_WINDOW_CLOSE,
            OP_WINDOW_STATUS,
        ];
        registered.into_iter().filter_map(find).map(op_info).chain(input::op_infos()).collect()
    }

    fn run(&self, req: Request) -> Result<Outcome, OpError> {
        let answer = match req.op.as_str() {
            OP_TRIGGER_LIST => serde_json::json!({ "triggers": to_value(self.0.triggers())? }),
            input::OP_STATE => to_value(self.0.input())?,
            OP_WINDOW_STATUS => to_value(self.0.window())?,
            _ => return Ok(Outcome::new(Plan::default())),
        };
        Ok(Outcome::new(Plan::answer(answer)))
    }

    fn begin(&self, _label: &str, _actor: &Actor) -> TxId {
        TxId(0)
    }

    fn commit(&self, _tx: TxId, _actor: &Actor) -> Result<(), OpError> {
        Err(OpError::not_found("no open transaction"))
    }

    fn rollback(&self, _tx: TxId, _actor: &Actor) -> Result<(), OpError> {
        Err(OpError::not_found("no open transaction"))
    }

    fn undo(&self, _actor: &Actor, _grants: &Grants) -> Result<Option<String>, OpError> {
        Ok(None)
    }

    fn redo(&self, _actor: &Actor, _grants: &Grants) -> Result<Option<String>, OpError> {
        Ok(None)
    }

    fn subscribe(&self, _grants: &Grants) -> Result<Receiver<Event>, OpError> {
        // The sender drops at once: an empty stream that ends.
        Ok(std::sync::mpsc::channel().1)
    }

    fn get(&self, _path: &DocPath, _grants: &Grants) -> Result<Option<Value>, OpError> {
        Ok(None)
    }

    fn log(&self, _after_seq: u64, _limit: usize, _grants: &Grants) -> Result<Vec<OpRecord>, OpError> {
        Ok(Vec::new())
    }
}

fn to_value(value: impl serde::Serialize) -> Result<Value, OpError> {
    serde_json::to_value(value).map_err(|e| OpError::internal(e.to_string()))
}

/// The story's state: the panel and the runner it runs on.
pub struct StoryState {
    runner: StoryRunner,
    panel: LivePanel,
    actor: Actor,
}

impl Default for StoryState {
    fn default() -> Self {
        let runner = StoryRunner::default();
        let panel = runner.0.panel(&runner);
        Self { runner, panel, actor: Actor::Person { name: "gallery".into() } }
    }
}

register_story! {
    id: "live-workspace", title: "Live workspace", group: Group::Composites, order: 90, state: StoryState,
    show: show,
}

/// Draws the story: the workspace at its natural height.
pub fn show(ui: &mut Ui, state: &mut StoryState) {
    // The gallery scrolls; a scroll area of the story's own would hide its controls from the gallery's Tab walk.
    state.panel.body(ui, &state.runner, &state.actor);
}
