//! The particle checks (#810, particles.md D2 to D7): what every renderer that lists [`Pass::Particles`] must do.
//!
//! The checks are listed in [`CHECKS`](super::CHECKS). A check passes trivially on a renderer without the pass. Unless a check says otherwise: a 64 x 64 target, a
//! perspective camera at (0, 0, 5) looking at the origin with a 30 degree field of view, no tone curve, exposure 1,
//! the offline preset at 4 samples and an opaque black background. Pictures are measured (centroids, covered-pixel
//! counts), never compared pixel for pixel across renderers: sample patterns are each renderer's own.

use omuzaki_core::glam::{Mat4, UVec2, Vec2, Vec3};
use omuzaki_core::time::Seconds;
use omuzaki_render_api::particles::{NEAR_FADE_M, evaluate};
use omuzaki_render_api::{
    AlphaEncoding, AlphaMode, Background, Camera, CellPick, Colour, ColourSet, EmitterShape, Fog, Image, Instance,
    Light, Material, Orientation, ParticleParams, ParticleShading, ParticleSystemDesc, ParticleSystemId, Pass,
    Projection, Quality, RenderError, Renderer, RendererExt, Scene, Span, SpawnBatch, Sprite, SpriteShape, TextureDesc,
    Tonemap, limits,
};

use super::{Failure, Outcome, expect_err, is_invalid, is_unknown};
use crate::shapes;

const SIZE: UVec2 = UVec2::new(64, 64);

fn camera() -> Camera {
    Camera::look_at(
        Vec3::new(0.0, 0.0, 5.0),
        Vec3::ZERO,
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 30.0, near: 0.1, far: 100.0 },
    )
}

fn scene() -> Scene {
    let mut s = Scene::new(camera());
    s.background = Background::Solid { colour: Colour::BLACK };
    s.post.tonemap = Tonemap::None;
    s
}

fn quality() -> Quality {
    Quality { samples: 4, ..Quality::offline() }
}

/// An unlit white disc of `size` that moves at 1 m/s along +X and lives 10 s, with no forces.
fn mover(size: f32) -> ParticleParams {
    ParticleParams {
        lifetime: Span::fixed(10.0),
        speed: Span::fixed(1.0),
        direction: [1.0, 0.0, 0.0],
        spread_deg: 0.0,
        gravity: [0.0; 3],
        drag: 0.0,
        wind: [0.0; 3],
        flutter: None,
        size: Span::fixed(size),
        size_curve: [1.0; 4],
        orientation: Orientation::billboard(Span::fixed(0.0)),
        colours: ColourSet::one(Colour::WHITE),
        coverage: 1.0,
        shading: ParticleShading::Unlit,
        edge: None,
        glow: 0.0,
        floor: None,
    }
}

/// `params` drawn as a disc, or `None` if the renderer has no particle pass (the check then passes).
fn system(r: &mut dyn Renderer, params: ParticleParams) -> Result<Option<ParticleSystemId>, Failure> {
    if !r.capabilities().supports(Pass::Particles) {
        return Ok(None);
    }
    Ok(Some(r.create_particle_system(&ParticleSystemDesc { sprite: Sprite::Shape(SpriteShape::Disc), params })?))
}

fn draw_at(r: &mut dyn Renderer, mut s: Scene, time: f64) -> Result<Image, Failure> {
    s.frame.time = Seconds(time);
    Ok(r.render_image(SIZE, AlphaEncoding::Straight, &s, &quality())?)
}

/// The pixel a world point projects to, in pixel units (the picture's top left is (0, 0)).
fn project(p: Vec3) -> Vec2 {
    let clip = camera().projection_matrix(1.0) * camera().view() * p.extend(1.0);
    let ndc = clip.truncate() / clip.w;
    #[allow(clippy::cast_precision_loss)] // 64
    let size = SIZE.as_vec2();
    Vec2::new((ndc.x * 0.5 + 0.5) * size.x, (0.5 - ndc.y * 0.5) * size.y)
}

/// The centroid of the lit pixels, weighted by their red level, and their count.
#[allow(clippy::cast_precision_loss)] // pixel coordinates of a 64 x 64 image
fn lit(image: &Image) -> (Option<Vec2>, u32) {
    let (mut sum, mut weight, mut count) = (Vec2::ZERO, 0.0, 0);
    for y in 0..image.size.y {
        for x in 0..image.size.x {
            let level = image.colour(x, y).map_or(0, |c| c.r);
            if level > 0 {
                let w = f32::from(level);
                sum += Vec2::new(x as f32 + 0.5, y as f32 + 0.5) * w;
                weight += w;
                count += 1;
            }
        }
    }
    ((weight > 0.0).then(|| sum / weight), count)
}

fn release_all(r: &mut dyn Renderer, ids: impl IntoIterator<Item = omuzaki_render_api::ResourceRef>) -> Outcome {
    for id in ids {
        r.release(id)?;
    }
    Ok(())
}

/// One particle with `params`, born at the origin at 0: its centroid at `t` is `want`'s projection within 0.75 px.
fn lands_at(r: &mut dyn Renderer, params: ParticleParams, t: f64, want: Vec3) -> Outcome {
    let Some(id) = system(r, params)? else { return Ok(()) };
    let mut s = scene();
    s.particles.push(SpawnBatch::new(id, Mat4::IDENTITY));
    let image = draw_at(r, s, t)?;
    r.release(id.into())?;
    let (centroid, _) = lit(&image);
    let centroid = centroid.ok_or_else(|| Failure("nothing was drawn".into()))?;
    let expected = project(want);
    ensure!(
        centroid.distance(expected) <= 0.75,
        "the particle is at pixel {centroid}, expected {expected} (the projection of {want})"
    );
    Ok(())
}

pub(super) fn particles_none_without_batches(r: &mut dyn Renderer) -> Outcome {
    let without = draw_at(r, scene(), 1.0)?;
    let Some(id) = system(r, mover(0.2))? else { return Ok(()) };
    let target = r.create_target(&omuzaki_render_api::TargetDesc { size: SIZE, alpha: AlphaEncoding::Straight })?;
    let mut s = scene();
    s.frame.time = Seconds(1.0);
    let report = r.render(target, &s, &quality())?;
    let with = r.read_target(target)?;
    release_all(r, [id.into(), target.into()])?;
    ensure!(report.cpu.get(Pass::Particles).is_none(), "the Particles pass ran with no spawn batches");
    ensure!(report.particles == 0, "{} particle slots reported with no batches", report.particles);
    ensure!(with.rgba8 == without.rgba8, "a system with no batches changed the picture");
    Ok(())
}

pub(super) fn particle_ballistic(r: &mut dyn Renderer) -> Outcome {
    lands_at(r, mover(0.2), 1.0, Vec3::X)
}

pub(super) fn particle_gravity(r: &mut dyn Renderer) -> Outcome {
    lands_at(r, ParticleParams { gravity: [0.0, -2.0, 0.0], ..mover(0.2) }, 1.0, Vec3::new(1.0, -1.0, 0.0))
}

pub(super) fn particle_drag(r: &mut dyn Renderer) -> Outcome {
    lands_at(r, ParticleParams { drag: 1.0, ..mover(0.2) }, 1.0, Vec3::new(1.0 - (-1.0f32).exp(), 0.0, 0.0))
}

pub(super) fn particle_unborn_and_dead(r: &mut dyn Renderer) -> Outcome {
    let Some(id) = system(r, ParticleParams { speed: Span::fixed(0.0), ..mover(0.5) })? else { return Ok(()) };
    let mut s = scene();
    s.particles.push(SpawnBatch::new(id, Mat4::IDENTITY));
    let alive = lit(&draw_at(r, s.clone(), 5.0)?).1;
    let unborn = lit(&draw_at(r, s.clone(), -0.01)?).1;
    let dead = lit(&draw_at(r, s, 10.01)?).1;
    r.release(id.into())?;
    ensure!(alive > 0, "the particle isn't drawn while it lives");
    ensure!(unborn == 0 && dead == 0, "{unborn} pixels before its birth and {dead} after its death");
    Ok(())
}

/// A 200-particle burst of small still discs filling a ball of radius 1.
fn cloud(r: &mut dyn Renderer, density: f32) -> Result<Option<u32>, Failure> {
    let Some(id) = system(r, ParticleParams { speed: Span::fixed(0.0), ..mover(0.06) })? else { return Ok(None) };
    let mut s = scene();
    s.frame.time = Seconds(1.0);
    s.particles.push(SpawnBatch {
        count: 200,
        seed: 7,
        shape: EmitterShape::sphere(1.0),
        ..SpawnBatch::new(id, Mat4::IDENTITY)
    });
    let image =
        r.render_image(SIZE, AlphaEncoding::Straight, &s, &Quality { particle_density: density, ..quality() })?;
    r.release(id.into())?;
    Ok(Some(lit(&image).1))
}

pub(super) fn particle_density(r: &mut dyn Renderer) -> Outcome {
    let (Some(full), Some(half), Some(none)) = (cloud(r, 1.0)?, cloud(r, 0.5)?, cloud(r, 0.0)?) else { return Ok(()) };
    ensure!(none == 0, "density 0 drew {none} pixels");
    ensure!(full > 0, "density 1 drew nothing");
    let share = f64::from(half) / f64::from(full);
    ensure!((0.35..=0.65).contains(&share), "density 0.5 covers {share:.2} of what density 1 covers");
    Ok(())
}

fn burst_scene(id: ParticleSystemId) -> Scene {
    let mut s = scene();
    s.particles.push(SpawnBatch {
        count: 120,
        seed: 3,
        shape: EmitterShape::sphere(0.2),
        ..SpawnBatch::new(id, Mat4::IDENTITY)
    });
    s
}

fn burst_params() -> ParticleParams {
    ParticleParams {
        spread_deg: 180.0,
        speed: Span::new(0.3, 0.8),
        gravity: [0.0, -0.5, 0.0],
        drag: 0.5,
        colours: ColourSet::new(&[Colour::rgb(255, 90, 120), Colour::rgb(90, 210, 255)])
            .unwrap_or(ColourSet::one(Colour::WHITE)),
        ..mover(0.08)
    }
}

fn max_difference(a: &Image, b: &Image) -> u8 {
    a.rgba8.iter().zip(&b.rgba8).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0)
}

pub(super) fn particles_deterministic(r: &mut dyn Renderer) -> Outcome {
    let Some(id) = system(r, burst_params())? else { return Ok(()) };
    let first = draw_at(r, burst_scene(id), 1.5)?;
    let second = draw_at(r, burst_scene(id), 1.5)?;
    r.release(id.into())?;
    let tol = u8::from(!r.capabilities().bit_exact);
    let worst = max_difference(&first, &second);
    ensure!(worst <= tol, "the same scene twice differs by up to {worst} levels (tolerance {tol})");
    ensure!(lit(&first).1 > 0, "nothing was drawn");
    Ok(())
}

pub(super) fn particles_no_history(r: &mut dyn Renderer) -> Outcome {
    let Some(id) = system(r, burst_params())? else { return Ok(()) };
    let direct = draw_at(r, burst_scene(id), 2.0)?;
    draw_at(r, burst_scene(id), 0.0)?;
    draw_at(r, burst_scene(id), 1.0)?;
    let after = draw_at(r, burst_scene(id), 2.0)?;
    r.release(id.into())?;
    let tol = u8::from(!r.capabilities().bit_exact);
    let worst = max_difference(&direct, &after);
    ensure!(worst <= tol, "t = 2 after t = 0 and t = 1 differs from t = 2 alone by {worst} levels");
    Ok(())
}

pub(super) fn particles_premultiplied(r: &mut dyn Renderer) -> Outcome {
    let params = burst_params();
    let Some(id) = system(r, params.clone())? else { return Ok(()) };
    let mut s = burst_scene(id);
    s.background = Background::Transparent;
    s.frame.time = Seconds(1.0);
    let image = r.render_image(SIZE, AlphaEncoding::Premultiplied, &s, &quality())?;
    r.release(id.into())?;
    // Where each particle is, and how far it reaches on screen, from the reference evaluation.
    let desc = ParticleSystemDesc { sprite: Sprite::Shape(SpriteShape::Disc), params };
    let batch = s.particles[0];
    let reach: Vec<(Vec2, f32)> = (batch.first..batch.first + batch.count)
        .filter_map(|g| evaluate(&desc, &batch, g, s.frame.time, 1.0))
        .map(|p| {
            let centre = project(p.position);
            let edge = project(p.position + Vec3::Y * p.size);
            (centre, centre.distance(edge).max(1.5))
        })
        .collect();
    let mut covered = 0;
    for y in 0..SIZE.y {
        for x in 0..SIZE.x {
            let c = image.colour(x, y).ok_or_else(|| Failure("outside".into()))?;
            ensure!(c.r <= c.a && c.g <= c.a && c.b <= c.a, "pixel ({x}, {y}) {c} isn't premultiplied");
            #[allow(clippy::cast_precision_loss)]
            let at = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
            let far = reach.iter().all(|&(centre, radius)| at.distance(centre) > radius + 3.0);
            ensure!(!far || c == Colour::TRANSPARENT, "pixel ({x}, {y}) is {c}, 3 px from every particle");
            covered += u32::from(c.a > 0);
        }
    }
    ensure!(covered > 0, "nothing was drawn");
    Ok(())
}

/// A still particle of `colour` and `size` at `z`.
fn still_at(r: &mut dyn Renderer, colour: Colour, size: f32) -> Result<Option<ParticleSystemId>, Failure> {
    system(r, ParticleParams { speed: Span::fixed(0.0), colours: ColourSet::one(colour), ..mover(size) })
}

pub(super) fn particle_depth(r: &mut dyn Renderer) -> Outcome {
    let Some(id) = still_at(r, Colour::rgb(0, 255, 0), 0.4)? else { return Ok(()) };
    let mesh = r.create_mesh(&shapes::quad(Vec2::splat(1.0)))?;
    let wall = r.create_material(&Material::unlit(Colour::rgb(255, 0, 0)))?;
    let green_at = |r: &mut dyn Renderer, z: f32| -> Result<u8, Failure> {
        let mut s = scene();
        s.instances.push(Instance::new(mesh, wall, Mat4::IDENTITY));
        s.particles.push(SpawnBatch::new(id, Mat4::from_translation(Vec3::new(0.0, 0.0, z))));
        let image = draw_at(r, s, 1.0)?;
        Ok(image.colour(32, 32).map_or(0, |c| c.g))
    };
    let (behind, front) = (green_at(r, -1.0)?, green_at(r, 1.0)?);
    release_all(r, [id.into(), mesh.into(), wall.into()])?;
    ensure!(behind == 0, "a particle behind an opaque quad shows through (green {behind})");
    ensure!(front > 200, "a particle in front of an opaque quad is hidden (green {front})");
    Ok(())
}

/// The summed red level of a still white disc `d` metres in front of a camera with a 1 cm near plane, sized to
/// `0.1 d` so it covers the same pixels at any depth.
fn red_at_depth(r: &mut dyn Renderer, id: ParticleSystemId, d: f32) -> Result<u32, Failure> {
    let mut s = scene();
    s.camera = Camera::look_at(
        Vec3::new(0.0, 0.0, 5.0),
        Vec3::ZERO,
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 30.0, near: 0.01, far: 100.0 },
    );
    s.particles.push(SpawnBatch {
        scale: 10.0 * d,
        ..SpawnBatch::new(id, Mat4::from_translation(Vec3::new(0.0, 0.0, 5.0 - d)))
    });
    let image = draw_at(r, s, 1.0)?;
    Ok(image.rgba8.as_chunks::<4>().0.iter().map(|p| u32::from(p[0])).sum())
}

pub(super) fn particle_near_fade(r: &mut dyn Renderer) -> Outcome {
    let Some(id) = still_at(r, Colour::WHITE, 0.01)? else { return Ok(()) };
    let [from, to] = NEAR_FADE_M;
    let at_lens = red_at_depth(r, id, 0.5 * from)?;
    let midway = red_at_depth(r, id, f32::midpoint(from, to))?;
    let clear = red_at_depth(r, id, 1.0)?;
    r.release(id.into())?;
    ensure!(at_lens == 0, "a particle nearer than the near fade is drawn (red {at_lens})");
    ensure!(clear > 0, "a particle 1 m from the eye isn't drawn");
    #[allow(clippy::cast_precision_loss)] // sums of a 64 x 64 image
    let share = midway as f32 / clear as f32;
    ensure!((0.25..=0.75).contains(&share), "midway through the near fade a particle shows {share:.2} of itself");
    Ok(())
}

pub(super) fn particle_before_blended(r: &mut dyn Renderer) -> Outcome {
    let Some(id) = still_at(r, Colour::rgb(0, 0, 255), 0.4)? else { return Ok(()) };
    let mesh = r.create_mesh(&shapes::quad(Vec2::splat(1.0)))?;
    let mut glass = Material::unlit(Colour::rgba(255, 0, 0, 128));
    glass.alpha = AlphaMode::Blend;
    let glass = r.create_material(&glass)?;
    let mut s = scene();
    s.instances.push(Instance::new(mesh, glass, Mat4::IDENTITY));
    s.particles.push(SpawnBatch::new(id, Mat4::from_translation(Vec3::new(0.0, 0.0, 1.0))));
    let image = draw_at(r, s, 1.0)?;
    release_all(r, [id.into(), mesh.into(), glass.into()])?;
    let centre = image.colour(32, 32).ok_or_else(|| Failure("outside".into()))?;
    let beside = image.colour(12, 32).ok_or_else(|| Failure("outside".into()))?;
    ensure!(beside.r > 100, "the blended quad isn't drawn ({beside})");
    ensure!(centre.r == 0 && centre.b > 200, "a particle in front of a blended quad is tinted by it ({centre})");
    Ok(())
}

pub(super) fn particle_fog(r: &mut dyn Renderer) -> Outcome {
    let Some(id) = still_at(r, Colour::WHITE, 0.6)? else { return Ok(()) };
    let mut s = scene();
    s.fog = Some(Fog { colour: Colour::BLACK, near: 4.0, far: 6.0 });
    s.particles.push(SpawnBatch::new(id, Mat4::IDENTITY));
    let image = draw_at(r, s, 1.0)?;
    r.release(id.into())?;
    let got = image.colour(32, 32).map_or(0, |c| c.r);
    ensure!(got.abs_diff(188) <= 2, "a white particle half into the fog is {got}, expected 188 (0.5 linear)");
    Ok(())
}

pub(super) fn particle_toon(r: &mut dyn Renderer) -> Outcome {
    if !r.capabilities().supports(Pass::Particles) {
        return Ok(());
    }
    let (albedo, shade) = (Colour::rgb(200, 180, 160), Colour::rgb(128, 128, 192));
    let params = ParticleParams {
        speed: Span::fixed(0.0),
        colours: ColourSet::one(albedo),
        shading: ParticleShading::toon(shade, 0.0, 0.1),
        ..mover(0.6)
    };
    let id = r.create_particle_system(&ParticleSystemDesc { sprite: Sprite::Shape(SpriteShape::Square), params })?;
    let lit_from = |r: &mut dyn Renderer, to_light: Vec3| -> Result<Colour, Failure> {
        let mut s = scene();
        // Intensity pi: a white light facing the surface gives the albedo itself (render-api D10's units).
        s.lights = vec![Light::Directional {
            to_light,
            colour: Colour::WHITE,
            intensity: std::f32::consts::PI,
            casts_shadow: false,
        }];
        s.particles.push(SpawnBatch::new(id, Mat4::IDENTITY));
        let image = draw_at(r, s, 1.0)?;
        image.colour(32, 32).ok_or_else(|| Failure("outside".into()))
    };
    let (front, back) = (lit_from(r, Vec3::Z)?, lit_from(r, -Vec3::Z)?);
    r.release(id.into())?;
    let encode = |v: Vec3| Colour::from_linear(v, 1.0);
    let lit_want = encode(albedo.to_linear());
    let shade_want = encode(albedo.to_linear() * shade.to_linear());
    let near = |a: Colour, b: Colour| [(a.r, b.r), (a.g, b.g), (a.b, b.b)].iter().all(|&(x, y)| x.abs_diff(y) <= 3);
    ensure!(near(front, lit_want), "lit from the camera: {front}, expected {lit_want}");
    ensure!(near(back, shade_want), "lit from behind: {back}, expected the shade {shade_want}");
    Ok(())
}

pub(super) fn particle_atlas_cell(r: &mut dyn Renderer) -> Outcome {
    if !r.capabilities().supports(Pass::Particles) {
        return Ok(());
    }
    // Two texels a cell each way, so a filtered read at the cell's centre sees only the cell.
    let mut desc = TextureDesc::colour(UVec2::new(4, 2));
    desc.mips = false;
    let (red, green) = ([255, 0, 0, 255], [0, 255, 0, 255]);
    let row = [red, red, green, green];
    let texture =
        r.create_texture(&desc, Some(row.iter().chain(&row).flatten().copied().collect::<Vec<u8>>().as_slice()))?;
    let params = ParticleParams { speed: Span::fixed(0.0), ..mover(0.6) };
    let id = r.create_particle_system(&ParticleSystemDesc { sprite: Sprite::atlas(texture, 2, 1), params })?;
    let mut s = scene();
    s.particles.push(SpawnBatch {
        cells: CellPick::list(&[1]).ok_or_else(|| Failure("cells".into()))?,
        ..SpawnBatch::new(id, Mat4::IDENTITY)
    });
    let image = draw_at(r, s, 1.0)?;
    release_all(r, [id.into(), texture.into()])?;
    let c = image.colour(32, 32).ok_or_else(|| Failure("outside".into()))?;
    ensure!(c.g > 200 && c.r < 40, "cell 1 of a red-green atlas is {c}, expected green");
    Ok(())
}

pub(super) fn particle_handles(r: &mut dyn Renderer) -> Outcome {
    if !r.capabilities().supports(Pass::Particles) {
        return Ok(());
    }
    let before = r.stats().particle_systems;
    let texture = r.create_texture(&TextureDesc::colour(UVec2::new(2, 2)), None)?;
    let id =
        r.create_particle_system(&ParticleSystemDesc { sprite: Sprite::atlas(texture, 2, 2), params: mover(0.2) })?;
    ensure!(r.stats().particle_systems == before + 1, "stats don't count the new system");
    expect_err(r.release(texture.into()), "releasing an atlas a live system reads", |e| {
        matches!(e, RenderError::InUse { .. })
    })?;
    r.release(id.into())?;
    ensure!(r.stats().particle_systems == before, "stats still count a released system");
    let mut s = scene();
    s.particles.push(SpawnBatch::new(id, Mat4::IDENTITY));
    expect_err(
        r.render_image(SIZE, AlphaEncoding::Straight, &s, &quality()),
        "a batch of a released system",
        is_unknown,
    )?;
    r.release(texture.into())?;
    Ok(())
}

pub(super) fn particle_limits(r: &mut dyn Renderer) -> Outcome {
    if !r.capabilities().supports(Pass::Particles) {
        return Ok(());
    }
    let id =
        r.create_particle_system(&ParticleSystemDesc { sprite: Sprite::Shape(SpriteShape::Disc), params: mover(0.2) })?;
    let over_limit = |e: &RenderError| matches!(e, RenderError::LimitExceeded { .. });
    let mut s = scene();
    s.particles.push(SpawnBatch { count: limits::MAX_PARTICLES + 1, ..SpawnBatch::new(id, Mat4::IDENTITY) });
    let too_many = r.render_image(SIZE, AlphaEncoding::Straight, &s, &quality());
    s.particles.clear();
    s.particles.resize(limits::MAX_SPAWN_BATCHES + 1, SpawnBatch::new(id, Mat4::IDENTITY));
    let too_many_batches = r.render_image(SIZE, AlphaEncoding::Straight, &s, &quality());
    s.particles.clear();
    s.particles.push(SpawnBatch::new(id, Mat4::from_translation(Vec3::NAN)));
    let not_finite = r.render_image(SIZE, AlphaEncoding::Straight, &s, &quality());
    r.release(id.into())?;
    expect_err(too_many, "more than MAX_PARTICLES slots", over_limit)?;
    expect_err(too_many_batches, "more than MAX_SPAWN_BATCHES batches", over_limit)?;
    expect_err(not_finite, "a non-finite emitter", is_invalid)?;
    Ok(())
}
