// Test setup may panic: a panic is a clear failure.
#![allow(clippy::unwrap_used, clippy::float_cmp)]

use super::*;

fn frame(set: &[(Channel, f32)]) -> ChannelBuf {
    let mut buf = ChannelBuf::default();
    for &(c, v) in set {
        buf.set(c, v);
    }
    buf
}

#[test]
fn the_builtin_map_feeds_every_table_channel_as_a_custom_parameter() {
    let map = ChannelMap::builtin().unwrap();
    let names: Vec<_> = map.params().map(|p| p.name.as_str()).collect();
    assert_eq!(names.len(), 82, "52 ARKit, 8 head and body, 22 extras; every default off");
    assert_eq!(names[0], custom_name("Omuzaki", CHANNELS[0].name));
    assert!(names.contains(&"OmuzakiJawOpen"));
    assert!(names.contains(&"OmuzakiHeadYaw"));
    assert!(!names.iter().any(|n| n.contains("Custom")), "the custom slots are not fed");
    assert_eq!(map.creations().len(), 82);
    let yaw = map.params().find(|p| p.name == "OmuzakiHeadYaw").unwrap();
    assert_eq!((yaw.min, yaw.max), Channel::HEAD_YAW.range());
    assert_eq!(map.defaults().count(), 17);
}

#[test]
fn defaults_are_opt_in_and_use_their_formula() {
    let mut map = ChannelMap::builtin().unwrap();
    map.enable_default("EyeOpenLeft", true).unwrap();
    map.enable_default("FaceAngleX", true).unwrap();
    assert!(matches!(map.enable_default("EyeOpenLeftt", true), Err(MapError::UnknownDefault(_))));
    let injector = Injector::new(&map);
    assert_eq!(injector.params().len(), 84);
    injector.push(&frame(&[(Channel::EYE_BLINK_LEFT, 0.25), (Channel::HEAD_YAW, 0.2)]), true);
    let mut snap = Snapshot::default();
    assert!(injector.read(&mut snap));
    let at = |name: &str| snap.values[injector.params().iter().position(|p| p.name == name).unwrap()];
    assert!((at("EyeOpenLeft") - 0.75).abs() < 1e-6, "1 - blink");
    assert!((at("FaceAngleX") - 0.2 * 57.29578).abs() < 1e-3, "radians to degrees");
    assert!((at("OmuzakiEyeBlinkLeft") - 0.25).abs() < 1e-6);
    assert!(snap.face_found);
    assert!(!injector.read(&mut snap), "nothing new until the next push");
}

#[test]
fn a_map_that_names_no_channel_says_which_one_was_meant() {
    let text = "[custom]\nprefix = \"Omuzaki\"\n[[default]]\nparam = \"MouthOpen\"\nterms = [{ channel = \"jawOpne\", weight = 1.0 }]\n";
    let err = ChannelMap::parse(text).unwrap_err();
    assert_eq!(err, MapError::UnknownChannel { name: "jawOpne".into(), nearest: Some("jawOpen".into()) });
    assert!(matches!(ChannelMap::parse("[custom]\nprefix = \"Om\"\nextra = 1\n"), Err(MapError::Parse(_))));
    assert!(matches!(ChannelMap::parse("[custom]\nprefix = \"Omuzaki_\"\n"), Err(MapError::BadName { .. })));
}

#[test]
fn values_outside_a_channel_range_are_clamped_and_nan_rests() {
    let map = ChannelMap::builtin().unwrap();
    let injector = Injector::new(&map);
    injector.push(&frame(&[(Channel::JAW_OPEN, 7.0), (Channel::MOUTH_CLOSE, f32::NAN)]), false);
    let mut snap = Snapshot::default();
    injector.read(&mut snap);
    let at = |name: &str| snap.values[injector.params().iter().position(|p| p.name == name).unwrap()];
    assert_eq!(at("OmuzakiJawOpen"), 1.0);
    assert_eq!(at("OmuzakiMouthClose"), 0.0);
}

#[test]
fn the_pacer_sends_each_frame_at_most_sixty_a_second_and_keeps_alive() {
    let mut pacer = Pacer::default();
    assert_eq!(pacer.due(Duration::ZERO, false), None, "nothing to send before the first frame");
    assert_eq!(pacer.due(Duration::ZERO, true), Some(Due::Frame));
    pacer.sent(Duration::ZERO);
    assert_eq!(pacer.due(Duration::from_millis(5), true), None, "too soon after the last send");
    assert_eq!(pacer.due(MIN_SEND_GAP, true), Some(Due::Frame), "exactly 60 a second passes");
    assert_eq!(pacer.due(Duration::from_millis(499), false), None);
    assert_eq!(pacer.due(KEEPALIVE_SEND, false), Some(Due::KeepAlive));
}

#[test]
fn a_late_look_does_not_push_later_sends_back() {
    // Frames every 1/60 s; the bridge looks every 1.3 ms, so it sees each frame a little late, by a varying amount.
    // Every frame is sent, none merges with the next.
    let look = Duration::from_micros(1_300);
    let mut pacer = Pacer::default();
    let (mut sent, mut fresh, mut next_frame, mut now) = (0_u32, false, Duration::ZERO, Duration::ZERO);
    while now < Duration::from_secs(10) {
        if now >= next_frame {
            fresh = true;
            next_frame += MIN_SEND_GAP;
        }
        if pacer.due(now, fresh).is_some() {
            pacer.sent(now);
            (sent, fresh) = (sent + 1, false);
        }
        now += look;
    }
    assert!(sent >= 599, "{sent} of 600 frames sent");
    // With a frame always waiting, the sends still average at most one per gap.
    let mut pacer = Pacer::default();
    let (mut sends, mut now) = (0_u32, Duration::ZERO);
    while now < Duration::from_secs(1) {
        if pacer.due(now, true).is_some() {
            pacer.sent(now);
            sends += 1;
        }
        now += Duration::from_micros(500);
    }
    assert!(sends <= 61, "{sends} sends in one second with a frame always waiting");
}

#[test]
fn an_owned_default_moves_to_add_and_the_rest_stay_in_set() {
    let mut map = ChannelMap::builtin().unwrap();
    map.enable_default("MouthOpen", true).unwrap();
    map.enable_default("MouthSmile", true).unwrap();
    let injector = Injector::new(&map);
    let params = injector.params();
    let mut batches = Batches::default();
    let snap = Snapshot::default();
    let one = batches.requests(params, &snap);
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].mode, InjectMode::Set);
    assert_eq!(one[0].parameter_values.len(), 84);
    assert!(batches.owned(params, "MouthOpen is controlled by another plugin"));
    assert!(!batches.owned(params, "MouthOpen is controlled by another plugin"), "already moved");
    let two = batches.requests(params, &snap);
    assert_eq!((two[0].mode, two[0].parameter_values.len()), (InjectMode::Set, 83));
    assert_eq!((two[1].mode, two[1].parameter_values[0].id.as_str()), (InjectMode::Add, "MouthOpen"));
    assert_eq!(batches.conflicts(params), ["MouthOpen"]);
    assert!(!mentions("MouthOpenness is owned", "MouthOpen"), "whole words only");
}

#[test]
fn a_concurrent_reader_never_sees_a_torn_frame() {
    let map = ChannelMap::builtin().unwrap();
    let injector = std::sync::Arc::new(Injector::new(&map));
    let writer = {
        let injector = std::sync::Arc::clone(&injector);
        std::thread::spawn(move || {
            for i in 0..20_000u16 {
                let v = f32::from(i % 2);
                let buf = frame(&CHANNELS[..52].iter().map(|c| (c.channel, v)).collect::<Vec<_>>());
                injector.push(&buf, i % 2 == 1);
            }
        })
    };
    let mut snap = Snapshot::default();
    while !writer.is_finished() {
        if injector.read(&mut snap) {
            let first = snap.values[0];
            assert!(snap.values[..52].iter().all(|&v| v == first), "a frame is whole");
            assert_eq!(snap.face_found, first == 1.0);
        }
    }
    writer.join().unwrap();
}
