//! VRM 1.0: the `VRMC_vrm` family of glTF extensions. The scene is already shaped like 1.0, so this layer is mostly
//! a faithful, validated translation between the extension JSON and the avatar types. The glTF around it is
//! `omuzaki-gltf`'s; this module plugs into it through [`ImportHooks`] and [`ExportHooks`].

mod mtoon;
mod read;
mod wire;
mod write;

use std::collections::BTreeMap;

use omuzaki_gltf::schema::{self, Document, Empty};
use omuzaki_gltf::{Carrier, Disposition, ExportHooks, ImportHooks, MaterialCx, RootCx, Tally, TargetNames, Unusable};
use omuzaki_io_api::{IoError, IoScene, Material, Node, Report, Severity, Shading};
use serde_json::Value;

/// The VRM 1.0 root extension.
pub(crate) const EXTENSION: &str = "VRMC_vrm";
/// Spring bones and colliders.
pub(crate) const SPRING_BONE: &str = "VRMC_springBone";
/// Node constraints (on nodes).
pub(crate) const CONSTRAINT: &str = "VRMC_node_constraint";

/// Reading a VRM 1.0 file: interprets the `VRMC_*` extensions and collects the findings that are the VRM layer's.
#[derive(Default)]
pub(crate) struct Import {
    /// Findings of the avatar layer, to be merged after the generic ones.
    pub(crate) report: Report,
}

impl ImportHooks for Import {
    fn supported_extensions(&self) -> &[&str] {
        &[EXTENSION, SPRING_BONE, CONSTRAINT, mtoon::MTOON]
    }

    fn extension(&mut self, carrier: Carrier, name: &str, _body: &Value) -> Disposition {
        match (carrier, name) {
            (Carrier::Root, EXTENSION | SPRING_BONE)
            | (Carrier::Node, CONSTRAINT)
            | (Carrier::Material, mtoon::MTOON) => Disposition::Interpreted,
            (Carrier::Root, "VRM") => Disposition::Discard {
                severity: Severity::Info,
                message: "the VRM 0.x extension was ignored: this file is read as VRM 1.0".to_owned(),
                hint: Some("import the file as vrm0 to read the 0.x data"),
            },
            _ => Disposition::Carry,
        }
    }

    /// Humanoid, expressions and the thumbnail refer to nodes, materials and images by file index, so none may move.
    fn unusable(&self) -> Unusable {
        Unusable::Refuse
    }

    fn material(&mut self, cx: &mut MaterialCx<'_>) -> Result<(), IoError> {
        // MToon takes precedence over `KHR_materials_unlit` (`VRMC_materials_mtoon-1.0/README.md`, "Dependencies").
        if let Some(body) = cx.def.extensions.others.get(mtoon::MTOON) {
            mtoon::read(cx, body, &mut self.report);
        }
        Ok(())
    }

    fn root(&mut self, cx: &mut RootCx<'_>) -> Result<(), IoError> {
        read::read(cx.document, cx.scene, &mut self.report)
    }
}

/// Writing a VRM 1.0 file: adds the `VRMC_*` extensions to the generic document.
pub(crate) struct Export {
    /// Findings of the avatar layer, to be merged after the generic ones.
    pub(crate) report: Report,
    constraints: BTreeMap<usize, Value>,
}

impl Export {
    /// Hooks for exporting `scene`.
    pub(crate) fn new(scene: &IoScene) -> Self {
        let mut report = Report::new();
        let constraints = scene.avatar.as_ref().map(|av| write::constraint_bodies(av, &mut report)).unwrap_or_default();
        Self { report, constraints }
    }
}

impl ExportHooks for Export {
    fn target_names(&self) -> TargetNames {
        TargetNames::OnMesh
    }

    fn node(&mut self, index: usize, _node: &Node, def: &mut schema::Node) {
        if let Some(body) = self.constraints.remove(&index) {
            def.extensions.insert(CONSTRAINT.to_owned(), body);
        }
    }

    /// A toon material arrives as the matte PBR stand-in; it gets the `MToon` body plus `KHR_materials_unlit`, the
    /// fallback the `MToon` spec suggests for viewers without an `MToon` shader.
    fn material(&mut self, index: usize, material: &Material, def: &mut schema::Material, tally: &mut Tally) {
        if let Shading::Toon(toon) = &material.shading {
            let at = format!("materials/{index}");
            let body = mtoon::write(toon, material.alpha, &at, tally);
            def.extensions.unlit = Some(Empty::default());
            def.extensions.others.insert(mtoon::MTOON.to_owned(), body);
        }
    }

    fn root(&mut self, scene: &IoScene, document: &mut Document, _tally: &mut Tally) {
        for (name, body) in write::root_extensions(scene, &mut self.report) {
            document.extensions.insert(name.to_owned(), body);
        }
    }
}
