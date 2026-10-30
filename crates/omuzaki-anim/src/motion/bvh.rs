//! The `BioVision` Hierarchy (`.bvh`), the commonest motion-capture interchange format (Mixamo, Rokoko, Perception Neuron, the
//! CMU library's BVH conversion, Blender), read and retargeted onto the VRM humanoid.
//!
//! **Rotations.** A joint rests at its `OFFSET` from its parent with no rotation, and turns by its rotation channels in
//! the order the file lists them (`Zrotation Xrotation Yrotation` is `Rz · Rx · Ry`), so its world rotation from the
//! rest is `M = M_parent · R`. The root's position channels place the hips.
//!
//! **Which joint is which.** Names differ between tools, so a joint is matched by what its name says once a namespace
//! (`mixamorig:`) and a side (`Left`, `L`, `l_`) are taken off: `Arm` or `UpperArm` is the upper arm, `ForeArm` the
//! lower, `UpLeg` or `Thigh` the upper leg, and so on. The spine is the chain of joints between the root and the neck,
//! however many there are: the first is the spine, the second the chest, the last the upper chest (a longer chain folds
//! its middle into them). The root is the hips.
//!
//! **Rest.** A BVH can rest in an A-pose or any other: each humanoid bone takes its joint's rotation from the joint's
//! rest direction (toward its child), so a limb points where the performer's did. **Facing** and **travel** are as for
//! [`asf`](super::asf): the first frame faces `+Z` and the hips travel is scaled to the hips height asked for.

use std::collections::HashMap;

use omuzaki_anim_api::HumanoidBone as B;
use omuzaki_core::glam::{Quat, Vec3};

use super::{HumanoidTake, Mapping, MotionError, facing};

/// The humanoid bones a BVH can drive, parents first.
const ORDER: [B; 22] = [
    B::Hips,
    B::Spine,
    B::Chest,
    B::UpperChest,
    B::Neck,
    B::Head,
    B::LeftShoulder,
    B::LeftUpperArm,
    B::LeftLowerArm,
    B::LeftHand,
    B::RightShoulder,
    B::RightUpperArm,
    B::RightLowerArm,
    B::RightHand,
    B::LeftUpperLeg,
    B::LeftLowerLeg,
    B::LeftFoot,
    B::RightUpperLeg,
    B::RightLowerLeg,
    B::RightFoot,
    B::LeftToes,
    B::RightToes,
];

/// The limb names, side taken off, for each side's bones, in order of preference.
const LIMBS: [(&[&str], B, B); 8] = [
    (&["shoulder", "collar", "clavicle"], B::LeftShoulder, B::RightShoulder),
    (&["arm", "upperarm", "uparm", "humerus"], B::LeftUpperArm, B::RightUpperArm),
    (&["forearm", "lowerarm", "radius", "elbow"], B::LeftLowerArm, B::RightLowerArm),
    (&["hand", "wrist"], B::LeftHand, B::RightHand),
    (&["upleg", "upperleg", "thigh", "femur"], B::LeftUpperLeg, B::RightUpperLeg),
    (&["leg", "lowerleg", "shin", "calf", "knee", "tibia"], B::LeftLowerLeg, B::RightLowerLeg),
    (&["foot", "ankle"], B::LeftFoot, B::RightFoot),
    (&["toebase", "toe", "toes"], B::LeftToes, B::RightToes),
];

/// One channel of a joint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Channel {
    Position(usize),
    Rotation(usize),
}

/// One joint.
#[derive(Clone, Debug)]
struct Joint {
    name: String,
    parent: Option<usize>,
    offset: Vec3,
    channels: Vec<Channel>,
    /// The `End Site` offset, for a joint with no child joint.
    end: Option<Vec3>,
}

/// A parsed BVH file: the hierarchy and its frames.
#[derive(Clone, Debug)]
pub struct Bvh {
    joints: Vec<Joint>,
    /// Seconds a frame.
    frame_time: f32,
    /// Every channel of every joint, in file order, a row a frame.
    frames: Vec<Vec<f32>>,
}

fn parse_err(line: usize, message: impl Into<String>) -> MotionError {
    MotionError::Parse { file: "motion", line, message: message.into() }
}

/// The words of `text` with the line each is on.
fn words(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.lines().enumerate().flat_map(|(n, line)| line.split_whitespace().map(move |w| (n + 1, w)))
}

fn number(line: usize, word: Option<(usize, &str)>) -> Result<f32, MotionError> {
    let (l, w) = word.ok_or_else(|| parse_err(line, "the file ends early"))?;
    w.parse::<f32>().ok().filter(|v| v.is_finite()).ok_or_else(|| parse_err(l, format!("{w:?} is not a number")))
}

/// A joint's name as its part: lowercase, with any namespace and separators taken off.
fn bare(name: &str) -> String {
    let name = name.rsplit(':').next().unwrap_or(name);
    name.chars().filter(char::is_ascii_alphanumeric).collect::<String>().to_ascii_lowercase()
}

/// The side a bare name is on and the part after the side, if it names a side.
fn sided(bare: &str) -> Option<(bool, &str)> {
    for (prefix, left) in [("left", true), ("right", false), ("l", true), ("r", false)] {
        if let Some(rest) = bare.strip_prefix(prefix)
            && !rest.is_empty()
        {
            return Some((left, rest));
        }
    }
    None
}

impl Bvh {
    /// Parses a `.bvh` file.
    ///
    /// # Errors
    /// [`MotionError::Parse`] for text that is not a BVH file; [`MotionError::Empty`] for no frames.
    #[allow(clippy::too_many_lines)] // one pass over the hierarchy and one over the frames
    pub fn parse(text: &str) -> Result<Self, MotionError> {
        let mut it = words(text).peekable();
        match it.next() {
            Some((_, w)) if w.eq_ignore_ascii_case("HIERARCHY") => {}
            other => return Err(parse_err(other.map_or(1, |(l, _)| l), "a BVH file starts with HIERARCHY")),
        }
        let mut joints: Vec<Joint> = Vec::new();
        let mut stack: Vec<Option<usize>> = Vec::new();
        let mut in_end = false;
        let mut line = 1;
        loop {
            let Some((l, w)) = it.next() else { return Err(parse_err(line, "no MOTION section")) };
            line = l;
            match w.to_ascii_uppercase().as_str() {
                "ROOT" | "JOINT" => {
                    let (_, name) = it.next().ok_or_else(|| parse_err(l, "a joint without a name"))?;
                    let parent = stack.last().copied().flatten();
                    if w.eq_ignore_ascii_case("ROOT") != joints.is_empty() {
                        return Err(parse_err(l, "one ROOT, first"));
                    }
                    joints.push(Joint { name: name.to_owned(), parent, offset: Vec3::ZERO, channels: vec![], end: None });
                }
                "END" => {
                    it.next();
                    in_end = true;
                }
                "{" => stack.push(if in_end { None } else { Some(joints.len().saturating_sub(1)) }),
                "}" => {
                    if stack.pop().is_none() {
                        return Err(parse_err(l, "a } without its {"));
                    }
                    in_end = false;
                }
                "OFFSET" => {
                    let v = Vec3::new(number(l, it.next())?, number(l, it.next())?, number(l, it.next())?);
                    let current = stack.last().copied().ok_or_else(|| parse_err(l, "an OFFSET outside a joint"))?;
                    if let Some(i) = current {
                        joints[i].offset = v;
                    } else {
                        let owner = stack
                            .iter()
                            .rev()
                            .find_map(|s| *s)
                            .ok_or_else(|| parse_err(l, "an End Site with no joint"))?;
                        joints[owner].end = Some(v);
                    }
                }
                "CHANNELS" => {
                    let count = number(l, it.next())?;
                    let i = stack.last().copied().flatten().ok_or_else(|| parse_err(l, "CHANNELS outside a joint"))?;
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // checked to 0..=6 first
                    let count = if (0.0..=6.0).contains(&count) { count as usize } else { return Err(parse_err(l, "a joint has 0 to 6 channels")) };
                    for _ in 0..count {
                        let (cl, c) = it.next().ok_or_else(|| parse_err(l, "the file ends early"))?;
                        let channel = match c.to_ascii_lowercase().as_str() {
                            "xposition" => Channel::Position(0),
                            "yposition" => Channel::Position(1),
                            "zposition" => Channel::Position(2),
                            "xrotation" => Channel::Rotation(0),
                            "yrotation" => Channel::Rotation(1),
                            "zrotation" => Channel::Rotation(2),
                            other => return Err(parse_err(cl, format!("unknown channel {other}"))),
                        };
                        joints[i].channels.push(channel);
                    }
                }
                "MOTION" => break,
                _ => return Err(parse_err(l, format!("unexpected {w:?}"))),
            }
        }
        if joints.is_empty() || !stack.is_empty() {
            return Err(parse_err(line, "the hierarchy is not closed"));
        }
        // Frames: n / Frame Time: t / the rows.
        let mut count = None;
        let mut frame_time = None;
        while count.is_none() || frame_time.is_none() {
            let (l, w) = it.next().ok_or_else(|| parse_err(line, "no Frames and Frame Time"))?;
            line = l;
            match w.to_ascii_lowercase().as_str() {
                "frames:" => count = Some(number(l, it.next())?),
                "frame" => {
                    it.next();
                    frame_time = Some(number(l, it.next())?);
                }
                _ => return Err(parse_err(l, format!("unexpected {w:?}"))),
            }
        }
        let width: usize = joints.iter().map(|j| j.channels.len()).sum();
        let mut frames = Vec::new();
        let mut row = Vec::with_capacity(width);
        for (l, w) in it {
            row.push(w.parse::<f32>().ok().filter(|v| v.is_finite()).ok_or_else(|| parse_err(l, format!("{w:?} is not a number")))?);
            if row.len() == width {
                frames.push(std::mem::replace(&mut row, Vec::with_capacity(width)));
            }
        }
        if !row.is_empty() {
            return Err(parse_err(line, "the last frame is cut short"));
        }
        if frames.is_empty() || count == Some(0.0) {
            return Err(MotionError::Empty);
        }
        let frame_time = frame_time.filter(|t| *t > 0.0).ok_or_else(|| parse_err(line, "Frame Time must be above zero"))?;
        Ok(Self { joints, frame_time, frames })
    }

    /// Frames a second of the recording.
    pub fn fps(&self) -> f32 {
        1.0 / self.frame_time
    }

    /// The joints' children, in file order.
    fn children(&self, i: usize) -> impl Iterator<Item = usize> + '_ {
        (0..self.joints.len()).filter(move |c| self.joints[*c].parent == Some(i))
    }

    /// Where joint `i` points at rest: toward its centre child (the one that names no side), else its first child, else
    /// its end site.
    fn rest_direction(&self, i: usize) -> Vec3 {
        let mut children = self.children(i).collect::<Vec<_>>();
        children.sort_by_key(|c| sided(&bare(&self.joints[*c].name)).is_some());
        children.first().map_or_else(|| self.joints[i].end.unwrap_or(Vec3::ZERO), |c| self.joints[*c].offset).normalize_or_zero()
    }

    /// Which joint each humanoid bone follows.
    fn humanoid(&self) -> HashMap<B, usize> {
        let mut map = HashMap::from([(B::Hips, 0)]);
        let named: Vec<String> = self.joints.iter().map(|j| bare(&j.name)).collect();
        for (i, name) in named.iter().enumerate() {
            if let Some((left, part)) = sided(name) {
                for (names, l, r) in LIMBS {
                    // Earlier names are preferred: a `hand` wins over a `wrist`.
                    let rank = names.iter().position(|n| *n == part);
                    let bone = if left { l } else { r };
                    let held = map.get(&bone).and_then(|j| names.iter().position(|n| sided(&named[*j]).is_some_and(|(_, p)| p == *n)));
                    if let Some(rank) = rank
                        && held.is_none_or(|h| rank < h)
                    {
                        map.insert(bone, i);
                    }
                }
            }
        }
        // The centre chain: root ... spine ... neck ... head.
        if let Some(head) = named.iter().position(|n| n == "head") {
            map.insert(B::Head, head);
            let mut chain = vec![];
            let mut at = self.joints[head].parent;
            while let Some(j) = at.filter(|j| *j != 0) {
                chain.push(j);
                at = self.joints[j].parent;
            }
            chain.reverse();
            let (necks, spine): (Vec<usize>, Vec<usize>) = chain.into_iter().partition(|j| named[*j].starts_with("neck"));
            if let Some(neck) = necks.last() {
                map.insert(B::Neck, *neck);
            }
            for (bone, joint) in [(B::Spine, spine.first()), (B::Chest, spine.get(1)), (B::UpperChest, spine.get(2).and(spine.last()))] {
                if let Some(j) = joint {
                    map.insert(bone, *j);
                }
            }
        }
        map
    }

    /// The rest hips height: the root above the lowest joint or end site, in file units.
    fn hips_height(&self) -> f32 {
        let mut at = vec![Vec3::ZERO; self.joints.len()];
        let mut lowest = 0.0_f32;
        for (i, joint) in self.joints.iter().enumerate() {
            at[i] = joint.parent.map_or(Vec3::ZERO, |p| at[p] + joint.offset);
            lowest = lowest.min(at[i].y).min(joint.end.map_or(0.0, |e| at[i].y + e.y));
        }
        -lowest
    }

    /// The motion retargeted onto the VRM humanoid, with its hips travel scaled to a body whose hips stand
    /// `hips_height` up (in the take's units when the file has no legs to measure).
    pub fn motion(&self, hips_height: f32) -> HumanoidTake {
        let map = self.humanoid();
        let mapping = Mapping::new(ORDER.into_iter().map(|bone| (bone, map.get(&bone).map(|j| (*j, self.rest_direction(*j))))));
        let n = self.joints.len();
        let mut world = vec![Quat::IDENTITY; n];
        let mut first: Option<(Quat, Vec3)> = None;
        let mut out = Vec::with_capacity(self.frames.len());
        for row in &self.frames {
            let mut values = row.iter();
            let mut hips = self.joints[0].offset;
            for (i, joint) in self.joints.iter().enumerate() {
                let mut local = Quat::IDENTITY;
                let mut position = joint.offset;
                for channel in &joint.channels {
                    let v = values.next().copied().unwrap_or(0.0);
                    match channel {
                        Channel::Position(axis) => position[*axis] = v,
                        Channel::Rotation(0) => local *= Quat::from_rotation_x(v.to_radians()),
                        Channel::Rotation(1) => local *= Quat::from_rotation_y(v.to_radians()),
                        Channel::Rotation(_) => local *= Quat::from_rotation_z(v.to_radians()),
                    }
                }
                if i == 0 {
                    hips = position;
                }
                // Joints come parents first in a BVH file.
                world[i] = (joint.parent.map_or(Quat::IDENTITY, |p| world[p]) * local).normalize();
            }
            let (turn, origin) = *first.get_or_insert_with(|| (facing(world[0]), hips));
            let mut pose = mapping.pose(&world, turn);
            pose.hips_offset = turn * (hips - origin);
            out.push(pose);
        }
        let take = HumanoidTake { fps: self.fps(), frames: out, hips_height: self.hips_height() };
        take.for_hips_height(hips_height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Mixamo-named skeleton resting in an A-pose (arms 45 degrees down), with `rows` of motion.
    fn bvh(rows: &[&str]) -> String {
        let s = 0.5_f32.sqrt() * 10.0;
        let arm = |side: &str, x: f32| {
            format!(
                "JOINT mixamorig:{side}Shoulder {{ OFFSET {} 10 0 CHANNELS 3 Zrotation Xrotation Yrotation
                 JOINT mixamorig:{side}Arm {{ OFFSET {} 0 0 CHANNELS 3 Zrotation Xrotation Yrotation
                 JOINT mixamorig:{side}ForeArm {{ OFFSET {} {} 0 CHANNELS 3 Zrotation Xrotation Yrotation
                 JOINT mixamorig:{side}Hand {{ OFFSET {} {} 0 CHANNELS 3 Zrotation Xrotation Yrotation
                 End Site {{ OFFSET {} {} 0 }} }} }} }} }}",
                x * 3.0, x * 8.0, x * s, -s, x * s, -s, x * 2.0, -2.0
            )
        };
        let leg = |side: &str, x: f32| {
            format!(
                "JOINT {side}UpLeg {{ OFFSET {} 0 0 CHANNELS 3 Zrotation Xrotation Yrotation
                 JOINT {side}Leg {{ OFFSET 0 -45 0 CHANNELS 3 Zrotation Xrotation Yrotation
                 JOINT {side}Foot {{ OFFSET 0 -50 0 CHANNELS 3 Zrotation Xrotation Yrotation
                 End Site {{ OFFSET 0 -5 10 }} }} }} }}",
                x * 10.0
            )
        };
        format!(
            "HIERARCHY\nROOT mixamorig:Hips {{ OFFSET 0 100 0 CHANNELS 6 Xposition Yposition Zposition Zrotation Xrotation Yrotation
             JOINT mixamorig:Spine {{ OFFSET 0 10 0 CHANNELS 3 Zrotation Xrotation Yrotation
             JOINT mixamorig:Spine1 {{ OFFSET 0 10 0 CHANNELS 3 Zrotation Xrotation Yrotation
             JOINT mixamorig:Neck {{ OFFSET 0 20 0 CHANNELS 3 Zrotation Xrotation Yrotation
             JOINT mixamorig:Head {{ OFFSET 0 10 0 CHANNELS 3 Zrotation Xrotation Yrotation End Site {{ OFFSET 0 20 0 }} }} }}
             {} {} }} }} {} {} }}\nMOTION\nFrames: {}\nFrame Time: 0.0333333\n{}\n",
            arm("Left", 1.0),
            arm("Right", -1.0),
            leg("Left", 1.0),
            leg("Right", -1.0),
            rows.len(),
            rows.join("\n")
        )
    }

    /// A row of every channel at zero (the hips 100 up), with `edits` (index, value) applied.
    fn row(edits: &[(usize, f32)]) -> String {
        let mut v = [0.0_f32; 6 + 3 * 18];
        v[1] = 100.0;
        for (i, x) in edits {
            v[*i] = *x;
        }
        v.iter().map(ToString::to_string).collect::<Vec<_>>().join(" ")
    }

    fn deg(a: Quat, b: Quat) -> f32 {
        a.angle_between(b).to_degrees()
    }

    #[test]
    fn the_names_map_onto_the_humanoid_and_the_a_pose_rests_as_one() {
        let file = Bvh::parse(&bvh(&[&row(&[])])).unwrap();
        let map = file.humanoid();
        let name = |b: B| file.joints[map[&b]].name.clone();
        assert_eq!(name(B::LeftLowerArm), "mixamorig:LeftForeArm");
        assert_eq!(name(B::RightUpperLeg), "RightUpLeg");
        assert_eq!((name(B::Spine), name(B::Chest)), ("mixamorig:Spine".into(), "mixamorig:Spine1".into()));
        assert!(!map.contains_key(&B::UpperChest), "a two-joint spine has no upper chest");
        let take = file.motion(0.9);
        let rest = &take.frames[0];
        // The file's arms hang 45 degrees down at rest, so the humanoid's upper arms turn 45 degrees from the T.
        assert!((deg(rest.get(B::LeftUpperArm), Quat::IDENTITY) - 45.0).abs() < 1e-3, "{rest:?}");
        let down = rest.get(B::LeftUpperArm) * Vec3::X;
        assert!(down.y < -0.7 && down.x > 0.7, "the left arm points down and out: {down}");
        assert!(deg(rest.get(B::LeftLowerArm), Quat::IDENTITY) < 1e-3, "the forearm is straight on it");
        assert!(deg(rest.get(B::Head), Quat::IDENTITY) < 1e-3);
        assert!((take.fps - 30.0).abs() < 1e-3);
        assert!((take.hips_height - 0.9).abs() < 1e-6);
    }

    #[test]
    fn a_bent_elbow_bends_the_lower_arm_and_the_hips_travel_in_metres() {
        // Channel layout: hips 0..6, spine 6, spine1 9, neck 12, head 15, left shoulder 18, arm 21, forearm 24 (its
        // first channel is Zrotation).
        let file = Bvh::parse(&bvh(&[&row(&[]), &row(&[(24, 60.0), (2, 50.0)])])).unwrap();
        let take = file.motion(0.9);
        let bent = &take.frames[1];
        assert!((deg(bent.get(B::LeftLowerArm), Quat::IDENTITY) - 60.0).abs() < 1e-2, "{bent:?}");
        // 50 units forward on a body whose hips stand 100 up, scaled to 0.9 m hips.
        assert!((bent.hips_offset - Vec3::new(0.0, 0.0, 0.45)).length() < 1e-4, "{}", bent.hips_offset);
    }

    #[test]
    fn garbage_is_refused_with_its_line() {
        assert!(matches!(Bvh::parse("nope"), Err(MotionError::Parse { line: 1, .. })));
        let cut = bvh(&[&row(&[])]).replace(" 0 0 0 0 0 0 0 0 0\n", "\n");
        assert!(matches!(Bvh::parse(&cut), Err(MotionError::Parse { .. })), "a short last frame");
        assert!(matches!(Bvh::parse(&bvh(&[])), Err(MotionError::Empty)));
    }
}
