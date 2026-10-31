//! The edit panels' own words.
//!
//! They are written in English here, each a named constant whose id the lexicon makes from its name
//! (`ds::lexicon`, `ui-panels-outliner` for `OUTLINER`), and passed through [`say`], which gives the text in the
//! language the design system speaks: the translation when the language has one, the English otherwise, and the
//! accented, longer version in the pseudo-locale, where a plain ASCII word on screen is one that skipped this table.

use std::borrow::Cow;

use crate::ds::strings::{fill, local};
use crate::ds::theme::Ds;

crate::ds::strings::lines! {
    prefix = "ui-panels";

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
    /// Where an agent connects to this studio: its MCP address.
    pub const AGENT_NONE_WHERE: &str = "Point an MCP client at {url}, or run omuzaki mcp in the project's folder.";
    /// A session's step count.
    pub const AGENT_STEPS: &str = "{steps} steps";
    /// A session's single step.
    pub const AGENT_ONE_STEP: &str = "1 step";
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
    /// Undo a session's single step.
    pub const AGENT_UNDO_ONE: &str = "Undo its step\u{2026}";
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
    /// What it does, for a single step.
    pub const UNDO_BODY_ONE: &str = "Its step is reverted as one step in your history, so Ctrl+Z brings it back.";
    /// The undo button.
    pub const UNDO_PRIMARY: &str = "Undo {steps} steps";
    /// The undo button, for a single step.
    pub const UNDO_PRIMARY_ONE: &str = "Undo 1 step";
    /// Its armed second step.
    pub const UNDO_CONFIRM: &str = "Confirm: undo {steps} steps";
    /// Its armed second step, for a single step.
    pub const UNDO_CONFIRM_ONE: &str = "Confirm: undo 1 step";
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
    /// The preview's change count, for one change.
    pub const APPROVAL_CHANGE: &str = "1 value";
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
    /// One unresolved comment thread on what the request touches.
    pub const REVIEW_THREAD_ONE: &str = "1 open comment thread";
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
    /// While History shows a past state in the viewport.
    pub const VIEWING_PAST: &str = "Viewing a past state: after {label}";
    /// The way back from it.
    pub const BACK_TO_NOW: &str = "Back to now";
    /// The viewport without a GPU to draw on.
    pub const NO_GPU: &str = "The viewport needs a GPU";
    /// What to do about it.
    pub const NO_GPU_HOW: &str = "Start the app with a Vulkan, Metal or DirectX 12 adapter.";
    /// The character doesn't build.
    pub const UNBUILT: &str = "The character doesn't build: {reason}";
    /// What the viewport's frame readout says.
    pub const READOUT: &str = "{ms} ms GPU";

    /// The first-run flow's panel (`shell.md` section 14): which step of how many.
    pub const SETUP_HEADER: &str = "Setup \u{b7} {n} of {of}";
    /// Leaves the flow at any step.
    pub const SETUP_SKIP: &str = "Skip setup";
    /// The previous step.
    pub const SETUP_BACK: &str = "Back";
    /// Why Back is off.
    pub const SETUP_BACK_FIRST: &str = "This is the first step";
    /// The next step.
    pub const SETUP_CONTINUE: &str = "Continue";
    /// Why Continue is off on Input.
    pub const SETUP_CHOOSE_INPUT: &str = "Choose how to track your face first";
    /// How long a step takes.
    pub const SETUP_ABOUT: &str = "About {seconds} s";
    /// The project line under the footer.
    pub const SETUP_PROJECT_LINE: &str = "Sample project \u{b7} not saved yet \u{b7} Setup can be reopened from the menu";
    /// A step in the step list, as a screen reader says it.
    pub const SETUP_STEP_STATE: &str = "Step {n}, {name}: {state}";
    /// Step names in the list.
    pub const SETUP_STEP_WELCOME: &str = "Welcome";
    /// Step names in the list.
    pub const SETUP_STEP_CHARACTER: &str = "Character";
    /// Step names in the list.
    pub const SETUP_STEP_INPUT: &str = "Input";
    /// Step names in the list.
    pub const SETUP_STEP_CALIBRATE: &str = "Calibrate";
    /// Step names in the list.
    pub const SETUP_STEP_OUTPUT: &str = "Output";
    /// Step names in the list.
    pub const SETUP_STEP_GO_LIVE: &str = "Go live";
    /// A step's state in the list: the word beside the shape and colour.
    pub const SETUP_STATE_DONE: &str = "done";
    /// A step's state in the list.
    pub const SETUP_STATE_CURRENT: &str = "current";
    /// A step's state in the list.
    pub const SETUP_STATE_SKIPPED: &str = "skipped";
    /// A step's state in the list.
    pub const SETUP_STATE_NEXT: &str = "to do";
    /// Welcome's title.
    pub const SETUP_WELCOME: &str = "Welcome to Omuzaki";
    /// What Welcome promises.
    pub const SETUP_WELCOME_LINE: &str = "Your camera never leaves this computer. Telemetry is off.";
    /// What the flow does and how long it takes.
    pub const SETUP_WELCOME_HOW: &str =
        "In about two minutes: pick a character, let the camera follow your face, and send the character to OBS.";
    /// Language and theme.
    pub const SETUP_FOLLOWS_SYSTEM: &str = "Language and theme follow your system. Change them any time in Settings.";
    /// Starts the flow.
    pub const SETUP_START: &str = "Start";
    /// Welcome's assistant offer, with an assistant configured.
    pub const SETUP_ASSISTANT: &str = "Let an assistant set this up";
    /// Welcome's assistant offer, without one.
    pub const SETUP_CONNECT: &str = "Connect an agent";
    /// Character's title.
    pub const SETUP_CHARACTER: &str = "Choose your character";
    /// Character's line.
    pub const SETUP_CHARACTER_HOW: &str = "You can change everything about it later.";
    /// The Keep card, on a rerun.
    pub const SETUP_KEEP: &str = "Keep this character";
    /// The Keep card's line.
    pub const SETUP_KEEP_LINE: &str = "Leaves the project as it is.";
    /// The sample card.
    pub const SETUP_SAMPLE: &str = "Use the sample";
    /// The sample card's line.
    pub const SETUP_SAMPLE_LINE: &str = "Ready now. It is already on the stage.";
    /// The sheet card.
    pub const SETUP_SHEET: &str = "Describe a character";
    /// The sheet card's line.
    pub const SETUP_SHEET_LINE: &str = "Say who they are in a sentence, and the kit builds them. Change anything after.";
    /// The describe box's label.
    pub const SETUP_DESCRIBE_LABEL: &str = "Describe your character";
    /// The describe box's placeholder.
    pub const SETUP_DESCRIBE_HINT: &str = "A sleepy librarian with long silver hair, round glasses and a blue cardigan";
    /// The describe box's button.
    pub const SETUP_DESCRIBE_GO: &str = "Make it";
    /// Why the button is off.
    pub const SETUP_DESCRIBE_EMPTY: &str = "Write a few words first";
    /// What the words made.
    pub const SETUP_DESCRIBED: &str = "Made {name} from {count} choices";
    /// The words the kit didn't read.
    pub const SETUP_DESCRIBED_UNKNOWN: &str = "Not used yet: {words}";
    /// The import card.
    pub const SETUP_IMPORT: &str = "Import a VRM";
    /// The import card's line.
    pub const SETUP_IMPORT_LINE: &str = "Bring your own model from VRoid Studio or another tool.";
    /// Why a card is off.
    pub const SETUP_NOT_IN_BUILD: &str = "Not in this version yet";
    /// The import card's drop zone.
    pub const SETUP_VRM: &str = "Drop a .vrm file here";
    /// The imported file.
    pub const SETUP_VRM_DONE: &str = "Using {file}";
    /// Input's title.
    pub const SETUP_INPUT: &str = "How will you track your face?";
    /// Input's line.
    pub const SETUP_INPUT_HOW: &str = "Your camera feed stays on this computer.";
    /// The webcam card.
    pub const SETUP_WEBCAM: &str = "Webcam";
    /// The webcam card's line.
    pub const SETUP_WEBCAM_LINE: &str = "Ready now. Good for most streams.";
    /// Why the webcam card is off.
    pub const SETUP_NO_WEBCAM: &str = "No camera to use";
    /// The face is found.
    pub const SETUP_FACE_FOUND: &str = "Face found \u{b7} {camera}";
    /// The face is not found yet.
    pub const SETUP_FACE_LOOKING: &str = "Looking for your face \u{b7} {camera}";
    /// The camera is starting.
    pub const SETUP_FACE_STARTING: &str = "Starting the camera \u{b7} {camera}";
    /// The camera picker (more than one camera).
    pub const SETUP_CAMERA: &str = "Camera";
    /// The iPhone card.
    pub const SETUP_IPHONE: &str = "iPhone";
    /// The iPhone card's line.
    pub const SETUP_IPHONE_LINE: &str = "Best mouth and tongue detail. Free tracker app.";
    /// The iPhone card's status.
    pub const SETUP_IPHONE_PAIR: &str = "Pair in about a minute";
    /// The life-layer card.
    pub const SETUP_SKIP_FOR_NOW: &str = "Skip for now";
    /// The life-layer card's line.
    pub const SETUP_SKIP_FOR_NOW_LINE: &str = "The avatar will idle with life animation.";
    /// Why the camera prompt comes.
    pub const SETUP_PERMISSION_WHY: &str =
        "Continue asks your system for the camera, so the avatar can follow your face. The picture stays on this computer.";
    /// The quick check's title.
    pub const SETUP_QUICK_CHECK: &str = "Quick check";
    /// The quick check.
    pub const SETUP_QUICK_CHECK_LINE: &str = "Blink, open your mouth, smile: the avatar should follow.";
    /// No camera.
    pub const SETUP_NOTE_NO_CAMERA: &str = "No camera found. Use an iPhone, or skip for now and the avatar idles.";
    /// The OS refused the camera.
    pub const SETUP_NOTE_DENIED: &str =
        "Omuzaki can't use the camera. Allow it in your system's privacy settings, or use an iPhone, or skip for now.";
    /// The tracker is downloading.
    pub const SETUP_NOTE_DOWNLOADING: &str =
        "Getting the face tracker: {percent}%. You can continue; the avatar idles until it is ready.";
    /// The tracker is not available.
    pub const SETUP_NOTE_TRACKER: &str =
        "The face tracker isn't available. Continue with the avatar idling, or use an iPhone.";
    /// No iPhone paired.
    pub const SETUP_NOTE_IPHONE: &str = "No iPhone is paired yet. Pair it from the Live panel, or pick another way.";
    /// The source didn't start.
    pub const SETUP_NOTE_INPUT: &str = "That didn't start. Continue tries again.";
    /// The output didn't open.
    pub const SETUP_NOTE_OUTPUT: &str = "The output didn't open. Choose it again, or Later.";
    /// The character choice didn't work.
    pub const SETUP_NOTE_CHARACTER: &str = "That didn't work. Choose again.";
    /// Calibration didn't finish.
    pub const SETUP_NOTE_CALIBRATION: &str = "Calibration didn't finish. Continue tries again; Use defaults moves on.";
    /// No output on this system.
    pub const SETUP_NOTE_NO_OUTPUT: &str = "This system offers no output Omuzaki can use yet. Later moves on.";
    /// Calibrate's title.
    pub const SETUP_CALIBRATE: &str = "Calibrate your face";
    /// What Calibrate does.
    pub const SETUP_CALIBRATE_HOW: &str =
        "Look at the camera with a relaxed face and press Continue. Hold still for about five seconds.";
    /// A capture runs.
    pub const SETUP_CALIBRATING: &str = "Hold still: {frames} of {of}";
    /// Calibrated.
    pub const SETUP_CALIBRATED: &str = "Calibrated";
    /// What calibrating did.
    pub const SETUP_CALIBRATED_LINE: &str = "Your relaxed face is now the character's rest.";
    /// Skips calibrating.
    pub const SETUP_USE_DEFAULTS: &str = "Use defaults";
    /// What the defaults are.
    pub const SETUP_USE_DEFAULTS_LINE: &str = "The defaults suit most faces; calibrate any time from the Live panel.";
    /// Output's title.
    pub const SETUP_OUTPUT: &str = "Send the character to your stream";
    /// Output's line.
    pub const SETUP_OUTPUT_HOW: &str =
        "One way that works on this computer. Choosing it opens it, so you can check it in OBS.";
    /// The output is on.
    pub const SETUP_OUTPUT_ON: &str = "On: add it in OBS";
    /// The window output.
    pub const SETUP_OUT_WINDOW: &str = "Output window";
    /// The window output's line.
    pub const SETUP_OUT_WINDOW_LINE: &str = "A clean window called “{title}” that OBS captures, with transparency.";
    /// Spout.
    pub const SETUP_OUT_SPOUT: &str = "Spout";
    /// Spout's line.
    pub const SETUP_OUT_SPOUT_LINE: &str = "Shares the picture with OBS on Windows, with transparency.";
    /// Syphon.
    pub const SETUP_OUT_SYPHON: &str = "Syphon";
    /// Syphon's line.
    pub const SETUP_OUT_SYPHON_LINE: &str = "Shares the picture with OBS on macOS, with transparency.";
    /// NDI.
    pub const SETUP_OUT_NDI: &str = "NDI";
    /// NDI's line.
    pub const SETUP_OUT_NDI_LINE: &str = "Sends the picture over your network.";
    /// The virtual camera.
    pub const SETUP_OUT_CAMERA: &str = "Virtual camera";
    /// The virtual camera's line.
    pub const SETUP_OUT_CAMERA_LINE: &str = "Shows up as a camera in OBS, Discord or a video call.";
    /// The OBS instructions' title.
    pub const SETUP_OBS: &str = "OBS instructions";
    /// OBS, step 1.
    pub const SETUP_OBS_1: &str = "1. In OBS, under Sources, press + and choose Window Capture.";
    /// OBS, step 2.
    pub const SETUP_OBS_2: &str = "2. Pick the window \"{title}\".";
    /// OBS, step 3.
    pub const SETUP_OBS_3: &str = "3. Turn on Allow Transparency if your OBS shows it.";
    /// Skips the output.
    pub const SETUP_LATER: &str = "Later";
    /// Go-live's title.
    pub const SETUP_DONE: &str = "You're set";
    /// What now.
    pub const SETUP_DONE_HOW: &str = "Perform from here, or customise the character first.";
    /// The health meter.
    pub const SETUP_HEALTH: &str = "{ready} of {of} ready";
    /// Health: the character.
    pub const SETUP_CHECK_CHARACTER: &str = "Character on the stage";
    /// Health: tracking.
    pub const SETUP_CHECK_TRACKING: &str = "The camera follows your face";
    /// Health: no tracking.
    pub const SETUP_CHECK_IDLE: &str = "No tracking: the avatar idles";
    /// Health: calibrated.
    pub const SETUP_CHECK_CALIBRATED: &str = "Calibrated to your face";
    /// Health: not calibrated.
    pub const SETUP_CHECK_DEFAULTS: &str = "Calibration: the defaults";
    /// Health: an output.
    pub const SETUP_CHECK_OUTPUT: &str = "Output on";
    /// Health: no output.
    pub const SETUP_CHECK_NO_OUTPUT: &str = "No output yet";
    /// Health: the layout.
    pub const SETUP_CHECK_LAYOUT: &str = "Layout saved";
    /// Ends the flow in the Perform workspace.
    pub const SETUP_PERFORM: &str = "Open Perform";
    /// Ends the flow in the Build workspace.
    pub const SETUP_CUSTOMISE: &str = "Customise the character";
    /// Ends the flow.
    pub const SETUP_CLOSE: &str = "Close";
    /// What an op the flow ran said when it failed.
    pub const SETUP_FAILED: &str = "{op} failed: {message}";
}

/// A preview's change count in words: "1 value", "3 values".
pub fn changes(ds: &Ds, count: usize) -> String {
    if count == 1 {
        say(ds, APPROVAL_CHANGE, &[]).into_owned()
    } else {
        say(ds, APPROVAL_CHANGES, &[("count", &count.to_string())]).into_owned()
    }
}

/// `text` in the language the context speaks, with its `{name}` placeholders filled from `args`.
pub fn say<'a>(ds: &Ds, text: &'a str, args: &[(&str, &str)]) -> Cow<'a, str> {
    let text = local(&*ds.strings, text);
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
