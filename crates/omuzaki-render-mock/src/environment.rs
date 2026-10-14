//! Image-based light for the CPU renderer: a radiance map reduced to nine spherical-harmonic coefficients, which give
//! the diffuse irradiance for any normal in a few multiplications (Ramamoorthi and Hanrahan, "An Efficient
//! Representation for Irradiance Environment Maps"). The mock shades flat per triangle and has no specular term, so it
//! has no use for a prefiltered specular chain; the GPU renderer builds one from the same radiance map.

use std::f64::consts::PI;

use omuzaki_core::glam::{Mat3, Vec3};
use omuzaki_render_api::{EnvironmentDesc, EnvironmentUse, RenderError, sky_radiance};

/// Pixels of the equirectangular grid a procedural sky is sampled on. Fine enough that the nine coefficients are the
/// sky's to within the quadrature error, small enough to project in microseconds.
const SKY_GRID: (u32, u32) = (64, 32);

/// What the mock keeps of an environment.
pub(crate) struct EnvironmentData {
    /// Radiance projected on the nine real spherical harmonics, linear RGB, in `Y00, Y1-1, Y10, Y11, Y2-2 ...` order.
    sh: [[f64; 3]; 9],
    /// Bytes the radiance map would occupy on a GPU (RGBA16F with a mip chain), for `ResourceStats`.
    pub(crate) gpu_bytes: u64,
}

/// The values of the nine real spherical harmonics towards the unit vector `n`.
fn basis(n: [f64; 3]) -> [f64; 9] {
    let [x, y, z] = n;
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

/// Cosine-lobe convolution factors per band (π, 2π/3, π/4), turning radiance coefficients into irradiance.
const BAND: [f64; 9] = {
    const A: [f64; 3] = [PI, 2.0 * PI / 3.0, PI / 4.0];
    [A[0], A[1], A[1], A[1], A[2], A[2], A[2], A[2], A[2]]
};

fn luminance(c: [f64; 3]) -> f64 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

impl EnvironmentData {
    /// Reduces `desc` to coefficients.
    ///
    /// # Errors
    /// `Invalid` per [`EnvironmentDesc::validate`], `LimitExceeded` for an HDRI side above `max_size`.
    pub(crate) fn new(desc: &EnvironmentDesc, max_size: u32) -> Result<Self, RenderError> {
        desc.validate()?;
        let (width, height) = match desc {
            EnvironmentDesc::Sky { .. } => SKY_GRID,
            EnvironmentDesc::Hdri { size, .. } => {
                if size.x > max_size || size.y > max_size {
                    return Err(RenderError::LimitExceeded { what: "environment size", limit: u64::from(max_size) });
                }
                (size.x, size.y)
            }
        };
        let gpu_bytes = u64::from(width) * u64::from(height) * 8 * 4 / 3;
        #[allow(clippy::cast_possible_truncation)] // the sky is defined in f32
        let radiance = |x: u32, y: u32, up: f64| -> [f64; 3] {
            match desc {
                EnvironmentDesc::Sky { zenith, horizon, ground } => {
                    let c = sky_radiance(*zenith, *horizon, *ground, up as f32);
                    [f64::from(c.x), f64::from(c.y), f64::from(c.z)]
                }
                EnvironmentDesc::Hdri { size, rgb } => {
                    let at = (y as usize * size.x as usize + x as usize) * 3;
                    [f64::from(rgb[at]), f64::from(rgb[at + 1]), f64::from(rgb[at + 2])]
                }
            }
        };
        Ok(Self { sh: project(width, height, radiance), gpu_bytes })
    }

    /// The diffuse irradiance (`∫ L max(n·ω, 0) dω`) for a unit normal in the environment's own frame.
    fn irradiance(&self, n: Vec3) -> Vec3 {
        let y = basis([f64::from(n.x), f64::from(n.y), f64::from(n.z)]);
        let mut out = [0.0; 3];
        for (k, coefficient) in self.sh.iter().enumerate() {
            for (o, c) in out.iter_mut().zip(coefficient) {
                *o += c * y[k] * BAND[k];
            }
        }
        // The nine-term fit can ring slightly below zero beside a very bright source.
        #[allow(clippy::cast_possible_truncation)] // back to the renderer's f32
        Vec3::new(out[0] as f32, out[1] as f32, out[2] as f32).max(Vec3::ZERO)
    }

    /// The luminance-weighted mean direction of the sky, in the environment's frame (the l=1 band points at it).
    fn brightest(&self) -> Vec3 {
        #[allow(clippy::cast_possible_truncation)] // back to the renderer's f32
        let [y, z, x] = [1, 2, 3].map(|k| luminance(self.sh[k]) as f32);
        Vec3::new(x, y, z).try_normalize().unwrap_or(Vec3::Y)
    }
}

/// Sub-samples per axis the projection aims for over the whole sphere: a texel is split until the grid is at least this
/// fine, so a coarse map (down to a single texel) is integrated over its footprint instead of read at its centre.
const MIN_SAMPLES: (u32, u32) = (128, 64);

/// Projects an equirectangular radiance map onto the nine harmonics. `radiance(x, y, up)` is the colour of texel
/// `(x, y)` where the sample's direction has height `up` (the sine of its elevation; a texel's colour is constant, a
/// procedural sky varies with `up`).
///
/// Every texel is integrated over its footprint on the sphere: split into a fixed grid of sub-samples, each weighted by
/// its solid angle (the cosine of its elevation). The weights are rescaled to sum to exactly 4π, so a uniform map
/// projects to exactly its colour however coarse it is. Reading a texel only at its centre would instead put a coarse
/// map's whole weight on one direction and invent directional light.
fn project(width: u32, height: u32, radiance: impl Fn(u32, u32, f64) -> [f64; 3]) -> [[f64; 3]; 9] {
    let (sub_x, sub_y) = (MIN_SAMPLES.0.div_ceil(width), MIN_SAMPLES.1.div_ceil(height));
    let (w, h) = (f64::from(width * sub_x), f64::from(height * sub_y));
    let mut sh = [[0.0; 3]; 9];
    let mut total = 0.0;
    for sy in 0..height * sub_y {
        let elevation = (0.5 - (f64::from(sy) + 0.5) / h) * PI;
        let (up, ring) = (elevation.sin(), elevation.cos());
        for sx in 0..width * sub_x {
            // u = atan2(z, x) / 2π + 0.5 (the three.js layout in `omuzaki_render_api`'s module docs).
            let phi = ((f64::from(sx) + 0.5) / w - 0.5) * 2.0 * PI;
            let weights = basis([ring * phi.cos(), up, ring * phi.sin()]);
            let colour = radiance(sx / sub_x, sy / sub_y, up);
            for (coefficient, weight) in sh.iter_mut().zip(weights) {
                for (c, l) in coefficient.iter_mut().zip(colour) {
                    *c += l * weight * ring;
                }
            }
            total += ring;
        }
    }
    let scale = 4.0 * PI / total;
    for c in sh.iter_mut().flatten() {
        *c *= scale;
    }
    sh
}

/// An environment as one frame uses it: the coefficients, the strength and the turn from world to sky.
pub(crate) struct EnvLight<'a> {
    data: &'a EnvironmentData,
    intensity: f32,
    to_sky: Mat3,
    toon_ramp: bool,
}

impl<'a> EnvLight<'a> {
    pub(crate) fn new(data: &'a EnvironmentData, using: &EnvironmentUse) -> Self {
        // Turning the sky by +θ about Y is turning every lookup direction by -θ.
        let to_sky = Mat3::from_rotation_y(-using.rotation_deg.to_radians());
        Self { data, intensity: using.intensity, to_sky, toon_ramp: using.toon_ramp }
    }

    /// The irradiance a surface with world-space normal `n` receives. With the toon ramp on, `ramp` maps the dot of the
    /// normal and the brightest direction to the ramp's 0..1 blend between the dark and bright side.
    pub(crate) fn irradiance(&self, n: Vec3, ramp: impl Fn(f32) -> f32) -> Vec3 {
        let n = self.to_sky * n;
        let light = if self.toon_ramp {
            let toward = self.data.brightest();
            self.data.irradiance(-toward).lerp(self.data.irradiance(toward), ramp(n.dot(toward)))
        } else {
            self.data.irradiance(n)
        };
        light * self.intensity
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_core::glam::UVec2;
    use omuzaki_render_api::Colour;

    use super::*;

    fn sky(zenith: Colour, horizon: Colour, ground: Colour) -> EnvironmentData {
        EnvironmentData::new(&EnvironmentDesc::Sky { zenith, horizon, ground }, 4096).unwrap()
    }

    #[test]
    fn a_uniform_sky_gives_pi_times_its_colour_in_every_direction() {
        let c = Colour::hex(0x00c0_8040);
        let env = sky(c, c, c);
        for n in [Vec3::Y, -Vec3::Y, Vec3::X, Vec3::new(0.6, 0.0, -0.8)] {
            let e = env.irradiance(n);
            assert!((e - c.to_linear() * std::f32::consts::PI).abs().max_element() < 2e-3, "{n:?} {e:?}");
        }
    }

    #[test]
    fn a_zenith_only_sky_lights_up_and_sideways_but_not_down() {
        // L = max(y, 0): the irradiance straight up is ∫ y² over the upper hemisphere = 2π/3.
        let env = sky(Colour::WHITE, Colour::BLACK, Colour::BLACK);
        let up = env.irradiance(Vec3::Y).x;
        assert!((up - 2.0 * std::f32::consts::PI / 3.0).abs() < 0.08, "up {up}");
        assert!(env.irradiance(Vec3::X).x > 0.2 && env.irradiance(Vec3::X).x < up);
        assert!(env.irradiance(-Vec3::Y).x < 0.1);
    }

    #[test]
    fn the_brightest_direction_follows_the_light() {
        assert!((sky(Colour::WHITE, Colour::BLACK, Colour::BLACK).brightest() - Vec3::Y).length() < 1e-3);
        assert!((sky(Colour::BLACK, Colour::BLACK, Colour::WHITE).brightest() + Vec3::Y).length() < 1e-3);
    }

    /// A constant white HDRI of `size`: every direction should get irradiance π.
    fn white(size: UVec2) -> EnvironmentData {
        let rgb = vec![1.0; size.x as usize * size.y as usize * 3];
        EnvironmentData::new(&EnvironmentDesc::Hdri { size, rgb }, 4096).unwrap()
    }

    #[test]
    fn a_constant_hdri_lights_every_direction_equally_at_any_size() {
        let directions =
            [Vec3::X, -Vec3::X, Vec3::Y, -Vec3::Y, Vec3::Z, -Vec3::Z, Vec3::new(1.0, 2.0, -3.0).normalize()];
        for size in [UVec2::new(1, 1), UVec2::new(4, 2), UVec2::new(3, 5)] {
            let env = white(size);
            let id = omuzaki_render_api::EnvironmentId::from_parts(0, 1);
            for (rotation_deg, toon_ramp) in [(0.0, false), (37.0, false), (0.0, true), (123.0, true)] {
                let using = EnvironmentUse { id, intensity: 1.0, rotation_deg, toon_ramp };
                let light = EnvLight::new(&env, &using);
                for n in directions {
                    let e = light.irradiance(n, |along| along.clamp(0.0, 1.0));
                    assert!(
                        (e - Vec3::splat(std::f32::consts::PI)).abs().max_element() < 1e-3,
                        "{size} rotation {rotation_deg} toon {toon_ramp} facing {n:?}: {e:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn an_hdri_side_above_the_limit_is_refused() {
        let hdri = EnvironmentDesc::Hdri { size: omuzaki_core::glam::UVec2::new(8, 4), rgb: vec![1.0; 96] };
        assert!(matches!(EnvironmentData::new(&hdri, 4), Err(RenderError::LimitExceeded { .. })));
    }
}
