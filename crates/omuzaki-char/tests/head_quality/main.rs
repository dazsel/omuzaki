//! Head quality over the face-shape space: every slider position must give a smooth, plausible head.
//!
//! The four metrics are in [`metrics`]; the shape space and the named profiles in [`space`]. The property test holds
//! every outline the contract accepts, on crania within 10% of the default, to the [`limits`]. Two ignored tests
//! produce the evidence for a change to the lower face:
//!
//! - `report`: the metrics over 600 samples of each population (`HEAD_QUALITY_SAMPLES` for more), as a Markdown
//!   table (and every sample as CSV when `HEAD_QUALITY_OUT` names a directory);
//! - `dump`: the meshes of the named profiles and 20 seeded samples as JSON for the contact sheets, into the directory
//!   `HEAD_QUALITY_OUT` names.
//!
//! `cargo test -p omuzaki-char --release --test head_quality -- --ignored --nocapture report`

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers: a bad fixture should stop the test with its reason
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)] // grid counts, pixels
#![allow(
    clippy::disallowed_methods,
    reason = "tests read fixtures and write evidence to a folder the developer names; the fence is for ops"
)]

mod anchors;
mod metrics;
mod space;

use std::fmt::Write as _;
use std::path::PathBuf;

use metrics::{Quality, Target, measure};
use omuzaki_char::head::cranium;
use omuzaki_char::head::mesh::{Resolution, build, build_head};
use omuzaki_char::head::outline::plausible;
use omuzaki_char_api::{Chin, Cranium, HeadOutline, HeadParams, HeadSurface};
use omuzaki_core::glam::{DVec3, Vec3};
use omuzaki_core::mesh::MeshData;
use omuzaki_face_api::Validate;
use proptest::prelude::*;
use proptest::test_runner::RngSeed;
use space::Population;

/// Samples per population in the report.
const REPORT_SAMPLES: u64 = 600;

/// The limits of a plausible head.
///
/// - **Flange**: the issue's 3 mm.
/// - **Chin**: front 130 and side 95 degrees. These are the pointiest chins of five CC0 anime references, measured the
///   same way (h = 0.25 x the face's half-width at the eye line) and rounded down to 5 degrees: front 134.6 (`VRoid` Project
///   base female and base male), side 96.0 (base male). The others are blunter: `Barely_Banquo`'s
///   "Generic Anime Face" 158.0 / 149.4, stevenburrow's anime portraits 142.4 and 142.8 in front.
/// - **Crease**: 19 degrees, the tuned close-up profile's join as the prototype built it (18.92, rounded up),
///   the value first proposed and later restored. The outline starts straight down at the eye line, where the cranium
///   above is already narrowing, so the side of the face turns in above the eye line and back out below it. Eased into
///   the cranium over a fixed 3 cm that S-bend read as a ledge (the close-up 22.5 degrees, the worst head 27.8), and
///   the limit was 26 until the blend band grew with the slope mismatch. Now the close-up measures 9.8 and the
///   worst of 4 000 even samples of each population (crania within 10%) 13.5: the limit leaves room for the
///   corners of the plausible space, where the width, exponent, chin and stretch line all sit at their limits at once.
/// - **Ripple**: 2.5 degrees, the close-up's 2.08 under the 3 cm join with room for the corners (worst 2.45 then). The
///   smooth join brought the close-up to 1.1 and the worst sample to 1.46.
pub mod limits {
    /// Degrees.
    pub const CREASE: f64 = 19.0;
    /// Millimetres.
    pub const FLANGE: f64 = 3.0;
    /// Degrees.
    pub const FRONT_TIP: f64 = 130.0;
    /// Degrees.
    pub const SIDE_TIP: f64 = 95.0;
    /// Degrees.
    pub const RIPPLE: f64 = 2.5;
}

/// Which limits `q` meets: crease, flange, chin (front and side), ripple.
fn passes(q: &Quality) -> [bool; 4] {
    [
        q.crease <= limits::CREASE,
        q.flange <= limits::FLANGE,
        q.front_tip >= limits::FRONT_TIP && q.side_tip >= limits::SIDE_TIP,
        q.ripple <= limits::RIPPLE,
    ]
}
/// Seeded samples per population on the contact sheets.
const SHEET_SAMPLES: u64 = 10;

fn live(params: &HeadParams) -> MeshData {
    build(params, Resolution::LIVE, None).unwrap()
}

/// The outline the head is measured against: the one it is built from, the slider's outline clamped to the plausible
/// space (`outline::plausible`).
fn target_of(params: &HeadParams) -> Target {
    Target::of(&plausible(&params.outline, &params.cranium))
}

fn quality(params: &HeadParams) -> Quality {
    measure(&live(params), Resolution::LIVE, params, &target_of(params))
}

fn out_dir() -> Option<PathBuf> {
    std::env::var_os("HEAD_QUALITY_OUT").map(PathBuf::from)
}

// ------------------------------------------------------------------------------------------------ the guarantee

/// Asserts `q` is within every limit.
fn assert_plausible(name: &str, q: &Quality) {
    let [crease, flange, chin, ripple] = passes(q);
    assert!(crease, "{name}: crease {:.2} deg at y {:.3} (limit {})", q.crease, q.crease_y, limits::CREASE);
    assert!(flange, "{name}: flange {:.2} mm (limit {})", q.flange, limits::FLANGE);
    assert!(chin, "{name}: chin tip {:.1} / {:.1} deg", q.front_tip, q.side_tip);
    assert!(ripple, "{name}: ripple {:.2} deg (limit {})", q.ripple, limits::RIPPLE);
}

fn arb_outline() -> impl Strategy<Value = HeadOutline> {
    let chin = (-0.5..-0.001_f64, 0.0..1.0_f64, 0.2..1.0_f64).prop_map(|(y, gap, min_ratio)| Chin {
        y,
        stretch_top: y + 0.001 + gap * (0.2 - y - 0.001),
        min_ratio,
    });
    (0.05..0.4_f64, -0.3..0.2_f64, 0.02..0.4_f64, 1.0..4.0_f64, proptest::option::of(chin), 0.0..0.05_f64).prop_map(
        |(half_width, top, height, exponent, chin, cheek_puff)| HeadOutline {
            half_width,
            top,
            height,
            exponent,
            chin,
            cheek_puff,
        },
    )
}

/// A cranium whose six extents are within 10% of the default's, as the face-shape guarantee covers (5% before
/// the eye-line join was smoothed: on 10% the old join's ripple reached 2.52 degrees over the 6 cm the metrics then read).
fn arb_cranium() -> impl Strategy<Value = Cranium> {
    let d = Cranium::default();
    let s = 0.9..1.1_f64;
    (s.clone(), s.clone(), s.clone(), s.clone(), s.clone(), s, 0.0..0.03_f64).prop_map(
        move |(w, df, db, up, dnb, dnf, cheek)| Cranium {
            half_width: d.half_width * w,
            half_depth_front: d.half_depth_front * df,
            half_depth_back: d.half_depth_back * db,
            half_height_up: d.half_height_up * up,
            half_height_down_back: d.half_height_down_back * dnb,
            half_height_down_front: d.half_height_down_front * dnf,
            cheek_fullness: cheek,
            ..d.clone()
        },
    )
}

/// Deterministic, 24 cases, unless `PROPTEST_RNG_SEED` or `PROPTEST_CASES` say otherwise (to explore further:
/// `PROPTEST_RNG_SEED=7 PROPTEST_CASES=2000`, in release), and without regression files: a failure found that way
/// becomes an ordinary test. The seed is the workspace's (`omuzaki-doc/tests/properties.rs`).
fn config() -> ProptestConfig {
    let mut config = ProptestConfig { failure_persistence: None, ..ProptestConfig::default() };
    if matches!(config.rng_seed, RngSeed::Random) {
        config.rng_seed = RngSeed::Fixed(0x5EED);
    }
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = 24;
    }
    config
}

proptest! {
    #![proptest_config(config())]

    /// Every outline the contract accepts, on a cranium within 10% of the default, gives a plausible head.
    #[test]
    fn every_face_shape_slider_position_gives_a_plausible_head(outline in arb_outline(), cranium in arb_cranium()) {
        let params = HeadParams { cranium, outline, ..HeadParams::default() };
        prop_assume!(params.validate().is_ok());
        assert_plausible(&format!("{:?}", params.outline), &quality(&params));
    }
}

/// Seeded samples per population the anchor test builds, unless `HEAD_QUALITY_ANCHOR_SAMPLES` says otherwise.
const ANCHOR_SAMPLES: u64 = 16;

/// The named profiles (and the optional local ones, when present), labelled.
fn profiles() -> Vec<(String, HeadParams)> {
    let mut heads = space::named();
    heads.extend(space::private().unwrap_or_default());
    heads.into_iter().map(|(name, params)| (name.to_owned(), params)).collect()
}

/// Ear roots and hair roots placed on the head as built land on its mesh, on the named profiles and on seeded samples
/// of the valid range and the preset band: the head part publishes this surface, so nothing floats off a
/// reshaped face.
#[test]
fn ear_roots_and_hair_roots_sit_on_the_built_head() {
    let count =
        std::env::var("HEAD_QUALITY_ANCHOR_SAMPLES").ok().and_then(|n| n.parse().ok()).unwrap_or(ANCHOR_SAMPLES);
    let mut heads = profiles();
    for population in [Population::Valid, Population::Presets] {
        heads.extend((0..count).map(|i| (format!("{} #{i}", population.name()), space::sample(population, i))));
    }
    let (mut worst, mut floating) = (0.0_f64, 0.0_f64);
    for (name, params) in &heads {
        let built = build_head(params, Resolution::LIVE, None).unwrap();
        let gap = anchors::worst_on(params, &built.mesh, built.surface.as_ref());
        assert!(gap <= anchors::ON_SURFACE, "{name}: an anchor is {:.3} mm off the head", gap * 1e3);
        worst = worst.max(gap);
        // What the anchors did when they were placed on the unreshaped cranium.
        floating = floating.max(anchors::worst_on(params, &built.mesh, cranium::surface(params, None).as_ref()));
    }
    eprintln!(
        "anchors over {} heads: on the built head at most {:.3} mm off the mesh; on the cranium {:.2} mm",
        heads.len(),
        worst * 1e3,
        floating * 1e3
    );
    assert!(floating > 0.005, "the test bites: on the cranium the anchors were only {:.2} mm off", floating * 1e3);
}

#[test]
fn the_named_profiles_are_plausible() {
    for (name, params) in &profiles() {
        assert_plausible(name, &quality(params));
    }
}

// ------------------------------------------------------------------------------------------------ report

fn percentile(sorted: &[f64], p: f64) -> f64 {
    let i = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[i]
}

/// Reads one metric off a head's quality.
type Metric = fn(&Quality) -> f64;

/// One row per metric: min, median, 90th and 99th percentile, max.
fn summary(name: &str, rows: &[Quality], out: &mut String) {
    let columns: [(&str, Metric); 6] = [
        ("crease (deg)", |q| q.crease),
        ("flange (mm)", |q| q.flange),
        ("spike: chin tip (deg)", |q| q.spike),
        ("  front tip (deg)", |q| q.front_tip),
        ("  side tip (deg)", |q| q.side_tip),
        ("ripple (deg)", |q| q.ripple),
    ];
    writeln!(out, "\n**{name}** ({} samples)\n", rows.len()).unwrap();
    writeln!(out, "| metric | min | median | p90 | p99 | max |").unwrap();
    writeln!(out, "|---|---|---|---|---|---|").unwrap();
    for (label, get) in columns {
        let mut v: Vec<f64> = rows.iter().map(get).collect();
        v.sort_by(f64::total_cmp);
        writeln!(
            out,
            "| {label} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} |",
            v[0],
            percentile(&v, 0.5),
            percentile(&v, 0.9),
            percentile(&v, 0.99),
            v[v.len() - 1]
        )
        .unwrap();
    }
    let rate = |k: Option<usize>| {
        let n = rows.iter().filter(|q| k.map_or_else(|| passes(q).iter().all(|p| *p), |k| passes(q)[k])).count();
        format!("{:.1}%", 100.0 * n as f64 / rows.len() as f64)
    };
    writeln!(
        out,
        "\nWithin the limits: crease {}, flange {}, chin {}, ripple {}; all four {}.",
        rate(Some(0)),
        rate(Some(1)),
        rate(Some(2)),
        rate(Some(3)),
        rate(None)
    )
    .unwrap();
}

fn row(name: &str, q: &Quality) -> String {
    let failed: Vec<&str> = ["crease", "flange", "chin", "ripple"]
        .into_iter()
        .zip(passes(q))
        .filter(|(_, ok)| !ok)
        .map(|(n, _)| n)
        .collect();
    let verdict = if failed.is_empty() { "all".to_owned() } else { format!("fails {}", failed.join(", ")) };
    format!(
        "| {name} | {:.2} | {:.2} | {:.1} / {:.1} | {:.2} | {verdict} |",
        q.crease, q.flange, q.front_tip, q.side_tip, q.ripple
    )
}

#[test]
#[ignore = "evidence: 1 200 Live builds; run with --release"]
fn report() {
    let mut out = String::new();
    let mut csv = String::from(
        "population,index,crease,crease_y,crease_phi,flange,spike,front_tip,side_tip,ripple,lowest,half_width,top,height,exponent,chin,stretch_top,min_ratio,puff\n",
    );
    writeln!(
        out,
        "| profile | crease (deg) | flange (mm) | chin tip front / side (deg) | ripple (deg) | within limits |"
    )
    .unwrap();
    writeln!(out, "|---|---|---|---|---|---|").unwrap();
    for (name, params) in &profiles() {
        writeln!(out, "{}", row(name, &quality(params))).unwrap();
    }
    for population in [Population::Valid, Population::Presets] {
        let count = std::env::var("HEAD_QUALITY_SAMPLES").ok().and_then(|n| n.parse().ok()).unwrap_or(REPORT_SAMPLES);
        let samples: Vec<HeadParams> = (0..count).map(|i| space::sample(population, i)).collect();
        let rows: Vec<Quality> = samples.iter().map(quality).collect();
        for (i, (q, p)) in rows.iter().zip(&samples).enumerate() {
            let o = &p.outline;
            let chin =
                o.chin.map_or(",,".to_owned(), |c| format!("{:.4},{:.4},{:.3}", c.y, c.stretch_top, c.min_ratio));
            writeln!(
                csv,
                "{},{i},{:.4},{:.4},{:.1},{:.4},{:.3},{:.3},{:.3},{:.4},{:.4},{:.4},{:.4},{:.4},{:.3},{chin},{:.4}",
                population.name(),
                q.crease,
                q.crease_y,
                q.crease_phi,
                q.flange,
                q.spike,
                q.front_tip,
                q.side_tip,
                q.ripple,
                q.lowest,
                o.half_width,
                o.top,
                o.height,
                o.exponent,
                o.cheek_puff
            )
            .unwrap();
        }
        summary(population.name(), &rows, &mut out);
    }
    println!("{out}");
    if let Some(dir) = out_dir() {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("metrics.csv"), csv).unwrap();
        std::fs::write(dir.join("report.md"), out).unwrap();
    }
}

// ------------------------------------------------------------------------------------------------ contact-sheet dump

/// `head`'s radius over a grid of directions down to 130 degrees from the crown (every 2.5 degrees, and every 5 round),
/// rows of `theta` then `phi`: enough for a sheet to draw what sits on the head.
fn radius_grid(head: &dyn HeadSurface) -> Vec<f32> {
    (0..=52)
        .flat_map(|i| (0..72).map(move |j| (f64::from(i) * 2.5, f64::from(j) * 5.0)))
        .map(|(theta, phi)| {
            let (theta, phi) = (theta.to_radians(), phi.to_radians());
            let dir = DVec3::new(theta.sin() * phi.sin(), theta.cos(), theta.sin() * phi.cos()).as_vec3();
            head.radius(dir)
        })
        .collect()
}

/// The ear roots and the radius grid on the built head and on the unreshaped cranium, for the anchor sheets.
fn json_anchors(params: &HeadParams, built: &dyn HeadSurface) -> serde_json::Value {
    let on_cranium = cranium::surface(params, None);
    let ears = |head: &dyn HeadSurface| {
        anchors::anchors(&params.ears, head)[..2].iter().map(Vec3::to_array).collect::<Vec<_>>()
    };
    serde_json::json!({
        "ears": { "built": ears(built), "cranium": ears(on_cranium.as_ref()) },
        "grid": { "theta_step": 2.5, "phi_step": 5.0, "built": radius_grid(built), "cranium": radius_grid(on_cranium.as_ref()) },
    })
}

fn json_mesh(name: &str, params: &HeadParams) -> serde_json::Value {
    let built = build_head(params, Resolution::LIVE, None).unwrap();
    let mesh = built.mesh;
    let target = target_of(params);
    let q = measure(&mesh, Resolution::LIVE, params, &target);
    let flat =
        |v: &[omuzaki_core::glam::Vec3]| v.iter().flat_map(omuzaki_core::glam::Vec3::to_array).collect::<Vec<f32>>();
    let outline: Vec<[f64; 2]> = (0..=200)
        .map(|i| {
            let y = target.top - (target.top - target.chin) * f64::from(i) / 200.0;
            [target.half_width_at(y), y]
        })
        .collect();
    serde_json::json!({
        "name": name,
        "centre": params.cranium.centre,
        "cranium": params.cranium,
        "outline": params.outline,
        "target": { "top": target.top, "chin": target.chin, "half_width": target.half_width, "curve": outline },
        "quality": { "crease": q.crease, "flange": q.flange, "spike": q.spike, "front_tip": q.front_tip,
                     "side_tip": q.side_tip, "ripple": q.ripple, "crease_y": q.crease_y, "crease_phi": q.crease_phi },
        "positions": flat(&mesh.positions),
        "normals": flat(&mesh.normals),
        "indices": mesh.indices,
        "anchors": json_anchors(params, built.surface.as_ref()),
    })
}

#[test]
#[ignore = "evidence: writes the contact-sheet meshes to HEAD_QUALITY_OUT"]
fn dump() {
    let Some(dir) = out_dir() else {
        eprintln!("skipped: set HEAD_QUALITY_OUT to a directory");
        return;
    };
    std::fs::create_dir_all(&dir).unwrap();
    let mut labelled = profiles();
    for population in [Population::Valid, Population::Presets] {
        for i in 0..SHEET_SAMPLES {
            labelled.push((format!("{} #{i}", population.name()), space::sample(population, i)));
        }
    }
    // `HEAD_QUALITY_PICK=valid:137,presets:12` appends particular samples (to look at the worst of a report).
    for pick in std::env::var("HEAD_QUALITY_PICK").unwrap_or_default().split(',').filter(|s| !s.is_empty()) {
        let (name, index) = pick.split_once(':').unwrap();
        let population = if name == "valid" { Population::Valid } else { Population::Presets };
        let index: u64 = index.parse().unwrap();
        labelled.push((format!("{} #{index}", population.name()), space::sample(population, index)));
    }
    let index: Vec<String> = labelled.iter().map(|(n, _)| n.clone()).collect();
    for (k, (name, params)) in labelled.iter().enumerate() {
        std::fs::write(dir.join(format!("head{k:02}.json")), json_mesh(name, params).to_string()).unwrap();
    }
    std::fs::write(dir.join("index.json"), serde_json::to_string(&index).unwrap()).unwrap();
}
