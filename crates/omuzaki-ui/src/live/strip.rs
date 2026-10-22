//! The expression strip along the foot of the Perform workspace's stage (`docs/design/ux/shell.md` section 12, element
//! 4): one button per bound expression, with its key, lit while the expression shows. A click presses the key through
//! `trigger.press`, the op the keyboard and an agent use, so the strip, a hotkey and an agent all do the same thing.
//!
//! The bindings are read through `trigger.list` twice a second and after each press; with no trigger ops (a build
//! without the performer) or nothing bound, the strip draws nothing and the stage is left clear.

use egui::{Align, Layout, Rect, Ui, UiBuilder, vec2};
use omuzaki_ops_api::{Actor, Runner};
use omuzaki_stage::perform::triggers::TriggerInfo;
use serde::Deserialize as _;
use serde_json::Value;

use super::model::{OP_TRIGGER_LIST, OP_TRIGGER_PRESS, object, run};
use super::panel::MOODS;
use super::text::{Word, say, text};
use crate::ds::widgets::Button;

/// How often the bindings are read again, in seconds.
const READ_EVERY: f64 = 0.5;
/// The strip's height, and its gap from the stage's foot.
const HEIGHT: f32 = 32.0;
const MARGIN: f32 = 12.0;

/// The strip's state: the bindings last read, and when.
#[derive(Debug, Default)]
pub struct ExpressionStrip {
    triggers: Vec<TriggerInfo>,
    read_at: Option<f64>,
}

impl ExpressionStrip {
    /// An empty strip; it reads the bindings on its first frame.
    pub fn new() -> Self {
        Self::default()
    }

    /// The bindings the strip last read.
    pub fn triggers(&self) -> &[TriggerInfo] {
        &self.triggers
    }

    /// Draws the strip along the foot of `stage`, on top of the picture.
    pub fn ui(&mut self, ui: &mut Ui, stage: Rect, runner: &dyn Runner, actor: &Actor) {
        let now = ui.input(|i| i.time);
        if self.read_at.is_none_or(|at| now - at >= READ_EVERY) {
            self.read(runner, actor);
            self.read_at = Some(now);
        }
        if self.triggers.is_empty() {
            return;
        }
        let ctx = ui.ctx().clone();
        let strip = Rect::from_min_max(
            stage.left_bottom() + vec2(MARGIN, -(HEIGHT + MARGIN)),
            stage.right_bottom() - vec2(MARGIN, MARGIN),
        );
        let mut pressed = None;
        ui.scope_builder(UiBuilder::new().max_rect(strip).layout(Layout::left_to_right(Align::Center)), |ui| {
            for info in &self.triggers {
                let id = info.binding.expression.as_str();
                let name = MOODS
                    .iter()
                    .find(|(mood, _)| *mood == id)
                    .map_or_else(|| id.to_owned(), |(_, word)| text(&ctx, *word).into_owned());
                let key = info.binding.key.as_str();
                let shown = say(&ctx, Word::StripItem, &[("name", &name), ("key", key)]);
                let label = say(&ctx, Word::StripPress, &[("name", &name), ("key", key)]);
                let button = Button::new(&shown).accessible_label(&label);
                let button = if info.active { button.primary() } else { button };
                let missing = text(&ctx, Word::HotkeyMissing);
                let button = if info.missing { button.disabled(&missing) } else { button };
                if ui.add(button).clicked() {
                    pressed = Some(info.binding.key.clone());
                }
            }
        });
        if let Some(key) = pressed {
            for down in [true, false] {
                let args = object(&[("key", Value::from(key.as_str())), ("down", Value::from(down))]);
                if let Err(error) = run(runner, actor, OP_TRIGGER_PRESS, args) {
                    tracing::warn!(%error, "the expression strip's press failed");
                }
            }
            self.read(runner, actor);
        }
    }

    /// Reads `trigger.list`; a build without it, or a failure, leaves the strip empty.
    fn read(&mut self, runner: &dyn Runner, actor: &Actor) {
        if !runner.ops().iter().any(|op| op.name == OP_TRIGGER_LIST) {
            self.triggers.clear();
            return;
        }
        let read = run(runner, actor, OP_TRIGGER_LIST, object(&[])).ok().and_then(|mut output| {
            let triggers = output.get_mut("triggers").map(Value::take).unwrap_or_default();
            Vec::<TriggerInfo>::deserialize(triggers).ok()
        });
        self.triggers = read.unwrap_or_default();
    }
}
