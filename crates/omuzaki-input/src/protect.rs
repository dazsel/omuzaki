//! The person's protected-program list (design §4, the kill switch): while one of these programs runs, capture pauses.
//! The list is kept in `input-protected.toml` in the data folder, edited through the [`ProtectList`] handle the shell
//! gets (never `Services`) or the `input.capture.protect` op (scope `process`).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use omuzaki_input_api::feed::ProtectList;
use serde::{Deserialize, Serialize};

use crate::shared::lock;

/// The list's file in the data folder.
pub(crate) const FILE: &str = "input-protected.toml";

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    programs: Vec<String>,
}

/// The list and the protected program the kill switch last found running.
#[derive(Debug, Default)]
pub(crate) struct Protected {
    path: Option<PathBuf>,
    programs: Mutex<Vec<String>>,
    running: Mutex<Option<String>>,
}

impl Protected {
    /// The list in `dir`, or in memory only. An unreadable file is an empty list.
    #[allow(clippy::disallowed_methods, reason = "the hub's own file in its data folder, never a path an op names; the fence is for ops")]
    pub(crate) fn open(dir: Option<&Path>) -> Self {
        let path = dir.map(|d| d.join(FILE));
        let programs = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|text| toml::from_str::<File>(&text).ok())
            .map(|f| f.programs)
            .unwrap_or_default();
        Self { path, programs: Mutex::new(programs), running: Mutex::new(None) }
    }

    /// The list.
    pub(crate) fn programs(&self) -> Vec<String> {
        lock(&self.programs).clone()
    }

    /// The kill switch found `program` running (or none).
    pub(crate) fn set_running(&self, program: Option<String>) {
        *lock(&self.running) = program;
    }
}

impl ProtectList for Protected {
    #[allow(clippy::disallowed_methods, reason = "the hub's own file in its data folder, never a path an op names; the fence is for ops")]
    fn set(&self, programs: &[String]) {
        let cleaned: Vec<String> =
            programs.iter().map(|p| p.trim().to_owned()).filter(|p| !p.is_empty()).collect();
        if let Some(path) = &self.path {
            let file = File { programs: cleaned.clone() };
            if let Ok(text) = toml::to_string(&file) {
                if let Some(dir) = path.parent() {
                    let _made = std::fs::create_dir_all(dir);
                }
                let _written = std::fs::write(path, text);
            }
        }
        *lock(&self.programs) = cleaned;
    }

    fn get(&self) -> Vec<String> {
        self.programs()
    }

    fn matching(&self) -> Option<String> {
        lock(&self.running).clone()
    }
}
