//! Checks that run on a scene: the scene's own invariants, the rights rules, perfect-sync completeness and the VRM
//! profile. Exporters call [`vrm_profile`] from their own [`Exporter::check`](crate::Exporter::check); everything
//! else runs for every export through [`preflight`], which [`run_export`](crate::run_export) calls first.

mod graph;
mod motion;
mod rights;
mod structure;
mod vrm;

use crate::{ARKIT_52, ExportOptions, Finding, IoScene, PerfectSync, Report};

pub(crate) use motion::animation_rights;
pub use motion::licence_forbids_derivatives;
pub use vrm::vrm_profile;

impl IoScene {
    /// Checks the scene's invariants: every index in range, the node graph a forest, vertex arrays the same length,
    /// triangle indices valid, morph targets consistent, skins matched to joints, finite numbers and unit
    /// rotations, and the avatar layer's references (humanoid nodes unique, expression binds that hit real morph
    /// targets, spring chains that descend, constraints with sources and no circular dependency between them). Rules are under `scene.*` and `avatar.*`.
    ///
    /// Importers must produce scenes that pass; [`run_import`](crate::run_import) enforces it.
    pub fn check(&self) -> Report {
        structure::check(self)
    }
}

/// Everything that must hold before *any* exporter runs: the scene's invariants, the rights rules (including the
/// model's own terms, so every format gets the sharing gate), and perfect-sync completeness. Exposed so a UI can show what would block an export without choosing a format first.
pub fn preflight(scene: &IoScene, options: &ExportOptions) -> Report {
    let mut report = scene.check();
    report.findings.extend(scene.passthrough_finding());
    report.extend(rights::rights(scene, options));
    report.extend(perfect_sync(scene, options.perfect_sync));
    report
}

/// With [`PerfectSync::Arkit52`] or more, all 52 names must exist as custom expressions. One that exists but has no
/// binds is a warning (it is inert), a missing one is an error.
fn perfect_sync(scene: &IoScene, mode: PerfectSync) -> Report {
    let mut r = Report::new();
    if !mode.wants_arkit() {
        return r;
    }
    // Perfect sync is a property of an avatar's expressions. A scene without one (a plain glTF prop) has nothing to
    // carry, and since the default is on, that must not be an error.
    let Some(avatar) = &scene.avatar else { return r };
    for name in ARKIT_52 {
        let found = avatar.expressions.iter().find(|(n, _)| n.as_str() == name);
        match found {
            None => r.push(
                Finding::error("perfect_sync.missing_channel", format!("expression {name} is missing"))
                    .at(format!("avatar/expressions/{name}"))
                    .hint("run the face baker before exporting"),
            ),
            Some((_, e)) if e.is_inert() => r.push(
                Finding::warning("perfect_sync.inert_channel", format!("expression {name} exists but drives nothing"))
                    .at(format!("avatar/expressions/{name}")),
            ),
            Some(_) => {}
        }
    }
    r
}
