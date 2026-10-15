//! Humanoid auto-mapping: which node of a skinned skeleton plays each VRM humanoid bone.
//!
//! FBX carries no humanoid data, and glTFs and VRMs often have none or a wrong one, so every importer needs this. The
//! pipeline is pure and deterministic (candidates are visited in node order):
//!
//! 1. **Names.** A node's name is split into lower-case words (camel case, digits and `_ . : -` separate them), the
//!    known prefixes are stripped and the side is read from `Left`/`Right`/`L`/`R` at either end. The remaining key
//!    is looked up in the table of the skeleton's [`Convention`], or else among the VRM bone names themselves and a few
//!    generic aliases ([`MatchHow::Exact`]). The convention is the one most nodes carry a marker for.
//! 2. **Topology.** Every candidate is checked against the parent rule of the VRM spec (`HUMAN_BONES`): a bone's
//!    nearest humanoid ancestor must be its spec parent. Left and right limbs must lie on opposite sides.
//! 3. **Structure.** Only if a required bone is still missing, or an override waits on a parent bone no name matched:
//!    the first node (from the roots, depth first) with two steeply downward children and an upward one is the hips;
//!    the downward chains are the legs, the upward chain is the spine up to the node that branches into two sideways
//!    chains (the arms), and the neck and head continue up. Leg and arm joints are told apart by rest positions, not
//!    by their place in the chain: the knee lies about halfway down, the foot near the ground under the knee, the toes
//!    forward of the foot; the upper arm and forearm are the two long segments. Where that evidence is missing the
//!    bone stays missing. The scene is in VRM space (+Z front), so the character's left is +X. These are
//!    [`MatchHow::Inferred`].
//!
//! The name tables list names only. Each comes from the public bone naming of its source: [`Convention::Vrm`] is the
//! `J_Bip_*` naming of `VRoid` Studio's VRM export, [`Convention::Mixamo`] the `mixamorig:` rig of Adobe Mixamo,
//! [`Convention::UnityBip`] the `Bip001` naming of 3ds Max Biped as it reaches Unity, [`Convention::Rigify`] the
//! metarig of Blender's Rigify add-on, and [`Convention::Mmd`] the standard Japanese bone names of `MikuMikuDance`.

use std::collections::{BTreeMap, BTreeSet};

use omuzaki_core::glam::{Mat4, Vec3};
use omuzaki_io_api::{Avatar, Finding, HUMAN_BONES, HumanBoneName, Humanoid, IoScene, NodeIx, Report};

/// Which node plays each humanoid bone, with how it was found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HumanoidProposal {
    /// The proposed bone assignment.
    pub humanoid: Humanoid,
    /// How each bone was found, in bone table order.
    pub matches: Vec<BoneMatch>,
    /// Warnings about the proposal (`humanoid.inferred`, `humanoid.ambiguous`, `humanoid.sides_overlap`).
    pub report: Report,
}

/// One assigned bone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoneMatch {
    /// The humanoid bone.
    pub bone: HumanBoneName,
    /// The node that plays it.
    pub node: NodeIx,
    /// How the node was found.
    pub how: MatchHow,
}

/// How a bone's node was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchHow {
    /// Taken from the override table.
    Override,
    /// The name is a VRM bone name (or a generic alias of one) in any case or separator style.
    Exact,
    /// The name is in the table of this convention.
    Convention(Convention),
    /// No name matched; the node was chosen by the skeleton's structure and rest positions.
    Inferred,
}

/// A bone naming convention with a name table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Convention {
    /// `J_Bip_C_Hips`, `J_Bip_L_UpperArm` (`VRoid` Studio).
    Vrm,
    /// `mixamorig:LeftForeArm` (Adobe Mixamo).
    Mixamo,
    /// `Bip001 L Forearm` (3ds Max Biped, Unity).
    UnityBip,
    /// `spine.001`, `upper_arm.L`, `shin.L` (Blender Rigify metarig).
    Rigify,
    /// `センター`, `左腕`, `左ひじ` (`MikuMikuDance`).
    Mmd,
}

/// Why no humanoid could be proposed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapError {
    /// The required bones that no node could be found for.
    pub missing_required: Vec<HumanBoneName>,
    /// The findings, errors first-class: `humanoid.required_missing`, `humanoid.override_invalid`, `humanoid.scale_invalid`.
    pub report: Report,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Left,
    Right,
}

struct Parsed {
    side: Option<Side>,
    key: String,
    marker: Option<Convention>,
}

type Table = &'static [(&'static str, &'static str)];
// A leading `*` marks a bone that exists once per side.
#[rustfmt::skip]
const VRM: Table = &[
    ("hips", "hips"), ("spine", "spine"), ("chest", "chest"), ("upper chest", "upperChest"), ("neck", "neck"),
    ("head", "head"), ("shoulder", "*shoulder"), ("upper arm", "*upperArm"), ("lower arm", "*lowerArm"),
    ("hand", "*hand"), ("upper leg", "*upperLeg"), ("lower leg", "*lowerLeg"), ("foot", "*foot"),
    ("toe base", "*toes"), ("face eye", "*eye"), ("jaw", "jaw"),
];
#[rustfmt::skip]
const MIXAMO: Table = &[
    ("hips", "hips"), ("spine", "spine"), ("spine 1", "chest"), ("spine 2", "upperChest"), ("neck", "neck"),
    ("head", "head"), ("shoulder", "*shoulder"), ("arm", "*upperArm"), ("fore arm", "*lowerArm"), ("hand", "*hand"),
    ("up leg", "*upperLeg"), ("leg", "*lowerLeg"), ("foot", "*foot"), ("toe base", "*toes"),
    ("eye", "*eye"), ("jaw", "jaw"),
];
#[rustfmt::skip]
const UNITY_BIP: Table = &[
    ("pelvis", "hips"), ("spine", "spine"), ("spine 1", "chest"), ("spine 2", "upperChest"), ("neck", "neck"),
    ("head", "head"), ("clavicle", "*shoulder"), ("upper arm", "*upperArm"), ("forearm", "*lowerArm"),
    ("hand", "*hand"), ("thigh", "*upperLeg"), ("calf", "*lowerLeg"), ("foot", "*foot"), ("toe 0", "*toes"),
    ("eye", "*eye"), ("jaw", "jaw"),
];
// In the Rigify metarig `spine` is the pelvis bone and the torso chain starts at `spine.001`.
#[rustfmt::skip]
const RIGIFY: Table = &[
    ("spine", "hips"), ("spine 001", "spine"), ("spine 002", "chest"), ("spine 003", "upperChest"),
    ("spine 004", "neck"), ("spine 006", "head"), ("shoulder", "*shoulder"), ("upper arm", "*upperArm"),
    ("forearm", "*lowerArm"), ("hand", "*hand"), ("thigh", "*upperLeg"), ("shin", "*lowerLeg"), ("foot", "*foot"),
    ("toe", "*toes"), ("eye", "*eye"), ("jaw", "jaw"),
];
// `センター` (center) is the root the legs hang from in practice; `下半身` (lower body) is left unmapped.
#[rustfmt::skip]
const MMD: Table = &[
    ("センター", "hips"), ("上半身", "spine"), ("上半身2", "chest"), ("上半身3", "upperChest"), ("首", "neck"), ("頭", "head"),
    ("肩", "*shoulder"), ("腕", "*upperArm"), ("ひじ", "*lowerArm"), ("手首", "*hand"), ("足", "*upperLeg"),
    ("ひざ", "*lowerLeg"), ("足首", "*foot"), ("つま先", "*toes"), ("目", "*eye"),
    ("jaw", "jaw"),
];
// Names common enough across tools to count as exact beside the VRM names themselves.
#[rustfmt::skip]
const ALIASES: Table = &[
    ("pelvis", "hips"), ("spine 1", "chest"), ("spine 2", "upperChest"), ("clavicle", "*shoulder"),
    ("arm", "*upperArm"), ("forearm", "*lowerArm"), ("fore arm", "*lowerArm"), ("up leg", "*upperLeg"),
    ("thigh", "*upperLeg"), ("leg", "*lowerLeg"), ("calf", "*lowerLeg"), ("toe base", "*toes"), ("toe", "*toes"),
];

const FINGERS: [&str; 5] = ["Thumb", "Index", "Middle", "Ring", "Little"];
const JOINTS: [[&str; 3]; 2] = [["Metacarpal", "Proximal", "Distal"], ["Proximal", "Intermediate", "Distal"]];

fn table(c: Convention) -> Table {
    match c {
        Convention::Vrm => VRM,
        Convention::Mixamo => MIXAMO,
        Convention::UnityBip => UNITY_BIP,
        Convention::Rigify => RIGIFY,
        Convention::Mmd => MMD,
    }
}

/// The key a convention gives finger `f` (0 thumb .. 4 little), joint `j` (0..3, base first).
fn finger_key(c: Convention, f: usize, j: usize) -> String {
    let word = FINGERS.get(f).copied().unwrap_or_default().to_ascii_lowercase();
    match c {
        Convention::Vrm => format!("{word} {}", j + 1),
        Convention::Mixamo => format!("hand {word} {}", j + 1),
        Convention::UnityBip if j == 0 => format!("finger {f}"),
        Convention::UnityBip => format!("finger {f}{j}"),
        Convention::Rigify if f == 0 => format!("thumb 0{}", j + 1),
        Convention::Rigify => format!("f {} 0{}", if f == 4 { "pinky" } else { &word }, j + 1),
        Convention::Mmd => {
            let name = ["親指", "人指", "中指", "薬指", "小指"].get(f).copied().unwrap_or_default();
            format!("{name}{}", if f == 0 { j } else { j + 1 })
        }
    }
}

fn capitalise(s: &str) -> String {
    let mut cs = s.chars();
    cs.next().map(|c| c.to_ascii_uppercase().to_string() + cs.as_str()).unwrap_or_default()
}

/// The bone a table entry names on `side`.
fn bone_of(base: &str, side: Option<Side>) -> Option<HumanBoneName> {
    let name = match (base.strip_prefix('*'), side) {
        (Some(b), Some(Side::Left)) => format!("left{}", capitalise(b)),
        (Some(b), Some(Side::Right)) => format!("right{}", capitalise(b)),
        (Some(_), None) => return None,
        (None, _) => base.to_owned(),
    };
    HumanBoneName::new(&name).ok()
}

fn lookup(t: Table, p: &Parsed) -> Option<HumanBoneName> {
    t.iter().find(|(k, _)| *k == p.key).and_then(|(_, base)| bone_of(base, p.side))
}

fn strip_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    s.get(..prefix.len()).filter(|h| h.eq_ignore_ascii_case(prefix)).and_then(|_| s.get(prefix.len()..))
}

fn tokens(s: &str) -> Vec<String> {
    let cs: Vec<char> = s.chars().collect();
    let (mut out, mut cur) = (Vec::new(), String::new());
    for (i, &c) in cs.iter().enumerate() {
        if !c.is_ascii_alphanumeric() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            continue;
        }
        let prev = i.checked_sub(1).and_then(|p| cs.get(p)).copied().filter(char::is_ascii_alphanumeric);
        let next_lower = cs.get(i + 1).is_some_and(char::is_ascii_lowercase);
        let split = prev.is_some_and(|p| {
            c.is_ascii_digit() != p.is_ascii_digit()
                || (c.is_ascii_uppercase() && (p.is_ascii_lowercase() || (p.is_ascii_uppercase() && next_lower)))
        });
        if split && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        cur.push(c.to_ascii_lowercase());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn side_of(token: &str) -> Option<Side> {
    match token {
        "left" | "l" => Some(Side::Left),
        "right" | "r" => Some(Side::Right),
        _ => None,
    }
}

fn parse(name: &str) -> Parsed {
    if !name.is_ascii() {
        // `MikuMikuDance`: `左`/`右` prefix, full-width digits.
        let (side, rest) = match (name.strip_prefix('左'), name.strip_prefix('右')) {
            (Some(r), _) => (Some(Side::Left), r),
            (_, Some(r)) => (Some(Side::Right), r),
            _ => (None, name),
        };
        let key = rest
            .chars()
            .map(|c| match c {
                '０'..='９' => char::from_digit(u32::from(c) - u32::from('０'), 10).unwrap_or(c),
                _ => c,
            })
            .collect();
        return Parsed { side, key, marker: Some(Convention::Mmd) };
    }
    let (rest, mut marker) = if let Some(r) = strip_ci(name, "mixamorig") {
        (r.trim_start_matches(|c: char| c.is_ascii_digit()).trim_start_matches(':'), Some(Convention::Mixamo))
    } else if let Some(r) = strip_ci(name, "bip001 ").or_else(|| strip_ci(name, "bip01 ")) {
        (r, Some(Convention::UnityBip))
    } else if let Some(r) = strip_ci(name, "j_bip_").or_else(|| strip_ci(name, "j_adj_")) {
        (r, Some(Convention::Vrm))
    } else {
        (name, None)
    };
    let mut toks = tokens(rest);
    if marker == Some(Convention::Vrm) && toks.first().is_some_and(|t| t == "c") {
        toks.remove(0);
    }
    let mut side = None;
    if toks.len() > 1 {
        if let Some(s) = toks.first().and_then(|t| side_of(t)) {
            (side, _) = (Some(s), toks.remove(0));
        } else if let Some(s) = toks.last().and_then(|t| side_of(t)) {
            (side, _) = (Some(s), toks.pop());
        }
    }
    let key = toks.join(" ");
    if marker.is_none() && matches!(key.as_str(), "spine 001" | "shin") {
        marker = Some(Convention::Rigify);
    }
    Parsed { side, key, marker }
}

/// The bone a parsed name stands for in `conv`'s table (fingers included).
fn in_convention(c: Convention, p: &Parsed) -> Option<HumanBoneName> {
    lookup(table(c), p).or_else(|| {
        let side = match p.side? {
            Side::Left => "left",
            Side::Right => "right",
        };
        (0..5).flat_map(|f| (0..3).map(move |j| (f, j))).find(|&(f, j)| finger_key(c, f, j) == p.key).and_then(
            |(f, j)| {
                let joint = JOINTS.get(usize::from(f > 0))?.get(j)?;
                HumanBoneName::new(&format!("{side}{}{joint}", FINGERS.get(f)?)).ok()
            },
        )
    })
}

/// The bone a parsed name stands for as a VRM bone name or a generic alias.
fn exact(p: &Parsed) -> Option<HumanBoneName> {
    lookup(ALIASES, p).or_else(|| {
        HUMAN_BONES.iter().find_map(|spec| {
            let q = parse(spec.name);
            (q.side == p.side && q.key == p.key).then(|| HumanBoneName::new(spec.name).ok()).flatten()
        })
    })
}

/// What a bone's expected parent is, given the bones assigned so far (the rule of `vrm_profile`).
enum Want {
    Free,
    Blocked,
    Under(HumanBoneName),
}

fn want(found: &Found, bone: HumanBoneName) -> Want {
    let spec = bone.spec();
    let mut up = spec.parent;
    let mut direct = true;
    while let Some(Ok(p)) = up.map(HumanBoneName::new) {
        if found.contains_key(&p) {
            return Want::Under(p);
        }
        if direct && spec.needs_parent {
            return Want::Blocked;
        }
        (direct, up) = (false, p.spec().parent);
    }
    Want::Free
}

type Found = BTreeMap<HumanBoneName, (usize, MatchHow)>;

/// The skinned skeleton with its parent table and rest positions.
struct Cx<'a> {
    scene: &'a IoScene,
    parent: Vec<Option<usize>>,
    world: Vec<Vec3>,
    cand: Vec<usize>,
}

impl<'a> Cx<'a> {
    fn new(scene: &'a IoScene) -> Self {
        let n = scene.nodes.len();
        let mut parent = vec![None; n];
        for (i, node) in scene.nodes.iter().enumerate() {
            for c in &node.children {
                if let Some(slot) = parent.get_mut(c.index()) {
                    *slot = Some(i);
                }
            }
        }
        let local = |i: usize| {
            scene.nodes.get(i).map_or(Mat4::IDENTITY, |nd| {
                let t = &nd.transform;
                Mat4::from_scale_rotation_translation(t.scale, t.rotation, t.translation)
            })
        };
        let world = (0..n)
            .map(|i| {
                let (mut m, mut cur) = (local(i), parent.get(i).copied().flatten());
                for _ in 0..n {
                    let Some(p) = cur else { break };
                    m = local(p) * m;
                    cur = parent.get(p).copied().flatten();
                }
                m.transform_point3(Vec3::ZERO)
            })
            .collect();
        let joints: BTreeSet<usize> =
            scene.skins.iter().flat_map(|s| s.joints.iter().map(|j| j.index())).filter(|&j| j < n).collect();
        let cand = if scene.skins.is_empty() { (0..n).collect() } else { joints.into_iter().collect() };
        Self { scene, parent, world, cand }
    }

    /// Whether `node` may play `bone` given `found`: its nearest humanoid ancestor must be the bone's parent.
    fn fits(&self, found: &Found, bone: HumanBoneName, node: usize) -> bool {
        match want(found, bone) {
            Want::Blocked => false,
            Want::Free => true,
            Want::Under(p) => {
                let mut cur = self.parent.get(node).copied().flatten();
                for _ in 0..self.parent.len() {
                    let Some(n) = cur else { return false };
                    if let Some((b, _)) = found.iter().find(|(_, (v, _))| *v == n) {
                        return *b == p;
                    }
                    cur = self.parent.get(n).copied().flatten();
                }
                false
            }
        }
    }

    /// Every candidate's name match, in node order.
    fn named(&self) -> BTreeMap<HumanBoneName, Vec<(usize, MatchHow)>> {
        let parsed: Vec<(usize, Parsed)> =
            self.cand.iter().filter_map(|&i| Some((i, parse(&self.scene.nodes.get(i)?.name)))).collect();
        let mut votes: BTreeMap<Convention, usize> = BTreeMap::new();
        for (_, p) in &parsed {
            if let Some(c) = p.marker {
                *votes.entry(c).or_default() += 1;
            }
        }
        // The first of the most-voted conventions (the enum order breaks ties).
        let scene_conv = votes.iter().fold(
            None,
            |best: Option<(Convention, usize)>, (&c, &n)| {
                if best.is_none_or(|(_, bn)| n > bn) { Some((c, n)) } else { best }
            },
        );
        let mut out: BTreeMap<HumanBoneName, Vec<(usize, MatchHow)>> = BTreeMap::new();
        for (i, p) in &parsed {
            let hit = scene_conv
                .and_then(|(c, _)| in_convention(c, p).map(|b| (b, MatchHow::Convention(c))))
                .or_else(|| exact(p).map(|b| (b, MatchHow::Exact)));
            if let Some((bone, how)) = hit {
                out.entry(bone).or_default().push((*i, how));
            }
        }
        out
    }

    fn kids(&self, n: usize) -> Vec<usize> {
        let kids = self.scene.nodes.get(n).map(|nd| nd.children.iter().map(|c| c.index()));
        kids.into_iter().flatten().filter(|c| self.cand.binary_search(c).is_ok()).collect()
    }

    fn delta(&self, a: usize, b: usize) -> Vec3 {
        self.world.get(b).copied().unwrap_or_default() - self.world.get(a).copied().unwrap_or_default()
    }

    fn is_down(&self, a: usize, b: usize) -> bool {
        let d = self.delta(a, b);
        d.y < 0.0 && -d.y > d.x.abs()
    }

    fn is_up(&self, a: usize, b: usize) -> bool {
        let d = self.delta(a, b);
        d.y > 0.0 && d.y >= d.x.abs()
    }

    /// Whether `b` is above `a` and further up than it is sideways or forward.
    fn is_axial_up(&self, a: usize, b: usize) -> bool {
        let d = self.delta(a, b);
        self.is_up(a, b) && d.y >= d.z.abs()
    }

    /// The children of `a` for which `pred` holds, left (+X) first and right (-X) last.
    fn extremes(&self, kids: &[usize]) -> Option<(usize, usize)> {
        let x = |n: &usize| self.world.get(*n).map_or(0.0, |p| p.x);
        let by_x = |a: &usize, b: &usize| x(a).total_cmp(&x(b)).then(b.cmp(a));
        Some((*kids.iter().max_by(|a, b| by_x(a, b))?, *kids.iter().min_by(|a, b| by_x(a, b))?)).filter(|(l, r)| l != r)
    }

    /// `start` and its descendants while each has exactly one candidate child.
    fn chain(&self, start: usize) -> Vec<usize> {
        let (mut out, mut cur) = (Vec::new(), start);
        for _ in 0..self.parent.len() {
            out.push(cur);
            match self.kids(cur).as_slice() {
                [only] => cur = *only,
                _ => break,
            }
        }
        out
    }

    fn find_hips(&self) -> Option<usize> {
        let mut stack: Vec<usize> = self
            .cand
            .iter()
            .rev()
            .copied()
            .filter(|&c| self.parent.get(c).copied().flatten().is_none_or(|p| self.cand.binary_search(&p).is_err()))
            .collect();
        let mut seen = BTreeSet::new();
        while let Some(n) = stack.pop() {
            if !seen.insert(n) {
                continue;
            }
            let kids = self.kids(n);
            let downs = kids.iter().filter(|&&k| self.is_down(n, k)).count();
            if downs >= 2 && kids.iter().any(|&k| self.is_up(n, k)) {
                return Some(n);
            }
            stack.extend(kids.into_iter().rev());
        }
        None
    }

    /// The structural guess (see the module docs); empty when no hips can be found.
    fn infer(&self) -> BTreeMap<HumanBoneName, usize> {
        let mut out = BTreeMap::new();
        let mut put = |name: String, n: usize| {
            if let Ok(b) = HumanBoneName::new(&name) {
                out.entry(b).or_insert(n);
            }
        };
        let Some(hips) = self.find_hips() else { return out };
        put("hips".into(), hips);
        let kids = self.kids(hips);
        let downs: Vec<usize> = kids.iter().copied().filter(|&k| self.is_down(hips, k)).collect();
        let ground = self.cand.iter().filter_map(|&c| self.world.get(c)).map(|p| p.y).fold(f32::INFINITY, f32::min);
        if let Some((l, r)) = self.extremes(&downs) {
            for (side, start) in [("left", l), ("right", r)] {
                for (name, n) in self.leg(&self.chain(start), ground, self.y(hips)) {
                    put(format!("{side}{name}"), n);
                }
            }
        }
        let Some(mut cur) = kids.iter().copied().find(|&k| self.is_up(hips, k)) else { return out };
        let mut spine = vec![cur];
        let arms = loop {
            let kids = self.kids(cur);
            let sideways: Vec<usize> =
                kids.iter().copied().filter(|&k| !self.is_up(cur, k) && !self.is_down(cur, k)).collect();
            if let Some((l, r)) = self.extremes(&sideways)
                && self.delta(cur, l).x > 0.0
                && self.delta(cur, r).x < 0.0
            {
                break Some((l, r));
            }
            match kids.iter().copied().find(|&k| self.is_up(cur, k)) {
                Some(n) if spine.len() <= self.parent.len() => {
                    spine.push(n);
                    cur = n;
                }
                _ => break None,
            }
        };
        let Some((l, r)) = arms else { return out };
        put("spine".into(), spine[0]);
        if let Some(&c) = spine.get(1) {
            put("chest".into(), c);
        }
        if let (Some(&u), true) = (spine.last(), spine.len() > 2) {
            put("upperChest".into(), u);
        }
        if let Some(first) = self.kids(cur).into_iter().find(|&k| self.is_up(cur, k)) {
            // The neck is a lone link with the head straight above it; a head's own children (eyes, jaw) sit forward
            // of it or have siblings, so a node that branches is the head itself.
            match self.kids(first).as_slice() {
                &[head] if self.is_axial_up(first, head) => (put("neck".into(), first), put("head".into(), head)),
                _ => ((), put("head".into(), first)),
            };
        }
        for (side, start) in [("left", l), ("right", r)] {
            for (name, n) in self.arm(&self.chain(start)) {
                put(format!("{side}{name}"), n);
            }
        }
        out
    }

    fn y(&self, n: usize) -> f32 {
        self.world.get(n).map_or(0.0, |p| p.y)
    }

    /// The leg bones down `chain`, told apart by rest positions rather than by place in the chain. The hip and knee
    /// stand clear of the ground (`ground` is the lowest joint); the knee is the joint nearest halfway down to the foot,
    /// and must lie between a quarter and three quarters of the way. The foot is the first joint near the ground and
    /// stands under the knee, and the toes point forward from it. Nothing is guessed past a joint the evidence does not
    /// find, so a missing lower leg or foot stays missing.
    fn leg(&self, chain: &[usize], ground: f32, hips_y: f32) -> Vec<(&'static str, usize)> {
        let height = hips_y - ground;
        let near_ground = |n: usize| self.y(n) - ground < 0.25 * height;
        let (high, low) = chain.split_at(chain.iter().position(|&n| near_ground(n)).unwrap_or(chain.len()));
        let Some((&upper, below)) = high.split_first().filter(|_| height > 0.0) else { return Vec::new() };
        let mut out = vec![("UpperLeg", upper)];
        let floor = low.first().map_or(ground, |&f| self.y(f));
        let drop = self.y(upper) - floor;
        let mid = |n: &usize| ((self.y(upper) - self.y(*n)) / drop - 0.5).abs();
        let knees = below.iter().filter(|n| drop > 0.0 && mid(n) <= 0.25);
        let Some(&knee) = knees.min_by(|a, b| mid(a).total_cmp(&mid(b))) else { return out };
        out.push(("LowerLeg", knee));
        // A joint forward of the knee is a toe, whatever its place: the foot is missing then.
        let Some(&foot) = low.first().filter(|&&f| self.delta(knee, f).z.abs() <= 0.15 * (self.y(knee) - self.y(f)))
        else {
            return out;
        };
        out.push(("Foot", foot));
        if let Some(&toes) = low.get(1).filter(|&&t| self.delta(foot, t).z > -self.delta(foot, t).y) {
            out.push(("Toes", toes));
        }
        out
    }

    /// The arm bones along `chain`, told apart by segment lengths: the upper arm and forearm are the only two long
    /// segments and follow each other (at least 0.6 of the longest); a short one before them comes from the shoulder,
    /// short ones after them lead into the fingers. Without exactly that pair the arm is left unresolved.
    fn arm(&self, chain: &[usize]) -> Vec<(&'static str, usize)> {
        let segs: Vec<f32> = chain
            .windows(2)
            .map(|w| w.first().zip(w.last()).map_or(0.0, |(a, b)| self.delta(*a, *b).length()))
            .collect();
        let longest = segs.iter().copied().fold(0.0, f32::max);
        let long: Vec<usize> =
            (0..segs.len()).filter(|&i| segs.get(i).is_some_and(|&s| longest > 0.0 && s >= 0.6 * longest)).collect();
        let &[i, j] = long.as_slice() else { return Vec::new() };
        if j != i + 1 {
            return Vec::new();
        }
        let mut out = Vec::new();
        if let Some(&s) = i.checked_sub(1).and_then(|s| chain.get(s)) {
            out.push(("Shoulder", s));
        }
        for (k, name) in ["UpperArm", "LowerArm", "Hand"].into_iter().enumerate() {
            if let Some(&n) = chain.get(i + k) {
                out.push((name, n));
            }
        }
        out
    }
}

fn node_ix(n: usize) -> NodeIx {
    NodeIx(u32::try_from(n).unwrap_or(u32::MAX))
}

/// Proposes a humanoid for the skinned skeleton of `scene` (all nodes if it has no skin). `overrides` win and are
/// checked like any other assignment.
///
/// # Errors
/// [`MapError`] if a required bone has no node (`humanoid.required_missing`, whose hint names the override table) or
/// an override is not usable (`humanoid.override_invalid`: the node is not in the scene, is already used, or breaks the
/// parent rule), or an assigned node has a zero or negative scale (`humanoid.scale_invalid`).
pub fn auto_map(scene: &IoScene, overrides: &BTreeMap<HumanBoneName, NodeIx>) -> Result<HumanoidProposal, MapError> {
    let cx = Cx::new(scene);
    let mut report = Report::new();
    let reserved: BTreeSet<usize> = overrides.values().map(|n| n.index()).collect();
    let named = cx.named();
    let mut found = Found::new();
    let is_free = |found: &Found, n: usize| !reserved.contains(&n) && !found.values().any(|(v, _)| *v == n);
    let bones = || HUMAN_BONES.iter().filter_map(|s| HumanBoneName::new(s.name).ok());
    let invalid = |bone: HumanBoneName, why: &str| {
        Finding::error("humanoid.override_invalid", format!("the override for {bone} is not usable: {why}"))
            .at(format!("avatar/humanoid/{bone}"))
            .hint("point the override at a node under the bone's parent in the skeleton")
    };
    for bone in bones() {
        if let Some(&n) = overrides.get(&bone) {
            let ix = n.index();
            if ix >= scene.nodes.len() {
                report.push(invalid(bone, "the node is not in the scene"));
            } else if found.values().any(|(v, _)| *v == ix) {
                report.push(invalid(bone, "the node already plays another bone"));
            } else {
                // Whether it sits under the bone's parent bone is checked once inference has settled the parents.
                found.insert(bone, (ix, MatchHow::Override));
            }
            continue;
        }
        let valid: Vec<(usize, MatchHow)> = named
            .get(&bone)
            .into_iter()
            .flatten()
            .filter(|(n, _)| is_free(&found, *n) && cx.fits(&found, bone, *n))
            .copied()
            .collect();
        if let Some(&first) = valid.first() {
            if valid.len() > 1 {
                let nodes: Vec<String> = valid.iter().map(|(n, _)| n.to_string()).collect();
                let msg = format!("nodes {} all look like {bone}; the lowest index was taken", nodes.join(", "));
                report.push(Finding::warning("humanoid.ambiguous", msg).at(format!("avatar/humanoid/{bone}")));
            }
            found.insert(bone, first);
        }
    }
    // Structure fills the gaps when a required bone is missing, and also when an override waits on a parent bone that no
    // name matched (an `upperChest` override over an unnamed chest), so the override is judged with its parents in place.
    let required_gap = bones().any(|b| b.spec().required && !found.contains_key(&b));
    let override_gap = found.iter().any(|(b, (n, how))| *how == MatchHow::Override && !cx.fits(&found, *b, *n));
    if required_gap || override_gap {
        let inferred = cx.infer();
        let mut added = Vec::new();
        let open: Vec<HumanBoneName> =
            bones().filter(|b| !found.contains_key(b) && !overrides.contains_key(b)).collect();
        for bone in open {
            if let Some(&n) = inferred.get(&bone)
                && is_free(&found, n)
                && cx.fits(&found, bone, n)
            {
                found.insert(bone, (n, MatchHow::Inferred));
                added.push(bone.to_string());
            }
        }
        if !added.is_empty() {
            let msg = format!("no bone names matched for {}; the skeleton's structure was used", added.join(", "));
            report.push(Finding::warning("humanoid.inferred", msg).hint("check the result, or set an override"));
        }
    }
    // Overrides are judged first, against the assignments as they stand, so that dropping a name match below cannot
    // make a wrong override look right.
    while let Some(bone) =
        found.iter().find(|(b, (n, how))| *how == MatchHow::Override && !cx.fits(&found, **b, *n)).map(|(b, _)| *b)
    {
        found.remove(&bone);
        report.push(invalid(bone, "the node is not under the bone's parent bone"));
    }
    // Assignments made later can change what an earlier one must sit under: drop what no longer fits.
    while let Some(bone) = found.iter().find(|(b, (n, _))| !cx.fits(&found, **b, *n)).map(|(b, _)| *b) {
        if let Some((_, MatchHow::Override)) = found.remove(&bone) {
            report.push(invalid(bone, "the node is not under the bone's parent bone"));
        }
    }
    check_found(scene, &cx, &found, &mut report);
    let missing_required: Vec<HumanBoneName> =
        bones().filter(|b| b.spec().required && !found.contains_key(b)).collect();
    for bone in &missing_required {
        let msg = format!("no node found for the required bone {bone}");
        let hint = format!("add {bone} to the override table, mapping it to the node that plays it");
        report.push(Finding::error("humanoid.required_missing", msg).at(format!("avatar/humanoid/{bone}")).hint(hint));
    }
    if report.has_errors() {
        return Err(MapError { missing_required, report });
    }
    let matches: Vec<BoneMatch> =
        found.iter().map(|(&bone, &(n, how))| BoneMatch { bone, node: node_ix(n), how }).collect();
    let humanoid = Humanoid { bones: matches.iter().map(|m| (m.bone, m.node)).collect() };
    Ok(HumanoidProposal { humanoid, matches, report })
}

/// The findings about a finished assignment: nodes with a zero or negative scale, and sides that overlap.
fn check_found(scene: &IoScene, cx: &Cx<'_>, found: &Found, report: &mut Report) {
    for (bone, &(n, _)) in found {
        if scene.nodes.get(n).is_some_and(|nd| nd.transform.scale.min_element() <= 0.0) {
            let msg = format!("the node for {bone} has a zero or negative scale");
            let hint = "give the node a positive scale, or map the bone to another node";
            report.push(Finding::error("humanoid.scale_invalid", msg).at(format!("avatar/humanoid/{bone}")).hint(hint));
        }
    }
    // Sides are judged relative to the body's centre, so a rig moved along X is still fine.
    let centre = HumanBoneName::new("hips")
        .ok()
        .and_then(|h| found.get(&h))
        .and_then(|(n, _)| cx.world.get(*n))
        .map_or(0.0, |p| p.x);
    let side_x = |prefix: &str| -> f32 {
        found
            .iter()
            .filter(|(b, _)| b.as_str().starts_with(prefix))
            .filter_map(|(_, (n, _))| cx.world.get(*n))
            .map(|p| p.x - centre)
            .sum()
    };
    if side_x("left") * side_x("right") > 0.0 {
        let msg = "the left and right bones lie on the same side of the body in the rest pose";
        report.push(Finding::warning("humanoid.sides_overlap", msg).hint("check the names, or set overrides"));
    }
}

/// Sets `scene.avatar.humanoid` (creating the avatar if there is none).
pub fn apply(scene: &mut IoScene, proposal: &HumanoidProposal) {
    scene.avatar.get_or_insert_with(Avatar::new).humanoid = proposal.humanoid.clone();
}
