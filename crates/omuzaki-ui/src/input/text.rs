//! The OS input surface's own words, each a named constant whose id the lexicon makes from its name
//! (`ui-input-start` for `START`), said through [`say`](crate::panels::text::say).

crate::ds::strings::lines! {
    prefix = "ui-input";

    /// The badge before capture starts.
    pub const OFF: &str = "Input capture: off";
    /// Start capture (only the person can).
    pub const START: &str = "Start";
    /// What Start does, for its tooltip.
    pub const START_TIP: &str = "Read the keyboard, mouse and gamepad for bindings and motion while Omuzaki shows this";
    /// The badge while capture runs: which devices, and how much of a key the motion sees.
    pub const ON: &str = "Reading {classes}: {detail}";
    /// Devices asked for that no backend serves now.
    pub const DEGRADED: &str = "{classes} unavailable";
    /// Stop capture.
    pub const STOP: &str = "Stop";
    /// Resume a paused capture.
    pub const RESUME: &str = "Resume";
    /// Paused by an actor (a person, an agent, a script).
    pub const PAUSED: &str = "Input capture: paused";
    /// Paused because the indicator was not drawn.
    pub const PAUSED_HIDDEN: &str = "Input capture: paused while Omuzaki was hidden";
    /// Paused because a protected program runs.
    pub const PAUSED_PROGRAM: &str = "Input capture: paused while {program} runs";
    /// Paused for a protected program the hub did not name.
    pub const PAUSED_PROTECTED: &str = "Input capture: paused while a protected program runs";
    /// Started, but no backend runs.
    pub const FAILED: &str = "Input capture failed: {reason}";
    /// An op the badge or the panel ran was refused.
    pub const REFUSED: &str = "{action} did not run: {reason}";

    /// Keys read as their zone.
    pub const DETAIL_ZONES: &str = "zones only";
    /// Keys read as their side.
    pub const DETAIL_HANDS: &str = "hands only";
    /// A list of two device classes.
    pub const AND: &str = "{first} and {last}";
    /// Every device class at once.
    pub const EVERY_DEVICE: &str = "every device";
    /// The keyboard.
    pub const KEYBOARD: &str = "keyboard";
    /// The mouse.
    pub const MOUSE: &str = "mouse";
    /// A pen or tablet.
    pub const PEN: &str = "pen";
    /// A gamepad.
    pub const GAMEPAD: &str = "gamepad";
    /// MIDI controllers.
    pub const MIDI: &str = "MIDI";
    /// OSC messages.
    pub const OSC: &str = "OSC";

    /// The panel's capture section.
    pub const CAPTURE: &str = "Capture";
    /// A backend that waits for macOS Input Monitoring.
    pub const NEEDS_MONITORING: &str = "macOS needs Input Monitoring: open System Settings, Privacy and Security, Input Monitoring, turn Omuzaki on, then check again.";
    /// A backend that waits for the Linux input group.
    pub const NEEDS_GROUP: &str = "Linux needs you in the input group to read the keyboard on Wayland: add yourself to it, sign out and in, then check again.";
    /// Read the state again.
    pub const CHECK_AGAIN: &str = "Check again";
    /// A backend that failed while others run.
    pub const BACKEND_FAILED: &str = "{backend} stopped: {reason}";
    /// What works on this machine, per class.
    pub const SUPPORT: &str = "{class}: {support}";
    /// It needs an opt-in.
    pub const SUPPORT_OPT_IN: &str = "needs an opt-in";
    /// It needs a permission.
    pub const SUPPORT_PERMISSION: &str = "needs a permission";
    /// It goes through the desktop portal.
    pub const SUPPORT_PORTAL: &str = "through the desktop portal";
    /// Not here.
    pub const SUPPORT_NO: &str = "not on this system";

    /// The panel's bindings section.
    pub const BINDINGS: &str = "Bindings";
    /// No binding yet.
    pub const NO_BINDINGS: &str = "No bindings yet";
    /// How to get some.
    pub const NO_BINDINGS_HOW: &str = "Add a preset below, or bind an expression from the Live panel's hotkeys.";
    /// A binding with no keys yet.
    pub const NO_KEYS: &str = "No keys";
    /// The keys column, for a screen reader.
    pub const KEYS_OF: &str = "Keys for {name}";
    /// Learn a binding's keys.
    pub const SET_KEYS: &str = "Set keys";
    /// Learn the keys of this binding, by name.
    pub const SET_KEYS_FOR: &str = "Set keys for {name}";
    /// Waiting for the keys.
    pub const PRESS_KEYS: &str = "Press the keys for {name}";
    /// Forget a binding's keys.
    pub const CLEAR_KEYS: &str = "Clear keys for {name}";
    /// Remove a binding.
    pub const REMOVE: &str = "Remove {name}";
    /// The visible word of a Clear button.
    pub const CLEAR_SHORT: &str = "Clear";
    /// The visible word of a Remove button.
    pub const REMOVE_SHORT: &str = "Remove";
    /// Two bindings share keys.
    pub const CONFLICT: &str = "Shares its keys with another binding";
    /// Learning reads the keyboard, so capture must be on.
    pub const NOT_CAPTURING: &str = "Setting keys reads your keyboard, so it needs input capture on: start it under Capture, then set the keys.";
    /// The keys were not learnt.
    pub const LEARN_FAILED: &str = "No keys were set: {reason}";
    /// Add a preset of bindings.
    pub const ADD_PRESET: &str = "Add the {preset} preset";
    /// What a binding does: an op.
    pub const ACTION_OP: &str = "Runs {op}";
    /// While held.
    pub const ACTION_HOLD: &str = "Holds {op}";
    /// A trigger slot.
    pub const ACTION_TRIGGER: &str = "Trigger {n}";
    /// Locomotion.
    pub const ACTION_MOVE: &str = "Move";
    /// Run.
    pub const ACTION_RUN: &str = "Run";
    /// Jump.
    pub const ACTION_JUMP: &str = "Jump";
    /// Sit.
    pub const ACTION_SIT: &str = "Sit";
    /// An emote.
    pub const ACTION_EMOTE: &str = "Emote {n}";
    /// Something newer than this build.
    pub const ACTION_OTHER: &str = "Another action";

    /// The panel's protected-programs section.
    pub const PROTECTED: &str = "Protected programs";
    /// What they do.
    pub const PROTECTED_HOW: &str = "Capture pauses while one of these runs, and resumes when it closes. Only you see this list.";
    /// The program name field.
    pub const PROGRAM: &str = "Program name";
    /// Its hint.
    pub const PROGRAM_HINT: &str = "For example, a password manager";
    /// Add it.
    pub const ADD: &str = "Add";
    /// Remove a program.
    pub const REMOVE_PROGRAM: &str = "Remove {program}";
    /// No program yet.
    pub const NO_PROGRAMS: &str = "No programs yet";

    /// Without an input hub.
    pub const NO_HUB: &str = "OS input isn't available";
    /// Why.
    pub const NO_HUB_HOW: &str = "This studio started without the input hub, so bindings and capture are off.";
}
