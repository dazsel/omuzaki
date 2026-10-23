//! The Theatre's own words, each a named constant whose id the lexicon makes from its name (`ui-theatre-pause` for
//! `PAUSE`), said through [`say`](crate::panels::text::say).

crate::ds::strings::lines! {
    prefix = "ui-theatre";

    /// The Theatre without its book: a build without the Theatre's service.
    pub const NO_BOOK: &str = "The Theatre isn't running";
    /// Why.
    pub const NO_BOOK_HOW: &str = "This studio was started without the Theatre's book, so there is nothing to watch.";
    /// No agent has worked yet.
    pub const NOBODY: &str = "No agent is working";
    /// How to start one.
    pub const NOBODY_HOW: &str = "Connect an agent over MCP, or start one from a prompt. Its plan, its steps and the character as it changes show here.";

    /// The plan board's heading for one agent's plan.
    pub const PLAN_OF: &str = "{agent}'s plan";
    /// No plan posted yet.
    pub const NO_PLAN: &str = "No plan yet";
    /// How a plan gets here.
    pub const NO_PLAN_HOW: &str = "An agent shows its plan here when it posts one. Its steps then nest under each item.";
    /// An item's step count.
    pub const ITEM_STEPS: &str = "{count} steps";
    /// An item with one step.
    pub const ITEM_ONE_STEP: &str = "1 step";
    /// A planned item.
    pub const PLANNED: &str = "Planned";
    /// The item being worked on.
    pub const RUNNING: &str = "Running";
    /// An item waiting for the person.
    pub const NEEDS_REVIEW: &str = "Needs review";
    /// A finished item.
    pub const DONE: &str = "Done";
    /// An item a gate failed on.
    pub const FAILED_GATE: &str = "Gate failed";
    /// The screen-reader name of an item's row: choosing it filters the steps.
    pub const ITEM_NAME: &str = "{item}, {status}: show its steps";

    /// The steps list filtered to one item.
    pub const SHOWING_ITEM: &str = "Steps of: {item}";
    /// Clear the filter.
    pub const SHOW_ALL: &str = "Show all steps";
    /// Steps under no plan item.
    pub const OTHER_STEPS: &str = "Other steps";
    /// No step yet.
    pub const NO_STEPS: &str = "No steps yet";
    /// How steps appear.
    pub const NO_STEPS_HOW: &str = "Each change to the character becomes a card here, with its pictures and its reason.";
    /// Show older cards.
    pub const OLDER: &str = "Show {count} older steps";
    /// A step's duration.
    pub const SECONDS: &str = "{seconds} s";
    /// A step that is undone.
    pub const UNDONE: &str = "Undone";
    /// A step's reason, under its card.
    pub const BECAUSE: &str = "Why: {reason}";
    /// No reason given for a step.
    pub const NO_REASON: &str = "No reason given yet. An agent gives one for each step it explains.";
    /// A gate failure's line under its card.
    pub const GATE_LINE: &str = "{gate}: {message}";
    /// Restoring needs app core's op.
    pub const NO_RESTORE: &str = "Showing the character before this step. Restoring it needs history.restore, which this build doesn't have yet.";
    /// The comment field of a rejected step.
    pub const REJECT_WHY: &str = "What should change?";
    /// Send the rejection.
    pub const REJECT_SEND: &str = "Send to the agent";
    /// A rejection's note, as the agent reads it.
    pub const REJECTED_NOTE: &str = "Rejected the step {label}: {comment}";

    /// The agent the controls act on.
    pub const AGENT: &str = "Agent";
    /// Pause the agent.
    pub const PAUSE: &str = "Pause";
    /// Let a paused agent take one step.
    pub const STEP: &str = "Step";
    /// Resume the agent.
    pub const RESUME: &str = "Resume";
    /// Stop the agent.
    pub const STOP: &str = "Stop";
    /// The pause mode's dropdown.
    pub const PAUSE_MODE: &str = "Pause when";
    /// Pause after every step.
    pub const MODE_EVERY_STEP: &str = "After each step";
    /// Pause after each plan item.
    pub const MODE_EACH_ITEM: &str = "After each plan item";
    /// Pause at failed gates only.
    pub const MODE_GATES: &str = "At gates only";
    /// Never pause.
    pub const MODE_OFF: &str = "Never";
    /// The agent is paused.
    pub const PAUSED: &str = "Paused";
    /// The agent is working.
    pub const WORKING: &str = "Working";
    /// The agent has stopped.
    pub const ENDED: &str = "Ended";
    /// The budget meter's spend.
    pub const SPENT_MONEY: &str = "{spent} of {cap}";
    /// Tokens spent, and the cap.
    pub const SPENT_TOKENS: &str = "{spent} of {cap} tokens";
    /// Tokens spent with no cap.
    pub const SPENT_TOKENS_FREE: &str = "{spent} tokens";
    /// Time spent, and the cap.
    pub const SPENT_TIME: &str = "{spent} of {cap}";
    /// Time spent with no cap.
    pub const SPENT_TIME_FREE: &str = "{spent}";
    /// The budget meter's limits button.
    pub const LIMITS: &str = "Limits";
    /// The money limit.
    pub const LIMIT_MONEY: &str = "Money limit, dollars";
    /// The token limit.
    pub const LIMIT_TOKENS: &str = "Token limit (0 for none)";
    /// The time limit.
    pub const LIMIT_MINUTES: &str = "Time limit, minutes (0 for none)";
    /// Save the limits.
    pub const LIMITS_SAVE: &str = "Set limits";
    /// What a limit does.
    pub const LIMITS_HOW: &str = "The agent pauses when it reaches a limit, and asks you before going on.";
    /// A refusal.
    pub const REFUSED: &str = "Not done: {reason}";

    /// The caption of the newest step.
    pub const CAPTION: &str = "{agent}: {label}";
    /// Follow the agent's work.
    pub const FOLLOW: &str = "Following {agent}";
    /// Not following.
    pub const FOLLOW_OFF: &str = "Follow {agent}";
    /// Draw to direct.
    pub const DRAW: &str = "Draw to direct";
    /// How to draw.
    pub const DRAW_HOW: &str = "Drag over the character to circle what should change.";
    /// A note's field.
    pub const NOTE_TEXT: &str = "What should change here?";
    /// Send the notes.
    pub const SEND_NOTES: &str = "Send {count} notes";
    /// Send one note.
    pub const SEND_NOTE: &str = "Send note";
    /// Drop the notes.
    pub const DISCARD_NOTES: &str = "Discard";
    /// The notes reached the agents.
    pub const SENT: &str = "Sent {count} notes to the agents";
    /// Notes not taken up yet.
    pub const OPEN_NOTES: &str = "{count} notes waiting for an agent";
    /// Pop the stage out.
    pub const POP_OUT: &str = "Pop out";
    /// The stage is in its own window.
    pub const POPPED_OUT: &str = "The stage is in its own window";
    /// Bring it back.
    pub const BRING_BACK: &str = "Bring it back";
    /// The pop-out window's title.
    pub const STAGE_WINDOW: &str = "Omuzaki Theatre";

    /// The compare strip's reference.
    pub const REFERENCE: &str = "Reference";
    /// The step before.
    pub const PREVIOUS: &str = "Previous step";
    /// The step on show.
    pub const CURRENT: &str = "Current";
    /// No reference set.
    pub const NO_REFERENCE: &str = "No reference";
    /// Use the current picture as the reference.
    pub const USE_AS_REFERENCE: &str = "Use as reference";
    /// Clear the reference.
    pub const CLEAR_REFERENCE: &str = "Clear reference";
    /// How much changed since the previous step.
    pub const CHANGED_SINCE: &str = "{share} changed since the previous step";
    /// How far from the reference.
    pub const FROM_REFERENCE: &str = "{share} differs from the reference";
    /// The pictures are taken from one camera.
    pub const MATCHED: &str = "Matched camera: the front view every step is pictured from";
    /// No pictures in this studio.
    pub const NO_PICTURES: &str = "No pictures: this studio doesn't take step pictures without a GPU and a project folder.";
    /// A picture not there yet.
    pub const PICTURE_LOADING: &str = "Not pictured yet";
}
