//! The worked example of `docs/design/puppet2d.md` §4: two parts, one parameter, one binding, evaluated at three
//! values, and the draw list with a multiply part. The numbers in the design are these.

use omuzaki_puppet::{Evaluator, PuppetBuilder, grid_mesh};
use omuzaki_puppet_api::{BlendMode, DrawCmd2D, Interp};

#[test]
fn the_worked_example_evaluates_to_the_designs_numbers() {
    let mut b = PuppetBuilder::new("worked", [40.0, 40.0]);
    let skin = b.texture("skin.png", [4, 4]);
    let shade = b.texture("shade.png", [4, 4]);
    let root = b.root();
    let face = b.part("face", root, skin, grid_mesh([-10.0, -10.0, 20.0, 20.0], 1, 1, [0.0, 0.0, 1.0, 1.0]));
    let blush = b.part("blush", root, shade, grid_mesh([-8.0, 0.0, 16.0, 4.0], 1, 1, [0.0, 0.0, 1.0, 1.0]));
    if let Some(n) = b.node_mut(face) {
        n.zsort = 0.1;
    }
    if let Some(d) = b.drawable_mut(blush) {
        d.blend = BlendMode::Multiply;
        d.opacity = 0.8;
    }
    let squash = b.param("squash", 0.0, 1.0, 0.0, &[0.0, 1.0]);
    // At squash = 1 the face's top edge comes down 4 and its bottom edge up 2.
    b.bind_deform(
        squash,
        face,
        Interp::Linear,
        vec![vec![[0.0, 0.0]; 4], vec![[0.0, 4.0], [0.0, 4.0], [0.0, -2.0], [0.0, -2.0]]],
    );
    let def = b.finish().unwrap_or_else(|e| panic!("{e}"));
    let mut e = Evaluator::new(def).unwrap_or_else(|e| panic!("{e}"));
    let mut at = |v: f32| {
        e.set(squash, [v, 0.0]);
        e.evaluate();
        e.points(face).iter().map(|p| [p.x, p.y]).collect::<Vec<_>>()
    };
    assert_eq!(at(0.0), vec![[-10.0, -10.0], [10.0, -10.0], [-10.0, 10.0], [10.0, 10.0]]);
    assert_eq!(at(0.5), vec![[-10.0, -8.0], [10.0, -8.0], [-10.0, 9.0], [10.0, 9.0]]);
    assert_eq!(at(1.0), vec![[-10.0, -6.0], [10.0, -6.0], [-10.0, 8.0], [10.0, 8.0]]);
    let list = e.draw_list();
    let meshes: Vec<_> = list
        .cmds
        .iter()
        .filter_map(|c| match c {
            DrawCmd2D::Mesh { texture, first, count, state } => {
                Some((texture.get(), *first, *count, state.blend, state.opacity))
            }
            _ => None,
        })
        .collect();
    // The face (larger zsort) draws first, then the blush multiplies over it at 0.8.
    assert_eq!(meshes, vec![(0, 0, 6, BlendMode::Normal, 1.0), (1, 6, 6, BlendMode::Multiply, 0.8)]);
    assert_eq!(list.vertices.len(), 8);
}
