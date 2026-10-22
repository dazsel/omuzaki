//! The edit panels' own words.
//!
//! They are written in English here and passed through [`say`], which gives the text itself in English and the
//! accented, longer version in the pseudo-locale, as the gallery's words are (`ds::gallery::text`): a plain ASCII word
//! on screen in the pseudo-locale is one that skipped this table. They move into the `Strings` table with the change
//! that holds `ds/strings.rs`.

use std::borrow::Cow;

use crate::ds::strings::{PSEUDO_LOCALE, fill, pseudo};
use crate::ds::theme::Ds;

/// The outliner's name, for its tree and its screen-reader label.
pub const OUTLINER: &str = "Outliner";
/// The outliner's filter field.
pub const FILTER: &str = "Filter parts";
/// The outliner with no character open.
pub const NO_CHARACTER: &str = "No character is open";
/// What to do about it.
pub const OPEN_CHARACTER: &str = "Open or create a character to see its parts.";
/// The viewport with no character open (`shell.md` section 13, "No project open").
pub const VIEWPORT_EMPTY: &str = "Start with a character";
/// What to do about it.
pub const VIEWPORT_EMPTY_HOW: &str = "Open a project that has a character, or create one.";
/// The inspector with no character open (`shell.md` section 13).
pub const INSPECTOR_EMPTY: &str = "Select a part to edit it";
/// Why it can't yet.
pub const INSPECTOR_EMPTY_HOW: &str = "Its settings appear here once a character is open.";
/// The Agent panel with no agent connected (`shell.md` section 13).
pub const AGENT_NONE: &str = "Connect an agent";
/// How.
pub const AGENT_NONE_HOW: &str =
    "Agents work through the same ops you do, and you approve what they ask for. Connect one over MCP to see it here.";
/// A session's step count.
pub const AGENT_STEPS: &str = "{steps} steps";
/// A session that made a call in the last minute.
pub const AGENT_WORKING: &str = "Working";
/// A session quiet for a minute or more.
pub const AGENT_IDLE: &str = "Idle";
/// A session with a request waiting for the person.
pub const AGENT_WAITING: &str = "Waiting for you";
/// A paused session.
pub const AGENT_PAUSED: &str = "Paused";
/// A session paused because its budget ran out.
pub const AGENT_PAUSED_BUDGET: &str = "Paused: budget reached";
/// A bridge session waiting to be paired.
pub const AGENT_PAUSED_PAIRING: &str = "Waiting to be paired";
/// A session paused while its work is undone.
pub const AGENT_PAUSED_UNDO: &str = "Paused while you undo";
/// An ended session.
pub const AGENT_ENDED: &str = "Ended";
/// Who a session works for and how it is connected.
pub const AGENT_FOR: &str = "For {person} \u{b7} {transport}";
/// The person running the studio, in a session's line.
pub const AGENT_YOU: &str = "you";
/// A sub-agent's role.
pub const AGENT_ROLE: &str = "Role: {role}";
/// Spend against the budget.
pub const AGENT_SPEND: &str = "{spent} of {budget}";
/// Tokens used.
pub const AGENT_TOKENS: &str = "{count} tokens";
/// The session's open transaction.
pub const AGENT_OPEN_TX: &str = "In progress: {label}";
/// Opens the grants popover.
pub const AGENT_GRANTS: &str = "Grants";
/// Its spoken name.
pub const AGENT_GRANTS_NAME: &str = "Grants for {name}";
/// A session control's spoken name.
pub const AGENT_CONTROL_NAME: &str = "{action} {name}";
/// Pause a session.
pub const AGENT_PAUSE: &str = "Pause";
/// Resume a paused session.
pub const AGENT_RESUME: &str = "Resume";
/// Stop a session: pause it and cancel its jobs.
pub const AGENT_STOP: &str = "Stop";
/// Undo every step a session made, as one step.
pub const AGENT_UNDO_ALL: &str = "Undo its {steps} steps\u{2026}";
/// End a session for good.
pub const AGENT_REVOKE: &str = "Revoke\u{2026}";
/// MCP over stdio.
pub const TRANSPORT_STDIO: &str = "MCP over stdio";
/// MCP over HTTP, with the token's label.
pub const TRANSPORT_HTTP: &str = "MCP over HTTP ({token})";
/// The built-in assistant.
pub const TRANSPORT_IN_APP: &str = "Built-in assistant";
/// A plugin.
pub const TRANSPORT_PLUGIN: &str = "Plugin";
/// An automation rule.
pub const TRANSPORT_AUTOMATION: &str = "Automation";
/// A replay of a recorded session.
pub const TRANSPORT_REPLAY: &str = "Replay";
/// The revoke confirm's title.
pub const REVOKE_TITLE: &str = "Revoke {name}?";
/// What revoking does.
pub const REVOKE_BODY: &str = "The session ends: its unfinished changes are undone and its waiting requests are withdrawn. Pause it instead to keep it.";
/// Also revoke the credential.
pub const REVOKE_CREDENTIAL: &str = "Also revoke its access, so it can't connect again";
/// The other sessions on that credential.
pub const REVOKE_OTHERS: &str = "This also ends: {names}";
/// The revoke button.
pub const REVOKE_PRIMARY: &str = "Revoke {name}";
/// Its armed second step.
pub const REVOKE_CONFIRM: &str = "Confirm: revoke {name}";
/// The undo-session confirm's title.
pub const UNDO_TITLE: &str = "Undo everything {name} did?";
/// What it does.
pub const UNDO_BODY: &str =
    "Its {steps} steps are reverted, newest first, as one step in your history, so Ctrl+Z brings them back.";
/// The undo button.
pub const UNDO_PRIMARY: &str = "Undo {steps} steps";
/// Its armed second step.
pub const UNDO_CONFIRM: &str = "Confirm: undo {steps} steps";
/// The confirm for a scope that reaches outside the project.
pub const GRANT_TITLE: &str = "Let {name} use {scope}?";
/// Its button.
pub const GRANT_PRIMARY: &str = "Grant {scope}";
/// Its armed second step.
pub const GRANT_CONFIRM: &str = "Confirm: grant {scope}";
/// Grants last as long as the session.
pub const GRANTS_SESSION_ONLY: &str = "This session only";
/// A scope the person does not hold.
pub const GRANTS_ABOVE_YOU: &str = "You don't hold this yourself";
/// The spending cap field.
pub const GRANTS_CAP: &str = "Spending cap in dollars";
/// Spend needs a cap first.
pub const GRANTS_CAP_FIRST: &str = "Set a cap above $0 first";
/// What it has spent, before the cap.
pub const GRANTS_SPENT: &str = "{spent} of";
/// Remove a folder.
pub const GRANTS_FS_REMOVE: &str = "Remove {root}";
/// The folder path field.
pub const GRANTS_FS_PATH: &str = "Folder path";
/// Add a folder.
pub const GRANTS_FS_ADD: &str = "Add folder";
/// The folder field is empty.
pub const GRANTS_FS_FIRST: &str = "Type a folder path first";
/// Take every grant back.
pub const GRANTS_REVOKE_ALL: &str = "Revoke all";
/// Remember the grants for this project.
pub const GRANTS_PROJECT: &str = "Grant for this project";
/// The widest grant, in words: native code.
pub const WIDEST_NATIVE: &str = "{name} can load native code, which runs with the app's full rights.";
/// Spend.
pub const WIDEST_SPEND: &str = "{name} can spend up to {cap} on paid connectors.";
/// Broadcast.
pub const WIDEST_BROADCAST: &str = "{name} can go live and switch scenes.";
/// Publish.
pub const WIDEST_PUBLISH: &str = "{name} can export or upload outside this computer.";
/// Network.
pub const WIDEST_NET: &str = "{name} can reach the network.";
/// Files.
pub const WIDEST_FS: &str = "{name} can read and write files in {count} folders outside the project.";
/// Edit.
pub const WIDEST_EDIT: &str = "{name} can edit the project, and every edit can be undone.";
/// Render.
pub const WIDEST_RENDER: &str = "{name} can read the project and render previews.";
/// Read.
pub const WIDEST_READ: &str = "{name} can read the project and change nothing.";
/// Nothing.
pub const WIDEST_NONE: &str = "{name} can do nothing until you grant it something.";
/// What a scope allows: read.
pub const SCOPE_READ: &str = "Read anything in the project.";
/// Edit characters.
pub const SCOPE_EDIT_CHARACTER: &str = "Edit characters: parts, face, rig and look.";
/// Edit scenes.
pub const SCOPE_EDIT_SCENE: &str = "Edit scenes, props, lights and cameras.";
/// Edit the library.
pub const SCOPE_EDIT_LIBRARY: &str = "Edit the library.";
/// Edit settings.
pub const SCOPE_EDIT_SETTINGS: &str = "Change your settings and profiles, never project data.";
/// Render.
pub const SCOPE_RENDER: &str = "Render previews and offline renders.";
/// Perform.
pub const SCOPE_PERFORM: &str = "Drive the character live.";
/// Broadcast.
pub const SCOPE_BROADCAST: &str = "Go live and switch scenes: your audience sees what it does.";
/// Publish.
pub const SCOPE_PUBLISH: &str = "Export or upload anything outside this computer.";
/// Spend.
pub const SCOPE_SPEND: &str = "Use paid connectors, up to the spending cap.";
/// Network.
pub const SCOPE_NET: &str = "Reach the network.";
/// Files.
pub const SCOPE_FS: &str = "Read and write files outside the project, in the folders you add.";
/// Native code.
pub const SCOPE_NATIVE: &str = "Load native code, which runs with the app's full rights.";
/// Input capture.
pub const SCOPE_INPUT: &str = "Read keystrokes and pointer input while another app has focus.";
/// Programs.
pub const SCOPE_PROCESS: &str = "Start named programs on this computer and check whether they run.";
/// Scripts.
pub const SCOPE_SCRIPT: &str = "Run and save scripts; each step is still checked.";
/// Review.
pub const SCOPE_REVIEW: &str = "Comment on reviews and reply to comments.";
/// An approval to run an op.
pub const APPROVAL_RUN: &str = "Run {op}";
/// An approval to pair a client.
pub const APPROVAL_PAIR: &str = "Pair {client} with this studio";
/// A budget crossed by a charge, with no request.
pub const APPROVAL_BUDGET: &str = "Go on past its budget";
/// Any other approval.
pub const APPROVAL_OTHER: &str = "Continue";
/// The pairing card's Approve.
pub const APPROVAL_PAIR_BUTTON: &str = "Pair";
/// The budget card's Approve.
pub const APPROVAL_ONCE: &str = "Approve once";
/// Raise the budget.
pub const APPROVAL_RAISE: &str = "Raise the budget to {budget}";
/// The preview's change count.
pub const APPROVAL_CHANGES: &str = "{count} values";
/// More approvals than the stack shows.
pub const APPROVAL_MORE: &str = "{count} more in the Agent panel";
/// The review card's title.
pub const REVIEW_WHAT: &str = "Review {op} before it runs";
/// The policy that asks.
pub const REVIEW_POLICY: &str = "Asked by the policy {policy}";
/// Why the requester asked.
pub const REVIEW_WHY: &str = "Why: {cause}";
/// Approve is not for the requester.
pub const REVIEW_OWN: &str = "You asked for this, so another reviewer approves it";
/// The reviewer's copy differs.
pub const REVIEW_DIFFERS: &str = "Your copy differs from what was requested: sync the project, then review";
/// No evidence to check against.
pub const REVIEW_UNKNOWN: &str = "This studio can't check the request against your copy yet";
/// Approvals so far.
pub const REVIEW_PROGRESS: &str = "{have} of {need} approvals";
/// Approvals so far, with names.
pub const REVIEW_PROGRESS_BY: &str = "{have} of {need} approvals: {names}";
/// The licence check.
pub const REVIEW_LICENCE: &str = "Licence check: {result}";
/// Open threads.
pub const REVIEW_THREADS: &str = "{count} open comment threads";
/// One open thread.
pub const REVIEW_THREAD: &str = "Open thread: {text}";
/// Imported feedback.
pub const REVIEW_FEEDBACK: &str = "Feedback: {text}";
/// Export for a remote reviewer.
pub const REVIEW_EXPORT: &str = "Export for a remote reviewer";
/// The fence chip's entry for one holder.
pub const FENCE_HOLDS: &str = "{name} holds {what}";
/// More holders than the chip names.
pub const FENCE_MORE: &str = "{count} more";
/// The note on a held part.
pub const FENCE_NOTE: &str = "{name} is working on this part. Your edit takes precedence and it will be told.";
/// Hide the note.
pub const FENCE_DISMISS: &str = "Dismiss";
/// A character with no parts.
pub const NO_PARTS: &str = "This character has no parts yet";
/// What to do about it.
pub const ADD_PART: &str = "Add one from the library or ask the assistant.";
/// A filter that matches nothing.
pub const NO_MATCH: &str = "No part matches \u{201c}{filter}\u{201d}";
/// The context menu's reorder entries (every drag has one, WCAG 2.5.7).
pub const MOVE_UP: &str = "Move up";
/// See [`MOVE_UP`].
pub const MOVE_DOWN: &str = "Move down";
/// Copies a part.
pub const DUPLICATE: &str = "Duplicate";
/// Removes a part (undoable).
pub const DELETE: &str = "Delete";
/// Turns a disabled part back on.
pub const ENABLE: &str = "Enable";
/// Turns a part off without removing it.
pub const DISABLE: &str = "Disable";
/// Frames the part in the viewport.
pub const FRAME: &str = "Frame";
/// Renames a part (F2).
pub const RENAME: &str = "Rename";
/// Why Rename is disabled: no op renames a part yet, and the panel never calls one by a guessed name.
pub const RENAME_NOT_YET: &str = "Not available yet: renaming a part needs a part.rename op";
/// An op the panel ran was refused: the runner's message follows.
pub const REFUSED: &str = "That didn't work: {reason}";
/// A part whose stored parameters fail its kind's check.
pub const PART_INVALID: &str = "Its parameters don't pass the kind's check";

/// The inspector's name.
pub const INSPECTOR: &str = "Inspector";
/// The inspector with nothing selected.
pub const SELECT_A_PART: &str = "Select a part to edit it";
/// What to do about it.
pub const SELECT_HOW: &str = "Click a part in the outliner or the viewport.";
/// The inspector's search field.
pub const SEARCH_SETTINGS: &str = "Search settings\u{2026}";
/// More than one part is selected.
pub const MANY_SELECTED: &str = "{count} parts selected";
/// What to do about it.
pub const MANY_HOW: &str = "Select one part to edit its settings.";
/// A search that matches no setting.
pub const NO_SETTING: &str = "No setting matches \u{201c}{query}\u{201d}";
/// The header's kind line.
pub const KIND: &str = "Kind: {kind}";
/// Retries rolling back an edit that could not be undone because the character changed meanwhile.
pub const ROLLBACK_RETRY: &str = "Try again";
/// Keeps an edit that could not be rolled back, as one undo step.
pub const ROLLBACK_KEEP: &str = "Keep the change";
/// The undo step of an edit in the inspector.
pub const SET: &str = "Set {name}";
/// `part.describe` failed: the runner's message follows.
pub const UNDESCRIBED: &str = "This part can't be described: {reason}";

/// The viewport's name.
pub const VIEWPORT: &str = "Viewport";
/// The camera presets (keys 1 to 4).
pub const FRONT: &str = "Front";
/// See [`FRONT`].
pub const THREE_QUARTER: &str = "\u{be}";
/// See [`FRONT`].
pub const SIDE: &str = "Side";
/// See [`FRONT`].
pub const BACK: &str = "Back";
/// Frames the selection (F).
pub const FRAME_SELECTION: &str = "Frame selection";
/// Frames the whole character (Home).
pub const FRAME_CHARACTER: &str = "Frame character";
/// The viewport without a GPU to draw on.
pub const NO_GPU: &str = "The viewport needs a GPU";
/// What to do about it.
pub const NO_GPU_HOW: &str = "Start the app with a Vulkan, Metal or DirectX 12 adapter.";
/// The character doesn't build.
pub const UNBUILT: &str = "The character doesn't build: {reason}";
/// What the viewport's frame readout says.
pub const READOUT: &str = "{ms} ms GPU";

/// The first-run flow's panel (`shell.md` section 14): which step of how many.
pub const SETUP_STEP: &str = "Step {n} of {of}";
/// Leaves the flow at any step.
pub const SETUP_SKIP: &str = "Skip setup";
/// Goes back a step.
pub const SETUP_BACK: &str = "Back";
/// Goes on to the next step.
pub const SETUP_NEXT: &str = "Next";
/// Step 0.
pub const SETUP_WELCOME: &str = "Welcome to Omuzaki";
/// What step 0 promises.
pub const SETUP_WELCOME_LINE: &str = "Your camera never leaves this computer. Telemetry is off.";
/// What the flow does and how long it takes.
pub const SETUP_WELCOME_HOW: &str =
    "In about two minutes: pick a character, let the camera follow your face, and send the character to OBS.";
/// Starts the flow.
pub const SETUP_START: &str = "Start";
/// Step 1.
pub const SETUP_CHARACTER: &str = "Choose your character";
/// The sample, already on the stage.
pub const SETUP_SAMPLE: &str = "Use the sample";
/// Bringing one's own model.
pub const SETUP_VRM: &str = "Or drop a .vrm file here to use your own model";
/// A model was taken in.
pub const SETUP_VRM_DONE: &str = "Using {file}";
/// Step 2.
pub const SETUP_INPUT: &str = "Let the camera follow your face";
/// What step 2 does.
pub const SETUP_INPUT_HOW: &str = "The character mirrors your face and head. The picture stays on this computer.";
/// No camera.
pub const SETUP_NO_CAMERA: &str =
    "No camera found. The character keeps breathing and blinking on its own until you add one.";
/// The camera picker.
pub const SETUP_CAMERA: &str = "Camera";
/// The face is tracked.
pub const SETUP_TRACKING: &str = "Following your face";
/// The face is not seen.
pub const SETUP_LOOKING: &str = "Looking for your face";
/// The camera is starting.
pub const SETUP_STARTING: &str = "Starting the camera";
/// Skips the camera.
pub const SETUP_SKIP_FOR_NOW: &str = "Skip for now";
/// Step 3.
pub const SETUP_CALIBRATE: &str = "Calibrate";
/// What step 3 does.
pub const SETUP_CALIBRATE_HOW: &str =
    "Look at the camera with a relaxed face and press Calibrate. Hold still for about five seconds.";
/// Starts a capture.
pub const SETUP_CALIBRATE_GO: &str = "Calibrate";
/// A capture is running.
pub const SETUP_CALIBRATING: &str = "Hold still: {frames} of {of}";
/// A capture finished.
pub const SETUP_CALIBRATED: &str = "Calibrated. Your relaxed face is the character's rest.";
/// A capture gave up.
pub const SETUP_CALIBRATE_FAILED: &str = "Your face was out of view. Face the camera and try again.";
/// Calibrating needs a tracked face.
pub const SETUP_CALIBRATE_NEEDS: &str = "Calibrating needs the camera to see your face";
/// Step 4.
pub const SETUP_OUTPUT: &str = "Send the character to OBS";
/// What step 4 does.
pub const SETUP_OUTPUT_HOW: &str = "Open the output window, then add it in OBS as a Window Capture of \"{title}\".";
/// Opens the output window.
pub const SETUP_OUTPUT_OPEN: &str = "Open the output window";
/// The output window is open.
pub const SETUP_OUTPUT_OPENED: &str = "The output window is open.";
/// Step 5.
pub const SETUP_DONE: &str = "You're set";
/// What now.
pub const SETUP_DONE_HOW: &str =
    "Perform from here, or customise the character first. Setup is in the command palette if you want it again.";
/// Ends the flow in the Perform workspace.
pub const SETUP_PERFORM: &str = "Open Perform";
/// Ends the flow in the Build workspace.
pub const SETUP_CUSTOMISE: &str = "Customise the character";
/// Ends the flow.
pub const SETUP_CLOSE: &str = "Close";
/// What an op the flow ran said when it failed.
pub const SETUP_FAILED: &str = "{op} failed: {message}";

/// `text` in the language the context speaks, with its `{name}` placeholders filled from `args`.
pub fn say<'a>(ds: &Ds, text: &'a str, args: &[(&str, &str)]) -> Cow<'a, str> {
    let text = if ds.strings.locale() == PSEUDO_LOCALE { Cow::Owned(pseudo(text)) } else { Cow::Borrowed(text) };
    if args.is_empty() { text } else { Cow::Owned(fill(&text, args)) }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::ds::strings::{English, Pseudo};
    use crate::ds::theme::{Settings, apply_settings, ds, set_strings};

    #[test]
    fn english_is_the_text_itself_and_the_pseudo_locale_is_not() {
        let ctx = egui::Context::default();
        apply_settings(&ctx, Settings::default());
        assert!(matches!(say(&ds(&ctx), OUTLINER, &[]), Cow::Borrowed(OUTLINER)));
        assert_eq!(say(&ds(&ctx), MANY_SELECTED, &[("count", "3")]), "3 parts selected");
        set_strings(&ctx, Arc::new(Pseudo(English)));
        let pseudo = say(&ds(&ctx), REFUSED, &[("reason", "no")]).into_owned();
        assert!(!pseudo.is_ascii() && pseudo.contains("no") && !pseudo.contains('{'), "{pseudo}");
    }
}
