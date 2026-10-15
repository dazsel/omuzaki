//! The Live workspace's gallery story, and the [`StoryRunner`] that lets it (and the evidence pictures) show the
//! workspace in each state without a performer.

use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

use egui::Ui;
use omuzaki_anim_api::RetargetConfig;
use omuzaki_doc_api::{DocPath, Value};
use omuzaki_ops_api::checks::op_info;
use omuzaki_ops_api::{Actor, Event, Grants, OpError, OpInfo, OpRecord, Outcome, Plan, Request, Runner, TxId, find};
use omuzaki_stage::perform::Tracking;
use omuzaki_stage::perform::host::{CalibrationState, PerformStatus};

use super::model::{LiveModel, OP_CALIBRATE, OP_SET_RETARGET};
use super::panel::LivePanel;
use crate::aux_window::Transparency;
use crate::ds::gallery::Group;
use crate::output_window::{OutputPlan, OutputWindow};
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

    /// The workspace on `runner`, showing this state, with a closed output window at the default plan.
    pub fn panel(self, runner: &dyn Runner) -> LivePanel {
        let mut model = LiveModel::new(runner);
        model.set_status(self.status());
        let output = OutputWindow::new(OutputPlan::default(), Transparency::Fallback);
        LivePanel::with_model(model, Some(Arc::new(Mutex::new(output))))
    }
}

/// A runner with no document that offers `perform.calibrate` and `perform.set_retarget` and accepts them without
/// doing anything. It has no `perform.status`, so the workspace shows whatever status its model is given
/// ([`LiveModel::set_status`]): the gallery and the evidence pictures pick a [`LiveState`] that way.
#[derive(Clone, Copy, Debug, Default)]
pub struct StoryRunner;

impl Runner for StoryRunner {
    fn ops(&self) -> Vec<OpInfo> {
        [OP_CALIBRATE, OP_SET_RETARGET].into_iter().filter_map(find).map(op_info).collect()
    }

    fn run(&self, _req: Request) -> Result<Outcome, OpError> {
        Ok(Outcome::new(Plan::default()))
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

/// The story's state: the panel and the runner it runs on.
struct State {
    runner: StoryRunner,
    panel: LivePanel,
    actor: Actor,
}

impl Default for State {
    fn default() -> Self {
        let runner = StoryRunner;
        let panel = LiveState::Calibrating.panel(&runner);
        Self { runner, panel, actor: Actor::Person { name: "gallery".into() } }
    }
}

register_story! {
    id: "live-workspace", title: "Live workspace", group: Group::Composites, order: 90, state: State,
    show: show,
}

fn show(ui: &mut Ui, state: &mut State) {
    // The gallery scrolls; a scroll area of the story's own would hide its controls from the gallery's Tab walk.
    state.panel.body(ui, &state.runner, &state.actor);
}
