//! The test-crate rule of `lint-deps` (ADR 0054 §6): each area test crate `omuzaki-test-<area>` depends only on its
//! area. An area whose own crate is an App (the stage) may use that App too. (The harness `omuzaki-test` still holds
//! the tests of the areas not split out yet, so it is not held to "no implementation" until they move.)
//!
//! Why: a test crate is in scope whenever anything it depends on changes. One test crate that reached every
//! implementation put every parity test in scope for any change; per-area crates keep a face change from running the
//! character's tests, and the other way round.

use crate::workspace::{DepKind, Dependency, Package};

use super::{CrateKind, Violation};

/// The harness: golden compare, bless, evidence. Every area test crate may use it.
pub const HARNESS: &str = "omuzaki-test";

/// The generator behind every App's `build.rs` (ADR 0054 section 6): it names no area, and an area test crate that
/// links registering crates needs it under `[build-dependencies]` like any other App.
const LINK_BUILD: &str = "omuzaki-link-build";

/// The name prefix of an area test crate: `omuzaki-test-char` tests the `char` area.
pub const AREA_PREFIX: &str = "omuzaki-test-";

/// Crates whose names don't say their area, with the area they belong to. Every other implementation or lib crate
/// belongs to the area its name starts with (`omuzaki-io-ops` is in `io`).
const MEMBERS: &[(&str, &str)] = &[
    // The rig and the animation runtime drive the character's body; their tests meet the character crate.
    ("omuzaki-rig", "char"),
    ("omuzaki-anim", "char"),
    // The glTF format library sits under the io formats.
    ("omuzaki-gltf", "io"),
    // The stage links the face and the renderer and draws the one into the other (#151, #2074): its tests meet both.
    ("omuzaki-face", "stage"),
    ("omuzaki-render", "stage"),
    // Voice and speech are the track area's (CLAUDE.md, area 6): the speech host's real models meet the tracker's.
    ("omuzaki-speech", "track"),
];

/// Crates of another area that an area test crate loads at run time, with the area whose test crate may use them. The
/// track area's real-model tests run the graphs through the runtimes (`omuzaki-ai-ort`, `omuzaki-ai-litert`), which no
/// implementation crate may depend on, so its test crate is the one place the tracker meets them; the speech models
/// are installed (smoke test included) and opened through the registry (`omuzaki-ai`) the way the app does. Only that
/// crate is allowed: every other area's test crate still fails on them. The mic's voice effects (`omuzaki-audio-fx`)
/// sit in front of the microphone mouth, so the track test crate measures that they never cost lip sync (#2587).
const GUESTS: &[(&str, &str)] = &[
    ("omuzaki-ai-ort", "track"),
    ("omuzaki-ai-litert", "track"),
    ("omuzaki-ai", "track"),
    ("omuzaki-audio-fx", "track"),
];

/// The area an area test crate covers, from its name; `None` for every other crate.
pub fn area_of(name: &str) -> Option<&str> {
    name.strip_prefix(AREA_PREFIX).filter(|area| !area.is_empty())
}

/// Whether the implementation or lib crate `name` belongs to `area`.
fn in_area(name: &str, area: &str) -> bool {
    let named = name.strip_prefix("omuzaki-").and_then(|rest| rest.strip_prefix(area));
    matches!(named, Some(rest) if rest.is_empty() || rest.starts_with('-'))
        || MEMBERS.iter().chain(GUESTS).any(|&(member, owner)| member == name && owner == area)
}

/// What the test-crate rule finds wrong with `package`'s dependency on `target`, a crate of kind `to`; `None` when it
/// is fine or `package` is not a test crate.
pub fn violation(package: &Package, dependency: &Dependency, target: &Package, to: CrateKind) -> Option<Violation> {
    let area = area_of(&package.name)?;
    let fine = match to {
        CrateKind::Api | CrateKind::Mock => true,
        CrateKind::Lib if target.name == LINK_BUILD => dependency.kind == DepKind::Build,
        CrateKind::Implementation | CrateKind::Lib => in_area(&target.name, area),
        // The harness, and the area's own App (`omuzaki-stage` for the stage area).
        CrateKind::App => target.name == HARNESS || in_area(&target.name, area),
    };
    if fine {
        return None;
    }
    let mut crates = vec![format!("omuzaki-{area}"), format!("omuzaki-{area}-*")];
    crates.extend(
        MEMBERS.iter().chain(GUESTS).filter(|&&(_, owner)| owner == area).map(|(member, _)| (*member).to_owned()),
    );
    Some(Violation {
        from: package.name.clone(),
        to: dependency.name.clone(),
        rule: format!(
            "the {area} area test crate may not depend on {to} under {section}; allowed: API crates, mocks, the \
             harness {HARNESS} and the {area} area's crates ({crates})",
            section = dependency.kind.section(),
            crates = crates.join(", "),
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::super::violations;
    use super::*;
    use crate::workspace::DepKind::{self, Build, Dev, Normal};
    use crate::workspace::testing::{with_layer, workspace};

    /// The violations in `crates`, with `omuzaki-gltf` a lib crate as in the real workspace.
    fn found(crates: &[(&str, &[(&str, DepKind)])]) -> Vec<String> {
        violations(&with_layer(workspace(crates), "omuzaki-gltf", "lib")).iter().map(ToString::to_string).collect()
    }

    /// Every crate the tests below name, with no dependencies of its own.
    const LEAVES: &[(&str, &[(&str, DepKind)])] = &[
        ("omuzaki-core", &[]),
        ("omuzaki-char", &[]),
        ("omuzaki-char-api", &[]),
        ("omuzaki-char-mock", &[]),
        ("omuzaki-rig", &[]),
        ("omuzaki-anim", &[]),
        ("omuzaki-face", &[]),
        ("omuzaki-render", &[]),
        ("omuzaki-io-ops", &[]),
        ("omuzaki-io-gltf", &[]),
        ("omuzaki-gltf", &[]),
        ("omuzaki-stage", &[]),
        ("omuzaki-track", &[]),
        ("omuzaki-ai-ort", &[]),
        ("omuzaki-ai", &[]),
    ];

    fn with_leaves<'a>(crates: &[(&'a str, &'a [(&'a str, DepKind)])]) -> Vec<(&'a str, &'a [(&'a str, DepKind)])> {
        crates.iter().chain(LEAVES).copied().collect()
    }

    #[test]
    fn an_area_test_crate_may_build_depend_on_the_link_generator_only() {
        let run = |kind| {
            let crates = [("omuzaki-test-face", &[("omuzaki-link-build", kind)][..]), ("omuzaki-link-build", &[])];
            violations(&with_layer(workspace(&crates), "omuzaki-link-build", "lib")).len()
        };
        assert_eq!(run(Build), 0);
        assert_eq!(run(Normal), 1, "the generator belongs under [build-dependencies] only");
    }

    #[test]
    fn area_test_crates_are_apps_named_for_their_area() {
        assert_eq!(CrateKind::of("omuzaki-test-char"), CrateKind::App);
        assert_eq!(CrateKind::of("omuzaki-test-render"), CrateKind::App);
        assert_eq!(area_of("omuzaki-test-io"), Some("io"));
        assert_eq!(area_of("omuzaki-test"), None, "the harness is no area");
        assert_eq!(area_of("omuzaki-test-"), None);
        assert_eq!(CrateKind::of("omuzaki-test-x-api"), CrateKind::Api, "a name can't make a contract an app");
    }

    #[test]
    fn an_area_owns_the_crates_its_name_starts_and_its_listed_members() {
        assert!(in_area("omuzaki-char", "char"));
        assert!(in_area("omuzaki-char-extra", "char"));
        assert!(in_area("omuzaki-rig", "char"));
        assert!(in_area("omuzaki-io-ops", "io"));
        assert!(in_area("omuzaki-gltf", "io"));
        assert!(!in_area("omuzaki-characters", "char"), "a prefix of the name is not the area");
        assert!(!in_area("omuzaki-face", "char"));
        assert!(!in_area("omuzaki-rig", "face"));
    }

    #[test]
    fn the_intended_split_passes() {
        let ws = with_leaves(&[
            ("omuzaki-test", &[("omuzaki-core", Normal), ("omuzaki-face", Normal), ("omuzaki-char-mock", Dev)]),
            (
                "omuzaki-test-char",
                &[
                    ("omuzaki-test", Dev),
                    ("omuzaki-char", Dev),
                    ("omuzaki-rig", Dev),
                    ("omuzaki-anim", Dev),
                    ("omuzaki-char-api", Dev),
                    ("omuzaki-char-mock", Dev),
                ],
            ),
            ("omuzaki-test-face", &[("omuzaki-test", Normal), ("omuzaki-face", Normal), ("omuzaki-core", Normal)]),
            ("omuzaki-test-track", &[("omuzaki-track", Dev), ("omuzaki-ai-ort", Dev), ("omuzaki-core", Dev)]),
            (
                "omuzaki-test-io",
                &[("omuzaki-io-ops", Dev), ("omuzaki-io-gltf", Dev), ("omuzaki-gltf", Dev), ("omuzaki-char-mock", Dev)],
            ),
        ]);
        assert_eq!(found(&ws), Vec::<String>::new());
    }

    #[test]
    fn an_area_test_crate_may_not_reach_another_area() {
        let got = found(&with_leaves(&[(
            "omuzaki-test-char",
            &[("omuzaki-face", Dev), ("omuzaki-gltf", Normal), ("omuzaki-render", Dev)],
        )]));
        assert_eq!(got.len(), 3, "{got:?}");
        assert!(
            got[0].starts_with(
                "omuzaki-test-char → omuzaki-face: the char area test crate may not depend on implementation crate \
                 under [dev-dependencies]"
            ),
            "{}",
            got[0]
        );
        assert!(got[0].contains("(omuzaki-char, omuzaki-char-*, omuzaki-rig, omuzaki-anim)"), "{}", got[0]);
        assert!(got[1].contains("may not depend on lib crate under [dependencies]"), "{}", got[1]);
        assert!(got[2].starts_with("omuzaki-test-char → omuzaki-render"), "{}", got[2]);
    }

    #[test]
    fn only_the_track_test_crate_may_load_the_runtime_wrappers_and_the_registry() {
        let got = found(&with_leaves(&[
            ("omuzaki-test-face", &[("omuzaki-face", Dev), ("omuzaki-ai-ort", Dev), ("omuzaki-ai", Dev)]),
            (
                "omuzaki-test-track",
                &[
                    ("omuzaki-track", Dev),
                    ("omuzaki-speech", Dev),
                    ("omuzaki-ai-ort", Dev),
                    ("omuzaki-ai", Dev),
                    ("omuzaki-render", Dev),
                ],
            ),
        ]));
        assert_eq!(got.len(), 3, "{got:?}");
        assert!(got[0].starts_with("omuzaki-test-face → omuzaki-ai-ort: the face area test crate"), "{}", got[0]);
        assert!(got[1].starts_with("omuzaki-test-face → omuzaki-ai: the face area test crate"), "{}", got[1]);
        assert!(got[2].starts_with("omuzaki-test-track → omuzaki-render:"), "{}", got[2]);
        let allowed = "(omuzaki-track, omuzaki-track-*, omuzaki-speech, omuzaki-ai-ort, omuzaki-ai-litert, omuzaki-ai, \
            omuzaki-audio-fx)";
        assert!(got[2].contains(allowed), "{}", got[2]);
    }

    #[test]
    fn an_area_test_crate_uses_the_harness_but_no_other_app() {
        let got = found(&with_leaves(&[
            ("omuzaki-test", &[]),
            ("omuzaki-test-face", &[("omuzaki-test", Normal), ("omuzaki-stage", Dev), ("omuzaki-test-char", Dev)]),
            ("omuzaki-test-char", &[]),
        ]));
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got[0].starts_with("omuzaki-test-face → omuzaki-stage: the face area test crate"), "{}", got[0]);
        assert!(got[1].starts_with("omuzaki-test-face → omuzaki-test-char"), "{}", got[1]);
    }

    #[test]
    fn the_stage_area_test_crate_uses_the_stage_and_the_face_and_renderer_it_links_and_nothing_else() {
        let fine = found(&with_leaves(&[(
            "omuzaki-test-stage",
            &[("omuzaki-stage", Dev), ("omuzaki-face", Dev), ("omuzaki-render", Dev), ("omuzaki-char-mock", Dev)],
        )]));
        assert_eq!(fine, Vec::<String>::new());
        let got = found(&with_leaves(&[("omuzaki-test-stage", &[("omuzaki-char", Dev), ("omuzaki-io-ops", Dev)])]));
        assert_eq!(got.len(), 2, "the character and io crates are other areas: {got:?}");
        assert!(got[0].contains("(omuzaki-stage, omuzaki-stage-*, omuzaki-face, omuzaki-render)"), "{}", got[0]);
        let other = found(&with_leaves(&[("omuzaki-test-face", &[("omuzaki-stage", Dev)])]));
        assert_eq!(other.len(), 1, "another area's test crate still may not use the stage: {other:?}");
    }

    #[test]
    fn other_apps_still_depend_on_anything() {
        assert_eq!(
            found(&with_leaves(&[("omuzaki-cli", &[("omuzaki-face", Normal), ("omuzaki-char", Normal)])])),
            Vec::<String>::new()
        );
    }
}
