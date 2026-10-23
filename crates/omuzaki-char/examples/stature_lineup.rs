//! Prints each template's real-world stature and a front-view outline at that size as JSON, for `tools/stature_lineup.py`
//! to draw next to a 1 m grid (`cargo run -p omuzaki-char --example stature_lineup > lineup.json`).
//!
//! The outline is the layout's cross-sections (the body mesh is not built from them yet), scaled by the template's
//! stature scale, so it shows exactly the proportions and sizes a character of the template is built at.

#![allow(clippy::unwrap_used, clippy::expect_used)] // a tool: a template without a layout should stop it loudly

use omuzaki_char::template::layout::{BodyLayout, section_basis};
use omuzaki_char::template::{Template, registry};
use omuzaki_char_api::{BodyRegion, BodyShape, HandParams, Proportions};
use serde_json::{Value, json};

const REGIONS: [BodyRegion; 6] =
    [BodyRegion::Torso, BodyRegion::Neck, BodyRegion::ArmL, BodyRegion::ArmR, BodyRegion::LegL, BodyRegion::LegR];
const SAMPLES: u32 = 24;

/// Both edges of a region seen from the front, as `[x, y]` points down one side and back up the other, in metres.
fn outline(layout: &BodyLayout, region: BodyRegion, scale: f64) -> Value {
    let region_curves = layout.region(region).expect("the template builds every region");
    let (mut left, mut right) = (Vec::new(), Vec::new());
    for i in 0..=SAMPLES {
        let section = region_curves.section(f64::from(i) / f64::from(SAMPLES));
        let (across, _) = section_basis(region, section.axis.as_vec3());
        let half = across.as_dvec3() * section.radii.x;
        left.push([(section.centre.x - half.x) * scale, (section.centre.y - half.y) * scale]);
        right.push([(section.centre.x + half.x) * scale, (section.centre.y + half.y) * scale]);
    }
    right.reverse();
    left.extend(right);
    json!(left)
}

fn describe(t: &dyn Template) -> Value {
    let defaults = t.defaults();
    let layout = t.layout(&Proportions::default(), &BodyShape::default(), &HandParams::default());
    let stature = t.stature(&layout).expect("the template has a stature");
    let scale = f64::from(stature.scale);
    let c = &defaults.head.cranium;
    // Where the head part puts the cranium: seated on the template's head joint, then lowered by the neck's drop.
    let joint = layout.head_base().expect("the template has a head joint");
    let [x, y, _] = omuzaki_char::head::part::seat(c, joint.y);
    let centre = [x, y - defaults.head.neck.drop];
    json!({
        "id": t.id(),
        "heads_tall": t.heads_tall(),
        "default_stature_m": t.default_stature_m(),
        "authored_height_m": t.authored_height_m(),
        "scale": scale,
        "height_m": stature.height_m,
        "head": {
            "centre": [centre[0] * scale, centre[1] * scale],
            "half_width": c.half_width * scale,
            "half_up": c.half_height_up * scale,
            "half_down": c.half_height_down_front * scale,
        },
        "regions": REGIONS.iter().map(|r| outline(&layout, *r, scale)).collect::<Vec<_>>(),
    })
}

fn main() {
    let all: Vec<Value> = registry::templates().into_iter().map(describe).collect();
    println!("{}", serde_json::to_string_pretty(&all).unwrap());
}
