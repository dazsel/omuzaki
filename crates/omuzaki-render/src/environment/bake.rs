//! The CPU half of an environment: everything a frame would otherwise have to filter, computed once at creation.
//!
//! - **Diffuse.** The radiance map is projected on nine spherical harmonics (Ramamoorthi and Hanrahan, "An Efficient
//!   Representation for Irradiance Environment Maps"), which give the cosine-weighted irradiance for any normal in a
//!   few multiplications. Every texel is integrated over its footprint on the sphere and weighted by solid angle, so a
//!   coarse map projects to its true average instead of putting its whole weight on one direction. This is the same
//!   maths as the mock (`omuzaki-render-mock`), written separately so the conformance suite compares two
//!   implementations.
//! - **Specular.** A cube map with a mip chain whose level `i` holds the sky blurred by a GGX lobe of roughness
//!   `i / (levels - 1)` (the split-sum prefilter of Karis, "Real Shading in Unreal Engine 4"). Each texel averages
//!   importance-sampled directions, reading a mip pyramid of the source chosen by the sample's solid angle so a small
//!   bright sun cannot flicker through the noise of 64 samples.
//!
//! Directions follow the contract's conventions (`omuzaki_render_api::EnvironmentDesc`): `+Y` up, and an equirectangular
//! map has `u = atan2(z, x) / 2π + 0.5` with rows running from straight up to straight down.

use std::borrow::Cow;
use std::f32::consts::{PI, TAU};
use std::f64::consts::PI as PI64;

use omuzaki_core::glam::{UVec2, Vec3};
use omuzaki_render_api::{Colour, EnvironmentDesc, RenderError, sky_radiance};

/// Side of the specular cube's base level, in texels.
pub(super) const SPECULAR_FACE: u32 = 64;
/// Levels of the specular chain, from roughness 0 at level 0 to roughness 1 at the last.
pub(super) const SPECULAR_LEVELS: u32 = 5;
/// Directions averaged per specular texel above level 0.
const GGX_SAMPLES: u32 = 64;
/// The grid a procedural sky is rendered on for the specular pyramid.
const SKY_PYRAMID: UVec2 = UVec2::new(128, 64);
/// The grid a procedural sky stands on for the harmonics (sampled analytically within each texel).
const SKY_GRID: (u32, u32) = (64, 32);
/// Sub-samples per axis the harmonics projection aims for over the whole sphere (see [`project`]).
const MIN_SAMPLES: (u32, u32) = (128, 64);

/// What the frame needs of the diffuse light, in the layout the shader reads.
pub(super) struct Diffuse {
    /// Nine harmonic coefficients (rgb in the first three lanes) already multiplied by the cosine lobe's band factors, so
    /// the shader's irradiance is the dot product with the basis.
    pub(super) packed: [[f32; 4]; 9],
    /// The luminance-weighted mean direction of the sky (the direction the toon ramp splits the sky along).
    pub(super) toward: Vec3,
    /// The irradiance facing away from `toward`.
    pub(super) dark: Vec3,
    /// The irradiance facing `toward`.
    pub(super) bright: Vec3,
}

/// The specular chain: per level, six faces of `size * size` RGBA texels (alpha 1) as `f32`.
pub(super) struct Specular {
    pub(super) levels: Vec<Vec<f32>>,
}

/// Everything derived from a radiance map.
pub(super) struct Baked {
    pub(super) diffuse: Diffuse,
    pub(super) specular: Specular,
}

/// Derives the diffuse light and the specular chain of `desc`.
///
/// # Errors
/// `Invalid` per [`EnvironmentDesc::validate`].
pub(super) fn bake(desc: &EnvironmentDesc) -> Result<Baked, RenderError> {
    desc.validate()?;
    let source = Source::new(desc);
    let diffuse = Harmonics::project(&source).diffuse();
    let specular = prefilter(&Pyramid::new(&source));
    Ok(Baked { diffuse, specular })
}

/// A radiance map: a procedural sky or an equirectangular HDRI.
enum Source<'a> {
    Sky { zenith: Colour, horizon: Colour, ground: Colour },
    Hdri { size: UVec2, rgb: &'a [f32] },
}

impl<'a> Source<'a> {
    fn new(desc: &'a EnvironmentDesc) -> Self {
        match desc {
            EnvironmentDesc::Sky { zenith, horizon, ground } => {
                Self::Sky { zenith: *zenith, horizon: *horizon, ground: *ground }
            }
            EnvironmentDesc::Hdri { size, rgb } => Self::Hdri { size: *size, rgb },
        }
    }

    /// The grid the harmonics are projected from.
    fn grid(&self) -> (u32, u32) {
        match self {
            Self::Sky { .. } => SKY_GRID,
            Self::Hdri { size, .. } => (size.x, size.y),
        }
    }

    /// The colour of texel `(x, y)`, where the sample's direction has height `up` (a sky varies with it, an HDRI's
    /// texel is constant).
    fn radiance(&self, x: u32, y: u32, up: f64) -> [f64; 3] {
        match self {
            #[allow(clippy::cast_possible_truncation)] // the sky is defined in f32
            Self::Sky { zenith, horizon, ground } => {
                let c = sky_radiance(*zenith, *horizon, *ground, up as f32);
                [f64::from(c.x), f64::from(c.y), f64::from(c.z)]
            }
            Self::Hdri { size, rgb } => {
                let at = (y as usize * size.x as usize + x as usize) * 3;
                [f64::from(rgb[at]), f64::from(rgb[at + 1]), f64::from(rgb[at + 2])]
            }
        }
    }
}

// ---- diffuse ----

/// The values of the nine real spherical harmonics towards the unit vector `[x, y, z]`.
fn basis([x, y, z]: [f64; 3]) -> [f64; 9] {
    [
        0.282_095,
        0.488_603 * y,
        0.488_603 * z,
        0.488_603 * x,
        1.092_548 * x * y,
        1.092_548 * y * z,
        0.315_392 * (3.0 * z * z - 1.0),
        1.092_548 * x * z,
        0.546_274 * (x * x - y * y),
    ]
}

/// Cosine-lobe convolution factors per coefficient (π, 2π/3 and π/4 by band): radiance coefficients to irradiance.
const BAND: [f64; 9] = {
    const A: [f64; 3] = [PI64, 2.0 * PI64 / 3.0, PI64 / 4.0];
    [A[0], A[1], A[1], A[1], A[2], A[2], A[2], A[2], A[2]]
};

fn luminance(c: [f64; 3]) -> f64 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

/// A radiance map reduced to nine harmonics per colour channel.
struct Harmonics([[f64; 3]; 9]);

impl Harmonics {
    /// Projects `source`, integrating every texel over its footprint.
    ///
    /// A texel is split into a grid of sub-samples until the whole map is at least [`MIN_SAMPLES`] fine, each weighted
    /// by its solid angle (the cosine of its elevation). The weights are rescaled to sum to exactly 4π, so a uniform map
    /// projects to exactly its colour however coarse it is.
    fn project(source: &Source<'_>) -> Self {
        let (width, height) = source.grid();
        // `width` is at most the texture limit and `sub_x` is 1 once it reaches 128, so these cannot overflow.
        let (sub_x, sub_y) = (MIN_SAMPLES.0.div_ceil(width), MIN_SAMPLES.1.div_ceil(height));
        let (w, h) = (width * sub_x, height * sub_y);
        // u = atan2(z, x) / 2π + 0.5: the angle of each column, tabulated once instead of per texel.
        let columns: Vec<(f64, f64)> = (0..w)
            .map(|sx| {
                let phi = ((f64::from(sx) + 0.5) / f64::from(w) - 0.5) * 2.0 * PI64;
                (phi.cos(), phi.sin())
            })
            .collect();
        let mut sh = [[0.0; 3]; 9];
        let mut total = 0.0;
        for sy in 0..h {
            let elevation = (0.5 - (f64::from(sy) + 0.5) / f64::from(h)) * PI64;
            let (up, ring) = (elevation.sin(), elevation.cos());
            for (sx, &(cos, sin)) in (0..w).zip(&columns) {
                let weights = basis([ring * cos, up, ring * sin]);
                let colour = source.radiance(sx / sub_x, sy / sub_y, up);
                for (coefficient, weight) in sh.iter_mut().zip(weights) {
                    for (c, l) in coefficient.iter_mut().zip(colour) {
                        *c += l * weight * ring;
                    }
                }
                total += ring;
            }
        }
        let scale = 4.0 * PI64 / total;
        sh.iter_mut().flatten().for_each(|c| *c *= scale);
        Self(sh)
    }

    /// The diffuse irradiance (`∫ L max(n·ω, 0) dω`) for a unit normal, in linear light.
    fn irradiance(&self, n: Vec3) -> Vec3 {
        let y = basis([f64::from(n.x), f64::from(n.y), f64::from(n.z)]);
        let mut out = [0.0; 3];
        for ((coefficient, y), band) in self.0.iter().zip(y).zip(BAND) {
            for (o, c) in out.iter_mut().zip(coefficient) {
                *o += c * y * band;
            }
        }
        // The nine-term fit can ring slightly below zero beside a very bright source.
        #[allow(clippy::cast_possible_truncation)] // back to the renderer's f32
        Vec3::new(out[0] as f32, out[1] as f32, out[2] as f32).max(Vec3::ZERO)
    }

    /// The luminance-weighted mean direction of the sky (the first band points at it), or `+Y` when the sky has none.
    fn brightest(&self) -> Vec3 {
        #[allow(clippy::cast_possible_truncation)] // back to the renderer's f32
        let [y, z, x] = [1, 2, 3].map(|k| luminance(self.0[k]) as f32);
        let direction = Vec3::new(x, y, z);
        // A uniform sky has first-band coefficients of rounding noise; against the average they are no direction at all.
        #[allow(clippy::cast_possible_truncation)] // back to the renderer's f32
        let average = luminance(self.0[0]).abs() as f32 * 0.282_095;
        if direction.length() <= 1e-4 * average.max(f32::MIN_POSITIVE) { Vec3::Y } else { direction.normalize() }
    }

    fn diffuse(&self) -> Diffuse {
        let toward = self.brightest();
        #[allow(clippy::cast_possible_truncation)] // the GPU works in f32
        let packed = std::array::from_fn(|k| {
            let [r, g, b] = self.0[k].map(|c| (c * BAND[k]) as f32);
            [r, g, b, 0.0]
        });
        Diffuse { packed, toward, dark: self.irradiance(-toward), bright: self.irradiance(toward) }
    }
}

// ---- specular ----

/// One level of the source pyramid.
struct Level<'a> {
    size: UVec2,
    rgb: Cow<'a, [f32]>,
}

impl Level<'_> {
    fn texel(&self, x: usize, y: usize) -> Vec3 {
        let at = (y * self.size.x as usize + x) * 3;
        Vec3::new(self.rgb[at], self.rgb[at + 1], self.rgb[at + 2])
    }

    /// Bilinear lookup at `(u, v)`: the columns wrap around the sky, the rows stop at the poles.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // texel indices < 2^24
    fn bilinear(&self, u: f32, v: f32) -> Vec3 {
        let (width, height) = (self.size.x as f32, self.size.y as f32);
        let (fx, fy) = (u * width - 0.5, v * height - 0.5);
        let (base_x, base_y) = (fx.floor(), fy.floor());
        let column = |step: f32| (base_x + step).rem_euclid(width) as usize;
        let row = |step: f32| (base_y + step).clamp(0.0, height - 1.0) as usize;
        let top = self.texel(column(0.0), row(0.0)).lerp(self.texel(column(1.0), row(0.0)), fx - base_x);
        let bottom = self.texel(column(0.0), row(1.0)).lerp(self.texel(column(1.0), row(1.0)), fx - base_x);
        top.lerp(bottom, fy - base_y)
    }

    /// The level half the size, rounded up (never below one texel a side). The destination grid does not line up with the
    /// source grid when a side is odd, so each destination texel integrates the source texels it *overlaps*, each
    /// weighted by the solid angle of the overlap (its width in `u` times the band of latitudes it spans). The mean
    /// radiance over the sphere is therefore preserved exactly at every level, for any size.
    fn halved(&self) -> Level<'static> {
        let size = UVec2::new(self.size.x.div_ceil(2).max(1), self.size.y.div_ceil(2).max(1));
        let columns = overlaps(self.size.x as usize, size.x as usize, |lo, hi| hi - lo);
        let rows = overlaps(self.size.y as usize, size.y as usize, band);
        let mut rgb = Vec::with_capacity(size.x as usize * size.y as usize * 3);
        for row_list in &rows {
            for column_list in &columns {
                let (mut sum, mut total) = ([0.0_f64; 3], 0.0_f64);
                for &(row, row_weight) in row_list {
                    for &(column, column_weight) in column_list {
                        let weight = row_weight * column_weight;
                        let t = self.texel(column, row);
                        for (s, c) in sum.iter_mut().zip(t.to_array()) {
                            *s += f64::from(c) * weight;
                        }
                        total += weight;
                    }
                }
                #[allow(clippy::cast_possible_truncation)] // back to f32
                rgb.extend(sum.map(|s| (s / total) as f32));
            }
        }
        Level { size, rgb: Cow::Owned(rgb) }
    }
}

/// The solid angle, up to the constant `2π`, between the heights `v_lo` and `v_hi` of an equirectangular map (`v` runs
/// 0 at the north pole to 1 at the south pole, so the latitude is `π (0.5 - v)`): `sin(lat_lo) - sin(lat_hi)`.
fn band(v_lo: f64, v_hi: f64) -> f64 {
    ((0.5 - v_lo) * PI64).sin() - ((0.5 - v_hi) * PI64).sin()
}

/// For each of `dst` equal cells along an axis, the `src` equal cells it overlaps with the weight `measure` gives the
/// overlapping interval `(lo, hi)` (in `0..1` along the axis).
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)] // cell counts far below 2^52; indices are non-negative
fn overlaps(src: usize, dst: usize, measure: impl Fn(f64, f64) -> f64) -> Vec<Vec<(usize, f64)>> {
    (0..dst)
        .map(|cell| {
            let (lo, hi) = (cell as f64 / dst as f64, (cell + 1) as f64 / dst as f64);
            let first = (lo * src as f64).floor() as usize;
            let last = ((hi * src as f64).ceil() as usize).min(src);
            (first..last)
                .filter_map(|index| {
                    let (a, b) = ((index as f64 / src as f64).max(lo), ((index + 1) as f64 / src as f64).min(hi));
                    (b > a).then(|| (index, measure(a, b)))
                })
                .collect()
        })
        .collect()
}

/// The source radiance map and its solid-angle-weighted halves down to one texel: the mip chain the prefilter reads.
struct Pyramid<'a> {
    levels: Vec<Level<'a>>,
}

impl<'a> Pyramid<'a> {
    fn new(source: &Source<'a>) -> Self {
        let base = match source {
            Source::Hdri { size, rgb } => Level { size: *size, rgb: Cow::Borrowed(rgb) },
            Source::Sky { .. } => {
                let mut rgb = Vec::with_capacity((SKY_PYRAMID.x * SKY_PYRAMID.y * 3) as usize);
                for y in 0..SKY_PYRAMID.y {
                    #[allow(clippy::cast_precision_loss)] // a 64-row grid
                    let up = ((0.5 - (y as f32 + 0.5) / SKY_PYRAMID.y as f32) * PI).sin();
                    let colour = source.radiance(0, 0, f64::from(up)).map(|c| {
                        #[allow(clippy::cast_possible_truncation)] // back to f32
                        {
                            c as f32
                        }
                    });
                    (0..SKY_PYRAMID.x).for_each(|_| rgb.extend(colour));
                }
                Level { size: SKY_PYRAMID, rgb: Cow::Owned(rgb) }
            }
        };
        let mut levels = vec![base];
        while let Some(last) = levels.last().filter(|l| l.size.x > 1 || l.size.y > 1) {
            let next = last.halved();
            levels.push(next);
        }
        Self { levels }
    }

    /// Solid angle of a texel of the base level at the equator, `(2π / w)(π / h)`: the footprint a lookup is filtered
    /// to. Towards the poles a texel narrows but keeps its height, so measuring it by its (smaller) polar area would pick
    /// levels that blur across latitude and smear a polar cap into the rows beside it.
    #[allow(clippy::cast_precision_loss)] // texel counts in f32
    fn texel_solid_angle(&self) -> f32 {
        let base = self.levels[0].size;
        2.0 * PI * PI / (base.x as f32 * base.y as f32)
    }

    /// The radiance towards `direction`, trilinearly filtered at `lod` (0 is the full-resolution map).
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // lod is 0..=15
    fn sample(&self, direction: Vec3, lod: f32) -> Vec3 {
        let u = direction.z.atan2(direction.x) / TAU + 0.5;
        let v = 0.5 - direction.y.clamp(-1.0, 1.0).asin() / PI;
        let lod = lod.clamp(0.0, (self.levels.len() - 1) as f32);
        let (low, t) = (lod.floor() as usize, lod.fract());
        let a = self.levels[low].bilinear(u, v);
        if t == 0.0 { a } else { a.lerp(self.levels[(low + 1).min(self.levels.len() - 1)].bilinear(u, v), t) }
    }
}

/// The direction through texel position `(u, v)` (each 0..1) of cube face `face`, in the order and orientation
/// every graphics API shares: `+X, -X, +Y, -Y, +Z, -Z`, with `v` increasing downwards on the side faces.
pub(super) fn face_direction(face: usize, u: f32, v: f32) -> Vec3 {
    let (sc, tc) = (2.0 * u - 1.0, 2.0 * v - 1.0);
    match face {
        0 => Vec3::new(1.0, -tc, -sc),
        1 => Vec3::new(-1.0, -tc, sc),
        2 => Vec3::new(sc, 1.0, tc),
        3 => Vec3::new(sc, -1.0, -tc),
        4 => Vec3::new(sc, -tc, 1.0),
        _ => Vec3::new(-sc, -tc, -1.0),
    }
    .normalize()
}

/// The `k`-th of `n` points of the Hammersley set: a low-discrepancy sequence on the unit square.
#[allow(clippy::cast_precision_loss)] // k and n are at most 64
fn hammersley(k: u32, n: u32) -> (f32, f32) {
    (k as f32 / n as f32, k.reverse_bits() as f32 * 2.328_306_4e-10)
}

/// A half vector distributed as the GGX lobe of roughness `alpha` about `normal`, from the point `(x, y)` of the unit
/// square, and the lobe's density `D(n·h)` there.
fn ggx_half_vector(normal: Vec3, alpha: f32, (x, y): (f32, f32)) -> (Vec3, f32) {
    let a2 = alpha * alpha;
    let cos_theta = ((1.0 - y) / (1.0 + (a2 - 1.0) * y)).sqrt();
    let sin_theta = (1.0 - cos_theta * cos_theta).max(0.0).sqrt();
    let phi = TAU * x;
    let helper = if normal.z.abs() < 0.999 { Vec3::Z } else { Vec3::X };
    let tangent = helper.cross(normal).normalize();
    let bitangent = normal.cross(tangent);
    let half = tangent * (phi.cos() * sin_theta) + bitangent * (phi.sin() * sin_theta) + normal * cos_theta;
    let denominator = cos_theta * cos_theta * (a2 - 1.0) + 1.0;
    (half, a2 / (PI * denominator * denominator))
}

/// Builds the specular chain: level 0 is the sky itself, each later level the sky blurred by a wider GGX lobe.
#[allow(clippy::cast_precision_loss)] // sizes and level indices are tiny
fn prefilter(pyramid: &Pyramid<'_>) -> Specular {
    let levels = (0..SPECULAR_LEVELS)
        .map(|level| {
            let size = (SPECULAR_FACE >> level).max(1);
            let roughness = level as f32 / (SPECULAR_LEVELS - 1) as f32;
            let alpha = roughness * roughness;
            // A cube texel's solid angle, on average: the footprint level 0 is filtered over.
            let footprint = 4.0 * PI / (6.0 * (size * size) as f32);
            let mut texels = Vec::with_capacity(6 * (size * size) as usize * 4);
            for face in 0..6 {
                for j in 0..size {
                    for i in 0..size {
                        let (u, v) = ((i as f32 + 0.5) / size as f32, (j as f32 + 0.5) / size as f32);
                        let normal = face_direction(face, u, v);
                        let colour = if level == 0 {
                            pyramid.sample(normal, 0.5 * (footprint / pyramid.texel_solid_angle()).log2())
                        } else {
                            filtered(pyramid, normal, alpha)
                        };
                        texels.extend([colour.x, colour.y, colour.z, 1.0]);
                    }
                }
            }
            texels
        })
        .collect();
    Specular { levels }
}

/// The sky seen through a GGX lobe of roughness `alpha` about `normal` (the view and the normal taken as one, the
/// split-sum approximation), by importance sampling.
#[allow(clippy::cast_precision_loss)] // sample counts are 64
fn filtered(pyramid: &Pyramid<'_>, normal: Vec3, alpha: f32) -> Vec3 {
    let (mut sum, mut weight) = (Vec3::ZERO, 0.0);
    for k in 0..GGX_SAMPLES {
        let (half, density) = ggx_half_vector(normal, alpha, hammersley(k, GGX_SAMPLES));
        let light = 2.0 * normal.dot(half) * half - normal;
        let cosine = normal.dot(light);
        if cosine <= 0.0 {
            continue;
        }
        // The sample's pdf over directions is D / 4 when the view is the normal. Reading the pyramid at the level
        // whose texels cover the sample's solid angle filters away the noise of few samples.
        let sample_angle = 4.0 / (GGX_SAMPLES as f32 * density + 1e-4);
        let lod = (0.5 * (sample_angle / pyramid.texel_solid_angle()).log2() + 0.5).max(0.0);
        sum += pyramid.sample(light, lod) * cosine;
        weight += cosine;
    }
    if weight > 0.0 { sum / weight } else { pyramid.sample(normal, 0.0) }
}

#[cfg(test)]
#[allow(clippy::cast_precision_loss, clippy::chunks_exact_to_as_chunks)] // test arithmetic on tiny grids
mod tests {
    use super::*;

    fn hdri(size: UVec2, pixel: impl Fn(u32, u32) -> [f32; 3]) -> EnvironmentDesc {
        let rgb = (0..size.y).flat_map(|y| (0..size.x).map(move |x| (x, y))).flat_map(|(x, y)| pixel(x, y)).collect();
        EnvironmentDesc::Hdri { size, rgb }
    }

    /// A grey sky with a bright 3 x 3 patch 45 degrees up towards +Z (columns 95..98 of 128 are `u` = 0.75).
    fn sun_sky() -> EnvironmentDesc {
        hdri(
            UVec2::new(128, 64),
            |x, y| if (95..98).contains(&x) && (15..18).contains(&y) { [50.0; 3] } else { [0.2; 3] },
        )
    }

    /// The specular texel `(i, j)` of `face` at `level`.
    fn texel(specular: &Specular, level: usize, face: usize, i: u32, j: u32) -> Vec3 {
        let size = (SPECULAR_FACE >> level).max(1) as usize;
        let at = ((face * size + j as usize) * size + i as usize) * 4;
        let t = &specular.levels[level][at..at + 3];
        Vec3::new(t[0], t[1], t[2])
    }

    #[test]
    fn a_uniform_sky_is_uniform_at_every_level_and_its_irradiance_is_pi_times_its_colour() {
        let colour = Vec3::new(0.5, 1.0, 2.0);
        for size in [UVec2::new(1, 1), UVec2::new(8, 4), UVec2::new(33, 17)] {
            let baked = bake(&hdri(size, |_, _| colour.to_array())).unwrap();
            let want = colour * PI;
            for e in [baked.diffuse.dark, baked.diffuse.bright] {
                assert!((e - want).abs().max_element() < 0.01 * want.max_element(), "{size}: {e}");
            }
            for (level, texels) in baked.specular.levels.iter().enumerate() {
                for t in texels.chunks_exact(4) {
                    let got = Vec3::new(t[0], t[1], t[2]);
                    assert!((got - colour).abs().max_element() < 1e-3, "{size}, level {level}: {got}");
                }
            }
        }
    }

    #[test]
    fn an_overhead_sky_lights_by_the_cosine_integral() {
        // L = max(y, 0): facing up the irradiance is the integral of y^2 over the upper hemisphere, 2 pi / 3.
        let sky = EnvironmentDesc::Sky { zenith: Colour::WHITE, horizon: Colour::BLACK, ground: Colour::BLACK };
        let harmonics = Harmonics::project(&Source::new(&sky));
        let up = harmonics.irradiance(Vec3::Y).x;
        assert!((up - 2.0 * PI / 3.0).abs() < 0.04 * up, "{up}");
        assert!(harmonics.irradiance(-Vec3::Y).x < 0.1);
        assert!((harmonics.brightest() - Vec3::Y).length() < 0.01, "{}", harmonics.brightest());
    }

    #[test]
    fn a_sun_sets_the_toon_split_direction_and_the_two_sides_differ() {
        let baked = bake(&sun_sky()).unwrap();
        let sun = Vec3::new(0.0, 1.0, 1.0).normalize();
        assert!(baked.diffuse.toward.dot(sun) > 0.95, "{}", baked.diffuse.toward);
        assert!(
            baked.diffuse.bright.x > 1.8 * baked.diffuse.dark.x,
            "{:?}",
            (baked.diffuse.bright, baked.diffuse.dark)
        );
    }

    #[test]
    fn the_specular_chain_blurs_the_sun_and_keeps_its_energy() {
        let baked = bake(&sun_sky()).unwrap();
        // Peak brightness of each level falls as the lobe widens.
        let peaks: Vec<f32> = baked
            .specular
            .levels
            .iter()
            .map(|texels| texels.chunks_exact(4).map(|t| t[0]).fold(0.0, f32::max))
            .collect();
        assert!(peaks.windows(2).all(|w| w[1] < w[0]), "{peaks:?}");
        assert!(peaks[0] > 5.0, "the sun is visible at roughness 0: {peaks:?}");
        // Each level's mean over the sphere stays the sky's mean (a cube texel at (u, v) covers solid angle ~ (1+u^2+v^2)^-1.5).
        // The sky's mean radiance over the sphere weights each row by its solid angle, cos(latitude).
        let row_cos = |y: u32| ((0.5 - (y as f32 + 0.5) / 64.0) * PI).cos();
        let (mut weighted, mut weights) = (0.0, 0.0);
        for y in 0..64 {
            for x in 0..128 {
                let value = if (95..98).contains(&x) && (15..18).contains(&y) { 50.0 } else { 0.2 };
                weighted += value * row_cos(y);
                weights += row_cos(y);
            }
        }
        let sky_mean = weighted / weights;
        for level in 0..SPECULAR_LEVELS as usize {
            let size = (SPECULAR_FACE >> level).max(1);
            let (mut sum, mut total) = (0.0, 0.0);
            for face in 0..6 {
                for j in 0..size {
                    for i in 0..size {
                        let (u, v) =
                            (2.0 * (i as f32 + 0.5) / size as f32 - 1.0, 2.0 * (j as f32 + 0.5) / size as f32 - 1.0);
                        let w = (1.0 + u * u + v * v).powf(-1.5);
                        sum += texel(&baked.specular, level, face, i, j).x * w;
                        total += w;
                    }
                }
            }
            let mean = sum / total;
            assert!((mean - sky_mean).abs() < 0.25 * sky_mean, "level {level}: mean {mean}, sky {sky_mean}");
        }
    }

    #[test]
    fn cube_faces_point_down_their_axes_and_agree_where_they_meet() {
        let axes = [Vec3::X, -Vec3::X, Vec3::Y, -Vec3::Y, Vec3::Z, -Vec3::Z];
        for (face, axis) in axes.into_iter().enumerate() {
            assert!((face_direction(face, 0.5, 0.5) - axis).length() < 1e-6, "face {face}");
        }
        // The right edge of +Z meets the left edge of +X; the top edge of +X meets +Y at the shared corner.
        assert!((face_direction(4, 1.0, 0.5) - face_direction(0, 0.0, 0.5)).length() < 1e-6);
        assert!((face_direction(0, 0.5, 0.0) - face_direction(2, 1.0, 0.5)).length() < 1e-6);
        assert!((face_direction(5, 0.0, 0.5) - face_direction(0, 1.0, 0.5)).length() < 1e-6);
    }

    #[test]
    fn the_pyramid_reads_the_contracts_layout() {
        // The sun sits at +Z, 45 degrees up: u = 0.75, v = 0.25.
        let desc = sun_sky();
        let pyramid = Pyramid::new(&Source::new(&desc));
        assert!(pyramid.sample(Vec3::new(0.0, 1.0, 1.0).normalize(), 0.0).x > 40.0);
        assert!(pyramid.sample(Vec3::new(0.0, 1.0, -1.0).normalize(), 0.0).x < 1.0, "-Z is dark");
        assert!(pyramid.sample(Vec3::new(1.0, 1.0, 0.0).normalize(), 0.0).x < 1.0, "+X is dark");
        assert_eq!(pyramid.levels.last().map(|l| l.size), Some(UVec2::ONE), "the chain ends at one texel");
    }

    #[test]
    fn a_bright_pole_row_counts_for_its_solid_angle_and_no_more() {
        // Only the top row of a 128 x 64 map is white: a polar cap of 2.8125 degrees. Looking up through the widest lobe
        // (roughness 1, which weights the hemisphere by the cosine alone) sees that cap's cosine integral over pi,
        // 2 pi (1 - cos(cap)) / pi = 0.00241. Averaging the pole's texels equally with the equator's gave 0.030; weighted, it is 0.00236 (-2%), so the tolerance is 10%.
        let desc = hdri(UVec2::new(128, 64), |_, y| if y == 0 { [1.0; 3] } else { [0.0; 3] });
        let pyramid = Pyramid::new(&Source::new(&desc));
        let want = (1.0 - (180.0_f32 / 64.0).to_radians().cos()) * 2.0 * PI / PI;
        let got = filtered(&pyramid, Vec3::Y, 1.0).x;
        println!("polar regression: got {got}, want {want}");
        assert!((got - want).abs() <= 0.10 * want, "filtered(+Y, roughness 1) = {got}, the cosine integral is {want}");
    }

    type Pattern = Box<dyn Fn(u32, u32) -> f32>;

    /// The mean radiance over the sphere of a level: each row weighted by the exact solid angle of its band.
    fn spherical_mean(level: &Level<'_>) -> f64 {
        let (w, h) = (level.size.x as usize, level.size.y as usize);
        let (mut sum, mut total) = (0.0, 0.0);
        for y in 0..h {
            let weight = band(y as f64 / h as f64, (y + 1) as f64 / h as f64);
            for x in 0..w {
                sum += f64::from(level.texel(x, y).x) * weight;
                total += weight;
            }
        }
        sum / total
    }

    fn level(size: UVec2, pixel: impl Fn(u32, u32) -> f32) -> Level<'static> {
        let rgb =
            (0..size.y).flat_map(|y| (0..size.x).map(move |x| (x, y))).flat_map(|(x, y)| [pixel(x, y); 3]).collect();
        Level { size, rgb: Cow::Owned(rgb) }
    }

    #[test]
    fn every_mip_keeps_the_spherical_mean_for_any_size_and_any_pole() {
        // A 128 x 65 map white only in its bottom row has a spherical mean of 0.000584; treating the odd leftover row as
        // part of a uniform grid made the first mip 0.5. Overlap resampling keeps the mean exact at every level.
        let sizes = [(128, 65), (128, 64), (127, 64), (33, 17), (65, 129), (7, 5), (1, 9), (9, 1), (3, 3), (256, 128)];
        for (w, h) in sizes {
            let size = UVec2::new(w, h);
            let patterns: [(&str, Pattern); 5] = [
                ("bottom row", Box::new(move |_, y| f32::from(y == h - 1))),
                ("top row", Box::new(|_, y| f32::from(y == 0))),
                ("last column", Box::new(move |x, _| f32::from(x == w - 1))),
                ("sun patch", Box::new(|x, y| if x < 2 && y < 3 { 40.0 } else { 0.1 })),
                ("gradient", Box::new(move |x, y| (x * 7 + y * 13 % 11) as f32 * 0.01)),
            ];
            for (name, pixel) in patterns {
                let mut current = level(size, pixel);
                let base = spherical_mean(&current);
                while current.size.x > 1 || current.size.y > 1 {
                    current = current.halved();
                    let mean = spherical_mean(&current);
                    assert!(
                        (mean - base).abs() <= 1e-4 * base,
                        "{name} {size} -> {}: mean {mean} against {base}",
                        current.size
                    );
                }
            }
        }
    }

    #[test]
    fn halving_a_power_of_two_map_is_what_the_pairwise_weighted_mean_gave() {
        // For even sizes every destination texel covers exactly four source texels, so the result must be the mean of
        // them weighted by their solid angle (the exact band; the earlier version used the cosine at the row centre).
        let size = UVec2::new(16, 8);
        let source = level(size, |x, y| ((x * 31 + y * 17) % 23) as f32);
        let halved = source.halved();
        assert_eq!(halved.size, UVec2::new(8, 4));
        for y in 0..4_usize {
            for x in 0..8_usize {
                let (mut sum, mut total) = (0.0, 0.0);
                for row in [2 * y, 2 * y + 1] {
                    let weight = band(row as f64 / 8.0, (row + 1) as f64 / 8.0);
                    for column in [2 * x, 2 * x + 1] {
                        sum += f64::from(source.texel(column, row).x) * weight;
                        total += weight;
                    }
                }
                let got = f64::from(halved.texel(x, y).x);
                assert!(
                    (got - sum / total).abs() <= 1e-5 * (sum / total).max(1.0),
                    "({x}, {y}): {got} against {}",
                    sum / total
                );
            }
        }
    }

    #[test]
    fn a_broad_polar_cap_matches_the_cosine_integral_for_both_light_terms() {
        // The top 8 rows of a 128 x 64 map are white: a cap of 22.5 degrees. Facing it, the cosine integral is
        // pi sin^2(22.5 deg) = 0.4601 irradiance, and 0.1464 once divided by pi (the roughness-1 lobe weights the
        // hemisphere by the cosine alone). Unlike the single-row cap this is wide enough to be sampled with few
        // samples, so the tolerance is tight. Measured with 64 samples: specular +1.8% (limit 5%), irradiance +4.9% (limit 10%; the nine-term fit of a hard cap edge, not the sampling).
        let desc = hdri(UVec2::new(128, 64), |_, y| if y < 8 { [1.0; 3] } else { [0.0; 3] });
        let cap = (22.5_f32).to_radians();
        let irradiance = PI * cap.sin().powi(2);
        let pyramid = Pyramid::new(&Source::new(&desc));
        let specular = filtered(&pyramid, Vec3::Y, 1.0).x;
        let diffuse = Harmonics::project(&Source::new(&desc)).irradiance(Vec3::Y).x;
        let (specular_error, diffuse_error) = (specular / (irradiance / PI) - 1.0, diffuse / irradiance - 1.0);
        println!("broad cap: specular {specular} ({specular_error:+.3}), irradiance {diffuse} ({diffuse_error:+.3})");
        assert!(
            specular_error.abs() <= 0.10,
            "filtered(+Y, roughness 1) is {specular}, the cosine integral {}",
            irradiance / PI
        );
        assert!(diffuse_error.abs() <= 0.10, "irradiance is {diffuse}, the cosine integral {irradiance}");
    }
}
