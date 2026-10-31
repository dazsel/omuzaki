//! Conformance checks: the behaviours of the render contract that no type signature can express.
//!
//! The mock passes them in its own tests; the real GPU renderer (`omuzaki-render`) runs the same [`run_all`] against
//! itself, which is what keeps "tested against the mock" and "works on the GPU" the same claim. Checks use coarse
//! pixel probes with a tolerance (and exact equality only where [`Capabilities::bit_exact`](omuzaki_render_api::Capabilities::bit_exact) says so), so they hold for
//! any correct renderer, not just this one.

use std::f32::consts::PI;
use std::fmt;

use omuzaki_core::glam::{Mat4, UVec2, Vec2, Vec3};
use omuzaki_core::id::{Slug, SlugError};
use omuzaki_render_api::{
    AlphaEncoding, AlphaMode, BackdropFit, Background, Camera, Colour, EdgeLines, EnvironmentDesc, EnvironmentUse,
    ExtraAttribute, Filter, Fog, FrameReport, Image, Instance, JointRange, Light, Mapping, Material, MaterialId,
    MeshData, MeshId, MeshOptions, MeshUsage, MorphRange, MorphSet, Outline, Overlay, PaintTarget, ParamValue,
    PixelRect, ProgramParams, Projection, Quality, RenderError, Renderer, RendererExt, Rim, Sampler, Scene, Shading,
    Smear, TargetDesc, TextureDesc, Tonemap, Toon, UvTransform, gradient_at, limits,
};

use crate::shapes;

/// Why a check failed, in words a renderer author can act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure(pub String);

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Failure {}

impl From<SlugError> for Failure {
    fn from(e: SlugError) -> Self {
        Self(format!("bad test name: {e}"))
    }
}

impl From<RenderError> for Failure {
    fn from(e: RenderError) -> Self {
        Self(format!("unexpected renderer error: {e}"))
    }
}

/// One named check.
pub type Check = fn(&mut dyn Renderer) -> Result<(), Failure>;

type Outcome = Result<(), Failure>;

macro_rules! ensure {
    ($cond:expr, $($msg:tt)+) => {
        // `!(a <= b)` is on purpose where a float is compared: a NaN must fail the check, not slip past it.
        #[allow(clippy::neg_cmp_op_on_partial_ord)]
        if !$cond {
            return Err(Failure(format!($($msg)+)));
        }
    };
}

mod particles;

/// Every check, by name.
pub const CHECKS: &[(&str, Check)] = &[
    ("stale handles are errors", stale_handles_are_errors),
    ("textures outlive their materials", textures_outlive_their_materials),
    ("stats follow creation and release", stats_follow_creation_and_release),
    ("invalid input is an error", invalid_input_is_an_error),
    ("the report follows the frame graph", the_report_follows_the_frame_graph),
    ("offline frames ignore history", offline_frames_ignore_history),
    ("background and alpha encoding", background_and_alpha_encoding),
    ("painted textures show through overlays", painted_textures_show_through_overlays),
    ("a surface fx front recolours or cuts what it has passed", surface_fx_fronts_recolour_or_cut),
    ("a surface fx under the paint leaves the overlays on top", surface_fx_under_paint_keeps_the_overlays),
    ("outlines hug the silhouette", outlines_hug_the_silhouette),
    ("an open double-sided sheet is outlined from both sides", open_sheets_are_outlined_from_both_sides),
    ("edge lines draw on the near side of every depth jump", edge_lines_draw_on_the_near_side_of_depth_jumps),
    ("blended instances sort back to front", blended_instances_sort_back_to_front),
    ("skinned instances follow their joints", skinned_instances_follow_their_joints),
    ("a smear pulls the trailing side back", a_smear_pulls_the_trailing_side_back),
    ("mirrored instances render like the original", mirrored_instances_render_like_the_original),
    ("more samples smooth edges", more_samples_smooth_edges),
    ("unknown shading programs fall back to toon", unknown_shading_programs_fall_back_to_toon),
    ("a point light falls off like three.js", point_lights_fall_off_like_threejs),
    ("orthographic rims use the view axis", orthographic_rims_use_the_view_axis),
    ("a spot light's cone has three.js edges", spot_cones_have_threejs_edges),
    ("fog fades linearly between near and far", fog_fades_linearly_between_near_and_far),
    ("lighting parameters are validated", lighting_parameters_are_validated),
    ("a sky lights surfaces by irradiance", a_sky_lights_surfaces_by_irradiance),
    ("an HDRI is equirectangular and rotates", an_hdri_is_equirectangular_and_rotates),
    ("the toon ramp quantises the sky's light", the_toon_ramp_quantises_the_skys_light),
    ("environments are resources", environments_are_resources),
    ("the environment backdrop shows the sky", the_environment_backdrop_shows_the_sky),
    ("gradient and texture backdrops show as authored", pictures_behind_show_as_authored),
    ("a UV transform picks the atlas cell", uv_transform_atlas),
    // Particles (#810, particles.md D2 to D7), in `conformance/particles.rs`.
    ("particles_none_without_batches", particles::particles_none_without_batches),
    ("particle_ballistic", particles::particle_ballistic),
    ("particle_gravity", particles::particle_gravity),
    ("particle_drag", particles::particle_drag),
    ("particle_unborn_and_dead", particles::particle_unborn_and_dead),
    ("particle_density", particles::particle_density),
    ("particles_deterministic", particles::particles_deterministic),
    ("particles_no_history", particles::particles_no_history),
    ("particles_premultiplied", particles::particles_premultiplied),
    ("particle_depth", particles::particle_depth),
    ("particle_before_blended", particles::particle_before_blended),
    ("particle_fog", particles::particle_fog),
    ("particle_near_fade", particles::particle_near_fade),
    ("particle_toon", particles::particle_toon),
    ("particle_atlas_cell", particles::particle_atlas_cell),
    ("particle_handles", particles::particle_handles),
    ("particle_limits", particles::particle_limits),
];

/// The checks for meshes rewritten in place: a renderer whose [`Capabilities::dynamic_meshes`] is true must pass them,
/// recolouring (`update_colours`, static meshes included) among them. [`run_capable`] runs them when it is.
pub const DYNAMIC_CHECKS: &[(&str, Check)] = &[
    ("updating vertices matches a fresh mesh", updating_vertices_matches_a_fresh_mesh),
    ("updating a static mesh is invalid", updating_a_static_mesh_is_invalid),
    ("recolouring matches a fresh mesh", recolouring_matches_a_fresh_mesh),
    ("recolouring input is validated", recolouring_input_is_validated),
];

/// The checks for sparse morph targets: a renderer whose [`Capabilities::morph_targets`] is true must pass them.
/// [`run_capable`] runs them when it is. The expected pictures are the hand-computed composition `skin(morph(base))`.
pub const MORPH_CHECKS: &[(&str, Check)] = &[
    ("zero morph weights match the base mesh", zero_morph_weights_match_the_base_mesh),
    ("a morph weight adds its delta", a_morph_weight_adds_its_delta),
    ("instances of one mesh morph independently", instances_of_one_mesh_morph_independently),
    ("morphing happens before skinning", morphing_happens_before_skinning),
    ("morph input is validated", morph_input_is_validated),
];

/// Runs [`DYNAMIC_CHECKS`] if `renderer` claims `dynamic_meshes` and [`MORPH_CHECKS`] if it claims `morph_targets`;
/// a renderer that claims neither passes trivially. It must be fresh and is left as it was found.
///
/// # Errors
/// The first [`Failure`], prefixed with the check's name.
pub fn run_capable(renderer: &mut dyn Renderer) -> Outcome {
    let before = counts(renderer);
    let caps = renderer.capabilities();
    let (dynamic, morph) = (caps.dynamic_meshes, caps.morph_targets);
    let claimed = [(dynamic, DYNAMIC_CHECKS), (morph, MORPH_CHECKS)];
    for (name, check) in claimed.iter().filter(|(on, _)| *on).flat_map(|(_, checks)| checks.iter()) {
        check(renderer).map_err(|f| Failure(format!("{name}: {f}")))?;
    }
    let after = counts(renderer);
    ensure!(before == after, "resources leaked: {before:?} before, {after:?} after");
    Ok(())
}

/// Runs every check on `renderer`, which must be fresh (nothing else uploaded) and is left as it was found.
///
/// # Errors
/// The first [`Failure`], prefixed with the check's name.
pub fn run_all(renderer: &mut dyn Renderer) -> Outcome {
    let before = counts(renderer);
    for (name, check) in CHECKS {
        check(renderer).map_err(|f| Failure(format!("{name}: {f}")))?;
    }
    let after = counts(renderer);
    ensure!(
        before == after,
        "resources leaked: {before:?} before, {after:?} after (every check must release what it makes)"
    );
    Ok(())
}

/// Live counts only: `gpu_bytes` may lag a release by the frames still in flight.
fn counts(r: &dyn Renderer) -> [u32; 6] {
    let s = r.stats();
    [s.meshes, s.textures, s.materials, s.targets, s.environments, s.particle_systems]
}

// ---- fixtures ---------------------------------------------------------------------------------------------------

const SIZE: UVec2 = UVec2::new(64, 64);
const CENTRE: (u32, u32) = (32, 32);

/// A camera on +Z looking at the origin; orthographic, 2 m tall, so 1 world unit is 32 pixels and the origin is
/// pixel (32, 32).
fn camera() -> Camera {
    Camera::look_at(
        Vec3::new(0.0, 0.0, 5.0),
        Vec3::ZERO,
        Vec3::Y,
        Projection::Orthographic { height: 2.0, near: 0.1, far: 20.0 },
    )
}

/// An empty scene with exact colours (no tone curve) on a flat background.
fn scene(background: Colour) -> Scene {
    let mut s = Scene::new(camera());
    s.background = Background::Solid { colour: background };
    s.post.tonemap = Tonemap::None;
    s
}

fn headlight(intensity: f32) -> Light {
    Light::Directional { to_light: Vec3::Z, colour: Colour::WHITE, intensity, casts_shadow: false }
}

/// No effects, one sample: pixels are exactly covered or not.
fn plain() -> Quality {
    Quality { samples: 1, render_scale: 1.0, post_effects: false.into(), ..Quality::offline() }
}

fn draw(r: &mut dyn Renderer, scene: &Scene, quality: &Quality) -> Result<Image, Failure> {
    Ok(r.render_image(SIZE, AlphaEncoding::Straight, scene, quality)?)
}

fn tolerance(r: &dyn Renderer) -> u8 {
    if r.capabilities().bit_exact { 0 } else { 2 }
}

fn close(a: Colour, b: Colour, tol: u8) -> bool {
    [(a.r, b.r), (a.g, b.g), (a.b, b.b), (a.a, b.a)].iter().all(|&(x, y)| x.abs_diff(y) <= tol)
}

fn pixel(img: &Image, (x, y): (u32, u32)) -> Result<Colour, Failure> {
    img.colour(x, y).ok_or_else(|| Failure(format!("pixel ({x}, {y}) is outside the image")))
}

fn expect_pixel(img: &Image, at: (u32, u32), want: Colour, tol: u8, what: &str) -> Outcome {
    let got = pixel(img, at)?;
    ensure!(close(got, want, tol), "{what}: pixel {at:?} is {got}, expected {want} (tolerance {tol})");
    Ok(())
}

fn same_image(a: &Image, b: &Image, tol: u8, what: &str) -> Outcome {
    ensure!(a.size == b.size, "{what}: sizes differ");
    let worst = a.rgba8.iter().zip(&b.rgba8).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
    ensure!(worst <= tol, "{what}: images differ by up to {worst} levels (tolerance {tol})");
    Ok(())
}

fn unlit(r: &mut dyn Renderer, colour: Colour) -> Result<MaterialId, Failure> {
    Ok(r.create_material(&Material::unlit(colour))?)
}

fn expect_err<T>(result: Result<T, RenderError>, what: &str, is: impl Fn(&RenderError) -> bool) -> Outcome {
    match result {
        Err(e) if is(&e) => Ok(()),
        Err(e) => Err(Failure(format!("{what}: failed with the wrong error: {e:?}"))),
        Ok(_) => Err(Failure(format!("{what}: expected an error but it succeeded"))),
    }
}

fn is_unknown(e: &RenderError) -> bool {
    matches!(e, RenderError::UnknownHandle { .. })
}

fn is_invalid(e: &RenderError) -> bool {
    matches!(e, RenderError::Invalid { .. })
}

// ---- resource lifetimes -----------------------------------------------------------------------------------------

fn stale_handles_are_errors(r: &mut dyn Renderer) -> Outcome {
    let mesh = r.create_mesh(&shapes::quad(Vec2::splat(0.5)))?;
    let material = unlit(r, Colour::WHITE)?;
    r.release(mesh.into())?;
    expect_err(r.release(mesh.into()), "releasing twice", is_unknown)?;

    let mut s = scene(Colour::BLACK);
    s.instances.push(Instance::new(mesh, material, Mat4::IDENTITY));
    expect_err(r.render_image(SIZE, AlphaEncoding::Straight, &s, &plain()), "drawing a released mesh", is_unknown)?;

    // The slot is reused by a new mesh; the old handle must still miss rather than draw the newcomer.
    let newcomer = r.create_mesh(&shapes::quad(Vec2::splat(0.5)))?;
    expect_err(
        r.render_image(SIZE, AlphaEncoding::Straight, &s, &plain()),
        "a stale handle after slot reuse",
        is_unknown,
    )?;
    r.release(newcomer.into())?;
    r.release(material.into())?;
    Ok(())
}

fn textures_outlive_their_materials(r: &mut dyn Renderer) -> Outcome {
    let texture = r.create_texture(&TextureDesc::colour(UVec2::new(2, 2)), None)?;
    let mut material = Material::unlit(Colour::WHITE);
    material.base_texture = Some(texture);
    let id = r.create_material(&material)?;
    expect_err(r.release(texture.into()), "releasing a texture a material reads", |e| {
        matches!(e, RenderError::InUse { .. })
    })?;

    r.update_material(id, &Material::unlit(Colour::WHITE))?;
    r.release(texture.into())?;
    material.base_texture = Some(texture);
    expect_err(r.create_material(&material), "a material naming a released texture", is_unknown)?;
    r.release(id.into())?;
    Ok(())
}

fn stats_follow_creation_and_release(r: &mut dyn Renderer) -> Outcome {
    let before = counts(r);
    let mesh = r.create_mesh(&shapes::cuboid(Vec3::ONE))?;
    let texture = r.create_texture(&TextureDesc::colour(UVec2::new(4, 4)), None)?;
    let material = unlit(r, Colour::WHITE)?;
    let target = r.create_target(&TargetDesc { size: SIZE, alpha: AlphaEncoding::Straight })?;
    let sky = EnvironmentDesc::Sky { zenith: Colour::WHITE, horizon: Colour::WHITE, ground: Colour::BLACK };
    let environment = r.create_environment(&sky)?;
    let during = counts(r);
    // One of each kind but particle systems, whose counts `particle_handles` checks.
    let mut expected = before.map(|n| n + 1);
    expected[5] = before[5];
    ensure!(during == expected, "counts after creating one of each were {during:?}, expected {expected:?}");
    ensure!(r.stats().gpu_bytes > 0, "gpu_bytes should reflect what was uploaded");
    for resource in [mesh.into(), texture.into(), material.into(), target.into(), environment.into()] {
        r.release(resource)?;
    }
    ensure!(counts(r) == before, "counts after releasing everything: {:?}, expected {before:?}", counts(r));
    Ok(())
}

fn invalid_input_is_an_error(r: &mut dyn Renderer) -> Outcome {
    let mut bad_mesh = shapes::quad(Vec2::ONE);
    bad_mesh.indices[5] = 99;
    expect_err(r.create_mesh(&bad_mesh), "an index past the last vertex", is_invalid)?;
    expect_err(r.create_mesh(&MeshData::default()), "an empty mesh", is_invalid)?;
    let mut bad_extra = shapes::quad(Vec2::ONE);
    bad_extra.extra.push(ExtraAttribute { name: Slug::new("strand")?, components: 2, data: vec![0.0; 3] });
    expect_err(r.create_mesh(&bad_extra), "an extra attribute with the wrong length", is_invalid)?;
    expect_err(
        r.create_texture(&TextureDesc::colour(UVec2::new(2, 2)), Some(&[0; 15])),
        "a short pixel buffer",
        is_invalid,
    )?;
    expect_err(r.create_texture(&TextureDesc::colour(UVec2::new(0, 2)), None), "a zero-sized texture", is_invalid)?;
    expect_err(
        r.create_target(&TargetDesc { size: UVec2::new(0, 4), alpha: AlphaEncoding::Straight }),
        "a zero-sized target",
        is_invalid,
    )?;
    let huge = r.capabilities().max_texture_size + 1;
    expect_err(
        r.create_target(&TargetDesc { size: UVec2::new(huge, 4), alpha: AlphaEncoding::Straight }),
        "a target past max_texture_size",
        |e| matches!(e, RenderError::LimitExceeded { .. }),
    )?;

    let static_texture = r.create_texture(&TextureDesc::colour(UVec2::new(2, 2)), None)?;
    let rect = PixelRect::full(UVec2::new(2, 2));
    expect_err(r.update_texture(static_texture, rect, &[0; 16]), "painting a static texture", is_invalid)?;
    r.release(static_texture.into())?;

    let mesh = r.create_mesh(&shapes::quad(Vec2::splat(0.5)))?;
    let material = unlit(r, Colour::WHITE)?;
    let mut s = scene(Colour::BLACK);
    s.lights = vec![headlight(1.0); limits::MAX_LIGHTS + 1];
    expect_err(r.render_image(SIZE, AlphaEncoding::Straight, &s, &plain()), "too many lights", |e| {
        matches!(e, RenderError::LimitExceeded { .. })
    })?;
    let mut s = scene(Colour::BLACK);
    let mut inst = Instance::new(mesh, material, Mat4::IDENTITY);
    inst.skin = Some(JointRange { start: 0, len: 4 });
    s.instances.push(inst);
    expect_err(
        r.render_image(SIZE, AlphaEncoding::Straight, &s, &plain()),
        "a skin range past the joints",
        is_invalid,
    )?;
    s.instances.clear();
    expect_err(
        r.render_image(SIZE, AlphaEncoding::Straight, &s, &Quality { samples: 3, ..plain() }),
        "3 samples",
        is_invalid,
    )?;
    r.release(mesh.into())?;
    r.release(material.into())?;
    Ok(())
}

// ---- frames -----------------------------------------------------------------------------------------------------

fn the_report_follows_the_frame_graph(r: &mut dyn Renderer) -> Outcome {
    let mesh = r.create_mesh(&shapes::quad(Vec2::splat(0.5)))?;
    let material = unlit(r, Colour::WHITE)?;
    let face = r.create_texture(&TextureDesc::colour(UVec2::new(2, 2)).dynamic(), None)?;
    let target = r.create_target(&TargetDesc { size: SIZE, alpha: AlphaEncoding::Straight })?;
    let mut s = scene(Colour::BLACK);
    s.frame.index = 42;
    s.instances.push(Instance::new(mesh, material, Mat4::IDENTITY));

    let check = |report: &FrameReport, r: &dyn Renderer| -> Outcome {
        ensure!(report.frame == 42, "the report names frame {}, not the scene's 42", report.frame);
        ensure!(
            report.render_size.x >= SIZE.x && report.render_size.y >= SIZE.y,
            "render_size {} is smaller than the target at scale 1",
            report.render_size
        );
        for (pass, _) in report.cpu.iter() {
            ensure!(r.capabilities().supports(pass), "reported {} but capabilities() doesn't list it", pass.name());
        }
        for pass in [omuzaki_render_api::Pass::Opaque, omuzaki_render_api::Pass::Resolve] {
            ensure!(report.cpu.get(pass).is_some(), "the {} pass must always run", pass.name());
        }
        match (r.capabilities().gpu_timings, report.gpu) {
            (false, Some(_)) => {
                return Err(Failure("gpu timings reported but capabilities().gpu_timings is false".into()));
            }
            (true, Some(gpu)) if gpu.frame > report.frame => {
                return Err(Failure(format!("gpu timings are from the future (frame {})", gpu.frame)));
            }
            _ => {}
        }
        Ok(())
    };

    let first = r.render(target, &s, &plain())?;
    check(&first, r)?;
    ensure!(
        first.cpu.get(omuzaki_render_api::Pass::PaintTextures).is_none(),
        "nothing was painted, so PaintTextures must not run"
    );

    r.update_texture(face, PixelRect::full(UVec2::new(2, 2)), &[255; 16])?;
    let after_paint = r.render(target, &s, &plain())?;
    ensure!(
        after_paint.cpu.get(omuzaki_render_api::Pass::PaintTextures).is_some(),
        "a paint since the last frame must show as a PaintTextures pass"
    );
    let next = r.render(target, &s, &plain())?;
    ensure!(
        next.cpu.get(omuzaki_render_api::Pass::PaintTextures).is_none(),
        "PaintTextures must not repeat without a new paint"
    );

    for resource in [mesh.into(), material.into(), face.into(), target.into()] {
        r.release(resource)?;
    }
    Ok(())
}

fn offline_frames_ignore_history(r: &mut dyn Renderer) -> Outcome {
    let mesh = r.create_mesh(&shapes::cuboid(Vec3::splat(0.4)))?;
    let red = r.create_material(&Material::toon(Colour::rgb(200, 40, 40)))?;
    let blue = r.create_material(&Material::toon(Colour::rgb(40, 40, 200)))?;
    let draw_with = |r: &mut dyn Renderer, material: MaterialId, turn: f32, frame: u64| -> Result<Image, Failure> {
        let mut s = scene(Colour::rgb(20, 20, 30));
        s.frame.index = frame;
        s.lights = vec![headlight(2.0)];
        s.instances.push(Instance::new(mesh, material, Mat4::from_rotation_y(turn)));
        draw(r, &s, &Quality::offline())
    };
    let first = draw_with(r, red, 0.4, 1)?;
    draw_with(r, blue, 1.1, 2)?;
    draw_with(r, red, 0.9, 3)?;
    let again = draw_with(r, red, 0.4, 1)?;
    same_image(&first, &again, tolerance(r), "the same scene after other frames")?;
    for resource in [mesh.into(), red.into(), blue.into()] {
        r.release(resource)?;
    }
    Ok(())
}

fn background_and_alpha_encoding(r: &mut dyn Renderer) -> Outcome {
    let tol = tolerance(r).max(1);
    let size = UVec2::new(4, 4);
    let render = |r: &mut dyn Renderer, background: Background, alpha: AlphaEncoding| {
        let mut s = scene(Colour::BLACK);
        s.background = background;
        r.render_image(size, alpha, &s, &plain())
    };
    let solid = Colour::rgb(0x33, 0x66, 0x99);
    let img = render(r, Background::Solid { colour: solid }, AlphaEncoding::Straight)?;
    expect_pixel(&img, (1, 1), solid, tol, "a solid opaque background")?;

    let img = render(r, Background::Transparent, AlphaEncoding::Straight)?;
    ensure!(pixel(&img, (1, 1))?.a == 0, "a transparent background must have alpha 0, got {}", pixel(&img, (1, 1))?);

    let half = Colour::rgba(0xff, 0x80, 0x00, 0x80);
    let straight = render(r, Background::Solid { colour: half }, AlphaEncoding::Straight)?;
    expect_pixel(&straight, (1, 1), half, tol, "straight alpha keeps the colour as authored")?;
    let premultiplied = render(r, Background::Solid { colour: half }, AlphaEncoding::Premultiplied)?;
    ensure!(premultiplied.alpha == AlphaEncoding::Premultiplied, "the image must say it is premultiplied");
    // Premultiplied multiplies the stored (encoded) numbers by alpha: 0xff * 128/255 = 128, 0x80 * 128/255 = 64.
    expect_pixel(&premultiplied, (1, 1), Colour::rgba(0x80, 0x40, 0x00, 0x80), tol + 1, "premultiplied alpha")?;
    Ok(())
}

/// A scene with one unlit quad (half-size 0.5) carrying `overlay`, drawn on a black background.
fn overlay_scene(r: &mut dyn Renderer, overlay: Option<Overlay>) -> Result<(Scene, MeshId, MaterialId), Failure> {
    let mesh = r.create_mesh(&shapes::quad(Vec2::splat(0.5)))?;
    let mut material = Material::unlit(Colour::WHITE);
    material.overlays.extend(overlay);
    let material = r.create_material(&material)?;
    let mut s = scene(Colour::BLACK);
    s.instances.push(Instance::new(mesh, material, Mat4::IDENTITY));
    Ok((s, mesh, material))
}

/// A surface fx swept along +X with no noise: behind the front (the quad's left half at amount 0.5) a tint turns the
/// surface its colour and a dissolve cuts it away (with its outline), and at amount 0 nothing changes.
fn surface_fx_fronts_recolour_or_cut(r: &mut dyn Renderer) -> Outcome {
    use omuzaki_render_api::{SurfaceFx, SurfaceFxKind};
    let tol = tolerance(r).max(1);
    let (s, mesh, material) = overlay_scene(r, None)?;
    let left = (CENTRE.0 - 12, CENTRE.1); // world x = -0.375
    let right = (CENTRE.0 + 12, CENTRE.1); // world x = 0.375
    let front = |kind: SurfaceFxKind, amount: f32| SurfaceFx {
        kind,
        amount,
        colour: Colour::rgb(255, 0, 0),
        edge: Colour::WHITE,
        edge_width: 0.0,
        glow: 0.0,
        sweep: Vec3::X,
        sweep_range: Vec2::new(-0.5, 0.5),
        sweep_mix: 1.0,
        ..SurfaceFx::default()
    };
    let with = |fx: SurfaceFx| Material { fx: Some(fx), ..Material::unlit(Colour::WHITE) };

    r.update_material(material, &with(front(SurfaceFxKind::Tint, 0.5)))?;
    let img = draw(r, &s, &plain())?;
    expect_pixel(&img, left, Colour::rgb(255, 0, 0), tol, "a tint recolours what its front has passed")?;
    expect_pixel(&img, right, Colour::WHITE, tol, "ahead of the front the surface is as it was")?;

    r.update_material(material, &with(front(SurfaceFxKind::Dissolve, 0.5)))?;
    let img = draw(r, &s, &plain())?;
    expect_pixel(&img, left, Colour::BLACK, tol, "a dissolve cuts away what its front has passed")?;
    expect_pixel(&img, right, Colour::WHITE, tol, "ahead of a dissolve the surface stays")?;

    r.update_material(material, &with(front(SurfaceFxKind::Dissolve, 0.0)))?;
    let img = draw(r, &s, &plain())?;
    expect_pixel(&img, left, Colour::WHITE, tol, "a front at amount 0 changes nothing")?;

    expect_err(
        r.update_material(material, &with(SurfaceFx { amount: 3.0, ..SurfaceFx::default() })),
        "a surface fx amount out of range",
        |e| matches!(e, RenderError::Invalid { .. }),
    )?;
    for resource in [mesh.into(), material.into()] {
        r.release(resource)?;
    }
    Ok(())
}

/// A tint over the whole quad covers a planar overlay in its middle, unless it goes under the paint: then the overlay
/// (a burnt face's painted eyes) stays on top of the tint.
fn surface_fx_under_paint_keeps_the_overlays(r: &mut dyn Renderer) -> Outcome {
    use omuzaki_render_api::SurfaceFx;
    let tol = tolerance(r).max(1);
    let face = r.create_texture(&TextureDesc::colour(UVec2::new(2, 2)).dynamic(), None)?;
    r.paint_texture(face, &mut |t: &mut dyn PaintTarget| {
        t.write_rgba(PixelRect::full(t.desc().size), &[0, 0, 255, 255].repeat(4))
    })?;
    let planar = Mapping::Planar { min: Vec2::splat(-0.25), max: Vec2::splat(0.25), fade_from: -1.0, fade_to: -1.0 };
    let overlay = Overlay { texture: face, mapping: planar, strength: 1.0 };
    let (s, mesh, material) = overlay_scene(r, Some(overlay))?;
    let probe_edge = (CENTRE.0 + 12, CENTRE.1); // world x = 0.375: on the quad, outside the planar rectangle
    let burnt = |under_paint: bool| {
        let mut m = Material::unlit(Colour::WHITE);
        m.overlays.push(overlay);
        m.fx = Some(SurfaceFx {
            amount: 2.0,
            colour: Colour::rgb(255, 0, 0),
            edge_width: 0.0,
            glow: 0.0,
            under_paint,
            ..SurfaceFx::default()
        });
        m
    };

    r.update_material(material, &burnt(false))?;
    let img = draw(r, &s, &plain())?;
    expect_pixel(&img, CENTRE, Colour::rgb(255, 0, 0), tol, "a tint over the paint covers the overlay")?;

    r.update_material(material, &burnt(true))?;
    let img = draw(r, &s, &plain())?;
    expect_pixel(&img, CENTRE, Colour::rgb(0, 0, 255), tol, "a tint under the paint leaves the overlay on top")?;
    expect_pixel(&img, probe_edge, Colour::rgb(255, 0, 0), tol, "outside the overlay the tint still shows")?;

    for resource in [mesh.into(), material.into(), face.into()] {
        r.release(resource)?;
    }
    Ok(())
}

fn painted_textures_show_through_overlays(r: &mut dyn Renderer) -> Outcome {
    let tol = tolerance(r).max(1);
    let size = UVec2::new(2, 2);
    let face = r.create_texture(&TextureDesc::colour(size).dynamic(), None)?;
    let fill = |colour: Colour| {
        move |t: &mut dyn PaintTarget| {
            t.write_rgba(PixelRect::full(t.desc().size), &[colour.r, colour.g, colour.b, 255].repeat(4))
        }
    };

    // The quad spans -0.5..0.5; a planar rectangle over -0.25..0.25 covers only its middle half.
    let planar = Mapping::Planar { min: Vec2::splat(-0.25), max: Vec2::splat(0.25), fade_from: -1.0, fade_to: -1.0 };
    let overlay = Overlay { texture: face, mapping: planar, strength: 1.0 };
    let (s, mesh, material) = overlay_scene(r, Some(overlay))?;
    let probe_edge = (CENTRE.0 + 12, CENTRE.1); // world x = 0.375: on the quad, outside the planar rectangle

    r.paint_texture(face, &mut fill(Colour::rgb(255, 0, 0)))?;
    let img = draw(r, &s, &plain())?;
    expect_pixel(&img, CENTRE, Colour::rgb(255, 0, 0), tol, "red paint through a planar overlay")?;
    expect_pixel(&img, probe_edge, Colour::WHITE, tol, "outside the planar rectangle the base shows")?;

    r.paint_texture(face, &mut fill(Colour::rgb(0, 0, 255)))?;
    let img = draw(r, &s, &plain())?;
    expect_pixel(&img, CENTRE, Colour::rgb(0, 0, 255), tol, "repainting changes the next frame")?;

    // A failing painter leaves the texture as it was, whatever it wrote before failing.
    let mut failing = |t: &mut dyn PaintTarget| -> Result<(), RenderError> {
        fill(Colour::rgb(0, 255, 0))(t)?;
        Err(RenderError::Backend("painter gave up".into()))
    };
    expect_err(r.paint_texture(face, &mut failing), "a failing painter", |e| matches!(e, RenderError::Backend(_)))?;
    let img = draw(r, &s, &plain())?;
    expect_pixel(&img, CENTRE, Colour::rgb(0, 0, 255), tol, "a failed paint must leave the old pixels")?;

    // Mapping::Uv puts the whole texture on the whole quad.
    r.update_material(material, &{
        let mut m = Material::unlit(Colour::WHITE);
        m.overlays.push(Overlay { texture: face, mapping: Mapping::Uv, strength: 1.0 });
        m
    })?;
    let img = draw(r, &s, &plain())?;
    expect_pixel(&img, probe_edge, Colour::rgb(0, 0, 255), tol, "a UV overlay covers the whole quad")?;

    for resource in [mesh.into(), material.into(), face.into()] {
        r.release(resource)?;
    }
    Ok(())
}

fn outlines_hug_the_silhouette(r: &mut dyn Renderer) -> Outcome {
    let tol = tolerance(r).max(1);
    let mesh = r.create_mesh(&shapes::cuboid(Vec3::splat(0.4)))?;
    let line = Colour::rgb(255, 0, 0);
    let mut material = Material::unlit(Colour::rgb(0, 255, 0));
    material.outline = Some(Outline { colour: line, width: 0.1 });
    let material = r.create_material(&material)?;
    let mut s = scene(Colour::BLACK);
    s.instances.push(Instance::new(mesh, material, Mat4::IDENTITY));

    // The cube's face ends at world x = 0.4 (pixel 45); 0.45 (pixel ~46) is inside any legal line width, 0.9 is not.
    let ring = (CENTRE.0 + 14, CENTRE.1);
    let far = (CENTRE.0 + 29, CENTRE.1);
    let img = draw(r, &s, &plain())?;
    expect_pixel(&img, CENTRE, Colour::rgb(0, 255, 0), tol, "the model stays in front of its outline")?;
    expect_pixel(&img, ring, line, tol, "the outline just outside the silhouette")?;
    expect_pixel(&img, far, Colour::BLACK, tol, "far from the model")?;

    s.outline_light = 0.0;
    let dark = draw(r, &s, &plain())?;
    expect_pixel(&dark, ring, Colour::BLACK, tol, "outline_light 0 removes the line's light")?;
    for resource in [mesh.into(), material.into()] {
        r.release(resource)?;
    }
    Ok(())
}

/// A single-layer card (a hair ribbon, #819) seen face on, from the front and from behind: the line runs round its
/// edge on both sides, outside the card and never over it.
fn open_sheets_are_outlined_from_both_sides(r: &mut dyn Renderer) -> Outcome {
    let tol = tolerance(r).max(1);
    let mesh = r.create_mesh(&shapes::quad(Vec2::splat(0.4)))?;
    let line = Colour::rgb(255, 0, 0);
    let mut material = Material::unlit(Colour::rgb(0, 255, 0));
    material.double_sided = true;
    material.outline = Some(Outline { colour: line, width: 0.1 });
    let material = r.create_material(&material)?;

    // The card's edge is at world x = 0.4 (pixel 44.8) and y = 0.4; 0.44 (pixel 46) is inside the line, 0.9 is not.
    let probes = [
        ((CENTRE.0 + 14, CENTRE.1), "the line beyond the right edge"),
        ((CENTRE.0 - 15, CENTRE.1), "the line beyond the left edge"),
        ((CENTRE.0, CENTRE.1 - 15), "the line beyond the top edge"),
        ((CENTRE.0 + 15, CENTRE.1 + 14), "the line round a corner"),
    ];
    for (turn, side) in [(0.0, "front"), (std::f32::consts::PI, "back")] {
        let mut s = scene(Colour::BLACK);
        s.instances.push(Instance::new(mesh, material, Mat4::from_rotation_y(turn)));
        let img = draw(r, &s, &plain())?;
        let seen = format!("seen from the {side}");
        expect_pixel(
            &img,
            CENTRE,
            Colour::rgb(0, 255, 0),
            tol,
            &format!("the card stays in front of its line, {seen}"),
        )?;
        expect_pixel(
            &img,
            (CENTRE.0 + 11, CENTRE.1),
            Colour::rgb(0, 255, 0),
            tol,
            &format!("inside the edge, {seen}"),
        )?;
        for (probe, what) in probes {
            expect_pixel(&img, probe, line, tol, &format!("{what}, {seen}"))?;
        }
        expect_pixel(&img, (CENTRE.0 + 29, CENTRE.1), Colour::BLACK, tol, &format!("far from the card, {seen}"))?;
    }
    for resource in [mesh.into(), material.into()] {
        r.release(resource)?;
    }
    Ok(())
}

fn edge_lines_draw_on_the_near_side_of_depth_jumps(r: &mut dyn Renderer) -> Outcome {
    let tol = tolerance(r).max(1);
    // A white card a metre in front of a grey one, on black: the near card's edge is at pixel 40.32, the far one's at
    // 56.32, so pixels 24..=39 and 8..=55 are covered along the middle row.
    let near = r.create_mesh(&shapes::quad(Vec2::splat(0.26)))?;
    let far = r.create_mesh(&shapes::quad(Vec2::splat(0.76)))?;
    let white = r.create_material(&Material::unlit(Colour::WHITE))?;
    let grey = r.create_material(&Material::unlit(Colour::rgb(128, 128, 128)))?;
    let line = Colour::rgb(255, 0, 0);
    let mut s = scene(Colour::BLACK);
    s.instances.push(Instance::new(far, grey, Mat4::from_translation(Vec3::new(0.0, 0.0, -0.5))));
    s.instances.push(Instance::new(near, white, Mat4::from_translation(Vec3::new(0.0, 0.0, 0.5))));
    let plain_image = draw(r, &s, &plain())?;
    expect_pixel(&plain_image, (39, 32), Colour::WHITE, tol, "no lines unless the scene asks")?;
    s.edges = Some(EdgeLines::new(line, 2.0));
    let img = draw(r, &s, &plain())?;
    let row = |x: u32| (x, CENTRE.1);
    for (x, colour, what) in [
        (39, line, "the near card's last pixel before the far one"),
        (38, line, "the band is two pixels wide"),
        (36, Colour::WHITE, "inside the near card, past the band"),
        (41, Colour::rgb(128, 128, 128), "the far side of the jump draws no line"),
        (55, line, "the far card's silhouette against the background"),
        (54, line, "two pixels there too"),
        (52, Colour::rgb(128, 128, 128), "the far card, past its band"),
        (57, Colour::BLACK, "the background draws no line"),
    ] {
        expect_pixel(&img, row(x), colour, tol, what)?;
    }
    expect_pixel(&img, CENTRE, Colour::WHITE, tol, "a flat card has no line inside it")?;
    for resource in [near.into(), far.into(), white.into(), grey.into()] {
        r.release(resource)?;
    }
    Ok(())
}

fn blended_instances_sort_back_to_front(r: &mut dyn Renderer) -> Outcome {
    let mesh = r.create_mesh(&shapes::quad(Vec2::splat(0.5)))?;
    let make = |r: &mut dyn Renderer, colour: Colour| -> Result<MaterialId, Failure> {
        let mut m = Material::unlit(colour);
        m.alpha = AlphaMode::Blend;
        Ok(r.create_material(&m)?)
    };
    let far = make(r, Colour::rgba(0, 255, 0, 128))?;
    let near = make(r, Colour::rgba(255, 0, 0, 128))?;
    let place = |material, z: f32| Instance::new(mesh, material, Mat4::from_translation(Vec3::new(0.0, 0.0, z)));
    let render = |r: &mut dyn Renderer, order: [Instance; 2]| {
        let mut s = scene(Colour::BLACK);
        s.instances.extend(order);
        draw(r, &s, &plain())
    };
    let (a, b) = (place(far, -0.5), place(near, 0.5));
    let one = render(r, [a, b])?;
    let two = render(r, [b, a])?;
    same_image(&one, &two, tolerance(r).max(1), "blended quads must not depend on submission order")?;
    let over = pixel(&one, CENTRE)?;
    ensure!(over.r > over.g, "the nearer red quad should dominate the blend, got {over}");
    for resource in [mesh.into(), far.into(), near.into()] {
        r.release(resource)?;
    }
    Ok(())
}

fn skinned_instances_follow_their_joints(r: &mut dyn Renderer) -> Outcome {
    let tol = tolerance(r).max(1);
    let mut quad = shapes::quad(Vec2::splat(0.25));
    quad.joints = vec![[0, 0, 0, 0]; 4];
    quad.weights = vec![[1.0, 0.0, 0.0, 0.0]; 4];
    let mesh = r.create_mesh(&quad)?;
    let material = unlit(r, Colour::WHITE)?;

    let mut s = scene(Colour::BLACK);
    let mut inst = Instance::new(mesh, material, Mat4::IDENTITY);
    inst.skin = Some(JointRange { start: 0, len: 1 });
    s.instances.push(inst);
    // The joint matrix maps bind space to model space: it moves the quad half a unit (16 px) to the right.
    s.joints.push(Mat4::from_translation(Vec3::new(0.5, 0.0, 0.0)));
    let img = draw(r, &s, &plain())?;
    expect_pixel(&img, CENTRE, Colour::BLACK, tol, "the bind-pose position is now empty")?;
    expect_pixel(&img, (CENTRE.0 + 16, CENTRE.1), Colour::WHITE, tol, "the quad at its skinned position")?;
    for resource in [mesh.into(), material.into()] {
        r.release(resource)?;
    }
    Ok(())
}

/// A disc in the XY plane, `radius` across, as the silhouette of a ball: its centre faces +Z and its rim faces out
/// from the centre, one joint for all of it.
fn skinned_disc(radius: f32) -> MeshData {
    let rim = 24_u16;
    let mut positions = vec![Vec3::ZERO];
    let mut normals = vec![Vec3::Z];
    for i in 0..rim {
        let a = 2.0 * PI * f32::from(i) / f32::from(rim);
        let out = Vec3::new(a.cos(), a.sin(), 0.0);
        positions.push(out * radius);
        normals.push(out);
    }
    let indices = (0..u32::from(rim)).flat_map(|i| [0, 1 + i, 1 + (i + 1) % u32::from(rim)]).collect();
    let n = positions.len();
    MeshData {
        uvs: vec![Vec2::ZERO; n],
        joints: vec![[0, 0, 0, 0]; n],
        weights: vec![[1.0, 0.0, 0.0, 0.0]; n],
        positions,
        normals,
        indices,
        ..MeshData::default()
    }
}

fn a_smear_pulls_the_trailing_side_back(r: &mut dyn Renderer) -> Outcome {
    let tol = tolerance(r).max(1);
    let mesh = r.create_mesh(&skinned_disc(0.25))?;
    let material = unlit(r, Colour::WHITE)?;
    // The disc moved half a unit (16 px) right since its trailing pose, which left it at the origin.
    let render = |r: &mut dyn Renderer, smear: Option<Smear>| {
        let mut s = scene(Colour::BLACK);
        let mut inst = Instance::new(mesh, material, Mat4::IDENTITY);
        inst.skin = Some(JointRange { start: 0, len: 1 });
        inst.smear = smear;
        s.instances.push(inst);
        s.joints = vec![Mat4::from_translation(Vec3::new(0.5, 0.0, 0.0)), Mat4::IDENTITY];
        draw(r, &s, &plain())
    };
    let trail = JointRange { start: 1, len: 1 };
    let posed = render(r, None)?;
    let streak = render(r, Some(Smear::new(trail, 1.0)))?;
    let behind = (CENTRE.0 + 4, CENTRE.1);
    expect_pixel(&posed, behind, Colour::BLACK, tol, "the posed disc is clear of where it was")?;
    expect_pixel(&streak, behind, Colour::WHITE, tol, "the smear streaks back to where it was")?;
    expect_pixel(&streak, (CENTRE.0 + 23, CENTRE.1), Colour::WHITE, tol, "the leading side holds")?;
    expect_pixel(&streak, (CENTRE.0 + 28, CENTRE.1), Colour::BLACK, tol, "and goes no further ahead")?;
    expect_pixel(&streak, (CENTRE.0 + 4, CENTRE.1 + 8), Colour::BLACK, tol, "the streak narrows behind")?;
    // A slow move, 0.15 m under a 0.2 m threshold, stays crisp.
    let mut s = scene(Colour::BLACK);
    let mut inst = Instance::new(mesh, material, Mat4::IDENTITY);
    inst.skin = Some(JointRange { start: 0, len: 1 });
    inst.smear = Some(Smear { threshold: 0.2, ..Smear::new(trail, 1.0) });
    s.instances.push(inst);
    s.joints = vec![Mat4::from_translation(Vec3::new(0.15, 0.0, 0.0)), Mat4::IDENTITY];
    let slow = draw(r, &s, &plain())?;
    s.instances[0].smear = None;
    let slow_posed = draw(r, &s, &plain())?;
    same_image(&slow, &slow_posed, tol, "a move under the threshold is drawn as posed")?;
    for resource in [mesh.into(), material.into()] {
        r.release(resource)?;
    }
    Ok(())
}

fn mirrored_instances_render_like_the_original(r: &mut dyn Renderer) -> Outcome {
    let tol = tolerance(r).max(1);
    let shell = r.create_mesh(&shapes::cuboid(Vec3::splat(0.4)))?;
    let core = r.create_mesh(&shapes::cuboid(Vec3::splat(0.1)))?;
    let mut outer = Material::unlit(Colour::rgb(0, 255, 0));
    outer.outline = Some(Outline { colour: Colour::rgb(255, 0, 0), width: 0.02 });
    let outer = r.create_material(&outer)?;
    let inner = unlit(r, Colour::rgb(0, 0, 255))?;
    let render = |r: &mut dyn Renderer, transform: Mat4| {
        let mut s = scene(Colour::BLACK);
        // A small cube sits inside the big one: it is hidden only if the big cube's *front* faces are drawn.
        s.instances.push(Instance::new(core, inner, Mat4::IDENTITY));
        s.instances.push(Instance::new(shell, outer, transform));
        draw(r, &s, &plain())
    };
    let original = render(r, Mat4::IDENTITY)?;
    let mirrored = render(r, Mat4::from_scale(Vec3::new(-1.0, 1.0, 1.0)))?;
    expect_pixel(&original, CENTRE, Colour::rgb(0, 255, 0), tol, "the shell hides what is inside it")?;
    expect_pixel(&mirrored, CENTRE, Colour::rgb(0, 255, 0), tol, "a mirrored shell still hides what is inside it")?;
    // The shell is symmetric in X, so mirroring it may not change a pixel (this also covers the outline hull, whose
    // back-face rule flips with the winding).
    same_image(&original, &mirrored, tol, "a mirrored shell")?;
    for resource in [shell.into(), core.into(), outer.into(), inner.into()] {
        r.release(resource)?;
    }
    Ok(())
}

fn more_samples_smooth_edges(r: &mut dyn Renderer) -> Outcome {
    if r.capabilities().max_samples < 4 {
        return Ok(());
    }
    let mesh = r.create_mesh(&shapes::quad(Vec2::splat(0.5)))?;
    let material = unlit(r, Colour::WHITE)?;
    let mut s = scene(Colour::BLACK);
    s.instances.push(Instance::new(mesh, material, Mat4::from_rotation_z(0.5)));
    // Pixels that are neither the background nor the fill: only a smoothed edge produces them.
    let blended = |img: &Image| img.rgba8.as_chunks::<4>().0.iter().filter(|p| p[0] != 0 && p[0] != 255).count();
    let hard = blended(&draw(r, &s, &Quality { samples: 1, ..plain() })?);
    let smooth = blended(&draw(r, &s, &Quality { samples: 4, ..plain() })?);
    ensure!(hard == 0, "one sample per pixel must give hard edges, found {hard} blended pixels");
    ensure!(smooth > 0, "four samples per pixel must smooth a rotated edge, found no blended pixels");
    r.release(mesh.into())?;
    r.release(material.into())?;
    Ok(())
}

fn unknown_shading_programs_fall_back_to_toon(r: &mut dyn Renderer) -> Outcome {
    let name = Slug::new("no-such-program")?;
    ensure!(!r.capabilities().shading_programs.contains(&name), "the test needs a program the renderer doesn't know");
    let tol = tolerance(r);

    let plain_mesh = r.create_mesh(&shapes::cuboid(Vec3::splat(0.4)))?;
    let mut with_extra = shapes::cuboid(Vec3::splat(0.4));
    let strand = vec![0.25; with_extra.positions.len() * 2];
    with_extra.extra.push(ExtraAttribute { name: Slug::new("strand")?, components: 2, data: strand });
    let extra_mesh = r.create_mesh(&with_extra)?;

    let base = Colour::rgb(200, 120, 160);
    let toon = r.create_material(&Material::toon(base))?;
    let mut program = Material::toon(base);
    program.shading =
        Shading::Program { name, params: ProgramParams::from([(Slug::new("width")?, ParamValue::Float(0.5))]) };
    let program = r.create_material(&program)?;

    let render = |r: &mut dyn Renderer, mesh, material| {
        let mut s = scene(Colour::rgb(10, 10, 20));
        s.lights = vec![headlight(2.0)];
        s.instances.push(Instance::new(mesh, material, Mat4::from_rotation_y(0.5)));
        draw(r, &s, &plain())
    };
    let expected = render(r, plain_mesh, toon)?;
    let fallback = render(r, extra_mesh, program)?;
    same_image(&expected, &fallback, tol, "an unknown program (and a mesh with extra attributes) draws as toon")?;
    for resource in [plain_mesh.into(), extra_mesh.into(), toon.into(), program.into()] {
        r.release(resource)?;
    }
    Ok(())
}

// ---- point lights and the rim's view direction ------------------------------------------------------------------

/// The range of the point lights the checks below use, in metres.
const RANGE: f32 = 7.0;

/// three.js `getDistanceAttenuation` with decay 2 (D10 in `docs/design/render-api.md`): the share of a point light's
/// colour that reaches a surface `distance` metres away. It is written out here rather than shared with the mock,
/// because this formula is the contract every renderer is held to.
fn three_js_falloff(distance: f32, range: f32) -> f32 {
    let window = (1.0 - (distance / range).powi(4)).clamp(0.0, 1.0);
    window * window / (distance * distance).max(0.01)
}

/// The colour at `at` when `instance` is drawn under `lights`, on a black background with no tone curve.
fn shade_at(r: &mut dyn Renderer, instance: Instance, lights: Vec<Light>, at: (u32, u32)) -> Result<Colour, Failure> {
    let mut s = scene(Colour::BLACK);
    s.lights = lights;
    s.instances.push(instance);
    pixel(&draw(r, &s, &plain())?, at)
}

/// A toon material with a white shade side, so the terminator ramp is 1 everywhere and only the light's falloff (or
/// the rim) shows.
fn flat_toon(base: Colour, rim: Rim) -> Material {
    let mut material = Material::toon(base);
    material.shading = Shading::Toon(Toon { shade: Colour::WHITE, rim, ..Toon::default() });
    material
}

/// A point light falls off like three.js (D10): inverse square with a smooth window that reaches zero exactly at the
/// range. The quad is small, so the mock's per-triangle lighting (at each triangle's centroid) agrees with a per-pixel
/// one to well within the tolerance.
fn point_lights_fall_off_like_threejs(r: &mut dyn Renderer) -> Outcome {
    const INTENSITY: f32 = 2.0;
    const DISTANCES: [f32; 6] = [1.0, 1.6, 2.4, 3.2, 4.5, 5.5];
    let tol = tolerance(r).max(1);
    let mesh = r.create_mesh(&shapes::quad(Vec2::splat(0.1)))?;
    let material = r.create_material(&flat_toon(Colour::WHITE, Rim { strength: 0.0, ..Rim::default() }))?;
    let light = |distance: f32| Light::Point {
        position: Vec3::new(0.0, 0.0, distance),
        colour: Colour::WHITE,
        intensity: INTENSITY,
        range: RANGE,
    };
    // Inside the range the centre pixel is intensity × falloff / π in linear light, then encoded like any pixel.
    for distance in DISTANCES {
        let level = INTENSITY * three_js_falloff(distance, RANGE) / PI;
        let want = Colour::from_linear(Vec3::splat(level), 1.0);
        let instance = Instance::new(mesh, material, Mat4::IDENTITY);
        let got = shade_at(r, instance, vec![light(distance)], CENTRE)?;
        ensure!(
            close(got, want, tol),
            "a point light {distance} m away: the centre is {got}, expected {want} (tolerance {tol})"
        );
    }
    // At the range and beyond, no light reaches the surface.
    for distance in [RANGE, 7.5, 9.0] {
        let instance = Instance::new(mesh, material, Mat4::IDENTITY);
        let got = shade_at(r, instance, vec![light(distance)], CENTRE)?;
        ensure!(
            close(got, Colour::BLACK, tol),
            "a point light {distance} m away (at or past its range): the centre is {got}, expected black"
        );
    }
    r.release(mesh.into())?;
    r.release(material.into())?;
    Ok(())
}

/// An orthographic camera sees every point along its own axis, so a rim depends on the normal alone. The GPU renderer
/// uses that constant axis; the mock once took the eye's direction from each triangle, which tilts the rim of a surface
/// that sits off the camera's axis.
fn orthographic_rims_use_the_view_axis(r: &mut dyn Renderer) -> Outcome {
    let tol = tolerance(r).max(1);
    let mesh = r.create_mesh(&shapes::quad(Vec2::splat(0.1)))?;
    // Power 1 and full strength, so the rim is exactly 1 - N·V and the expected value is a subtraction.
    let rim = Rim { colour: Colour::WHITE, power: 1.0, strength: 1.0 };
    let material = r.create_material(&flat_toon(Colour::BLACK, rim))?;
    // The quad sits 0.8 m off the camera's axis, which is where the probe reads (pixel 57 covers world x 0.78..0.81).
    // Seen from the eye (5 m away on the axis) that would tilt the view by about 9°; an orthographic view must not.
    let offset = Mat4::from_translation(Vec3::new(0.8, 0.0, 0.0));
    for degrees in [0.0_f32, 60.0] {
        let transform = offset * Mat4::from_rotation_y(degrees.to_radians());
        let normal = transform.transform_vector3(Vec3::Z);
        let want = Colour::from_linear(Vec3::splat(1.0 - normal.dot(Vec3::Z)), 1.0);
        let instance = Instance::new(mesh, material, transform);
        let got = shade_at(r, instance, Vec::new(), (57, 32))?;
        ensure!(
            close(got, want, tol),
            "a surface tilted {degrees}° at 0.8 m off the axis: the rim is {got}, expected {want} (tolerance {tol})"
        );
    }
    r.release(mesh.into())?;
    r.release(material.into())?;
    Ok(())
}

// ---- spot lights, fog and environments (LK-08) --------------------------------------------------------------------

/// A pixel's colour as linear light, for checks whose expected values are physical quantities.
fn linear(c: Colour) -> Vec3 {
    c.to_linear()
}

/// three.js `smoothstep(e0, e1, x)`; the contract's cone edge (a zero-width penumbra is a hard step).
fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    if e1 <= e0 {
        return f32::from(x >= e0);
    }
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Renders `instance` after `setup` has dressed an otherwise empty scene, and reads pixel `at`.
fn probe(
    r: &mut dyn Renderer,
    setup: impl FnOnce(&mut Scene),
    instance: Instance,
    at: (u32, u32),
) -> Result<Colour, Failure> {
    let mut s = scene(Colour::BLACK);
    setup(&mut s);
    s.instances.push(instance);
    pixel(&draw(r, &s, &plain())?, at)
}

/// The world x of the centre of pixel column `px` in the checks' camera.
fn world_x(px: u32) -> f32 {
    (f32::from(u16::try_from(px).unwrap_or(0)) + 0.5 - 32.0) / 32.0
}

/// A spot light follows D10's distance falloff inside a three.js cone: full strength on the axis, a hard edge at the
/// outer angle without penumbra, and `smoothstep(cos outer, cos(outer × (1 - penumbra)), cos angle)` with it. Each probe
/// is a one-pixel quad, so the mock's per-triangle lighting agrees with a per-pixel one to within the tolerance.
fn spot_cones_have_threejs_edges(r: &mut dyn Renderer) -> Outcome {
    const INTENSITY: f32 = 3.0;
    const HEIGHT: f32 = 3.0;
    const OUTER: f32 = 15.0;
    // A one-pixel quad moved to the centre of column `px`: its lighting point is within half a pixel of the probe.
    let tol = tolerance(r).max(4);
    let mesh = r.create_mesh(&shapes::quad(Vec2::splat(0.5 / 32.0)))?;
    let material = r.create_material(&flat_toon(Colour::WHITE, Rim { strength: 0.0, ..Rim::default() }))?;
    let spot = |penumbra: f32| Light::Spot {
        position: Vec3::new(0.0, 0.0, HEIGHT),
        direction: -Vec3::Z,
        colour: Colour::WHITE,
        intensity: INTENSITY,
        range: RANGE,
        cone_outer_deg: OUTER,
        penumbra,
        casts_shadow: false,
    };
    // Columns 46 (8.6° off the axis), 52 (12.1°), 56 (14.0°, inside the edge) and 60 (15.9°, outside it).
    for (penumbra, column) in [(0.0, 46), (0.0, 56), (0.0, 60), (1.0, 46), (1.0, 52), (0.5, 52), (0.5, 56)] {
        let x = world_x(column);
        let distance = x.hypot(HEIGHT);
        let cos_angle = HEIGHT / distance;
        let outer = OUTER.to_radians();
        let cone = smoothstep(outer.cos(), (outer * (1.0 - penumbra)).cos(), cos_angle);
        let level = INTENSITY * three_js_falloff(distance, RANGE) * cone / PI;
        let want = Colour::from_linear(Vec3::splat(level), 1.0);
        // Row 32's centre is half a pixel below the camera's axis.
        let instance = Instance::new(mesh, material, Mat4::from_translation(Vec3::new(x, -0.5 / 32.0, 0.0)));
        let got = probe(r, |s| s.lights = vec![spot(penumbra)], instance, (column, 32))?;
        ensure!(
            close(got, want, tol),
            "penumbra {penumbra}, {:.1}° off the axis: the probe is {got}, expected {want} (tolerance {tol})",
            cos_angle.acos().to_degrees()
        );
    }
    r.release(mesh.into())?;
    r.release(material.into())?;
    Ok(())
}

/// Linear fog blends a surface toward the fog colour by `(depth - near) / (far - near)`, clamped, where depth is the
/// distance along the view direction. The background is not fogged.
fn fog_fades_linearly_between_near_and_far(r: &mut dyn Renderer) -> Outcome {
    let tol = tolerance(r).max(1);
    let mesh = r.create_mesh(&shapes::quad(Vec2::splat(0.1)))?;
    let surface = Colour::rgb(200, 50, 50);
    let fog = Fog { colour: Colour::rgb(20, 40, 200), near: 4.0, far: 6.0 };
    let material = unlit(r, surface)?;
    let mut s = scene(Colour::rgb(9, 9, 9));
    s.fog = Some(fog);
    // The camera is 5 m from the origin, so a quad at z has depth 5 - z: 3 (before near), 4 (at near), 5 (halfway),
    // 6 (at far) and 8 (beyond far).
    let placed: [(f32, f32, u32); 5] =
        [(-0.75, 2.0, 8), (-0.4, 1.0, 19), (0.0, 0.0, 32), (0.4, -1.0, 44), (0.75, -3.0, 56)];
    for (x, z, _) in placed {
        s.instances.push(Instance::new(mesh, material, Mat4::from_translation(Vec3::new(x, 0.0, z))));
    }
    let image = draw(r, &s, &plain())?;
    for (_, z, column) in placed {
        let amount = fog.amount(5.0 - z);
        let want = Colour::from_linear(linear(surface).lerp(linear(fog.colour), amount), 1.0);
        expect_pixel(&image, (column, 32), want, tol, &format!("a surface {} m away (fog {amount})", 5.0 - z))?;
    }
    expect_pixel(&image, (2, 2), Colour::rgb(9, 9, 9), tol, "the background")?;
    r.release(mesh.into())?;
    r.release(material.into())?;
    Ok(())
}

/// A sky from which the surface can be read: a quad of white toon material (no shade, no rim) that faces `normal`, seen
/// by a camera 5 m out along it. Its linear value is the sky's irradiance over π, times the intensity.
fn sky_value(
    r: &mut dyn Renderer,
    env: &EnvironmentDesc,
    using: impl Fn(EnvironmentUse) -> EnvironmentUse,
    normal: Vec3,
) -> Result<Vec3, Failure> {
    let up = if normal.abs_diff_eq(Vec3::Y, 1e-3) || normal.abs_diff_eq(-Vec3::Y, 1e-3) { Vec3::Z } else { Vec3::Y };
    let eye = normal * 5.0;
    let mesh = r.create_mesh(&shapes::quad(Vec2::splat(0.1)))?;
    let material = r.create_material(&flat_toon(Colour::WHITE, Rim { strength: 0.0, ..Rim::default() }))?;
    let id = r.create_environment(env)?;
    let mut s = scene(Colour::BLACK);
    s.camera = Camera::look_at(eye, Vec3::ZERO, up, camera().projection);
    s.environment = Some(using(EnvironmentUse::new(id)));
    // The quad faces +Z; turn it to face `normal`.
    let turn = Mat4::from_quat(omuzaki_core::glam::Quat::from_rotation_arc(Vec3::Z, normal));
    s.instances.push(Instance::new(mesh, material, turn));
    let got = draw(r, &s, &plain()).and_then(|img| pixel(&img, CENTRE));
    r.release(mesh.into())?;
    r.release(material.into())?;
    r.release(id.into())?;
    Ok(linear(got?))
}

fn expect_level(got: Vec3, want: f32, tol: f32, what: &str) -> Outcome {
    let worst = (got - Vec3::splat(want)).abs().max_element();
    // Written so that a NaN fails too.
    if worst.is_nan() || worst > tol {
        return Err(Failure(format!("{what}: the surface is lit to {got}, expected {want} (tolerance {tol})")));
    }
    Ok(())
}

/// The diffuse light from a sky is its cosine-weighted irradiance over π, whatever the intensity: a uniform sky of
/// colour `c` lights a white surface to `c × intensity` on every side, and a sky that is bright only overhead lights
/// up-facing surfaces to 2/3, side-facing ones to 2/(3π) and down-facing ones not at all (worked by hand from
/// `∫ L max(n·ω, 0) dω / π` with `L = max(y, 0)`). Prefiltered maps are approximate, hence the tolerance.
fn a_sky_lights_surfaces_by_irradiance(r: &mut dyn Renderer) -> Outcome {
    let uniform = Colour::rgb(200, 100, 50);
    let flat = EnvironmentDesc::Sky { zenith: uniform, horizon: uniform, ground: uniform };
    for normal in [Vec3::Z, Vec3::Y, -Vec3::Y] {
        let got = sky_value(r, &flat, |u| EnvironmentUse { intensity: 0.5, ..u }, normal)?;
        let want = linear(uniform) * 0.5;
        ensure!(
            (got - want).abs().max_element() <= 0.01,
            "a uniform sky at intensity 0.5 should light a surface facing {normal} to {want}, got {got}"
        );
    }
    let overhead = EnvironmentDesc::Sky { zenith: Colour::WHITE, horizon: Colour::BLACK, ground: Colour::BLACK };
    expect_level(sky_value(r, &overhead, |u| u, Vec3::Y)?, 2.0 / 3.0, 0.04, "facing up under an overhead sky")?;
    expect_level(
        sky_value(r, &overhead, |u| u, Vec3::Z)?,
        2.0 / (3.0 * PI),
        0.04,
        "facing sideways under an overhead sky",
    )?;
    expect_level(sky_value(r, &overhead, |u| u, -Vec3::Y)?, 0.0, 0.03, "facing down under an overhead sky")?;
    expect_level(
        sky_value(r, &overhead, |u| EnvironmentUse { intensity: 0.0, ..u }, Vec3::Y)?,
        0.0,
        0.002,
        "intensity 0",
    )?;
    Ok(())
}

/// An HDRI whose bright half is the `z > 0` side of the world (columns 2 and 3 of four: `u = atan2(z, x) / 2π + 0.5`
/// puts `+Z` at three quarters across). A surface facing a bright hemisphere gets irradiance π, so a linear value of
/// 1; one facing the dark hemisphere gets 0. (A nine-term fit of a hard edge lands within about 8% of that, so the
/// tolerance is 0.1.) Turning the sky by `rotation_deg` about +Y (counter-clockwise from above)
/// moves the bright side from +Z toward +X.
fn an_hdri_is_equirectangular_and_rotates(r: &mut dyn Renderer) -> Outcome {
    let pixel_rgb = |bright: bool| if bright { [1.0_f32; 3] } else { [0.0; 3] };
    let rgb: Vec<f32> = (0..2).flat_map(|_| (0..4).flat_map(|column| pixel_rgb(column >= 2))).collect();
    let hdri = EnvironmentDesc::Hdri { size: UVec2::new(4, 2), rgb };
    let turned = |degrees: f32| move |u: EnvironmentUse| EnvironmentUse { rotation_deg: degrees, ..u };
    let cases = [
        (Vec3::Z, 0.0, 1.0, "facing +Z, sky unturned"),
        (-Vec3::Z, 0.0, 0.0, "facing -Z, sky unturned"),
        (Vec3::X, 90.0, 1.0, "facing +X, sky turned 90° (the bright side moves from +Z to +X)"),
        (Vec3::X, -90.0, 0.0, "facing +X, sky turned -90°"),
        (Vec3::Z, 180.0, 0.0, "facing +Z, sky turned 180°"),
    ];
    for (normal, degrees, want, what) in cases {
        expect_level(sky_value(r, &hdri, turned(degrees), normal)?, want, 0.1, what)?;
    }
    Ok(())
}

/// With `toon_ramp` the sky lights in bands like a key light: a surface well on the bright side takes the full
/// brightest-side irradiance and one well on the dark side takes the opposite side's, instead of shading smoothly.
/// Under an overhead sky those are 2/3 and 0; the surfaces below are tilted 53° up and down from facing the camera.
fn the_toon_ramp_quantises_the_skys_light(r: &mut dyn Renderer) -> Outcome {
    let overhead = EnvironmentDesc::Sky { zenith: Colour::WHITE, horizon: Colour::BLACK, ground: Colour::BLACK };
    let tilted = |degrees: f32| Vec3::new(0.0, degrees.to_radians().sin(), degrees.to_radians().cos());
    let ramp = |u: EnvironmentUse| EnvironmentUse { toon_ramp: true, ..u };
    expect_level(sky_value(r, &overhead, ramp, tilted(53.13))?, 2.0 / 3.0, 0.04, "banded, tilted up")?;
    expect_level(sky_value(r, &overhead, ramp, tilted(-53.13))?, 0.0, 0.03, "banded, tilted down")?;
    let smooth = sky_value(r, &overhead, |u| u, tilted(53.13))?.x;
    ensure!(
        smooth < 2.0 / 3.0 - 0.06,
        "unbanded, a surface tilted 53° from the zenith is dimmer than one facing it ({smooth})"
    );
    Ok(())
}

/// The linear colour at the centre of a picture with the environment as its background, seen from the origin looking
/// along `forward` through a 30° lens, with `cover` putting a white unlit quad in front of the camera.
fn backdrop_value(
    r: &mut dyn Renderer,
    env: Option<&EnvironmentDesc>,
    using: impl Fn(EnvironmentUse) -> EnvironmentUse,
    forward: Vec3,
    cover: bool,
) -> Result<(Vec3, u8), Failure> {
    let lens = Projection::Perspective { fov_y_deg: 30.0, near: 0.1, far: 20.0 };
    let id = env.map(|e| r.create_environment(e)).transpose()?;
    let mut s = scene(Colour::rgb(255, 0, 255));
    s.background = Background::Environment;
    s.camera = Camera::look_at(Vec3::ZERO, forward, Vec3::Y, lens);
    s.environment = id.map(|id| using(EnvironmentUse::new(id)));
    let quad = if cover {
        let mesh = r.create_mesh(&shapes::quad(Vec2::splat(4.0)))?;
        let material = r.create_material(&Material::unlit(Colour::WHITE))?;
        let place = Mat4::from_translation(forward * 3.0)
            * Mat4::from_quat(omuzaki_core::glam::Quat::from_rotation_arc(Vec3::Z, -forward));
        s.instances.push(Instance::new(mesh, material, place));
        Some((mesh, material))
    } else {
        None
    };
    let got = draw(r, &s, &plain()).and_then(|img| pixel(&img, CENTRE));
    if let Some((mesh, material)) = quad {
        r.release(mesh.into())?;
        r.release(material.into())?;
    }
    if let Some(id) = id {
        r.release(id.into())?;
    }
    let got = got?;
    Ok((linear(got), got.a))
}

/// `Background::Environment` shows the sky's radiance in each pixel's view direction, turned and scaled as the scene
/// uses it, opaque; a surface in front hides it, and without an environment it is black. A procedural sky looking at the
/// horizon shows its horizon colour; the HDRI of `an_hdri_is_equirectangular_and_rotates` shows its bright half towards
/// +Z, and towards +X once turned 90°.
fn the_environment_backdrop_shows_the_sky(r: &mut dyn Renderer) -> Outcome {
    let horizon = Colour::rgb(200, 120, 60);
    let sky = EnvironmentDesc::Sky { zenith: Colour::WHITE, horizon, ground: Colour::BLACK };
    let same = |u: EnvironmentUse| u;
    let (level, alpha) = backdrop_value(r, Some(&sky), same, Vec3::Z, false)?;
    ensure!(alpha == 255, "the environment backdrop is opaque, got alpha {alpha}");
    ensure!(
        (level - linear(horizon)).abs().max_element() <= 0.01,
        "looking at the horizon shows the horizon colour {}, got {level}",
        linear(horizon)
    );
    let (up, _) = backdrop_value(r, Some(&sky), same, Vec3::new(0.0, 1.0, 0.01), false)?;
    expect_level(up, 1.0, 0.01, "looking straight up shows the zenith")?;
    let (half, _) = backdrop_value(r, Some(&sky), |u| EnvironmentUse { intensity: 0.5, ..u }, Vec3::Z, false)?;
    ensure!(
        (half - linear(horizon) * 0.5).abs().max_element() <= 0.01,
        "intensity 0.5 halves the backdrop, got {half}"
    );
    let rgb: Vec<f32> =
        (0..2).flat_map(|_| (0..4).flat_map(|column| [if column >= 2 { 1.0_f32 } else { 0.0 }; 3])).collect();
    let hdri = EnvironmentDesc::Hdri { size: UVec2::new(4, 2), rgb };
    let turned = |degrees: f32| move |u: EnvironmentUse| EnvironmentUse { rotation_deg: degrees, ..u };
    for (forward, degrees, want, what) in [
        (Vec3::Z, 0.0, 1.0, "looking at +Z, sky unturned"),
        (-Vec3::Z, 0.0, 0.0, "looking at -Z, sky unturned"),
        (Vec3::X, 90.0, 1.0, "looking at +X, sky turned 90°"),
        (Vec3::X, -90.0, 0.0, "looking at +X, sky turned -90°"),
    ] {
        expect_level(backdrop_value(r, Some(&hdri), turned(degrees), forward, false)?.0, want, 0.01, what)?;
    }
    expect_level(backdrop_value(r, Some(&sky), same, -Vec3::Z, true)?.0, 1.0, 0.01, "a white surface in front")?;
    let (none, alpha) = backdrop_value(r, None, same, Vec3::Z, false)?;
    ensure!(alpha == 255, "without an environment the backdrop is opaque black, got alpha {alpha}");
    expect_level(none, 0.0, 0.002, "without an environment")?;
    Ok(())
}

/// The gradient a picture backdrop shows at output pixel `(_, y)` of a `size` frame, as straight bytes: it changes down
/// the frame only.
fn gradient_pixel(top: Colour, bottom: Colour, size: UVec2, (_, y): (u32, u32)) -> Colour {
    #[allow(clippy::cast_precision_loss)] // tiny images
    let c = gradient_at(top, bottom, (y as f32 + 0.5) / size.y as f32);
    let straight = if c.w > 0.0 { c.truncate() / c.w } else { Vec3::ZERO };
    let byte = |v: f32| omuzaki_render_api::to_byte(v);
    Colour::rgba(byte(straight.x), byte(straight.y), byte(straight.z), byte(c.w))
}

/// Gradient and texture backdrops are pictures shown as authored (scenes D2): whatever the exposure and tone curve, a
/// gradient's pixels are its colours blended in encoded values, a texture fills the frame by `cover` or sits in it by
/// `contain` with the gradient in the bars and through its holes, the scene lies over them as over a transparent
/// output, and a released texture fails the render.
fn pictures_behind_show_as_authored(r: &mut dyn Renderer) -> Outcome {
    let (top, bottom) = (Colour::rgb(20, 40, 200), Colour::rgb(240, 200, 40));
    let tol = tolerance(r).max(1);
    let mut s = scene(Colour::BLACK);
    s.post.tonemap = Tonemap::Aces;
    s.post.exposure = 3.0;
    s.background = Background::Gradient { top, bottom };
    let image = draw(r, &s, &plain())?;
    for y in [0, 13, 31, 50, 63] {
        let want = gradient_pixel(top, bottom, SIZE, (5, y));
        expect_pixel(&image, (5, y), want, tol, "a gradient row, exposed and tone mapped as authored")?;
    }

    // The scene over the gradient is the scene over nothing, laid over it: premultiplied, the two add.
    let mesh = r.create_mesh(&small_quad())?;
    let half =
        r.create_material(&Material { alpha: AlphaMode::Blend, ..Material::unlit(Colour::rgba(255, 0, 0, 128)) })?;
    s.instances.push(Instance::new(mesh, half, Mat4::IDENTITY));
    let over = r.render_image(SIZE, AlphaEncoding::Premultiplied, &s, &plain())?;
    s.background = Background::Transparent;
    let alone = r.render_image(SIZE, AlphaEncoding::Premultiplied, &s, &plain())?;
    for at in [CENTRE, (2, 2)] {
        let (scene_px, got) = (pixel(&alone, at)?, pixel(&over, at)?);
        let behind = gradient_pixel(top, bottom, SIZE, at);
        let keep = 1.0 - f32::from(scene_px.a) / 255.0;
        let add = |a: u8, b: u8| a.saturating_add(omuzaki_render_api::to_byte(f32::from(b) / 255.0 * keep));
        let want = Colour::rgba(
            add(scene_px.r, behind.r),
            add(scene_px.g, behind.g),
            add(scene_px.b, behind.b),
            add(scene_px.a, behind.a),
        );
        ensure!(close(got, want, tol + 1), "the scene over the gradient at {at:?}: got {got}, expected {want}");
    }
    s.instances.clear();
    r.release(half.into())?;
    r.release(mesh.into())?;

    // A 4 x 2 grid of colour blocks, each block 2 x 2 texels, so a block's middle reads one colour through any filter.
    let blocks = [
        Colour::rgb(255, 0, 0),
        Colour::rgb(0, 255, 0),
        Colour::rgb(0, 0, 255),
        Colour::rgba(0, 0, 0, 0),
        Colour::rgb(255, 255, 0),
        Colour::rgb(0, 255, 255),
        Colour::rgb(255, 0, 255),
        Colour::rgb(128, 128, 128),
    ];
    let texels = UVec2::new(8, 4);
    let rgba: Vec<u8> = (0..texels.y)
        .flat_map(|y| (0..texels.x).map(move |x| (x / 2 + 4 * (y / 2)) as usize))
        .flat_map(|block| {
            let c = blocks[block];
            [c.r, c.g, c.b, c.a]
        })
        .collect();
    let texture = r.create_texture(&TextureDesc::colour(texels), Some(&rgba))?;
    for (size, fit) in [
        (UVec2::new(64, 64), BackdropFit::Cover),
        (UVec2::new(64, 64), BackdropFit::Contain),
        (UVec2::new(32, 64), BackdropFit::Cover),
        (UVec2::new(64, 24), BackdropFit::Contain),
    ] {
        s.background = Background::Texture { texture, fit, top, bottom };
        let image = r.render_image(size, AlphaEncoding::Straight, &s, &plain())?;
        #[allow(clippy::cast_precision_loss)] // tiny images
        let frame = size.x as f32 / size.y as f32;
        let mut checked = [0u32; 2];
        for y in 0..size.y {
            for x in 0..size.x {
                #[allow(clippy::cast_precision_loss)]
                let at = Vec2::new((x as f32 + 0.5) / size.x as f32, (y as f32 + 0.5) / size.y as f32);
                let uv = fit.uv(at, frame, 2.0);
                let behind = gradient_pixel(top, bottom, size, (x, y));
                let outside = uv.cmplt(Vec2::ZERO).any() || uv.cmpgt(Vec2::ONE).any();
                // Texel coordinates with texel centres on whole numbers; a block's two texels are 2k and 2k + 1.
                let p = uv * texels.as_vec2() - 0.5;
                let inner = |v: f32| v.rem_euclid(2.0) <= 1.0 && v > -0.01;
                let want = if outside {
                    // Not next to the picture's edge, where the filter reaches across it.
                    if (uv.x > -0.1 && uv.x < 1.1) && (uv.y > -0.1 && uv.y < 1.1) {
                        continue;
                    }
                    checked[1] += 1;
                    behind
                } else if inner(p.x) && inner(p.y) {
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    let block = blocks[(p.x.max(0.0) as usize / 2) + 4 * (p.y.max(0.0) as usize / 2)];
                    checked[0] += 1;
                    if block.a == 0 { behind } else { block }
                } else {
                    continue;
                };
                expect_pixel(&image, (x, y), want, tol, &format!("{fit:?} in a {}x{} frame", size.x, size.y))?;
            }
        }
        ensure!(checked[0] > 20, "{fit:?} in a {}x{} frame: few picture pixels checked ({checked:?})", size.x, size.y);
        if fit == BackdropFit::Contain {
            ensure!(checked[1] > 20, "{fit:?}: the bars show the gradient ({checked:?})");
        }
    }
    r.release(texture.into())?;
    expect_err(r.render_image(SIZE, AlphaEncoding::Straight, &s, &plain()), "a released backdrop texture", is_unknown)?;
    Ok(())
}

/// Environments are resources: counted in the stats, dead after release (a render naming one fails), and rejected
/// when malformed.
fn environments_are_resources(r: &mut dyn Renderer) -> Outcome {
    let sky = EnvironmentDesc::Sky { zenith: Colour::WHITE, horizon: Colour::WHITE, ground: Colour::WHITE };
    let before = r.stats().environments;
    let id = r.create_environment(&sky)?;
    ensure!(r.stats().environments == before + 1, "the stats count a live environment");
    let mut s = scene(Colour::BLACK);
    s.environment = Some(EnvironmentUse::new(id));
    draw(r, &s, &plain())?;
    r.release(id.into())?;
    ensure!(r.stats().environments == before, "the stats forget a released environment");
    expect_err(r.render_image(SIZE, AlphaEncoding::Straight, &s, &plain()), "a released environment", is_unknown)?;
    expect_err(r.release(id.into()), "releasing an environment twice", is_unknown)?;
    let short = EnvironmentDesc::Hdri { size: UVec2::new(2, 2), rgb: vec![0.0; 11] };
    expect_err(r.create_environment(&short), "an HDRI with too few pixels", is_invalid)?;
    let negative = EnvironmentDesc::Hdri { size: UVec2::new(1, 1), rgb: vec![0.0, -1.0, 0.0] };
    expect_err(r.create_environment(&negative), "an HDRI with a negative value", is_invalid)?;
    Ok(())
}

/// Spot lights, fog and environment uses with impossible numbers fail the render with `Invalid` and leave the target as it
/// was, like every other bad scene.
fn lighting_parameters_are_validated(r: &mut dyn Renderer) -> Outcome {
    let spot = |cone_outer_deg: f32, penumbra: f32, direction: Vec3| Light::Spot {
        position: Vec3::new(0.0, 0.0, 3.0),
        direction,
        colour: Colour::WHITE,
        intensity: 1.0,
        range: 5.0,
        cone_outer_deg,
        penumbra,
        casts_shadow: false,
    };
    let bad_lights =
        [spot(0.0, 0.0, -Vec3::Z), spot(120.0, 0.0, -Vec3::Z), spot(30.0, 1.5, -Vec3::Z), spot(30.0, 0.0, Vec3::ZERO)];
    for light in bad_lights {
        let mut s = scene(Colour::BLACK);
        s.lights.push(light);
        expect_err(
            r.render_image(SIZE, AlphaEncoding::Straight, &s, &plain()),
            "an impossible spot light",
            is_invalid,
        )?;
    }
    for fog in [Fog { colour: Colour::WHITE, near: 3.0, far: 3.0 }, Fog { colour: Colour::WHITE, near: -1.0, far: 3.0 }]
    {
        let mut s = scene(Colour::BLACK);
        s.fog = Some(fog);
        expect_err(r.render_image(SIZE, AlphaEncoding::Straight, &s, &plain()), "impossible fog", is_invalid)?;
    }
    Ok(())
}

// ---- dynamic meshes and morph targets -----------------------------------------------------------------------------

/// A 0.5 x 0.5 quad centred on the origin: 16 x 16 pixels in the middle of the frame.
fn small_quad() -> MeshData {
    shapes::quad(Vec2::splat(0.25))
}

/// The quad with every vertex moved by `by`.
fn moved(mut mesh: MeshData, by: Vec3) -> MeshData {
    mesh.positions.iter_mut().for_each(|p| *p += by);
    mesh
}

/// A one-target morph set that moves all of `mesh`'s vertices by `by`, built the way an importer's dense deltas are.
fn slide_morph(mesh: &MeshData, by: Vec3) -> Result<MorphSet, Failure> {
    let deltas = vec![by; mesh.positions.len()];
    Ok(MorphSet::from_dense(mesh.positions.len(), &[(Slug::new("slide")?, &deltas, &[])], 0.0)?)
}

fn morphable(r: &mut dyn Renderer, mesh: &MeshData, by: Vec3) -> Result<MeshId, Failure> {
    let options = MeshOptions { morphs: Some(slide_morph(mesh, by)?), ..MeshOptions::default() };
    Ok(r.create_mesh_with(mesh, &options)?)
}

/// Renders `mesh` once with its instance customised by `tweak`, on a scene whose morph weights are `weights`.
fn draw_one(
    r: &mut dyn Renderer,
    mesh: MeshId,
    material: MaterialId,
    weights: &[f32],
    tweak: impl FnOnce(&mut Scene, &mut Instance),
) -> Result<Image, RenderError> {
    let mut s = scene(Colour::BLACK);
    s.morph_weights.extend_from_slice(weights);
    let mut inst = Instance::new(mesh, material, Mat4::IDENTITY);
    if !weights.is_empty() {
        inst.morph = Some(MorphRange { start: 0, len: u32::try_from(weights.len()).unwrap_or(u32::MAX) });
    }
    tweak(&mut s, &mut inst);
    s.instances.push(inst);
    r.render_image(SIZE, AlphaEncoding::Straight, &s, &plain())
}

fn updating_vertices_matches_a_fresh_mesh(r: &mut dyn Renderer) -> Outcome {
    let tol = tolerance(r).max(1);
    let dynamic = r.create_mesh_with(&small_quad(), &MeshOptions { usage: MeshUsage::Dynamic, morphs: None })?;
    let material = unlit(r, Colour::WHITE)?;
    let target = moved(small_quad(), Vec3::new(0.5, 0.0, 0.0));
    let fresh = r.create_mesh(&target)?;

    let before = draw_one(r, dynamic, material, &[], |_, _| {})?;
    expect_pixel(&before, CENTRE, Colour::WHITE, tol, "the quad before the update")?;
    r.update_vertices(dynamic, &target.positions, &target.normals)?;
    let updated = draw_one(r, dynamic, material, &[], |_, _| {})?;
    let expected = draw_one(r, fresh, material, &[], |_, _| {})?;
    expect_pixel(&updated, CENTRE, Colour::BLACK, tol, "the old position is now empty")?;
    expect_pixel(&updated, (CENTRE.0 + 16, CENTRE.1), Colour::WHITE, tol, "the quad at its updated position")?;
    same_image(&updated, &expected, tol, "an updated mesh against a freshly created one")?;
    for resource in [dynamic.into(), fresh.into(), material.into()] {
        r.release(resource)?;
    }
    Ok(())
}

fn updating_a_static_mesh_is_invalid(r: &mut dyn Renderer) -> Outcome {
    let quad = small_quad();
    let fixed = r.create_mesh(&quad)?;
    expect_err(r.update_vertices(fixed, &quad.positions, &quad.normals), "updating a static mesh", is_invalid)?;
    let dynamic = r.create_mesh_with(&quad, &MeshOptions { usage: MeshUsage::Dynamic, morphs: None })?;
    expect_err(r.update_vertices(dynamic, &quad.positions[..3], &quad.normals[..3]), "too few vertices", is_invalid)?;
    expect_err(r.update_vertices(dynamic, &quad.positions, &quad.normals[..3]), "too few normals", is_invalid)?;
    let mut nan = quad.positions.clone();
    nan[0] = Vec3::NAN;
    expect_err(r.update_vertices(dynamic, &nan, &quad.normals), "a non-finite position", is_invalid)?;
    r.update_vertices(dynamic, &quad.positions, &quad.normals)?;
    r.release(dynamic.into())?;
    expect_err(r.update_vertices(dynamic, &quad.positions, &quad.normals), "a released mesh", is_unknown)?;
    r.release(fixed.into())?;
    Ok(())
}

/// The small quad with every vertex coloured `colour`.
fn coloured_quad(colour: [f32; 4]) -> MeshData {
    let mut quad = small_quad();
    quad.colours = vec![colour; quad.positions.len()];
    quad
}

fn recolouring_matches_a_fresh_mesh(r: &mut dyn Renderer) -> Outcome {
    let tol = tolerance(r).max(1);
    let (red, green) = ([1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0]);
    // A static mesh: a palette change recolours meshes no runtime rewrites.
    let mesh = r.create_mesh(&coloured_quad(red))?;
    let fresh = r.create_mesh(&coloured_quad(green))?;
    let material = r.create_material(&Material { vertex_colours: true, ..Material::unlit(Colour::WHITE) })?;

    let before = draw_one(r, mesh, material, &[], |_, _| {})?;
    expect_pixel(&before, CENTRE, Colour::rgb(255, 0, 0), tol, "the quad before recolouring")?;
    r.update_colours(mesh, &coloured_quad(green).colours)?;
    let recoloured = draw_one(r, mesh, material, &[], |_, _| {})?;
    let expected = draw_one(r, fresh, material, &[], |_, _| {})?;
    expect_pixel(&recoloured, CENTRE, Colour::rgb(0, 255, 0), tol, "the quad after recolouring")?;
    same_image(&recoloured, &expected, tol, "a recoloured mesh against a freshly created one")?;
    for resource in [mesh.into(), fresh.into(), material.into()] {
        r.release(resource)?;
    }
    Ok(())
}

fn recolouring_input_is_validated(r: &mut dyn Renderer) -> Outcome {
    let quad = coloured_quad([0.5; 4]);
    let plain = r.create_mesh(&small_quad())?;
    expect_err(r.update_colours(plain, &quad.colours), "a mesh created without colours", is_invalid)?;
    let mesh = r.create_mesh(&quad)?;
    expect_err(r.update_colours(mesh, &quad.colours[..3]), "too few colours", is_invalid)?;
    let mut nan = quad.colours.clone();
    nan[1][2] = f32::NAN;
    expect_err(r.update_colours(mesh, &nan), "a non-finite colour", is_invalid)?;
    r.update_colours(mesh, &quad.colours)?;
    r.release(mesh.into())?;
    expect_err(r.update_colours(mesh, &quad.colours), "a released mesh", is_unknown)?;
    r.release(plain.into())?;
    Ok(())
}

fn zero_morph_weights_match_the_base_mesh(r: &mut dyn Renderer) -> Outcome {
    let tol = tolerance(r).max(1);
    let plain_mesh = r.create_mesh(&small_quad())?;
    let morphed = morphable(r, &small_quad(), Vec3::new(0.5, 0.0, 0.0))?;
    let material = unlit(r, Colour::WHITE)?;
    let base = draw_one(r, plain_mesh, material, &[], |_, _| {})?;
    same_image(&draw_one(r, morphed, material, &[0.0], |_, _| {})?, &base, tol, "weight 0")?;
    same_image(&draw_one(r, morphed, material, &[], |_, _| {})?, &base, tol, "no morph range at all")?;
    for resource in [plain_mesh.into(), morphed.into(), material.into()] {
        r.release(resource)?;
    }
    Ok(())
}

fn a_morph_weight_adds_its_delta(r: &mut dyn Renderer) -> Outcome {
    let tol = tolerance(r).max(1);
    let by = Vec3::new(0.5, 0.0, 0.0);
    let morphed = morphable(r, &small_quad(), by)?;
    let material = unlit(r, Colour::WHITE)?;
    for (weight, shift) in [(1.0, 1.0), (0.5, 0.5), (-1.0, -1.0)] {
        let expected_mesh = r.create_mesh(&moved(small_quad(), by * shift))?;
        let got = draw_one(r, morphed, material, &[weight], |_, _| {})?;
        let want = draw_one(r, expected_mesh, material, &[], |_, _| {})?;
        same_image(&got, &want, tol, &format!("weight {weight} against base + {weight} * delta"))?;
        r.release(expected_mesh.into())?;
    }
    r.release(morphed.into())?;
    r.release(material.into())?;
    Ok(())
}

fn instances_of_one_mesh_morph_independently(r: &mut dyn Renderer) -> Outcome {
    let tol = tolerance(r).max(1);
    let morphed = morphable(r, &small_quad(), Vec3::new(0.5, 0.0, 0.0))?;
    let material = unlit(r, Colour::WHITE)?;
    let mut s = scene(Colour::BLACK);
    s.morph_weights.extend([0.0, 1.0]);
    for (row, weight_at) in [(0.5, 0), (-0.5, 1)] {
        let mut inst = Instance::new(morphed, material, Mat4::from_translation(Vec3::new(0.0, row, 0.0)));
        inst.morph = Some(MorphRange { start: weight_at, len: 1 });
        s.instances.push(inst);
    }
    let img = draw(r, &s, &plain())?;
    // Upper instance (weight 0) stays in the centre column; lower (weight 1) moved 16 px right.
    expect_pixel(&img, (CENTRE.0, CENTRE.1 - 16), Colour::WHITE, tol, "the unmorphed instance at its place")?;
    expect_pixel(&img, (CENTRE.0 + 16, CENTRE.1 - 16), Colour::BLACK, tol, "the unmorphed instance is not shifted")?;
    expect_pixel(&img, (CENTRE.0 + 16, CENTRE.1 + 16), Colour::WHITE, tol, "the morphed instance at its place")?;
    expect_pixel(&img, (CENTRE.0, CENTRE.1 + 16), Colour::BLACK, tol, "the morphed instance left its base place")?;
    r.release(morphed.into())?;
    r.release(material.into())?;
    Ok(())
}

fn morphing_happens_before_skinning(r: &mut dyn Renderer) -> Outcome {
    let tol = tolerance(r).max(1);
    let mut quad = small_quad();
    quad.joints = vec![[0, 0, 0, 0]; 4];
    quad.weights = vec![[1.0, 0.0, 0.0, 0.0]; 4];
    let morphed = morphable(r, &quad, Vec3::new(0.5, 0.0, 0.0))?;
    let material = unlit(r, Colour::WHITE)?;
    // Weight 0.5 moves the quad to x in 0..0.5; the joint then doubles x: 0..1 world, pixels 32..64. Skinning
    // first and morphing after would give x in -0.25..0.75, pixels 24..56, so the probes at 28 and 60 tell the
    // orders apart.
    let got = draw_one(r, morphed, material, &[0.5], |s, inst| {
        inst.skin = Some(JointRange { start: 0, len: 1 });
        s.joints.push(Mat4::from_scale(Vec3::new(2.0, 1.0, 1.0)));
    })?;
    let expected_mesh = r.create_mesh(&moved(quad.clone(), Vec3::new(0.25, 0.0, 0.0)))?;
    let want = draw_one(r, expected_mesh, material, &[], |s, inst| {
        inst.skin = Some(JointRange { start: 0, len: 1 });
        s.joints.push(Mat4::from_scale(Vec3::new(2.0, 1.0, 1.0)));
    })?;
    expect_pixel(&got, (60, CENTRE.1), Colour::WHITE, tol, "morph then skin reaches the right")?;
    expect_pixel(&got, (28, CENTRE.1), Colour::BLACK, tol, "morph then skin leaves the left empty")?;
    same_image(&got, &want, tol, "the hand-computed skin(morph(base))")?;
    for resource in [morphed.into(), expected_mesh.into(), material.into()] {
        r.release(resource)?;
    }
    Ok(())
}

fn morph_input_is_validated(r: &mut dyn Renderer) -> Outcome {
    let quad = small_quad();
    // A set built for 8 vertices names vertex 7, which a 4-vertex quad lacks.
    let big = MorphSet::from_dense(8, &[(Slug::new("far")?, &[Vec3::X; 8], &[])], 0.0)?;
    expect_err(
        r.create_mesh_with(&quad, &MeshOptions { morphs: Some(big), ..MeshOptions::default() }),
        "a morph set for a bigger mesh",
        is_invalid,
    )?;
    let too_many = r.capabilities().max_morph_targets as usize + 1;
    let deltas = vec![Vec3::X; quad.positions.len()];
    let names = (0..too_many).map(|i| Slug::new(format!("t{i}"))).collect::<Result<Vec<_>, _>>()?;
    let targets: Vec<(Slug, &[Vec3], &[Vec3])> = names.into_iter().map(|n| (n, deltas.as_slice(), &[][..])).collect();
    let many = MorphSet::from_dense(quad.positions.len(), &targets, 0.0)?;
    expect_err(
        r.create_mesh_with(&quad, &MeshOptions { morphs: Some(many), ..MeshOptions::default() }),
        "more morph targets than max_morph_targets",
        |e| matches!(e, RenderError::LimitExceeded { .. }),
    )?;

    let morphed = morphable(r, &quad, Vec3::X)?;
    let plain_mesh = r.create_mesh(&quad)?;
    let material = unlit(r, Colour::WHITE)?;
    let two = draw_one(r, morphed, material, &[0.0, 0.0], |_, _| {});
    expect_err(two, "two weights for a one-target mesh", is_invalid)?;
    let unmorphable = draw_one(r, plain_mesh, material, &[0.0], |_, _| {});
    expect_err(unmorphable, "weights for a mesh without morph targets", is_invalid)?;
    let nan = draw_one(r, morphed, material, &[f32::NAN], |_, _| {});
    expect_err(nan, "a non-finite weight", is_invalid)?;
    for resource in [morphed.into(), plain_mesh.into(), material.into()] {
        r.release(resource)?;
    }
    Ok(())
}

/// A 2x1 atlas, red | blue, on a quad whose UV covers it whole. A scale of 0.5 along u keeps one cell on the quad and the
/// offset (0 or 0.5) picks which; the base texture and a `Mapping::Uv` overlay both go through the transform.
fn uv_transform_atlas(r: &mut dyn Renderer) -> Outcome {
    let tol = tolerance(r).max(1);
    // Hard texels: the mock reads the nearest texel whatever the filter, so the check asks for nearest sampling too.
    let desc = TextureDesc {
        sampler: Sampler { filter: Filter::Nearest, ..Sampler::default() },
        ..TextureDesc::colour(UVec2::new(2, 1))
    };
    let atlas = r.create_texture(&desc, Some(&[255, 0, 0, 255, 0, 0, 255, 255]))?;
    let quad = r.create_mesh(&shapes::quad(Vec2::splat(0.5)))?;
    let (red, blue) = (Colour::rgb(255, 0, 0), Colour::rgb(0, 0, 255));
    let transform = |offset: f32| UvTransform { scale: Vec2::new(0.5, 1.0), offset: Vec2::new(offset, 0.0) };
    let base = |uv_transform: UvTransform| Material {
        base_texture: Some(atlas),
        uv_transform,
        ..Material::unlit(Colour::WHITE)
    };
    let overlay = |uv_transform: UvTransform| {
        let mut m = Material { uv_transform, ..Material::unlit(Colour::WHITE) };
        m.overlays.push(Overlay { texture: atlas, mapping: Mapping::Uv, strength: 1.0 });
        m
    };

    let material = r.create_material(&base(UvTransform::IDENTITY))?;
    let mut s = scene(Colour::BLACK);
    s.instances.push(Instance::new(quad, material, Mat4::IDENTITY));
    let img = draw(r, &s, &plain())?;
    expect_pixel(&img, (CENTRE.0 + 8, CENTRE.1), blue, tol, "the identity transform shows the right cell at u = 0.75")?;
    expect_pixel(&img, (CENTRE.0 - 8, CENTRE.1), red, tol, "and the left cell at u = 0.25")?;

    let variants: [(&str, &dyn Fn(UvTransform) -> Material); 2] = [("base texture", &base), ("overlay", &overlay)];
    for (name, make) in variants {
        for (offset, want) in [(0.0, red), (0.5, blue)] {
            r.update_material(material, &make(transform(offset)))?;
            let img = draw(r, &s, &plain())?;
            for dx in [-12, 12] {
                let at = (CENTRE.0.saturating_add_signed(dx), CENTRE.1);
                expect_pixel(&img, at, want, tol, &format!("the {name} at offset {offset}"))?;
            }
        }
    }

    let infinite = UvTransform { scale: Vec2::new(f32::INFINITY, 1.0), offset: Vec2::ZERO };
    expect_err(r.create_material(&base(infinite)), "an infinite UV scale", is_invalid)?;
    let not_a_number = UvTransform { scale: Vec2::ONE, offset: Vec2::new(0.0, f32::NAN) };
    expect_err(r.update_material(material, &base(not_a_number)), "a NaN UV offset", is_invalid)?;

    for resource in [quad.into(), material.into()] {
        r.release(resource)?;
    }
    r.release(atlas.into())?;
    Ok(())
}
