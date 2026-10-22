//! Pixel snapshots of the gallery (#202, `design-system.md` section 10): every story group in the dark, light and high
//! contrast themes at 100% and 200%, each held to its golden through the project's golden harness (`omuzaki-test`:
//! manifests in `tests/goldens/ui-gallery.toml`, images in the content-addressed store outside git).
//!
//! The full suite runs where the golden store is (`OMUZAKI_GOLDENS`) and says why it skipped elsewhere; a changed
//! image fails with its expected, actual and diff images in the evidence folder, and `OMUZAKI_BLESS=1` accepts it.
//! The harness itself is proved on every run over a temporary store: a story blessed and rendered again matches
//! pixel for pixel (the software rasteriser is deterministic), and an intentional colour change fails with a diff.

use egui::vec2;
use omuzaki_test::compare::Tolerance;
use omuzaki_test::goldens::{GoldenError, GoldenId, Goldens, Mode, Outcome};
use omuzaki_ui::ds::gallery::Group;
use omuzaki_ui::ds::gallery::capture::{Shot, render};
use omuzaki_ui::ds::{Settings, ThemeKind};

/// The suite's name in the store and its manifest's file name.
const SUITE: &str = "ui-gallery";
/// What a re-render may drift: text antialiasing on another rasteriser build, never a changed colour or shape.
const TOLERANCE: Tolerance = Tolerance { per_channel: 2, max_bad_fraction: 0.0005 };
/// The gallery's width in points; the height fits every card.
const WIDTH: f32 = 1680.0;

const GROUPS: [(Group, &str); 5] = [
    (Group::Foundations, "foundations"),
    (Group::Controls, "controls"),
    (Group::Display, "display"),
    (Group::Feedback, "feedback"),
    (Group::Composites, "composites"),
];
const THEMES: [(ThemeKind, &str); 3] =
    [(ThemeKind::Dark, "dark"), (ThemeKind::Light, "light"), (ThemeKind::HighContrast, "high-contrast")];
const SCALES: [(f32, &str); 2] = [(1.0, "100"), (2.0, "200")];

/// `group` in `theme`, as tall as its cards, at `pixels_per_point`.
fn shot(group: Group, theme: ThemeKind, pixels_per_point: f32) -> Shot {
    let mut shot = Shot::new(Settings { theme, ..Settings::default() });
    shot.size = vec2(WIDTH, 0.0);
    shot.pixels_per_point = pixels_per_point;
    shot.group = Some(group);
    shot
}

fn id(name: &str) -> GoldenId {
    GoldenId::new(SUITE, name).unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn every_group_in_three_themes_at_100_and_200_percent_matches_its_golden() {
    let goldens = match Goldens::from_env() {
        Ok(goldens) => goldens,
        Err(GoldenError::NoGoldensDir) => {
            eprintln!("skipped: OMUZAKI_GOLDENS is not set, so the golden store is not reachable here");
            return;
        }
        Err(e) => panic!("{e}"),
    };
    let mode = Mode::from_flag_or_env(false);
    let mut changed = Vec::new();
    for (group, group_name) in GROUPS {
        for (theme, theme_name) in THEMES {
            for (pixels_per_point, scale_name) in SCALES {
                let name = format!("{group_name}-{theme_name}-{scale_name}");
                let image = render(shot(group, theme, pixels_per_point)).unwrap_or_else(|e| panic!("{name}: {e}"));
                match goldens.check(&id(&name), &image, TOLERANCE, mode) {
                    Ok(outcome) if outcome.is_ok() => {}
                    Ok(Outcome::Changed(finding)) => changed.push(format!("{name}: {:?}", finding.status)),
                    Ok(Outcome::Passed(_)) => {}
                    Err(e) => changed.push(format!("{name}: {e}")),
                }
            }
        }
    }
    assert!(
        changed.is_empty(),
        "{} gallery images differ from their goldens; look at the evidence in {} and bless the intended ones with \
         OMUZAKI_BLESS=1:\n{}",
        changed.len(),
        goldens.out().display(),
        changed.join("\n")
    );
}

#[test]
fn a_blessed_story_matches_itself_and_a_colour_change_fails_with_a_diff() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let goldens = Goldens::new(dir.path().join("store"), dir.path().join("manifests"), dir.path().join("out"));
    let golden = id("feedback-dark-100");
    let image = render(shot(Group::Feedback, ThemeKind::Dark, 1.0)).unwrap_or_else(|e| panic!("{e}"));

    let blessed = goldens.check(&golden, &image, TOLERANCE, Mode::Bless).unwrap_or_else(|e| panic!("{e}"));
    assert!(blessed.is_ok(), "blessing records the image: {blessed:?}");
    let manifest = std::fs::read_to_string(goldens.manifest_path(SUITE)).unwrap_or_else(|e| panic!("{e}"));
    assert!(manifest.contains("feedback-dark-100"), "the manifest names the case: {manifest}");

    // The same story rendered again: the software rasteriser gives the same pixels.
    let again = render(shot(Group::Feedback, ThemeKind::Dark, 1.0)).unwrap_or_else(|e| panic!("{e}"));
    match goldens.check(&golden, &again, Tolerance::EXACT, Mode::Check).unwrap_or_else(|e| panic!("{e}")) {
        Outcome::Passed(stats) => assert!(stats.passed, "{stats:?}"),
        Outcome::Changed(finding) => panic!("a re-render differs from itself: {finding:?}"),
    }

    // An intentional colour change: one card's corner in another colour, as a changed token would paint it.
    let mut recoloured = again;
    for y in 40..120 {
        for x in 40..240 {
            recoloured.put_pixel(x, y, image::Rgba([255, 0, 160, 255]));
        }
    }
    match goldens.check(&golden, &recoloured, TOLERANCE, Mode::Check).unwrap_or_else(|e| panic!("{e}")) {
        Outcome::Passed(stats) => panic!("a colour change passed: {stats:?}"),
        Outcome::Changed(finding) => assert!(finding.stats.is_some_and(|s| !s.passed), "{finding:?}"),
    }
    let evidence = goldens.out().join(SUITE);
    for file in ["feedback-dark-100.expected.png", "feedback-dark-100.actual.png", "feedback-dark-100.diff.png"] {
        assert!(evidence.join(file).is_file(), "the evidence has {file}");
    }
}
