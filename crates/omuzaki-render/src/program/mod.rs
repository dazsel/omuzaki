//! Shading programs: looks beyond toon (hair strands and the angel ring, and whatever follows) that register
//! themselves by name (`Shading::Program`, design decision D13, spec §39 rule 2).
//!
//! # Adding a program
//!
//! One file in this directory and one `mod` line below. The file declares a [`ProgramSpec`] and registers it:
//!
//! ```ignore
//! register_shading_program!(
//!     "my-look",
//!     ProgramSpec {
//!         name: "my-look",
//!         params: &[ParamSpec::float("strength", 0.5, 0.0, 1.0)],
//!         attributes: &[AttributeSpec { name: "my-uv", components: 2 }],
//!         toon: |_| Toon::default(),
//!         wgsl: include_str!("my_look.wgsl"),
//!     }
//! );
//! ```
//!
//! Nothing else is edited: no central list, no enum, no change to the renderer. The registry is what the linker
//! collected (`inventory`, as ops are), sorted by name so indices are stable. It feeds `Capabilities::shading_programs`,
//! parameter checking and packing, the extra vertex streams and the shader the Opaque pass compiles for the program.
//!
//! # What a program is given
//!
//! - **Parameters** are packed, in declaration order, into the material uniform's `program[]` (64 floats; colours
//!   arrive in linear light). The shader sees each one's float offset as a constant named `P_<NAME>` (upper case,
//!   `-` as `_`), generated from the declaration so the two cannot drift, and reads them with `program_float`,
//!   `program_vec2` and `program_vec3`.
//! - **Extra vertex attributes** (`MeshData.extra`) arrive in `ProgramIn.extra[i]`, `i` being the declaration order,
//!   as a `vec4` whose unused components are `0, 0, 0, 1`. A mesh that lacks an attribute (or has it with another
//!   component count) supplies zeros: its slot is bound to one shared zero buffer with stride 0.
//! - **Three hooks** in the shader (`wgsl`): `program_albedo` changes the surface colour before lighting,
//!   `program_light` the lit colour after it and `program_alpha(p, alpha)` (optional, identity by default) the alpha of a
//!   blended surface; only the Transparent pass calls it. `ProgramIn.world_position` is the fragment's world position.
//!   Everything else (the toon terminator, lights, rim, skin, base texture,
//!   overlays, alpha, outline) is the renderer's, so a program stays small.
//! - **The toon base** the program lights with comes from `toon`, a function of the resolved parameters: the contract
//!   gives `Shading::Program` no toon fields of its own.
//!
//! An unknown program name is not an error (contract rule 5): the material draws as the default toon. The mistake is
//! still reported where a caller can ask: [`check_shading_program`] returns an [`UnknownProgram`] that suggests the
//! closest registered names.

use std::fmt;
use std::sync::OnceLock;

use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{Vec2, Vec3};
use omuzaki_core::id::Slug;
use omuzaki_render_api::{ParamValue, ProgramParams, RenderError, Shading, Toon};

mod band_highlight;
mod eye;
mod gem;
mod hair;
mod metal;
mod physical;
mod shadow_catcher;
mod sheen;
mod skin;

/// Floats in the material uniform's `program[]` (`array<vec4<f32>, 16>`).
pub(crate) const PACKED_FLOATS: usize = 64;
/// The most extra vertex attributes a program reads (the most a mesh carries).
pub(crate) const MAX_ATTRIBUTES: usize = 4;

/// One parameter of a program: its name, default and, for numbers, the range a material may set.
#[derive(Debug)]
pub(crate) struct ParamSpec {
    pub(crate) name: &'static str,
    pub(crate) default: ParamValue,
    /// Inclusive bounds on every component of a `Float`, `Vec2` or `Vec3`; `None` accepts any finite value.
    pub(crate) range: Option<(f32, f32)>,
}

impl ParamSpec {
    /// A number in `lo..=hi`.
    pub(crate) const fn float(name: &'static str, default: f32, lo: f32, hi: f32) -> Self {
        Self { name, default: ParamValue::Float(default), range: Some((lo, hi)) }
    }

    /// Three numbers, each in `lo..=hi`.
    pub(crate) const fn vec3(name: &'static str, default: [f32; 3], lo: f32, hi: f32) -> Self {
        Self { name, default: ParamValue::Vec3(Vec3::new(default[0], default[1], default[2])), range: Some((lo, hi)) }
    }

    /// A colour given as `0xRRGGBB`.
    pub(crate) const fn colour(name: &'static str, rgb: u32) -> Self {
        Self { name, default: ParamValue::Colour(Colour::hex(rgb)), range: None }
    }

    /// How many floats the parameter takes in the packed block.
    fn floats(&self) -> usize {
        match self.default {
            ParamValue::Float(_) | ParamValue::Bool(_) => 1,
            ParamValue::Vec2(_) => 2,
            ParamValue::Vec3(_) | ParamValue::Colour(_) => 3,
        }
    }

    fn type_name(&self) -> &'static str {
        match self.default {
            ParamValue::Float(_) => "float",
            ParamValue::Vec2(_) => "vec2",
            ParamValue::Vec3(_) => "vec3",
            ParamValue::Colour(_) => "colour",
            ParamValue::Bool(_) => "bool",
        }
    }
}

/// An extra vertex attribute a program reads (`MeshData.extra`).
#[derive(Debug)]
pub(crate) struct AttributeSpec {
    pub(crate) name: &'static str,
    /// Floats per vertex, `1..=4`.
    pub(crate) components: u8,
}

/// Everything the renderer needs to know about one program.
#[derive(Debug)]
pub(crate) struct ProgramSpec {
    /// The name materials select it by (a valid `Slug`).
    pub(crate) name: &'static str,
    /// Its parameters; their order is their order in the packed block.
    pub(crate) params: &'static [ParamSpec],
    /// The extra vertex attributes it reads, at most [`MAX_ATTRIBUTES`]; the order is the `ProgramIn.extra` index.
    pub(crate) attributes: &'static [AttributeSpec],
    /// The toon model the program lights with, given the resolved parameters.
    pub(crate) toon: fn(&Values) -> Toon,
    /// WGSL defining `program_albedo` and `program_light` (see `program/none.wgsl` for the signatures) and, optionally,
    /// `program_alpha` (see `program/alpha_default.wgsl`).
    pub(crate) wgsl: &'static str,
}

inventory::collect!(ProgramSpec);

/// Registers a [`ProgramSpec`] under its name from the file that declares it (spec §39 rule 2). The name comes first
/// so the registration is recorded, and listed by `omuzaki registrations`, as `"hair"` rather than as the whole spec
/// literal (#1638); the build fails if it differs from the spec's own `name`.
macro_rules! register_shading_program {
    ($name:literal, $spec:expr) => {
        const _: () = ::core::assert!(
            $crate::program::same_name($name, $spec.name),
            ::core::concat!("register_shading_program!(", $name, ", ..) names a spec of another name")
        );
        ::inventory::submit! { $spec }
        // The link gate (ADR 0015) compares this record with the registration the scan reads from the source.
        ::omuzaki_core::__record_registration!("shading_program", $name);
    };
}
use register_shading_program;

// ------------------------------------------------------------------------------------------------ the style axis

/// A program's parameters followed by the style axis's (#2607): `style` (`0` the program's physical model, `1` the cel
/// look) and the cel toon the program blends toward (`cel-shade`, `cel-shift`, `cel-softness`, `cel-rim`,
/// `cel-rim-power`, `cel-rim-strength`, `cel-emissive`, `cel-skin` with `cel-skin-floor` and `cel-skin-hue`), which
/// [`cel_toon`] turns back into the [`Toon`] the program lights with. The look writes them from the material's own toon
/// when it moves a toon material along the axis, so at `style` 1 the program draws exactly what the toon did: the axis
/// has no step. `$style` is the default style; the cel defaults are a flat white toon with no rim (what a program that
/// lights itself wants by default).
macro_rules! with_cel {
    ($style:expr; $($param:expr),* $(,)?) => {
        &[
            $($param,)*
            $crate::program::ParamSpec::float("style", $style, 0.0, 1.0),
            $crate::program::ParamSpec::colour("cel-shade", 0x00ff_ffff),
            $crate::program::ParamSpec::float("cel-shift", 0.0, -1.0, 1.0),
            $crate::program::ParamSpec::float("cel-softness", 1.0, 0.0, 1.0),
            $crate::program::ParamSpec::colour("cel-rim", 0x006d_74a8),
            $crate::program::ParamSpec::float("cel-rim-power", 3.0, 0.0, 64.0),
            $crate::program::ParamSpec::float("cel-rim-strength", 0.0, 0.0, 4.0),
            $crate::program::ParamSpec::colour("cel-emissive", 0x0000_0000),
            $crate::program::ParamSpec::float("cel-skin", 0.0, 0.0, 1.0),
            $crate::program::ParamSpec::float("cel-skin-floor", 0.42, 0.0, 1.0),
            $crate::program::ParamSpec::float("cel-skin-hue", 0.55, 0.0, 1.0),
        ]
    };
}
use with_cel;

/// The cel toon a program on the style axis lights with, from its `cel-*` parameters ([`with_cel!`]).
pub(crate) fn cel_toon(values: &Values) -> Toon {
    let f = |name: &str, default: f32| values.float(name).unwrap_or(default);
    let skin = (f("cel-skin", 0.0) >= 0.5)
        .then(|| omuzaki_render_api::Skin { floor: f("cel-skin-floor", 0.42), hue_keep: f("cel-skin-hue", 0.55) });
    Toon {
        shade: values.colour("cel-shade").unwrap_or(Colour::WHITE),
        shift: f("cel-shift", 0.0),
        softness: f("cel-softness", 1.0),
        rim: omuzaki_render_api::Rim {
            colour: values.colour("cel-rim").unwrap_or(Colour::hex(0x006d_74a8)),
            power: f("cel-rim-power", 3.0),
            strength: f("cel-rim-strength", 0.0),
        },
        emissive: values.colour("cel-emissive").unwrap_or(Colour::BLACK),
        skin,
    }
}

/// Whether a registration's name is its spec's name, at compile time.
pub(crate) const fn same_name(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// A material's parameters with defaults filled in, in declaration order.
pub(crate) struct Values {
    items: Vec<(&'static str, ParamValue)>,
}

impl Values {
    /// The colour parameter `name`, if the program declares one.
    pub(crate) fn colour(&self, name: &str) -> Option<Colour> {
        self.items.iter().find_map(|(n, v)| match v {
            ParamValue::Colour(c) if *n == name => Some(*c),
            _ => None,
        })
    }

    /// The float parameter `name`, if the program declares one.
    pub(crate) fn float(&self, name: &str) -> Option<f32> {
        self.items.iter().find_map(|(n, v)| match v {
            ParamValue::Float(f) if *n == name => Some(*f),
            _ => None,
        })
    }
}

/// A material's program, resolved: what to compile, what to upload, how to light.
pub(crate) struct Resolved {
    /// Position in the registry, which names the program's shader and pipelines.
    pub(crate) index: usize,
    /// The parameters packed into `program[]`.
    pub(crate) packed: [[f32; 4]; PACKED_FLOATS / 4],
    /// The toon model the program asked for.
    pub(crate) toon: Toon,
}

/// Why a name matched no program: the names that nearly matched it, and every name there is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownProgram {
    /// The name that was asked for.
    pub name: String,
    /// Registered names close to it, nearest first (at most three).
    pub suggestions: Vec<String>,
    /// Every registered name.
    pub known: Vec<String>,
}

impl fmt::Display for UnknownProgram {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown shading program {:?}", self.name)?;
        if let Some(first) = self.suggestions.first() {
            write!(f, "; did you mean {first:?}?")?;
        }
        write!(f, " (known programs: {})", self.known.join(", "))
    }
}

impl std::error::Error for UnknownProgram {}

/// The programs this build contains, by name.
pub(crate) struct Registry {
    programs: Vec<&'static ProgramSpec>,
}

impl Registry {
    /// Checks `specs` and sorts them by name.
    ///
    /// # Errors
    /// `Backend` naming the first rule a declaration breaks: a duplicate name, a name that is not a slug, a parameter
    /// block over 64 floats, more than four attributes or a component count outside `1..=4`.
    pub(crate) fn build(mut specs: Vec<&'static ProgramSpec>) -> Result<Self, RenderError> {
        specs.sort_by_key(|s| s.name);
        for pair in specs.windows(2) {
            if pair[0].name == pair[1].name {
                return Err(bad(pair[0], "is registered twice; rename one of the programs"));
            }
        }
        for spec in &specs {
            check_declaration(spec)?;
        }
        Ok(Self { programs: specs })
    }

    /// The programs linked into this binary.
    ///
    /// # Errors
    /// As [`build`](Self::build). The result is computed once.
    pub(crate) fn global() -> Result<&'static Self, RenderError> {
        static GLOBAL: OnceLock<Result<Registry, RenderError>> = OnceLock::new();
        GLOBAL
            .get_or_init(|| Self::build(inventory::iter::<ProgramSpec>.into_iter().collect()))
            .as_ref()
            .map_err(Clone::clone)
    }

    /// Every registered name, in order.
    pub(crate) fn names(&self) -> Vec<Slug> {
        self.programs.iter().filter_map(|s| Slug::new(s.name).ok()).collect()
    }

    /// The program called `name`, with its index.
    ///
    /// # Errors
    /// [`UnknownProgram`], with did-you-mean suggestions.
    pub(crate) fn lookup(&self, name: &str) -> Result<(usize, &'static ProgramSpec), UnknownProgram> {
        if let Ok(index) = self.programs.binary_search_by_key(&name, |s| s.name) {
            return Ok((index, self.programs[index]));
        }
        let known: Vec<&str> = self.programs.iter().map(|s| s.name).collect();
        Err(UnknownProgram {
            name: name.to_owned(),
            suggestions: suggest(name, &known),
            known: known.into_iter().map(str::to_owned).collect(),
        })
    }

    /// The program at `index` (an index [`lookup`](Self::lookup) returned).
    pub(crate) fn get(&self, index: usize) -> Option<&'static ProgramSpec> {
        self.programs.get(index).copied()
    }

    /// Resolves a material's shading to its program, or `None` when it is not a program or the program is unknown (the
    /// material then draws as the default toon, contract rule 5).
    ///
    /// # Errors
    /// `Invalid` if a parameter is unknown, of the wrong type or out of range. The message lists what is valid.
    pub(crate) fn resolve(&self, shading: &Shading) -> Result<Option<Resolved>, RenderError> {
        let Shading::Program { name, params } = shading else { return Ok(None) };
        let Ok((index, spec)) = self.lookup(name.as_str()) else { return Ok(None) };
        let values = values_of(spec, params)?;
        let toon = (spec.toon)(&values);
        Ok(Some(Resolved { index, packed: pack(&values), toon }))
    }
}

fn bad(spec: &ProgramSpec, what: &str) -> RenderError {
    RenderError::Backend(format!("shading program {:?} {what}", spec.name))
}

fn check_declaration(spec: &ProgramSpec) -> Result<(), RenderError> {
    let slug = |name: &str, what: &str| {
        Slug::new(name)
            .map(drop)
            .map_err(|e| bad(spec, &format!("has a {what} {name:?} that is not a valid name: {e}")))
    };
    slug(spec.name, "name")?;
    let mut floats = 0;
    for (i, p) in spec.params.iter().enumerate() {
        slug(p.name, "parameter")?;
        if spec.params[..i].iter().any(|q| q.name == p.name) {
            return Err(bad(spec, &format!("declares the parameter {:?} twice", p.name)));
        }
        floats += p.floats();
    }
    if floats > PACKED_FLOATS {
        return Err(bad(spec, &format!("declares {floats} floats of parameters; the most is {PACKED_FLOATS}")));
    }
    if spec.attributes.len() > MAX_ATTRIBUTES {
        return Err(bad(
            spec,
            &format!("reads {} extra attributes; the most is {MAX_ATTRIBUTES}", spec.attributes.len()),
        ));
    }
    for a in spec.attributes {
        slug(a.name, "attribute")?;
        if !(1..=4).contains(&a.components) {
            return Err(bad(spec, &format!("reads {:?} with {} components; use 1 to 4", a.name, a.components)));
        }
    }
    Ok(())
}

/// The parameters `params` sets on `spec`, checked, with defaults for the rest, in declaration order.
fn values_of(spec: &'static ProgramSpec, params: &ProgramParams) -> Result<Values, RenderError> {
    let invalid = |reason: String| RenderError::invalid("shading program parameters", reason);
    let declared: Vec<&str> = spec.params.iter().map(|p| p.name).collect();
    for name in params.keys() {
        if !declared.contains(&name.as_str()) {
            let hint = suggest(name.as_str(), &declared)
                .first()
                .map_or_else(String::new, |s| format!("; did you mean {s:?}?"));
            return Err(invalid(format!(
                "{:?} has no parameter {:?}{hint} (it has: {})",
                spec.name,
                name.as_str(),
                declared.join(", ")
            )));
        }
    }
    let mut items = Vec::with_capacity(spec.params.len());
    for p in spec.params {
        let value = match Slug::new(p.name).ok().and_then(|k| params.get(&k)) {
            None => p.default,
            Some(v) => {
                check_value(spec, p, v).map_err(invalid)?;
                *v
            }
        };
        items.push((p.name, value));
    }
    Ok(Values { items })
}

fn check_value(spec: &ProgramSpec, p: &ParamSpec, value: &ParamValue) -> Result<(), String> {
    if std::mem::discriminant(value) != std::mem::discriminant(&p.default) {
        return Err(format!("{:?}.{} is a {}, not the {} given", spec.name, p.name, p.type_name(), kind_of(value)));
    }
    let Some((lo, hi)) = p.range else { return Ok(()) };
    let parts: &[f32] = match value {
        ParamValue::Float(v) => &[*v],
        ParamValue::Vec2(v) => &[v.x, v.y],
        ParamValue::Vec3(v) => &[v.x, v.y, v.z],
        ParamValue::Colour(_) | ParamValue::Bool(_) => &[],
    };
    match parts.iter().find(|v| !(lo..=hi).contains(*v)) {
        Some(v) => Err(format!("{:?}.{} is {v}, outside {lo}..={hi}", spec.name, p.name)),
        None => Ok(()),
    }
}

fn kind_of(value: &ParamValue) -> &'static str {
    match value {
        ParamValue::Float(_) => "float",
        ParamValue::Vec2(_) => "vec2",
        ParamValue::Vec3(_) => "vec3",
        ParamValue::Colour(_) => "colour",
        ParamValue::Bool(_) => "bool",
    }
}

/// The parameters as the shader reads them: tightly packed floats in declaration order, colours in linear light.
fn pack(values: &Values) -> [[f32; 4]; PACKED_FLOATS / 4] {
    let mut flat = [0.0_f32; PACKED_FLOATS];
    let mut at = 0;
    for (_, value) in &values.items {
        let parts: ([f32; 3], usize) = match *value {
            ParamValue::Float(v) => ([v, 0.0, 0.0], 1),
            ParamValue::Bool(v) => ([f32::from(u8::from(v)), 0.0, 0.0], 1),
            ParamValue::Vec2(Vec2 { x, y }) => ([x, y, 0.0], 2),
            ParamValue::Vec3(Vec3 { x, y, z }) => ([x, y, z], 3),
            ParamValue::Colour(c) => (c.to_linear().to_array(), 3),
        };
        // The declaration was checked to fit, so the slice is in range.
        if let Some(slot) = flat.get_mut(at..at + parts.1) {
            slot.copy_from_slice(&parts.0[..parts.1]);
        }
        at += parts.1;
    }
    std::array::from_fn(|row| std::array::from_fn(|c| flat[row * 4 + c]))
}

/// The WGSL constants naming each parameter's float offset (`P_RING_STRENGTH`), generated from the declaration.
fn offsets(spec: &ProgramSpec) -> String {
    let mut at = 0;
    spec.params
        .iter()
        .map(|p| {
            let line = format!("const P_{}: u32 = {at}u;\n", p.name.to_ascii_uppercase().replace('-', "_"));
            at += p.floats();
            line
        })
        .collect()
}

/// The shader of the Opaque pass for `spec`, or for no program (`None`): the shared declarations, the environment, the toon
/// model, the program-parameter helpers, the generated offsets and the program's hooks (`program_alpha` defaulting to the
/// identity).
pub(crate) fn opaque_source(spec: Option<&ProgramSpec>) -> String {
    let (prelude, hooks) = spec.map_or_else(|| (String::new(), include_str!("none.wgsl")), |s| (offsets(s), s.wgsl));
    // `program_alpha` is optional: a program that does not define it leaves the alpha alone.
    let alpha = if hooks.contains("fn program_alpha(") { "" } else { include_str!("alpha_default.wgsl") };
    [
        include_str!("../shader/common.wgsl"),
        include_str!("../shader/environment.wgsl"),
        include_str!("../shader/toon.wgsl"),
        include_str!("support.wgsl"),
        &prelude,
        hooks,
        alpha,
    ]
    .join("\n")
}

/// Checks that `name` is a registered shading program. A material may name any program (an unknown one draws as toon),
/// so this is for tools and ops that want to tell the author "this look needs a program this renderer lacks".
///
/// # Errors
/// [`UnknownProgram`] with the closest registered names. (If the registry itself is invalid, `known` is empty: renderer
/// creation reports that case.)
pub fn check_shading_program(name: &str) -> Result<(), UnknownProgram> {
    match Registry::global() {
        Ok(registry) => registry.lookup(name).map(drop),
        Err(_) => Err(UnknownProgram { name: name.to_owned(), suggestions: Vec::new(), known: Vec::new() }),
    }
}

/// Registered names close to `name`, nearest first, at most three: within a third of its length in edits, or
/// containing it (or contained by it).
fn suggest(name: &str, candidates: &[&str]) -> Vec<String> {
    let limit = (name.chars().count() / 3).max(1);
    let mut near: Vec<(usize, &str)> = candidates
        .iter()
        .filter_map(|&c| {
            let d = edit_distance(name, c);
            let contained = !name.is_empty() && (c.contains(name) || name.contains(c));
            (d <= limit || contained).then_some((d, c))
        })
        .collect();
    near.sort_unstable();
    near.into_iter().take(3).map(|(_, c)| c.to_owned()).collect()
}

/// Edit distance over chars where swapping two neighbours costs one edit (optimal string alignment), because a transposed
/// pair of letters is the commonest typo.
fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut table = vec![vec![0_usize; b.len() + 1]; a.len() + 1];
    for (i, row) in table.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in table[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let same = usize::from(a[i - 1] != b[j - 1]);
            let mut best = (table[i - 1][j] + 1).min(table[i][j - 1] + 1).min(table[i - 1][j - 1] + same);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(table[i - 2][j - 2] + 1);
            }
            table[i][j] = best;
        }
    }
    table[a.len()][b.len()]
}

#[cfg(test)]
mod tests;
