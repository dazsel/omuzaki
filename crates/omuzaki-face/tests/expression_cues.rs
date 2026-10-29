//! Cuts and a short's cue track (#2409): `ExpressionPlayer::cut` switches faces on one frame, and `CueTrack` plays a
//! short's cues through the player at their own times, frame-exact and the same on every run.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::float_cmp)]

use omuzaki_anim_api::{Channel, ChannelBuf};
use omuzaki_face::player::{CueAction, CueError, CueTrack, ExpressionCue, ExpressionPlayer};
use omuzaki_face_api::{ExpressionId, ExpressionSet};

fn id(name: &str) -> ExpressionId {
    ExpressionId::new(name).unwrap()
}

fn cue(at_s: f64, name: &str, action: CueAction) -> ExpressionCue {
    ExpressionCue { at_s, expression: id(name), action }
}

#[test]
fn a_cut_shows_the_new_mood_at_once_and_drops_the_old() {
    let set = ExpressionSet::library().unwrap();
    let mut player = ExpressionPlayer::new(None);
    player.show(&set, &id("happy")).unwrap();
    player.step(0.5);
    player.cut(&set, &id("rage")).unwrap();
    assert_eq!((player.weight(&id("rage")), player.weight(&id("happy"))), (1.0, 0.0), "before any step");
    let rage = player.blend(&set, &ChannelBuf::NEUTRAL);
    assert_eq!(rage[Channel::ANGER_VEIN], 1.0);
    // A momentary accent cuts in over the mood and leaves it be.
    player.cut(&set, &id("vein")).unwrap();
    assert_eq!((player.weight(&id("vein")), player.weight(&id("rage"))), (1.0, 1.0));
}

#[test]
fn the_cue_track_plays_each_cue_at_its_own_time_whatever_the_frame_rate() {
    let set = ExpressionSet::library().unwrap();
    let cues = vec![
        cue(1.0, "shocked", CueAction::Cut),
        cue(0.0, "happy", CueAction::Show),
        cue(2.0, "deadpan", CueAction::Cut),
        cue(3.0, "deadpan", CueAction::Hide),
    ];
    let face_at = |fps: f64, t: f64| {
        let mut track = CueTrack::new(&set, None, cues.clone()).unwrap();
        let mut frame = 0.0;
        while frame / fps < t {
            track.advance_to(&set, frame / fps).unwrap();
            frame += 1.0;
        }
        track.advance_to(&set, t).unwrap();
        (
            track.player().weight(&id("happy")),
            track.player().weight(&id("shocked")),
            track.blend(&set, &ChannelBuf::NEUTRAL),
        )
    };
    // Happy fades in over its 0.12 s; at 1 s the cut takes the face to shocked on the frame.
    let (happy, shocked, _) = face_at(30.0, 0.5);
    assert_eq!((happy, shocked), (1.0, 0.0));
    let (happy, shocked, face) = face_at(30.0, 1.0);
    assert_eq!((happy, shocked), (0.0, 1.0), "a cut, not a cross-fade");
    assert_eq!(face[Channel::EYES_BLANK], 1.0);
    // The same faces at 24 and 60 frames a second, at the times both land on.
    for t in [0.5, 1.0, 2.0, 2.5] {
        let (a, b) = (face_at(24.0, t).2, face_at(60.0, t).2);
        for c in [Channel::EYES_BLANK, Channel::EYES_PILL, Channel::MOUTH_SMILE_LEFT] {
            assert!((a[c] - b[c]).abs() < 1e-4, "{c} at {t} s: {} at 24 fps, {} at 60", a[c], b[c]);
        }
    }
    assert_eq!(face_at(30.0, 2.0).2[Channel::EYES_PILL], 1.0);
    // Hidden at 3 s, deadpan fades out over its own fade-out.
    assert!(face_at(30.0, 3.5).2[Channel::EYES_PILL] < 1e-6);
}

#[test]
fn a_bad_cue_is_refused() {
    let set = ExpressionSet::library().unwrap();
    let bad_time = CueTrack::new(&set, None, vec![cue(-1.0, "happy", CueAction::Show)]);
    assert!(matches!(bad_time, Err(CueError::BadTime { index: 0, .. })));
    let unknown = CueTrack::new(&set, None, vec![cue(0.0, "nope", CueAction::Show)]);
    assert!(matches!(unknown, Err(CueError::Unknown(_))));
}
