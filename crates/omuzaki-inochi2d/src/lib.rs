//! `Inochi2D` puppets (spec v3 EX-08, ADR 0010): the INP1 file (`.inp`, and Inochi Creator's `.inx`) mapped onto
//! [`omuzaki_puppet_api::PuppetDef`], with an [`ImportReport`] of what was mapped, made up and left behind; the INP2
//! container; and an INP1 writer for our own puppets.
//!
//! ```no_run
//! # fn demo(bytes: &[u8]) -> Result<(), omuzaki_inochi2d::ImportError> {
//! let imported = omuzaki_inochi2d::import(bytes, &omuzaki_inochi2d::Limits::default())?;
//! for finding in &imported.report.findings {
//!     println!("{} at {}: {}", finding.id, finding.path, finding.detail);
//! }
//! # Ok(()) }
//! ```
//!
//! Files come from the internet, so hostile input is the default: every declared length and count is checked against
//! the bytes left and against [`Limits`] before anything is allocated for it, nothing panics, and a refused file names
//! its rule (`inochi2d.<rule>`, see [`rule`]) and where (a byte offset, a section, or a JSON path such as
//! `nodes/children/2/mesh/indices/7`).
//!
//! This is our own code, written from the format as `Inochi2D`'s sources and docs describe it (the research note,
//! `docs/research/2026-10-inochi2d-format.md`, lists them with their commits). No code or documentation text is copied.
//!
//! # Conventions, and where they come from
//!
//! Sources are `Inochi2D` 0.8.7 (`inochi2d`, BSD-2-Clause) unless named; paths are under its `source/inochi2d/`.
//!
//! - **Axes.** `Inochi2D`'s model space is y-down pixels, the same as [`omuzaki_puppet_api`]'s: its camera is
//!   `mat4.orthographic(0, w, h, 0, ..)` (`math/camera.d:68`), and a part made from a texture puts its top-left vertex,
//!   `(-w/2, -h/2)`, at uv `(0, 0)` (`core/nodes/part/package.d:159-170`). Positions and uvs pass through unchanged.
//!   Positive angles turn clockwise on screen in both.
//! - **Transforms.** A node's transform is `T * R * S` with `R` the Euler rotation of `rot = [x, y, z]`
//!   (`math/transform.d:94-99`; 0.9 writes the order out as `T * Rz * Ry * Rx * S` in `core/math/transform.d`).
//!   Points have z = 0, so the plane part is `Rz * [[cos y, sin x sin y], [0, cos x]] * S`: with one of the x and y
//!   angles zero it is exactly `Rz * diag(cos y, cos x) * S`, and the x and y angles fold into the scale. Both at once
//!   leave a shear we drop, reported as `inochi2d.rotation_xy`. The z translation is dropped (`inochi2d.translate_z`).
//!   A node's global transform is its parent's times its own (`core/nodes/package.d:360-388`), as ours is.
//! - **Draw order.** `Inochi2D` sorts its drawables by total z (a node's `zsort` plus its parents',
//!   `core/nodes/package.d:273-289`) descending, stably (`core/puppet.d:293-297, 505-512`): a larger z draws further
//!   back, as [`omuzaki_puppet_api::Node::zsort`] says. `zsort` passes through.
//! - **Part vertices.** A part draws its vertices at `vertex - origin + deform`, in its node's space
//!   (`shaders/basic/basic.vert`, with `offset = data.origin`); the importer subtracts `origin` once, so our vertices are
//!   the drawn positions. A mesh group's `origin` takes no part in its deformation (`core/nodes/meshgroup/package.d`),
//!   so its vertices are kept as they are. Mesh data is `verts`, `uvs`, `indices`, `origin`, flat
//!   (`core/meshdata.d:155-188`); `Inochi2D` draws a mesh only when its indices are whole triangles.
//! - **Textures.** Slot ids index the file's texture section; a part's first id that isn't `uint.max` is its albedo,
//!   the next ones emission and bump maps (`core/nodes/part/package.d:481-493`), which we drop. Pixels are stored
//!   premultiplied and kept as stored (see [`Texture`]).
//! - **Masks.** A mask names its source by uuid with mode `Mask` or `DodgeMask` (`core/nodes/common.d`,
//!   `core/nodes/part/package.d:514-524` also reads the older `masked_by` list). `Inochi2D` drops a mask whose source
//!   is missing; we refuse the file (`inochi2d.mask_source`), since it is broken. A source our model can't mask with (a
//!   composite, a mesh group, itself) is dropped with a finding.
//! - **Parameters.** Axis points are stored normalised, `0..1` across `min..max` (`core/param/package.d`); a key at
//!   `t` is at `min + t * (max - min)` in the parameter's units. Missing `min`, `max` and `axis_points` default to
//!   `[0, 0]`, `[1, 1]` and `[[0, 1], [0, 1]]`. A one-axis parameter reads `values[x][0]` only.
//! - **Bindings.** `values[x][y]` and `isSet[x][y]` (`core/param/binding.d`) become our x-major lists, `x * ny + y`
//!   ([`omuzaki_puppet_api::Param::key_index`]). A binding to a missing node is dropped, as `Inochi2D`'s finalize drops it.
//!   Targets: `zSort`, `transform.{t,r,s}.{x,y,z}`, `deform`, `opacity`, `tint.{r,g,b}`; the rest (`screenTint.*`,
//!   `emissionStrength`, `alphaThreshold`, physics settings, `transform.t.z`) have no target in the contract and are
//!   dropped with `inochi2d.binding_target`. Interpolation: Nearest, Linear and Cubic map; Stepped reads as Nearest
//!   and Bezier as Cubic (`core/package.d:642-670`).
//! - **Physics.** A `SimplePhysics` node becomes a group and a [`omuzaki_puppet_api::Pendulum`] anchored on it, with
//!   `Inochi2D`'s defaults for missing fields (gravity 1, length 100, frequency 1, damping 0.5 and 0.5, output scale
//!   `[1, 1]`; `core/nodes/drivers/simplephysics.d`). The puppet's `physics` (pixels per metre, gravity) becomes
//!   [`omuzaki_puppet_api::PhysicsWorld`].
//! - **Node types.** `Node`, `Part`, `Composite`, `MeshGroup` and `SimplePhysics` (0.8), and `Mask` (0.7, a mesh drawn
//!   only into the stencil). Any other type is kept as [`omuzaki_puppet_api::NodeKind::Unknown`] with
//!   `inochi2d.unknown_node`; `Inochi2D` itself skips such a node and its subtree (`core/nodes/package.d:816-829`).
//!   `lockToRoot` (follow the root, not the parents) has no field in the contract and is reported.
//! - **Metadata.** Kept verbatim, with `source = "inochi2d"`; rights are copied key by key as text, and when the file
//!   states none, none are assumed (`inochi2d.rights_unknown`).
//! - **Canvas.** `Inochi2D` has none; ours is the smallest box centred on the root that holds every part at rest.
//! - **INP2.** The container (`Inochi2D` 0.9, `tech-docs/inp2.md`) is read and checked, but its payload's schema is not
//!   settled upstream (0.9's own writer is unimplemented), so [`import`] refuses it with `inochi2d.inp2_payload`;
//!   [`read_container`] still returns its tree, textures and vendor data.
#![deny(missing_docs)]

mod container;
mod convert;
mod error;
mod inp2;
mod payload;
mod report;
mod texture;
mod writer;

pub use container::{
    Container, EXT_SECT, Format, INP_SECT, INP1_MAGIC, INP2_MAGIC, Payload, RawTexture, TEX_SECT, TextureEncoding,
    UnknownSection, VendorData, detect, read_container,
};
pub use error::{ImportError, Limits, WriteError};
pub use inp2::DataNode;
pub use report::{Finding, FindingKind, ImportReport, Mapped, rule};
pub use texture::Texture;
pub use writer::{MAX_WRITE_DEPTH, write_inp1};

use omuzaki_puppet_api::{PuppetDef, TextureRef};

/// An imported puppet.
#[derive(Clone, Debug, PartialEq)]
pub struct Imported<'a> {
    /// The puppet, checked ([`PuppetDef::check`] passed). Texture `i` is named `textures/<i>.png`.
    pub def: PuppetDef,
    /// The decoded textures, in [`PuppetDef::textures`] order.
    pub textures: Vec<Texture>,
    /// What was mapped, synthesised, dropped and noted.
    pub report: ImportReport,
    /// The file's vendor data, kept uninterpreted.
    pub vendor: Vec<VendorData<'a>>,
    /// Sections this reader doesn't know, kept as they are.
    pub unknown: Vec<UnknownSection<'a>>,
}

/// Imports an `Inochi2D` puppet.
///
/// # Errors
/// [`ImportError::Malformed`] for bytes out of shape, [`ImportError::Limit`] for declared sizes past `limits`, and
/// [`ImportError::Unsupported`] for an INP2 payload. Each names its rule and where.
pub fn import<'a>(bytes: &'a [u8], limits: &Limits) -> Result<Imported<'a>, ImportError> {
    let container = read_container(bytes, limits)?;
    let Payload::Json(json) = container.payload else {
        return Err(ImportError::Unsupported {
            rule: rule::INP2_PAYLOAD,
            detail: "an INP2 (Inochi2D 0.9) file: its container reads, but its puppet schema is not mapped yet".into(),
        });
    };
    let mut report = ImportReport::default();
    if !container.has_texture_section {
        report.add(rule::NO_TEXTURES, FindingKind::Note, "TEX_SECT", "the file has no texture section");
    }
    for (i, v) in container.vendor.iter().enumerate() {
        report.add(
            rule::VENDOR_DATA,
            FindingKind::Note,
            format!("EXT_SECT/{i}"),
            format!("{:?}: {} bytes, kept uninterpreted", v.name, v.data.len()),
        );
    }
    for u in &container.unknown {
        let tag = u.tag.map_or_else(|| "untagged".to_owned(), |t| format!("{:?}", String::from_utf8_lossy(&t)));
        report.add(
            rule::UNKNOWN_SECTION,
            FindingKind::Note,
            format!("@{}", u.offset),
            format!("{tag}: {} bytes, kept uninterpreted", u.data.len()),
        );
    }

    let mut def = convert::convert(json, container.textures.len(), limits, &mut report)?;
    let textures = texture::decode_all(&container.textures, limits, &mut report)?;
    def.textures = textures
        .iter()
        .enumerate()
        .map(|(i, t)| TextureRef { file: format!("textures/{i}.png"), size: t.size })
        .collect();
    def.canvas = convert::canvas(&def, &mut report);
    report.mapped = convert::mapped(&def);
    def.check().map_err(|e| ImportError::Malformed {
        rule: rule::CHECK,
        path: e.path,
        detail: format!("{}: {}", e.rule.id(), e.detail),
    })?;
    Ok(Imported { def, textures, report, vendor: container.vendor, unknown: container.unknown })
}
