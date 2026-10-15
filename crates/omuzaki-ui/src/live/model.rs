//! [`LiveModel`]: what the Live workspace shows and the ops it runs, apart from drawing.
//!
//! Every action is an op run through the shell's runner, as the person, so agents and the UI share one path. Only ops
//! that are registered in the runner are run, under the names and arguments those ops declare; a control whose op is
//! not on main yet is drawn disabled ([`Control`]) and never calls a guessed name.

use omuzaki_doc_api::Value;
use omuzaki_ops_api::{Actor, Grants, Mode, OpError, Request, Runner};
use omuzaki_stage::perform::Tracking;
use omuzaki_stage::perform::host::{CalibrationState, PerformStatus};
use serde::Deserialize as _;

/// `perform.calibrate`: start a neutral capture.
pub const OP_CALIBRATE: &str = "perform.calibrate";
/// `perform.set_retarget`: the smoothing dial (and the other retarget dials).
pub const OP_SET_RETARGET: &str = "perform.set_retarget";
/// `perform.status`: tracking, calibration, settings and frame rate.
pub const OP_STATUS: &str = "perform.status";
/// `io.import`: import a model file into the project.
pub const OP_IMPORT: &str = "io.import";

/// Frames the Calibrate button asks for: 10 s at a webcam's 30 fps (the prompt says 10 seconds).
pub const CALIBRATE_FRAMES: u64 = 300;
/// The smoothing dial's range, as `perform.set_retarget` declares it.
pub const SMOOTHING_RANGE: std::ops::RangeInclusive<f64> = 0.0..=1.0;
/// Hotkey rows the table always shows (V0 exit criterion 6).
pub const HOTKEY_ROWS: usize = 4;

/// Whether a control can act.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Control {
    /// Its op is registered: it works.
    Ready,
    /// Its op is registered in the design but not in this build's runner.
    Missing(&'static str),
    /// Its op is not on main yet; the control is drawn disabled with a "not available yet" tooltip.
    NotYet,
}

/// The ops this build's runner has, read once from its registry.
// One flag per op the workspace runs, each read by its own name: not a state machine in disguise.
#[allow(clippy::struct_excessive_bools)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Ops {
    /// `perform.calibrate` is registered.
    pub calibrate: bool,
    /// `perform.set_retarget` is registered.
    pub set_retarget: bool,
    /// `perform.status` is registered.
    pub status: bool,
    /// `io.import` is registered.
    pub import: bool,
}

impl Ops {
    /// The ops `runner` has.
    pub fn of(runner: &dyn Runner) -> Self {
        let names: Vec<&str> = runner.ops().iter().map(|op| op.name).collect();
        let has = |name: &str| names.contains(&name);
        Self {
            calibrate: has(OP_CALIBRATE),
            set_retarget: has(OP_SET_RETARGET),
            status: has(OP_STATUS),
            import: has(OP_IMPORT),
        }
    }

    fn control(registered: bool, op: &'static str) -> Control {
        if registered { Control::Ready } else { Control::Missing(op) }
    }

    /// The Calibrate button.
    pub fn calibrate(self) -> Control {
        Self::control(self.calibrate, OP_CALIBRATE)
    }

    /// The smoothing slider.
    pub fn smoothing(self) -> Control {
        Self::control(self.set_retarget, OP_SET_RETARGET)
    }

    /// The "Drop a VRM" target: absent, not broken, when the op is missing.
    pub fn import(self) -> Control {
        Self::control(self.import, OP_IMPORT)
    }

    /// The camera picker and preview: `input.bind_camera` and `input.preview` are not on main.
    pub const fn camera() -> Control {
        Control::NotYet
    }

    /// Hotkey add and remove: `trigger.bind`, `trigger.unbind` and `trigger.list` are not on main.
    pub const fn hotkeys() -> Control {
        Control::NotYet
    }
}

/// What went wrong with the last op, for the workspace's status line.
#[derive(Clone, Debug, PartialEq)]
pub struct Failure {
    /// The op.
    pub op: &'static str,
    /// Its error, as the runner said it.
    pub message: String,
}

/// The Live workspace's state.
#[derive(Clone, Debug)]
pub struct LiveModel {
    ops: Ops,
    status: Option<PerformStatus>,
    smoothing: f64,
    failure: Option<Failure>,
    imported: Option<String>,
}

impl LiveModel {
    /// The model for `runner`'s ops, before any status has been read.
    pub fn new(runner: &dyn Runner) -> Self {
        Self::with_ops(Ops::of(runner))
    }

    /// The model with these ops (the gallery story builds one without a runner).
    pub fn with_ops(ops: Ops) -> Self {
        Self { ops, status: None, smoothing: 0.0, failure: None, imported: None }
    }

    /// The ops.
    pub fn ops(&self) -> Ops {
        self.ops
    }

    /// The performer's status as last read.
    pub fn status(&self) -> Option<&PerformStatus> {
        self.status.as_ref()
    }

    /// The smoothing value the slider shows.
    pub fn smoothing(&self) -> f64 {
        self.smoothing
    }

    /// The slider's value, while it is dragged (not sent until it commits).
    pub fn smoothing_mut(&mut self) -> &mut f64 {
        &mut self.smoothing
    }

    /// The last failure, if the last op failed.
    pub fn failure(&self) -> Option<&Failure> {
        self.failure.as_ref()
    }

    /// The name of the file the last import took in.
    pub fn imported(&self) -> Option<&str> {
        self.imported.as_deref()
    }

    /// Whether a face is tracked, from the last status.
    pub fn tracking(&self) -> Option<Tracking> {
        self.status.as_ref().map(|status| status.tracking)
    }

    /// The calibration state, from the last status.
    pub fn calibration(&self) -> Option<CalibrationState> {
        self.status.as_ref().map(|status| status.calibration)
    }

    /// Whether a capture is running (the button is disabled meanwhile).
    pub fn calibrating(&self) -> bool {
        matches!(self.calibration(), Some(CalibrationState::Capturing { .. }))
    }

    /// Takes `status` in, as `perform.status` returned it. The slider follows the applied smoothing when the first
    /// status arrives and whenever the performer has applied a newer settings change, never in between: a status read
    /// during a drag must not pull the handle back.
    pub fn set_status(&mut self, status: PerformStatus) {
        let newer = self.status.as_ref().is_none_or(|old| status.retarget_seq != old.retarget_seq);
        if newer {
            self.smoothing = f64::from(status.retarget.smoothing);
        }
        self.status = Some(status);
    }

    /// Reads `perform.status`. A failure clears the status (the workspace shows "waiting for the performer").
    pub fn refresh(&mut self, runner: &dyn Runner, actor: &Actor) {
        if !self.ops.status {
            return;
        }
        let read = run(runner, actor, OP_STATUS, object(&[])).and_then(|output| {
            PerformStatus::deserialize(output).map_err(|e| OpError::internal(format!("{OP_STATUS} answered {e}")))
        });
        match read {
            Ok(status) => self.set_status(status),
            Err(error) => {
                self.status = None;
                self.fail(OP_STATUS, &error);
            }
        }
    }

    /// Runs `perform.calibrate` for [`CALIBRATE_FRAMES`] frames.
    pub fn calibrate(&mut self, runner: &dyn Runner, actor: &Actor) {
        let args = object(&[("frames", Value::from(CALIBRATE_FRAMES))]);
        self.act(runner, actor, self.ops.calibrate, OP_CALIBRATE, args);
    }

    /// Runs `perform.set_retarget` with the slider's smoothing, clamped to the op's range.
    pub fn commit_smoothing(&mut self, runner: &dyn Runner, actor: &Actor) {
        self.smoothing = self.smoothing.clamp(*SMOOTHING_RANGE.start(), *SMOOTHING_RANGE.end());
        let args = object(&[("smoothing", Value::from(self.smoothing))]);
        self.act(runner, actor, self.ops.set_retarget, OP_SET_RETARGET, args);
    }

    /// Runs `io.import` on the dropped file at `path`.
    pub fn import(&mut self, runner: &dyn Runner, actor: &Actor, path: &str) {
        if self.act(runner, actor, self.ops.import, OP_IMPORT, object(&[("path", Value::from(path))])) {
            let name = std::path::Path::new(path).file_name().map_or(path, |n| n.to_str().unwrap_or(path));
            self.imported = Some(name.to_owned());
        }
    }

    /// Runs an action op when it is registered; returns whether it succeeded.
    fn act(&mut self, runner: &dyn Runner, actor: &Actor, registered: bool, op: &'static str, args: Value) -> bool {
        if !registered {
            // The control is drawn disabled; a call that still arrives (a script, a stale frame) does nothing.
            return false;
        }
        match run(runner, actor, op, args) {
            Ok(_) => {
                self.failure = None;
                true
            }
            Err(error) => {
                self.fail(op, &error);
                false
            }
        }
    }

    fn fail(&mut self, op: &'static str, error: &OpError) {
        self.failure = Some(Failure { op, message: error.to_string() });
    }
}

/// Runs `op` with `args` as `actor` (a person: the UI is a person's transport) and returns its output.
fn run(runner: &dyn Runner, actor: &Actor, op: &str, args: Value) -> Result<Value, OpError> {
    let request = Request {
        op: op.to_owned(),
        args,
        mode: Mode::Apply,
        actor: actor.clone(),
        grants: Grants::person(),
        cause: None,
        tx: None,
        base: None,
    };
    runner.run(request).map(|outcome| outcome.plan.output.unwrap_or(Value::Null))
}

/// A JSON object of `pairs`.
fn object(pairs: &[(&str, Value)]) -> Value {
    Value::Object(pairs.iter().map(|(key, value)| ((*key).to_owned(), value.clone())).collect())
}

#[cfg(test)]
mod tests {
    use omuzaki_anim_api::RetargetConfig;

    use super::*;

    fn status(seq: u64, smoothing: f32) -> PerformStatus {
        PerformStatus {
            tracking: Tracking::Tracking,
            calibration: CalibrationState::Idle,
            retarget: RetargetConfig { smoothing, ..RetargetConfig::default() },
            retarget_seq: seq,
            source_fps: 30.0,
        }
    }

    #[test]
    fn the_slider_follows_applied_changes_but_a_status_read_mid_drag_does_not_pull_it_back() {
        let mut model = LiveModel::with_ops(Ops::default());
        model.set_status(status(0, 0.25));
        assert!((model.smoothing() - 0.25).abs() < 1e-6, "the first status sets the slider");
        *model.smoothing_mut() = 0.8;
        model.set_status(status(0, 0.25));
        assert!((model.smoothing() - 0.8).abs() < 1e-6, "the same applied settings leave the drag alone");
        model.set_status(status(1, 0.5));
        assert!((model.smoothing() - 0.5).abs() < 1e-6, "a newer applied change (an agent's, say) shows");
    }

    #[test]
    fn controls_follow_the_registered_ops_and_the_missing_ones_name_their_op() {
        let ops = Ops { calibrate: true, ..Ops::default() };
        assert_eq!(ops.calibrate(), Control::Ready);
        assert_eq!(ops.smoothing(), Control::Missing(OP_SET_RETARGET));
        assert_eq!(ops.import(), Control::Missing(OP_IMPORT));
        assert_eq!((Ops::camera(), Ops::hotkeys()), (Control::NotYet, Control::NotYet));
    }

    #[test]
    fn an_action_whose_op_is_not_registered_does_nothing() {
        let mut model = LiveModel::with_ops(Ops::default());
        let runner = crate::live::story::StoryRunner;
        let actor = Actor::Person { name: "p".into() };
        model.calibrate(&runner, &actor);
        model.import(&runner, &actor, "x.vrm");
        assert!(model.failure().is_none() && model.imported().is_none());
    }
}
