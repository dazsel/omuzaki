//! The neck: a short tapered open cylinder from inside the collar up into the head (port of the prototype's
//! `CylinderGeometry(0.030, 0.038, 0.11, 24, 4, true)` and its weights in `head.js`).
//!
//! Vertex order, normals and triangle order are three.js's, so the mesh can be compared with the prototype's vertex for
//! vertex. The neck belongs to the head joint above [`WEIGHT_LINE_ABOVE_JOINT`], and below it fades from the `neck` bone
//! into the `chest` bone over [`BLEND_LENGTH`], so the collar follows the torso and the top follows the head.

use std::f64::consts::TAU;

use omuzaki_char_api::Neck;
use omuzaki_core::glam::Vec3;
use omuzaki_core::mesh::{MeshData, MeshError};
use omuzaki_face_api::narrow;

/// Columns round the neck.
const RADIAL: u32 = 24;
/// Rows from the top to the bottom.
const ROWS: u32 = 4;
/// The neck's top sits this far above the head joint, tucked up behind the chin (the prototype: 0.61 against 0.56).
pub const TOP_ABOVE_JOINT: f32 = 0.05;
/// Above this height over the head joint a vertex follows the head alone (the prototype: 0.60 against 0.56).
pub const WEIGHT_LINE_ABOVE_JOINT: f32 = 0.04;
/// Below the line the weight moves from `neck` to `chest` over this length (the prototype: `(0.6 - y) * 4`).
pub const BLEND_LENGTH: f32 = 0.25;
/// A neck shorter than this has no mesh: the head sits on the collar.
pub const MIN_HEIGHT: f64 = 1e-4;

/// The neck mesh with its top ring at `top` (bind space), or `None` for a neck shorter than [`MIN_HEIGHT`].
///
/// Skinned to `joints` indices 0 (head), 1 (neck) and 2 (chest); `line` is the height above which a vertex is all head
/// ([`weights`]).
///
/// # Errors
/// [`MeshError`] if the numbers make an invalid mesh (a non-finite radius).
pub fn mesh(neck: &Neck, top: Vec3, line: f32) -> Result<Option<MeshData>, MeshError> {
    if neck.height < MIN_HEIGHT {
        return Ok(None);
    }
    let (radius_top, radius_bottom, height) = (neck.radius_top, neck.radius_bottom, neck.height);
    // The surface slopes by this much per unit of radius; three.js uses it as the normal's y before normalising.
    let slope = (radius_bottom - radius_top) / height;
    let (mut positions, mut normals, mut joints, mut weight_sets) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for row in 0..=ROWS {
        let along = f64::from(row) / f64::from(ROWS);
        let radius = along * (radius_bottom - radius_top) + radius_top;
        for column in 0..=RADIAL {
            let (sin, cos) = (f64::from(column) / f64::from(RADIAL) * TAU).sin_cos();
            // The first row is the top ring; each row below it is `height / ROWS` lower.
            #[allow(clippy::cast_possible_truncation)] // metres, well inside f32
            let at = Vec3::new((radius * sin) as f32, (-along * height) as f32, (radius * cos) as f32);
            let point = top + at;
            let normal = Vec3::new(narrow(sin), narrow(slope), narrow(cos)).normalize_or_zero();
            let (bones, shares) = weights(point.y, line);
            positions.push(point);
            normals.push(normal);
            joints.push(bones);
            weight_sets.push(shares);
        }
    }
    let mut indices = Vec::new();
    let ring = RADIAL + 1;
    for column in 0..RADIAL {
        for row in 0..ROWS {
            let (a, b) = (row * ring + column, (row + 1) * ring + column);
            let (c, d) = (b + 1, a + 1);
            // three.js leaves out the degenerate triangle of a cone's apex; both radii are positive here
            indices.extend([a, b, d, b, c, d]);
        }
    }
    let mesh = MeshData { positions, normals, joints, weights: weight_sets, indices, ..MeshData::default() };
    mesh.validate()?;
    Ok(Some(mesh))
}

/// The skin weights of a neck vertex at height `y`: all head above `line`, then `neck` fading into `chest` over
/// [`BLEND_LENGTH`] below it.
pub fn weights(y: f32, line: f32) -> ([u16; 4], [f32; 4]) {
    if y > line {
        return ([0; 4], [1.0, 0.0, 0.0, 0.0]);
    }
    let chest = ((line - y) / BLEND_LENGTH).clamp(0.0, 1.0);
    ([1, 2, 0, 0], [1.0 - chest, chest, 0.0, 0.0])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_mesh() -> MeshData {
        mesh(&Neck::default(), Vec3::new(0.0, 0.61, -0.022), 0.60).unwrap().unwrap()
    }

    #[test]
    fn it_is_threejs_open_24_by_4_cylinder() {
        let m = default_mesh();
        assert_eq!((m.positions.len(), m.triangle_count()), (5 * 25, 24 * 4 * 2));
        let (top, bottom) = (m.positions[0], m.positions[4 * 25]);
        assert!((top - Vec3::new(0.0, 0.61, -0.022 + 0.03)).length() < 1e-6, "{top:?}");
        assert!((bottom - Vec3::new(0.0, 0.50, -0.022 + 0.038)).length() < 1e-6, "{bottom:?}");
    }

    #[test]
    fn the_seam_columns_coincide_and_normals_are_unit() {
        let m = default_mesh();
        for row in 0..5 {
            let (a, b) = (m.positions[row * 25], m.positions[row * 25 + 24]);
            assert!((a - b).length() < 1e-6, "row {row}");
        }
        assert!(m.normals.iter().all(|n| (n.length() - 1.0).abs() < 1e-6));
    }

    #[test]
    fn a_zero_height_neck_has_no_mesh() {
        let neck = Neck { height: 0.0, ..Neck::default() };
        assert!(mesh(&neck, Vec3::ZERO, 0.0).unwrap().is_none());
    }

    #[test]
    fn weights_fade_from_neck_to_chest_below_the_line() {
        let line = 0.6;
        assert_eq!(weights(0.7, line).1, [1.0, 0.0, 0.0, 0.0]);
        let (joints, w) = weights(0.55, line);
        assert_eq!(&joints[..2], &[1, 2]);
        assert!((w[0] - 0.8).abs() < 1e-6 && (w[1] - 0.2).abs() < 1e-6, "{w:?}");
        assert_eq!(weights(0.1, line).1, [0.0, 1.0, 0.0, 0.0], "past the blend it is all chest");
        assert!(default_mesh().weights.iter().all(|w| (w.iter().sum::<f32>() - 1.0).abs() < 1e-6));
    }
}
