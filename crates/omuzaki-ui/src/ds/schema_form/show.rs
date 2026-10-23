//! Drawing a [`Form`]: one ds control per field, chosen by [`Kind`].

use egui::{Id, Rect, RichText, Sense, Ui, WidgetInfo, WidgetType, vec2};

use super::model::{Entry, Form, IntControl, Kind, Layout, Level, MAX_SEGMENTS, Node, OPEN, Opt, TextRule};
use super::text;
use super::value::FormValue;
use crate::ds::a11y;
use crate::ds::icons;
use crate::ds::paint::{self, RingPlacement};
use crate::ds::strings::Msg;
use crate::ds::theme::{Ds, ds};
use crate::ds::tokens::{radius, size, space};
use crate::ds::widgets::{
    Button, Choice, ColourEvent, ColourField, CurveEditor, CurveEvent, Dropdown, FormRow, NumberField, ParamEvent,
    ParamSlider, Section, Segment, Segmented, TextField, Toggle, segmented,
};

/// Options a dropdown holds without allocating; a longer list costs one `Vec` per frame.
const INLINE_CHOICES: usize = 32;

/// One field of a form, as [`FormOutput::edit`] names it. [`Form::pointer`] gives its place in the value as a JSON
/// Pointer and [`Form::field_value`] its value.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FieldId(pub(super) usize);

/// Where an edit is in its gesture, so a host can make one gesture one transaction (`shell.md` section 7: "One
/// gesture is one transaction; Esc rolls it back").
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EditPhase {
    /// The value moved during a gesture that is still going on (a drag, a colour being picked, text being typed):
    /// apply it live, inside the gesture's transaction.
    Preview,
    /// The gesture ended, or a one-step edit happened (a click, a key, a reset): the value now is its result.
    Commit,
    /// The gesture was cancelled (Esc) and the value put back as it was before it.
    Cancel,
}

/// One field's edit this frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FieldEdit {
    /// The field edited.
    pub field: FieldId,
    /// Where the edit is in its gesture.
    pub phase: EditPhase,
}

/// What showing a form reported.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FormOutput {
    /// Some control changed the value this frame (including a drag in progress, a cancel and a reset).
    pub changed: bool,
    /// Every field that was edited this frame and where its gesture is, in the order a host must handle them. Two
    /// fields can report in one frame (clicking a toggle takes focus from a text field: the text field's gesture
    /// ends and the toggle changes), and each reports its own phase: none is merged into or replaces another. A
    /// gesture that ends comes before the edit that ended it, and the rest follow in the order the fields are drawn.
    /// Empty when nothing was edited. A frame without edits allocates nothing.
    pub edits: Vec<FieldEdit>,
}

/// A form that edits `value` according to `form`.
#[must_use = "a form does nothing until it is shown"]
pub struct SchemaForm<'a> {
    form: &'a Form,
    value: &'a mut FormValue,
    salt: Option<Id>,
}

/// What a form keeps from frame to frame, so a field that needs a buffer allocates nothing once it has one.
#[derive(Clone, Default)]
struct Scratch {
    /// Where messages with a placeholder are written ("Reset Width").
    words: String,
    /// The text of each exact integer field, by its id: what the person typed, which may not be a number yet.
    ints: Vec<IntText>,
}

/// An exact integer field's text, and the value it was last written from or parsed to.
#[derive(Clone)]
struct IntText {
    id: Id,
    text: String,
    value: i128,
    /// The field had keyboard focus last frame: the person is typing, so the text is theirs.
    focused: bool,
    /// The value when the field took focus: what Escape puts back.
    start: i128,
    /// Escape cancelled the gesture; the blur it causes has nothing left to end.
    cancelled: bool,
    /// A preview began a gesture that leaving the field has not ended yet.
    open: bool,
}

/// What one frame of a form carries down to its fields.
struct Cx<'a> {
    ds: &'a Ds,
    out: FormOutput,
    /// The ends of gestures reported this frame. They go before everything in `out.edits`: a gesture ends (a text
    /// field loses focus) because of the input that made the other edit, so it happened first.
    ends: Vec<FieldEdit>,
    scratch: Scratch,
}

impl Cx<'_> {
    /// Records `node`'s edit. Every report is kept: a field reports once per frame, and two fields in one frame are
    /// two edits.
    fn report(&mut self, node: &Node, phase: EditPhase) {
        self.out.changed = true;
        let edit = FieldEdit { field: FieldId(node.index), phase };
        if phase != EditPhase::Preview && has_gesture(&node.kind) {
            self.ends.push(edit);
        } else {
            self.out.edits.push(edit);
        }
    }
}

/// Whether the control of `kind` has gestures that last (a drag, a picker, text being typed), so that a `Commit` or
/// `Cancel` from it can be the end of one begun in an earlier frame. The rest are one step: the change is the edit.
const fn has_gesture(kind: &Kind) -> bool {
    match kind {
        Kind::Real { slider, .. } => *slider,
        Kind::Int { control, .. } => !matches!(control, IntControl::Number),
        Kind::Colour { .. } | Kind::Curve | Kind::Text(_) | Kind::Json => true,
        Kind::Choice(_) | Kind::Toggle | Kind::Section(_) | Kind::ReadOnly => false,
    }
}

/// Where the scratch is kept between frames.
const SCRATCH: &str = "ds.schema_form.scratch";

/// `msg` with its one placeholder `{key}` replaced by `value`, written into `buf`.
fn say<'b>(buf: &'b mut String, ds: &Ds, msg: Msg, key: &str, value: &str) -> &'b str {
    buf.clear();
    let pattern = ds.text(msg);
    let open = pattern.find('{');
    match open.and_then(|at| pattern[at + 1..].strip_prefix(key)?.strip_prefix('}').map(|rest| (at, rest))) {
        Some((at, rest)) => {
            buf.push_str(&pattern[..at]);
            buf.push_str(value);
            buf.push_str(rest);
        }
        None => buf.push_str(&pattern),
    }
    buf
}

impl<'a> SchemaForm<'a> {
    /// A form over `value`, which should come from `form` ([`Form::default_value`] or [`Form::value_from_json`]).
    pub const fn new(form: &'a Form, value: &'a mut FormValue) -> Self {
        Self { form, value, salt: None }
    }

    /// Names the form, so two forms in one `Ui` (the palette's arguments and the inspector's) keep separate state
    /// (focus, open sections, half-typed numbers). Use a value that names what the form edits.
    pub fn id_salt(mut self, salt: impl std::hash::Hash + std::fmt::Debug) -> Self {
        self.salt = Some(Id::new(salt));
        self
    }

    /// Shows the form and reports whether, and how, it changed the value.
    pub fn show(self, ui: &mut Ui) -> FormOutput {
        let ds = ds(ui.ctx());
        let Self { form, value, salt } = self;
        let salt = salt.unwrap_or_else(|| Id::new("ds.schema_form"));
        let scratch_id = Id::new(SCRATCH).with(salt);
        let scratch = ui.ctx().data_mut(|d| d.remove_temp::<Scratch>(scratch_id)).unwrap_or_default();
        let mut cx = Cx { ds: &ds, out: FormOutput::default(), ends: Vec::new(), scratch };
        ui.push_id(salt, |ui| match (&form.root.kind, value) {
            (Kind::Section(children), FormValue::Object(values)) => {
                let has_advanced = form.advanced.count > 0;
                if has_advanced {
                    top_level(ui, &mut cx, children, values, &form.essentials, true);
                    top_level(ui, &mut cx, children, values, &form.advanced, false);
                } else {
                    entries(ui, &mut cx, children, values, &form.essentials);
                }
            }
            (_, value) => field(ui, &mut cx, &form.root, value),
        });
        let Cx { mut out, mut ends, scratch, .. } = cx;
        if !ends.is_empty() {
            ends.append(&mut out.edits);
            out.edits = ends;
        }
        ui.ctx().data_mut(|d| d.insert_temp(scratch_id, scratch));
        out
    }
}

/// One of the two top-level sections.
fn top_level(
    ui: &mut Ui,
    cx: &mut Cx<'_>,
    children: &[Node],
    values: &mut [(String, FormValue)],
    layout: &Layout,
    essentials: bool,
) {
    if layout.count == 0 {
        return;
    }
    let ds = cx.ds;
    let (title, salt) = if essentials {
        (ds.text(Msg::FormEssentials), "essentials")
    } else {
        (ds.text(Msg::FormAdvanced), "advanced")
    };
    let mut section = Section::new(&title).default_open(essentials).id_salt(salt);
    let badge;
    if !essentials {
        badge = ds.text_with(Msg::FormMore, &[("count", &layout.count.to_string())]);
        section = section.badge(&badge);
    }
    section.show(ui, |ui| entries(ui, cx, children, values, layout));
}

fn entries(ui: &mut Ui, cx: &mut Cx<'_>, children: &[Node], values: &mut [(String, FormValue)], layout: &Layout) {
    for entry in &layout.entries {
        match entry {
            Entry::Field(i) => field_at(ui, cx, children, values, *i),
            Entry::Group { title, fields } => {
                Section::new(title).default_open(true).id_salt(("group", title)).show(ui, |ui| {
                    for i in fields {
                        field_at(ui, cx, children, values, *i);
                    }
                });
            }
        }
    }
}

/// Field `i`, or a read-only row when the value has nothing at that position (a value not made from this form).
fn field_at(ui: &mut Ui, cx: &mut Cx<'_>, children: &[Node], values: &mut [(String, FormValue)], i: usize) {
    let Some(node) = children.get(i) else {
        debug_assert!(false, "the layout names field {i} of {}", children.len());
        return;
    };
    match values.get_mut(i) {
        Some((_, value)) => field(ui, cx, node, value),
        None => read_only(ui, cx, node),
    }
}

/// The phase of a one-step edit: a commit when something changed.
fn one_step(changed: bool) -> Option<EditPhase> {
    changed.then_some(EditPhase::Commit)
}

/// What one row decided: where its edit is, and whether its reset was pressed.
#[derive(Default)]
struct RowOut {
    phase: Option<EditPhase>,
    reset: bool,
}

fn field(ui: &mut Ui, cx: &mut Cx<'_>, node: &Node, value: &mut FormValue) {
    if node.optional && *value == FormValue::Absent {
        return unset(ui, cx, node, value);
    }
    let modified = node.default.as_ref().is_some_and(|d| d != value);
    let mut out = RowOut::default();
    let ds = cx.ds;
    match (&node.kind, &mut *value) {
        (Kind::Real { min, max, slider }, FormValue::Number(n)) => {
            let range = min.unwrap_or(-OPEN)..=max.unwrap_or(OPEN);
            if *slider {
                out.phase = slider_over(ui, node, n, range);
            } else {
                row(ui, ds, &mut cx.scratch.words, node, modified, &mut out, |ui, name, _, out| {
                    let shown = number_field(NumberField::new(name, n, range), node).show(ui);
                    out.phase = one_step(shown.response.changed());
                    out.reset |= shown.reset;
                });
            }
        }
        (Kind::Int { min, max, control }, FormValue::Integer(n)) => {
            out = int_field(ui, cx, node, (*min, *max), *control, n, modified);
        }
        (Kind::Choice(opts), FormValue::Choice { index, value }) => {
            row(ui, ds, &mut cx.scratch.words, node, modified, &mut out, |ui, name, _, out| {
                if choice(ui, name, node, opts, index) {
                    if let Some(opt) = opts.get(*index) {
                        value.clone_from(&opt.value);
                    }
                    out.phase = Some(EditPhase::Commit);
                }
            });
        }
        (Kind::Toggle, FormValue::Bool(on)) => {
            // A toggle draws its own label, so there is no row; the reset follows it.
            let response = ui.add(Toggle::switch(&node.label, on).id_salt(&node.path));
            out.phase = one_step(response.changed());
            if node.default.is_some() {
                let at =
                    egui::pos2(response.rect.right() + space::S4 + size::MIN_TARGET / 2.0, response.rect.center().y);
                let words = &mut cx.scratch.words;
                reset_target(ui, ds, words, (&node.label, &node.path), at, modified, &mut out.reset);
            }
        }
        (Kind::Colour { alpha }, FormValue::Colour(colour)) => {
            row(ui, ds, &mut cx.scratch.words, node, modified, &mut out, |ui, name, _, out| {
                let shown = ColourField::new(name, colour).alpha(*alpha).id_salt(&node.path).show(ui);
                out.phase = match shown.event {
                    Some(ColourEvent::Preview(_)) => Some(EditPhase::Preview),
                    Some(ColourEvent::Commit { .. }) => Some(EditPhase::Commit),
                    Some(ColourEvent::Cancel(_)) => Some(EditPhase::Cancel),
                    _ => one_step(shown.response.changed()),
                };
            });
        }
        (Kind::Curve, FormValue::Curve(curve)) => {
            row(ui, ds, &mut cx.scratch.words, node, modified, &mut out, |ui, name, _, out| {
                let reason = text::say(ds, text::NO_TANGENTS, &[]);
                let shown = CurveEditor::new(name, curve).id_salt(&node.path).without_tangents(&reason).show(ui);
                out.phase = match shown.event {
                    Some(CurveEvent::Preview) => Some(EditPhase::Preview),
                    Some(CurveEvent::Commit { .. }) => Some(EditPhase::Commit),
                    Some(CurveEvent::Rollback) => Some(EditPhase::Cancel),
                    None => one_step(shown.response.changed()),
                };
            });
        }
        (Kind::Text(rule), FormValue::Text(text)) => {
            let checked = ui.id().with(("checked", node.path.as_str()));
            let pattern_ok = rule.pattern.as_ref().is_none_or(|p| pattern_holds(ui.ctx(), checked, p, text));
            let length_ok = rule.length_ok(text);
            row(ui, ds, &mut cx.scratch.words, node, modified, &mut out, |ui, name, words, out| {
                let mut field = TextField::new(name, text).id_salt(&node.path);
                if !length_ok {
                    field = field.error(say_length(words, ds, rule));
                } else if let (false, Some(pattern)) = (pattern_ok, &rule.pattern) {
                    field = field.error(say(words, ds, Msg::FormPatternError, "pattern", pattern.source()));
                }
                let response = ui.add(field);
                out.phase = text_phase(ui.ctx(), checked.with("original"), &response, text);
            });
        }
        (Kind::Json, FormValue::Json(json)) => json_field(ui, cx, node, json, modified, &mut out),
        (Kind::Section(children), FormValue::Object(values)) => {
            // A nested object the schema marks advanced starts folded; a top-level one is already under Advanced.
            let nested = node.path.matches('/').count() > 1;
            let open = !nested || node.level == Level::Essentials;
            Section::new(&node.label).default_open(open).id_salt(&node.path).show(ui, |ui| {
                for i in 0..children.len() {
                    field_at(ui, cx, children, values, i);
                }
            });
        }
        _ => read_only(ui, cx, node),
    }
    if let (true, Some(default)) = (out.reset, &node.default) {
        value.clone_from(default);
        out.phase = Some(EditPhase::Commit);
    }
    if let Some(phase) = out.phase {
        cx.report(node, phase);
    }
}

/// A text field's phase: Escape puts back the text the field had when it took focus and cancels the gesture; anything
/// else is [`typed`].
fn text_phase(ctx: &egui::Context, id: Id, response: &egui::Response, text: &mut String) -> Option<EditPhase> {
    let cancelled = id.with("cancelled");
    let open = id.with("open");
    if response.gained_focus() {
        ctx.data_mut(|d| {
            d.insert_temp(id, text.clone());
            d.remove_temp::<bool>(cancelled);
            d.remove_temp::<bool>(open);
        });
    }
    // The blur that follows an Escape ends nothing more: the gesture was cancelled.
    if response.lost_focus() && ctx.data_mut(|d| d.remove_temp::<bool>(cancelled)).is_some() {
        return None;
    }
    let escaped = ctx.input(|i| i.key_pressed(egui::Key::Escape)) && (response.has_focus() || response.lost_focus());
    if escaped {
        ctx.data_mut(|d| {
            d.insert_temp(cancelled, true);
            d.remove_temp::<bool>(open);
        });
        // The text field clears on Escape; the person asked to leave, not to delete.
        if let Some(original) = ctx.data(|d| d.get_temp::<String>(id)) {
            *text = original;
        }
        response.surrender_focus();
        return Some(EditPhase::Cancel);
    }
    typed(ctx, open, response)
}

/// A typed field's phase: every change is a preview, and leaving the field ends the gesture, once. egui reports a
/// lost focus in more than one frame (and in each pass of a frame that runs twice), so the end is reported only for
/// a gesture that is open: one a preview began (kept under `open`), or one this frame's keystroke began.
fn typed(ctx: &egui::Context, open: Id, response: &egui::Response) -> Option<EditPhase> {
    if response.lost_focus() {
        let was_open = ctx.data_mut(|d| d.remove_temp::<bool>(open)).is_some();
        return (was_open || response.changed()).then_some(EditPhase::Commit);
    }
    if response.changed() {
        ctx.data_mut(|d| d.insert_temp(open, true));
        return Some(EditPhase::Preview);
    }
    None
}

/// `field` with the `x-ui` keys of `node`.
fn number_field<'a>(mut field: NumberField<'a>, node: &'a Node) -> NumberField<'a> {
    field = field.unit(&node.unit).id_salt(&node.path);
    if let Some(step) = node.step {
        field = field.step(step);
    }
    if let Some(decimals) = node.decimals {
        field = field.decimals(decimals);
    }
    match default_f64(node) {
        Some(d) => field.default_value(d),
        None => field,
    }
}

/// A number field's default as the float its widget edits.
fn default_f64(node: &Node) -> Option<f64> {
    match node.default {
        Some(FormValue::Number(d)) => Some(d),
        #[allow(clippy::cast_precision_loss, reason = "only fields whose integers are exact as floats edit a float")]
        Some(FormValue::Integer(d)) => Some(d as f64),
        _ => None,
    }
}

/// A `ParamSlider` with the `x-ui` keys of `node`; it draws its own label, modified dot and reset.
fn slider_over(ui: &mut Ui, node: &Node, n: &mut f64, range: std::ops::RangeInclusive<f64>) -> Option<EditPhase> {
    let mut slider = ParamSlider::new(&node.label, n, range).unit(&node.unit).id_salt(&node.path);
    if let Some(d) = default_f64(node) {
        slider = slider.default_value(d);
    }
    if let Some(step) = node.step {
        slider = slider.step(step);
    }
    if let Some(decimals) = node.decimals {
        slider = slider.decimals(decimals);
    }
    let shown = slider.show(ui);
    let mut phase = None;
    for event in shown.events.iter() {
        let this = match event {
            ParamEvent::Begin(_) | ParamEvent::Preview(_) => EditPhase::Preview,
            ParamEvent::Commit { .. } => EditPhase::Commit,
            ParamEvent::Rollback(_) => EditPhase::Cancel,
            ParamEvent::ShowInHistory => continue,
        };
        if phase.is_none_or(|p| p == EditPhase::Preview) {
            phase = Some(this);
        }
    }
    phase.or_else(|| one_step(shown.response.changed()))
}

/// An integer field: a slider or number field over a float when every value it allows is exact as one, else text
/// parsed exactly.
fn int_field(
    ui: &mut Ui,
    cx: &mut Cx<'_>,
    node: &Node,
    (min, max): (Option<i128>, Option<i128>),
    control: IntControl,
    n: &mut i128,
    modified: bool,
) -> RowOut {
    let mut out = RowOut::default();
    let ds = cx.ds;
    if control == IntControl::Exact {
        let id = ui.id().with(("int", node.path.as_str()));
        let Scratch { words, ints } = &mut cx.scratch;
        let at = if let Some(at) = ints.iter().position(|t| t.id == id) {
            at
        } else {
            ints.push(IntText {
                id,
                text: n.to_string(),
                value: *n,
                focused: false,
                start: *n,
                cancelled: false,
                open: false,
            });
            ints.len() - 1
        };
        if let Some(entry) = ints.get_mut(at) {
            row(ui, ds, words, node, modified, &mut out, |ui, name, words, out| {
                exact_int(ui, ds, words, name, node, (min, max), entry, n, out);
            });
        }
        return out;
    }
    // The widgets edit a float: an untouched integer is never converted, an edited one is read back whole and kept
    // inside its bounds. Only fields whose every value is exact as a float come here.
    #[allow(clippy::cast_precision_loss, reason = "every value of this field is exact as a float")]
    let range = min.map_or(-OPEN, |m| m as f64)..=max.map_or(OPEN, |m| m as f64);
    #[allow(clippy::cast_precision_loss, reason = "every value of this field is exact as a float")]
    let mut edited = *n as f64;
    if control == IntControl::Slider {
        out.phase = slider_over(ui, node, &mut edited, range);
    } else {
        row(ui, ds, &mut cx.scratch.words, node, modified, &mut out, |ui, name, _, out| {
            let shown = number_field(NumberField::new(name, &mut edited, range), node).show(ui);
            out.phase = one_step(shown.response.changed());
            out.reset |= shown.reset;
        });
    }
    if out.phase.is_some() {
        #[allow(clippy::cast_possible_truncation, reason = "a float cast saturates, and the bounds clamp it")]
        let whole = edited.round() as i128;
        *n = whole.clamp(min.unwrap_or(i128::MIN), max.unwrap_or(i128::MAX));
    }
    out
}

/// Writes `n` into `text`, replacing what it held, without a new allocation once `text` has room.
fn write_int(text: &mut String, n: i128) {
    text.clear();
    // Writing into a `String` cannot fail.
    let _ = std::fmt::Write::write_fmt(text, format_args!("{n}"));
}

/// An integer typed as text: parsed as an integer, never through a float, and refused with a message when it is not
/// a whole number within the bounds. The value only ever changes to a number the field allows.
#[allow(clippy::too_many_arguments, reason = "one row's state, borrowed apart")]
fn exact_int(
    ui: &mut Ui,
    ds: &Ds,
    words: &mut String,
    name: &str,
    node: &Node,
    (min, max): (Option<i128>, Option<i128>),
    entry: &mut IntText,
    n: &mut i128,
    out: &mut RowOut,
) {
    let salt = ("int-field", node.path.as_str());
    // A value changed from outside (a reset, an undo, another view) is shown, unless the person is typing.
    if entry.value != *n && !entry.focused {
        write_int(&mut entry.text, *n);
        entry.value = *n;
    }
    // Up and Down step the value, as they do in a number field (Shift: ten steps), staying within the bounds.
    // Only a focused field takes the keys: stepping consumes them.
    let stepped = if entry.focused { arrow_step(ui, node, *n, (min, max)) } else { None };
    if let Some(stepped) = stepped {
        *n = stepped;
        write_int(&mut entry.text, stepped);
        entry.value = stepped;
        out.phase = Some(EditPhase::Commit);
    }
    let refusal = match entry.text.trim().parse::<i128>() {
        Err(_) => Some(text::say(ds, text::WHOLE, &[])),
        Ok(v) => match (min, max) {
            (Some(m), _) if v < m => Some(text::say(ds, text::AT_LEAST, &[("min", &m.to_string())])),
            (_, Some(m)) if v > m => Some(text::say(ds, text::AT_MOST, &[("max", &m.to_string())])),
            _ => None,
        },
    };
    let mut field = TextField::new(name, &mut entry.text).id_salt(salt).clearable(false);
    if let Some(refusal) = &refusal {
        words.clear();
        words.push_str(refusal);
        field = field.error(words);
    }
    let response = ui.add(field);
    if response.gained_focus() {
        entry.start = *n;
        entry.cancelled = false;
        entry.open = false;
    }
    if response.lost_focus() && std::mem::take(&mut entry.cancelled) {
        return;
    }
    let escaped =
        ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) && (response.has_focus() || response.lost_focus());
    entry.focused = response.has_focus() && !escaped;
    if escaped {
        entry.cancelled = true;
        entry.open = false;
        // The text field clears on Escape; the person asked to leave, not to delete.
        *n = entry.start;
        entry.value = entry.start;
        write_int(&mut entry.text, entry.start);
        response.surrender_focus();
        out.phase = Some(EditPhase::Cancel);
        return;
    }
    if !response.changed() && !response.lost_focus() {
        return;
    }
    // Leaving the field ends the gesture once, even though egui reports the lost focus in more than one frame.
    let leaving = response.lost_focus();
    let was_open = leaving && std::mem::take(&mut entry.open);
    match entry.text.trim().parse::<i128>() {
        Ok(v) if min.is_none_or(|m| v >= m) && max.is_none_or(|m| v <= m) => {
            entry.value = v;
            if *n != v {
                *n = v;
                out.phase = Some(EditPhase::Preview);
                entry.open = true;
            }
            if leaving && (was_open || out.phase.is_some()) {
                entry.open = false;
                out.phase = Some(EditPhase::Commit);
            }
        }
        // Text that is not an allowed number leaves the value alone; leaving the field puts the value's text back.
        // The value is the last valid preview, so that is what the gesture ends with.
        _ if leaving => {
            write_int(&mut entry.text, *n);
            entry.value = *n;
            out.phase = was_open.then_some(EditPhase::Commit);
        }
        _ => {}
    }
}

/// The value an Up or Down key press moves `n` to, if one was pressed and moved it.
fn arrow_step(ui: &Ui, node: &Node, n: i128, (min, max): (Option<i128>, Option<i128>)) -> Option<i128> {
    #[allow(clippy::cast_possible_truncation, reason = "a whole step of at least one, saturating")]
    let step = node.step.map_or(1, |s| s as i128).max(1);
    let (up, down, ten) = ui.input_mut(|i| {
        let shift = i.modifiers.shift_only();
        let mods = if shift { egui::Modifiers::SHIFT } else { egui::Modifiers::NONE };
        (i.consume_key(mods, egui::Key::ArrowUp), i.consume_key(mods, egui::Key::ArrowDown), shift)
    });
    let by = if ten { step.saturating_mul(10) } else { step };
    let moved = match (up, down) {
        (true, false) => n.saturating_add(by),
        (false, true) => n.saturating_sub(by),
        _ => return None,
    };
    let moved = moved.clamp(min.unwrap_or(i128::MIN), max.unwrap_or(i128::MAX));
    (moved != n).then_some(moved)
}

/// A value with no control of its own, typed as JSON; the form is held back while the text doesn't parse.
fn json_field(ui: &mut Ui, cx: &mut Cx<'_>, node: &Node, json: &mut String, modified: bool, out: &mut RowOut) {
    let ds = cx.ds;
    let checked = ui.id().with(("checked", node.path.as_str()));
    let parses = serde_json::from_str::<omuzaki_doc_api::Value>(json).is_ok();
    row(ui, ds, &mut cx.scratch.words, node, modified, out, |ui, name, _, out| {
        let error = if parses { std::borrow::Cow::Borrowed("") } else { text::say(ds, text::NOT_JSON, &[]) };
        let mut field = TextField::new(name, json).id_salt(&node.path);
        if !parses {
            field = field.error(&error);
        }
        let response = ui.add(field);
        out.phase = text_phase(ui.ctx(), checked.with("original"), &response, json);
    });
}

/// What the last pattern check of one field was, so the matcher runs when the text changes and not every frame.
#[derive(Clone)]
struct Checked {
    pattern: String,
    text: String,
    holds: bool,
}

fn pattern_holds(ctx: &egui::Context, id: Id, pattern: &super::pattern::Pattern, text: &str) -> bool {
    ctx.data_mut(|d| {
        let check = d.get_temp_mut_or_insert_with(id, || Checked {
            pattern: pattern.source().to_owned(),
            text: text.to_owned(),
            holds: pattern.is_match(text),
        });
        if check.text != text || check.pattern != pattern.source() {
            check.pattern.clear();
            check.pattern.push_str(pattern.source());
            check.text.clear();
            check.text.push_str(text);
            check.holds = pattern.is_match(text);
        }
        check.holds
    })
}

/// What a text field's length needs, written into `buf`.
fn say_length<'b>(buf: &'b mut String, ds: &Ds, rule: &TextRule) -> &'b str {
    let (min, max) = (rule.min_len.to_string(), rule.max_len.map(|m| m.to_string()));
    let text = match (rule.min_len, &max) {
        (0, Some(max)) => ds.text_with(Msg::FormLengthMax, &[("max", max)]),
        (_, Some(max)) => ds.text_with(Msg::FormLengthBetween, &[("min", &min), ("max", max)]),
        (_, None) => ds.text_with(Msg::FormLengthMin, &[("min", &min)]),
    };
    buf.clear();
    buf.push_str(&text);
    buf
}

/// A segmented control for two to five options whose labels fit whole, a dropdown for the rest. True when the choice
/// changed.
fn choice(ui: &mut Ui, name: &str, node: &Node, opts: &[Opt], index: &mut usize) -> bool {
    let len = opts.len();
    if (2..=MAX_SEGMENTS).contains(&len) {
        let segments: [Segment<'_, usize>; MAX_SEGMENTS] = std::array::from_fn(|i| {
            let at = i.min(len - 1);
            Segment::new(at, opts.get(at).map_or("", |o| o.label.as_str()))
        });
        // Cut labels ("n…", "h…") make a person hover each one to read it: a dropdown names them all.
        if segmented::fits(ui.ctx(), &segments[..len], ui.available_width()) {
            return ui.add(Segmented::new(name, index, &segments[..len]).id_salt(&node.path)).changed();
        }
    }
    if len <= INLINE_CHOICES {
        let choices: [Choice<'_, usize>; INLINE_CHOICES] = std::array::from_fn(|i| {
            let at = i.min(len.saturating_sub(1));
            Choice::new(at, opts.get(at).map_or("", |o| o.label.as_str()))
        });
        return ui.add(Dropdown::new(name, index, &choices[..len]).id_salt(&node.path).stretch()).changed();
    }
    let choices: Vec<Choice<'_, usize>> = opts.iter().enumerate().map(|(i, o)| Choice::new(i, &o.label)).collect();
    ui.add(Dropdown::new(name, index, &choices).id_salt(&node.path).stretch()).changed()
}

/// A labelled row: the field's label, its control and, for a field with a default, the reset after it, whose space
/// is kept while it is hidden so the control never moves when the value leaves its default (`shell.md` section 7).
fn row(
    ui: &mut Ui,
    ds: &Ds,
    words: &mut String,
    node: &Node,
    modified: bool,
    out: &mut RowOut,
    control: impl FnOnce(&mut Ui, &str, &mut String, &mut RowOut),
) {
    FormRow::new(&node.label).modified(modified).show(ui, |ui, name| {
        if node.default.is_none() {
            control(ui, name, words, out);
            return;
        }
        // The control gets the row less the reset's column; the reset is placed by hand, not in a child `Ui` (which
        // would allocate every frame).
        let full = ui.available_rect_before_wrap();
        ui.set_max_width((full.width() - size::MIN_TARGET - space::S4).max(0.0));
        let top = ui.cursor().top();
        control(ui, name, words, out);
        let centre_y = top.midpoint(ui.min_rect().bottom());
        let at = egui::pos2(full.right() - size::MIN_TARGET / 2.0, centre_y);
        reset_target(ui, ds, words, (name, &node.path), at, modified, &mut out.reset);
    });
}

/// The 24 pt reset target centred on `at`: drawn, focusable and named ("Reset Width") only while the value is away
/// from its default; its space is kept otherwise, so nothing moves when it appears.
fn reset_target(
    ui: &mut Ui,
    ds: &Ds,
    words: &mut String,
    (label, path): (&str, &str),
    at: egui::Pos2,
    modified: bool,
    reset: &mut bool,
) {
    let rect = Rect::from_center_size(at, vec2(size::MIN_TARGET, size::MIN_TARGET));
    ui.expand_to_include_rect(rect);
    if !modified {
        return;
    }
    let id = ui.id().with(("reset", path));
    let response = ui.interact(rect, id, Sense::click());
    let t = &ds.theme;
    let hover = paint::fade(ui.ctx(), ds, id, paint::hot(&response));
    let box_rect = paint::square(rect.center(), 22.0);
    if hover > 0.0 {
        ui.painter().rect_filled(box_rect, radius::CONTROL, t.bg3.gamma_multiply(hover));
    }
    let ink = paint::lerp(t.text3, t.text, hover);
    paint::icon(ui.painter(), ds, rect.center(), icons::ARROW_COUNTER_CLOCKWISE, 14.0, ink, false);
    paint::focus_ring(ui.painter(), t, &response, box_rect, radius::CONTROL, RingPlacement::Outside);
    let name = say(words, ds, Msg::ParamReset, "label", label);
    a11y::describe(&response, || WidgetInfo::labeled(WidgetType::Button, true, name), |_| {});
    *reset |= response.clicked();
}

/// An optional field with no value: its label and a Set button, which gives it its natural value.
fn unset(ui: &mut Ui, cx: &mut Cx<'_>, node: &Node, value: &mut FormValue) {
    let ds = cx.ds;
    let mut set = false;
    FormRow::new(&node.label).show(ui, |ui, name| {
        let label = text::say(ds, text::SET, &[]);
        let named = text::say(ds, text::SET_FIELD, &[("label", name)]);
        set = ui.add(Button::new(&label).ghost().icon(icons::PLUS).accessible_label(&named)).clicked();
    });
    if set {
        *value = node.natural_value();
        cx.report(node, EditPhase::Commit);
    }
}

/// A shape no control handles, or a value that does not fit its field: say so, and leave the value alone.
fn read_only(ui: &mut Ui, cx: &mut Cx<'_>, node: &Node) {
    let ds = cx.ds;
    let text = say(&mut cx.scratch.words, ds, Msg::FormUnsupported, "type", &node.type_name);
    FormRow::new(&node.label).show(ui, |ui, name| {
        let response = ui.label(RichText::new(text).font(ds.text.body.clone()).color(ds.theme.text3));
        a11y::describe(&response, || WidgetInfo::labeled(WidgetType::Label, true, format!("{name}: {text}")), |_| {});
    });
}
