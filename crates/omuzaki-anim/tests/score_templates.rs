//! The built-in score templates, held to their acceptance numerically: no rendering, only the sampled channels.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::float_cmp,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use omuzaki_anim::score_templates::{ScoreTemplateError, ScoreTemplates};
use omuzaki_anim_api::{BlendMode, Channel, ChannelBuf, CompiledScore, Cue, CueKind, Layer};
use omuzaki_core::time::Seconds;
use omuzaki_face_api::ExpressionSet;

const TOL: f32 = 0.02;

fn templates() -> &'static ScoreTemplates {
    ScoreTemplates::builtin().expect("the built-in templates load")
}

fn compiled(id: &str) -> CompiledScore {
    templates().get(id).unwrap_or_else(|| panic!("no template {id}")).score.compile().unwrap()
}

fn sample(score: &CompiledScore, t: f64) -> ChannelBuf {
    let mut layer = Layer::absent(BlendMode::Override);
    score.sample(Seconds(t), &mut layer);
    layer.values
}

fn value(score: &CompiledScore, name: &str, t: f64) -> f32 {
    sample(score, t).get(Channel::from_name(name).unwrap())
}

fn near(got: f32, want: f32, what: &str) {
    assert!((got - want).abs() <= TOL, "{what}: {got} vs {want}");
}

fn markers(cues: &[Cue], name: &str) -> usize {
    cues.iter().filter(|c| matches!(&c.kind, CueKind::Marker { name: n } if n == name)).count()
}

fn clips(cues: &[Cue], id: &str) -> usize {
    cues.iter().filter(|c| matches!(&c.kind, CueKind::Clip { clip, .. } if clip.as_str() == id)).count()
}

#[test]
fn the_nine_templates_load_in_order() {
    let ids: Vec<&str> = templates().all().iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, ["intro-wave", "outro", "kiss-mwah", "laugh", "gasp", "sad", "smug", "shy", "angry"]);
    for t in templates().all() {
        assert!(t.origin.starts_with("design target: "), "{}", t.id);
    }
}

#[test]
fn every_template_stays_in_range_and_returns_to_rest() {
    for t in templates().all() {
        let score = t.score.compile().unwrap();
        let duration = t.score.duration.0;
        for track in &t.score.tracks {
            for key in &track.keys {
                assert!((0.0..=duration).contains(&key.time.0), "{}: {} key at {}", t.id, track.channel, key.time.0);
            }
        }
        let steps = (duration * 100.0).round() as u32;
        for step in 0..=steps {
            let raw = sample(&score, f64::from(step) / 100.0);
            let mut clean = raw;
            clean.sanitize();
            assert_eq!(raw, clean, "{} at step {step}", t.id);
        }
        let end = sample(&score, duration);
        for track in &t.score.tracks {
            near(end.get(track.channel), track.initial, &format!("{} {} at the end", t.id, track.channel));
        }
    }
}

#[test]
fn sampling_is_deterministic() {
    for t in templates().all() {
        let (a, b) = (t.score.compile().unwrap(), t.score.compile().unwrap());
        for step in 0..=(t.score.duration.0 * 100.0) as u32 {
            let time = f64::from(step) / 100.0;
            assert_eq!(sample(&a, time).checksum(), sample(&b, time).checksum(), "{} at {time}", t.id);
        }
    }
}

#[test]
fn cues_use_only_known_ids() {
    let expressions = ExpressionSet::starter();
    let mut clip_ids = std::collections::BTreeSet::new();
    for t in templates().all() {
        for cue in &t.score.cues {
            match &cue.kind {
                CueKind::Expression { expression, .. } => {
                    assert!(expressions.0.contains_key(expression), "{}: unknown expression {expression}", t.id);
                }
                // The pose seed is not in this crate, so clip ids can only be listed.
                CueKind::Clip { clip, .. } => {
                    clip_ids.insert(clip.as_str().to_owned());
                }
                _ => {}
            }
        }
    }
    assert_eq!(clip_ids.into_iter().collect::<Vec<_>>(), ["bow", "wave"]);
}

#[test]
fn intro_wave_beats() {
    let s = compiled("intro-wave");
    near(value(&s, "mouthSmileLeft", 1.0), 0.7, "smile at 1.0");
    near(value(&s, "mouthSmileLeft", 3.0), 0.0, "smile at 3.0");
    near(value(&s, "headRoll", 1.2), 0.08, "roll at 1.2");
    near(value(&s, "headRoll", 3.0), 0.0, "roll at 3.0");
    let at_start = s.fired(Seconds(-0.01), Seconds(0.0));
    assert_eq!(at_start.len(), 1);
    assert!(matches!(&at_start[0].kind, CueKind::Marker { name } if name == "intro"));
    assert_eq!(clips(s.fired(Seconds(0.35), Seconds(0.45)), "wave"), 1);
    assert_eq!(clips(s.cues(), "wave"), 1);
}

#[test]
fn outro_beats() {
    let s = compiled("outro");
    assert_eq!(markers(s.fired(Seconds(-0.01), Seconds(0.0)), "outro"), 1);
    assert_eq!(markers(s.fired(Seconds(3.99), Seconds(4.0)), "end"), 1);
    assert_eq!((clips(s.cues(), "wave"), clips(s.cues(), "bow")), (1, 1));
    near(value(&s, "mouthSmileRight", 2.0), 0.7, "smile at 2.0");
    near(value(&s, "headPitch", 3.0), 0.2, "bow at 3.0");
    near(value(&s, "headPitch", 4.0), 0.0, "bow at 4.0");
}

#[test]
fn kiss_mwah_beats() {
    let s = compiled("kiss-mwah");
    near(value(&s, "mouthPucker", 0.4), 0.9, "pucker at 0.4");
    near(value(&s, "mouthPucker", 0.6), 0.0, "pucker at 0.6");
    near(value(&s, "jawOpen", 0.55), 0.25, "jaw at 0.55");
    assert_eq!(markers(s.fired(Seconds(0.45), Seconds(0.55)), "mwah"), 1);
    assert_eq!(markers(s.cues(), "mwah"), 1);
    near(value(&s, "eyesHeart", 1.4), 0.0, "hearts at 1.4");
}

#[test]
fn laugh_pulses_four_times() {
    let s = compiled("laugh");
    let (mut pulses, mut open) = (0, false);
    for step in 0..=160 {
        let high = value(&s, "jawOpen", f64::from(step) / 100.0) >= 0.45;
        if high && !open {
            pulses += 1;
        }
        open = high;
    }
    assert_eq!(pulses, 4);
}

#[test]
fn gasp_raises_the_brows() {
    near(value(&compiled("gasp"), "browInnerUp", 0.3), 0.9, "brows at 0.3");
}

#[test]
fn parse_reports_the_file_and_refuses_bad_input() {
    let good = include_str!("../library/scores/gasp.toml");
    let err = |text: &str| ScoreTemplates::parse(&[("a.toml", text)]).unwrap_err();
    assert!(matches!(err("id = 1"), ScoreTemplateError::Parse { file, .. } if file == "a.toml"));
    assert!(matches!(err(&format!("extra = 1\n{good}")), ScoreTemplateError::Parse { .. }));
    assert!(matches!(err(&good.replace("[score]", "[score]\nextra = 1")), ScoreTemplateError::Parse { .. }));
    assert!(matches!(err(&good.replace("design target: ", "")), ScoreTemplateError::Header { .. }));
    assert!(matches!(err(&good.replace("id = \"gasp\"", "id = \"Not A Slug\"")), ScoreTemplateError::Header { .. }));
    assert!(matches!(err(&good.replace("duration = 1.2", "duration = 0.1")), ScoreTemplateError::OutOfRange { .. }));
    assert!(matches!(
        err(&good.replace("channel = \"jawOpen\"", "channel = \"browInnerUp\"")),
        ScoreTemplateError::Score { .. }
    ));
    assert_eq!(
        ScoreTemplates::parse(&[("a.toml", good), ("b.toml", good)]).unwrap_err(),
        ScoreTemplateError::DuplicateId("gasp".into())
    );
}

#[test]
fn a_misspelt_field_anywhere_in_the_score_is_refused() {
    let text = include_str!("../library/scores/intro-wave.toml");
    let err = |from: &str, to: &str| {
        assert!(text.contains(from), "{from} not in the file");
        ScoreTemplates::parse(&[("a.toml", &text.replacen(from, to, 1))]).unwrap_err()
    };
    for (from, to, named) in [
        ("duration =", "durations =", "durations"),
        ("[[score.tracks]]\nchannel =", "[[score.tracks]]\ninitals = 0.0\nchannel =", "initals"),
        ("over = 0.35", "ovre = 0.35", "ovre"),
        ("time = 0.2, value", "time = 0.2, easee = \"linear\", value", "easee"),
        ("fade_s =", "fade = 0.25\nfade_s =", "fade"),
        ("name = \"intro\"", "name = \"intro\"\nweight = 1.0", "weight"),
    ] {
        match err(from, to) {
            ScoreTemplateError::Parse { message, .. } => assert!(message.contains(named), "{named}: {message}"),
            other => panic!("{named}: {other}"),
        }
    }
}
