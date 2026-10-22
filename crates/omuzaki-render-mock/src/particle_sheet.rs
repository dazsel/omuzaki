//! The particle contact sheet (#809, #940): every sprite shape with and without its edge, the streak, a tumbling toon
//! strip at three times and an atlas cell, one case per cell. The mock's goldens and the GPU's are drawn from it, so
//! the two renderers can be measured case by case.

use omuzaki_core::glam::{Mat4, UVec2, Vec3};
use omuzaki_core::time::Seconds;
use omuzaki_render_api::{
    AlphaEncoding, Background, Camera, CellPick, Colour, ColourSet, Edge, Image, Light, Orientation, ParticleParams,
    ParticleShading, ParticleSystemDesc, Quality, RenderError, Renderer, RendererExt, Scene, Span, SpawnBatch, Sprite,
    SpriteShape, TextureDesc,
};

/// The size of one case.
pub const CELL: UVec2 = UVec2::new(192, 192);
/// Cases per row.
pub const COLUMNS: u32 = 6;

/// A still particle: no speed, no gravity, a long life.
#[must_use]
pub fn still(size: f32, orientation: Orientation, colour: Colour) -> ParticleParams {
    ParticleParams {
        lifetime: Span::fixed(20.0),
        speed: Span::fixed(0.0),
        direction: [0.0, 1.0, 0.0],
        spread_deg: 0.0,
        gravity: [0.0; 3],
        drag: 0.0,
        wind: [0.0; 3],
        flutter: None,
        size: Span::fixed(size),
        size_curve: [1.0; 4],
        orientation,
        colours: ColourSet::one(colour),
        coverage: 1.0,
        shading: ParticleShading::Unlit,
        edge: None,
        glow: 0.0,
        floor: None,
    }
}

/// Three metres back on +Z, looking at the origin.
#[must_use]
pub fn camera() -> Camera {
    Camera::look_at(Vec3::new(0.0, 0.0, 3.0), Vec3::ZERO, Vec3::Y, Camera::default().projection)
}

/// A soft sky and a warm key from the upper left, for the toon-lit cases.
#[must_use]
pub fn lights() -> Vec<Light> {
    vec![
        Light::Hemisphere { sky: Colour::hex(0x00f3_e8ff), ground: Colour::hex(0x003a_2a24), intensity: 0.6 },
        Light::Directional {
            to_light: Vec3::new(-0.6, 1.0, 1.5),
            colour: Colour::hex(0x00ff_f0e0),
            intensity: 2.5,
            casts_shadow: false,
        },
    ]
}

/// The offline preset at four samples.
#[must_use]
pub fn quality() -> Quality {
    Quality { samples: 4, ..Quality::offline() }
}

/// One case: a system and the time it is drawn at.
pub struct Case {
    /// The system drawn.
    pub desc: ParticleSystemDesc,
    /// The scene time, seconds.
    pub time: f64,
    /// The batch's atlas cells.
    pub cells: CellPick,
}

/// A 2 x 2 atlas, premultiplied: red, green, blue and a white ring, so the cell picked is plain to see.
#[must_use]
#[allow(clippy::cast_precision_loss)] // 32 x 32
pub fn atlas_pixels() -> Vec<u8> {
    let mut rgba = Vec::new();
    for y in 0..32u32 {
        for x in 0..32u32 {
            let (cx, cy) = (x / 16, y / 16);
            let (lx, ly) = ((x % 16) as f32 - 7.5, (y % 16) as f32 - 7.5);
            let d = (lx * lx + ly * ly).sqrt();
            let px = match (cx, cy) {
                (0, 0) => [255, 0, 0, 255],
                (1, 0) => [0, 255, 0, 255],
                (0, 1) => [0, 0, 255, 255],
                _ if (4.0..7.0).contains(&d) => [255, 255, 255, 255],
                _ => [0, 0, 0, 0],
            };
            rgba.extend(px);
        }
    }
    rgba
}

/// The cases, in sheet order. The atlas texture is created on `renderer`; the caller releases it (the last case's
/// sprite names it).
///
/// # Errors
/// What the renderer returns for the atlas.
pub fn cases(renderer: &mut dyn Renderer) -> Result<Vec<Case>, RenderError> {
    let pink = Colour::hex(0x00ff_5a7a);
    let shapes = [
        SpriteShape::Square,
        SpriteShape::rect(0.6),
        SpriteShape::Disc,
        SpriteShape::Petal,
        SpriteShape::star(5),
        SpriteShape::Heart,
    ];
    let mut out = Vec::new();
    for edge in [None, Some(Edge { shade: 0.45, width_px: 3.0 })] {
        for shape in shapes {
            let params = ParticleParams { edge, ..still(1.1, Orientation::billboard(Span::fixed(0.0)), pink) };
            out.push(Case {
                desc: ParticleSystemDesc { sprite: Sprite::Shape(shape), params },
                time: 1.0,
                cells: CellPick::All,
            });
        }
        // The streak stands upright: it follows a velocity straight up.
        let mut params = ParticleParams {
            edge,
            speed: Span::fixed(0.2),
            ..still(0.25, Orientation::velocity(4.0), Colour::hex(0x00c8_d6ec))
        };
        params.direction = [0.0, 1.0, 0.0];
        out.push(Case {
            desc: ParticleSystemDesc { sprite: Sprite::Shape(SpriteShape::Streak), params },
            time: 0.0,
            cells: CellPick::All,
        });
    }
    for step in 0..3 {
        let params = ParticleParams {
            shading: ParticleShading::toon(Colour::hex(0x00b8_a8c8), 0.0, 0.1),
            ..still(1.1, Orientation::tumble(Span::fixed(45.0)), Colour::hex(0x00ff_c94d))
        };
        let desc = ParticleSystemDesc { sprite: Sprite::Shape(SpriteShape::rect(0.6)), params };
        out.push(Case { desc, time: 2.0 + f64::from(step), cells: CellPick::All });
    }
    let mut desc = TextureDesc::colour(UVec2::new(32, 32));
    desc.alpha = AlphaEncoding::Premultiplied;
    desc.mips = false;
    let texture = renderer.create_texture(&desc, Some(&atlas_pixels()))?;
    let params = still(1.1, Orientation::billboard(Span::fixed(0.0)), Colour::WHITE);
    out.push(Case {
        desc: ParticleSystemDesc { sprite: Sprite::atlas(texture, 2, 2), params },
        time: 1.0,
        cells: CellPick::list(&[3]).ok_or_else(|| RenderError::invalid("cells", "one cell"))?,
    });
    Ok(out)
}

/// Every case side by side, [`COLUMNS`] to a row, over `background` into a target of `alpha`. Releases what it makes.
///
/// # Errors
/// The first error the renderer returns.
pub fn sheet(renderer: &mut dyn Renderer, background: Background, alpha: AlphaEncoding) -> Result<Image, RenderError> {
    let cases = cases(renderer)?;
    let rows = u32::try_from(cases.len()).unwrap_or(0).div_ceil(COLUMNS);
    let size = UVec2::new(CELL.x * COLUMNS, CELL.y * rows);
    let mut sheet = vec![0u8; (size.x * size.y * 4) as usize];
    let row_bytes = (CELL.x * 4) as usize;
    for (i, case) in (0u32..).zip(&cases) {
        let system = renderer.create_particle_system(&case.desc)?;
        let mut scene = Scene::new(camera());
        scene.background = background;
        scene.lights = lights();
        scene.frame.time = Seconds(case.time);
        scene.particles.push(SpawnBatch { cells: case.cells, seed: 3, ..SpawnBatch::new(system, Mat4::IDENTITY) });
        let image = renderer.render_image(CELL, alpha, &scene, &quality());
        renderer.release(system.into())?;
        let image = image?;
        let (cx, cy) = (i % COLUMNS, i / COLUMNS);
        for y in 0..CELL.y {
            let from = (y * CELL.x * 4) as usize;
            let to = (((cy * CELL.y + y) * size.x + cx * CELL.x) * 4) as usize;
            sheet[to..to + row_bytes].copy_from_slice(&image.rgba8[from..from + row_bytes]);
        }
    }
    if let Some(texture) = cases.last().and_then(|c| c.desc.sprite.texture()) {
        renderer.release(texture.into())?;
    }
    Ok(Image { size, alpha, rgba8: sheet })
}
