//! Tiny mesh builders the mock parts share: an ellipsoid, a tube round a body region and a ribbon along a clump.

use omuzaki_char_api::{BodyFit, BodyRegion, ExtraAttribute, HAIR_INFO, HAIR_UV, HairClump, MeshData};
use omuzaki_core::glam::{Vec2, Vec3};
use omuzaki_core::id::{Slug, SlugError};

/// A UV ellipsoid in bind space, `theta` from the top down to `theta_max_deg`, `phi` from the front (+z) toward the
/// left (+x) once round. `radius` gives the distance from `centre` along a unit direction. Normals are radial, which
/// is enough for a stand-in.
pub fn ellipsoid(
    centre: Vec3,
    radius: &dyn Fn(Vec3) -> f32,
    rings: u32,
    segments: u32,
    theta_max_deg: f32,
) -> MeshData {
    let mut mesh = MeshData::default();
    for i in 0..=rings {
        let theta = theta_max_deg.to_radians() * frac(i, rings);
        for j in 0..=segments {
            let phi = std::f32::consts::TAU * frac(j, segments);
            let dir = Vec3::new(theta.sin() * phi.sin(), theta.cos(), theta.sin() * phi.cos());
            mesh.positions.push(centre + dir * radius(dir));
            mesh.normals.push(dir);
            mesh.uvs.push(Vec2::new(frac(j, segments), frac(i, rings)));
        }
    }
    for i in 0..rings {
        for j in 0..segments {
            let a = i * (segments + 1) + j;
            let b = a + segments + 1;
            mesh.indices.extend([a, b, a + 1, b, b + 1, a + 1]);
        }
    }
    mesh
}

fn frac(i: u32, n: u32) -> f32 {
    #[allow(clippy::cast_precision_loss)] // small counts
    {
        i as f32 / n.max(1) as f32
    }
}

/// An open tube round `region` of `fit`, from `t = 0` to `t = 1`. Each ring also gets a skin weight toward the bone
/// pair `(first, second)`: `first` at the start of the region, `second` at its end.
pub fn tube(fit: &dyn BodyFit, region: BodyRegion, rings: u32, segments: u32) -> Option<MeshData> {
    let mut mesh = MeshData::default();
    for i in 0..=rings {
        let t = frac(i, rings);
        let s = fit.section(region, t)?;
        for j in 0..segments {
            let phi = std::f32::consts::TAU * frac(j, segments);
            let (sin, cos) = phi.sin_cos();
            let dir = Vec3::new(sin * s.radii.x, 0.0, cos * s.radii.y);
            mesh.positions.push(s.centre + dir);
            mesh.normals.push(Vec3::new(sin / s.radii.x, 0.0, cos / s.radii.y).normalize_or_zero());
            mesh.uvs.push(Vec2::new(frac(j, segments), t));
            mesh.joints.push([0, 1, 0, 0]);
            mesh.weights.push([1.0 - t, t, 0.0, 0.0]);
        }
    }
    for i in 0..rings {
        for j in 0..segments {
            let a = i * segments + j;
            let b = i * segments + (j + 1) % segments;
            mesh.indices.extend([a, b, a + segments, b, b + segments, a + segments]);
        }
    }
    Some(mesh)
}

/// An empty mesh carrying the hair program's two extra attributes, in the order [`ribbon`] fills them.
///
/// # Errors
/// Only if a contract constant stopped being a valid slug (a test catches that).
pub fn hair_mesh() -> Result<MeshData, SlugError> {
    let attribute =
        |name: &str, components| Slug::new(name).map(|name| ExtraAttribute { name, components, data: Vec::new() });
    Ok(MeshData { extra: vec![attribute(HAIR_UV, 2)?, attribute(HAIR_INFO, 3)?], ..MeshData::default() })
}

/// A flat ribbon along a clump: two vertices per rest point, as wide as the clump's taper says there. Counter-clockwise
/// seen from outside (away from the head's vertical axis), so the front face and the normal agree: the renderer flips
/// the normal on a back face, and hair shading that reads `N.V` (the angel ring) would see it inward. The mesh must come from
/// [`hair_mesh`]: ribbons fill its `hair-uv` and `hair-info` attributes, `seed` being the clump's random number.
/// `head_centre` is the origin of the polar angle in `hair-info`: the angle from the head's top (+Y) to the direction
/// from that centre to the point, in radians, as the prototype's `acos(dir.y)` and `HAIR_INFO` define it (0 at the
/// crown, pi/2 at the equator).
pub fn ribbon(clump: &HairClump, seed: f32, head_centre: Vec3, mesh: &mut MeshData) {
    let base = u32::try_from(mesh.positions.len()).unwrap_or(u32::MAX);
    let last = clump.rest.len().saturating_sub(1);
    for (k, p) in clump.rest.iter().enumerate() {
        let t = if last == 0 { 0.0 } else { crate::util::frac_usize(k, last) };
        let tangent = (clump.rest[(k + 1).min(last)] - clump.rest[k.saturating_sub(1)]).normalize_or_zero();
        let outward = Vec3::new(p.x, 0.0, p.z).try_normalize().unwrap_or(Vec3::Z);
        let side = tangent.cross(outward).normalize_or_zero() * (clump.width.at(t) * 0.5);
        mesh.positions.push(*p - side);
        mesh.positions.push(*p + side);
        mesh.normals.extend([outward; 2]);
        mesh.uvs.extend([Vec2::new(-1.0, t), Vec2::new(1.0, t)]);
        let polar = (*p - head_centre).normalize_or_zero().y.clamp(-1.0, 1.0).acos();
        if let [uv, info] = mesh.extra.as_mut_slice() {
            uv.data.extend([-1.0, t, 1.0, t]);
            info.data.extend([seed, clump.shade, polar, seed, clump.shade, polar]);
        }
    }
    for k in 0..u32::try_from(last).unwrap_or(0) {
        let i = base + 2 * k;
        mesh.indices.extend([i, i + 1, i + 2, i + 1, i + 3, i + 2]);
    }
}
