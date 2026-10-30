//! An actor's keys played: the pose keys blended with their eases into a body pose, the face keys into channels, the
//! talk spans into syllables, all handed to the live performer (life, anime grammar, animator) stepped at its fixed
//! 60 Hz on the sequence's clock, so blinks, breathing and settle come on top of the keys as they do live.
//!
//! The same actor and seed give the same frames on every run: every value is a function of the time and the seed.

use omuzaki_anim::BusAnimator;
use omuzaki_anim::grammar::{GrammarConfig, GrammarStage};
use omuzaki_anim::hands::{HandLibrary, HandRest};
use omuzaki_anim::life::Life;
use omuzaki_anim::poses::{Hand, PoseLibrary, Posture};
use omuzaki_anim::retarget::ArkitRetarget;
use omuzaki_anim_api::{
    BlendMode, ChannelBuf, ChannelMask, DeviceProfile, HumanoidBone, HumanoidPose, Layer, LifeConfig, Tick,
};
use omuzaki_core::glam::{EulerRot, Mat4, Quat, Vec3};
use omuzaki_core::time::Seconds;
use omuzaki_face_api::{ExpressionId, ExpressionSet};
use omuzaki_stage::frame::FrameInput;
use omuzaki_stage::perform::input::NoSource;
use omuzaki_stage::perform::{Performer, PerformerParts, STEP_HZ};

use crate::curve::Ease;
use crate::def::Actor;
use crate::error::SequenceError;

/// The pose a key without one starts from.
pub const DEFAULT_POSE: &str = "stand-relaxed";
/// Syllables a second while talking: an anchor's brisk read.
pub const SYLLABLES_PER_S: f32 = 6.5;
/// The visemes talking cycles through.
const VISEMES: [&str; 5] = ["aa", "oh", "ee", "ih", "ou"];
/// Seconds the mouth takes to open into a span and close out of it.
const TALK_FADE_S: f32 = 0.06;

/// A pose key resolved: its posture and its ease in.
#[derive(Clone, Debug)]
struct PoseAt {
    t: f32,
    posture: Posture,
    ease: Ease,
}

/// A face key resolved: its channels and its ease in.
#[derive(Clone, Debug)]
struct FaceAt {
    t: f32,
    values: ChannelBuf,
    ease: Ease,
}

/// An actor's keys, resolved against its body, ready to play.
pub struct ActorPlayer {
    poses: Vec<PoseAt>,
    faces: Vec<FaceAt>,
    talk: Vec<[f32; 2]>,
    gaze: Option<crate::curve::Track<[f32; 2]>>,
    position: crate::curve::Track<[f32; 3]>,
    yaw: crate::curve::Track<f32>,
    expressions: ExpressionSet,
    visemes: Vec<ExpressionId>,
    rest: ChannelBuf,
    mask: ChannelMask,
    hand_rest: HandRest,
    performer: Option<Performer>,
    next_step: u64,
    seed: u32,
    input: FrameInput,
}

impl std::fmt::Debug for ActorPlayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActorPlayer").field("poses", &self.poses.len()).finish_non_exhaustive()
    }
}

fn invalid(m: impl Into<String>) -> SequenceError {
    SequenceError::Invalid(m.into())
}

/// The posture of `key` at `at`: its library pose (mirrored if asked), its bones set outright, its hands.
fn posture_of(key: &crate::def::PoseKey, at: &str) -> Result<Posture, SequenceError> {
    let library = PoseLibrary::builtin().map_err(|e| SequenceError::Render(format!("the pose library: {e}")))?;
    let hands = HandLibrary::builtin().map_err(|e| SequenceError::Render(format!("the hand library: {e}")))?;
    let id = key.pose.as_deref().unwrap_or(DEFAULT_POSE);
    let def = library.get(id).ok_or_else(|| {
        let known: Vec<&str> = library.iter().map(|p| p.id.as_str()).collect();
        invalid(format!("{at}.pose: there is no pose '{id}'; the library has {}", known.join(", ")))
    })?;
    let mut posture = def.posture(hands).map_err(|e| invalid(format!("{at}.pose: '{id}': {e}")))?;
    if key.mirror {
        posture = posture.mirrored();
    }
    for (name, [x, y, z]) in &key.bones {
        let bone = HumanoidBone::from_name(name).ok_or_else(|| invalid(format!("{at}.bones: no bone '{name}'")))?;
        posture.body.set(bone, Quat::from_euler(EulerRot::XYZ, x.to_radians(), y.to_radians(), z.to_radians()));
    }
    for (side, shape) in &key.hands {
        let hand =
            hands.get(shape).ok_or_else(|| invalid(format!("{at}.hands.{side}: there is no hand shape '{shape}'")))?;
        match side.as_str() {
            "left" => posture.left = Hand::of(hand, 1.0),
            _ => posture.right = Hand::of(hand, 1.0),
        }
    }
    Ok(posture)
}

/// Where `t` falls among keys at `times`: the key before, the key after and the share of the way between them, or the
/// one key it is on or past.
fn segment(times: impl ExactSizeIterator<Item = f32>, t: f32) -> Option<(usize, usize, f32)> {
    let times: Vec<f32> = times.collect();
    let (first, last) = (*times.first()?, *times.last()?);
    if t <= first {
        return Some((0, 0, 0.0));
    }
    if t >= last {
        let i = times.len() - 1;
        return Some((i, i, 0.0));
    }
    let next = times.partition_point(|k| *k <= t);
    let (a, b) = (times[next - 1], times[next]);
    let u = if b > a { (t - a) / (b - a) } else { 1.0 };
    Some((next - 1, next, u))
}

/// A hash of `n` with `seed` in `0..1`.
fn unit(seed: u32, n: u32) -> f32 {
    let mut h = seed.wrapping_mul(0x9e37_79b9) ^ n.wrapping_mul(0x85eb_ca6b);
    h ^= h >> 16;
    h = h.wrapping_mul(0x7feb_352d);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846c_a68b);
    h ^= h >> 16;
    #[allow(clippy::cast_precision_loss, reason = "24 bits are exact")]
    let u = (h >> 8) as f32 / 16_777_216.0;
    u
}

impl ActorPlayer {
    /// The player of `actor`, its faces made from `expressions` (the body's own, the library filling what it lacks)
    /// over the resting face `rest`, its hands closed on a rig whose thumbs rest as `hand_rest`, its life seeded by
    /// `seed`. `life` off plays the keys alone.
    ///
    /// # Errors
    /// [`SequenceError::Invalid`] for a pose, hand shape or expression nobody has.
    pub fn new(
        actor: &Actor,
        at: &str,
        expressions: &ExpressionSet,
        rest: ChannelBuf,
        hand_rest: HandRest,
        seed: u32,
    ) -> Result<Self, SequenceError> {
        let mut set = ExpressionSet::library().map_err(|e| SequenceError::Render(format!("the expressions: {e}")))?;
        set.0.extend(expressions.0.iter().map(|(k, v)| (k.clone(), v.clone())));
        let mut keys = actor.pose.clone();
        if keys.is_empty() {
            keys.push(crate::def::PoseKey {
                t: 0.0,
                pose: None,
                mirror: false,
                bones: std::collections::BTreeMap::new(),
                hands: std::collections::BTreeMap::new(),
                ease: Ease::default(),
            });
        }
        let poses = keys
            .iter()
            .enumerate()
            .map(|(i, key)| {
                Ok(PoseAt { t: key.t, posture: posture_of(key, &format!("{at}.pose[{i}]"))?, ease: key.ease })
            })
            .collect::<Result<Vec<_>, SequenceError>>()?;
        let mut mask = ChannelMask::EMPTY;
        let moves = |id: &ExpressionId, mask: &mut ChannelMask| {
            if let Some(e) = set.0.get(id) {
                for channel in e.deltas.0.keys() {
                    mask.insert(*channel);
                }
            }
        };
        let mut faces = Vec::with_capacity(actor.face.len());
        for (i, key) in actor.face.iter().enumerate() {
            let ids = key
                .set
                .iter()
                .map(|(name, w)| {
                    let id = ExpressionId::new(name).map_err(|e| invalid(format!("{at}.face[{i}]: '{name}': {e}")))?;
                    if !set.0.contains_key(&id) {
                        let known: Vec<String> = set.0.keys().map(ToString::to_string).collect();
                        return Err(invalid(format!(
                            "{at}.face[{i}]: there is no expression '{name}'; there are {}",
                            known.join(", ")
                        )));
                    }
                    Ok((id, *w))
                })
                .collect::<Result<Vec<_>, SequenceError>>()?;
            for (id, _) in &ids {
                moves(id, &mut mask);
            }
            let weights: Vec<(&ExpressionId, f32)> = ids.iter().map(|(id, w)| (id, *w)).collect();
            let values = set.blend(&rest, &weights).map_err(|e| invalid(format!("{at}.face[{i}]: {}", e.0)))?;
            faces.push(FaceAt { t: key.t, values, ease: key.ease });
        }
        let visemes = VISEMES
            .iter()
            .filter_map(|name| ExpressionId::new(*name).ok())
            .filter(|id| set.0.contains_key(id))
            .collect::<Vec<_>>();
        if !actor.talk.is_empty() {
            for id in &visemes {
                moves(id, &mut mask);
            }
        }
        if actor.gaze.is_some() {
            mask |= ChannelMask::GAZE;
        }
        let performer = actor.life.then(|| {
            let parts = PerformerParts {
                source: Box::new(NoSource),
                retarget: Box::new(ArkitRetarget::new(DeviceProfile::Webcam)),
                life: Box::new(Life::new(LifeConfig::new(u64::from(seed)))),
                animator: Box::new(BusAnimator::default()),
            };
            let performer = Performer::new(parts, rest, HumanoidPose::REST);
            match GrammarStage::new(&GrammarConfig::default()) {
                Ok(grammar) => performer.with_grammar(grammar),
                Err(_) => performer,
            }
        });
        Ok(Self {
            poses,
            faces,
            talk: actor.talk.clone(),
            gaze: actor.gaze.clone(),
            position: actor.position.clone(),
            yaw: actor.yaw.clone(),
            expressions: set,
            visemes,
            rest,
            mask,
            hand_rest,
            performer,
            next_step: 0,
            seed,
            input: FrameInput::default(),
        })
    }

    /// The keyed body pose at `t`, before life.
    #[must_use]
    pub fn pose_at(&self, t: f32) -> HumanoidPose {
        let Some((a, b, u)) = segment(self.poses.iter().map(|k| k.t), t) else {
            return HumanoidPose::REST;
        };
        let posture = if a == b {
            self.poses[a].posture
        } else {
            Posture::blend(&self.poses[a].posture, &self.poses[b].posture, self.poses[b].ease.apply(u))
        };
        posture.to_humanoid(&self.hand_rest)
    }

    /// How open the talking mouth is at `t` (0 to 1) and which viseme it shapes.
    #[must_use]
    pub fn mouth_at(&self, t: f32) -> Option<(usize, f32)> {
        let span = self.talk.iter().find(|[a, b]| *a <= t && t < *b)?;
        if self.visemes.is_empty() {
            return None;
        }
        let local = t - span[0];
        let fade = (local / TALK_FADE_S).min((span[1] - t) / TALK_FADE_S).clamp(0.0, 1.0);
        let phase = local * SYLLABLES_PER_S;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a syllable count")]
        let syllable = phase.floor() as u32;
        let within = phase.fract();
        // Each syllable opens and closes; some open wider than others, and a few barely move (consonants).
        let size = 0.45 + 0.55 * unit(self.seed ^ 0x5eed, syllable);
        let open = (std::f32::consts::PI * within).sin().powf(0.8) * size * fade;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss,
            reason = "an index"
        )]
        let viseme = (unit(self.seed, syllable) * self.visemes.len() as f32) as usize % self.visemes.len();
        Some((viseme, open))
    }

    /// The keyed face at `t`, before life: the face keys blended, the mouth's syllable and the gaze on top.
    #[must_use]
    pub fn face_at(&self, t: f32) -> ChannelBuf {
        let mut values = match segment(self.faces.iter().map(|k| k.t), t) {
            None => self.rest,
            Some((a, b, _)) if a == b => self.faces[a].values,
            Some((a, b, u)) => {
                let mut v = self.faces[a].values;
                v.lerp_all(&self.faces[b].values, self.faces[b].ease.apply(u));
                v
            }
        };
        if let Some((viseme, open)) = self.mouth_at(t)
            && let Ok(talking) = self.expressions.blend(&values, &[(&self.visemes[viseme], open)])
        {
            values = talking;
        }
        if let Some(gaze) = &self.gaze {
            let [x, y] = gaze.at(t);
            values.set_gaze(x.clamp(-1.0, 1.0), y.clamp(-1.0, 1.0));
        }
        values
    }

    /// Where the actor stands at `t`: its position and yaw as a root.
    #[must_use]
    pub fn root_at(&self, t: f32) -> Mat4 {
        Mat4::from_rotation_translation(
            Quat::from_rotation_y(self.yaw.at(t).to_radians()),
            Vec3::from(self.position.at(t)),
        )
    }

    /// The frame input at `t` seconds of the shot, with the pose held at `pose_t` (the frame the character's motion
    /// last moved on: the same as `t` on ones, earlier on twos). Steps the performer to `t`; times must not go back.
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn advance(&mut self, t: f32, pose_t: f32) -> &FrameInput {
        let base = self.pose_at(pose_t);
        let face = self.face_at(t);
        self.input.root = self.root_at(pose_t);
        let Some(performer) = self.performer.as_mut() else {
            self.input.pose = base;
            self.input.channels = face;
            return &self.input;
        };
        let due = (f64::from(t) * f64::from(STEP_HZ)).floor().max(0.0) as u64;
        while self.next_step <= due {
            let tick = Tick::at_rate(self.next_step, STEP_HZ);
            let (layer, _, _) = performer.score_mut();
            *layer = Layer { values: face, mask: self.mask, weight: 1.0, mode: BlendMode::Override };
            performer.set_rest(self.rest, base);
            let out = performer.step(Tick { time: Seconds(tick.time.0), ..tick });
            self.input.pose = *out.pose;
            self.input.channels = out.bus.values;
            self.next_step += 1;
        }
        &self.input
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curve::{Curve, Track};
    use crate::def::{FaceKey, PoseKey};

    fn actor() -> Actor {
        let mut bones = std::collections::BTreeMap::new();
        bones.insert("head".to_owned(), [0.0, 20.0, 0.0]);
        Actor {
            id: "a".into(),
            character: omuzaki_char_api::CharId::new("kit").unwrap(),
            position: Track::Hold([0.0; 3]),
            yaw: Track::Hold(0.0),
            pose: vec![
                PoseKey {
                    t: 0.0,
                    pose: None,
                    mirror: false,
                    bones: std::collections::BTreeMap::new(),
                    hands: std::collections::BTreeMap::new(),
                    ease: Ease::default(),
                },
                PoseKey {
                    t: 1.0,
                    pose: Some("point".into()),
                    mirror: false,
                    bones,
                    hands: std::collections::BTreeMap::new(),
                    ease: Ease::Named(Curve::Linear),
                },
            ],
            face: vec![
                FaceKey { t: 0.0, set: std::collections::BTreeMap::new(), ease: Ease::default() },
                FaceKey { t: 1.0, set: [("happy".to_owned(), 1.0)].into(), ease: Ease::Named(Curve::Linear) },
            ],
            gaze: None,
            talk: vec![[0.0, 2.0]],
            life: false,
            surface: None,
        }
    }

    fn player() -> ActorPlayer {
        ActorPlayer::new(&actor(), "actor", &ExpressionSet::default(), ChannelBuf::NEUTRAL, HandRest::nominal(), 1)
            .unwrap()
    }

    #[test]
    fn poses_blend_key_to_key_and_bones_override() {
        let p = player();
        let head = HumanoidBone::Head;
        let at_end = p.pose_at(1.0).get(head);
        let want = Quat::from_euler(EulerRot::XYZ, 0.0, 20f32.to_radians(), 0.0);
        assert!(at_end.dot(want).abs() > 1.0 - 1e-6, "the key's bone is set outright: {at_end:?} vs {want:?}");
        let half = p.pose_at(0.5).get(head);
        assert!((half.angle_between(Quat::IDENTITY) - 10f32.to_radians()).abs() < 0.02, "halfway is halfway");
        assert_eq!(p.pose_at(5.0).get(head), at_end, "held after the last key");
    }

    #[test]
    fn the_mouth_opens_and_closes_through_a_span() {
        let p = player();
        assert!(p.mouth_at(2.5).is_none(), "silent outside the spans");
        let opens: Vec<f32> = (0..40).map(|i| p.mouth_at(i as f32 * 0.025).map_or(0.0, |m| m.1)).collect();
        assert!(opens.iter().any(|o| *o > 0.3), "it opens");
        let low = opens.iter().filter(|o| **o < 0.15).count();
        assert!(low >= 3, "and nearly closes between syllables: {opens:?}");
    }

    #[test]
    fn faces_blend_and_unknown_expressions_are_refused() {
        let p = player();
        assert_ne!(p.face_at(1.0).as_slice(), ChannelBuf::NEUTRAL.as_slice());
        let mut bad = actor();
        bad.face[1].set = [("no-such-face".to_owned(), 1.0)].into();
        let err =
            ActorPlayer::new(&bad, "actor", &ExpressionSet::default(), ChannelBuf::NEUTRAL, HandRest::nominal(), 1)
                .unwrap_err();
        assert!(err.to_string().contains("no-such-face"));
    }

    #[test]
    fn life_runs_deterministically() {
        let mut with_life = actor();
        with_life.life = true;
        let run = || {
            let mut p = ActorPlayer::new(
                &with_life,
                "a",
                &ExpressionSet::default(),
                ChannelBuf::NEUTRAL,
                HandRest::nominal(),
                9,
            )
            .unwrap();
            (0..48).map(|i| p.advance(i as f32 / 24.0, i as f32 / 24.0).channels.checksum()).collect::<Vec<_>>()
        };
        assert_eq!(run(), run());
    }
}
