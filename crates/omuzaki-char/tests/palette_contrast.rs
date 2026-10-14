//! The contrast checker (`palette::contrast`): the colour maths against published reference values, the Machado et
//! al. (2009) simulation, the fix search against 500 seeded random pairs, and the `palette.check_contrast` query op
//! against the in-memory document.
//!
//! The op is planned through the shared harness in `common`, so the editing contract's helpers apply here too; the
//! query changes nothing, which the tests check by comparing the document before and after.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use common::{apply, assert_error, mock_node, plan, refused};
use omuzaki_char::palette::contrast::{
    Deficiency, PairKind, PaletteCheckContrast, Thresholds, delta_e, delta_l, lift_to_pass, lift_to_pass_all_vision,
    measure, oklab, passes_all_vision, relative_luminance, simulate, wcag_ratio,
};
use omuzaki_char::palette::ops::PaletteSet;
use omuzaki_char::palette::skin_tone::Oklch;
use omuzaki_core::colour::Colour;
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{ErrorCode, Op, OpKind, OpMeta, Scope, Undo, find};
use serde_json::{Value, json};

/// Takes the address of each op's `meta`, so this test binary links the modules that register them.
///
/// `inventory` registers an op through a submission in its module's object file, and the linker drops that file when
/// nothing in it is referenced: the ops would then be missing from `find`. Nothing else here calls into them.
// workaround for the link-time drop. Remove this anchor when the link gate lands.
#[used]
static LINKED_OPS: [fn() -> OpMeta; 2] = [<PaletteCheckContrast as Op>::meta, <PaletteSet as Op>::meta];

const CHECK: &str = "palette.check_contrast";

// ------------------------------------------------------------------------------------------------ colour maths

/// The WCAG 2.x relative luminance from the published formula, in `f64`, for checking the implementation against.
fn luminance_reference(rgb: [u8; 3]) -> f64 {
    let lin = |v: u8| {
        let c = f64::from(v) / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * lin(rgb[0]) + 0.7152 * lin(rgb[1]) + 0.0722 * lin(rgb[2])
}

#[test]
fn wcag_luminance_follows_the_published_formula_across_the_threshold() {
    // 10 and 11 straddle the 0.04045 linearisation threshold (10/255 is linear, 11/255 is the power curve).
    for v in [0_u8, 10, 11, 0x77, 128, 255] {
        let grey = Colour::rgb(v, v, v);
        assert!((relative_luminance(grey) - luminance_reference([v, v, v])).abs() < 1e-6, "grey {v}");
    }
    let mixed = Colour::rgb(0x12, 0xab, 0x40);
    assert!((relative_luminance(mixed) - luminance_reference([0x12, 0xab, 0x40])).abs() < 1e-6);
}

#[test]
fn wcag_ratio_matches_the_published_reference_values() {
    let white = Colour::WHITE;
    assert!((wcag_ratio(Colour::BLACK, white) - 21.0).abs() < 1e-9, "black on white is 21:1");
    assert!((wcag_ratio(Colour::rgb(0x77, 0x77, 0x77), white) - 4.48).abs() < 0.01, "#777 on white is 4.48:1");
    assert!((wcag_ratio(Colour::rgb(0x76, 0x76, 0x76), white) - 4.54).abs() < 0.01, "#767676 on white is 4.54:1");
    assert!((wcag_ratio(white, Colour::rgb(0x77, 0x77, 0x77)) - 4.48).abs() < 0.01, "the ratio is symmetric");
    assert!((wcag_ratio(white, white) - 1.0).abs() < 1e-9, "a colour against itself is 1:1");
}

#[test]
fn oklab_matches_ottosson_reference_values() {
    // Björn Ottosson's published OKLab of pure red (sRGB #ff0000) is (0.627955, 0.224863, 0.125846).
    let red = oklab(Colour::rgb(255, 0, 0));
    for (got, want) in red.iter().zip([0.627_955, 0.224_863, 0.125_846]) {
        assert!((got - want).abs() < 1e-4, "red: {red:?}");
    }
    let white = oklab(Colour::WHITE);
    assert!((white[0] - 1.0).abs() < 1e-4 && white[1].abs() < 1e-4 && white[2].abs() < 1e-4, "white: {white:?}");
    let black = oklab(Colour::BLACK);
    assert!(black.iter().all(|v| v.abs() < 1e-4), "black: {black:?}");
}

#[test]
fn value_separation_measures_are_in_oklab_units() {
    let (black, white) = (Colour::BLACK, Colour::WHITE);
    assert!((delta_l(black, white) - 1.0).abs() < 1e-6);
    assert!((delta_e(black, white) - 1.0).abs() < 1e-6, "delta-E is plain OKLab units, black to white being 1");
    assert_eq!(delta_e(white, white), 0.0);
    let m = measure(black, white);
    assert!((m.wcag - 21.0).abs() < 1e-9 && (m.delta_l - 1.0).abs() < 1e-6);
}

// ------------------------------------------------------------------------------------------------ simulation

#[test]
fn machado_simulation_matches_the_matrix_result() {
    // Pure red through the severity-1.0 matrices on linear RGB (Machado et al. 2009), encoded to sRGB and rounded.
    let red = Colour::rgb(255, 0, 0);
    assert_eq!(simulate(red, Deficiency::Deuteranopia), Colour::rgb(163, 144, 0));
    assert_eq!(simulate(red, Deficiency::Protanopia), Colour::rgb(109, 95, 0));
    assert_eq!(simulate(red, Deficiency::Tritanopia), Colour::rgb(255, 0, 15));
}

#[test]
fn simulation_keeps_white_and_grey_fixed() {
    // Each matrix row sums to one, so a neutral (equal linear channels) is a fixed point of every dichromacy.
    for d in Deficiency::ALL {
        assert_eq!(simulate(Colour::WHITE, d), Colour::WHITE, "{}", d.as_str());
        let grey = Colour::rgb(0x80, 0x80, 0x80);
        assert_eq!(simulate(grey, d), grey, "{}", d.as_str());
    }
}

// ------------------------------------------------------------------------------------------------ fix search

/// A deterministic 64-bit xorshift: the same 500 pairs on every run.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn colour(&mut self) -> Colour {
        let v = self.next_u64().to_le_bytes();
        Colour::rgb(v[0], v[1], v[2])
    }
}

#[test]
fn a_suggestion_once_applied_passes_its_threshold_over_500_random_pairs() {
    let thresholds = Thresholds::default();
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    let (mut failing, mut moved_lightness) = (0, 0);
    for i in 0..500 {
        let (movable, fixed) = (rng.colour(), rng.colour());
        let kind = if i % 2 == 0 { PairKind::Mass } else { PairKind::Line };
        if kind.passes(&measure(movable, fixed), &thresholds) {
            continue;
        }
        failing += 1;
        let lift = lift_to_pass(movable, fixed, kind, &thresholds)
            .unwrap_or_else(|| panic!("pair {i} can't be fixed: {movable} against {fixed}"));
        assert!(
            kind.passes(&measure(lift.colour, fixed), &thresholds),
            "pair {i}: {movable} -> {} still fails {kind:?} against {fixed}",
            lift.colour
        );
        assert!(lift.shift.abs() <= 1.0, "pair {i}: a lightness move is at most 1");
        moved_lightness += usize::from(lift.shift != 0.0);
    }
    assert!(failing > 100, "the seeded pairs should include many failures, got {failing}");
    assert_eq!(moved_lightness, failing, "every fix moves the lightness");
}

#[test]
fn a_passing_pair_needs_no_move() {
    let t = Thresholds::default();
    let lift = lift_to_pass(Colour::BLACK, Colour::WHITE, PairKind::Line, &t).unwrap();
    assert_eq!((lift.colour, lift.shift), (Colour::BLACK, 0.0));
}

#[test]
fn a_mass_fix_moves_lightness_by_the_gap_and_keeps_the_hue() {
    // Mid grey against a colour 0.03 lighter: the nearest passing move is about 0.07 further on the far side.
    let t = Thresholds::default();
    let fixed = Colour::rgb(0x80, 0x80, 0x80);
    let movable = Colour::rgb(0x7a, 0x60, 0x70);
    let before = oklab(movable)[0];
    let lift = lift_to_pass(movable, fixed, PairKind::Mass, &t).unwrap();
    let gap_now = (oklab(fixed)[0] - before).abs();
    let want = t.delta_l - gap_now;
    assert!(lift.shift.abs() >= want - 1e-3, "shift {} vs needed {want}", lift.shift);
    assert!(lift.shift.abs() <= want + 0.02, "the move is the smallest that passes: {}", lift.shift);
    assert!(PairKind::Mass.passes(&measure(lift.colour, fixed), &t));
    // The hue is kept to within a degree (OKLCH h; a chromatic colour, so 8-bit rounding can't swing it).
    let (before, after) = (Oklch::from_colour(movable), Oklch::from_colour(lift.colour));
    let dh = (after.h - before.h + 540.0).rem_euclid(360.0) - 180.0;
    assert!(dh.abs() <= 1.0, "hue {} -> {} ({dh} degrees)", before.h, after.h);
}

#[test]
fn a_chromatic_fix_keeps_the_hue_within_a_degree() {
    let t = Thresholds::default();
    let fixed = Colour::rgb(0xc8, 0x5a, 0x6e);
    for movable in [Colour::rgb(0xb4, 0x50, 0x64), Colour::rgb(0x3a, 0x7a, 0x55), Colour::rgb(0x4a, 0x5a, 0xb0)] {
        let lift = lift_to_pass(movable, fixed, PairKind::Mass, &t).unwrap();
        let (before, after) = (Oklch::from_colour(movable), Oklch::from_colour(lift.colour));
        let dh = (after.h - before.h + 540.0).rem_euclid(360.0) - 180.0;
        assert!(dh.abs() <= 1.0, "{movable} -> {}: hue {} -> {}", lift.colour, before.h, after.h);
    }
}

#[test]
fn an_all_vision_fix_passes_in_normal_vision_and_every_simulation() {
    let t = Thresholds::default();
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let (mut failing, mut fixed_all, mut stricter) = (0, 0, 0);
    for i in 0..500 {
        let (movable, fixed) = (rng.colour(), rng.colour());
        let kind = if i % 2 == 0 { PairKind::Mass } else { PairKind::Line };
        if passes_all_vision(movable, fixed, kind, &t) {
            continue;
        }
        failing += 1;
        let Some(lift) = lift_to_pass_all_vision(movable, fixed, kind, &t) else { continue };
        fixed_all += 1;
        assert!(passes_all_vision(lift.colour, fixed, kind, &t), "pair {i}: {movable} -> {} vs {fixed}", lift.colour);
        for d in Deficiency::ALL {
            let m = measure(simulate(lift.colour, d), simulate(fixed, d));
            assert!(kind.passes(&m, &t), "pair {i} under {}", d.as_str());
        }
        // The normal-vision fix is never larger, and sometimes fails a simulation.
        let plain = lift_to_pass(movable, fixed, kind, &t).unwrap();
        assert!(plain.shift.abs() <= lift.shift.abs() + 1e-9, "pair {i}");
        stricter += usize::from(!passes_all_vision(plain.colour, fixed, kind, &t));
    }
    assert!(failing > 100, "got {failing} failing pairs");
    assert!(fixed_all * 10 >= failing * 9, "nearly every pair has an all-vision fix: {fixed_all}/{failing}");
    assert!(stricter > 0, "some normal-vision fixes break under a simulation, which is why this exists");
}

#[test]
fn a_known_pair_needs_a_larger_move_to_be_colour_blind_safe() {
    // A red line on a green mass: close in luminance under deuteranopia. Normal vision passes on a smaller move.
    let t = Thresholds::default();
    let (movable, fixed) = (Colour::rgb(0xb0, 0x60, 0x30), Colour::rgb(0x40, 0x90, 0x40));
    let plain = lift_to_pass(movable, fixed, PairKind::Line, &t).unwrap();
    let safe = lift_to_pass_all_vision(movable, fixed, PairKind::Line, &t).unwrap();
    assert!(passes_all_vision(safe.colour, fixed, PairKind::Line, &t));
    assert!(safe.shift.abs() >= plain.shift.abs());
}

// ------------------------------------------------------------------------------------------------ the op

/// The palette the op tests use: the standard slots except `sclera`, which the mock character does not have.
fn palette() -> Value {
    json!({
        "skin": "#f9d3c5",
        "hair_root": "#5a3b2e",
        "outfit_a": "#24392f",
        "outfit_b": "#7a9c8c",
        "eyes": "#18342a",
        "lip": "#b5585a",
        "lash": "#222222",
    })
}

/// A palette whose adjacent masses and lip sit close together, so several default pairs fail.
fn failing_palette() -> Value {
    json!({
        "skin": "#f9d3c5",
        "hair_root": "#f0c8b4",
        "outfit_a": "#f5f5f5",
        "outfit_b": "#f2f0ee",
        "eyes": "#18342a",
        "lip": "#f0bfb5",
        "lash": "#222222",
    })
}

/// The mock character with the given palette, as a project holding it as `mock`.
fn doc_with(palette: Value) -> MemDoc {
    let mut node = mock_node();
    node["palette"] = palette;
    MemDoc::from_value(json!({ "characters": { "mock": node } }))
}

/// Plans the check on `doc` and returns its output.
fn check(doc: &MemDoc, args: Value) -> Value {
    plan(doc, CHECK, args).unwrap().output.expect("a query answers")
}

/// The pairs of a check output, as a slice.
fn pairs(out: &Value) -> &[Value] {
    out["pairs"].as_array().expect("pairs is a list")
}

#[test]
fn the_op_is_a_read_only_query() {
    let meta = (find(CHECK).expect("registered").meta)();
    assert_eq!((meta.kind, meta.scope, meta.undo), (OpKind::Query, Scope::Read, Undo::None));
}

#[test]
fn the_default_pairs_are_checked_or_skipped_with_a_note() {
    let doc = doc_with(palette());
    let before = doc.root().clone();
    let out = check(&doc, json!({ "character": "mock" }));
    assert_eq!(doc.root(), &before, "a query changes nothing");
    assert_eq!(out["background"], "#ffffff");
    assert_eq!(pairs(&out).len(), 6, "seven defaults, the sclera pair skipped");
    let skipped = out["skipped"].as_array().unwrap();
    assert_eq!(skipped.len(), 1);
    assert!(skipped[0]["note"].as_str().unwrap().contains("`sclera`"));
    // Hair root against skin is a mass pair with a value gap of its own; its fields are all there.
    let hair = &pairs(&out)[0];
    assert_eq!(
        (hair["a"].as_str(), hair["b"].as_str(), hair["kind"].as_str()),
        (Some("hair_root"), Some("skin"), Some("mass"))
    );
    for d in ["protanopia", "deuteranopia", "tritanopia"] {
        assert!(hair["simulated"][d]["delta_l"].is_number(), "{d}");
    }
}

#[test]
fn a_failing_pair_gets_a_suggestion_that_passes_once_applied() {
    let doc = doc_with(failing_palette());
    let out = check(&doc, json!({ "character": "mock" }));
    let mut checked = 0;
    for pair in pairs(&out).iter().filter(|p| p["pass"] == json!(false)) {
        let fix = &pair["suggestion"];
        assert!(fix.is_object(), "a failing pair with movable slots gets a fix: {pair}");
        let slot = fix["slot"].as_str().unwrap();
        let mut edited = doc.clone();
        apply(&mut edited, "palette.set", json!({ "character": "mock", "slot": slot, "colour": fix["new_colour"] }));
        let recheck = check(
            &edited,
            json!({ "character": "mock", "pairs": [{ "a": pair["a"], "b": pair["b"], "kind": pair["kind"] }] }),
        );
        assert_eq!(recheck["pass"], json!(true), "after {slot} -> {}: {recheck}", fix["new_colour"]);
        if fix["colour_blind_safe"] == json!(true) {
            for d in ["protanopia", "deuteranopia", "tritanopia"] {
                assert_eq!(recheck["pairs"][0]["simulated"][d]["pass"], json!(true), "{d} after {slot}: {recheck}");
            }
        }
        checked += 1;
    }
    assert!(checked >= 3, "the failing palette has several failing default pairs, got {checked}");
}

#[test]
fn a_pair_that_passes_in_normal_vision_but_fails_a_dichromacy_gets_a_fix() {
    // #cc0000 on black is 3.57:1 in normal vision (passes WCAG 3.0) but 2.38:1 under protanopia, so it still needs a fix.
    let mut palette = palette();
    palette["lip"] = json!("#cc0000");
    let doc = doc_with(palette);
    let pair = json!({ "a": "lip", "b": "background", "kind": "line" });
    let out = check(&doc, json!({ "character": "mock", "background": "#000000", "pairs": [pair] }));
    let entry = &pairs(&out)[0];
    assert_eq!(entry["pass"], json!(true), "normal vision passes: {entry}");
    assert_eq!(entry["simulated"]["protanopia"]["pass"], json!(false), "protanopia fails: {entry}");
    let fix = &entry["suggestion"];
    assert_eq!(fix["slot"], json!("lip"), "the only movable side is lip: {fix}");
    assert_eq!(fix["colour_blind_safe"], json!(true), "a lightness passes every view: {fix}");

    // Applying the suggestion passes all four views.
    let mut edited = doc.clone();
    apply(&mut edited, "palette.set", json!({ "character": "mock", "slot": "lip", "colour": fix["new_colour"] }));
    let recheck = check(&edited, json!({ "character": "mock", "background": "#000000", "pairs": [pair] }));
    let after = &pairs(&recheck)[0];
    assert_eq!(after["pass"], json!(true), "normal vision after the fix: {after}");
    for d in ["protanopia", "deuteranopia", "tritanopia"] {
        assert_eq!(after["simulated"][d]["pass"], json!(true), "{d} after the fix: {after}");
    }
}

#[test]
fn a_locked_slot_never_gets_a_suggestion() {
    let doc = doc_with(failing_palette());
    // outfit_a is near white, so it fails against the background; the background cannot move and outfit_a is locked.
    let out = check(
        &doc,
        json!({ "character": "mock", "pairs": [{ "a": "outfit_a", "b": "background", "kind": "mass" }], "locked": ["outfit_a"] }),
    );
    let pair = &pairs(&out)[0];
    assert_eq!(pair["pass"], json!(false), "a near-white outfit fails against white");
    assert!(pair["suggestion"].is_null(), "outfit_a is locked and the background is fixed: no fix");
    assert!(pair["note"].as_str().unwrap().contains("neither side can move"));

    // With skin movable, the fix moves skin and never the locked outfit.
    let out = check(
        &doc,
        json!({ "character": "mock", "pairs": [{ "a": "skin", "b": "outfit_a", "kind": "mass" }], "locked": ["outfit_a"] }),
    );
    let fix = &pairs(&out)[0]["suggestion"];
    assert_eq!(fix["slot"], json!("skin"), "only the unlocked side moves: {fix}");
}

#[test]
fn thresholds_are_configurable() {
    let doc = doc_with(palette());
    let out = check(&doc, json!({ "character": "mock", "thresholds": { "delta_l": 0.0, "wcag": 1.0 } }));
    assert_eq!(out["pass"], json!(true), "every pair passes at zero and one");
    assert!(pairs(&out).iter().all(|p| p["suggestion"].is_null()));
    assert_eq!(out["thresholds"]["wcag"], json!(1.0));
}

#[test]
fn the_output_is_deterministic() {
    let doc = doc_with(palette());
    let args = json!({ "character": "mock" });
    assert_eq!(check(&doc, args.clone()), check(&doc, args));
}

#[test]
fn refusals_name_the_exact_field() {
    let doc = doc_with(palette());
    let e = refused(&doc, CHECK, json!({ "character": "ghost" }));
    assert_error(&e, ErrorCode::NotFound, "character", &["ghost"], &["mock"]);

    let e = refused(&doc, CHECK, json!({ "character": "mock", "background": "white" }));
    assert_error(&e, ErrorCode::Invalid, "background", &["white"], &["#ffffff"]);

    // A translucent background has nothing to composite over: refused, naming the field, not measured without alpha.
    let e = refused(&doc, CHECK, json!({ "character": "mock", "background": "#ffffff80" }));
    assert_error(&e, ErrorCode::Invalid, "background", &["translucent", "128"], &["opaque"]);

    let e = refused(&doc, CHECK, json!({ "character": "mock", "pairs": [] }));
    assert_error(&e, ErrorCode::Invalid, "pairs", &["at least one pair"], &["default"]);

    let e =
        refused(&doc, CHECK, json!({ "character": "mock", "pairs": [{ "a": "Hair!", "b": "skin", "kind": "mass" }] }));
    assert_error(&e, ErrorCode::Invalid, "pairs/0/a", &["Hair!"], &["hair_root"]);

    let e =
        refused(&doc, CHECK, json!({ "character": "mock", "pairs": [{ "a": "skin", "b": "skin", "kind": "line" }] }));
    assert_error(&e, ErrorCode::Invalid, "pairs/0/b", &["skin"], &["different"]);

    let e = refused(&doc, CHECK, json!({ "character": "mock", "thresholds": { "delta_l": -0.1 } }));
    assert_error(&e, ErrorCode::Invalid, "thresholds/delta_l", &["-0.1"], &["0.10"]);

    let e = refused(&doc, CHECK, json!({ "character": "mock", "thresholds": { "wcag": 30.0 } }));
    assert_error(&e, ErrorCode::Invalid, "thresholds/wcag", &["30"], &["21"]);

    let e = refused(&doc, CHECK, json!({ "character": "mock", "locked": ["Bad Name"] }));
    assert_error(&e, ErrorCode::Invalid, "locked/0", &["Bad Name"], &["hair_root"]);
}

#[test]
fn an_opaque_background_with_explicit_alpha_is_accepted() {
    let doc = doc_with(palette());
    let out = check(&doc, json!({ "character": "mock", "background": "#000000ff" }));
    assert_eq!(out["background"], "#000000");
}
