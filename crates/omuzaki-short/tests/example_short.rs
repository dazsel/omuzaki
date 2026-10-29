//! The project's own example short (`shorts/gerald.short.toml`) reads, times and meets the talking-head bar on its
//! timeline: what `short.plan` reports before anything renders.

use omuzaki_short::ShortDef;
use omuzaki_short::ops::bar_checks;
use omuzaki_short::timeline::Timeline;
use omuzaki_short::voice::voice_for;

const GERALD: &str = include_str!("../shorts/gerald.short.toml");

#[test]
fn the_example_short_meets_the_bar_on_its_timeline() {
    let def = ShortDef::from_toml(GERALD).expect("the example reads");
    let voice = voice_for(&def, |_| unreachable!("the example is silent")).expect("a silent voice");
    let timeline = Timeline::new(&def, &voice).expect("the example times");
    let measures = timeline.measures();
    assert!((40.0..=90.0).contains(&measures.duration_s), "about a minute: {}", measures.duration_s);
    for check in bar_checks(&measures) {
        assert!(check.ok, "{check:?} of {measures:?}");
    }
}
