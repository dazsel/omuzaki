//! [`ProgramService`]: the program compositor. The studio hands it every frame the outputs carry
//! ([`compose`](ProgramService::compose)); it plays the scene transition and draws the overlay's widgets in, with alpha.
//! The show's events reach it through [`Tee`], the ops through the service, and the document's widgets through
//! [`follow`](ProgramService::follow).
//!
//! **Scene switches.** `program.switch_scene` asks for a switch. With an output running, the next composed frame is
//! held as the outgoing picture and the switch is then handed to the stage ([`take_switch`](ProgramService::take_switch));
//! the transition blends the held picture into the new scene's live frames. With no output running there is nothing
//! to blend and the switch is a cut.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use omuzaki_core::time::Clock;
use omuzaki_doc_api::DocRead;
use omuzaki_live_api::event::{EventSink, LiveEvent, PublishError};
use omuzaki_render_api::Image;
use serde_json::{Value, json};

use crate::live::{Alert, LiveWidget, State};
use crate::model::{Kind, OverlaySettings, WidgetDef, WidgetId, paths};
use crate::paint::Fonts;
use crate::transition::{Blender, TransitionSpec};

/// How long a requested switch waits for an output frame to hold before it cuts anyway, seconds.
pub const CAPTURE_WAIT_S: f64 = 0.25;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Where a scene switch is.
#[derive(Debug, Default)]
enum Switch {
    /// None asked for.
    #[default]
    Idle,
    /// Asked for at `at`: the next composed frame is held.
    Requested { scene: String, spec: TransitionSpec, at: f64 },
    /// The outgoing frame is held; the stage switches next.
    Ready { scene: String, spec: TransitionSpec, from: Box<Image> },
    /// Blending from the held frame since `started`: the first frame composed after the stage switched, so a scene that
    /// took a while to stage still plays its whole transition.
    Running { scene: String, spec: TransitionSpec, from: Box<Image>, started: Option<f64> },
}

/// A timer action for [`ProgramService::timer`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TimerAction {
    /// Start or resume.
    Start,
    /// Pause.
    Pause,
    /// Back to its start, paused.
    Reset,
    /// Add `seconds` (negative takes time off).
    Add,
    /// Set it to `seconds`.
    Set,
}

/// The compositor's counters, for `program.status` and the health meter.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, schemars::JsonSchema)]
pub struct ComposeStats {
    /// Frames composed.
    pub frames: u64,
    /// How long the last compose took, milliseconds.
    pub last_ms: f64,
    /// The longest compose so far, milliseconds.
    pub max_ms: f64,
}

/// Adaptive quality as the app reports it, for `program.status` (the controller runs where the frame is rendered).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, schemars::JsonSchema)]
pub struct QualityReport {
    /// Whether the controller may change quality now.
    pub adaptive: bool,
    /// Why it is holding still while on: `"recording"` while a take records and changes are not allowed then.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub held: Option<String>,
    /// How many rungs of the degrade ladder are applied (0 is the quality asked for).
    pub rung: u32,
    /// Changes of quality so far.
    pub changes: u64,
    /// The last change: the rung's name and which way.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last: Option<String>,
    /// The smoothed frame time it holds against the budget, milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_ms: Option<f64>,
    /// The budget, milliseconds.
    pub budget_ms: f64,
}

struct Inner {
    widgets: BTreeMap<WidgetId, LiveWidget>,
    settings: OverlaySettings,
    /// Widgets the document holds that don't parse, with why.
    problems: Vec<(String, String)>,
    /// The scene on the program; `None` while it follows the project's default.
    scene: Option<String>,
    switch: Switch,
    blender: Blender,
    last_step: Option<f64>,
    stats: ComposeStats,
    quality: Option<QualityReport>,
}

/// The program compositor. Insert it into the runner's services for the `overlay.*` and `program.*` ops.
pub struct ProgramService {
    inner: Mutex<Inner>,
    clock: Arc<dyn Clock>,
    fonts: Fonts,
}

impl std::fmt::Debug for ProgramService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProgramService").finish_non_exhaustive()
    }
}

impl ProgramService {
    /// A compositor with no widgets, timed on `clock`.
    ///
    /// # Errors
    /// A message if the bundled fonts don't load (a build fault).
    pub fn new(clock: Arc<dyn Clock>) -> Result<Self, String> {
        Ok(Self {
            inner: Mutex::new(Inner {
                widgets: BTreeMap::new(),
                settings: OverlaySettings::default(),
                problems: Vec::new(),
                scene: None,
                switch: Switch::Idle,
                blender: Blender::default(),
                last_step: None,
                stats: ComposeStats::default(),
                quality: None,
            }),
            clock,
            fonts: Fonts::new()?,
        })
    }

    fn now(&self) -> f64 {
        self.clock.now().0
    }

    /// Reads the overlay from the document: widgets added, changed and removed since the last call. A widget's live
    /// state survives a change to its definition. Cold; call it when the document changes.
    pub fn follow(&self, doc: &dyn DocRead) {
        let mut inner = lock(&self.inner);
        inner.settings =
            doc.get(&paths::settings()).and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default();
        let mut seen = Vec::new();
        let mut problems = Vec::new();
        if let Some(Value::Object(widgets)) = doc.get(&paths::widgets()) {
            for (key, value) in widgets {
                let Ok(id) = WidgetId::new(key.clone()) else {
                    problems.push((key.clone(), "not a valid widget id".to_owned()));
                    continue;
                };
                let def = match serde_json::from_value::<WidgetDef>(value.clone()) {
                    Ok(def) => def,
                    Err(e) => {
                        problems.push((key.clone(), e.to_string()));
                        continue;
                    }
                };
                if let Err((field, why)) = def.check() {
                    problems.push((key.clone(), format!("{field}: {why}")));
                    continue;
                }
                match inner.widgets.get_mut(&id) {
                    Some(live) if live.def == def => {}
                    Some(live) => live.redefine(def),
                    None => {
                        inner.widgets.insert(id.clone(), LiveWidget::new(def));
                    }
                }
                seen.push(id);
            }
        }
        inner.widgets.retain(|id, _| seen.contains(id));
        inner.problems = problems;
    }

    /// Takes an event from the show: alerts queue, chat lines arrive, goals and subathon timers move.
    pub fn observe(&self, event: &LiveEvent) {
        let now = self.now();
        for widget in lock(&self.inner).widgets.values_mut() {
            widget.observe(event, now);
        }
    }

    /// Queues `alert` on widget `id`, or on the first alerts widget. Returns the widget that took it.
    ///
    /// # Errors
    /// A message when there is no such alerts widget.
    pub fn alert(&self, id: Option<&WidgetId>, alert: Alert) -> Result<WidgetId, String> {
        let mut inner = lock(&self.inner);
        let target = match id {
            Some(id) => inner.widgets.get_mut(id).map(|w| (id.clone(), w)),
            None => inner
                .widgets
                .iter_mut()
                .find(|(_, w)| w.def.content.kind() == Kind::Alerts)
                .map(|(i, w)| (i.clone(), w)),
        };
        let (id, widget) = target.ok_or_else(|| "there is no alerts widget".to_owned())?;
        if widget.push_alert(alert) { Ok(id) } else { Err(format!("'{id}' is not an alerts widget")) }
    }

    /// Runs `action` on timer `id`. Returns the timer's state after.
    ///
    /// # Errors
    /// A message when there is no such timer.
    pub fn timer(&self, id: &WidgetId, action: TimerAction, seconds: f64) -> Result<Value, String> {
        let mut inner = lock(&self.inner);
        let widget = inner.widgets.get_mut(id).ok_or_else(|| format!("there is no widget '{id}'"))?;
        let start = match &widget.def.content {
            crate::model::Content::Timer(t) if t.mode == crate::model::TimerMode::Stopwatch => 0.0,
            crate::model::Content::Timer(t) => t.seconds,
            _ => return Err(format!("'{id}' is not a timer")),
        };
        let State::Timer { seconds: now, running } = &mut widget.state else {
            return Err(format!("'{id}' is not a timer"));
        };
        match action {
            TimerAction::Start => *running = true,
            TimerAction::Pause => *running = false,
            TimerAction::Reset => (*now, *running) = (start, false),
            TimerAction::Add => *now = (*now + seconds).max(0.0),
            TimerAction::Set => *now = seconds.max(0.0),
        }
        let report = widget.state.report();
        widget.touch();
        Ok(report)
    }

    /// Sets goal `id` to `value`, or adds `value` to it. Returns the new value.
    ///
    /// # Errors
    /// A message when there is no such goal.
    pub fn goal(&self, id: &WidgetId, value: f64, add: bool) -> Result<f64, String> {
        let mut inner = lock(&self.inner);
        let widget = inner.widgets.get_mut(id).ok_or_else(|| format!("there is no widget '{id}'"))?;
        let State::Goal(current) = &mut widget.state else { return Err(format!("'{id}' is not a goal")) };
        *current = if add { (*current + value).max(0.0) } else { value.max(0.0) };
        let out = *current;
        widget.touch();
        Ok(out)
    }

    /// Asks for the program to switch to `scene` with `spec` (the overlay's default transition when `None`). A switch
    /// asked for while another runs replaces it.
    pub fn request_switch(&self, scene: String, spec: Option<TransitionSpec>) {
        let now = self.now();
        let mut inner = lock(&self.inner);
        let spec = spec.unwrap_or_else(|| inner.settings.transition.clone());
        inner.switch = Switch::Requested { scene, spec, at: now };
    }

    /// The scene the stage should switch to now, if one is due. `outputs` says whether an output runs (and so
    /// whether frames are being composed): without one, a requested switch is due at once, as a cut.
    pub fn take_switch(&self, outputs: bool) -> Option<String> {
        let now = self.now();
        let mut inner = lock(&self.inner);
        let switch = std::mem::take(&mut inner.switch);
        let (next, due) = match switch {
            Switch::Requested { scene, at, .. } if !outputs || now - at > CAPTURE_WAIT_S => (Switch::Idle, Some(scene)),
            Switch::Ready { scene, spec, from } if spec.seconds() > 0.0 => {
                (Switch::Running { scene: scene.clone(), spec, from, started: None }, Some(scene))
            }
            Switch::Ready { scene, .. } => (Switch::Idle, Some(scene)),
            other => (other, None),
        };
        inner.switch = next;
        if let Some(scene) = &due {
            inner.scene = Some(scene.clone());
        }
        due
    }

    /// Adaptive quality as the project sets it (`program.set_adaptive`).
    pub fn adaptive(&self) -> crate::model::AdaptiveSpec {
        lock(&self.inner).settings.adaptive.clone()
    }

    /// What the app's quality controller is doing, for `program.status`.
    pub fn report_quality(&self, report: QualityReport) {
        lock(&self.inner).quality = Some(report);
    }

    /// The scene on the program, once a switch has happened; `None` while it follows the project's default.
    pub fn scene(&self) -> Option<String> {
        lock(&self.inner).scene.clone()
    }

    /// Plays the transition and draws the visible widgets into `frame`, which the outputs carry next. Draws a widget's
    /// picture again only when it changed.
    pub fn compose(&self, frame: &mut Image) {
        let started = Instant::now();
        let now = self.now();
        let mut inner = lock(&self.inner);
        let inner = &mut *inner;
        let dt = inner.last_step.map_or(0.0, |last| (now - last).max(0.0));
        inner.last_step = Some(now);
        for widget in inner.widgets.values_mut() {
            widget.step(now, dt);
        }
        // The transition, on the scene's own pixels, before anything is drawn over them.
        match std::mem::take(&mut inner.switch) {
            Switch::Requested { scene, spec, .. } => {
                inner.switch = Switch::Ready { scene, spec, from: Box::new(frame.clone()) };
            }
            Switch::Running { scene, spec, from, started } => {
                let started = started.unwrap_or(now);
                #[allow(clippy::cast_possible_truncation, reason = "a progress fraction")]
                let t = ((now - started) / spec.seconds().max(1e-6)) as f32;
                if t < 1.0 {
                    inner.blender.blend(&spec, &from, frame, t);
                    inner.switch = Switch::Running { scene, spec, from, started: Some(started) };
                }
            }
            other => inner.switch = other,
        }
        let size = (frame.size.x, frame.size.y);
        let safe = f64::from(inner.settings.safe) * f64::from(size.1);
        let scene = inner.scene.clone();
        let mut order: Vec<&mut LiveWidget> =
            inner.widgets.values_mut().filter(|w| w.def.shows_in(scene.as_deref()) && w.showing()).collect();
        // Alerts on top, then the chat, then the rest.
        order.sort_by_key(|w| match w.def.content.kind() {
            Kind::Alerts => 3,
            Kind::Chat => 2,
            _ => 1,
        });
        for widget in order {
            let (opacity, rise) = widget.motion(now, size.1);
            let layout = widget.def.layout.clone();
            let picture = widget.picture(&self.fonts, size);
            let (ax, ay) = layout.anchor.fractions();
            let free_x = f64::from(size.0) - 2.0 * safe - f64::from(picture.width);
            let free_y = f64::from(size.1) - 2.0 * safe - f64::from(picture.height);
            let x = safe + f64::from(ax) * free_x + f64::from(layout.x) * f64::from(size.0);
            let y = safe + f64::from(ay) * free_y + f64::from(layout.y) * f64::from(size.1) + f64::from(rise);
            #[allow(clippy::cast_possible_truncation, reason = "pixel positions")]
            picture.blend_into(frame, x.round() as i64, y.round() as i64, opacity);
        }
        let ms = started.elapsed().as_secs_f64() * 1000.0;
        inner.stats.frames += 1;
        inner.stats.last_ms = ms;
        inner.stats.max_ms = inner.stats.max_ms.max(ms);
    }

    /// The widgets with their live state, as `overlay.list` reports them.
    pub fn widgets(&self) -> Value {
        let inner = lock(&self.inner);
        let widgets: Vec<Value> = inner
            .widgets
            .iter()
            .map(|(id, w)| {
                json!({
                    "id": id.as_str(),
                    "kind": w.def.content.kind().name(),
                    "visible": w.def.visible,
                    "scenes": w.def.scenes,
                    "state": w.state.report(),
                })
            })
            .collect();
        let problems: Vec<Value> = inner.problems.iter().map(|(id, why)| json!({ "id": id, "problem": why })).collect();
        json!({ "widgets": widgets, "problems": problems })
    }

    /// The program's state, as `program.status` reports it.
    pub fn status(&self) -> Value {
        let now = self.now();
        let inner = lock(&self.inner);
        let transition = match &inner.switch {
            Switch::Idle => Value::Null,
            Switch::Requested { scene, spec, .. } | Switch::Ready { scene, spec, .. } => {
                json!({ "to": scene, "kind": spec.kind.name(), "state": "starting" })
            }
            Switch::Running { scene, spec, started, .. } => json!({
                "to": scene,
                "kind": spec.kind.name(),
                "state": "running",
                "progress": started.map_or(0.0, |s| ((now - s) / spec.seconds().max(1e-6)).clamp(0.0, 1.0)),
            }),
        };
        json!({
            "scene": inner.scene,
            "transition": transition,
            "default_transition": inner.settings.transition,
            "widgets": inner.widgets.len(),
            "compose": inner.stats,
            "quality": inner.quality,
        })
    }
}

/// An [`EventSink`] that shows every event to the overlay and then hands it on: put it between the platform adapters
/// and the automation engine, so alerts and the chat box see what the show sees.
pub struct Tee {
    next: Arc<dyn EventSink>,
    overlay: Arc<ProgramService>,
}

impl std::fmt::Debug for Tee {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Tee")
    }
}

impl Tee {
    /// Shows events to `overlay`, then publishes them into `next`.
    pub fn new(next: Arc<dyn EventSink>, overlay: Arc<ProgramService>) -> Self {
        Self { next, overlay }
    }
}

impl EventSink for Tee {
    fn publish(&self, event: LiveEvent) -> Result<(), PublishError> {
        // The overlay sees it even when the show can't take it now: a full queue drops chat for the automations, not
        // for the chat box. A retried event is shown once, on its first try.
        let result = self.next.publish(event.clone());
        if !matches!(result, Err(PublishError::Full)) || event.kind.as_str() == "chat.message" {
            self.overlay.observe(&event);
        }
        result
    }

    fn producer_dropped(&self, count: u64) {
        self.next.producer_dropped(count);
    }
}
