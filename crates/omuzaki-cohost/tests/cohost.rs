//! The co-host on a recording perform host: the turn loop's emotes become held expression triggers at their words, a
//! barge-in lets go of them, and the AI's voice reaches the host's lip sync.
// Test helpers may unwrap: a panic there is a clear test failure.
#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use omuzaki_anim_api::{Calibration, DeviceProfile, RetargetConfig};
use omuzaki_cohost::stage::{PerformStage, key_of};
use omuzaki_cohost::{Cohost, attach_voice};
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_face_api::{ExpressionId, ExpressionSet};
use omuzaki_llm_api::LlmClient;
use omuzaki_llm_api::stream::{LlmEvent, StopReason};
use omuzaki_llm_mock::{FakeLlm, Script, ScriptTurn};
use omuzaki_performer::brain::LlmBrain;
use omuzaki_performer::persona::Persona;
use omuzaki_performer::show::{Parts, Show, ShowConfig};
use omuzaki_performer_api::ears::Heard;
use omuzaki_performer_api::event::PerformerEvent;
use omuzaki_performer_api::ids::VoiceId;
use omuzaki_performer_api::intent::{GazeTarget, Intent};
use omuzaki_performer_api::stage::Stage;
use omuzaki_performer_api::voice::VoiceLead;
use omuzaki_performer_mock::{FakeVoice, ScriptedEars};
use omuzaki_stage::perform::Tracking;
use omuzaki_stage::perform::host::{
    CalibrationState, PerformError, PerformHost, PerformStatus, RetargetChange, RetargetReceipt,
};
use omuzaki_stage::perform::triggers::TriggerBinding;
use omuzaki_stage_api::ids::MemberId;
use omuzaki_track_voice::{LipSyncConfig, VoiceReader};

/// Records binds, presses and the voice.
#[derive(Default)]
struct Host {
    calls: Mutex<Vec<String>>,
    voice: Mutex<Option<VoiceReader>>,
}

impl Host {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl PerformHost for Host {
    fn calibrate(&self, _: usize) -> Result<(), PerformError> {
        Ok(())
    }

    fn set_retarget(&self, _: RetargetChange) -> Result<RetargetReceipt, PerformError> {
        Err(PerformError::Unsupported)
    }

    fn set_calibration(&self, _: Calibration) -> Result<(), PerformError> {
        Ok(())
    }

    fn calibration(&self) -> Calibration {
        Calibration::identity(DeviceProfile::Webcam)
    }

    fn status(&self) -> PerformStatus {
        PerformStatus {
            tracking: Tracking::Tracking,
            calibration: CalibrationState::Capturing { frames: 0, of: 1 },
            retarget: RetargetConfig::default(),
            retarget_seq: 0,
            source_fps: 30.0,
        }
    }

    fn set_voice(&self, voice: Option<VoiceReader>, _: LipSyncConfig) -> Result<(), PerformError> {
        *self.voice.lock().unwrap() = voice;
        Ok(())
    }

    fn bind_trigger_checked(
        &self,
        binding: TriggerBinding,
    ) -> Result<omuzaki_stage::perform::triggers::BindOutcome, PerformError> {
        if !self.expressions().0.contains_key(&binding.expression) {
            return Err(PerformError::UnknownExpression);
        }
        self.calls.lock().unwrap().push(format!("bind {} {}", binding.key, binding.expression));
        Ok(omuzaki_stage::perform::triggers::BindOutcome { slot: 0, replaced: false })
    }

    fn unbind_trigger(&self, key: &str) -> Result<bool, PerformError> {
        self.calls.lock().unwrap().push(format!("unbind {key}"));
        Ok(true)
    }

    fn press(&self, key: &str, down: bool) -> Result<bool, PerformError> {
        self.calls.lock().unwrap().push(format!("{} {key}", if down { "down" } else { "up" }));
        Ok(true)
    }

    fn expressions(&self) -> ExpressionSet {
        ExpressionSet::starter()
    }
}

fn secs(ms: u64) -> Seconds {
    #[allow(clippy::cast_precision_loss)]
    Seconds(ms as f64 / 1000.0)
}

fn member() -> MemberId {
    MemberId::new("mochi").unwrap()
}

fn reply(text: &str) -> ScriptTurn {
    ScriptTurn::new(vec![
        LlmEvent::start("fake"),
        LlmEvent::TextDelta(text.into()),
        LlmEvent::Stop(StopReason::EndTurn),
    ])
}

struct Rig {
    clock: Arc<FakeClock>,
    host: Arc<Host>,
    cohost: Cohost,
    lines: Vec<(u64, PerformerEvent)>,
    /// `(ms, call)` as the host saw them.
    timeline: Vec<(u64, String)>,
}

impl Rig {
    fn new(ears: Vec<(Seconds, Heard)>, script: Script) -> Self {
        let clock = Arc::new(FakeClock::default());
        let host = Arc::new(Host::default());
        let stage = Arc::new(PerformStage::new(Arc::clone(&host) as Arc<dyn PerformHost>, member()));
        let brain = LlmBrain::new(Arc::new(FakeLlm::new(script)) as Arc<dyn LlmClient>, Persona::named("Mochi"));
        let parts = Parts::new(
            Box::new(brain),
            Box::new(ScriptedEars::new(Arc::clone(&clock), ears)),
            Box::new(FakeVoice::new(Arc::clone(&clock), Seconds(0.2))),
            Arc::clone(&stage) as Arc<dyn Stage>,
        );
        let config = ShowConfig::new(member(), VoiceId::new("kokoro-82m/af_heart").unwrap()).reactions(false);
        let show = Show::new(&Persona::named("Mochi"), parts, config);
        let voice = attach_voice(host.as_ref(), LipSyncConfig::default()).unwrap();
        Self { clock, host, cohost: Cohost::new(show, stage, Some(voice)), lines: Vec::new(), timeline: Vec::new() }
    }

    fn run_to(&mut self, end: u64) {
        let start = self.timeline.last().map_or(0, |(t, _)| *t);
        for now in (start..=end).step_by(5) {
            self.clock.set(secs(now));
            let before = self.host.calls().len();
            self.cohost.step(secs(now));
            for call in &self.host.calls()[before..] {
                self.timeline.push((now, call.clone()));
            }
            self.lines.extend(self.cohost.show().drain().into_iter().map(|l| (l.t, l.event)));
        }
    }
}

#[test]
fn the_inventory_is_the_characters_moods() {
    let host = Arc::new(Host::default());
    let stage = PerformStage::new(host, member());
    let inventory = stage.inventory(&member()).unwrap();
    let names: Vec<String> = inventory.expressions.iter().map(ToString::to_string).collect();
    assert_eq!(names, ["angry", "happy", "relaxed", "sad", "surprised"]);
    assert_eq!(inventory.clips, []);
    assert!(stage.inventory(&MemberId::new("other").unwrap()).is_err());
}

#[test]
fn an_emote_holds_its_trigger_at_its_word_and_lets_go_after_its_hold() {
    let ears = vec![
        (secs(0), Heard::onset(secs(0))),
        (secs(0), Heard::final_text(0, "Mochi, hello", secs(0), secs(0))),
    ];
    let mut rig =
        Rig::new(ears, Script::new("fake").with_turn(reply("Hi chat! [emote happy 0.8 hold 1s] Welcome in!")));
    rig.run_to(3_000);
    // "Hi chat!" plays 0.2..0.68 s on the fake voice; the emote sits on the next word.
    assert_eq!(
        rig.timeline,
        [(680, "bind ai:happy happy".to_owned()), (680, "down ai:happy".to_owned()), (1_680, "up ai:happy".to_owned())]
    );
    assert!(rig.lines.iter().any(|(t, e)| *t == 680 && matches!(e, PerformerEvent::Intent { .. })));
    // The voice is wired to the host's lip sync.
    assert!(rig.host.voice.lock().unwrap().is_some());
}

#[test]
fn a_barge_in_lets_go_of_the_face_and_stop_frees_the_keys() {
    let mut ears = Vec::new();
    ears.push((secs(0), Heard::final_text(0, "Mochi, tell me", secs(0), secs(0))));
    ears.extend([(secs(1_000), Heard::onset(secs(1_000))), (secs(1_100), Heard::held(secs(1_100)))]);
    let script =
        Script::new("fake").with_turn(reply("[emote sad hold 8s] It was a long, long winter for the little crane."));
    let mut rig = Rig::new(ears, script);
    rig.run_to(1_200);
    let calls: Vec<&str> = rig.timeline.iter().map(|(_, c)| c.as_str()).collect();
    assert_eq!(calls, ["bind ai:sad sad", "down ai:sad", "up ai:sad"]);
    assert_eq!(rig.timeline[2].0, 1_100);
    rig.cohost.stop(secs(1_200));
    assert!(rig.host.calls().contains(&"unbind ai:sad".to_owned()));
}

#[test]
fn what_the_stage_cannot_do_is_filtered_not_faked() {
    let mut rig = Rig::new(Vec::new(), Script::new("fake"));
    rig.cohost.show().intent(secs(0), &Intent::look_at(GazeTarget::Camera, 1.0));
    rig.cohost.show().intent(secs(0), &Intent::emote(ExpressionId::new("smug").unwrap(), 1.0, 1.0));
    rig.run_to(10);
    let why: Vec<String> = rig
        .lines
        .iter()
        .filter_map(|(_, e)| match e {
            PerformerEvent::Filtered { why, .. } => Some(why.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(why, ["not supported by this stage", "unknown expression"]);
    assert_eq!(key_of(&ExpressionId::new("happy").unwrap()), "ai:happy");
}

#[test]
fn the_ai_voice_reaches_the_lip_sync_as_it_plays() {
    let host = Host::default();
    let lead = attach_voice(&host, LipSyncConfig::default()).unwrap();
    let mut pump = lead.clone();
    // A 300 ms vowel-like buzz predicted to play from 1 s.
    let pcm: Vec<f32> = (0..14_400)
        .map(|i| {
            #[allow(clippy::cast_precision_loss)]
            let t = i as f32 / 48_000.0;
            0.4 * (2.0 * std::f32::consts::PI * 150.0 * t).sin()
                * (1.0 + (2.0 * std::f32::consts::PI * 800.0 * t).sin())
        })
        .collect();
    pump.push(Seconds(1.0), 0, 48_000, &pcm, false);
    let mut reader = host.voice.lock().unwrap().take().unwrap();
    lead.poll(Seconds(0.5));
    assert!(reader.poll().is_none());
    lead.poll(Seconds(1.2));
    assert!(reader.poll().is_some_and(|f| f.loudness > 0.0));
}
