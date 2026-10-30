//! Toon volumes: cel-shaded smoke, goo, cloud, steam, fire, dust and splash that billow, grow and fade like the
//! hand-drawn effects of an anime or a cartoon, as surfaces with hard two-tone shading and an ink line, solid or
//! see-through.
//!
//! A volume is a field of puffs. An emitter (a point in the world or on an actor's bone) releases puffs at a rate; each
//! rises and spreads, swells, wobbles in a turbulent drift, then thins away. The puffs melt into one surface (a smooth
//! union of spheres, its edge broken up by noise so it reads as billows and wisps rather than balls), which is meshed
//! every frame with surface nets and drawn with the toon shader and an outline. Everything is a pure function of time
//! and the emitter's path, so any frame renders on its own and two renders match.
//!
//! The parameters are the ones an effects animator reaches for: rate, life, size, rise, spread, turbulence, gravity,
//! density over time, colours, opacity, line; the presets set them for each look.

use omuzaki_core::colour::Colour;
use omuzaki_core::glam::Vec3;
use omuzaki_core::mesh::MeshData;
use omuzaki_render_api::{AlphaMode, Material, Outline, Rim, Shading, Toon};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::curve::Track;
use crate::def::Actor;

/// The most puffs alive in one volume.
pub const MAX_PUFFS: usize = 160;
/// The finest grid a volume is meshed on, cells along its longest side.
pub const MAX_RESOLUTION: u32 = 128;

/// A look for a volume: the numbers each parameter takes when the volume leaves it out.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VolumePreset {
    /// Grey smoke rising and spreading: a fire's, a chimney's, a burning vampire's. The default.
    #[default]
    Smoke,
    /// Thick, slow, toxic green goo that boils up and clings.
    Goo,
    /// A white, slow cumulus that hardly moves.
    Cloud,
    /// Fast, small, white steam that vanishes quickly.
    Steam,
    /// Orange flame licks that glow on their own.
    Fire,
    /// Low tan dust that spreads along the ground.
    Dust,
    /// Liquid thrown up and out that falls back in sheets and drops, see-through: a slime burst, spray, paint.
    Splash,
}

impl VolumePreset {
    /// Every preset, for listings.
    pub const ALL: [Self; 7] = [Self::Smoke, Self::Goo, Self::Cloud, Self::Steam, Self::Fire, Self::Dust, Self::Splash];

    /// The preset's name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Smoke => "smoke",
            Self::Goo => "goo",
            Self::Cloud => "cloud",
            Self::Steam => "steam",
            Self::Fire => "fire",
            Self::Dust => "dust",
            Self::Splash => "splash",
        }
    }

    /// The preset's numbers.
    pub fn tuning(self) -> Tuning {
        let t = |colour: u32, shade: [f32; 3], line: u32, rate, life, size, rise, spread, turbulence| Tuning {
            colour: Colour::hex(colour),
            shade,
            line: Colour::hex(line),
            glow: 0.0,
            rate,
            life,
            size,
            rise,
            spread,
            turbulence,
            gravity: 0.0,
            opacity: 1.0,
        };
        match self {
            Self::Smoke => t(0x00c8_c4cc, [0.62, 0.6, 0.7], 0x004a_4654, 9.0, 2.6, 0.2, 0.45, 0.18, 0.55),
            Self::Goo => t(0x0096_e070, [0.5, 0.78, 0.42], 0x0034_6a2a, 11.0, 3.2, 0.24, 0.32, 0.26, 0.7),
            Self::Cloud => t(0x00fa_fbff, [0.78, 0.82, 0.95], 0x007a_86a8, 2.0, 12.0, 0.6, 0.03, 0.05, 0.35),
            Self::Steam => t(0x00f2_f6fa, [0.75, 0.8, 0.88], 0x0090_9aa8, 14.0, 1.1, 0.09, 0.9, 0.12, 0.5),
            Self::Fire => {
                Tuning { glow: 1.2, ..t(0x00ff_a030, [0.95, 0.45, 0.2], 0x00a0_3010, 16.0, 0.9, 0.13, 1.1, 0.1, 0.8) }
            }
            Self::Dust => t(0x00d8_c4a0, [0.72, 0.64, 0.55], 0x006e_5c44, 10.0, 2.0, 0.16, 0.08, 0.6, 0.45),
            Self::Splash => Tuning {
                gravity: 4.5,
                opacity: 0.8,
                ..t(0x009f_dcff, [0.62, 0.8, 0.92], 0x0030_6a8a, 40.0, 1.3, 0.09, 2.4, 1.3, 0.5)
            },
        }
    }
}

/// The numbers a preset gives a volume.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Tuning {
    /// Lit colour.
    pub colour: Colour,
    /// Shadow side as a multiplier of the lit colour.
    pub shade: [f32; 3],
    /// The ink line.
    pub line: Colour,
    /// How much it lights itself.
    pub glow: f32,
    /// Puffs a second.
    pub rate: f32,
    /// Seconds a puff lives.
    pub life: f32,
    /// A full-grown puff's radius, metres.
    pub size: f32,
    /// Metres a second up.
    pub rise: f32,
    /// Metres a second outward.
    pub spread: f32,
    /// 0 to 1: how much the drift and the edge's noise break the shapes up.
    pub turbulence: f32,
    /// Metres a second squared down: thrown liquid arcs back to the ground.
    pub gravity: f32,
    /// 0 (unseen) to 1 (solid): how much shows through.
    pub opacity: f32,
}

/// Where a volume's puffs come from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Emitter {
    /// The point, metres: in the world, or in the bone's frame when `actor` and `bone` are given.
    #[serde(default)]
    pub at: [f32; 3],
    /// An actor of the shot whose bone carries the emitter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    /// That actor's humanoid bone (`chest`, `head`, `hips`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bone: Option<String>,
    /// Puffs are born anywhere inside this radius, metres.
    #[serde(default = "emitter_radius")]
    pub radius: f32,
}

fn emitter_radius() -> f32 {
    0.15
}

impl Default for Emitter {
    fn default() -> Self {
        Self { at: [0.0; 3], actor: None, bone: None, radius: emitter_radius() }
    }
}

/// A toon volume in a shot. Every number left out takes its preset's.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VolumeDef {
    /// Its name in the shot.
    pub id: String,
    /// The look.
    #[serde(default)]
    pub preset: VolumePreset,
    /// Where the puffs come from.
    #[serde(default)]
    pub emitter: Emitter,
    /// When it starts releasing puffs, seconds from the shot's start.
    #[serde(default)]
    pub start: f32,
    /// When it stops releasing them; the last puffs still live out their life.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<f32>,
    /// Lit colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colour: Option<Colour>,
    /// Shadow side, as a multiplier of the lit colour per channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shade: Option<[f32; 3]>,
    /// The ink line's colour; `line_width` 0 draws none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<Colour>,
    /// The ink line's width, metres at 1 m from the camera.
    #[serde(default = "line_width")]
    pub line_width: f32,
    /// Self-light, 0 to 4 (fire glows).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glow: Option<f32>,
    /// Puffs a second.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate: Option<f32>,
    /// Seconds a puff lives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub life: Option<f32>,
    /// Full-grown puff radius, metres.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<f32>,
    /// Rise, metres a second.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rise: Option<f32>,
    /// Outward spread, metres a second.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spread: Option<f32>,
    /// Turbulence, 0 to 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turbulence: Option<f32>,
    /// Pull down, metres a second squared, 0 to 20 (a splash falls back).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gravity: Option<f32>,
    /// How solid, 0.05 to 1: below 1 what is behind shows through (slime, spray, a thin smoke).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f32>,
    /// How thick it is over time, 0 to 2: scales every puff (key it up to boil over, down to clear).
    #[serde(default = "density")]
    pub density: Track<f32>,
    /// Grid cells along the volume's longest side, 16 to 128: finer shows smaller wisps and costs more.
    #[serde(default = "resolution")]
    pub resolution: u32,
    /// Picks the puffs.
    #[serde(default)]
    pub seed: u32,
}

fn line_width() -> f32 {
    0.004
}

fn density() -> Track<f32> {
    Track::Hold(1.0)
}

fn resolution() -> u32 {
    64
}

impl VolumeDef {
    /// The volume's numbers: its own where given, its preset's otherwise.
    pub fn tuning(&self) -> Tuning {
        let p = self.preset.tuning();
        Tuning {
            colour: self.colour.unwrap_or(p.colour),
            shade: self.shade.unwrap_or(p.shade),
            line: self.line.unwrap_or(p.line),
            glow: self.glow.unwrap_or(p.glow),
            rate: self.rate.unwrap_or(p.rate),
            life: self.life.unwrap_or(p.life),
            size: self.size.unwrap_or(p.size),
            rise: self.rise.unwrap_or(p.rise),
            spread: self.spread.unwrap_or(p.spread),
            turbulence: self.turbulence.unwrap_or(p.turbulence),
            gravity: self.gravity.unwrap_or(p.gravity),
            opacity: self.opacity.unwrap_or(p.opacity),
        }
    }

    /// The material it draws with: hard two-tone toon, its line, its glow.
    pub fn material(&self) -> Material {
        let t = self.tuning();
        let lit = t.colour.to_linear();
        let shade = Colour::from_linear(Vec3::from(t.shade).clamp(Vec3::ZERO, Vec3::ONE), 1.0);
        let mut material = Material::toon(t.colour);
        material.shading = Shading::Toon(Toon {
            shade,
            shift: 0.12,
            softness: 0.02,
            rim: Rim { colour: Colour::WHITE, power: 4.0, strength: 0.0 },
            emissive: Colour::from_linear((lit * t.glow).clamp(Vec3::ZERO, Vec3::ONE), 1.0),
            skin: None,
        });
        material.outline = (self.line_width > 0.0).then_some(Outline { colour: t.line, width: self.line_width });
        material.receives_shadows = false;
        if t.opacity < 1.0 {
            material.base = Colour::from_linear(lit, t.opacity.clamp(0.0, 1.0));
            material.alpha = AlphaMode::Blend;
        }
        material
    }

    /// Checks the numbers, naming `at`; `actors` are the shot's, for an emitter on a bone.
    ///
    /// # Errors
    /// A sentence naming the first bad value.
    pub fn check(&self, at: &str, actors: &[Actor]) -> Result<(), String> {
        let t = self.tuning();
        let range = |name: &str, v: f32, lo: f32, hi: f32| {
            if v.is_finite() && (lo..=hi).contains(&v) {
                Ok(())
            } else {
                Err(format!("{at}.{name} is {lo} to {hi}, got {v}"))
            }
        };
        range("rate", t.rate, 0.1, 60.0)?;
        range("life", t.life, 0.1, 30.0)?;
        range("size", t.size, 0.01, 5.0)?;
        range("rise", t.rise, -5.0, 5.0)?;
        range("spread", t.spread, 0.0, 5.0)?;
        range("turbulence", t.turbulence, 0.0, 1.0)?;
        range("glow", t.glow, 0.0, 4.0)?;
        range("gravity", t.gravity, 0.0, 20.0)?;
        range("opacity", t.opacity, 0.05, 1.0)?;
        range("line_width", self.line_width, 0.0, 0.05)?;
        range("emitter.radius", self.emitter.radius, 0.0, 10.0)?;
        if !(16..=MAX_RESOLUTION).contains(&self.resolution) {
            return Err(format!("{at}.resolution is 16 to {MAX_RESOLUTION}"));
        }
        if t.rate * t.life > MAX_PUFFS as f32 {
            return Err(format!(
                "{at}: rate x life is {} puffs alive at once, over the {MAX_PUFFS} a volume holds; lower the rate or the \
                 life",
                t.rate * t.life
            ));
        }
        self.density.check(&format!("{at}.density"))?;
        match (&self.emitter.actor, &self.emitter.bone) {
            (None, None) => {}
            (Some(actor), Some(bone)) => {
                if !actors.iter().any(|a| &a.id == actor) {
                    return Err(format!("{at}.emitter: there is no actor '{actor}' in the shot"));
                }
                if omuzaki_anim_api::HumanoidBone::from_name(bone).is_none() {
                    return Err(format!("{at}.emitter: '{bone}' is not a humanoid bone"));
                }
            }
            _ => return Err(format!("{at}.emitter: give both an actor and a bone, or neither")),
        }
        Ok(())
    }
}

/// One puff alive at a moment.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Puff {
    /// Centre, world metres.
    pub centre: Vec3,
    /// Radius, metres.
    pub radius: f32,
}

/// A hash of `(a, b, seed)` in `0..1`.
fn hash(a: u32, b: u32, seed: u32) -> f32 {
    let mut h = a.wrapping_mul(0x8da6_b343) ^ b.wrapping_mul(0xd816_3841) ^ seed.wrapping_mul(0x9e37_79b9);
    h ^= h >> 16;
    h = h.wrapping_mul(0x7feb_352d);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846c_a68b);
    h ^= h >> 16;
    #[allow(clippy::cast_precision_loss, reason = "24 bits are exact")]
    let unit = (h >> 8) as f32 / 16_777_216.0;
    unit
}

/// A puff's size over its life (`x` in `0..=1`): it swells fast, holds, and thins away.
fn swell(x: f32) -> f32 {
    let grow = smoothstep(0.0, 0.22, x);
    let fade = 1.0 - smoothstep(0.62, 1.0, x);
    grow * fade
}

fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let u = ((x - a) / (b - a)).clamp(0.0, 1.0);
    u * u * (3.0 - 2.0 * u)
}

/// The puffs of `def` alive at `t` (seconds from the shot's start), the emitter at time `s` being `emitter(s)`.
pub fn puffs(def: &VolumeDef, t: f32, seed: u32, emitter: &dyn Fn(f32) -> Vec3) -> Vec<Puff> {
    let tune = def.tuning();
    let mut out = Vec::new();
    if t < def.start {
        return out;
    }
    let density = def.density.at(t).clamp(0.0, 2.0);
    if density <= 0.0 {
        return out;
    }
    let end = def.stop.map_or(t, |stop| stop.min(t));
    let seed = seed ^ def.seed.wrapping_mul(0x27d4_eb2f) ^ id_hash(&def.id);
    // The oldest puff still alive, then every one born since.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "puff counts are small")]
    let first = (((t - tune.life - def.start) * tune.rate).floor().max(0.0)) as u32;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "puff counts are small")]
    let last = (((end - def.start) * tune.rate).floor().max(0.0)) as u32;
    for k in first..=last {
        #[allow(clippy::cast_precision_loss, reason = "puff counts are small")]
        let born = def.start + k as f32 / tune.rate;
        let age = t - born;
        if age < 0.0 || age > tune.life || born > end {
            continue;
        }
        let r = |i: u32| hash(k, i, seed);
        // Born anywhere in the emitter's ball, heading up and out with its own speed.
        let dir = Vec3::new(r(1) * 2.0 - 1.0, r(2) * 2.0 - 1.0, r(3) * 2.0 - 1.0).normalize_or_zero();
        let start = emitter(born) + dir * def.emitter.radius * r(4).cbrt();
        let out_dir = Vec3::new(dir.x, 0.0, dir.z).normalize_or_zero();
        let speed = 0.7 + 0.6 * r(5);
        let travel =
            Vec3::Y * (tune.rise * speed * age - 0.5 * tune.gravity * age * age) + out_dir * tune.spread * speed * age;
        // A turbulent drift: each puff wanders on its own slow noise.
        let wander = tune.turbulence * tune.size * 1.2;
        let drift = Vec3::new(
            crate::curve::wiggle(age, 0.7, seed ^ k.wrapping_mul(3)),
            crate::curve::wiggle(age, 0.5, seed ^ k.wrapping_mul(5)),
            crate::curve::wiggle(age, 0.7, seed ^ k.wrapping_mul(7)),
        ) * wander
            * age.min(1.0);
        let radius = tune.size * (0.6 + 0.6 * r(6)) * swell(age / tune.life) * density;
        if radius > 1e-3 {
            out.push(Puff { centre: start + travel + drift, radius });
        }
        if out.len() >= MAX_PUFFS {
            break;
        }
    }
    out
}

fn id_hash(id: &str) -> u32 {
    id.bytes().fold(0x811c_9dc5u32, |h, b| (h ^ u32::from(b)).wrapping_mul(0x0100_0193))
}

/// 3D value noise in `0..1`.
fn noise3(p: Vec3, seed: u32) -> f32 {
    let i = p.floor();
    let f = p - i;
    let u = f * f * (Vec3::splat(3.0) - 2.0 * f);
    #[allow(clippy::cast_possible_truncation, reason = "a floored coordinate")]
    let (x, y, z) = (i.x as i32, i.y as i32, i.z as i32);
    #[allow(clippy::cast_sign_loss, reason = "the cell's bits are hashed")]
    let corner = |dx: i32, dy: i32, dz: i32| {
        hash(((x + dx) as u32).wrapping_mul(0x9e37_79b1) ^ ((y + dy) as u32), (z + dz) as u32, seed)
    };
    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
    let x00 = lerp(corner(0, 0, 0), corner(1, 0, 0), u.x);
    let x10 = lerp(corner(0, 1, 0), corner(1, 1, 0), u.x);
    let x01 = lerp(corner(0, 0, 1), corner(1, 0, 1), u.x);
    let x11 = lerp(corner(0, 1, 1), corner(1, 1, 1), u.x);
    lerp(lerp(x00, x10, u.y), lerp(x01, x11, u.y), u.z)
}

/// The volume's signed distance at `p` (negative inside): the puffs melted together, the edge broken by noise that
/// flows upward over time.
fn field(puffs: &[Puff], p: Vec3, blend: f32, roughness: f32, scale: f32, t: f32, seed: u32) -> f32 {
    let d = puffs
        .iter()
        .map(|puff| p.distance(puff.centre) - puff.radius)
        .reduce(|d, di| smooth_min(d, di, blend))
        .unwrap_or(f32::MAX);
    if roughness <= 0.0 {
        return d;
    }
    let q = p / scale - Vec3::Y * t * 0.6;
    let n = noise3(q, seed) * 0.6 + noise3(q * 2.1, seed ^ 1) * 0.3 + noise3(q * 4.3, seed ^ 2) * 0.1;
    d + (n - 0.5) * 2.0 * roughness
}

/// Polynomial smooth minimum (Quilez): melts two distances within `k` of each other.
fn smooth_min(a: f32, b: f32, k: f32) -> f32 {
    let h = (0.5 + 0.5 * (b - a) / k).clamp(0.0, 1.0);
    b + (a - b) * h - k * h * (1.0 - h)
}

/// The volume's surface at `t` as a mesh in world space, or `None` when nothing is alive.
pub fn mesh(def: &VolumeDef, t: f32, seed: u32, emitter: &dyn Fn(f32) -> Vec3) -> Option<MeshData> {
    let alive = puffs(def, t, seed, emitter);
    if alive.is_empty() {
        return None;
    }
    let tune = def.tuning();
    let roughness = tune.turbulence * tune.size * 0.45;
    let blend = tune.size * 0.45;
    let margin = roughness + blend * 0.5 + 1e-3;
    let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    for p in &alive {
        lo = lo.min(p.centre - Vec3::splat(p.radius + margin));
        hi = hi.max(p.centre + Vec3::splat(p.radius + margin));
    }
    #[allow(clippy::cast_precision_loss, reason = "a resolution of at most 128")]
    let cell = (hi - lo).max_element() / def.resolution as f32;
    let seed = seed ^ id_hash(&def.id);
    let f = |p: Vec3| field(&alive, p, blend, roughness, tune.size * 0.8, t, seed);
    surface_nets(&f, lo, hi, cell)
}

/// Meshes the zero level of `f` over the box `lo..hi` on a grid of `cell`-sized cells (naive surface nets: one vertex
/// per cell the surface crosses, at the mean of its edge crossings; one quad per grid edge the surface crosses), with
/// smooth normals from the field's gradient. Outward-facing triangles, counter-clockwise from outside.
pub fn surface_nets(f: &dyn Fn(Vec3) -> f32, lo: Vec3, hi: Vec3, cell: f32) -> Option<MeshData> {
    const CORNERS: [[usize; 3]; 8] =
        [[0, 0, 0], [1, 0, 0], [0, 1, 0], [1, 1, 0], [0, 0, 1], [1, 0, 1], [0, 1, 1], [1, 1, 1]];
    const EDGES: [[usize; 2]; 12] =
        [[0, 1], [2, 3], [4, 5], [6, 7], [0, 2], [1, 3], [4, 6], [5, 7], [0, 4], [1, 5], [2, 6], [3, 7]];
    if !(cell > 0.0 && cell.is_finite()) {
        return None;
    }
    let size = ((hi - lo) / cell).ceil();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a grid of at most a few hundred")]
    let [nx, ny, nz] = [size.x as usize + 1, size.y as usize + 1, size.z as usize + 1];
    if nx * ny * nz > 400 * 400 * 400 {
        return None;
    }
    let at = |x: usize, y: usize, z: usize| {
        #[allow(clippy::cast_precision_loss, reason = "grid indices")]
        let v = lo + Vec3::new(x as f32, y as f32, z as f32) * cell;
        v
    };
    // Samples at the grid's points.
    let (px, py, pz) = (nx + 1, ny + 1, nz + 1);
    let index = |x: usize, y: usize, z: usize| (z * py + y) * px + x;
    let mut values = vec![0.0f32; px * py * pz];
    for z in 0..pz {
        for y in 0..py {
            for x in 0..px {
                values[index(x, y, z)] = f(at(x, y, z));
            }
        }
    }
    let value = |x: usize, y: usize, z: usize| values[index(x, y, z)];
    let gradient = |p: Vec3| {
        let e = cell * 0.5;
        Vec3::new(
            f(p + Vec3::X * e) - f(p - Vec3::X * e),
            f(p + Vec3::Y * e) - f(p - Vec3::Y * e),
            f(p + Vec3::Z * e) - f(p - Vec3::Z * e),
        )
        .normalize_or_zero()
    };
    let mut vertex_of = vec![u32::MAX; nx * ny * nz];
    let cell_index = |x: usize, y: usize, z: usize| (z * ny + y) * nx + x;
    let mut mesh = MeshData::default();
    for z in 0..nz {
        for y in 0..ny {
            for x in 0..nx {
                let v: [f32; 8] = CORNERS.map(|[dx, dy, dz]| value(x + dx, y + dy, z + dz));
                let inside = v.iter().filter(|s| **s < 0.0).count();
                if inside == 0 || inside == 8 {
                    continue;
                }
                let mut sum = Vec3::ZERO;
                let mut n = 0.0;
                for [a, b] in EDGES {
                    if (v[a] < 0.0) != (v[b] < 0.0) {
                        let s = v[a] / (v[a] - v[b]);
                        let pa = Vec3::from(CORNERS[a].map(|c| {
                            #[allow(clippy::cast_precision_loss, reason = "0 or 1")]
                            let c = c as f32;
                            c
                        }));
                        let pb = Vec3::from(CORNERS[b].map(|c| {
                            #[allow(clippy::cast_precision_loss, reason = "0 or 1")]
                            let c = c as f32;
                            c
                        }));
                        sum += pa + (pb - pa) * s;
                        n += 1.0;
                    }
                }
                let p = at(x, y, z) + sum / n * cell;
                vertex_of[cell_index(x, y, z)] = u32::try_from(mesh.positions.len()).unwrap_or(u32::MAX);
                mesh.positions.push(p);
                mesh.normals.push(gradient(p));
            }
        }
    }
    // One quad per grid edge the surface crosses, joining the four cells around it.
    let mut quad = |cells: [(usize, usize, usize); 4], flip: bool| {
        let ids = cells.map(|(x, y, z)| vertex_of[cell_index(x, y, z)]);
        if ids.contains(&u32::MAX) {
            return;
        }
        let [a, b, c, d] = if flip { [ids[0], ids[3], ids[2], ids[1]] } else { ids };
        mesh.indices.extend_from_slice(&[a, b, c, a, c, d]);
    };
    for z in 0..pz {
        for y in 0..py {
            for x in 0..px {
                let here = value(x, y, z) < 0.0;
                // Edge along +X from (x, y, z): the cells around it share y-1..y and z-1..z.
                if x + 1 < px && y >= 1 && z >= 1 && y < ny && z < nz && x < nx && here != (value(x + 1, y, z) < 0.0) {
                    quad([(x, y - 1, z - 1), (x, y, z - 1), (x, y, z), (x, y - 1, z)], !here);
                }
                if y + 1 < py && x >= 1 && z >= 1 && x < nx && z < nz && y < ny && here != (value(x, y + 1, z) < 0.0) {
                    quad([(x - 1, y, z - 1), (x - 1, y, z), (x, y, z), (x, y, z - 1)], !here);
                }
                if z + 1 < pz && x >= 1 && y >= 1 && x < nx && y < ny && z < nz && here != (value(x, y, z + 1) < 0.0) {
                    quad([(x - 1, y - 1, z), (x, y - 1, z), (x, y, z), (x - 1, y, z)], !here);
                }
            }
        }
    }
    (!mesh.indices.is_empty()).then_some(mesh)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(preset: VolumePreset) -> VolumeDef {
        serde_json::from_value(serde_json::json!({"id": "v", "preset": preset.name()})).unwrap()
    }

    #[test]
    fn a_sphere_meshes_closed_and_facing_out() {
        let f = |p: Vec3| p.length() - 0.5;
        let mesh = surface_nets(&f, Vec3::splat(-0.7), Vec3::splat(0.7), 0.05).unwrap();
        mesh.validate().unwrap();
        for p in &mesh.positions {
            assert!((p.length() - 0.5).abs() < 0.03, "a vertex off the sphere: {p}");
        }
        // Every triangle faces away from the centre.
        for tri in mesh.indices.chunks(3) {
            let [a, b, c] = [0, 1, 2].map(|i| mesh.positions[tri[i] as usize]);
            let n = (b - a).cross(c - a);
            assert!(n.dot(a + b + c) > 0.0, "a triangle faces in");
        }
        // Closed: every edge is shared by exactly two triangles.
        let mut edges = std::collections::BTreeMap::new();
        for tri in mesh.indices.chunks(3) {
            for (a, b) in [(tri[0], tri[1]), (tri[1], tri[2]), (tri[2], tri[0])] {
                *edges.entry((a.min(b), a.max(b))).or_insert(0) += 1;
            }
        }
        assert!(edges.values().all(|n| *n == 2), "the mesh has holes");
    }

    #[test]
    fn puffs_are_born_rise_and_die() {
        let volume = def(VolumePreset::Smoke);
        let at = |_: f32| Vec3::ZERO;
        assert!(puffs(&volume, -0.1, 1, &at).is_empty(), "nothing before the start");
        let early = puffs(&volume, 0.5, 1, &at);
        let later = puffs(&volume, 2.0, 1, &at);
        assert!(later.len() > early.len());
        let mean_y = |p: &[Puff]| p.iter().map(|p| p.centre.y).sum::<f32>() / p.len() as f32;
        assert!(mean_y(&later) > mean_y(&early), "smoke rises");
        let stopped = VolumeDef { stop: Some(1.0), ..volume };
        assert!(puffs(&stopped, 1.0 + stopped.tuning().life + 0.1, 1, &at).is_empty(), "all gone after the last life");
    }

    #[test]
    fn the_same_time_gives_the_same_mesh() {
        let volume = def(VolumePreset::Goo);
        let at = |s: f32| Vec3::new(s * 0.1, 0.0, 0.0);
        let a = mesh(&volume, 1.3, 9, &at).unwrap();
        let b = mesh(&volume, 1.3, 9, &at).unwrap();
        assert_eq!(a.positions, b.positions);
        assert_eq!(a.indices, b.indices);
    }

    #[test]
    fn a_splash_arcs_up_and_falls_back_and_shows_what_is_behind() {
        let volume = def(VolumePreset::Splash);
        let at = |_: f32| Vec3::ZERO;
        let t = volume.tuning();
        // A puff born at the start peaks near rise / gravity and is back below its peak by its life's end.
        let height = |age: f32| {
            let one = VolumeDef { stop: Some(0.0), ..volume.clone() };
            let p = puffs(&one, age, 3, &at);
            p.iter().map(|p| p.centre.y).sum::<f32>() / p.len().max(1) as f32
        };
        let peak = t.rise / t.gravity;
        assert!(height(peak) > height(0.1), "it rises first");
        assert!(height(t.life * 0.95) < height(peak) - 0.3, "then falls: {} at the peak", height(peak));
        let material = volume.material();
        material.validate().unwrap();
        assert_eq!(material.alpha, AlphaMode::Blend);
        assert!((material.base.alpha() - 0.8).abs() < 0.01);
        let solid = VolumeDef { opacity: Some(1.0), ..volume.clone() };
        assert_eq!(solid.material().alpha, AlphaMode::Opaque, "a solid one stays opaque");
        let thin = VolumeDef { opacity: Some(0.0), ..volume };
        assert!(thin.check("v", &[]).unwrap_err().contains("opacity"));
    }

    #[test]
    fn density_scales_it_away() {
        let mut volume = def(VolumePreset::Smoke);
        volume.density = Track::Hold(0.0);
        assert!(mesh(&volume, 1.0, 1, &|_| Vec3::ZERO).is_none());
    }

    #[test]
    fn every_preset_checks_and_makes_a_valid_material() {
        for preset in VolumePreset::ALL {
            let volume = def(preset);
            volume.check("v", &[]).unwrap();
            volume.material().validate().unwrap();
        }
    }
}
