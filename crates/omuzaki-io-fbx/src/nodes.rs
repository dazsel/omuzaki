//! The node hierarchy: ufbx's nodes as `Node { name, transform, children }`, and the findings about the conversion.

use omuzaki_core::glam::{DMat3, DQuat, DVec3};
use omuzaki_io_api::{Finding, ImportCtx, IoError, IoScene, Node, NodeIx, Report, Transform};
use ufbx::SceneRoot;

use crate::load::{TARGET_AXES, fraction};

/// The longest a creator string is kept, in characters; it is from an untrusted file and only a diagnostic.
const GENERATOR_CHARS: usize = 200;
/// How far a unit scale may be from one metre and still count as metres (ufbx rounds near powers of ten itself).
const UNIT_TOLERANCE: f64 = 1e-9;

/// The change of basis from the file's space to canonical space: the linear part of the root transform ufbx computes from
/// the file's axes and unit (`SpaceConversion::TransformRoot`), a signed permutation of the axes times the unit scale.
///
/// Every node's local transform is conjugated by it, so a turn about the file's up axis becomes a turn about +Y at the
/// node that has it. ufbx's own `AdjustTransforms` would adjust only the root's children and leave the rest in the file's
/// frame.
struct Basis {
    /// Axes and unit scale together; maps a file position to a canonical one.
    linear: DMat3,
    /// The axes alone (orthogonal; its determinant is -1 for a file of the other handedness).
    axes: DMat3,
}

impl Basis {
    fn new(root: &ufbx::Transform) -> Self {
        let rotation = DQuat::from_xyzw(root.rotation.x, root.rotation.y, root.rotation.z, root.rotation.w).normalize();
        let scale = DVec3::new(root.scale.x, root.scale.y, root.scale.z);
        let linear = DMat3::from_quat(rotation) * DMat3::from_diagonal(scale);
        // The unit scale is uniform; dividing it out leaves the orthogonal part.
        let unit = linear.x_axis.length();
        Self { linear, axes: linear * (1.0 / unit) }
    }

    /// `local` in canonical space: translation scaled and turned, rotation conjugated, scale permuted.
    fn apply(&self, local: &ufbx::Transform) -> Transform {
        let q = DQuat::from_xyzw(local.rotation.x, local.rotation.y, local.rotation.z, local.rotation.w);
        let turn = if q.is_finite() && q.length_squared() > f64::EPSILON { q.normalize() } else { DQuat::IDENTITY };
        // A rotation seen through an axis permutation is a rotation (a mirror reverses its sense, which the matrix
        // conjugation carries), so the result is always a proper one.
        let rotation = DQuat::from_mat3(&(self.axes * DMat3::from_quat(turn) * self.axes.transpose())).normalize();
        let scale = DVec3::new(local.scale.x, local.scale.y, local.scale.z);
        // One non-zero entry per row of `axes`, so this permutes the scale components.
        let abs = DMat3::from_cols(self.axes.x_axis.abs(), self.axes.y_axis.abs(), self.axes.z_axis.abs());
        let translation = self.linear * DVec3::new(local.translation.x, local.translation.y, local.translation.z);
        Transform { translation: translation.as_vec3(), rotation: rotation.as_quat(), scale: (abs * scale).as_vec3() }
    }
}

/// Builds the scene's nodes and the findings from a loaded FBX.
///
/// # Errors
/// [`IoError::Limit`] over `Limits::max_nodes`, [`IoError::Cancelled`], [`IoError::Internal`] if ufbx's graph is not
/// the tree it promises.
pub(crate) fn convert(loaded: &SceneRoot, ctx: &ImportCtx<'_>) -> Result<(IoScene, Report), IoError> {
    let max_nodes = u64::from(ctx.options.limits.max_nodes);
    let total = loaded.nodes.len() as u64;
    // ufbx's root node is not one of ours, so it doesn't count.
    let got = total.saturating_sub(1);
    if got > max_nodes {
        return Err(IoError::Limit { what: "node count", limit: max_nodes, got });
    }
    // ufbx's `typed_id` is a node's index in `nodes`; ours skips the root.
    let mut ix: Vec<Option<NodeIx>> = vec![None; loaded.nodes.len()];
    let basis = Basis::new(&loaded.root_node.local_transform);
    let mut scene = IoScene::new();
    for (n, node) in loaded.nodes.iter().enumerate() {
        if node.is_root {
            continue;
        }
        let mut converted = Node::new(node.element.name.to_string());
        converted.transform = basis.apply(&node.local_transform);
        let t = &converted.transform;
        if !(t.translation.is_finite() && t.rotation.is_finite() && t.scale.is_finite()) {
            return Err(IoError::Malformed {
                message: format!("node {n} has a transform that is not a finite number"),
                at: Some(format!("nodes/{n}")),
            });
        }
        ix[n] = Some(scene.add_node(converted));
        if n % 4096 == 0 {
            ctx.progress.checkpoint("fbx.nodes", fraction(n as u64, total))?;
        }
    }
    let lookup = |node: &ufbx::Node| {
        ix.get(node.element.typed_id as usize)
            .copied()
            .flatten()
            .ok_or_else(|| IoError::Internal("an FBX node refers to a node that was not read".to_owned()))
    };
    for node in loaded.nodes.iter().filter(|n| !n.is_root) {
        let me = lookup(node)?;
        let children = node.children.iter().map(|c| lookup(c)).collect::<Result<Vec<_>, _>>()?;
        scene.nodes[me.index()].children = children;
    }
    scene.roots = loaded.root_node.children.iter().map(|c| lookup(c)).collect::<Result<_, _>>()?;
    let creator = loaded.metadata.creator.trim();
    if !creator.is_empty() {
        scene.source_generator = Some(creator.chars().take(GENERATOR_CHARS).collect());
    }
    Ok((scene, findings(loaded)))
}

/// What the conversion to canonical space changed.
fn findings(loaded: &SceneRoot) -> Report {
    let mut report = Report::new();
    let unit = loaded.settings.original_unit_meters;
    if (unit - 1.0).abs() > UNIT_TOLERANCE {
        report.push(
            Finding::info(
                "fbx.units_converted",
                format!("the file's unit is {unit} m; positions were scaled to metres"),
            )
            .at("GlobalSettings/UnitScaleFactor"),
        );
    }
    // `settings.axes` keeps the file's own axes even though the nodes were converted.
    let (a, t) = (loaded.settings.axes, TARGET_AXES);
    if (a.right, a.up, a.front) != (t.right, t.up, t.front) {
        let message = format!(
            "the file's axes (right {:?}, up {:?}, front {:?}) were rotated to right-handed Y-up with the front toward +Z",
            a.right, a.up, a.front
        );
        report.push(Finding::info("fbx.axes_converted", message).at("GlobalSettings"));
    }
    report
}
