//! The shape of a scene's node graph, computed once and shared by the preparation steps: each node's parent, the nodes
//! in hierarchy order, and their world matrices in one pass (one matrix multiplication per node with a parent).

use omuzaki_core::glam::{Mat4, Vec3};
use omuzaki_io_api::IoScene;

/// The parent of every node, by index: `None` for a root or a node nothing lists as a child.
pub(crate) fn parents(scene: &IoScene) -> Vec<Option<usize>> {
    let mut parent = vec![None; scene.nodes.len()];
    for (i, node) in scene.nodes.iter().enumerate() {
        for child in &node.children {
            if let Some(slot) = parent.get_mut(child.index()) {
                *slot = Some(i);
            }
        }
    }
    parent
}

/// Every node once, depth first from the scene's roots and then from every other node with no parent (by index),
/// children in file order, each with the parent it was reached through (`None` for a top node): parents always come
/// before their children. A node `keep` refuses is left out with its whole subtree. A node only a cycle reaches (which
/// a valid scene never has) is left out.
pub(crate) fn hierarchy_order(
    scene: &IoScene,
    parent: &[Option<usize>],
    keep: &dyn Fn(usize) -> bool,
) -> Vec<(usize, Option<usize>)> {
    let n = scene.nodes.len();
    let mut seen = vec![false; n];
    let mut order = Vec::with_capacity(n);
    let tops =
        scene.roots.iter().map(|r| r.index()).chain((0..n).filter(|&i| parent.get(i).copied().flatten().is_none()));
    for top in tops {
        let mut stack = vec![(top, None)];
        while let Some((i, via)) = stack.pop() {
            if i >= n || seen[i] || !keep(i) {
                continue;
            }
            seen[i] = true;
            order.push((i, via));
            let children = scene.nodes[i].children.iter().rev().map(|c| c.index());
            stack.extend(children.filter(|&c| parent.get(c).copied().flatten() == Some(i)).map(|c| (c, Some(i))));
        }
    }
    order
}

/// Every node's world matrix and what it took.
#[derive(Clone, Debug)]
pub(crate) struct World {
    /// Each node's world matrix, scale included, by node index (the identity for a node the order leaves out).
    pub matrices: Vec<Mat4>,
    /// Whether a node and all its ancestors have unit scale, by node index.
    pub unit_scale: Vec<bool>,
}

impl World {
    /// The world position of node `i`.
    pub(crate) fn position(&self, i: usize) -> Option<Vec3> {
        self.matrices.get(i).map(|m| m.w_axis.truncate())
    }
}

/// The world matrices of the nodes in `order` (from [`hierarchy_order`]), each its parent's times its own local one.
pub(crate) fn world(scene: &IoScene, order: &[(usize, Option<usize>)]) -> World {
    counted(scene, order).0
}

/// [`world`], with how many matrix multiplications it took: one per node with a parent.
fn counted(scene: &IoScene, order: &[(usize, Option<usize>)]) -> (World, usize) {
    let n = scene.nodes.len();
    let (mut matrices, mut unit_scale, mut multiplies) = (vec![Mat4::IDENTITY; n], vec![true; n], 0);
    for &(i, via) in order {
        let Some(node) = scene.nodes.get(i) else { continue };
        let t = &node.transform;
        let local = Mat4::from_scale_rotation_translation(t.scale, t.rotation, t.translation);
        matrices[i] = match via.and_then(|p| matrices.get(p)) {
            Some(&up) => {
                multiplies += 1;
                up * local
            }
            None => local,
        };
        unit_scale[i] = t.scale == Vec3::ONE && via.is_none_or(|p| unit_scale.get(p).copied().unwrap_or(true));
    }
    (World { matrices, unit_scale }, multiplies)
}

/// The world matrices of every node of `scene`.
pub(crate) fn world_of(scene: &IoScene) -> World {
    world(scene, &hierarchy_order(scene, &parents(scene), &|_| true))
}

#[cfg(test)]
mod tests {
    use omuzaki_core::glam::{Quat, Vec3};
    use omuzaki_io_api::{IoScene, Node};

    use super::{counted, hierarchy_order, parents, world_of};

    /// The world of `scene` and the multiplications it took.
    fn world_counted(scene: &IoScene) -> (super::World, usize) {
        counted(scene, &hierarchy_order(scene, &parents(scene), &|_| true))
    }

    #[test]
    fn world_transforms_take_one_multiplication_per_node() {
        // Listed leaf first so that index order is not hierarchy order.
        let mut scene = IoScene::new();
        let n = 10_000;
        let ids: Vec<_> = (0..n).map(|_| scene.add_node(Node::new("j").at(Vec3::Y))).collect();
        for w in ids.windows(2) {
            scene.attach(w[1], w[0]);
        }
        scene.roots.push(ids[n - 1]);
        let (world, multiplies) = world_counted(&scene);
        assert_eq!(multiplies, n - 1, "one per node that has a parent");
        assert_eq!(world.position(0), Some(Vec3::new(0.0, 10_000.0, 0.0)), "the deepest node sits n steps up");
    }

    #[test]
    fn world_positions_compose_scale_rotation_and_translation() {
        let mut scene = IoScene::new();
        let root = scene.add_node(Node::new("root").at(Vec3::new(1.0, 0.0, 0.0)));
        scene.nodes[root.index()].transform.rotation = Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
        scene.nodes[root.index()].transform.scale = Vec3::splat(2.0);
        let child = scene.add_node(Node::new("child").at(Vec3::new(1.0, 0.0, 0.0)));
        scene.attach(root, child);
        let (world, multiplies) = world_counted(&scene);
        assert_eq!(multiplies, 1);
        assert_eq!(world_of(&scene).matrices, world.matrices);
        // Scaled by 2, turned a quarter about Z (+X becomes +Y), then moved by the root's translation.
        assert!((world.position(child.index()).unwrap() - Vec3::new(1.0, 2.0, 0.0)).length() < 1e-5);
        assert_eq!(
            (world.unit_scale[root.index()], world.unit_scale[child.index()]),
            (false, false),
            "scale is inherited"
        );
    }
}
