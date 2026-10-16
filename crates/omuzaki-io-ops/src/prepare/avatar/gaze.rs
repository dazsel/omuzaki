//! The one gaze route, chosen at prepare time (design D3), and the look-at it reads.
//!
//! The eight `eyeLook*` channels reach the model through exactly one route, first match wins:
//! 1. **Bone**: the file's look-at is bone type (or absent) and both eye bones exist;
//! 2. **Native `ARKit`**: the model has its own `eyeLook*` expressions (synthesis outcome `Present` or `FromMorph`,
//!    never `FromPreset`);
//! 3. **Presets**: the model has look presets;
//! 4. **Inert**: none of these (`arkit.gaze_inert`).
//!
//! A direction the chosen route cannot show is reported once (`arkit.gaze_partial`, naming the directions), read
//! from [`ImportedAvatar::driven`], the same mask liveliness measures read, so the report and the measures agree.

use omuzaki_anim_api::{Channel, HumanoidBone, Region};
use omuzaki_avatar_api::avatar::ImportedAvatar;
use omuzaki_avatar_api::gaze::{AvatarGaze, AvatarLookAt, LookDirection};
use omuzaki_io_api::{ExpressionName, Finding, IoScene, LookAtKind, Report};

use crate::prepare::arkit::{Arkit52Report, Synthesis};

/// The route `avatar` can show gaze through, given its expressions (already converted), its skeleton, the file's
/// look-at and what synthesis built. Sets `avatar.gaze` and `avatar.look_at`.
pub fn choose(scene: &IoScene, arkit: &Arkit52Report, avatar: &mut ImportedAvatar) {
    let look_at = scene.avatar.as_ref().and_then(|a| a.look_at);
    avatar.look_at = look_at.map(AvatarLookAt::from);
    let bone_type = avatar.look_at.is_none_or(|l| l.kind == LookAtKind::Bone);
    let eyes =
        [HumanoidBone::LeftEye, HumanoidBone::RightEye].iter().all(|b| avatar.skeleton.humanoid_bone(*b).is_some());
    let native = arkit.channels.iter().any(|c| {
        Channel::from_name(c.channel).is_some_and(|ch| ch.region() == Region::Gaze)
            && matches!(c.how, Synthesis::Present | Synthesis::FromMorph { .. })
    });
    let presets = LookDirection::ALL.iter().any(|d| avatar.expression(&ExpressionName::Preset(d.preset())).is_some());
    avatar.gaze = if bone_type && eyes {
        AvatarGaze::Bone
    } else if native {
        AvatarGaze::NativeArkit
    } else if presets {
        AvatarGaze::Presets
    } else {
        AvatarGaze::Inert
    };
}

/// The gaze findings, once the drivers are set: `arkit.gaze_inert` on the inert route, `arkit.gaze_partial` naming
/// the directions another route cannot show.
pub fn findings(avatar: &ImportedAvatar, report: &mut Report) {
    if avatar.gaze == AvatarGaze::Inert {
        let msg = "this model has no eye bones, gaze shapes or look expressions, so its eyes won't follow your gaze";
        let hint = "add eye bones or look expressions to the model, or map gaze expressions to the eyeLook channels";
        report.push(Finding::info("arkit.gaze_inert", msg).at("arkit/gaze").hint(hint));
        return;
    }
    let driven = avatar.driven();
    let missing: Vec<&str> = [
        (LookDirection::Up, "up"),
        (LookDirection::Down, "down"),
        (LookDirection::Left, "left"),
        (LookDirection::Right, "right"),
    ]
    .into_iter()
    .filter(|(d, _)| d.channels().iter().any(|c| !driven.contains(*c)))
    .map(|(_, name)| name)
    .collect();
    if !missing.is_empty() {
        let msg = format!("this model's eyes can't look {}", missing.join(" or "));
        let hint = "add the missing look expressions or gaze shapes to the model";
        report.push(Finding::info("arkit.gaze_partial", msg).at("arkit/gaze").hint(hint));
    }
}
