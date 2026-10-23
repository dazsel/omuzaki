//! The Script workspace's own words, each a named constant whose id the lexicon makes from its name
//! (`ui-script-run` for `RUN`), said through [`say`](crate::panels::text::say).

crate::ds::strings::lines! {
    prefix = "ui-script";

    /// A studio started without scripting.
    pub const NO_SCRIPTING: &str = "Scripts aren't available";
    /// Why.
    pub const NO_SCRIPTING_HOW: &str = "This studio was started without a document to run scripts on.";

    /// The script name field's label.
    pub const SCRIPT_NAME: &str = "Script name";
    /// The name of a script nobody named yet; also its run's undo step.
    pub const UNTITLED: &str = "Untitled script";
    /// Save the script.
    pub const SAVE: &str = "Save";
    /// Save it in the project.
    pub const SAVE_PROJECT: &str = "Save to project";
    /// Save a macro in My scripts.
    pub const SAVE_MINE: &str = "Save to My scripts";
    /// A save went through.
    pub const SAVED: &str = "Saved as {name}";
    /// Run the script.
    pub const RUN: &str = "Run";
    /// Run it without changing anything.
    pub const DRY_RUN: &str = "Dry run";
    /// Stop the run.
    pub const STOP: &str = "Stop";
    /// Why Run is off.
    pub const NOTHING_TO_RUN: &str = "Write a script first";
    /// Cancel.
    pub const CANCEL: &str = "Cancel";

    /// The editor's name for a screen reader.
    pub const EDITOR: &str = "Script";
    /// The empty editor's hint.
    pub const EDITOR_HINT: &str = "Type ops:: to see what you can call. Ctrl+Enter runs, Ctrl+Space completes.";
    /// The divider between the editor and the output.
    pub const DIVIDER: &str = "Resize the editor and the output";
    /// No problem found.
    pub const NO_PROBLEMS: &str = "No problems";
    /// One problem.
    pub const ONE_PROBLEM: &str = "1 problem";
    /// Several problems.
    pub const PROBLEMS: &str = "{count} problems";
    /// A problem with its place; a click goes there.
    pub const AT_LINE: &str = "Line {line}, column {column}: {message}";
    /// An op's summary on hover.
    pub const OP_HOVER: &str = "{op} ({scope}): {summary}";

    /// An area in the completion list.
    pub const AREA_OPS: &str = "{count} ops";
    /// A number field.
    pub const KIND_NUMBER: &str = "number";
    /// A whole-number field.
    pub const KIND_INTEGER: &str = "whole number";
    /// A text field.
    pub const KIND_TEXT: &str = "text";
    /// A true-or-false field.
    pub const KIND_BOOL: &str = "true or false";
    /// A list field.
    pub const KIND_LIST: &str = "list";
    /// A map field.
    pub const KIND_MAP: &str = "map";
    /// A field of any kind.
    pub const KIND_ANY: &str = "any value";
    /// A field the op needs.
    pub const REQUIRED: &str = "required";
    /// A number's range.
    pub const RANGE: &str = "{min} to {max}";
    /// A number's floor.
    pub const AT_LEAST: &str = "at least {min}";
    /// A number's ceiling.
    pub const AT_MOST: &str = "at most {max}";

    /// The output before anything ran.
    pub const OUTPUT_EMPTY: &str = "What the script prints shows here.";
    /// A run under way.
    pub const RUNNING: &str = "Running… Esc stops it.";
    /// A dry run's result.
    pub const DRY_DONE: &str = "Dry run: nothing changed. Each call saw the document as it was before the script, so later calls may plan differently in a real run.";
    /// A run's undo step; a click opens History.
    pub const UNDO_STEP: &str = "Undo: {name}, {count} ops";
    /// A run that called no edit.
    pub const NOTHING_CHANGED: &str = "Done. Nothing changed.";
    /// A stopped run.
    pub const STOPPED: &str = "Stopped. Nothing changed.";
    /// A failed run, with its place; a click goes there.
    pub const FAILED_AT: &str = "Line {line}, column {column}: {message}";
    /// After a failure.
    pub const ROLLED_BACK: &str = "Nothing changed: the run was undone whole.";

    /// The consent card's title.
    pub const CONSENT_TITLE: &str = "Run this script?";
    /// The person wrote it.
    pub const BY_YOU: &str = "You saved this script.";
    /// An agent saved it.
    pub const BY_AGENT: &str = "The agent {agent} saved this script.";
    /// Another person saved it.
    pub const BY_PERSON: &str = "{person} saved this script.";
    /// A plugin saved it.
    pub const BY_PLUGIN: &str = "The plugin {plugin} saved this script.";
    /// Omuzaki saved it (an import).
    pub const BY_SYSTEM: &str = "This script came in with an import.";
    /// It names an op that reaches outside the studio.
    pub const REACHES: &str = "It can reach outside the studio: it streams, publishes, spends, uses the network, files or programs.";
    /// The list of ops.
    pub const CONSENT_CALLS: &str = "It calls:";

    /// Start an empty script.
    pub const NEW_SCRIPT: &str = "New script";
    /// The project's items.
    pub const IN_PROJECT: &str = "In this project";
    /// My scripts.
    pub const MINE: &str = "My scripts";
    /// No project items.
    pub const NONE_IN_PROJECT: &str = "None yet. Save a script to the project to share it with this character.";
    /// No items of the person's own.
    pub const NONE_MINE: &str = "None yet. Your own scripts and macros, in every project.";
    /// The list couldn't be read.
    pub const LIST_FAILED: &str = "The scripts couldn't be listed: {reason}";
    /// An item that can't be read.
    pub const UNREADABLE: &str = "{id} can't be read: {problem}";
    /// A script.
    pub const KIND_SCRIPT: &str = "script";
    /// A macro.
    pub const KIND_MACRO: &str = "macro";
    /// A row's name for a screen reader.
    pub const OPEN_ITEM: &str = "Open {name}, {kind}";
    /// Delete an item.
    pub const DELETE_ITEM: &str = "Delete {name}";
    /// Confirm the delete.
    pub const DELETE_SURE: &str = "Delete";
    /// Keep it.
    pub const KEEP: &str = "Keep";

    /// Search the ops.
    pub const SEARCH_OPS: &str = "Search ops";
    /// No op matches.
    pub const NO_OPS_FOUND: &str = "No op matches.";
    /// Insert a call.
    pub const INSERT: &str = "Insert";
    /// Insert, for a screen reader.
    pub const INSERT_CALL: &str = "Insert a call to {op}";

    /// Start recording.
    pub const RECORD: &str = "Record a macro";
    /// Stop recording.
    pub const STOP_RECORDING: &str = "Stop recording";
    /// The recording chip.
    pub const REC: &str = "● REC";
    /// While recording.
    pub const RECORDING_HOW: &str = "Recording your edits. Undo takes a step out; agents' work is never recorded.";
    /// The status bar after Ctrl+Shift+M started a recording.
    pub const REC_STARTED: &str = "Recording a macro. Ctrl+Shift+M stops.";
    /// The status bar after Ctrl+Shift+M stopped it.
    pub const REC_STOPPED: &str = "Recording stopped. Save it in the Script workspace's Macros panel.";
    /// The REC chip's tooltip.
    pub const REC_HOW_TO_STOP: &str = "Recording a macro: Ctrl+Shift+M stops";
    /// A full recorder.
    pub const REC_FULL: &str = "The recording got too long and left the later edits out.";
    /// A new recording's name.
    pub const NEW_MACRO: &str = "New macro";
    /// The new recording's section.
    pub const NEW_RECORDING: &str = "New recording";
    /// Its name field.
    pub const MACRO_NAME: &str = "Macro name";
    /// A recording with no steps.
    pub const NO_STEPS: &str = "No edits were recorded.";
    /// Why the save buttons are off.
    pub const NEEDS_NAME_AND_STEPS: &str = "Name it, and record at least one edit";
    /// Write a macro out as a script.
    pub const SAVE_AS_SCRIPT: &str = "Save as script";
    /// Put the recording away.
    pub const DISCARD: &str = "Discard";
    /// The saved macros.
    pub const SAVED_MACROS: &str = "Saved macros";
    /// No saved macros.
    pub const NO_MACROS: &str = "None yet. Record one, or press Ctrl+Shift+M anywhere.";
    /// Replay a macro.
    pub const REPLAY: &str = "Replay";
    /// Why Replay is off.
    pub const REPLAYING: &str = "A replay is under way";
    /// A replay went through.
    pub const REPLAYED: &str = "Replayed {count} steps as one undo step.";
    /// A replay failed.
    pub const REPLAY_FAILED: &str = "Step {step} ({op}) failed: {message}. Nothing changed.";
    /// A step naming an op this build lacks.
    pub const UNAVAILABLE_OP: &str = "{op} isn't in this build";
    /// Save a macro to the library.
    pub const SAVE_LIBRARY: &str = "Save as library item";
    /// Why not: empty.
    pub const LIB_EMPTY: &str = "An empty macro can't be a library item";
    /// Why not: an op beyond a character or the scene.
    pub const LIB_SCOPE: &str = "{op} needs {scope}; library items only edit a character or the scene";
    /// Why not: two characters.
    pub const LIB_TWO_TARGETS: &str = "It edits more than one character; a library item edits one";
    /// Why not: no library save in this build.
    pub const LIB_NOT_IN_BUILD: &str = "Saving to the library isn't in this build yet";
}
