//! The checks every puppet passes before a runtime takes it, so evaluation never indexes out of range or meets a NaN.
//! Importers and builders call [`PuppetDef::check`]; puppets come from the internet, so nothing is assumed.

use crate::model::{NodeKind, PuppetDef};
use crate::param::{BindTarget, BindValues};

/// Which rule a puppet broke (stable ids for reports and tests).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Rule {
    /// The tree: one root at index 0, every parent before its child.
    Tree,
    /// A mesh: lengths agree, indices in range, finite numbers.
    Mesh,
    /// A texture index or size.
    Texture,
    /// A lattice's shape.
    Lattice,
    /// A mask source.
    Mask,
    /// A parameter's range or keys.
    Param,
    /// A binding's node, target or values.
    Binding,
    /// A pendulum.
    Physics,
}

impl Rule {
    /// The rule's id, `puppet.<rule>`.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Tree => "puppet.tree",
            Self::Mesh => "puppet.mesh",
            Self::Texture => "puppet.texture",
            Self::Lattice => "puppet.lattice",
            Self::Mask => "puppet.mask",
            Self::Param => "puppet.param",
            Self::Binding => "puppet.binding",
            Self::Physics => "puppet.physics",
        }
    }
}

/// A broken rule, with where.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{} at {path}: {detail}", rule.id())]
pub struct PuppetError {
    /// The rule.
    pub rule: Rule,
    /// Where (`nodes/3/mesh/triangles/7`).
    pub path: String,
    /// What is wrong.
    pub detail: String,
}

fn fail(rule: Rule, path: impl Into<String>, detail: impl Into<String>) -> PuppetError {
    PuppetError { rule, path: path.into(), detail: detail.into() }
}

fn finite2(v: [f32; 2]) -> bool {
    v[0].is_finite() && v[1].is_finite()
}

fn check_mesh(mesh: &crate::model::Mesh, path: &str, textured: bool) -> Result<(), PuppetError> {
    let n = mesh.vertices.len();
    if let Some(i) = mesh.vertices.iter().position(|v| !finite2(*v)) {
        return Err(fail(Rule::Mesh, format!("{path}/vertices/{i}"), "not a finite number"));
    }
    if textured && mesh.uvs.len() != n {
        return Err(fail(Rule::Mesh, format!("{path}/uvs"), format!("{} uvs for {n} vertices", mesh.uvs.len())));
    }
    if let Some(i) = mesh.uvs.iter().position(|v| !finite2(*v)) {
        return Err(fail(Rule::Mesh, format!("{path}/uvs/{i}"), "not a finite number"));
    }
    for (i, t) in mesh.triangles.iter().enumerate() {
        if t.iter().any(|&k| k as usize >= n) {
            return Err(fail(Rule::Mesh, format!("{path}/triangles/{i}"), format!("an index past the {n} vertices")));
        }
    }
    Ok(())
}

impl PuppetDef {
    /// Checks every rule.
    ///
    /// # Errors
    /// The first rule broken, with its path.
    pub fn check(&self) -> Result<(), PuppetError> {
        if !finite2(self.canvas) || self.canvas[0] <= 0.0 || self.canvas[1] <= 0.0 {
            return Err(fail(Rule::Tree, "canvas", "the canvas needs a positive, finite size"));
        }
        for (i, t) in self.textures.iter().enumerate() {
            if t.size[0] == 0 || t.size[1] == 0 {
                return Err(fail(Rule::Texture, format!("textures/{i}"), "an empty texture"));
            }
        }
        if self.nodes.first().is_none_or(|root| root.parent.is_some()) {
            return Err(fail(Rule::Tree, "nodes/0", "the first node is the root, with no parent"));
        }
        for (i, node) in self.nodes.iter().enumerate() {
            let path = format!("nodes/{i}");
            if i > 0 && node.parent.is_none_or(|p| p.get() >= i) {
                return Err(fail(Rule::Tree, path, "every node but the root has a parent listed before it"));
            }
            let tr = &node.transform;
            if !finite2(tr.translation) || !finite2(tr.scale) || !tr.rotation.is_finite() || !node.zsort.is_finite() {
                return Err(fail(Rule::Tree, format!("{path}/transform"), "not a finite number"));
            }
            let drawable = match &node.kind {
                NodeKind::Part(part) => {
                    check_mesh(&part.mesh, &format!("{path}/mesh"), true)?;
                    if part.texture.get() >= self.textures.len() {
                        return Err(fail(Rule::Texture, format!("{path}/texture"), "no such texture"));
                    }
                    Some(&part.draw)
                }
                NodeKind::Composite(c) => Some(&c.draw),
                NodeKind::Mask(mesh) | NodeKind::MeshDeformer(mesh) => {
                    check_mesh(mesh, &format!("{path}/mesh"), false)?;
                    None
                }
                NodeKind::Lattice(l) => {
                    if l.cols < 2 || l.rows < 2 || l.cols > 64 || l.rows > 64 {
                        return Err(fail(Rule::Lattice, path, "a lattice is 2 to 64 points each way"));
                    }
                    if !finite2(l.origin) || !finite2(l.size) || l.size[0] <= 0.0 || l.size[1] <= 0.0 {
                        return Err(fail(Rule::Lattice, path, "a lattice needs a positive, finite extent"));
                    }
                    None
                }
                NodeKind::Group | NodeKind::Unknown { .. } => None,
            };
            if let Some(draw) = drawable {
                for (m, mask) in draw.masks.iter().enumerate() {
                    let src = mask.source.get();
                    let ok = src != i
                        && self.nodes.get(src).is_some_and(|n| matches!(n.kind, NodeKind::Part(_) | NodeKind::Mask(_)));
                    if !ok {
                        return Err(fail(
                            Rule::Mask,
                            format!("{path}/masks/{m}"),
                            "a mask source is another part or a mask mesh",
                        ));
                    }
                }
                if !draw.opacity.is_finite() || !draw.mask_threshold.is_finite() {
                    return Err(fail(Rule::Tree, path, "not a finite number"));
                }
            }
        }
        for (p, param) in self.params.iter().enumerate() {
            self.check_param(p, param)?;
        }
        for (i, pendulum) in self.physics.iter().enumerate() {
            let path = format!("physics/{i}");
            if pendulum.anchor.get() >= self.nodes.len() || pendulum.param.get() >= self.params.len() {
                return Err(fail(Rule::Physics, path, "an anchor or parameter that doesn't exist"));
            }
            let numbers = [pendulum.length, pendulum.frequency, pendulum.gravity, pendulum.angle_damping];
            if numbers.iter().any(|v| !v.is_finite()) || pendulum.length <= 0.0 || pendulum.frequency <= 0.0 {
                return Err(fail(Rule::Physics, path, "a pendulum needs a positive length and frequency"));
            }
        }
        if !(self.world.pixels_per_meter.is_finite()
            && self.world.pixels_per_meter > 0.0
            && self.world.gravity.is_finite())
        {
            return Err(fail(Rule::Physics, "world", "a positive scale and a finite gravity"));
        }
        Ok(())
    }

    fn check_param(&self, p: usize, param: &crate::param::Param) -> Result<(), PuppetError> {
        let path = format!("params/{p}");
        for axis in 0..2 {
            let keys = &param.keys[axis];
            if keys.is_empty() || keys.iter().any(|k| !k.is_finite()) || keys.windows(2).any(|w| w[0] >= w[1]) {
                return Err(fail(Rule::Param, format!("{path}/keys/{axis}"), "keys are finite and strictly ascending"));
            }
            if !(param.min[axis].is_finite() && param.max[axis].is_finite() && param.min[axis] <= param.max[axis]) {
                return Err(fail(Rule::Param, path, "min and max are finite, min at most max"));
            }
        }
        if !param.two_axes && param.keys[1].len() != 1 {
            return Err(fail(Rule::Param, format!("{path}/keys/1"), "a one-axis parameter has one y key"));
        }
        let points = param.key_points();
        for (b, binding) in param.bindings.iter().enumerate() {
            let path = format!("{path}/bindings/{b}");
            let Some(node) = self.nodes.get(binding.node.get()) else {
                return Err(fail(Rule::Binding, path, "no such node"));
            };
            if binding.values.len() != points || (!binding.set.is_empty() && binding.set.len() != points) {
                return Err(fail(
                    Rule::Binding,
                    path,
                    format!("{} values for {points} key points", binding.values.len()),
                ));
            }
            match (&binding.values, binding.target) {
                (BindValues::Offsets(sets), BindTarget::Deform) => {
                    let Some(n) = node.kind.deform_points() else {
                        return Err(fail(Rule::Binding, path, "a deform binding on a node with no points"));
                    };
                    for (k, set) in sets.iter().enumerate() {
                        if set.len() != n || set.iter().any(|v| !finite2(*v)) {
                            return Err(fail(
                                Rule::Binding,
                                format!("{path}/values/{k}"),
                                format!("{} offsets for {n} points, all finite", set.len()),
                            ));
                        }
                    }
                }
                (BindValues::Scalar(values), target) if target != BindTarget::Deform => {
                    if values.iter().any(|v| !v.is_finite()) {
                        return Err(fail(Rule::Binding, path, "not a finite number"));
                    }
                }
                _ => return Err(fail(Rule::Binding, path, "deform takes offsets; every other target takes numbers")),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Drawable, Mesh, Node, NodeIx, Part, TexIx, TextureRef, Transform2};
    use crate::param::{Binding, Interp, Param};

    fn quad() -> PuppetDef {
        let mesh = Mesh {
            vertices: vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]],
            uvs: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            triangles: vec![[0, 1, 2], [0, 2, 3]],
        };
        let node = |name: &str, parent, kind| Node {
            name: name.into(),
            parent,
            enabled: true,
            transform: Transform2::default(),
            zsort: 0.0,
            kind,
        };
        PuppetDef {
            canvas: [10.0, 10.0],
            textures: vec![TextureRef { file: "t.png".into(), size: [4, 4] }],
            nodes: vec![
                node("root", None, NodeKind::Group),
                node(
                    "quad",
                    Some(NodeIx(0)),
                    NodeKind::Part(Part { mesh, texture: TexIx(0), draw: Drawable::default() }),
                ),
            ],
            params: vec![Param {
                name: "p".into(),
                two_axes: false,
                min: [0.0, 0.0],
                max: [1.0, 0.0],
                default: [0.0, 0.0],
                keys: [vec![0.0, 1.0], vec![0.0]],
                bindings: vec![Binding {
                    node: NodeIx(1),
                    target: BindTarget::Deform,
                    interp: Interp::Linear,
                    set: Vec::new(),
                    values: BindValues::Offsets(vec![vec![[0.0; 2]; 4], vec![[1.0, 0.0]; 4]]),
                }],
            }],
            ..PuppetDef::default()
        }
    }

    #[test]
    fn a_good_puppet_passes() {
        assert_eq!(quad().check(), Ok(()));
    }

    #[test]
    fn each_broken_rule_names_itself_and_where() {
        let mut p = quad();
        if let NodeKind::Part(part) = &mut p.nodes[1].kind {
            part.mesh.triangles.push([0, 1, 9]);
        }
        let e = p.check().err();
        assert_eq!(e.as_ref().map(|e| (e.rule, e.path.as_str())), Some((Rule::Mesh, "nodes/1/mesh/triangles/2")));

        let mut p = quad();
        p.nodes[1].parent = Some(NodeIx(1));
        assert_eq!(p.check().err().map(|e| e.rule), Some(Rule::Tree), "a node that is its own parent");

        let mut p = quad();
        if let BindValues::Offsets(sets) = &mut p.params[0].bindings[0].values {
            sets[1].pop();
        }
        assert_eq!(p.check().err().map(|e| e.path), Some("params/0/bindings/0/values/1".into()));

        let mut p = quad();
        p.params[0].keys[0] = vec![1.0, 0.0];
        assert_eq!(p.check().err().map(|e| e.rule), Some(Rule::Param), "keys out of order");

        let mut p = quad();
        if let NodeKind::Part(part) = &mut p.nodes[1].kind {
            part.draw.masks.push(crate::model::MaskRef { source: NodeIx(1), mode: crate::model::MaskMode::Mask });
        }
        assert_eq!(p.check().err().map(|e| e.rule), Some(Rule::Mask), "a part that masks itself");
    }

    #[test]
    fn the_model_round_trips_through_json() {
        let p = quad();
        let json = serde_json::to_string(&p).unwrap_or_default();
        let back: Result<PuppetDef, _> = serde_json::from_str(&json);
        assert_eq!(back.ok(), Some(p));
    }
}
