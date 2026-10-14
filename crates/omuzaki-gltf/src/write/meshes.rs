//! Meshes: primitives packed into accessors.

use omuzaki_io_api::{ExportCtx, IoError, IoScene, MaterialIx, Mesh, Primitive, Severity};

use super::Writer;
use crate::hooks::TargetNames;
use crate::pack::Packer;
use crate::schema::{self as gltf, code};
use crate::tally::Tally;

impl Writer<'_> {
    pub(super) fn meshes(&mut self, ctx: &ExportCtx<'_>) -> Result<(), IoError> {
        let count = self.scene.meshes.len();
        for (i, mesh) in self.scene.meshes.iter().enumerate() {
            ctx.progress.checkpoint("write meshes", 0.1 + 0.7 * crate::fraction(i, count))?;
            let mut def = self.mesh(mesh);
            self.hooks.mesh(i, mesh, &mut def);
            self.out.document.meshes.push(def);
        }
        Ok(())
    }

    fn mesh(&mut self, mesh: &Mesh) -> gltf::Mesh {
        let names = (!mesh.morph_target_names.is_empty())
            .then(|| serde_json::json!({ "targetNames": mesh.morph_target_names }));
        let pack = &mut self.out.pack;
        let mut primitives: Vec<gltf::Primitive> = mesh.primitives.iter().map(|p| primitive(pack, p)).collect();
        // VRM 1.0 and Blender put the names on the mesh; VRM 0.x exporters put them on each primitive.
        let extras = match self.hooks.target_names() {
            TargetNames::OnMesh => names,
            TargetNames::OnPrimitives => {
                for p in &mut primitives {
                    p.extras.clone_from(&names);
                }
                None
            }
        };
        gltf::Mesh { name: mesh.name.clone(), primitives, extras, ..gltf::Mesh::default() }
    }
}

fn primitive(pack: &mut Packer, p: &Primitive) -> gltf::Primitive {
    let m = &p.mesh;
    let n = m.positions.len();
    let mut def = gltf::Primitive {
        material: p.material.map(MaterialIx::index),
        mode: Some(code::TRIANGLES),
        ..gltf::Primitive::default()
    };
    let attributes = &mut def.attributes;
    attributes.insert("POSITION".into(), pack.vec3(&m.positions, true));
    attributes.insert("NORMAL".into(), pack.vec3(&m.normals, false));
    if !m.uvs.is_empty() {
        attributes.insert("TEXCOORD_0".into(), pack.vec2(&m.uvs));
    }
    if !m.colours.is_empty() {
        attributes.insert("COLOR_0".into(), pack.vec4(&m.colours));
    }
    if !m.joints.is_empty() {
        attributes.insert("JOINTS_0".into(), pack.joints(&m.joints));
        attributes.insert("WEIGHTS_0".into(), pack.vec4(&m.weights));
    }
    for extra in &m.extra {
        let name = format!("_{}", extra.name.as_str().to_ascii_uppercase());
        attributes.insert(name, pack.extra(extra));
    }
    def.indices = Some(pack.indices(&m.indices, n));
    for target in &p.morph_targets {
        let mut t = std::collections::BTreeMap::new();
        t.insert("POSITION".to_owned(), pack.deltas(&target.positions, true));
        if !target.normals.is_empty() {
            t.insert("NORMAL".to_owned(), pack.deltas(&target.normals, false));
        }
        def.targets.push(t);
    }
    def
}

/// Empty meshes and skins aren't valid glTF (a mesh needs a primitive, a skin a joint).
pub(super) fn check(scene: &IoScene, tally: &mut Tally) {
    for (i, mesh) in scene.meshes.iter().enumerate() {
        if mesh.primitives.is_empty() {
            let msg = "a mesh with no primitives can't be written: glTF needs at least one";
            tally.note(
                Severity::Error,
                "gltf.mesh_empty",
                msg,
                format!("meshes/{i}"),
                Some("remove the mesh, or give it geometry"),
            );
        }
    }
    for (m, mesh) in scene.meshes.iter().enumerate() {
        for (p, prim) in mesh.primitives.iter().enumerate() {
            for (t, target) in prim.morph_targets.iter().enumerate() {
                if !target.positions.iter().chain(&target.normals).all(|v| v.is_finite()) {
                    let msg = "a morph target has NaN or infinite deltas, which would corrupt the accessor bounds";
                    let at = format!("meshes/{m}/primitives/{p}/targets/{t}");
                    tally.note(Severity::Error, "gltf.non_finite", msg, at, None);
                }
            }
        }
    }
    for (i, skin) in scene.skins.iter().enumerate() {
        if skin.joints.is_empty() {
            let msg = "a skin with no joints can't be written: glTF needs at least one";
            tally.note(Severity::Error, "gltf.skin_empty", msg, format!("skins/{i}"), None);
        }
    }
}
