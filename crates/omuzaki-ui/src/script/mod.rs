//! The Script workspace (#1097, #1221; `scripting.md` sections 7 and 8, spec §30): a person writes and runs Rhai
//! scripts over the same ops an agent calls, and records their own edits as macros to replay.
//!
//! - **Scripts** (left): the project's scripts and macros and My scripts, macros marked by a record glyph.
//! - **Console** (centre): the editor (line numbers, highlighting, `script.check` problems underlined as you type,
//!   completions from the registry, an op's summary on hover) above the output. Ctrl+Enter runs, Ctrl+Shift+Enter dry
//!   runs, Esc stops. A run goes through the `Runner` as the person, off the UI thread, and is one undo step.
//! - **Macros** and **Ops** (right): record, list, replay and save macros; search the registry and insert a call.
//! - Ctrl+Shift+M records from anywhere, and the status bar shows "● REC" while it does.
//!
//! Every change is an op: runs through [`RunnerCalls`](omuzaki_script::RunnerCalls), saves through `script.save` and
//! `script.save_mine`, replays through [`omuzaki_script::play`]. The pure parts (completions, the consent decision, the
//! highlighter, a macro written as a script) are their own modules and are tested without a window.

pub mod complete;
pub mod consent;
pub mod console;
pub mod highlight;
pub mod macros;
pub mod ops_ref;
pub mod registration;
pub mod scripts;
pub mod source_gen;
pub mod store;
pub mod text;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use omuzaki_doc_api::DocPath;
use omuzaki_ops_api::{Actor, Grants, Mode, OpError, Request, Runner};
use omuzaki_script::{Macro, MacroRecorder, Place, RecordedCall, ScriptKind, ScriptStore};
use serde::Deserialize;
use serde_json::{Value, json};

use self::complete::Callable;
use self::consent::{Author, ConsentBook};

/// A saved item as `script.list` lists it.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Listed {
    /// Its id.
    pub id: String,
    /// Its name; missing when the item can't be read.
    #[serde(default)]
    pub name: Option<String>,
    /// Script or macro.
    #[serde(default)]
    pub kind: Option<ScriptKind>,
    /// In the project or in My scripts.
    pub place: Place,
    /// The hash of its text.
    #[serde(default)]
    pub hash: Option<String>,
    /// Who saved it last (project items).
    #[serde(default)]
    pub author: Option<Value>,
    /// Why it can't be read, when it can't.
    #[serde(default)]
    pub problem: Option<String>,
}

impl Listed {
    /// Its name, or its id.
    pub fn title(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.id)
    }

    /// Whether it is a macro.
    pub fn is_macro(&self) -> bool {
        self.kind == Some(ScriptKind::Macro)
    }
}

/// The script open in the console.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opened {
    /// The id and place it was saved under; `None` for a new script.
    pub saved: Option<(String, Place)>,
    /// Its name.
    pub name: String,
    /// Its source as it was opened.
    pub source: String,
    /// Who saved it last.
    pub author: Author,
}

/// What the panels share.
struct Shared {
    /// The script the console should show, and a count that moves each time another is opened.
    opened: (u64, Opened),
    /// Text the Ops panel asked the console to put at its cursor.
    insert: Option<String>,
    /// The recording under way.
    recorder: Option<MacroRecorder>,
    /// The last recording, until it is saved or put away.
    recorded: Option<Macro>,
    /// The macro the person last chose, by id and place, for the Macros panel.
    chosen_macro: Option<(String, Place)>,
    /// Whether the last recording filled up and left later edits out.
    full: bool,
    /// The callable ops, by the registry size they were read at.
    callables: Option<(usize, Rc<Vec<Callable>>)>,
}

/// The Script workspace's state, shared by its panels and the shell's REC item. Clones share it.
#[derive(Clone)]
pub struct Scripting {
    runner: Arc<dyn Runner>,
    mine: Option<Arc<dyn ScriptStore>>,
    project: String,
    consent: Rc<ConsentBook>,
    shared: Rc<RefCell<Shared>>,
}

impl std::fmt::Debug for Scripting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scripting").field("project", &self.project).finish_non_exhaustive()
    }
}

impl Scripting {
    /// Scripting over `runner`, with My scripts in `mine` (the same store the executor's `script.*_mine` ops use) and
    /// the person's agreed runs in `consent`; `project` names the open project for those agreements.
    pub fn new(
        runner: Arc<dyn Runner>,
        mine: Option<Arc<dyn ScriptStore>>,
        project: impl Into<String>,
        consent: ConsentBook,
    ) -> Self {
        let opened = Opened { saved: None, name: String::new(), source: String::new(), author: Author::Me };
        Self {
            runner,
            mine,
            project: project.into(),
            consent: Rc::new(consent),
            shared: Rc::new(RefCell::new(Shared {
                opened: (0, opened),
                insert: None,
                recorder: None,
                recorded: None,
                chosen_macro: None,
                full: false,
                callables: None,
            })),
        }
    }

    /// The runner everything goes through.
    pub fn runner(&self) -> &Arc<dyn Runner> {
        &self.runner
    }

    /// The open project's name for agreed runs.
    pub fn project(&self) -> &str {
        &self.project
    }

    /// The runs the person agreed to.
    pub fn consent(&self) -> &ConsentBook {
        &self.consent
    }

    /// The ops a script can call, read from the runner once per registry size.
    pub fn callables(&self) -> Rc<Vec<Callable>> {
        let ops = self.runner.ops();
        let mut shared = self.shared.borrow_mut();
        if let Some((size, callables)) = &shared.callables
            && *size == ops.len()
        {
            return Rc::clone(callables);
        }
        let callables = Rc::new(complete::callables(&ops, omuzaki_script::script_call));
        shared.callables = Some((ops.len(), Rc::clone(&callables)));
        callables
    }

    /// Opens `opened` in the console.
    pub fn open(&self, opened: Opened) {
        let mut shared = self.shared.borrow_mut();
        let count = shared.opened.0 + 1;
        shared.opened = (count, opened);
    }

    /// The script the console should show, and the count that moves with each open.
    pub fn opened(&self) -> (u64, Opened) {
        self.shared.borrow().opened.clone()
    }

    /// Asks the console to put `text` at its cursor.
    pub fn insert(&self, text: String) {
        self.shared.borrow_mut().insert = Some(text);
    }

    /// The text waiting to be put at the cursor.
    pub fn take_insert(&self) -> Option<String> {
        self.shared.borrow_mut().insert.take()
    }

    /// The saved scripts and macros, through `script.list`.
    ///
    /// # Errors
    /// The op's refusal.
    pub fn list(&self, actor: &Actor) -> Result<Vec<Listed>, OpError> {
        let answer = run(self.runner.as_ref(), actor, "script.list", json!({}))?;
        serde_json::from_value(answer["scripts"].clone()).map_err(|e| OpError::internal(format!("script.list: {e}")))
    }

    /// What `item` holds: its source, or its macro.
    ///
    /// # Errors
    /// `NotFound` when it is gone, or the store's error.
    pub fn body(&self, item: &Listed) -> Result<Body, OpError> {
        let gone = || OpError::not_found(format!("`{}` is no longer saved", item.id));
        match item.place {
            Place::Project => {
                let path =
                    DocPath::parse(&format!("scripts/{}", item.id)).map_err(|e| OpError::internal(e.to_string()))?;
                let node = self.runner.get(&path, &Grants::person())?.ok_or_else(gone)?;
                if let Some(source) = node.get("source").and_then(Value::as_str) {
                    return Ok(Body::Source(source.to_owned()));
                }
                let steps: Vec<RecordedCall> =
                    serde_json::from_value(node.get("steps").cloned().unwrap_or(Value::Null))
                        .map_err(|e| OpError::internal(format!("the macro's steps can't be read: {e}")))?;
                Ok(Body::Macro(Macro { name: item.title().to_owned(), steps }))
            }
            Place::Mine => {
                let store = self.mine.as_ref().ok_or_else(gone)?;
                let stored = store.get(&item.id)?.ok_or_else(gone)?;
                match stored.kind {
                    ScriptKind::Script => Ok(Body::Source(stored.body)),
                    ScriptKind::Macro => omuzaki_script::from_jsonl(&stored.body)
                        .map(Body::Macro)
                        .map_err(|e| OpError::internal(format!("the macro file can't be read: {e}"))),
                }
            }
        }
    }

    /// Opens `item` in the console as `me`.
    ///
    /// # Errors
    /// As [`body`](Self::body).
    pub fn open_item(&self, item: &Listed, me: &Actor) -> Result<(), OpError> {
        let source = match self.body(item)? {
            Body::Source(source) => source,
            Body::Macro(m) => source_gen::macro_source(&m, omuzaki_script::script_call),
        };
        let author = match item.place {
            Place::Project => Author::of(item.author.as_ref(), me),
            Place::Mine => Author::Me,
        };
        self.open(Opened { saved: Some((item.id.clone(), item.place)), name: item.title().to_owned(), source, author });
        Ok(())
    }

    /// Whether a recording is under way.
    pub fn recording(&self) -> bool {
        self.shared.borrow().recorder.is_some()
    }

    /// Starts recording `me`'s own edits.
    ///
    /// # Errors
    /// The runner's refusal to subscribe.
    pub fn start_recording(&self, me: &Actor) -> Result<(), OpError> {
        let recorder = MacroRecorder::start(self.runner.as_ref(), me, &Grants::person())?;
        let mut shared = self.shared.borrow_mut();
        shared.recorder = Some(recorder);
        shared.full = false;
        Ok(())
    }

    /// Stops recording; the macro, named `name`, waits in [`recorded`](Self::recorded) to be saved.
    pub fn stop_recording(&self, name: &str) {
        let mut shared = self.shared.borrow_mut();
        let Some(recorder) = shared.recorder.take() else { return };
        match recorder.finish(name) {
            Ok(m) => shared.recorded = Some(m),
            Err(overflowed) => {
                shared.full = true;
                shared.recorded = Some(overflowed.partial);
            }
        }
    }

    /// Starts or stops recording, as Ctrl+Shift+M does; a stopped recording is named `name`.
    ///
    /// # Errors
    /// As [`start_recording`](Self::start_recording).
    pub fn toggle_recording(&self, me: &Actor, name: &str) -> Result<(), OpError> {
        if self.recording() {
            self.stop_recording(name);
            Ok(())
        } else {
            self.start_recording(me)
        }
    }

    /// Takes in the runner's events for the recording, without blocking; the shell calls it every frame. A recorder
    /// that filled up is stopped, and [`full`](Self::full) says so.
    pub fn poll(&self) {
        let full = {
            let mut shared = self.shared.borrow_mut();
            let Some(recorder) = shared.recorder.as_mut() else { return };
            recorder.poll();
            recorder.overflowed()
        };
        if full {
            self.stop_recording("");
        }
    }

    /// The last recording, until it is saved or put away.
    pub fn recorded(&self) -> Option<Macro> {
        self.shared.borrow().recorded.clone()
    }

    /// Puts the last recording away.
    pub fn discard_recorded(&self) {
        self.shared.borrow_mut().recorded = None;
    }

    /// Whether the last recording filled up and left later edits out.
    pub fn full(&self) -> bool {
        self.shared.borrow().full
    }

    /// The macro the person last chose in a list.
    pub fn chosen_macro(&self) -> Option<(String, Place)> {
        self.shared.borrow().chosen_macro.clone()
    }

    /// Chooses a macro, or none.
    pub fn choose_macro(&self, chosen: Option<(String, Place)>) {
        self.shared.borrow_mut().chosen_macro = chosen;
    }
}

/// What a saved item holds.
#[derive(Clone, Debug, PartialEq)]
pub enum Body {
    /// Rhai source.
    Source(String),
    /// A macro.
    Macro(Macro),
}

/// Runs `op` as `actor` and returns its answer.
///
/// # Errors
/// The op's refusal.
pub fn run(runner: &dyn Runner, actor: &Actor, op: &str, args: Value) -> Result<Value, OpError> {
    let outcome = runner.run(Request {
        op: op.to_owned(),
        args,
        mode: Mode::Apply,
        actor: actor.clone(),
        grants: Grants::person(),
        cause: None,
        tx: None,
        base: None,
    })?;
    Ok(outcome.plan.output.unwrap_or(Value::Null))
}

/// A slug made from `name` for a new item's id: lower case, dashes between words, at most 40 characters.
pub fn slug_of(name: &str) -> String {
    let mut slug = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
        if slug.len() >= 40 {
            break;
        }
    }
    let slug = slug.trim_end_matches('-').to_owned();
    if slug.is_empty() { "script".to_owned() } else { slug }
}
