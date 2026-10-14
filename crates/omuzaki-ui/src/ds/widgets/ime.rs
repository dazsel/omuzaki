//! IME composition for the widgets that take text (SE-02).
//!
//! egui reports a composition as `Event::Ime` events: a non-empty `Preedit` shows or updates the composing text, a
//! `Commit` ends it with the final text, an empty `Preedit` or `Commit` drops it. egui's own `TextEdit` writes the
//! preedit into the text it edits, which puts half-typed text into the owner's value and into the undo history. So
//! the widgets here never hand `Event::Ime` to the `TextEdit`:
//!
//! - [`begin`] reads the frame's IME events first and takes them out. A commit becomes an ordinary `Event::Text`, so
//!   the committed text goes in through the normal edit path (selection replaced, undo step, change reported).
//! - The preedit is kept beside the text, never in it, and drawn by a layouter ([`Ime::layouter`]) with an underline.
//! - A preedit that starts over a selection is drawn in place of that selection; the text is untouched, a commit
//!   replaces the selection (one undo step), a cancel shows it again.
//!
//! **An external reset cancels the composition.** When the owner replaces the widget's text (a clear button, a reset), the
//! text at the start of the next frame is not the one the widget last published. That is checked before any IME event
//! is processed, and ends the composition outright: the preedit and the selection it stood in for are dropped, nothing
//! of the old text is ever put back, and a later commit goes in at the caret like any text with no composition.
//!
//! **The cancellation rule is an allowlist.** The OS IME has already consumed every key it wanted, so any event that
//! still reaches the widget while it composes is the person editing or moving. Every key press, text, paste, cut and
//! pointer press therefore ends the composition first and then runs normally. The exceptions are Enter and Escape
//! (they stay with the IME), and events that change nothing (modifier state, key releases). No shortcut is named
//! anywhere, so shortcuts egui adds later are covered too.

use std::cell::Cell;
use std::sync::Arc;

use egui::text::{CCursor, CCursorRange, LayoutJob, TextFormat};
use egui::{Align, Color32, Context, Event, FontId, Galley, Id, ImeEvent, Key, Stroke, TextBuffer, TextEdit, Ui, Vec2};

/// What a widget remembers of a running composition between frames.
#[derive(Clone)]
struct Composition {
    /// The composing text, never part of the widget's value.
    preedit: String,
    /// The editor's selection when the composition began. The composition ends when the selection is anything else.
    /// `None` when text was inserted earlier in the frame it began: the caret is known only after that frame.
    range: Option<CCursorRange>,
    /// The widget's text as the last frame left it: a different text now was put there by the owner (a reset, a clear
    /// button), which ends the composition.
    seen: String,
}

impl Composition {
    /// The characters the preedit stands in for, as `from..to`: the selection it began over, or an empty span at the
    /// caret. Both are the end of the text while the caret is not known yet.
    fn span(&self) -> (usize, usize) {
        self.range.map_or((usize::MAX, usize::MAX), |r| {
            (r.primary.index.0.min(r.secondary.index.0), r.primary.index.0.max(r.secondary.index.0))
        })
    }

    /// Whether the composition began over a selection, which the editor's own state does not show meanwhile.
    fn over_selection(&self) -> bool {
        let (from, to) = self.span();
        from != to
    }

    /// The selection the editor's state holds while this composition runs: the caret at the start of what the preedit
    /// replaces (so the editor never paints a selection that the galley does not have).
    fn held(&self) -> Option<CCursorRange> {
        self.range.map(|_| CCursorRange::one(CCursor::new(self.span().0)))
    }
}

/// A composition as one widget's frame sees it. Create it with [`begin`] before the `TextEdit` is shown.
pub(crate) struct Ime {
    composition: Option<Composition>,
    /// Where the layouter put the end of the preedit and the editor's own caret, in galley coordinates: the IME cursor
    /// rectangle the editor reports is at its caret, and is moved to the end of the preedit by the difference.
    preedit_end_x: Cell<f32>,
    caret_x: Cell<f32>,
}

/// Takes the frame's IME events out of the input for the widget with edit id `id` (whose text is `value`), and returns
/// its composition state.
/// Call it once per frame, before showing the `TextEdit`, and hand the result to [`Ime::finish`] after.
pub(crate) fn begin(ctx: &Context, id: Id, value: &str) -> Ime {
    let key = id.with("composition");
    let stored = ctx.data(|d| d.get_temp::<Composition>(key));
    let focused = ctx.memory(|m| m.has_focus(id));
    // The platform drops a composition when focus moves, and so does the widget.
    let mut composition = if focused { stored } else { None };
    let mut interrupt = false;
    let caret = TextEdit::load_state(ctx, id).and_then(|state| state.cursor.char_range());
    // A composition that began in a frame that also inserted text takes the caret as it is now.
    if let Some(c) = composition.as_mut().filter(|c| c.range.is_none()) {
        c.range = caret;
    }
    let moved = |c: &Composition| caret.zip(c.held()).is_some_and(|(r, held)| !same_range(r, held));
    // The editor's state to set before it runs: the selection back (a composition over it ended), or the caret held at
    // the start of the selection a composition replaces.
    let mut set_state: Option<CCursorRange> = None;
    if composition.is_none() && stored_over_selection(ctx, key, focused) {
        // The widget lost focus mid-composition: the selection comes back.
        set_state = stored_range(ctx, key);
    }
    // A text that is not the one the last frame left was replaced by the owner: the composition is over, and the
    // selection it stood in for is not the owner's to get back.
    if composition.as_ref().is_some_and(|c| c.seen != value) {
        composition = None;
        interrupt = true;
    }
    // A caret that is somewhere else than where the composition began (a click, a move) ends it first.
    if composition.as_ref().is_some_and(moved) {
        composition = None;
        interrupt = true;
    }
    if focused {
        ctx.input_mut(|i| {
            let events = std::mem::take(&mut i.events);
            let mut ime_turn = false;
            let mut inserted = false;
            for event in events {
                match event {
                    Event::Ime(ime) => match ime {
                        ImeEvent::Preedit { text, .. } if !text.is_empty() => {
                            // Text committed earlier in this frame moves the caret: it is read after the frame.
                            let range = if inserted { None } else { composition.as_ref().map_or(caret, |c| c.range) };
                            let started = composition.is_none();
                            composition = Some(Composition { preedit: text, range, seen: String::new() });
                            if started && let Some(held) = composition.as_ref().filter(|c| c.over_selection()) {
                                set_state = held.held();
                            }
                            ime_turn = true;
                        }
                        // Empty notifications with no composition running are noise some platforms repeat.
                        ImeEvent::Preedit { text, .. } | ImeEvent::Commit(text)
                            if text.is_empty() && composition.is_none() => {}
                        ImeEvent::Commit(text) if !text.is_empty() => {
                            end(&mut composition, &mut set_state);
                            ime_turn = true;
                            inserted = true;
                            i.events.push(Event::Text(text));
                        }
                        ImeEvent::Preedit { .. } | ImeEvent::Commit(_) => {
                            end(&mut composition, &mut set_state);
                            ime_turn = true;
                        }
                        ImeEvent::DeleteSurrounding { .. } => i.events.push(Event::Ime(ime)),
                        #[expect(deprecated, reason = "egui no longer sends them; named so the match stays exhaustive")]
                        ImeEvent::Enabled | ImeEvent::Disabled => {}
                    },
                    // The key that confirms a candidate or cancels one is the IME's, in the frame of the commit too.
                    Event::Key { key: Key::Enter | Key::Escape, .. } if composition.is_some() || ime_turn => {}
                    // Everything else that edits or moves ends the composition before it takes effect.
                    Event::Text(_)
                    | Event::Paste(_)
                    | Event::Cut
                    | Event::Key { pressed: true, .. }
                    | Event::PointerButton { pressed: true, .. }
                        if composition.is_some() =>
                    {
                        end(&mut composition, &mut set_state);
                        interrupt = true;
                        i.events.push(event);
                    }
                    other => {
                        // Typing or a key before a preedit in this frame has already used up or moved the selection.
                        inserted |= matches!(
                            other,
                            Event::Text(_) | Event::Paste(_) | Event::Cut | Event::Key { pressed: true, .. }
                        );
                        i.events.push(other);
                    }
                }
            }
        });
    }
    if let Some(range) = set_state
        && let Some(mut state) = TextEdit::load_state(ctx, id)
    {
        state.cursor.set_char_range(Some(range));
        TextEdit::store_state(ctx, id, state);
    }
    if interrupt {
        ctx.memory_mut(egui::Memory::interrupt_ime);
    }
    Ime { composition, preedit_end_x: Cell::new(0.0), caret_x: Cell::new(0.0) }
}

fn same_range(a: CCursorRange, b: CCursorRange) -> bool {
    a.primary.index == b.primary.index && a.secondary.index == b.secondary.index
}

/// Ends the composition. When it stood in for a selection, that selection is put back in the editor's state.
fn end(composition: &mut Option<Composition>, set_state: &mut Option<CCursorRange>) {
    if let Some(c) = composition.take()
        && c.over_selection()
    {
        *set_state = c.range;
    }
}

/// The selection a stored composition stood in for, when the widget is no longer focused and so ends it.
fn stored_over_selection(ctx: &Context, key: Id, focused: bool) -> bool {
    !focused && ctx.data(|d| d.get_temp::<Composition>(key)).is_some_and(|c| c.over_selection())
}

fn stored_range(ctx: &Context, key: Id) -> Option<CCursorRange> {
    ctx.data(|d| d.get_temp::<Composition>(key)).and_then(|c| c.range)
}

impl Ime {
    /// True while a preedit shows.
    pub(crate) const fn composing(&self) -> bool {
        self.composition.is_some()
    }

    /// A layouter that lays `text` out with the preedit underlined at the caret. Pass it to `TextEdit::layouter` only
    /// while [`composing`](Self::composing); `align` and the fonts are the ones the `TextEdit` would have used.
    pub(crate) fn layouter(
        &self,
        font: FontId,
        colour: Color32,
        align: Align,
    ) -> impl FnMut(&Ui, &dyn TextBuffer, f32) -> Arc<Galley> + '_ {
        move |ui, text, _wrap_width| {
            let text = text.as_str();
            let len = text.chars().count();
            let (preedit, at, to) = self.composition.as_ref().map_or(("", 0, 0), |c| {
                let (from, to) = c.span();
                (c.preedit.as_str(), from.min(len), to.min(len))
            });
            let byte = |chars: usize| text.char_indices().nth(chars).map_or(text.len(), |(i, _)| i);
            let (head, tail) = (&text[..byte(at)], &text[byte(to)..]);
            let line_height = ui.fonts_mut(|f| f.row_height(&font)) + ui.spacing().extra_text_line_spacing;
            let plain = TextFormat {
                font_id: font.clone(),
                color: colour,
                line_height: Some(line_height),
                ..TextFormat::default()
            };
            let composing = TextFormat { underline: Stroke::new(1.0, colour), ..plain.clone() };
            let mut job = LayoutJob::default();
            job.append(head, 0.0, plain.clone());
            job.append(preedit, 0.0, composing);
            job.append(tail, 0.0, plain);
            job.halign = align;
            job.keep_trailing_whitespace = true;
            let galley = ui.fonts_mut(|f| f.layout_job(job));
            // The editor's caret is held at the start of what the preedit replaces, so its IME rectangle is at the
            // start of the painted preedit; the end of it is read off this same galley.
            let plen = preedit.chars().count();
            self.caret_x.set(galley.pos_from_cursor(CCursor::new(at)).min.x);
            self.preedit_end_x.set(galley.pos_from_cursor(CCursor::new(at + plen)).min.x);
            galley
        }
    }

    /// Settles the frame after the `TextEdit` was shown: remembers the composition, moves the IME cursor rectangle to
    /// the end of the preedit (where the platform puts its candidate window), and lets screen readers hear the
    /// composing text with the value.
    pub(crate) fn finish(&self, ctx: &Context, id: Id, value: &str) {
        let key = id.with("composition");
        let Some(mut composition) = self.composition.clone() else {
            if ctx.data(|d| d.get_temp::<Composition>(key)).is_some() {
                ctx.data_mut(|d| d.remove::<Composition>(key));
            }
            return;
        };
        ctx.output_mut(|o| {
            if let Some(ime) = o.ime.as_mut() {
                ime.cursor_rect =
                    ime.cursor_rect.translate(Vec2::new(self.preedit_end_x.get() - self.caret_x.get(), 0.0));
            }
        });
        let len = value.chars().count();
        let (from, to) = composition.span();
        let (from, to) = (from.min(len), to.min(len));
        let spoken: String =
            value.chars().take(from).chain(composition.preedit.chars()).chain(value.chars().skip(to)).collect();
        ctx.accesskit_node_builder(id, |node| node.set_value(spoken));
        value.clone_into(&mut composition.seen);
        ctx.data_mut(|d| d.insert_temp(key, composition));
    }
}
