//! Generator dispatch: every group of a hairstyle compiled to clumps.
//!
//! A [`Generator`] variant maps to one function, `gen::<name>::compile(&Gen, &mut HairGenCtx) -> CompiledGroup`, and a
//! twin `gen::<name>::clump_count(&Gen) -> usize` that says how many clumps `compile` makes from the parameters alone
//! (a recolour has no head to compile against, but must still know which vertices belong to which clump). Each generator
//! lives in its own file; this file never changes for a new generator.
//!
//! **Order is part of the result.** Groups compile in id order; a mirrored group makes its clumps first and the mirror
//! images after them; the random draws are consumed in that order. Streaks (CB-34) pick clumps by this order, so it is
//! stable by construction.
//!
//! **Randomness.** [`HairGenCtx::rnd`] draws from the part's forked stream, or, for [`HairRng::LegacyLcg`], from the
//! prototype's Park-Miller stream shared by all groups (parity mode). Generators never construct their own generators.

use omuzaki_char_api::{
    BodyFit, Bulge, Dynamics, Generator, GroupId, GuidePoint, HairClump, HairGroup, HairParams, HairRng, HairSide,
    HeadSurface, LegacyLcg, LockShape, Quality, Taper,
};
use omuzaki_core::glam::Vec3;
use omuzaki_core::mesh::MeshData;
use omuzaki_core::rng::Rng;
use omuzaki_face_api::{Palette, narrow};

use super::r#gen;
use super::tie::Tie;

/// A stream of uniform numbers in `0..1`.
pub trait UnitStream {
    /// The next number.
    fn next_unit(&mut self) -> f64;
}

impl<T: UnitStream + ?Sized> UnitStream for Box<T> {
    fn next_unit(&mut self) -> f64 {
        (**self).next_unit()
    }
}

impl UnitStream for LegacyLcg {
    fn next_unit(&mut self) -> f64 {
        LegacyLcg::next_unit(self)
    }
}

/// The part's own forked stream as a [`UnitStream`].
pub struct Forked<'a>(pub &'a mut Rng);

impl UnitStream for Forked<'_> {
    fn next_unit(&mut self) -> f64 {
        f64::from(self.0.next_f32())
    }
}

/// The stream a hairstyle draws from, as its [`HairRng`] chooses: `forked` for new characters, the prototype's
/// Park-Miller stream (seeded from the parameters) in parity mode.
///
/// # Errors
/// The offending seed, if it is not a valid [`LegacyLcg`] seed.
pub fn stream<'a>(mode: HairRng, forked: &'a mut Rng) -> Result<Box<dyn UnitStream + 'a>, u32> {
    Ok(match mode {
        HairRng::Forked => Box::new(Forked(forked)),
        HairRng::LegacyLcg { seed } => Box::new(LegacyLcg::new(seed).ok_or(seed)?),
    })
}

/// What a generator sees: the head it grows from, the shared random stream, the palette and quality, and the group's
/// motion settings.
pub struct HairGenCtx<'a> {
    /// The head surface the hair is rooted on.
    pub head: &'a dyn HeadSurface,
    /// The character's colours.
    pub palette: &'a Palette,
    /// Level of detail.
    pub quality: Quality,
    /// The body the hair hangs over, where the build has one (a test or tool with a bare head has none). Generators
    /// that fall past the shoulders keep clear of it (`curtain`).
    pub body: Option<&'a dyn BodyFit>,
    group: &'a GroupId,
    dynamics: &'a Dynamics,
    rng: &'a mut dyn UnitStream,
}

impl<'a> HairGenCtx<'a> {
    /// A context for the group `group` with motion `dynamics`, drawing from `rng`.
    pub fn new(
        head: &'a dyn HeadSurface,
        palette: &'a Palette,
        quality: Quality,
        group: &'a GroupId,
        dynamics: &'a Dynamics,
        rng: &'a mut dyn UnitStream,
    ) -> Self {
        Self { head, palette, quality, body: None, group, dynamics, rng }
    }

    /// The same context with the `body` the hair hangs over.
    #[must_use]
    pub fn with_body(mut self, body: Option<&'a dyn BodyFit>) -> Self {
        self.body = body;
        self
    }

    /// The next random number in `0..1` (the prototype's `rnd`).
    pub fn rnd(&mut self) -> f64 {
        self.rng.next_unit()
    }

    /// A random number in `lo..hi` (the prototype's `rr`).
    pub fn rr(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.rnd()
    }

    /// The bind-space position of a guide point on this ctx's head.
    pub fn point(&self, p: GuidePoint) -> Vec3 {
        p.resolve(self.head)
    }

    /// The group being compiled.
    pub fn group(&self) -> &GroupId {
        self.group
    }

    /// The group's motion settings.
    pub fn dynamics(&self) -> &Dynamics {
        self.dynamics
    }

    /// A clump of this group with the group's motion settings, so no generator fills them in by hand. `rest` is root
    /// first.
    pub fn clump(&self, rest: Vec<Vec3>, width: Taper, thickness: Bulge, shade: f64) -> HairClump {
        HairClump {
            group: self.group.clone(),
            rest,
            width,
            thickness,
            stiffness: self.dynamics.stiffness,
            shade: narrow(shade),
            simulate: self.dynamics.simulate,
            follow: self.dynamics.follow,
            tint: None,
        }
    }
}

/// How a group's clumps become triangles. Internal to this crate, not an API type.
#[derive(Clone, Debug, PartialEq)]
pub enum GroupGeometry {
    /// A lens tube per clump ([`tube`](super::tube)).
    Tube,
    /// Tubes and the hair ties that gather them (a tail): drawn as [`Tube`](Self::Tube), plus a band per tie in the
    /// part's tie mesh ([`tie`](super::tie)).
    Tied {
        /// The bands, one per gather.
        ties: Vec<Tie>,
    },
    /// The curtain: each clump owns `columns` columns of one wide strip, ordered across the head by `u` (one per clump).
    /// The strips are meshed by `curtain::group_mesh`; the runtime (#322) refills them.
    Strip {
        /// Columns across one guide.
        columns: u8,
        /// The clump's position across the curtain.
        u: Vec<f32>,
    },
    /// The cap: a finished shell mesh over the skull carrying `hair-uv` and `hair-info`, with no clumps of its own.
    Shell(MeshData),
}

impl GroupGeometry {
    /// Whether the group's clumps are drawn as tubes.
    pub fn is_tube(&self) -> bool {
        matches!(self, Self::Tube | Self::Tied { .. })
    }
}

/// What a generator makes: clumps and how they are drawn.
#[derive(Clone, Debug, PartialEq)]
pub struct CompiledGroup {
    /// The clumps, root first, in the generator's order.
    pub clumps: Vec<HairClump>,
    /// How they become triangles.
    pub geometry: GroupGeometry,
}

impl CompiledGroup {
    /// A group of tubes.
    pub fn tubes(clumps: Vec<HairClump>) -> Self {
        Self { clumps, geometry: GroupGeometry::Tube }
    }

    /// A group with nothing in it yet, drawn as `geometry`: what a generator that is not written returns.
    pub fn empty(geometry: GroupGeometry) -> Self {
        Self { clumps: Vec::new(), geometry }
    }

    /// The group followed by its mirror image across the plane `x = mid_x`: clumps (and a strip's `u`, a tail's ties and a shell's
    /// vertices) are reflected, and the winding of a shell is reversed so it still faces outward. No random numbers are
    /// drawn: the mirror is exact.
    #[must_use]
    pub fn with_mirror(mut self, mid_x: f32) -> Self {
        let flip = |p: Vec3| Vec3::new(2.0 * mid_x - p.x, p.y, p.z);
        let mirrored: Vec<HairClump> = self
            .clumps
            .iter()
            .map(|c| HairClump { rest: c.rest.iter().copied().map(flip).collect(), ..c.clone() })
            .collect();
        self.clumps.extend(mirrored);
        match &mut self.geometry {
            GroupGeometry::Tube => {}
            GroupGeometry::Tied { ties } => {
                let reflected: Vec<Tie> = ties.iter().map(|t| t.mirrored(mid_x)).collect();
                ties.extend(reflected);
            }
            GroupGeometry::Strip { u, .. } => {
                let reflected: Vec<f32> = u.iter().map(|u| -u).collect();
                u.extend(reflected);
            }
            GroupGeometry::Shell(shell) => mirror_shell(shell, mid_x),
        }
        self
    }
}

/// Appends the mirror image of `shell` to it, reversing each mirrored triangle's winding.
fn mirror_shell(shell: &mut MeshData, mid_x: f32) {
    let n = u32::try_from(shell.positions.len()).unwrap_or(0);
    let reflected: Vec<Vec3> = shell.positions.iter().map(|p| Vec3::new(2.0 * mid_x - p.x, p.y, p.z)).collect();
    let normals: Vec<Vec3> = shell.normals.iter().map(|v| Vec3::new(-v.x, v.y, v.z)).collect();
    let indices: Vec<u32> =
        shell.indices.as_chunks::<3>().0.iter().flat_map(|t| [t[0] + n, t[2] + n, t[1] + n]).collect();
    shell.positions.extend(reflected);
    shell.normals.extend(normals);
    let uvs = shell.uvs.clone();
    shell.uvs.extend(uvs);
    shell.indices.extend(indices);
    for attribute in &mut shell.extra {
        let copy = attribute.data.clone();
        attribute.data.extend(copy);
    }
    shell.colours.clear();
}

/// Where the generators are looked up: [`Dispatch`] in the part, a stub in the pipeline tests.
pub trait GroupGenerator {
    /// Compiles one group's generator.
    fn compile(&self, generator: &Generator, ctx: &mut HairGenCtx<'_>) -> CompiledGroup;

    /// How many clumps [`compile`](Self::compile) makes for `generator` (before mirroring), from the parameters alone.
    fn clump_count(&self, generator: &Generator) -> usize;
}

/// The real generators, one `match` arm each.
pub struct Dispatch;

impl GroupGenerator for Dispatch {
    fn compile(&self, generator: &Generator, ctx: &mut HairGenCtx<'_>) -> CompiledGroup {
        match generator {
            Generator::Cap(g) => r#gen::cap::compile(g, ctx),
            Generator::PartSweep(g) => r#gen::part_sweep::compile(g, ctx),
            Generator::Framing(g) => r#gen::framing::compile(g, ctx),
            Generator::Braid(g) => r#gen::braid::compile(g, ctx),
            Generator::Curtain(g) => r#gen::curtain::compile(g, ctx),
            Generator::Bangs(g) => r#gen::bangs::compile(g, ctx),
            Generator::Tail(g) => r#gen::tail::compile(g, ctx),
            Generator::Bun(g) => r#gen::bun::compile(g, ctx),
            Generator::Ahoge(g) => r#gen::ahoge::compile(g, ctx),
            Generator::Guides(g) => r#gen::guides::compile(g, ctx),
        }
    }

    fn clump_count(&self, generator: &Generator) -> usize {
        match generator {
            Generator::Cap(g) => r#gen::cap::clump_count(g),
            Generator::PartSweep(g) => r#gen::part_sweep::clump_count(g),
            Generator::Framing(g) => r#gen::framing::clump_count(g),
            Generator::Braid(g) => r#gen::braid::clump_count(g),
            Generator::Curtain(g) => r#gen::curtain::clump_count(g),
            Generator::Bangs(g) => r#gen::bangs::clump_count(g),
            Generator::Tail(g) => r#gen::tail::clump_count(g),
            Generator::Bun(g) => r#gen::bun::clump_count(g),
            Generator::Ahoge(g) => r#gen::ahoge::clump_count(g),
            Generator::Guides(g) => r#gen::guides::clump_count(g),
        }
    }
}

/// One enabled group, compiled.
#[derive(Clone, Debug)]
pub struct CompiledEntry {
    /// The group's id.
    pub id: GroupId,
    /// Whether the group is simulated (decides which mesh it lands in).
    pub simulate: bool,
    /// Its clumps and geometry.
    pub group: CompiledGroup,
    /// How its locks are shaped ([`crate::hair::lock`]).
    pub lock: LockShape,
}

/// Why compiling failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CompileError {
    /// A generator made a different number of clumps than its `clump_count` said, which would make a recolour tint the
    /// wrong strands.
    #[error("group '{group}': the generator made {made} clumps but reports {reported}")]
    ClumpCount {
        /// The group.
        group: GroupId,
        /// What `compile` made (before mirroring).
        made: usize,
        /// What `clump_count` said.
        reported: usize,
    },
}

/// Compiles every enabled group of `params`, in id order, mirroring those that ask (except a bilateral generator's, see [`is_bilateral`]). `mid_x` is the plane of symmetry.
///
/// # Errors
/// [`CompileError::ClumpCount`] if a generator disagrees with its own `clump_count`.
pub fn compile_groups(
    params: &HairParams,
    generators: &dyn GroupGenerator,
    head: &dyn HeadSurface,
    palette: &Palette,
    quality: Quality,
    rng: &mut dyn UnitStream,
) -> Result<Vec<CompiledEntry>, CompileError> {
    compile_groups_over(params, generators, head, None, palette, quality, rng)
}

/// [`compile_groups`] for hair that hangs over `body`.
///
/// # Errors
/// As [`compile_groups`].
pub fn compile_groups_over(
    params: &HairParams,
    generators: &dyn GroupGenerator,
    head: &dyn HeadSurface,
    body: Option<&dyn BodyFit>,
    palette: &Palette,
    quality: Quality,
    rng: &mut dyn UnitStream,
) -> Result<Vec<CompiledEntry>, CompileError> {
    let mid_x = head.centre().x;
    let mut entries = Vec::new();
    for (id, group) in enabled(params) {
        let mut ctx = HairGenCtx::new(head, palette, quality, id, &group.dynamics, rng).with_body(body);
        let mut compiled = generators.compile(&group.generator, &mut ctx);
        let reported = generators.clump_count(&group.generator);
        if compiled.clumps.len() != reported {
            return Err(CompileError::ClumpCount { group: id.clone(), made: compiled.clumps.len(), reported });
        }
        if group.mirror && !is_bilateral(&group.generator) {
            compiled = compiled.with_mirror(mid_x);
        }
        entries.push(CompiledEntry {
            id: id.clone(),
            simulate: group.dynamics.simulate,
            group: compiled,
            lock: group.lock,
        });
    }
    Ok(entries)
}

/// The enabled groups, in the order they build.
pub fn enabled(params: &HairParams) -> impl Iterator<Item = (&GroupId, &HairGroup)> {
    params.groups.iter().filter(|(_, g)| g.enabled)
}

/// Whether a generator makes both sides of the head itself, so its group's `mirror` is ignored: mirroring would add a
/// second pair of the sides it already makes. A framing group is bilateral only at [`HairSide::Both`]. The match has no
/// wildcard: a new generator has to say which kind it is.
pub fn is_bilateral(generator: &Generator) -> bool {
    match generator {
        Generator::Cap(_) | Generator::PartSweep(_) | Generator::Curtain(_) | Generator::Bangs(_) => true,
        Generator::Braid(g) => !r#gen::braid::own_path(g),
        Generator::Framing(g) => g.side == HairSide::Both,
        Generator::Tail(_) | Generator::Bun(_) | Generator::Ahoge(_) | Generator::Guides(_) => false,
    }
}

/// How many clumps an enabled group has once mirrored (a [bilateral](is_bilateral) generator's group is never mirrored).
pub fn clumps_in(group: &HairGroup, generators: &dyn GroupGenerator) -> usize {
    generators.clump_count(&group.generator) * if group.mirror && !is_bilateral(&group.generator) { 2 } else { 1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_legacy_stream_is_the_prototypes() {
        let mut forked = Rng::new(1);
        {
            let mut s = stream(HairRng::LegacyLcg { seed: 7 }, &mut forked).unwrap();
            // seed 7: (7 * 16807 mod 2147483647 - 1) / 2147483646
            assert_eq!(s.next_unit(), (117_649.0 - 1.0) / 2_147_483_646.0);
        }
        assert!(stream(HairRng::LegacyLcg { seed: 0 }, &mut forked).is_err());
    }

    #[test]
    fn a_mirrored_strip_negates_u_and_a_shell_keeps_facing_out() {
        let tri = MeshData {
            positions: vec![Vec3::new(1.0, 0.0, 0.0), Vec3::new(2.0, 0.0, 0.0), Vec3::new(1.0, 1.0, 0.0)],
            normals: vec![Vec3::Z; 3],
            indices: vec![0, 1, 2],
            ..MeshData::default()
        };
        let mirrored = CompiledGroup::empty(GroupGeometry::Shell(tri)).with_mirror(0.0);
        let GroupGeometry::Shell(m) = mirrored.geometry else { panic!("shell") };
        assert_eq!(m.positions[3], Vec3::new(-1.0, 0.0, 0.0));
        let face = |t: &[u32]| {
            let p = |i: u32| m.positions[i as usize];
            (p(t[1]) - p(t[0])).cross(p(t[2]) - p(t[0])).z
        };
        assert!(face(&m.indices[0..3]) > 0.0 && face(&m.indices[3..6]) > 0.0, "both halves face +Z");
        let strip = CompiledGroup::empty(GroupGeometry::Strip { columns: 5, u: vec![0.25] }).with_mirror(0.0);
        // The mirrored half is a second set of clumps; with none, the strip's `u` list just doubles.
        assert_eq!(strip.geometry, GroupGeometry::Strip { columns: 5, u: vec![0.25, -0.25] });
    }
}
