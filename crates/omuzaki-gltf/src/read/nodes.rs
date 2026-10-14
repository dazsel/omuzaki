//! Nodes and the default scene's roots.

use omuzaki_core::glam::{Mat3, Mat4, Quat, Vec3, Vec4};
use omuzaki_io_api::{IoError, MeshIx, Node, NodeIx, Passthrough, SkinIx, Transform};

use super::extensions;
use crate::hooks::{Carrier, ImportHooks};
use crate::ix32;
use crate::schema::{self as gltf, Document};
use crate::tally::Tally;

/// How far from length 1 a rotation may be before the importer normalises it: a few ulps of noise is left alone, so a
/// file this crate wrote reads back bit-identically.
const UNIT_TOLERANCE: f32 = 1e-6;

/// Builds node `index`.
///
/// # Errors
/// [`IoError::Malformed`] for a transform that can't be a transform: non-finite numbers, a zero quaternion, a matrix
/// that isn't 16 numbers.
pub(crate) fn node(
    index: usize,
    def: &gltf::Node,
    hooks: &mut dyn ImportHooks,
    tally: &mut Tally,
) -> Result<Node, IoError> {
    let at = format!("/nodes/{index}");
    let mut node = Node::new(def.name.clone());
    node.transform = transform(def, &at, tally)?;
    node.children = def.children.iter().map(|&c| NodeIx(ix32(c))).collect();
    node.mesh = def.mesh.map(|m| MeshIx(ix32(m)));
    node.skin = def.skin.map(|s| SkinIx(ix32(s)));
    node.passthrough = Passthrough {
        extensions: extensions::carried(def.extensions.clone(), Carrier::Node, &at, hooks, tally),
        extras: def.extras.clone(),
    };
    if def.weights.as_ref().is_some_and(|w| w.iter().any(|&x| x != 0.0)) {
        tally.warn(
            "gltf.morph_weights_dropped",
            "default morph target weights were dropped",
            format!("{at}/weights"),
            None,
        );
    }
    Ok(node)
}

fn transform(def: &gltf::Node, at: &str, tally: &mut Tally) -> Result<Transform, IoError> {
    let has_trs = def.translation.is_some() || def.rotation.is_some() || def.scale.is_some();
    if let (Some(matrix), false) = (&def.matrix, has_trs) {
        return from_matrix(matrix, at, tally);
    }
    if def.matrix.is_some() {
        let msg = "a node has both a matrix and translation/rotation/scale; the matrix was ignored";
        tally.warn("gltf.node_transform_ambiguous", msg, at, None);
    }
    let finite = |what: &str, ok: bool| {
        if ok {
            Ok(())
        } else {
            Err(IoError::Malformed { message: format!("{what} isn't finite"), at: Some(at.to_owned()) })
        }
    };
    let mut t = Transform::default();
    if let Some(v) = def.translation {
        t.translation = Vec3::from_array(v);
        finite("a translation", t.translation.is_finite())?;
    }
    if let Some(v) = def.scale {
        t.scale = Vec3::from_array(v);
        finite("a scale", t.scale.is_finite())?;
    }
    if let Some(v) = def.rotation {
        t.rotation = unit_rotation(v, at)?;
    }
    Ok(t)
}

fn unit_rotation(v: [f32; 4], at: &str) -> Result<Quat, IoError> {
    let q = Quat::from_array(v);
    let length = q.length();
    if !length.is_finite() || length <= 0.0 {
        return Err(IoError::Malformed {
            message: "a rotation is zero or not finite".into(),
            at: Some(format!("{at}/rotation")),
        });
    }
    Ok(if (length - 1.0).abs() > UNIT_TOLERANCE { q / length } else { q })
}

/// How flat the axes of a matrix may be before it is called singular, as the volume of the box their *directions*
/// span (1 for perpendicular axes, 0 for axes in a plane). `f32` resolves a direction to about 1e-7.
const SINGULAR: f32 = 1e-6;

/// The unit direction of `axis`, or none for the zero vector. The axis is divided by its largest component first, so
/// squaring it for the length can neither overflow nor underflow, whatever the matrix's scale.
fn direction(axis: Vec3) -> Option<Vec3> {
    let peak = axis.abs().max_element();
    (peak > 0.0).then(|| (axis / peak).normalize())
}

/// The length of `axis`, computed like [`direction`] so it stays finite and non-zero at any scale.
fn length(axis: Vec3) -> f32 {
    let peak = axis.abs().max_element();
    if peak > 0.0 { peak * (axis / peak).length() } else { 0.0 }
}

/// Decomposes a column-major matrix into translation, rotation and scale. Anything that isn't a plain affine
/// transform (shear, projection, a singular matrix) is approximated and reported.
///
/// Singular means the axes span no volume: one is zero, or they lie in a plane. That is judged on their directions,
/// so it doesn't depend on the matrix's scale (a determinant does: it goes with the cube of a uniform scale, and a
/// matrix scaled by 9e-5 has a determinant under 1e-12 though nothing is wrong with it).
fn from_matrix(values: &[f32], at: &str, tally: &mut Tally) -> Result<Transform, IoError> {
    let malformed = |message: &str| IoError::Malformed { message: message.into(), at: Some(format!("{at}/matrix")) };
    let cols = <[f32; 16]>::try_from(values).map_err(|_| malformed("a matrix needs 16 numbers"))?;
    let m = Mat4::from_cols_array(&cols);
    if !m.is_finite() {
        return Err(malformed("a matrix isn't finite"));
    }
    let affine = m.row(3).abs_diff_eq(Vec4::W, 1e-6);
    let axes = [m.x_axis.truncate(), m.y_axis.truncate(), m.z_axis.truncate()];
    let translation = m.w_axis.truncate();
    let volume = match axes.map(direction) {
        [Some(x), Some(y), Some(z)] => Some(x.dot(y.cross(z))),
        _ => None,
    };
    let Some(volume) = volume.filter(|v| v.abs() >= SINGULAR) else {
        let msg = "a singular matrix was reduced to its translation";
        tally.warn(
            "gltf.matrix_approximated",
            msg,
            format!("{at}/matrix"),
            Some("write translation, rotation and scale"),
        );
        return Ok(Transform { translation, ..Transform::default() });
    };
    // A mirror (negative volume) is carried by the sign of the x scale, as glTF's own decomposition does.
    let flip = if volume < 0.0 { -1.0 } else { 1.0 };
    let scale = Vec3::new(flip * length(axes[0]), length(axes[1]), length(axes[2]));
    if !scale.is_finite() {
        return Err(malformed("a matrix scale is too large to hold"));
    }
    let rotation =
        Quat::from_mat3(&Mat3::from_cols(axes[0] / scale.x, axes[1] / scale.y, axes[2] / scale.z)).normalize();
    let linear = Mat3::from_cols(axes[0], axes[1], axes[2]);
    let peak = axes.iter().map(|a| a.abs().max_element()).fold(0.0f32, f32::max);
    let exact = (Mat3::from_quat(rotation) * Mat3::from_diagonal(scale)).abs_diff_eq(linear, 1e-5 * peak);
    if affine && exact {
        tally.info(
            "gltf.matrix_decomposed",
            "node matrices were decomposed into translation, rotation and scale",
            format!("{at}/matrix"),
        );
    } else {
        let msg = "a matrix with shear or a projection was approximated by translation, rotation and scale";
        tally.warn(
            "gltf.matrix_approximated",
            msg,
            format!("{at}/matrix"),
            Some("write translation, rotation and scale"),
        );
    }
    Ok(Transform { translation, rotation, scale })
}

/// The nodes of the default scene (see [`Document::default_roots`]).
///
/// # Errors
/// [`IoError::Malformed`] when `scene` names a scene that doesn't exist.
pub(crate) fn roots(doc: &Document) -> Result<Vec<NodeIx>, IoError> {
    Ok(doc.default_roots()?.into_iter().map(|n| NodeIx(ix32(n))).collect())
}

#[cfg(test)]
mod tests {
    use omuzaki_core::rng::Rng;
    use omuzaki_io_api::{Report, Severity};

    use super::*;

    fn def(f: impl FnOnce(&mut gltf::Node)) -> gltf::Node {
        let mut d = gltf::Node::default();
        f(&mut d);
        d
    }

    fn built(index: usize, def: &gltf::Node, tally: &mut Tally) -> Result<Node, IoError> {
        node(index, def, &mut crate::hooks::NoHooks, tally)
    }

    fn report(t: Tally) -> Report {
        let mut r = Report::new();
        t.finish(&mut r);
        r
    }

    #[test]
    fn trs_is_read_and_a_slightly_long_rotation_is_normalised() {
        let mut t = Tally::default();
        let d = def(|d| {
            d.translation = Some([1.0, 2.0, 3.0]);
            d.scale = Some([2.0, 2.0, 2.0]);
            d.rotation = Some([0.0, 0.0, 0.0, 2.0]);
        });
        let n = built(0, &d, &mut t).unwrap();
        assert_eq!(n.transform.translation, Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(n.transform.rotation, Quat::IDENTITY);
        // A unit quaternion that is a few ulps off is left exactly as written.
        let off = [0.0, 0.0, 0.0, 1.000_000_1];
        let kept = built(0, &def(|d| d.rotation = Some(off)), &mut t).unwrap();
        assert_eq!(kept.transform.rotation.to_array(), off);
    }

    #[test]
    fn matrices_decompose_and_the_unrepresentable_are_reported() {
        let mut tally = Tally::default();
        let scale = Vec3::new(2.0, 1.0, 1.0);
        let matrix = Mat4::from_scale_rotation_translation(scale, Quat::from_rotation_y(0.5), Vec3::Y);
        let decomposed = built(3, &def(|d| d.matrix = Some(matrix.to_cols_array().to_vec())), &mut tally).unwrap();
        assert!((decomposed.transform.translation - Vec3::Y).length() < 1e-6);
        assert!((decomposed.transform.scale - scale).length() < 1e-5);
        let shear = [1.0, 0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];
        built(4, &def(|d| d.matrix = Some(shear.to_vec())), &mut tally).unwrap();
        built(5, &def(|d| d.matrix = Some(vec![0.0; 16])), &mut tally).unwrap();
        let findings = report(tally).findings;
        let rules: Vec<_> = findings.iter().map(|f| (f.rule.as_str(), f.severity)).collect();
        assert_eq!(
            rules,
            [
                ("gltf.matrix_decomposed", Severity::Info),
                ("gltf.matrix_approximated", Severity::Warning),
                ("gltf.matrix_approximated", Severity::Warning),
            ]
        );
        assert_eq!(findings[1].location.as_deref(), Some("/nodes/4/matrix"));
    }

    /// Reads `m` back as a node's matrix: the transform, and the findings as (rule, severity, message).
    fn from_node_matrix(m: Mat4) -> (Transform, Vec<(String, Severity, String)>) {
        let mut tally = Tally::default();
        let node = built(0, &def(|d| d.matrix = Some(m.to_cols_array().to_vec())), &mut tally).unwrap();
        let findings = report(tally).findings.into_iter().map(|f| (f.rule, f.severity, f.message)).collect();
        (node.transform, findings)
    }

    /// Uniform scales from far below the old absolute test (`|det| < 1e-12` is any scale under about 1e-4) to far above 1.
    const SCALES: [f32; 14] = [1e-6, 9e-5, 1e-4, 1e-3, 0.03, 0.5, 1.0, 2.0, 37.0, 1e3, 4.5e4, 1e5, 3e5, 1e6];

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() <= 1e-5 * a.abs().max(b.abs())
    }

    #[test]
    fn a_matrix_decomposes_at_any_uniform_scale() {
        // The regression: a determinant test called a valid matrix of scale 9e-5 singular and read it as scale 1.
        let m = Mat4::from_scale_rotation_translation(Vec3::splat(9e-5), Quat::from_rotation_y(0.5), Vec3::Y);
        let (t, findings) = from_node_matrix(m);
        assert!(close(t.scale.x, 9e-5) && close(t.scale.y, 9e-5) && close(t.scale.z, 9e-5), "{:?}", t.scale);
        assert_eq!(t.translation, Vec3::Y);
        assert!(t.rotation.dot(Quat::from_rotation_y(0.5)).abs() > 1.0 - 1e-6, "{:?}", t.rotation);
        let rules: Vec<_> = findings.iter().map(|f| (f.0.as_str(), f.1)).collect();
        assert_eq!(rules, [("gltf.matrix_decomposed", Severity::Info)], "no 'singular' warning");

        // Property: random rotations, per-axis scales (some mirrored) and translations, at every uniform scale.
        let mut rng = Rng::new(7);
        for k in SCALES {
            for _ in 0..40 {
                let mut axis = Vec3::new(rng.range_f32(-1.0, 1.0), rng.range_f32(-1.0, 1.0), rng.range_f32(-1.0, 1.0));
                axis = if axis.length() > 0.1 { axis.normalize() } else { Vec3::Y };
                let rotation = Quat::from_axis_angle(axis, rng.range_f32(-3.0, 3.0));
                let mut scale = Vec3::new(rng.range_f32(0.5, 2.0), rng.range_f32(0.5, 2.0), rng.range_f32(0.5, 2.0));
                if rng.next_f32() < 0.25 {
                    scale.y = -scale.y;
                }
                let translation =
                    Vec3::new(rng.range_f32(-9.0, 9.0), rng.range_f32(-9.0, 9.0), rng.range_f32(-9.0, 9.0));
                let m = Mat4::from_scale_rotation_translation(scale * k, rotation, translation);
                let (t, findings) = from_node_matrix(m);
                let rules: Vec<_> = findings.iter().map(|f| f.0.as_str()).collect();
                assert_eq!(rules, ["gltf.matrix_decomposed"], "k {k}, {m:?}");
                assert_eq!(t.translation, translation, "k {k}");
                for (got, want) in t.scale.abs().to_array().into_iter().zip((scale * k).abs().to_array()) {
                    assert!(close(got, want), "k {k}: scale {:?} for {scale:?}", t.scale);
                }
                let back = Mat4::from_scale_rotation_translation(t.scale, t.rotation, t.translation);
                for (got, want) in
                    Mat3::from_mat4(back).to_cols_array().into_iter().zip(Mat3::from_mat4(m).to_cols_array())
                {
                    assert!((got - want).abs() <= 1e-5 * 2.0 * 2.0 * k, "k {k}: {got} vs {want}");
                }
            }
        }
    }

    #[test]
    fn a_degenerate_matrix_is_singular_at_any_scale_and_a_sheared_one_is_not() {
        let mut checked = 0;
        for k in SCALES {
            let (x, y) = (Vec4::new(k, 0.0, 0.0, 0.0), Vec4::new(0.0, k, 0.0, 0.0));
            let t = Vec4::new(1.0, 2.0, 3.0, 1.0);
            for (label, cols) in [
                ("a zero axis", [Vec4::ZERO, y, Vec4::new(0.0, 0.0, k, 0.0)]),
                ("collinear axes", [x, x * 2.0, y]),
                ("coplanar axes", [x, y, x + y * 3.0]),
                ("all axes zero", [Vec4::ZERO; 3]),
            ] {
                let (transform, findings) = from_node_matrix(Mat4::from_cols(cols[0], cols[1], cols[2], t));
                assert_eq!(findings.len(), 1, "{label} at {k}");
                assert!(
                    findings[0].0 == "gltf.matrix_approximated" && findings[0].2.contains("singular"),
                    "{label} at {k}"
                );
                assert_eq!(transform, Transform { translation: Vec3::new(1.0, 2.0, 3.0), ..Transform::default() });
                checked += 1;
            }
            // Shear is not singular, and is told apart from a clean decomposition whatever the scale.
            let shear = Mat4::from_cols(x, Vec4::new(0.0, k, k, 0.0), Vec4::new(0.0, 0.0, k, 0.0), t);
            let (_, findings) = from_node_matrix(shear);
            assert_eq!(findings.len(), 1);
            assert!(findings[0].2.contains("shear"), "shear at {k}: {findings:?}");
        }
        assert_eq!(checked, 4 * SCALES.len());
    }

    #[test]
    fn a_matrix_too_large_to_hold_a_scale_is_malformed_not_infinite() {
        let huge = Mat4::from_cols(
            Vec4::new(3e38, 3e38, 0.0, 0.0),
            Vec4::new(0.0, 1.0, 0.0, 0.0),
            Vec4::new(0.0, 0.0, 1.0, 0.0),
            Vec4::W,
        );
        let mut tally = Tally::default();
        let err = built(0, &def(|d| d.matrix = Some(huge.to_cols_array().to_vec())), &mut tally).unwrap_err();
        assert!(matches!(err, IoError::Malformed { .. }), "{err:?}");
    }

    #[test]
    fn nonsense_transforms_are_malformed() {
        let mut t = Tally::default();
        for d in [
            def(|d| d.rotation = Some([0.0; 4])),
            def(|d| d.translation = Some([f32::INFINITY, 0.0, 0.0])),
            def(|d| d.matrix = Some(vec![1.0; 15])),
            def(|d| d.matrix = Some(vec![f32::INFINITY; 16])),
        ] {
            assert!(matches!(built(0, &d, &mut t), Err(IoError::Malformed { .. })));
        }
    }

    #[test]
    fn roots_are_scene_node_indices() {
        let doc = Document {
            nodes: vec![def(|d| d.children = vec![1]), gltf::Node::default(), gltf::Node::default()],
            ..Document::default()
        };
        assert_eq!(roots(&doc).unwrap(), [NodeIx(0), NodeIx(2)]);
    }
}
