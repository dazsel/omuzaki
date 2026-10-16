//! `HairParams::check` over every generator value (the class sweep of #1775): each number the generators read is
//! finite and in range where it enters, the error names the field's path, and the defaults and the tail and bun presets
//! still pass.

#![allow(clippy::unwrap_used, clippy::expect_used)] // helpers build fixtures; a bad one should stop the test with its reason

use std::collections::BTreeMap;

use omuzaki_char::hair::r#gen::{bun, tail};
use omuzaki_char_api::{
    BangLayer, BangsGen, BraidGen, BunGen, CapGen, CurtainGen, Dynamics, FramingGen, Generator, GroupId, GuideClump,
    GuidePoint, GuidesGen, HairGroup, HairParams, PartSweepGen, TailGen, Wisp,
};
use omuzaki_face_api::{Check, Validate};

/// The check's message for one group holding `generator`, or an empty string if it passes.
fn errors(generator: Generator) -> String {
    let mut c = Check::new();
    c.nested("groups/g", &HairGroup { enabled: true, mirror: false, dynamics: Dynamics::default(), generator });
    c.finish().err().map(|e| e.to_string()).unwrap_or_default()
}

/// Edits one default `G` with each case's closure: every case must fail, naming `groups/g/generator/<path>`.
fn every_case_fails<G: Clone + Default>(wrap: fn(G) -> Generator, cases: &[(&str, &dyn Fn(&mut G))]) {
    assert_eq!(errors(wrap(G::default())), "", "the default passes");
    for (path, edit) in cases {
        let mut g = G::default();
        edit(&mut g);
        let message = errors(wrap(g));
        assert!(
            message.contains(&format!("groups/g/generator/{path}:")),
            "an edit that should fail at `{path}` gave: {message:?}"
        );
    }
}

const NAN: f64 = f64::NAN;

#[test]
fn the_cap_checks_its_style_constants() {
    every_case_fails::<CapGen>(
        Generator::Cap,
        &[
            ("front_blend/0", &|g| g.front_blend[0] = NAN),
            ("front_blend/1", &|g| g.front_blend[1] = 1.5),
            ("notch_edges/0", &|g| g.notch_edges[0] = -1.5),
            ("notch_edges/1", &|g| g.notch_edges[1] = NAN),
            ("notch_width", &|g| g.notch_width = 0.0),
            ("crown_rise", &|g| g.crown_rise = -0.1),
            ("strand_seed", &|g| g.strand_seed = 1.5),
            ("shade", &|g| g.shade = 0.2),
            ("hairline_front_deg", &|g| g.hairline_front_deg = NAN),
            ("thickness", &|g| g.thickness = f64::INFINITY),
        ],
    );
}

#[test]
fn the_part_sweep_checks_every_number() {
    every_case_fails::<PartSweepGen>(
        Generator::PartSweep,
        &[
            ("part_front_deg", &|g| g.part_front_deg = NAN),
            ("part_back_deg", &|g| g.part_back_deg = 200.0),
            ("end_from_deg", &|g| g.end_from_deg = -400.0),
            ("end_to_deg", &|g| g.end_to_deg = f64::INFINITY),
            ("end_height_front", &|g| g.end_height_front = NAN),
            ("end_height_back", &|g| g.end_height_back = 9.0),
            ("part_start", &|g| g.part_start = 0.9),
            ("part_spread", &|g| g.part_spread = -0.01),
            ("end_spread", &|g| g.end_spread = 0.0),
            ("sweep_ease", &|g| g.sweep_ease = NAN),
            ("height_blend_deg/0", &|g| g.height_blend_deg[0] = -1.0),
            ("height_blend_deg/1", &|g| g.height_blend_deg[1] = 181.0),
            ("lift", &|g| g.lift = NAN),
            ("belly", &|g| g.belly = 0.5),
            ("alternate_step", &|g| g.alternate_step = 0.1),
            ("shades/0", &|g| g.shades[0] = 0.1),
            ("shades/1", &|g| g.shades[1] = NAN),
        ],
    );
}

#[test]
fn framing_checks_its_spread_and_outline() {
    every_case_fails::<FramingGen>(
        Generator::Framing,
        &[
            ("spread", &|g| g.spread = NAN),
            ("edge/1/y", &|g| g.edge[1][0] = f64::INFINITY),
            ("edge/2/x", &|g| g.edge[2][1] = 7.0),
        ],
    );
}

#[test]
fn the_braid_checks_its_path_and_strands() {
    every_case_fails::<BraidGen>(
        Generator::Braid,
        &[
            ("path_from_deg", &|g| g.path_from_deg = NAN),
            ("path_to_deg", &|g| g.path_to_deg = 500.0),
            ("height_front", &|g| g.height_front = NAN),
            ("height_back", &|g| g.height_back = -4.0),
            ("across", &|g| g.across = NAN),
            ("out", &|g| g.out = 1.0),
            ("strand_width", &|g| g.strand_width = 5.0),
            ("strand_thickness", &|g| g.strand_thickness = NAN),
            ("lobes", &|g| g.lobes = NAN),
        ],
    );
}

#[test]
fn the_curtain_checks_its_root_heights_and_envelope() {
    every_case_fails::<CurtainGen>(
        Generator::Curtain,
        &[
            ("root_height_front", &|g| g.root_height_front = NAN),
            ("root_height_back", &|g| g.root_height_back = f64::INFINITY),
            ("envelope/2/y", &|g| g.envelope[2][0] = NAN),
            ("envelope/1/distance", &|g| g.envelope[1][1] = -0.5),
            ("length_front", &|g| g.length_front = NAN),
            ("spread_deg", &|g| g.spread_deg = NAN),
        ],
    );
}

#[test]
fn the_bangs_check_every_layer_and_wisp_value() {
    let layer = |edit: fn(&mut BangLayer)| {
        move |g: &mut BangsGen| {
            g.layers = vec![BangLayer::default()];
            edit(&mut g.layers[0]);
        }
    };
    let wisp = |edit: fn(&mut Wisp)| {
        move |g: &mut BangsGen| {
            g.wisps = vec![Wisp::default()];
            edit(&mut g.wisps[0]);
        }
    };
    every_case_fails::<BangsGen>(
        Generator::Bangs,
        &[
            ("sweep_deg", &|g| g.sweep_deg = NAN),
            ("layers/0/tip_heights/0", &layer(|l| l.tip_heights = vec![NAN])),
            ("layers/0/width", &layer(|l| l.width = NAN)),
            ("layers/0/hook_deg", &layer(|l| l.hook_deg = 200.0)),
            ("layers/0/root_offset", &layer(|l| l.root_offset = NAN)),
            ("layers/0/tip_offset", &layer(|l| l.tip_offset = 5.0)),
            ("layers/0/spread_deg", &layer(|l| l.spread_deg = NAN)),
            ("wisps/0/phi_deg", &wisp(|w| w.phi_deg = NAN)),
            ("wisps/0/tip_height", &wisp(|w| w.tip_height = 9.0)),
            ("wisps/0/width", &wisp(|w| w.width = 0.0)),
        ],
    );
}

#[test]
fn guides_check_each_clump_shade() {
    let mut g = GuidesGen::default();
    g.clumps = vec![GuideClump {
        points: vec![GuidePoint::Head([0.0; 3]), GuidePoint::Head([0.0, -0.1, 0.0])],
        ..GuideClump::default()
    }];
    assert_eq!(errors(Generator::Guides(g.clone())), "");
    g.clumps[0].shade = NAN;
    assert!(errors(Generator::Guides(g.clone())).contains("groups/g/generator/clumps/0/shade:"));
    g.clumps[0].shade = 0.2;
    assert!(errors(Generator::Guides(g)).contains("clumps/0/shade"));
}

#[test]
fn the_tail_and_bun_check_their_numbers() {
    every_case_fails::<TailGen>(
        Generator::Tail,
        &[
            ("root", &|g| g.root[0] = NAN),
            ("root/phi_deg", &|g| g.root[1] = 400.0),
            ("root/offset", &|g| g.root[2] = 2.0),
            ("spread_deg", &|g| g.spread_deg = NAN),
            ("length", &|g| g.length = NAN),
            ("droop", &|g| g.droop = 2.0),
            ("tie_radius", &|g| g.tie_radius = 0.0),
            ("chain_bones", &|g| g.chain_bones = 3),
            ("chain_bones", &|g| g.chain_bones = 7),
            ("gather", &|g| g.gather = 13),
            ("width/mid", &|g| g.width.mid = NAN),
            ("thickness/base", &|g| g.thickness.base = NAN),
        ],
    );
    every_case_fails::<BunGen>(
        Generator::Bun,
        &[
            ("centre", &|g| g.centre[0] = NAN),
            ("centre/phi_deg", &|g| g.centre[1] = -400.0),
            ("radius", &|g| g.radius = NAN),
            ("wraps", &|g| g.wraps = f64::INFINITY),
            ("gather", &|g| g.gather = 13),
            ("width/start", &|g| g.width.start = 0.0),
            ("thickness/swell", &|g| g.thickness.swell = -1.0),
        ],
    );
}

#[test]
fn the_defaults_and_every_preset_still_pass() {
    assert_eq!(HairParams::default().validate(), Ok(()));
    let group = |generator: Generator, mirror: bool| HairGroup {
        enabled: true,
        mirror,
        dynamics: Dynamics::default(),
        generator,
    };
    let all: BTreeMap<GroupId, HairGroup> = [
        ("a", group(Generator::Cap(CapGen::default()), false)),
        ("b", group(Generator::PartSweep(PartSweepGen::default()), false)),
        ("c", group(Generator::Framing(FramingGen::default()), false)),
        ("d", group(Generator::Braid(BraidGen::default()), false)),
        ("e", group(Generator::Curtain(CurtainGen::default()), false)),
        ("f", group(Generator::Bangs(BangsGen::default()), false)),
        ("g", group(Generator::Tail(tail::ponytail()), false)),
        ("h", group(Generator::Tail(tail::twintails()), true)),
        ("i", group(Generator::Bun(bun::low_bun()), false)),
        ("j", group(Generator::Bun(bun::high_bun()), false)),
        ("k", group(Generator::Bun(BunGen::default()), false)),
    ]
    .into_iter()
    .map(|(name, g)| (GroupId::new(name).unwrap(), g))
    .collect();
    let params = HairParams { groups: all, ..HairParams::default() };
    assert_eq!(params.validate(), Ok(()));
}
