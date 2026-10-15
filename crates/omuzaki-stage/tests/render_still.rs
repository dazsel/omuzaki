//! Stills on the mock renderer and the mock character: determinism of the PNG bytes, one still per shot in order, nothing
//! left on the renderer after a call, and the refusals that come before anything is created.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_char_api::{BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, RegistryBuilder};
use omuzaki_char_mock::mock_character;
use omuzaki_core::glam::{UVec2, Vec3};
use omuzaki_face_api::{ExpressionId, ExpressionSet};
use omuzaki_render_api::{AlphaEncoding, Camera, Image, Projection, Quality, RenderError, Renderer};
use omuzaki_render_mock::MockRenderer;
use omuzaki_stage::StageError;
use omuzaki_stage::ops::render::still::{Shot, StillLight, StillSpec, encode_png, render_stills};
use omuzaki_stage::recipe::LightRig;
use omuzaki_stage::scene::frame::frame;
use omuzaki_stage::scene::landmarks::Landmarks;
use omuzaki_stage_api::{CameraDef, Framing};

#[path = "scene/templates.rs"]
mod templates;

const SIDE: u32 = 64;

/// The mock character with the starter expression library, which the mock profile lacks.
fn character() -> BuiltCharacter {
    let mut def = mock_character().expect("the fixture parses");
    def.face.expressions = ExpressionSet::starter();
    RegistryBuilder
        .build(&CharId::new("mock").expect("a valid id"), &def, &BuildOptions::default(), &BuildServices::default())
        .expect("the mock character builds")
}

/// The viewer studio light, as a still takes it.
fn spec() -> StillSpec {
    let rig = LightRig::load("viewer-studio").expect("the light rig loads");
    let lights = rig.to_lights().expect("the rig's lights convert");
    let light = StillLight::new(lights, rig.background(), rig.post(false));
    StillSpec::new(light, UVec2::splat(SIDE), Quality::offline())
}

/// A camera looking at the head from `eye`.
fn camera(eye: Vec3) -> Camera {
    Camera::look_at(
        eye,
        Vec3::new(0.0, 0.78, 0.0),
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 30.0, near: 0.05, far: 10.0 },
    )
}

fn front() -> Shot {
    Shot::new(camera(Vec3::new(0.0, 0.8, 0.95)))
}

fn side() -> Shot {
    Shot::new(camera(Vec3::new(0.95, 0.8, 0.0)))
}

fn back() -> Shot {
    Shot::new(camera(Vec3::new(0.0, 0.8, -0.95)))
}

fn expression(name: &str) -> ExpressionId {
    ExpressionId::new(name).expect("a valid expression id")
}

#[test]
fn the_same_still_gives_the_same_png_bytes() {
    let spec = spec();
    let mut first = MockRenderer::new();
    let mut second = MockRenderer::new();
    let a = render_stills(&mut first, character(), &spec, &[front()]).expect("the still renders");
    let b = render_stills(&mut second, character(), &spec, &[front()]).expect("the still renders");
    let bytes_a = encode_png(&a[0].image).expect("the PNG encodes");
    let bytes_b = encode_png(&b[0].image).expect("the PNG encodes");
    assert_eq!(bytes_a, bytes_b, "two fresh renders of one still give identical PNG bytes");
}

#[test]
fn one_still_per_shot_in_order() {
    let spec = spec();
    let mut renderer = MockRenderer::new();
    let stills = render_stills(&mut renderer, character(), &spec, &[front(), side(), back()]).expect("renders");
    assert_eq!(stills.len(), 3, "one still per shot");
    for still in &stills {
        assert_eq!(still.image.size, spec.size, "every still is the target size");
    }
    assert_ne!(stills[0].image.rgba8, stills[2].image.rgba8, "the front and the back differ");

    // The first still of a longer run is the still of its shot alone: settling is the same, and the order is kept.
    let mut alone_renderer = MockRenderer::new();
    let alone = render_stills(&mut alone_renderer, character(), &spec, &[front()]).expect("renders");
    assert_eq!(stills[0].image, alone[0].image, "the first shot starts from the same settled character");
}

#[test]
fn nothing_is_left_on_the_renderer() {
    let spec = spec();
    let mut renderer = MockRenderer::new();
    let before = renderer.stats();
    render_stills(&mut renderer, character(), &spec, &[front()]).expect("the still renders");
    assert_eq!(renderer.stats(), before, "a successful call releases everything it created");

    let failing = Shot::new(camera(Vec3::new(0.0, 0.8, 0.95))).with_expression(expression("smug"), 1.0);
    render_stills(&mut renderer, character(), &spec, &[front(), failing]).expect_err("the expression is unknown");
    assert_eq!(renderer.stats(), before, "a failing call releases everything it created too");
}

#[test]
fn an_unknown_expression_is_an_error_naming_it() {
    let spec = spec();
    let mut renderer = MockRenderer::new();
    let shot = front().with_expression(expression("smug"), 1.0);
    match render_stills(&mut renderer, character(), &spec, &[shot]) {
        Err(StageError::Face { reason }) => assert!(reason.contains("smug"), "the error names it: {reason}"),
        other => panic!("expected a face error naming the expression, got {other:?}"),
    }
}

#[test]
fn no_shots_or_a_zero_size_is_refused_before_anything_is_created() {
    let spec = spec();
    let mut renderer = MockRenderer::new();
    let before = renderer.stats();

    let none = render_stills(&mut renderer, character(), &spec, &[]);
    assert!(
        matches!(none, Err(StageError::Render { source: RenderError::Invalid { .. }, .. })),
        "no shots is refused: {none:?}"
    );

    let empty = StillSpec::new(spec.light.clone(), UVec2::new(0, SIDE), Quality::offline());
    let zero = render_stills(&mut renderer, character(), &empty, &[front()]);
    assert!(
        matches!(zero, Err(StageError::Render { source: RenderError::Invalid { .. }, .. })),
        "a zero side is refused: {zero:?}"
    );
    assert_eq!(renderer.stats(), before, "nothing was created");
}

#[test]
fn an_expression_changes_the_face() {
    let spec = spec();
    let mut renderer = MockRenderer::new();
    let neutral = render_stills(&mut renderer, character(), &spec, &[front()]).expect("renders");
    let mut renderer = MockRenderer::new();
    let smiling = front().with_expression(expression("happy"), 1.0);
    let happy = render_stills(&mut renderer, character(), &spec, &[smiling]).expect("renders");
    assert_ne!(neutral[0].image.rgba8, happy[0].image.rgba8, "a full happy changes the picture");
}

#[test]
fn png_bytes_decode_to_the_image() {
    let spec = spec();
    let mut renderer = MockRenderer::new();
    let stills = render_stills(&mut renderer, character(), &spec, &[front()]).expect("renders");
    let image = &stills[0].image;
    let bytes = encode_png(image).expect("the PNG encodes");
    let decoded = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png).expect("it decodes").to_rgba8();
    assert_eq!((decoded.width(), decoded.height()), (image.size.x, image.size.y));
    assert_eq!(decoded.into_raw(), image.rgba8, "the decoded pixels are the image's pixels");
}

#[test]
fn a_short_image_is_refused() {
    let image = Image { size: UVec2::splat(4), alpha: AlphaEncoding::default(), rgba8: vec![0; 4 * 4 * 4 - 1] };
    assert!(
        matches!(encode_png(&image), Err(StageError::Render { source: RenderError::Invalid { .. }, .. })),
        "one byte short is refused"
    );
}

/// The pixels a PNG of `image` decodes to, as RGBA8.
fn decoded(image: &Image) -> Vec<u8> {
    let bytes = encode_png(image).expect("the PNG encodes");
    image::load_from_memory_with_format(&bytes, image::ImageFormat::Png).expect("it is a PNG").to_rgba8().into_raw()
}

#[test]
fn a_premultiplied_image_is_written_straight() {
    // Half-transparent red: premultiplied, its red is 64 of 128; straight, it is 128.
    let image = Image {
        size: UVec2::new(3, 1),
        alpha: AlphaEncoding::Premultiplied,
        rgba8: vec![64, 0, 0, 128, 0, 0, 0, 0, 200, 100, 50, 255],
    };
    assert_eq!(decoded(&image), vec![128, 0, 0, 128, 0, 0, 0, 0, 200, 100, 50, 255]);
}

#[test]
fn a_straight_image_is_written_as_it_is() {
    let rgba8 = vec![64, 0, 0, 128, 10, 20, 30, 255];
    let image = Image { size: UVec2::new(2, 1), alpha: AlphaEncoding::Straight, rgba8: rgba8.clone() };
    assert_eq!(decoded(&image), rgba8);
}

#[test]
fn a_shot_renders_the_same_pixels_at_any_position() {
    // The mock's hair simulates in time, so a shot that came after more steps would differ if the shots advanced it.
    let spec = spec();
    let mut renderer = MockRenderer::new();
    let stills = render_stills(&mut renderer, character(), &spec, &[front(), side(), front()]).expect("renders");
    assert_eq!(stills[0].image, stills[2].image, "the same shot at positions 0 and 2 is identical");
}

/// #1978: a still shows the character in its base pose, the pose its landmarks are measured in, so the hands preset
/// puts the hands, which hang out at the sides in a template's bind pose, in the middle of the frame. In the T-pose
/// the hands are elsewhere and the middle band at the sides is empty background.
#[test]
fn the_hands_shot_shows_the_hands_at_the_sides_of_the_middle_band() {
    let cell = UVec2::new(240, 135);
    let aspect = 16.0 / 9.0;
    for template in templates::TEMPLATES {
        let built = templates::build(template);
        let landmarks = Landmarks::measure(&built).expect("the template measures");
        let camera = frame(Framing::Hands, &landmarks, &CameraDef::preset(Framing::Hands), aspect).expect("it frames");
        let mut still_spec = spec();
        still_spec.size = cell;
        let stills =
            render_stills(&mut MockRenderer::new(), built, &still_spec, &[Shot::new(camera)]).expect("it renders");
        let image = &stills[0].image;
        let pixel = |x: u32, y: u32| {
            let at = ((y * cell.x + x) * 4) as usize;
            [image.rgba8[at], image.rgba8[at + 1], image.rgba8[at + 2]]
        };
        let background = pixel(0, 0);
        // The hand tips stand at about 0.15 and 0.85 of the width, at the height of the middle of the frame.
        let band = (cell.y * 2 / 5)..(cell.y * 3 / 5);
        for (side, columns) in [("left", 0..cell.x / 4), ("right", cell.x * 3 / 4..cell.x)] {
            let skin = band
                .clone()
                .flat_map(|y| columns.clone().map(move |x| (x, y)))
                .filter(|&(x, y)| pixel(x, y) != background)
                .count();
            assert!(skin > 20, "{template}: {skin} pixels of the {side} hand in the middle band");
        }
    }
}
