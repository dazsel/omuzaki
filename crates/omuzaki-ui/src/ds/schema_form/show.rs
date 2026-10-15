//! Drawing a [`Form`]: one ds control per field, chosen by [`Kind`].

use egui::{Align, Id, RichText, Sense, Ui, UiBuilder, WidgetInfo, WidgetType};

use super::model::{Entry, Form, Kind, Layout, MAX_SEGMENTS, Node, Opt, UNBOUNDED};
use super::value::FormValue;
use crate::ds::a11y;
use crate::ds::icons;
use crate::ds::strings::Msg;
use crate::ds::theme::{Ds, ds};
use crate::ds::widgets::{
    Button, Choice, ColourEvent, ColourField, CurveEditor, CurveEvent, Dropdown, FormRow, NumberField, ParamSlider,
    Section, Segment, Segmented, TextField, Toggle,
};

/// Options a dropdown holds without allocating; a longer list costs one `Vec` per frame.
const INLINE_CHOICES: usize = 32;

/// What showing a form reported.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FormOutput {
    /// Some control changed the value this frame (including a drag in progress and a reset).
    pub changed: bool,
}

/// A form that edits `value` according to `form`.
#[must_use = "a form does nothing until it is shown"]
pub struct SchemaForm<'a> {
    form: &'a Form,
    value: &'a mut FormValue,
    salt: Option<Id>,
}

/// What one frame of a form carries down to its fields.
struct Cx<'a> {
    ds: &'a Ds,
    changed: bool,
    /// Where messages with a placeholder are written ("Reset Width"), so a field that needs one allocates nothing:
    /// the buffer lives from frame to frame.
    words: String,
}

/// Where the buffer is kept between frames.
const WORDS: &str = "ds.schema_form.words";

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

    /// Shows the form and reports whether it changed the value.
    pub fn show(self, ui: &mut Ui) -> FormOutput {
        let ds = ds(ui.ctx());
        let words_id = Id::new(WORDS);
        let words = ui.ctx().data_mut(|d| d.remove_temp::<String>(words_id)).unwrap_or_default();
        let mut cx = Cx { ds: &ds, changed: false, words };
        let Self { form, value, salt } = self;
        ui.push_id(salt.unwrap_or_else(|| Id::new("ds.schema_form")), |ui| match (&form.root.kind, value) {
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
        ui.ctx().data_mut(|d| d.insert_temp(words_id, cx.words));
        FormOutput { changed: cx.changed }
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

fn field(ui: &mut Ui, cx: &mut Cx<'_>, node: &Node, value: &mut FormValue) {
    let modified = node.default.as_ref().is_some_and(|d| d != value);
    let mut reset = false;
    // Borrowed apart, so a control's closure can write `changed` while the row holds the buffer.
    let Cx { ds, changed, words } = &mut *cx;
    let ds: &Ds = ds;
    match (&node.kind, &mut *value) {
        (Kind::Slider { min, max, integer }, FormValue::Number(n)) => {
            *changed |= slider(ui, node, n, *min..=*max, *integer);
        }
        (Kind::Number { integer }, FormValue::Number(n)) => {
            row(ui, ds, words, node, modified, &mut reset, |ui, name, _| {
                *changed |=
                    NumberField::new(name, n, -UNBOUNDED..=UNBOUNDED).id_salt(&node.path).show(ui).response.changed();
                if *integer {
                    *n = n.round();
                }
            });
        }
        (Kind::Choice(opts), FormValue::Choice { index, value }) => {
            row(ui, ds, words, node, modified, &mut reset, |ui, name, _| {
                if choice(ui, name, node, opts, index) {
                    if let Some(opt) = opts.get(*index) {
                        value.clone_from(&opt.value);
                    }
                    *changed = true;
                }
            });
        }
        (Kind::Toggle, FormValue::Bool(on)) => {
            // A toggle draws its own label, so there is no row; the reset sits after it.
            let toggle = Toggle::switch(&node.label, on).id_salt(&node.path);
            *changed |= if modified {
                beside(ui, |ui| {
                    let changed = ui.add(toggle).changed();
                    reset_button(ui, ds, words, &node.label, &mut reset);
                    changed
                })
            } else {
                ui.add(toggle).changed()
            };
        }
        (Kind::Colour { alpha }, FormValue::Colour(colour)) => {
            row(ui, ds, words, node, modified, &mut reset, |ui, name, _| {
                let out = ColourField::new(name, colour).alpha(*alpha).id_salt(&node.path).show(ui);
                let edited = matches!(
                    out.event,
                    Some(ColourEvent::Preview(_) | ColourEvent::Commit { .. } | ColourEvent::Cancel(_))
                );
                *changed |= out.response.changed() || edited;
            });
        }
        (Kind::Curve, FormValue::Curve(curve)) => {
            row(ui, ds, words, node, modified, &mut reset, |ui, name, _| {
                let out = CurveEditor::new(name, curve).id_salt(&node.path).show(ui);
                let edited =
                    matches!(out.event, Some(CurveEvent::Preview | CurveEvent::Commit { .. } | CurveEvent::Rollback));
                *changed |= out.response.changed() || edited;
            });
        }
        (Kind::Text(pattern), FormValue::Text(text)) => {
            row(ui, ds, words, node, modified, &mut reset, |ui, name, words| {
                let broken = pattern.as_ref().filter(|p| !p.is_match(text));
                let mut field = TextField::new(name, text).id_salt(&node.path);
                if let Some(pattern) = broken {
                    field = field.error(say(words, ds, Msg::FormPatternError, "pattern", pattern.source()));
                }
                *changed |= ui.add(field).changed();
            });
        }
        (Kind::Section(children), FormValue::Object(values)) => {
            Section::new(&node.label).default_open(true).id_salt(&node.path).show(ui, |ui| {
                for i in 0..children.len() {
                    field_at(ui, cx, children, values, i);
                }
            });
        }
        _ => read_only(ui, cx, node),
    }
    if let (true, Some(default)) = (reset, &node.default) {
        value.clone_from(default);
        cx.changed = true;
    }
}

fn slider(ui: &mut Ui, node: &Node, n: &mut f64, range: std::ops::RangeInclusive<f64>, integer: bool) -> bool {
    let mut slider = ParamSlider::new(&node.label, n, range).unit(&node.unit).id_salt(&node.path);
    if let Some(FormValue::Number(default)) = node.default {
        slider = slider.default_value(default);
    }
    // A whole-number field steps by one and shows no decimals unless the schema says otherwise.
    if let Some(step) = node.step.or(integer.then_some(1.0)) {
        slider = slider.step(step);
    }
    if let Some(decimals) = node.decimals.or(integer.then_some(0)) {
        slider = slider.decimals(decimals);
    }
    let changed = slider.show(ui).response.changed();
    if integer {
        *n = n.round();
    }
    changed
}

/// A segmented control for two to five options, a dropdown for the rest. True when the choice changed.
fn choice(ui: &mut Ui, name: &str, node: &Node, opts: &[Opt], index: &mut usize) -> bool {
    let len = opts.len();
    if (2..=MAX_SEGMENTS).contains(&len) {
        let segments: [Segment<'_, usize>; MAX_SEGMENTS] = std::array::from_fn(|i| {
            let at = i.min(len - 1);
            Segment::new(at, opts.get(at).map_or("", |o| o.label.as_str()))
        });
        return ui.add(Segmented::new(name, index, &segments[..len]).id_salt(&node.path)).changed();
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

/// A labelled row; when the field is away from its default the control is followed by a reset button.
fn row<R>(
    ui: &mut Ui,
    ds: &Ds,
    words: &mut String,
    node: &Node,
    modified: bool,
    reset: &mut bool,
    control: impl FnOnce(&mut Ui, &str, &mut String) -> R,
) -> R {
    FormRow::new(&node.label)
        .modified(modified)
        .show(ui, |ui, name| {
            if modified {
                beside(ui, |ui| {
                    let out = control(ui, name, words);
                    reset_button(ui, ds, words, name, reset);
                    out
                })
            } else {
                control(ui, name, words)
            }
        })
        .inner
}

/// `add` in a left-to-right row of the room that is left. Not `Ui::horizontal`, which boxes its closure: a form is drawn
/// every frame.
fn beside<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let builder =
        UiBuilder::new().max_rect(ui.available_rect_before_wrap()).layout(egui::Layout::left_to_right(Align::Center));
    let mut row = ui.new_child(builder);
    let out = add(&mut row);
    ui.allocate_rect(row.min_rect(), Sense::hover());
    out
}

fn reset_button(ui: &mut Ui, ds: &Ds, words: &mut String, label: &str, reset: &mut bool) {
    let text = ds.text(Msg::ParamResetDefault);
    let name = say(words, ds, Msg::ParamReset, "label", label);
    *reset |= ui.add(Button::new(&text).ghost().icon(icons::ARROW_COUNTER_CLOCKWISE).accessible_label(name)).clicked();
}

/// A shape no control handles, or a value that does not fit its field: say so, and leave the value alone.
fn read_only(ui: &mut Ui, cx: &mut Cx<'_>, node: &Node) {
    let Cx { ds, words, .. } = cx;
    let ds: &Ds = ds;
    let text = say(words, ds, Msg::FormUnsupported, "type", &node.type_name);
    FormRow::new(&node.label).show(ui, |ui, name| {
        let response = ui.label(RichText::new(text).font(ds.text.body.clone()).color(ds.theme.text3));
        a11y::describe(&response, || WidgetInfo::labeled(WidgetType::Label, true, format!("{name}: {text}")), |_| {});
    });
}
