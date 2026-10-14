//! The project format is TOML (spec §7): every cold type of the contract must survive a round trip through it, and
//! TOML's limits (no top-level arrays, no `null`) must not trip any of them.

use omuzaki_anim_api::{
    Calibration, Channel, ChannelRange, ChannelTrack, Cue, CueKind, DeviceProfile, Ease, HumanoidBone, Key, LifeConfig,
    Personality, Response, RetargetConfig, Score, Skeleton, SpringChain, SpringParams, SpringSet, Transform,
};
use omuzaki_core::glam::Vec3;
use omuzaki_core::time::Seconds;
use serde::{Deserialize, Serialize};

fn round_trip<T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug>(value: &T) -> String {
    let text = toml::to_string(value).unwrap_or_else(|e| panic!("serialising to TOML failed: {e}"));
    let back: T = toml::from_str(&text).unwrap_or_else(|e| panic!("reading back failed: {e}\n{text}"));
    assert_eq!(&back, value, "{text}");
    text
}

#[test]
fn a_score_is_a_readable_toml_file() {
    let score = Score {
        duration: Seconds(4.0),
        tracks: vec![ChannelTrack {
            channel: Channel::MOUTH_SMILE_LEFT,
            initial: 0.0,
            keys: vec![Key { time: Seconds(0.5), value: 0.75, over: 0.25, ease: Ease::OutCubic }],
        }],
        cues: vec![
            Cue { time: Seconds(1.0), kind: CueKind::Marker { name: "intro".into() } },
            Cue {
                time: Seconds(2.0),
                kind: CueKind::Expression {
                    expression: "smug".parse().unwrap(),
                    intensity: 0.5,
                    hold_s: 1.0,
                    fade_s: 0.2,
                },
            },
        ],
    };
    let text = round_trip(&score);
    assert!(text.contains("channel = \"mouthSmileLeft\""), "{text}");
}

#[test]
fn performer_settings_round_trip() {
    let mut cal = Calibration::identity(DeviceProfile::Webcam);
    cal.set_range(Channel::JAW_OPEN, ChannelRange { lo: 0.05, hi: 0.5 });
    assert!(round_trip(&cal).contains("jawOpen"));

    let mut config = RetargetConfig { mirror: true, ..RetargetConfig::default() };
    config.responses.insert(Channel::JAW_OPEN, Response { dead: 0.1, ..Response::default() });
    round_trip(&config);

    round_trip(&LifeConfig::new(42).with_personality(Personality::Shy));
}

#[test]
fn rig_data_round_trips_under_a_table() {
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct RigFile {
        skeleton: Skeleton,
        springs: SpringSet,
    }
    let skeleton =
        Skeleton::humanoid(&HumanoidBone::ALL, |_| Transform::from_translation(Vec3::new(0.0, 0.1, 0.0))).unwrap();
    let head = skeleton.humanoid_bone(HumanoidBone::Head).unwrap();
    let mut chain = SpringChain::new("ear", &[head, head], SpringParams::default());
    chain.center = Some(head);
    let file = RigFile { skeleton, springs: SpringSet { chains: vec![chain], groups: vec![] } };
    round_trip(&file);
}
