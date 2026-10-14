//! The 0.x `meta`: reading it into the scene and writing it back.
//!
//! Every rights decision is `omuzaki-io-api`'s, so the 1.0 and 0.x paths and every other exporter share one set of rules:
//! `Vrm0Meta::to_meta` and `imported_provenance` on the way in (the licence table, both 0.x URLs, unknown keys), and
//! `Vrm0Meta::from_meta` on the way out (which licence name to write, with a finding for every term 0.x cannot say, and
//! the unknown keys back). This module only parses the JSON, finds the thumbnail's texture and serialises the result.

use omuzaki_gltf::schema::Document;
use omuzaki_gltf::to_json;
use omuzaki_io_api::{Finding, IoError, IoScene, Report, Vrm0Meta};
use serde_json::Value;

/// Reads the 0.x meta into `scene.meta` (1.0 vocabulary) and `scene.provenance` (derived from `licenseName`, which
/// the 1.0 shape has already lost). `UniVRM` writes `-1` for "no thumbnail", which the typed meta can't hold.
pub(super) fn read(
    raw: &Value,
    file: Option<&str>,
    scene: &mut IoScene,
    at: &str,
    report: &mut Report,
) -> Result<(), IoError> {
    let mut body = raw.clone();
    if let Some(obj) = body.as_object_mut()
        && obj.get("texture").and_then(Value::as_i64).is_some_and(|t| t < 0)
    {
        obj.remove("texture");
    }
    let v0: Vrm0Meta = if body.is_null() {
        report.push(
            Finding::warning("vrm0.meta_missing", "the file has no meta; its terms are unknown")
                .at(format!("{at}/meta")),
        );
        Vrm0Meta::default()
    } else {
        serde_json::from_value(body).map_err(|e| IoError::Malformed {
            message: format!("VRM.meta is invalid: {e}"),
            at: Some(format!("{at}/meta")),
        })?
    };
    let thumbnail = v0.texture.and_then(|t| scene.textures.get(t as usize)).map(|t| t.image);
    let asset_copyright = scene.meta.copyright_information.take();
    scene.meta = v0.to_meta(thumbnail);
    scene.meta.copyright_information = asset_copyright;
    scene.provenance = Some(v0.imported_provenance(file));
    Ok(())
}

/// The 0.x `meta` object for `scene`, with a finding for everything it cannot say exactly.
pub(super) fn write(scene: &IoScene, document: &mut Document, report: &mut Report) -> Value {
    let (mut v0, findings) = Vrm0Meta::from_meta(&scene.meta, scene.provenance.as_ref());
    report.findings.extend(findings);
    v0.texture = scene.meta.thumbnail_image.and_then(|i| u32::try_from(document.texture_for_image(i.index())).ok());
    to_json(&v0)
}
