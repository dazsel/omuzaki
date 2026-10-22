//! Icons: Phosphor Regular by default, Fill for "on" (`design-system.md` section 5).
//!
//! The glyphs are the `egui-phosphor` constants, so no code point is ever typed by hand. Regular and Fill share code
//! points: [`TypeScale::icon`](super::theme::TypeScale::icon) picks the weight, which gives state a shape cue as well
//! as a colour (a visible eye is a solid eye, a hidden one a slashed eye).
//!
//! Domain icons (face profile, brow, mouth, fringe, outfit, expression, channel, op, agent, theatre, gate) are drawn
//! by us as SVG and compiled into a second icon font by an `xtask`; that pipeline is a separate task.

/// A visible part.
pub const EYE: &str = egui_phosphor::regular::EYE;
/// A hidden part.
pub const EYE_SLASH: &str = egui_phosphor::regular::EYE_SLASH;
/// Fenced by an agent or locked by you.
pub const LOCK: &str = egui_phosphor::regular::LOCK;
/// Unlocked.
pub const LOCK_OPEN: &str = egui_phosphor::regular::LOCK_OPEN;
/// Passed, on.
pub const CHECK: &str = egui_phosphor::regular::CHECK;
/// Success.
pub const CHECK_CIRCLE: &str = egui_phosphor::regular::CHECK_CIRCLE;
/// Close, clear, remove.
pub const X: &str = egui_phosphor::regular::X;
/// Error.
pub const X_CIRCLE: &str = egui_phosphor::regular::X_CIRCLE;
/// A failing gate, a warning triangle.
pub const WARNING: &str = egui_phosphor::regular::WARNING;
/// A warning.
pub const WARNING_CIRCLE: &str = egui_phosphor::regular::WARNING_CIRCLE;
/// Information.
pub const INFO: &str = egui_phosphor::regular::INFO;
/// A collapsed section or tree row.
pub const CARET_RIGHT: &str = egui_phosphor::regular::CARET_RIGHT;
/// An expanded section; a dropdown.
pub const CARET_DOWN: &str = egui_phosphor::regular::CARET_DOWN;
/// Scroll or sort up.
pub const CARET_UP: &str = egui_phosphor::regular::CARET_UP;
/// A dropdown.
pub const CARET_UP_DOWN: &str = egui_phosphor::regular::CARET_UP_DOWN;
/// Reset to default.
pub const ARROW_COUNTER_CLOCKWISE: &str = egui_phosphor::regular::ARROW_COUNTER_CLOCKWISE;
/// Redo.
pub const ARROW_CLOCKWISE: &str = egui_phosphor::regular::ARROW_CLOCKWISE;
/// Search.
pub const MAGNIFYING_GLASS: &str = egui_phosphor::regular::MAGNIFYING_GLASS;
/// Pick a colour from the screen.
pub const EYEDROPPER: &str = egui_phosphor::regular::EYEDROPPER;
/// A linked palette colour.
pub const LINK: &str = egui_phosphor::regular::LINK;
/// Unlink.
pub const LINK_BREAK: &str = egui_phosphor::regular::LINK_BREAK;
/// Add.
pub const PLUS: &str = egui_phosphor::regular::PLUS;
/// Remove.
pub const MINUS: &str = egui_phosphor::regular::MINUS;
/// A more menu.
pub const DOTS_THREE: &str = egui_phosphor::regular::DOTS_THREE;
/// The notification log.
pub const BELL: &str = egui_phosphor::regular::BELL;
/// An agent.
pub const ROBOT: &str = egui_phosphor::regular::ROBOT;
/// Run, resume.
pub const PLAY: &str = egui_phosphor::regular::PLAY;
/// Pause.
pub const PAUSE: &str = egui_phosphor::regular::PAUSE;
/// Stop.
pub const STOP: &str = egui_phosphor::regular::STOP;
/// A dot.
pub const CIRCLE: &str = egui_phosphor::regular::CIRCLE;
/// A part or mesh.
pub const CUBE: &str = egui_phosphor::regular::CUBE;
/// The outliner.
pub const TREE_STRUCTURE: &str = egui_phosphor::regular::TREE_STRUCTURE;
/// The inspector.
pub const SLIDERS_HORIZONTAL: &str = egui_phosphor::regular::SLIDERS_HORIZONTAL;
/// Colours.
pub const PALETTE: &str = egui_phosphor::regular::PALETTE;
/// Paint.
pub const PAINT_BRUSH: &str = egui_phosphor::regular::PAINT_BRUSH;
/// Undo.
pub const ARROW_U_UP_LEFT: &str = egui_phosphor::regular::ARROW_U_UP_LEFT;
/// Redo.
pub const ARROW_U_UP_RIGHT: &str = egui_phosphor::regular::ARROW_U_UP_RIGHT;
/// Time, age.
pub const CLOCK: &str = egui_phosphor::regular::CLOCK;
/// Money.
pub const COINS: &str = egui_phosphor::regular::COINS;
/// A meter, performance.
pub const GAUGE: &str = egui_phosphor::regular::GAUGE;
/// A passed gate.
pub const SHIELD_CHECK: &str = egui_phosphor::regular::SHIELD_CHECK;
/// A failed gate.
pub const SHIELD_WARNING: &str = egui_phosphor::regular::SHIELD_WARNING;
/// A person.
pub const USER: &str = egui_phosphor::regular::USER;
/// Settings.
pub const GEAR: &str = egui_phosphor::regular::GEAR;
/// A folder.
pub const FOLDER: &str = egui_phosphor::regular::FOLDER;
/// An image.
pub const IMAGE: &str = egui_phosphor::regular::IMAGE;
/// An op or a quick action.
pub const LIGHTNING: &str = egui_phosphor::regular::LIGHTNING;
/// Generated or assisted.
pub const SPARKLE: &str = egui_phosphor::regular::SPARKLE;
/// Stop, needs you.
pub const HAND_PALM: &str = egui_phosphor::regular::HAND_PALM;
/// Go live.
pub const BROADCAST: &str = egui_phosphor::regular::BROADCAST;
/// Recording.
pub const RECORD: &str = egui_phosphor::regular::RECORD;
/// Audio level.
pub const WAVEFORM: &str = egui_phosphor::regular::WAVEFORM;
/// A live signal.
pub const ACTIVITY: &str = egui_phosphor::regular::ACTIVITY;
/// A key cap or credential.
pub const KEY: &str = egui_phosphor::regular::KEY;
/// The command key.
pub const COMMAND: &str = egui_phosphor::regular::COMMAND;
/// A comment or reason.
pub const CHAT_CIRCLE_TEXT: &str = egui_phosphor::regular::CHAT_CIRCLE_TEXT;
/// Next.
pub const ARROW_RIGHT: &str = egui_phosphor::regular::ARROW_RIGHT;
/// Back.
pub const ARROW_LEFT: &str = egui_phosphor::regular::ARROW_LEFT;
/// A marker.
pub const FLAG: &str = egui_phosphor::regular::FLAG;
/// Delete.
pub const TRASH: &str = egui_phosphor::regular::TRASH;
/// Copy.
pub const COPY: &str = egui_phosphor::regular::COPY;
/// A face or expression.
pub const SMILEY: &str = egui_phosphor::regular::SMILEY;
/// Hair.
pub const HAIR_DRYER: &str = egui_phosphor::regular::HAIR_DRYER;
/// Outfit.
pub const T_SHIRT: &str = egui_phosphor::regular::T_SHIRT;
/// The Enter key on a key cap.
pub const ENTER: &str = egui_phosphor::regular::ARROW_ELBOW_DOWN_LEFT;
/// The Backspace key on a key cap.
pub const BACKSPACE: &str = egui_phosphor::regular::BACKSPACE;
/// A camera tracker.
pub const WEBCAM: &str = egui_phosphor::regular::WEBCAM;
/// The life layer.
pub const HEARTBEAT: &str = egui_phosphor::regular::HEARTBEAT;
/// A hotkey.
pub const KEYBOARD: &str = egui_phosphor::regular::KEYBOARD;
/// A phone as a tracker (iPhone face tracking).
pub const DEVICE_MOBILE: &str = egui_phosphor::regular::DEVICE_MOBILE;
/// A model file brought in from elsewhere.
pub const UPLOAD: &str = egui_phosphor::regular::UPLOAD_SIMPLE;
/// A picture sent to another program (an output).
pub const MONITOR: &str = egui_phosphor::regular::MONITOR_PLAY;
/// A step still to come.
pub const CIRCLE_DASHED: &str = egui_phosphor::regular::CIRCLE_DASHED;

/// The icon that goes with a status: the shape half of "icon, word and colour" (P9).
pub const fn status(status: super::tokens::Status) -> &'static str {
    use super::tokens::Status;
    match status {
        Status::Info => INFO,
        Status::Ok => CHECK_CIRCLE,
        Status::Warn => WARNING,
        Status::Danger => X_CIRCLE,
    }
}
