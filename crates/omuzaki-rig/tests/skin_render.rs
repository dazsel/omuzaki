//! The acceptance test of the pose solve and skinning: a humanoid pose is solved, skinned, rendered through the mock
//! renderer, and the picture is compared with one worked out by hand.
//!
//! A little figure (head, torso, an arm in three parts, a leg) is built from boxes, each bound rigidly to one bone.
//! The figure is posed by a normalised VRM pose: the hips shift, the left arm goes up and its forearm bends back to
//! horizontal. Every box then has a hand-computed place (rotations are multiples of 90 degrees, so the boxes stay
//! axis-aligned and each expected rectangle is a few additions). The same picture must come out whatever the bones'
//! own axes are, so the test runs on an aligned rig and on one with randomly tilted bones.

// Test helpers may panic (a panic is a clear test failure); pixel coordinates are tiny, so the float casts are exact.
#![allow(clippy::unwrap_used, clippy::cast_precision_loss)]

mod common;

use common::{Character, character};
use omuzaki_anim_api::{HumanoidBone, HumanoidPose, Pose};
use omuzaki_core::glam::{Mat4, Quat, UVec2, Vec2, Vec3};
use omuzaki_render_api::{
    AlphaEncoding, Background, Camera, Colour, Image, Instance, Material, MeshData, Projection, Quality, Renderer,
    RendererExt, Scene, Tonemap,
};
use omuzaki_render_mock::{MockRenderer, shapes};
use omuzaki_rig::pose::HumanoidRig;
use omuzaki_rig::skeleton::RigSkeleton;
use omuzaki_rig::skin::Skinner;

const SIZE: u32 = 128;
/// Orthographic, 2 m tall: 64 pixels to the metre. The camera looks at the hips from the front.
const PIXELS_PER_METRE: f32 = 64.0;
const EYE_HEIGHT: f32 = 0.95;
/// Pixels around an expected edge that are left unchecked (the rasteriser decides which side of the edge they fall on).
const EDGE_BAND: f32 = 0.75;

/// One box bound to one bone: its centre is `offset` from the bone's rest position, in world axes.
struct Part {
    bone: HumanoidBone,
    offset: Vec3,
    half: Vec3,
    colour: Colour,
}

fn parts() -> Vec<Part> {
    let part = |bone, offset: [f32; 3], half: [f32; 3], rgb: [u8; 3]| Part {
        bone,
        offset: Vec3::from(offset),
        half: Vec3::from(half),
        colour: Colour::rgb(rgb[0], rgb[1], rgb[2]),
    };
    // Boxes along a limb stop short of the joints, so neighbours never overlap in the picture.
    vec![
        part(HumanoidBone::Head, [0.0, 0.1, 0.0], [0.09, 0.1, 0.09], [240, 200, 60]),
        part(HumanoidBone::Spine, [0.0, 0.1, 0.0], [0.1, 0.18, 0.05], [60, 120, 240]),
        part(HumanoidBone::LeftUpperArm, [0.12, 0.0, 0.0], [0.11, 0.025, 0.025], [230, 60, 60]),
        part(HumanoidBone::LeftLowerArm, [0.12, 0.0, 0.0], [0.11, 0.025, 0.025], [60, 220, 90]),
        part(HumanoidBone::LeftHand, [0.05, 0.0, 0.0], [0.04, 0.03, 0.03], [220, 90, 220]),
        part(HumanoidBone::RightUpperArm, [-0.12, 0.0, 0.0], [0.11, 0.025, 0.025], [60, 220, 220]),
        part(HumanoidBone::LeftUpperLeg, [0.0, -0.2, 0.0], [0.04, 0.18, 0.04], [250, 140, 40]),
    ]
}

/// The part's box in the bind pose, rigidly bound to its bone's joint.
fn part_mesh(ch: &Character, part: &Part) -> MeshData {
    let joint = u16::try_from(ch.id(part.bone)).unwrap();
    let mut mesh = shapes::cuboid(part.half);
    let centre = ch.rest_pos[usize::from(joint)] + part.offset;
    mesh.positions.iter_mut().for_each(|p| *p += centre);
    mesh.joints = vec![[joint, 0, 0, 0]; mesh.positions.len()];
    mesh.weights = vec![[1.0, 0.0, 0.0, 0.0]; mesh.positions.len()];
    mesh
}

fn render(ch: &Character, pose: &HumanoidPose) -> Image {
    let rig = HumanoidRig::new(RigSkeleton::new(ch.skeleton.clone()));
    let mut skinner = Skinner::new(rig.rig_skeleton().clone());

    let mut renderer = MockRenderer::new();
    let camera = Camera::look_at(
        Vec3::new(0.0, EYE_HEIGHT, 5.0),
        Vec3::new(0.0, EYE_HEIGHT, 0.0),
        Vec3::Y,
        Projection::Orthographic { height: 2.0, near: 0.1, far: 20.0 },
    );
    let mut scene = Scene::new(camera);
    scene.background = Background::Solid { colour: Colour::BLACK };
    scene.post.tonemap = Tonemap::None;

    // The rig stage of a frame: solve the pose, skin it into the scene's palette, point every instance at the range.
    let mut local = Pose::IDENTITY;
    rig.apply_humanoid(pose, &mut local);
    let range = skinner.push(&local, &mut scene.joints);
    for part in parts() {
        let mesh = renderer.create_mesh(&part_mesh(ch, &part)).unwrap();
        let material = renderer.create_material(&Material::unlit(part.colour)).unwrap();
        let mut instance = Instance::new(mesh, material, Mat4::IDENTITY);
        instance.skin = Some(range);
        scene.instances.push(instance);
    }
    scene.validate().unwrap();

    let quality = Quality { samples: 1, render_scale: 1.0, post_effects: false, ..Quality::offline() };
    renderer.render_image(UVec2::splat(SIZE), AlphaEncoding::Straight, &scene, &quality).unwrap()
}

/// A rectangle in the picture's plane, world metres.
struct Rect {
    min: Vec2,
    max: Vec2,
    colour: Colour,
}

impl Rect {
    fn centred(centre: Vec2, half: Vec2, colour: Colour) -> Self {
        Self { min: centre - half, max: centre + half, colour }
    }

    /// The rectangle in pixel coordinates (y down), grown by `by` pixels on every side (negative shrinks).
    fn in_pixels(&self, by: f32) -> (Vec2, Vec2) {
        let to_px = |p: Vec2| {
            Vec2::new(
                SIZE as f32 / 2.0 + p.x * PIXELS_PER_METRE,
                SIZE as f32 / 2.0 - (p.y - EYE_HEIGHT) * PIXELS_PER_METRE,
            )
        };
        let (a, b) = (to_px(self.min), to_px(self.max));
        (Vec2::new(a.x, b.y) - Vec2::splat(by), Vec2::new(b.x, a.y) + Vec2::splat(by))
    }

    fn contains(&self, pixel_centre: Vec2, grow: f32) -> bool {
        let (lo, hi) = self.in_pixels(grow);
        pixel_centre.cmpge(lo).all() && pixel_centre.cmple(hi).all()
    }
}

/// Compares `image` with `expected` everywhere except a thin band along the edges, and insists the comparison was not
/// vacuous: a good share of every rectangle's area must have been checked.
fn compare(image: &Image, expected: &[Rect], what: &str) {
    let close = |a: Colour, b: Colour| [(a.r, b.r), (a.g, b.g), (a.b, b.b)].iter().all(|&(x, y)| x.abs_diff(y) <= 2);
    let mut inside = 0;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let centre = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
            let got = image.colour(x, y).unwrap();
            if let Some(rect) = expected.iter().find(|r| r.contains(centre, -EDGE_BAND)) {
                assert!(close(got, rect.colour), "{what}: pixel ({x}, {y}) is {got}, expected {}", rect.colour);
                inside += 1;
            } else if !expected.iter().any(|r| r.contains(centre, EDGE_BAND)) {
                assert!(close(got, Colour::BLACK), "{what}: pixel ({x}, {y}) is {got}, expected the background");
            }
        }
    }
    let area: f32 = expected
        .iter()
        .map(|r| {
            let (lo, hi) = r.in_pixels(0.0);
            (hi.x - lo.x) * (hi.y - lo.y)
        })
        .sum();
    // The thinnest boxes are 3 pixels across, of which the edge band leaves about half.
    assert!(inside as f32 > 0.4 * area, "{what}: only {inside} of {area} pixels were compared");
}

/// The 90-degree turn about +Z used below: `(x, y)` becomes `(-y, x)`.
fn quarter_turn(v: Vec2) -> Vec2 {
    Vec2::new(-v.y, v.x)
}

fn rest_rects(ch: &Character, shift: Vec2) -> Vec<Rect> {
    parts()
        .iter()
        .map(|p| {
            let centre = ch.rest_pos[ch.id(p.bone)].truncate() + p.offset.truncate() + shift;
            Rect::centred(centre, p.half.truncate(), p.colour)
        })
        .collect()
}

fn figures() -> [(&'static str, Character); 2] {
    let all = HumanoidBone::ALL;
    [("aligned bones", character(&all, false, 3, 4, 1)), ("tilted bones", character(&all, true, 3, 4, 2))]
}

#[test]
fn the_rest_pose_renders_the_figure_where_it_was_built() {
    for (name, ch) in figures() {
        let image = render(&ch, &HumanoidPose::REST);
        compare(&image, &rest_rects(&ch, Vec2::ZERO), name);
    }
}

#[test]
fn a_waving_pose_renders_where_the_hand_calculation_puts_it() {
    let shift = Vec2::new(0.2, -0.1); // the hips offset
    let mut pose = HumanoidPose::REST;
    pose.hips_offset = shift.extend(0.0);
    pose.set(HumanoidBone::LeftUpperArm, Quat::from_rotation_z(std::f32::consts::FRAC_PI_2)); // arm straight up
    pose.set(HumanoidBone::LeftLowerArm, Quat::from_rotation_z(-std::f32::consts::FRAC_PI_2)); // forearm back to level

    for (name, ch) in figures() {
        // The left shoulder joint, worked out from the proportions (hips 0.95 + spine 0.1 + chest 0.12 + upper chest
        // 0.12 = 1.29; shoulder +0.03, +0.08; upper arm +0.12 along x), then moved by the hips offset.
        let shoulder_rest = Vec2::new(0.15, 1.37);
        assert!((ch.rest_pos[ch.id(HumanoidBone::LeftUpperArm)].truncate() - shoulder_rest).length() < 1e-5);
        let shoulder = shoulder_rest + shift;

        // The upper arm points up: its box is turned a quarter about the shoulder. The elbow is 0.27 m along the
        // arm. The forearm is turned back a quarter, so it points sideways again, from the elbow; the wrist is
        // 0.25 m along it. The hand follows level.
        let elbow = shoulder + quarter_turn(Vec2::new(0.27, 0.0));
        let wrist = elbow + Vec2::new(0.25, 0.0);
        let colour = |bone| parts().into_iter().find(|p| p.bone == bone).unwrap().colour;

        let mut expected = rest_rects(&ch, shift);
        expected.retain(|r| {
            ![HumanoidBone::LeftUpperArm, HumanoidBone::LeftLowerArm, HumanoidBone::LeftHand]
                .iter()
                .any(|&b| colour(b) == r.colour)
        });
        expected.extend([
            Rect::centred(
                shoulder + quarter_turn(Vec2::new(0.12, 0.0)),
                Vec2::new(0.025, 0.11),
                colour(HumanoidBone::LeftUpperArm),
            ),
            Rect::centred(elbow + Vec2::new(0.12, 0.0), Vec2::new(0.11, 0.025), colour(HumanoidBone::LeftLowerArm)),
            Rect::centred(wrist + Vec2::new(0.05, 0.0), Vec2::new(0.04, 0.03), colour(HumanoidBone::LeftHand)),
        ]);

        let image = render(&ch, &pose);
        compare(&image, &expected, name);
    }
}

#[test]
fn the_pose_changes_the_picture() {
    let (_, ch) = &figures()[0];
    let mut pose = HumanoidPose::REST;
    pose.set(HumanoidBone::LeftUpperArm, Quat::from_rotation_z(std::f32::consts::FRAC_PI_2));
    assert_ne!(render(ch, &pose).rgba8, render(ch, &HumanoidPose::REST).rgba8, "a raised arm must show");
}
