//! Capability scopes (spec §6): what an actor is allowed to do.

use std::fmt;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A capability. Serialised as its spec name (`edit:character`), which is also what users see in grant prompts.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
pub enum Scope {
    /// Read anything in the project.
    #[serde(rename = "read")]
    Read,
    /// Edit characters (parts, face, rig, look).
    #[serde(rename = "edit:character")]
    EditCharacter,
    /// Edit scenes, props, lights and cameras.
    #[serde(rename = "edit:scene")]
    EditScene,
    /// Edit the library.
    #[serde(rename = "edit:library")]
    EditLibrary,
    /// Change the person's settings, profiles and session list (never project data).
    #[serde(rename = "edit:settings")]
    EditSettings,
    /// Render previews and offline renders.
    #[serde(rename = "render")]
    Render,
    /// Drive the avatar live.
    #[serde(rename = "perform")]
    Perform,
    /// Go live, switch scenes.
    #[serde(rename = "broadcast")]
    Broadcast,
    /// Export or upload anything outside the machine.
    #[serde(rename = "publish")]
    Publish,
    /// Use paid connectors, within a budget.
    #[serde(rename = "spend")]
    Spend,
    /// Network access.
    #[serde(rename = "net")]
    Net,
    /// File access outside the project, and any file write. The paths themselves are checked against the roots in
    /// [`Grants`](crate::Grants), by [`OpCtx::fs_access`](crate::OpCtx::fs_access).
    #[serde(rename = "fs")]
    Fs,
    /// Load native code.
    #[serde(rename = "native")]
    Native,
    /// Capture OS keyboard, mouse, pen or gamepad input while another app has focus.
    #[serde(rename = "input")]
    Input,
    /// Lets this start named programs on this computer and check whether a named program is already running.
    /// Separate from `native`, so launching a program (OBS, say) does not allow loading plugins or native code.
    #[serde(rename = "process")]
    Process,
    /// Start a script run or save a script. Inner requests of a run are still checked against their own scopes.
    #[serde(rename = "script")]
    Script,
    /// Comment on a review and reply to a comment (ADR 0056). Writes only plain `comments/` nodes: it does not allow
    /// resolving, reopening, deleting or importing comments, creating tasks, or approving anything (those are
    /// person-only ops, and approving is never a scope), and it does not allow editing a character, a scene or the
    /// library.
    #[serde(rename = "review")]
    Review,
}

impl Scope {
    /// Every scope, in a stable order (for docs and grant UIs).
    pub const ALL: [Scope; 17] = [
        Scope::Read,
        Scope::EditCharacter,
        Scope::EditScene,
        Scope::EditLibrary,
        Scope::EditSettings,
        Scope::Render,
        Scope::Perform,
        Scope::Broadcast,
        Scope::Publish,
        Scope::Spend,
        Scope::Net,
        Scope::Fs,
        Scope::Native,
        Scope::Input,
        Scope::Process,
        Scope::Script,
        Scope::Review,
    ];

    /// Whether this scope reaches outside the project. A grant prompt and a scope badge show these as risky (warning
    /// icon, heavier border, two-step confirm):
    /// - `broadcast`: sends the avatar's output to a live audience.
    /// - `publish`: exports or uploads anything outside the machine.
    /// - `spend`: uses paid connectors.
    /// - `native`: loads native code, which runs with the app's full rights.
    /// - `input`: reads keystrokes and pointer input from other apps.
    /// - `process`: lets this start named programs on this computer and check whether a named program is already running.
    pub const fn is_risky(self) -> bool {
        matches!(self, Scope::Broadcast | Scope::Publish | Scope::Spend | Scope::Native | Scope::Input | Scope::Process)
    }

    /// The spec name, e.g. `edit:character`.
    pub const fn name(self) -> &'static str {
        match self {
            Scope::Read => "read",
            Scope::EditCharacter => "edit:character",
            Scope::EditScene => "edit:scene",
            Scope::EditLibrary => "edit:library",
            Scope::EditSettings => "edit:settings",
            Scope::Render => "render",
            Scope::Perform => "perform",
            Scope::Broadcast => "broadcast",
            Scope::Publish => "publish",
            Scope::Spend => "spend",
            Scope::Net => "net",
            Scope::Fs => "fs",
            Scope::Native => "native",
            Scope::Input => "input",
            Scope::Process => "process",
            Scope::Script => "script",
            Scope::Review => "review",
        }
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for Scope {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Scope::ALL.into_iter().find(|sc| sc.name() == s).ok_or_else(|| format!("unknown scope {s:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip_through_serde_and_from_str() {
        for s in Scope::ALL {
            assert_eq!(s.name().parse::<Scope>().unwrap(), s);
            assert_eq!(serde_json::to_string(&s).unwrap(), format!("\"{}\"", s.name()));
        }
        assert!("edit:everything".parse::<Scope>().is_err());
    }

    #[test]
    fn risky_scopes_are_exactly_the_ones_that_reach_outside() {
        let risky: Vec<&str> = Scope::ALL.into_iter().filter(|s| s.is_risky()).map(Scope::name).collect();
        assert_eq!(risky, ["broadcast", "publish", "spend", "native", "input", "process"]);
    }

    #[test]
    fn edit_settings_round_trips_and_sits_after_edit_library() {
        let names: Vec<&str> = Scope::ALL.iter().map(|s| s.name()).collect();
        let at = names.iter().position(|n| *n == "edit:library").unwrap();
        assert_eq!(names[at + 1], "edit:settings");
        assert_eq!(names.iter().filter(|n| **n == "edit:settings").count(), 1);
        assert_eq!("edit:settings".parse::<Scope>().unwrap(), Scope::EditSettings);
        assert_eq!(serde_json::to_string(&Scope::EditSettings).unwrap(), "\"edit:settings\"");
        assert!(!Scope::EditSettings.is_risky());
    }

    #[test]
    fn input_and_process_round_trip() {
        for (scope, name) in [(Scope::Input, "input"), (Scope::Process, "process")] {
            assert_eq!(scope.name(), name);
            assert_eq!(scope.to_string(), name);
            assert_eq!(name.parse::<Scope>().unwrap(), scope);
            assert_eq!(serde_json::to_string(&scope).unwrap(), format!("\"{name}\""));
            assert_eq!(serde_json::from_str::<Scope>(&format!("\"{name}\"")).unwrap(), scope);
        }
        assert!([Scope::Input, Scope::Process].iter().all(|s| Scope::ALL.contains(s)));
        assert!("Input".parse::<Scope>().is_err());
        assert!("process ".parse::<Scope>().is_err());
    }

    #[test]
    fn review_round_trips_and_is_not_risky() {
        assert_eq!(Scope::Review.name(), "review");
        assert_eq!(Scope::Review.to_string(), "review");
        assert_eq!("review".parse::<Scope>().unwrap(), Scope::Review);
        assert_eq!(serde_json::to_string(&Scope::Review).unwrap(), "\"review\"");
        assert_eq!(serde_json::from_str::<Scope>("\"review\"").unwrap(), Scope::Review);
        assert_eq!(Scope::ALL.iter().filter(|s| **s == Scope::Review).count(), 1);
        assert!(!Scope::Review.is_risky(), "a comment is plain data that a person can delete");
        assert!("Review".parse::<Scope>().is_err());
    }
}
