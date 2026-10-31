//! The linked texture painter (#2575): what turns a part's texture layer, a painting the character keeps as JSON, into
//! the pixels the build lays over the part's materials.

use omuzaki_char_api::PaintedTexture;
use omuzaki_paint::{Painting, render};
use serde::Deserialize as _;

/// Paints the painting `value` holds: premultiplied sRGB RGBA8, rows top to bottom, what the build's texture wants.
///
/// # Errors
/// Why the value isn't a painting, or why the painting doesn't render.
pub fn paint_texture(value: &serde_json::Value) -> Result<PaintedTexture, String> {
    let painting = Painting::deserialize(value).map_err(|e| format!("not a painting: {e}"))?;
    let canvas = render(&painting).map_err(|e| e.to_string())?;
    let mut rgba8 = Vec::with_capacity(canvas.pixels().len() * 4);
    for px in canvas.pixels() {
        // The canvas holds premultiplied sRGB-encoded floats, the texture's own encoding.
        rgba8.extend(px.iter().map(|v| unit_to_u8(*v)));
    }
    Ok(PaintedTexture { width: canvas.width(), height: canvas.height(), rgba8 })
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to 0..=255 first
fn unit_to_u8(v: f32) -> u8 {
    (v * 255.0).round().clamp(0.0, 255.0) as u8
}

omuzaki_char_api::register_texture_painter!(paint_texture);

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_painting_comes_back_as_premultiplied_pixels() {
        let painting = json!({
            "name": "stripe", "width": 8, "height": 4, "order": ["paint"],
            "layers": { "paint": { "name": "paint", "marks": [
                { "type": "shape", "geometry": { "kind": "rect", "x": 0, "y": 0, "w": 4, "h": 4 }, "fill": "#ff000080" }
            ] } }
        });
        let t = paint_texture(&painting).unwrap();
        assert_eq!((t.width, t.height, t.rgba8.len()), (8, 4, 8 * 4 * 4));
        let left = &t.rgba8[..4];
        assert!(left[3] > 120 && left[3] < 136, "half covered: {left:?}");
        assert_eq!(left[0], left[3], "premultiplied red equals its alpha");
        assert_eq!(&t.rgba8[7 * 4..8 * 4], &[0, 0, 0, 0], "the right half is clear");
        assert!(paint_texture(&json!({"width": 0})).unwrap_err().starts_with("not a painting"));
    }
}
