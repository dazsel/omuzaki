//! Building puppets in code: the auto-rig, the tests and the synthetic fixtures all use this, so a puppet is never
//! assembled by hand-indexing the model.

use omuzaki_puppet_api::{
    BindTarget, BindValues, Binding, Drawable, Interp, Lattice, Mesh, Node, NodeIx, NodeKind, Param, ParamIx, Part,
    Pendulum, PuppetDef, PuppetError, PuppetMeta, TexIx, TextureRef, Transform2,
};

/// Builds a [`PuppetDef`] node by node.
#[derive(Clone, Debug)]
pub struct PuppetBuilder {
    def: PuppetDef,
}

fn ix(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

impl PuppetBuilder {
    /// A puppet called `name` on a canvas of `canvas` model units, with its root node (index 0).
    #[must_use]
    pub fn new(name: &str, canvas: [f32; 2]) -> Self {
        let root = Node {
            name: "root".into(),
            parent: None,
            enabled: true,
            transform: Transform2::default(),
            zsort: 0.0,
            kind: NodeKind::Group,
        };
        let meta = PuppetMeta { name: name.into(), source: "builder".into(), ..PuppetMeta::default() };
        Self { def: PuppetDef { meta, canvas, nodes: vec![root], ..PuppetDef::default() } }
    }

    /// The root node.
    #[must_use]
    pub const fn root(&self) -> NodeIx {
        NodeIx(0)
    }

    /// The metadata, to fill in.
    pub fn meta_mut(&mut self) -> &mut PuppetMeta {
        &mut self.def.meta
    }

    /// Adds a texture.
    pub fn texture(&mut self, file: &str, size: [u32; 2]) -> TexIx {
        self.def.textures.push(TextureRef { file: file.into(), size });
        TexIx(ix(self.def.textures.len() - 1))
    }

    /// Adds a node of `kind` under `parent`, at `translation`.
    pub fn node(&mut self, name: &str, parent: NodeIx, translation: [f32; 2], kind: NodeKind) -> NodeIx {
        self.def.nodes.push(Node {
            name: name.into(),
            parent: Some(parent),
            enabled: true,
            transform: Transform2 { translation, ..Transform2::default() },
            zsort: 0.0,
            kind,
        });
        NodeIx(ix(self.def.nodes.len() - 1))
    }

    /// Adds a group (a pivot) under `parent`, at `translation`.
    pub fn group(&mut self, name: &str, parent: NodeIx, translation: [f32; 2]) -> NodeIx {
        self.node(name, parent, translation, NodeKind::Group)
    }

    /// Adds a part drawing `mesh` with `texture`.
    pub fn part(&mut self, name: &str, parent: NodeIx, texture: TexIx, mesh: Mesh) -> NodeIx {
        self.node(name, parent, [0.0, 0.0], NodeKind::Part(Part { mesh, texture, draw: Drawable::default() }))
    }

    /// Adds a lattice of `cols` by `rows` points over `origin`..`origin + size`.
    pub fn lattice(
        &mut self,
        name: &str,
        parent: NodeIx,
        cols: u32,
        rows: u32,
        origin: [f32; 2],
        size: [f32; 2],
    ) -> NodeIx {
        self.node(name, parent, [0.0, 0.0], NodeKind::Lattice(Lattice { cols, rows, origin, size }))
    }

    /// The node at `node`, to adjust (its transform, zsort, drawable).
    pub fn node_mut(&mut self, node: NodeIx) -> Option<&mut Node> {
        self.def.nodes.get_mut(node.get())
    }

    /// The drawable of a part or composite, to adjust (blend mode, opacity, masks).
    pub fn drawable_mut(&mut self, node: NodeIx) -> Option<&mut Drawable> {
        match &mut self.def.nodes.get_mut(node.get())?.kind {
            NodeKind::Part(p) => Some(&mut p.draw),
            NodeKind::Composite(c) => Some(&mut c.draw),
            _ => None,
        }
    }

    /// Adds a one-axis parameter from `min` to `max`, at rest at `default`, with keys at `keys`.
    pub fn param(&mut self, name: &str, min: f32, max: f32, default: f32, keys: &[f32]) -> ParamIx {
        self.def.params.push(Param {
            name: name.into(),
            two_axes: false,
            min: [min, 0.0],
            max: [max, 0.0],
            default: [default, 0.0],
            keys: [keys.to_vec(), vec![0.0]],
            bindings: Vec::new(),
        });
        ParamIx(ix(self.def.params.len() - 1))
    }

    /// Adds a two-axis parameter.
    pub fn param2(
        &mut self,
        name: &str,
        min: [f32; 2],
        max: [f32; 2],
        default: [f32; 2],
        keys: [&[f32]; 2],
    ) -> ParamIx {
        self.def.params.push(Param {
            name: name.into(),
            two_axes: true,
            min,
            max,
            default,
            keys: [keys[0].to_vec(), keys[1].to_vec()],
            bindings: Vec::new(),
        });
        ParamIx(ix(self.def.params.len() - 1))
    }

    /// Binds `target` of `node` to `param`, one number per key point (x-major).
    pub fn bind(&mut self, param: ParamIx, node: NodeIx, target: BindTarget, interp: Interp, values: Vec<f32>) {
        if let Some(p) = self.def.params.get_mut(param.get()) {
            p.bindings.push(Binding { node, target, interp, set: Vec::new(), values: BindValues::Scalar(values) });
        }
    }

    /// Binds `node`'s points to `param`, one offset set per key point (x-major).
    pub fn bind_deform(&mut self, param: ParamIx, node: NodeIx, interp: Interp, sets: Vec<Vec<[f32; 2]>>) {
        if let Some(p) = self.def.params.get_mut(param.get()) {
            p.bindings.push(Binding {
                node,
                target: BindTarget::Deform,
                interp,
                set: Vec::new(),
                values: BindValues::Offsets(sets),
            });
        }
    }

    /// The parameter at `param`, to adjust.
    pub fn param_mut(&mut self, param: ParamIx) -> Option<&mut Param> {
        self.def.params.get_mut(param.get())
    }

    /// Adds a pendulum.
    pub fn pendulum(&mut self, pendulum: Pendulum) {
        self.def.physics.push(pendulum);
    }

    /// The puppet so far.
    #[must_use]
    pub fn def(&self) -> &PuppetDef {
        &self.def
    }

    /// The finished puppet, checked.
    ///
    /// # Errors
    /// The first rule the puppet breaks.
    pub fn finish(self) -> Result<PuppetDef, PuppetError> {
        self.def.check()?;
        Ok(self.def)
    }
}

/// A grid mesh over the rectangle `rect` (`[x, y, w, h]`, model units), `cols` by `rows` cells, with texture
/// coordinates spanning `uv` (`[u0, v0, u1, v1]`).
#[must_use]
pub fn grid_mesh(rect: [f32; 4], cols: u32, rows: u32, uv: [f32; 4]) -> Mesh {
    let (cols, rows) = (cols.max(1), rows.max(1));
    let mut mesh = Mesh::default();
    for r in 0..=rows {
        for c in 0..=cols {
            #[allow(clippy::cast_precision_loss, reason = "small grids")]
            let (fx, fy) = (c as f32 / cols as f32, r as f32 / rows as f32);
            mesh.vertices.push([rect[0] + rect[2] * fx, rect[1] + rect[3] * fy]);
            mesh.uvs.push([uv[0] + (uv[2] - uv[0]) * fx, uv[1] + (uv[3] - uv[1]) * fy]);
        }
    }
    let stride = cols + 1;
    for r in 0..rows {
        for c in 0..cols {
            let i = r * stride + c;
            mesh.triangles.push([i, i + 1, i + stride + 1]);
            mesh.triangles.push([i, i + stride + 1, i + stride]);
        }
    }
    mesh
}
