//! Acclaim motion capture (`.asf` skeleton, `.amc` motion), the format of the CMU motion-capture library, read and
//! retargeted onto the VRM humanoid.
//!
//! **Rotations.** Each ASF bone has a rest frame `C` (its `axis`, Euler degrees about fixed X, Y then Z) and turns by
//! its `dof` angles about that frame's axes, so its world rotation from the rest is `M = M_parent · C · R(dof) · C⁻¹`
//! (`R` the same fixed-axis Euler order), with the root's from its own six channels. A CMU skeleton rests in a T-pose
//! with `+Y` up and its left along `+X`, the VRM humanoid's rest, so a VRM bone's local rotation is
//! `M_parent⁻¹ · M` over the ASF bones it stands for.
//!
//! **Facing.** The take is turned about `+Y` so the first frame faces `+Z`, and its hips travel is measured from the
//! first frame, in metres (ASF lengths are `1 / length-unit` inches), scaled to the hips height asked for.
//!
//! Fingers are not in the CMU skeleton's motion: they stay at rest, for the hand shapes to pose.

use std::collections::HashMap;

use omuzaki_anim_api::{HumanoidBone as B, HumanoidPose};
use omuzaki_core::glam::{EulerRot, Quat, Vec3};

use super::{HumanoidTake, MotionError};

/// Metres in an inch.
const INCH_M: f32 = 0.0254;

/// Which ASF bone each VRM bone takes its world rotation from (the deepest of a run, so a bone the humanoid lacks
/// folds into the next one up: `upperneck` into the neck, `lwrist` into the hand).
const MAP: [(B, &str); 22] = [
    (B::Hips, "root"),
    (B::Spine, "lowerback"),
    (B::Chest, "upperback"),
    (B::UpperChest, "thorax"),
    (B::Neck, "upperneck"),
    (B::Head, "head"),
    (B::LeftShoulder, "lclavicle"),
    (B::LeftUpperArm, "lhumerus"),
    (B::LeftLowerArm, "lradius"),
    (B::LeftHand, "lhand"),
    (B::RightShoulder, "rclavicle"),
    (B::RightUpperArm, "rhumerus"),
    (B::RightLowerArm, "rradius"),
    (B::RightHand, "rhand"),
    (B::LeftUpperLeg, "lfemur"),
    (B::LeftLowerLeg, "ltibia"),
    (B::LeftFoot, "lfoot"),
    (B::RightUpperLeg, "rfemur"),
    (B::RightLowerLeg, "rtibia"),
    (B::RightFoot, "rfoot"),
    (B::LeftToes, "ltoes"),
    (B::RightToes, "rtoes"),
];

/// Where a humanoid bone points in the VRM T-pose (from its joint to its child's), for the bones whose ASF rest
/// direction is compared with it; `None` leaves the bone's rotation as the ASF bone's (the hips, the feet).
fn rest_direction(bone: B) -> Option<Vec3> {
    Some(match bone {
        B::Spine | B::Chest | B::UpperChest | B::Neck | B::Head => Vec3::Y,
        B::LeftShoulder | B::LeftUpperArm | B::LeftLowerArm | B::LeftHand => Vec3::X,
        B::RightShoulder | B::RightUpperArm | B::RightLowerArm | B::RightHand => Vec3::NEG_X,
        B::LeftUpperLeg | B::LeftLowerLeg | B::RightUpperLeg | B::RightLowerLeg => Vec3::NEG_Y,
        _ => return None,
    })
}

/// One ASF bone.
#[derive(Clone, Debug)]
struct Bone {
    name: String,
    /// Rest direction, unit, world.
    direction: Vec3,
    /// Length in file units.
    length: f32,
    /// Rest frame.
    axis: Quat,
    /// Which of rx, ry, rz the motion gives, in the order it gives them.
    dof: Vec<usize>,
    parent: Option<usize>,
}

/// An ASF skeleton.
#[derive(Clone, Debug)]
pub struct Skeleton {
    /// File units in an inch (`:units length`).
    length_unit: f32,
    /// Index 0 is the root.
    bones: Vec<Bone>,
    /// The root's channel order: indices into `[tx, ty, tz, rx, ry, rz]`.
    root_order: Vec<usize>,
    by_name: HashMap<String, usize>,
}

/// Fixed-axis X then Y then Z, in degrees: `Rz · Ry · Rx`.
fn euler_xyz(deg: [f32; 3]) -> Quat {
    let [x, y, z] = deg.map(f32::to_radians);
    Quat::from_euler(EulerRot::ZYX, z, y, x)
}

fn parse_err(file: &'static str, line: usize, message: impl Into<String>) -> MotionError {
    MotionError::Parse { file, line, message: message.into() }
}

fn numbers(file: &'static str, line: usize, words: &[&str]) -> Result<Vec<f32>, MotionError> {
    words
        .iter()
        .map(|w| w.parse::<f32>().ok().filter(|v| v.is_finite()).ok_or_else(|| parse_err(file, line, format!("{w:?} is not a number"))))
        .collect()
}

impl Skeleton {
    /// Parses an `.asf` file.
    ///
    /// # Errors
    /// [`MotionError::Parse`] for text that is not an ASF skeleton.
    #[allow(clippy::too_many_lines)] // one pass over the file's sections
    pub fn parse(text: &str) -> Result<Self, MotionError> {
        const F: &str = "skeleton";
        let root = Bone {
            name: "root".to_owned(),
            direction: Vec3::Y,
            length: 0.0,
            axis: Quat::IDENTITY,
            dof: vec![],
            parent: None,
        };
        let mut sk = Self {
            length_unit: 1.0,
            bones: vec![root],
            root_order: vec![0, 1, 2, 3, 4, 5],
            by_name: HashMap::from([("root".to_owned(), 0)]),
        };
        let mut section = "";
        let mut current: Option<Bone> = None;
        for (n, raw) in text.lines().enumerate() {
            let line = n + 1;
            let trimmed = raw.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if let Some(name) = trimmed.strip_prefix(':') {
                section = name.split_whitespace().next().unwrap_or("");
                continue;
            }
            let words: Vec<&str> = trimmed.split_whitespace().collect();
            match (section, words.first().copied()) {
                ("units", Some("length")) => {
                    let v = numbers(F, line, &words[1..2.min(words.len())])?;
                    sk.length_unit = v.first().copied().filter(|v| *v > 0.0).ok_or_else(|| parse_err(F, line, "no length unit"))?;
                }
                ("root", Some("order")) => {
                    sk.root_order = words[1..]
                        .iter()
                        .map(|w| match w.to_ascii_uppercase().as_str() {
                            "TX" => Ok(0),
                            "TY" => Ok(1),
                            "TZ" => Ok(2),
                            "RX" => Ok(3),
                            "RY" => Ok(4),
                            "RZ" => Ok(5),
                            other => Err(parse_err(F, line, format!("unknown root channel {other}"))),
                        })
                        .collect::<Result<_, _>>()?;
                }
                ("bonedata", Some("begin")) => {
                    current = Some(Bone {
                        name: String::new(),
                        direction: Vec3::Y,
                        length: 0.0,
                        axis: Quat::IDENTITY,
                        dof: vec![],
                        parent: None,
                    });
                }
                ("bonedata", Some("end")) => {
                    let bone = current.take().ok_or_else(|| parse_err(F, line, "end without begin"))?;
                    if bone.name.is_empty() {
                        return Err(parse_err(F, line, "a bone without a name"));
                    }
                    sk.by_name.insert(bone.name.clone(), sk.bones.len());
                    sk.bones.push(bone);
                }
                ("bonedata", Some(key)) => {
                    let Some(bone) = current.as_mut() else { continue };
                    match key {
                        "name" => words.get(1).copied().unwrap_or_default().clone_into(&mut bone.name),
                        "direction" => {
                            let v = numbers(F, line, &words[1..words.len().min(4)])?;
                            if v.len() == 3 {
                                bone.direction = Vec3::new(v[0], v[1], v[2]).normalize_or_zero();
                            }
                        }
                        "length" => bone.length = numbers(F, line, &words[1..2.min(words.len())])?.first().copied().unwrap_or(0.0),
                        "axis" => {
                            let v = numbers(F, line, &words[1..words.len().min(4)])?;
                            let order = words.get(4).copied().unwrap_or("XYZ");
                            if v.len() != 3 || !order.eq_ignore_ascii_case("XYZ") {
                                return Err(parse_err(F, line, "only `axis x y z XYZ` is read"));
                            }
                            bone.axis = euler_xyz([v[0], v[1], v[2]]);
                        }
                        "dof" => {
                            bone.dof = words[1..]
                                .iter()
                                .filter_map(|w| match w.to_ascii_lowercase().as_str() {
                                    "rx" => Some(Ok(0)),
                                    "ry" => Some(Ok(1)),
                                    "rz" => Some(Ok(2)),
                                    "l" => None,
                                    other => Some(Err(parse_err(F, line, format!("unsupported dof {other}")))),
                                })
                                .collect::<Result<_, _>>()?;
                        }
                        _ => {}
                    }
                }
                ("hierarchy", Some("begin" | "end")) => {}
                ("hierarchy", Some(parent)) => {
                    let p = *sk.by_name.get(parent).ok_or_else(|| parse_err(F, line, format!("unknown bone {parent}")))?;
                    for child in &words[1..] {
                        let c = *sk.by_name.get(*child).ok_or_else(|| parse_err(F, line, format!("unknown bone {child}")))?;
                        sk.bones[c].parent = Some(p);
                    }
                }
                _ => {}
            }
        }
        // Parents must come first for the forward pass; ASF lists bones in any order.
        if sk.bones.iter().skip(1).any(|b| b.parent.is_none()) {
            return Err(parse_err(F, 0, "a bone has no parent in the hierarchy"));
        }
        Ok(sk)
    }

    /// The order to visit bones so each comes after its parent.
    fn order(&self) -> Vec<usize> {
        let mut out = vec![0];
        let mut i = 0;
        while i < out.len() {
            let p = out[i];
            out.extend((0..self.bones.len()).filter(|c| self.bones[*c].parent == Some(p)));
            i += 1;
        }
        out
    }

    /// The rest hips height: the root above the lowest bone end, metres.
    fn hips_height(&self, order: &[usize]) -> f32 {
        let mut ends = vec![Vec3::ZERO; self.bones.len()];
        for &i in &order[1..] {
            let b = &self.bones[i];
            let p = b.parent.map_or(Vec3::ZERO, |p| ends[p]);
            ends[i] = p + b.direction * b.length;
        }
        let lowest = ends.iter().map(|e| e.y).fold(0.0_f32, f32::min);
        -lowest / self.length_unit * INCH_M
    }

    /// Reads an `.amc` motion on this skeleton, recorded at `fps`, retargeted onto the VRM humanoid with its hips
    /// travel scaled to a body whose hips stand `hips_height` metres up.
    ///
    /// # Errors
    /// [`MotionError::Parse`] for text that is not an AMC motion of this skeleton;
    /// [`MotionError::Empty`] for no frames. A humanoid bone the skeleton lacks (a skeleton with no toes, one neck bone, no
    /// hands) stays straight on its parent.
    #[allow(clippy::too_many_lines)] // one pass over the file and one over the frames
    pub fn motion(&self, text: &str, fps: f32, hips_height: f32) -> Result<HumanoidTake, MotionError> {
        const F: &str = "motion";
        let order = self.order();
        let n = self.bones.len();
        // Raw channels per frame: root's six, then each bone's dof angles.
        let mut frames: Vec<Vec<Option<Vec<f32>>>> = Vec::new();
        for (k, raw) in text.lines().enumerate() {
            let line = k + 1;
            let t = raw.trim();
            if t.is_empty() || t.starts_with('#') || t.starts_with(':') {
                continue;
            }
            if t.bytes().all(|c| c.is_ascii_digit()) {
                frames.push(vec![None; n]);
                continue;
            }
            let words: Vec<&str> = t.split_whitespace().collect();
            let Some(frame) = frames.last_mut() else {
                return Err(parse_err(F, line, "values before the first frame number"));
            };
            let Some(&i) = self.by_name.get(words[0]) else {
                return Err(parse_err(F, line, format!("unknown bone {}", words[0])));
            };
            frame[i] = Some(numbers(F, line, &words[1..])?);
        }
        if frames.is_empty() {
            return Err(MotionError::Empty);
        }
        let unit_m = INCH_M / self.length_unit;
        // A CMU bone need not rest along the humanoid's: its legs rest 20 degrees apart, its collarbones tilted up.
        // Each humanoid bone takes its ASF bone's rotation from the ASF rest *direction*, so the limb points where the
        // performer's did, not 20 degrees inward of it.
        let aligns: Vec<Quat> = MAP
            .iter()
            .map(|(bone, name)| {
                let theirs = self.by_name.get(*name).map_or(Vec3::ZERO, |i| self.bones[*i].direction);
                match rest_direction(*bone) {
                    Some(ours) if theirs.length_squared() > 0.5 => Quat::from_rotation_arc(ours, theirs.normalize()),
                    _ => Quat::IDENTITY,
                }
            })
            .collect();
        let mut world = vec![Quat::IDENTITY; n];
        let mut out = Vec::with_capacity(frames.len());
        let mut first: Option<(Quat, Vec3)> = None;
        for frame in &frames {
            let mut hips = Vec3::ZERO;
            for &i in &order {
                let bone = &self.bones[i];
                let values = frame[i].as_deref().unwrap_or(&[]);
                if i == 0 {
                    let mut ch = [0.0_f32; 6];
                    for (slot, v) in self.root_order.iter().zip(values) {
                        ch[*slot] = *v;
                    }
                    hips = Vec3::new(ch[0], ch[1], ch[2]) * unit_m;
                    world[0] = euler_xyz([ch[3], ch[4], ch[5]]);
                    continue;
                }
                let mut deg = [0.0_f32; 3];
                for (axis, v) in bone.dof.iter().zip(values) {
                    deg[*axis] = *v;
                }
                let local = bone.axis * euler_xyz(deg) * bone.axis.inverse();
                let parent = bone.parent.map_or(Quat::IDENTITY, |p| world[p]);
                world[i] = (parent * local).normalize();
            }
            // Face +Z on the first frame: the root's heading about +Y.
            let (turn, origin) = *first.get_or_insert_with(|| {
                let forward = world[0] * Vec3::Z;
                let yaw = forward.x.atan2(forward.z);
                (Quat::from_rotation_y(-yaw), hips)
            });
            let mut pose = HumanoidPose::REST;
            let mut worlds: HashMap<B, Quat> = HashMap::with_capacity(MAP.len());
            // MAP lists parents first, so a bone the skeleton lacks takes its parent's world: it stays straight on it.
            for ((bone, name), align) in MAP.into_iter().zip(&aligns) {
                let mine = match self.by_name.get(name) {
                    Some(i) => turn * world[*i] * *align,
                    None => bone.parent().and_then(|p| worlds.get(&p).copied()).unwrap_or(turn),
                };
                worlds.insert(bone, mine);
            }
            for (bone, _) in MAP {
                let mine = worlds[&bone];
                let parent = bone.parent().and_then(|p| worlds.get(&p).copied()).unwrap_or(Quat::IDENTITY);
                pose.set(bone, (parent.inverse() * mine).normalize());
            }
            let travel = turn * (hips - origin);
            pose.hips_offset = travel;
            out.push(pose);
        }
        let take = HumanoidTake { fps, frames: out, hips_height: self.hips_height(&order) };
        Ok(take.for_hips_height(hips_height))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ASF: &str = "\
:version 1.10
:units
  length 0.45
  angle deg
:root
   order TX TY TZ RX RY RZ
   axis XYZ
:bonedata
";

    fn bone(id: usize, name: &str, dir: &str, len: f32, axis: &str, dof: &str) -> String {
        let dof = if dof.is_empty() { String::new() } else { format!("    dof {dof}\n") };
        format!("  begin\n    id {id}\n    name {name}\n    direction {dir}\n    length {len}\n    axis {axis} XYZ\n{dof}  end\n")
    }

    /// A tiny CMU-shaped skeleton: every mapped bone, straight T-pose directions.
    fn skeleton() -> String {
        let rows = [
            ("lowerback", "0 1 0", "0 0 0", "rx ry rz"),
            ("upperback", "0 1 0", "0 0 0", "rx ry rz"),
            ("thorax", "0 1 0", "0 0 0", "rx ry rz"),
            ("lowerneck", "0 1 0", "0 0 0", "rx ry rz"),
            ("upperneck", "0 1 0", "0 0 0", "rx ry rz"),
            ("head", "0 1 0", "0 0 0", "rx ry rz"),
            ("lclavicle", "1 0 0", "0 0 0", "ry rz"),
            ("lhumerus", "1 0 0", "0 0 -90", "rx ry rz"),
            ("lradius", "1 0 0", "0 0 -90", "rx"),
            ("lwrist", "1 0 0", "0 0 -90", "ry"),
            ("lhand", "1 0 0", "0 0 -90", "rx rz"),
            ("rclavicle", "-1 0 0", "0 0 0", "ry rz"),
            ("rhumerus", "-1 0 0", "0 0 90", "rx ry rz"),
            ("rradius", "-1 0 0", "0 0 90", "rx"),
            ("rwrist", "-1 0 0", "0 0 90", "ry"),
            ("rhand", "-1 0 0", "0 0 90", "rx rz"),
            ("lfemur", "0 -1 0", "0 0 0", "rx ry rz"),
            ("ltibia", "0 -1 0", "0 0 0", "rx"),
            ("lfoot", "0 0 1", "0 0 0", "rx rz"),
            ("ltoes", "0 0 1", "0 0 0", "rx"),
            ("rfemur", "0 -1 0", "0 0 0", "rx ry rz"),
            ("rtibia", "0 -1 0", "0 0 0", "rx"),
            ("rfoot", "0 0 1", "0 0 0", "rx rz"),
            ("rtoes", "0 0 1", "0 0 0", "rx"),
        ];
        let mut s = ASF.to_owned();
        for (i, (name, dir, axis, dof)) in rows.iter().enumerate() {
            s += &bone(i + 1, name, dir, 7.0, axis, dof);
        }
        s += ":hierarchy\n  begin\n    root lowerback lfemur rfemur\n    lowerback upperback\n    upperback thorax\n";
        s += "    thorax lowerneck lclavicle rclavicle\n    lowerneck upperneck\n    upperneck head\n";
        s += "    lclavicle lhumerus\n    lhumerus lradius\n    lradius lwrist\n    lwrist lhand\n";
        s += "    rclavicle rhumerus\n    rhumerus rradius\n    rradius rwrist\n    rwrist rhand\n";
        s += "    lfemur ltibia\n    ltibia lfoot\n    lfoot ltoes\n    rfemur rtibia\n    rtibia rfoot\n    rfoot rtoes\n  end\n";
        s
    }

    fn angle_deg(a: Quat, b: Quat) -> f32 {
        a.angle_between(b).to_degrees()
    }

    #[test]
    fn a_still_frame_is_the_rest_pose_and_a_bent_elbow_bends_the_lower_arm() {
        let sk = Skeleton::parse(&skeleton()).unwrap();
        let amc = ":FULLY-SPECIFIED\n:DEGREES\n1\nroot 0 30 0 0 0 0\n2\nroot 0 30 0 0 0 0\nlradius 90\nlowerback 0 30 0\n";
        let take = sk.motion(amc, 120.0, 0.9).unwrap();
        assert_eq!(take.frames.len(), 2);
        let rest = &take.frames[0];
        assert!(rest.rotations.iter().all(|q| angle_deg(*q, Quat::IDENTITY) < 1e-3), "{rest:?}");
        let bent = &take.frames[1];
        assert!((angle_deg(bent.get(B::LeftLowerArm), Quat::IDENTITY) - 90.0).abs() < 1e-3);
        // The lower arm's rest frame is turned -90 about Z, so its rx turns about the world's Y: the elbow folds the
        // forearm forward or back in the horizontal plane, never up.
        let axis = bent.get(B::LeftLowerArm).to_axis_angle().0;
        assert!(axis.y.abs() > 0.99, "{axis}");
        // The spine's ry is a turn about +Y, and only the spine carries it: the chest above is unturned locally.
        assert!((angle_deg(bent.get(B::Spine), Quat::from_rotation_y(30f32.to_radians()))).abs() < 1e-3);
        assert!(angle_deg(bent.get(B::Chest), Quat::IDENTITY) < 1e-3);
        assert!((take.seconds() - 1.0 / 120.0).abs() < 1e-6);
    }

    #[test]
    fn the_take_is_turned_to_face_plus_z_and_its_travel_is_in_metres_from_the_first_frame() {
        let sk = Skeleton::parse(&skeleton()).unwrap();
        // Facing -Z (turned 180 about Y), stepping 10 units towards its own front.
        let amc = ":DEGREES\n1\nroot 0 30 0 0 180 0\n2\nroot 0 30 -10 0 180 0\n";
        let take = sk.motion(amc, 60.0, 0.0).unwrap();
        assert!(angle_deg(take.frames[0].get(B::Hips), Quat::IDENTITY) < 1e-3);
        let step = take.frames[1].hips_offset;
        let metres = 10.0 * INCH_M / 0.45;
        assert!((step - Vec3::new(0.0, 0.0, metres)).length() < 1e-4, "{step}");
    }

    #[test]
    fn garbage_is_refused_with_its_line() {
        assert!(matches!(Skeleton::parse(":units\n length x\n"), Err(MotionError::Parse { line: 2, .. })));
        let sk = Skeleton::parse(&skeleton()).unwrap();
        assert!(matches!(sk.motion("1\nnobone 1 2 3\n", 60.0, 1.0), Err(MotionError::Parse { line: 2, .. })));
        assert_eq!(sk.motion(":DEGREES\n", 60.0, 1.0), Err(MotionError::Empty));
    }
}
