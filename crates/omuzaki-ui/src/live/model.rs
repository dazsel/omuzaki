//! [`LiveModel`]: what the Live workspace shows and the ops it runs, apart from drawing.
//!
//! Every action is an op run through the shell's runner, as the person, so agents and the UI share one path. Only ops
//! that are registered in the runner are run, under the names and arguments those ops declare; a control whose op is
//! not on main yet is drawn disabled ([`Control`]) and never calls a guessed name.

use omuzaki_doc_api::Value;
use omuzaki_ops_api::{Actor, Grants, Mode, OpError, Request, Runner};
use omuzaki_stage::perform::Tracking;
use omuzaki_stage::perform::host::{CalibrationState, PerformStatus};
use omuzaki_stage::perform::input::{InputState, SourceKind};
use omuzaki_stage::perform::triggers::TriggerInfo;
use omuzaki_stream_api::window::WindowState;
use serde::Deserialize as _;

/// `perform.calibrate`: start a neutral capture.
pub const OP_CALIBRATE: &str = "perform.calibrate";
/// `perform.set_retarget`: the smoothing dial (and the other retarget dials).
pub const OP_SET_RETARGET: &str = "perform.set_retarget";
/// `perform.status`: tracking, calibration, settings and frame rate.
pub const OP_STATUS: &str = "perform.status";
/// `io.import`: import a model file into the project.
pub const OP_IMPORT: &str = "io.import";
/// `trigger.bind`: bind a key to a face expression.
pub const OP_TRIGGER_BIND: &str = "trigger.bind";
/// `trigger.unbind`: remove a key's binding.
pub const OP_TRIGGER_UNBIND: &str = "trigger.unbind";
/// `trigger.list`: the bindings, and which one is showing.
pub const OP_TRIGGER_LIST: &str = "trigger.list";
/// `output.window.open`: open (or re-open with other settings) the clean output window.
pub const OP_WINDOW_OPEN: &str = "output.window.open";
/// `output.window.close`: close the clean output window.
pub const OP_WINDOW_CLOSE: &str = "output.window.close";
/// `output.window.status`: whether the output window is open, its size, backdrop and whether it is minimised.
pub const OP_WINDOW_STATUS: &str = "output.window.status";
/// `input.state`: the source, the bound camera, the mirror, the permission, the cameras and tracking.
pub const OP_INPUT_STATE: &str = "input.state";
/// `input.select`: choose what drives the face.
pub const OP_INPUT_SELECT: &str = "input.select";
/// `input.bind_camera`: bind the camera the webcam source reads (the person only).
pub const OP_INPUT_BIND_CAMERA: &str = "input.bind_camera";
/// `input.preview`: start or stop the mirror, the capture that drives the face.
pub const OP_INPUT_PREVIEW: &str = "input.preview";
/// `input.request_camera`: show the OS camera permission prompt (the person only).
pub const OP_INPUT_REQUEST_CAMERA: &str = "input.request_camera";

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
    /// `trigger.bind` is registered.
    pub trigger_bind: bool,
    /// `trigger.unbind` is registered.
    pub trigger_unbind: bool,
    /// `trigger.list` is registered.
    pub trigger_list: bool,
    /// `output.window.open` is registered.
    pub window_open: bool,
    /// `output.window.close` is registered.
    pub window_close: bool,
    /// `output.window.status` is registered.
    pub window_status: bool,
    /// `input.state` is registered.
    pub input_state: bool,
    /// `input.select` is registered.
    pub input_select: bool,
    /// `input.bind_camera` is registered.
    pub input_bind_camera: bool,
    /// `input.preview` is registered.
    pub input_preview: bool,
    /// `input.request_camera` is registered.
    pub input_request_camera: bool,
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
            trigger_bind: has(OP_TRIGGER_BIND),
            trigger_unbind: has(OP_TRIGGER_UNBIND),
            trigger_list: has(OP_TRIGGER_LIST),
            window_open: has(OP_WINDOW_OPEN),
            window_close: has(OP_WINDOW_CLOSE),
            window_status: has(OP_WINDOW_STATUS),
            input_state: has(OP_INPUT_STATE),
            input_select: has(OP_INPUT_SELECT),
            input_bind_camera: has(OP_INPUT_BIND_CAMERA),
            input_preview: has(OP_INPUT_PREVIEW),
            input_request_camera: has(OP_INPUT_REQUEST_CAMERA),
        }
    }

    fn control(registered: bool, op: &'static str) -> Control {
        if registered { Control::Ready } else { Control::Missing(op) }
    }

    /// The first of `needed` that isn't registered, as the control's state.
    fn all(needed: &[(bool, &'static str)]) -> Control {
        needed.iter().find(|(registered, _)| !registered).map_or(Control::Ready, |(_, op)| Control::Missing(op))
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

    /// The camera picker and the tracking switch: they read `input.state` and run `input.bind_camera`,
    /// `input.select` and `input.preview`.
    pub fn camera(self) -> Control {
        Self::all(&[
            (self.input_state, OP_INPUT_STATE),
            (self.input_select, OP_INPUT_SELECT),
            (self.input_bind_camera, OP_INPUT_BIND_CAMERA),
            (self.input_preview, OP_INPUT_PREVIEW),
        ])
    }

    /// The Allow camera button: `input.request_camera`.
    pub fn request_camera(self) -> Control {
        Self::control(self.input_request_camera, OP_INPUT_REQUEST_CAMERA)
    }

    /// The output window's switch and settings: they open, close and read the window through its ops.
    pub fn output(self) -> Control {
        Self::all(&[
            (self.window_open, OP_WINDOW_OPEN),
            (self.window_close, OP_WINDOW_CLOSE),
            (self.window_status, OP_WINDOW_STATUS),
        ])
    }

    /// The hotkey table: it binds, unbinds and lists, so it needs all three trigger ops.
    pub fn hotkeys(self) -> Control {
        Self::all(&[
            (self.trigger_bind, OP_TRIGGER_BIND),
            (self.trigger_unbind, OP_TRIGGER_UNBIND),
            (self.trigger_list, OP_TRIGGER_LIST),
        ])
    }
}

/// What the output window shows behind the character, as the person picks it.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Backdrop {
    /// The key colour, which every capture tool can remove. The default.
    #[default]
    Key,
    /// A transparent surface, where this computer has one; elsewhere the window takes the key and says so.
    Transparent,
}

impl Backdrop {
    /// `output.window.open`'s `backdrop` argument.
    pub const fn arg(self) -> &'static str {
        match self {
            Self::Key => "key",
            Self::Transparent => "transparent",
        }
    }
}

/// The output window's settings as the person picked them; they are the arguments of the next open.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct WindowChoice {
    /// What shows behind the character.
    pub backdrop: Backdrop,
    /// Keep the window above other windows.
    pub on_top: bool,
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
    triggers: Vec<TriggerInfo>,
    smoothing: f64,
    failure: Option<Failure>,
    imported: Option<String>,
    input: Option<InputState>,
    window: Option<WindowState>,
    window_choice: WindowChoice,
    /// The last open asked for a transparent surface and got the key (`output.window.open`'s `fallback`).
    window_fallback: bool,
}

impl LiveModel {
    /// The model for `runner`'s ops, before any status has been read.
    pub fn new(runner: &dyn Runner) -> Self {
        Self::with_ops(Ops::of(runner))
    }

    /// The model with these ops (the gallery story builds one without a runner).
    pub fn with_ops(ops: Ops) -> Self {
        Self {
            ops,
            status: None,
            triggers: Vec::new(),
            smoothing: 0.0,
            failure: None,
            imported: None,
            input: None,
            window: None,
            window_choice: WindowChoice::default(),
            window_fallback: false,
        }
    }

    /// The ops.
    pub fn ops(&self) -> Ops {
        self.ops
    }

    /// The performer's status as last read.
    pub fn status(&self) -> Option<&PerformStatus> {
        self.status.as_ref()
    }

    /// The key bindings as `trigger.list` last listed them.
    pub fn triggers(&self) -> &[TriggerInfo] {
        &self.triggers
    }

    /// The binding that shows `expression`, if a key is bound to it.
    pub fn trigger_for(&self, expression: &str) -> Option<&TriggerInfo> {
        self.triggers.iter().find(|info| info.binding.expression.as_str() == expression)
    }

    /// Replaces the listed bindings (the story's states, and tests).
    pub fn set_triggers(&mut self, triggers: Vec<TriggerInfo>) {
        self.triggers = triggers;
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

    /// The tracking input as `input.state` last read it.
    pub fn input(&self) -> Option<&InputState> {
        self.input.as_ref()
    }

    /// The output window as `output.window.status` last read it.
    pub fn window(&self) -> Option<WindowState> {
        self.window
    }

    /// Whether the output window is open, as last read.
    pub fn window_open(&self) -> bool {
        matches!(self.window, Some(WindowState::Open { .. }))
    }

    /// The output window settings the next open asks for.
    pub fn window_choice(&self) -> WindowChoice {
        self.window_choice
    }

    /// Whether the window that is open asked for transparency and got the key instead.
    pub fn window_fallback(&self) -> bool {
        self.window_fallback && self.window_open()
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

    /// Reads `perform.status` and the key bindings. A failure clears what it was reading (the workspace shows
    /// "waiting for the performer").
    pub fn refresh(&mut self, runner: &dyn Runner, actor: &Actor) {
        self.refresh_status(runner, actor);
        self.refresh_triggers(runner, actor);
        self.refresh_input(runner, actor);
        self.refresh_window(runner, actor);
    }

    /// Reads `input.state`.
    fn refresh_input(&mut self, runner: &dyn Runner, actor: &Actor) {
        if !self.ops.input_state {
            return;
        }
        let read = run(runner, actor, OP_INPUT_STATE, object(&[])).and_then(|output| {
            InputState::deserialize(output).map_err(|e| OpError::internal(format!("{OP_INPUT_STATE} answered {e}")))
        });
        match read {
            Ok(view) => self.input = Some(view),
            Err(error) => {
                self.input = None;
                self.fail(OP_INPUT_STATE, &error);
            }
        }
    }

    /// Reads `output.window.status`.
    fn refresh_window(&mut self, runner: &dyn Runner, actor: &Actor) {
        if !self.ops.window_status {
            return;
        }
        let read = run(runner, actor, OP_WINDOW_STATUS, object(&[])).and_then(|output| {
            WindowState::deserialize(output).map_err(|e| OpError::internal(format!("{OP_WINDOW_STATUS} answered {e}")))
        });
        match read {
            Ok(state) => self.window = Some(state),
            Err(error) => {
                self.window = None;
                self.fail(OP_WINDOW_STATUS, &error);
            }
        }
    }

    /// Reads `perform.status`.
    fn refresh_status(&mut self, runner: &dyn Runner, actor: &Actor) {
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

    /// Reads `trigger.list`.
    fn refresh_triggers(&mut self, runner: &dyn Runner, actor: &Actor) {
        if !self.ops.trigger_list {
            return;
        }
        let read = run(runner, actor, OP_TRIGGER_LIST, object(&[])).and_then(|mut output| {
            let triggers = output.get_mut("triggers").map(Value::take).unwrap_or_default();
            Vec::<TriggerInfo>::deserialize(triggers)
                .map_err(|e| OpError::internal(format!("{OP_TRIGGER_LIST} answered {e}")))
        });
        match read {
            Ok(triggers) => self.triggers = triggers,
            Err(error) => {
                self.triggers.clear();
                self.fail(OP_TRIGGER_LIST, &error);
            }
        }
    }

    /// Runs `trigger.bind`: `key` shows `expression`, toggled by each press (the op's default). A key that showed
    /// another expression moves to this one (the op replaces it); the table is read again so it shows the result.
    pub fn bind_hotkey(&mut self, runner: &dyn Runner, actor: &Actor, key: &str, expression: &str) {
        let old = self.trigger_for(expression).map(|info| info.binding.key.clone());
        let args = object(&[("key", Value::from(key)), ("expression", Value::from(expression))]);
        if !self.act(runner, actor, self.ops.trigger_bind, OP_TRIGGER_BIND, args) {
            return;
        }
        // One key per expression in the table: once the new key is bound, the one it had before is let go.
        match old.filter(|old| !old.eq_ignore_ascii_case(key)) {
            Some(old) => self.unbind_hotkey(runner, actor, &old),
            None => self.refresh_triggers(runner, actor),
        }
    }

    /// Runs `trigger.unbind` on `key`, then reads the table again.
    pub fn unbind_hotkey(&mut self, runner: &dyn Runner, actor: &Actor, key: &str) {
        let args = object(&[("key", Value::from(key))]);
        if self.act(runner, actor, self.ops.trigger_unbind, OP_TRIGGER_UNBIND, args) {
            self.refresh_triggers(runner, actor);
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

    /// Opens the output window with the settings picked ([`WindowChoice`]) through `output.window.open`, or closes it
    /// through `output.window.close`, then reads its status again.
    pub fn set_window_open(&mut self, runner: &dyn Runner, actor: &Actor, open: bool) {
        if open {
            self.open_window(runner, actor);
        } else if self.act(runner, actor, self.ops.window_close, OP_WINDOW_CLOSE, object(&[])) {
            self.window_fallback = false;
        }
        self.refresh_window(runner, actor);
    }

    /// Picks the window's settings; an open window re-opens with them (an open replaces the window).
    pub fn set_window_choice(&mut self, runner: &dyn Runner, actor: &Actor, choice: WindowChoice) {
        if choice == self.window_choice {
            return;
        }
        self.window_choice = choice;
        if self.window_open() {
            self.open_window(runner, actor);
            self.refresh_window(runner, actor);
        }
    }

    fn open_window(&mut self, runner: &dyn Runner, actor: &Actor) {
        if !self.ops.window_open {
            return;
        }
        let args = object(&[
            ("backdrop", Value::from(self.window_choice.backdrop.arg())),
            ("always_on_top", Value::from(self.window_choice.on_top)),
        ]);
        match run(runner, actor, OP_WINDOW_OPEN, args) {
            Ok(answer) => {
                self.failure = None;
                self.window_fallback = answer.get("fallback").and_then(Value::as_bool).unwrap_or(false);
            }
            Err(error) => self.fail(OP_WINDOW_OPEN, &error),
        }
    }

    /// Binds `camera` (an id `input.state` listed; `None` unbinds) through `input.bind_camera`, then reads the input
    /// again.
    pub fn bind_camera(&mut self, runner: &dyn Runner, actor: &Actor, camera: Option<&str>) {
        let args = object(&[("camera", Value::from(camera))]);
        if self.act(runner, actor, self.ops.input_bind_camera, OP_INPUT_BIND_CAMERA, args) {
            self.refresh_input(runner, actor);
        }
    }

    /// Starts tracking the face from the bound camera (`input.select` to the webcam when another source is chosen,
    /// then `input.preview` on), or stops it (`input.preview` off); then reads the input again. Also the Retry after an
    /// unplug: starting again opens the camera afresh.
    pub fn set_tracking(&mut self, runner: &dyn Runner, actor: &Actor, on: bool) {
        let webcam = self.input.as_ref().is_some_and(|view| view.source == SourceKind::Webcam);
        if on && !webcam {
            let args = object(&[("source", Value::from("webcam"))]);
            if !self.act(runner, actor, self.ops.input_select, OP_INPUT_SELECT, args) {
                return;
            }
        }
        let args = object(&[("on", Value::from(on))]);
        if self.act(runner, actor, self.ops.input_preview, OP_INPUT_PREVIEW, args) {
            self.refresh_input(runner, actor);
        }
    }

    /// Shows the OS camera prompt through `input.request_camera`, then reads the input again.
    pub fn request_camera(&mut self, runner: &dyn Runner, actor: &Actor) {
        if self.act(runner, actor, self.ops.input_request_camera, OP_INPUT_REQUEST_CAMERA, object(&[])) {
            self.refresh_input(runner, actor);
        }
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
        assert_eq!(ops.camera(), Control::Missing(OP_INPUT_STATE));
        assert_eq!(ops.output(), Control::Missing(OP_WINDOW_OPEN));
        assert_eq!(ops.hotkeys(), Control::Missing(OP_TRIGGER_BIND));
        let input = Ops {
            input_state: true,
            input_select: true,
            input_bind_camera: true,
            input_preview: true,
            ..Ops::default()
        };
        assert_eq!(input.camera(), Control::Ready);
        assert_eq!(input.request_camera(), Control::Missing(OP_INPUT_REQUEST_CAMERA), "the prompt has its own op");
        let triggers = Ops { trigger_bind: true, trigger_unbind: true, ..Ops::default() };
        assert_eq!(triggers.hotkeys(), Control::Missing(OP_TRIGGER_LIST), "the table needs all three");
    }

    #[test]
    fn an_action_whose_op_is_not_registered_does_nothing() {
        let mut model = LiveModel::with_ops(Ops::default());
        let runner = crate::live::story::StoryRunner::default();
        let actor = Actor::Person { name: "p".into() };
        model.calibrate(&runner, &actor);
        model.import(&runner, &actor, "x.vrm");
        assert!(model.failure().is_none() && model.imported().is_none());
    }
}
