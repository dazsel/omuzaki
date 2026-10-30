//! From marks to pixels: each paint layer's marks are drawn in order on a canvas of its own, its mask applied, and the
//! layers composited back to front with their blend modes and opacity. Clipped layers composite onto the layer under
//! them keeping its alpha (a clipping group, as in Photoshop or Clip Studio); groups composite their layers first
//! (isolated) or pass them through to what is under the group.

use std::collections::BTreeMap;

use kurbo::{Cap, Join, StrokeOpts};

use crate::blend::{BlendMode, Keep};
use crate::brush::{Brush, StrokeInput, default_blend, preset, smudge, stroke_mask};
use crate::bucket::{FloodOptions, flood};
use crate::canvas::{Canvas, Mask};
use crate::error::PaintError;
use crate::geom::Fill;
use crate::hatch::{HatchLines, hatch_lines};
use crate::model::{Layer, LayerKind, Mark, Painting};
use crate::raster::{FLATTEN_TOLERANCE, FillRule, feather, fill_path, polygons};

/// The finished picture.
///
/// # Errors
/// The first problem [`Painting::check`] finds.
pub fn render(p: &Painting) -> Result<Canvas, PaintError> {
    p.check()?;
    let pixels = layer_pixels(p)?;
    let mut out = Canvas::filled(p.width, p.height, p.background)?;
    composite_children(p, None, &pixels, &mut out)?;
    Ok(out)
}

/// Each paint layer's own pixels (its marks and mask; not its opacity, blend or clipping), by id.
///
/// # Errors
/// The first problem [`Painting::check`] finds.
pub fn layer_pixels(p: &Painting) -> Result<BTreeMap<String, Canvas>, PaintError> {
    p.check()?;
    let mut done: BTreeMap<String, Canvas> = BTreeMap::new();
    for id in p.sample_order()? {
        let layer = p.layer(id)?;
        let canvas = draw_layer(p, layer, &done)?;
        done.insert(id.to_owned(), canvas);
    }
    Ok(done)
}

/// One layer alone: its marks drawn in order, then its mask.
fn draw_layer(p: &Painting, layer: &Layer, done: &BTreeMap<String, Canvas>) -> Result<Canvas, PaintError> {
    let mut canvas = Canvas::new(p.width, p.height)?;
    for m in &layer.marks {
        let keep = if m.alpha_lock() { Keep::Atop } else { Keep::Over };
        draw_mark(&mut canvas, m, keep, done)?;
    }
    if let Some(marks) = &layer.mask {
        let mut m = Canvas::new(p.width, p.height)?;
        for mark in marks {
            draw_mark(&mut m, mark, Keep::Over, done)?;
        }
        let mut mask = Mask::new(m.bounds());
        for (v, px) in mask.values.iter_mut().zip(m.pixels()) {
            *v = px[3];
        }
        canvas.apply_mask(&mask);
    }
    Ok(canvas)
}

/// Draws one mark onto `canvas`. `done` holds the layers a bucket fill may sample.
///
/// # Errors
/// [`PaintError::Invalid`] when the mark can't be drawn (it should have been checked), or a sampled layer is missing.
pub fn draw_mark(
    canvas: &mut Canvas,
    mark: &Mark,
    keep: Keep,
    done: &BTreeMap<String, Canvas>,
) -> Result<(), PaintError> {
    let clip = canvas.bounds();
    #[allow(clippy::cast_possible_truncation)] // opacities are 0..1
    let opacity = |o: f64| o as f32;
    match mark {
        Mark::Stroke(s) => {
            let brush = s.brush("stroke")?;
            let points = s.points("stroke")?;
            let input = StrokeInput {
                points: &points,
                brush: &brush,
                size: s.size,
                smooth: s.smooth,
                closed: s.closed,
                seed: s.seed,
            };
            if brush.smudge > 0.0 && !s.erase {
                smudge(canvas, &input, s.opacity, keep);
                return Ok(());
            }
            let mask = stroke_mask(&input, clip);
            let mode = if s.erase { None } else { s.blend.or_else(|| default_blend(&brush)) };
            canvas.paint_mask(&mask, s.colour.premul(), opacity(s.opacity), mode, keep);
        }
        Mark::Shape(s) => {
            let path = s.geometry.to_path("geometry")?;
            let mut mask = fill_path(&path, s.fill_rule, clip);
            if s.feather > 0.0 {
                mask = feather(&mask, s.feather, clip);
            }
            lay(canvas, &mask, &s.fill, opacity(s.opacity), s.erase, s.blend, keep);
            if let Some(o) = &s.outline {
                let style = kurbo::Stroke::new(o.width).with_caps(Cap::Round).with_join(Join::Round);
                let line = kurbo::stroke(path.iter(), &style, &StrokeOpts::default(), FLATTEN_TOLERANCE);
                let line_mask = fill_path(&line, FillRule::Nonzero, clip);
                lay(canvas, &line_mask, &Fill::Colour(o.colour), opacity(s.opacity), s.erase, s.blend, keep);
            }
        }
        Mark::Bucket(b) => {
            let source = if b.sample.is_empty() {
                canvas.clone()
            } else {
                let mut src = Canvas::new(canvas.width(), canvas.height())?;
                for id in &b.sample {
                    let layer = done.get(id).ok_or_else(|| PaintError::NoLayer(id.clone()))?;
                    src.composite_from(layer, 1.0, BlendMode::Normal, Keep::Over)?;
                }
                src
            };
            let options = FloodOptions { tolerance: b.tolerance, gap: b.gap, grow: b.grow };
            let mut mask = flood(&source, b.at[0], b.at[1], options);
            if b.feather > 0.0 {
                mask = feather(&mask, b.feather, clip);
            }
            lay(canvas, &mask, &b.fill, opacity(b.opacity), false, b.blend, keep);
        }
        Mark::Hatch(h) => {
            let region = h.region.to_path("region")?;
            let polys = polygons(&region);
            let lines = hatch_lines(
                &polys,
                HatchLines {
                    angle: h.angle,
                    spacing: h.spacing,
                    taper: h.taper,
                    inset: h.inset,
                    jitter: h.jitter,
                    seed: h.seed,
                },
            );
            let base = preset(&h.brush)?;
            // The hatch's own taper comes through the points' pressure, so the brush's length tapers are off.
            let brush = Brush { size_pressure: 1.0, taper_start: 0.0, taper_end: 0.0, ..base };
            let mut all = Mask::new(clip);
            for (i, pts) in lines.iter().enumerate() {
                let input = StrokeInput {
                    points: pts,
                    brush: &brush,
                    size: h.width,
                    smooth: false,
                    closed: false,
                    seed: h.seed.wrapping_add(i as u64),
                };
                let m = stroke_mask(&input, clip);
                for y in m.rect.y0..m.rect.y1 {
                    for x in m.rect.x0..m.rect.x1 {
                        let v = m.get(x, y);
                        if let Some(a) = all.at_mut(x, y) {
                            *a = a.max(v);
                        }
                    }
                }
            }
            lay(canvas, &all, &Fill::Colour(h.colour), opacity(h.opacity), false, h.blend, keep);
        }
    }
    Ok(())
}

/// Lays `fill` down through `mask`, or erases through it.
fn lay(canvas: &mut Canvas, mask: &Mask, fill: &Fill, opacity: f32, erase: bool, blend: Option<BlendMode>, keep: Keep) {
    let mode = blend.unwrap_or_default();
    if erase {
        canvas.paint_mask(mask, [0.0; 4], opacity, None, keep);
        return;
    }
    match fill {
        Fill::Colour(c) => canvas.paint_mask(mask, c.premul(), opacity, Some(mode), keep),
        Fill::Gradient(g) => canvas.paint_mask_with(mask, &|x, y| g.at(x, y), opacity, mode, keep),
    }
}

/// Composites the children of `parent` (the top level when `None`) onto `dst`, back to front.
fn composite_children(
    p: &Painting,
    parent: Option<&str>,
    pixels: &BTreeMap<String, Canvas>,
    dst: &mut Canvas,
) -> Result<(), PaintError> {
    // The open clipping group: its base's pixels (with clipped layers on them) and the base itself.
    let mut group: Option<(Canvas, &Layer)> = None;
    for id in p.children(parent) {
        let layer = p.layer(id)?;
        if layer.clip && layer.kind == LayerKind::Paint {
            if let Some((base, base_layer)) = group.as_mut()
                && base_layer.visible
                && layer.visible
            {
                let mut own = pixels.get(id).ok_or_else(|| PaintError::NoLayer(id.to_owned()))?.clone();
                own.blur(layer.blur);
                #[allow(clippy::cast_possible_truncation)] // 0..1
                base.composite_from(&own, layer.opacity as f32, layer.blend.unwrap_or_default(), Keep::Atop)?;
            }
            // A clipped layer with no base under it shows nothing, as in Photoshop.
            continue;
        }
        if let Some((base, base_layer)) = group.take() {
            flush(dst, &base, base_layer)?;
        }
        match layer.kind {
            LayerKind::Paint => {
                let mut own = pixels.get(id).ok_or_else(|| PaintError::NoLayer(id.to_owned()))?.clone();
                own.blur(layer.blur);
                group = Some((own, layer));
            }
            LayerKind::Group => {
                if !layer.visible {
                    continue;
                }
                match layer.blend {
                    None => {
                        // Pass-through: the group's layers land straight on what is under it, faded as a whole.
                        let mut through = dst.clone();
                        composite_children(p, Some(id), pixels, &mut through)?;
                        #[allow(clippy::cast_possible_truncation)] // 0..1
                        dst.lerp_to(&through, layer.opacity as f32)?;
                    }
                    Some(mode) => {
                        let mut isolated = Canvas::new(p.width, p.height)?;
                        composite_children(p, Some(id), pixels, &mut isolated)?;
                        isolated.blur(layer.blur);
                        #[allow(clippy::cast_possible_truncation)] // 0..1
                        dst.composite_from(&isolated, layer.opacity as f32, mode, Keep::Over)?;
                    }
                }
            }
        }
    }
    if let Some((base, base_layer)) = group.take() {
        flush(dst, &base, base_layer)?;
    }
    Ok(())
}

fn flush(dst: &mut Canvas, base: &Canvas, layer: &Layer) -> Result<(), PaintError> {
    if !layer.visible {
        return Ok(());
    }
    #[allow(clippy::cast_possible_truncation)] // 0..1
    dst.composite_from(base, layer.opacity as f32, layer.blend.unwrap_or_default(), Keep::Over)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::colour::Colour;
    use crate::geom::Geometry;
    use crate::model::{Bucket, Outline, ShapeMark, Stroke};

    fn rect(x: f64, y: f64, w: f64, h: f64, fill: &str) -> Mark {
        Mark::Shape(ShapeMark {
            geometry: Geometry::Rect { x, y, w, h, radius: 0.0 },
            fill: Fill::Colour(fill.parse().unwrap()),
            fill_rule: FillRule::Nonzero,
            feather: 0.0,
            outline: None,
            opacity: 1.0,
            blend: None,
            erase: false,
            alpha_lock: false,
            tag: None,
        })
    }

    fn painting(layers: Vec<(&str, Layer)>) -> Painting {
        let mut p = Painting::new("t", 40, 40);
        for (id, l) in layers {
            p.order.push(id.into());
            p.layers.insert(id.into(), l);
        }
        p
    }

    fn with(mut l: Layer, marks: Vec<Mark>) -> Layer {
        l.marks = marks;
        l
    }

    fn px(c: &Canvas, x: i32, y: i32) -> Colour {
        Colour::from_premul(c.get(x, y))
    }

    #[test]
    fn layers_stack_back_to_front() {
        let p = painting(vec![
            ("a", with(Layer::paint("a"), vec![rect(0.0, 0.0, 20.0, 20.0, "#ff0000")])),
            ("b", with(Layer::paint("b"), vec![rect(10.0, 10.0, 20.0, 20.0, "#0000ff")])),
        ]);
        let c = render(&p).unwrap();
        assert_eq!(px(&c, 5, 5), Colour::rgb(255, 0, 0));
        assert_eq!(px(&c, 15, 15), Colour::rgb(0, 0, 255));
        assert_eq!(px(&c, 35, 5), Colour::CLEAR);
    }

    #[test]
    fn a_clipped_layer_only_shows_on_its_base() {
        let mut shade = with(Layer::paint("shade"), vec![rect(0.0, 0.0, 40.0, 40.0, "#000000")]);
        shade.clip = true;
        shade.blend = Some(BlendMode::Multiply);
        shade.opacity = 0.5;
        let p = painting(vec![
            ("base", with(Layer::paint("base"), vec![rect(10.0, 10.0, 20.0, 20.0, "#ffffff")])),
            ("shade", shade),
        ]);
        let c = render(&p).unwrap();
        assert_eq!(px(&c, 5, 5), Colour::CLEAR);
        assert_eq!(px(&c, 20, 20), Colour::rgb(128, 128, 128));
    }

    #[test]
    fn alpha_lock_paints_only_where_there_is_paint() {
        let mut over = rect(0.0, 0.0, 40.0, 40.0, "#00ff00");
        if let Mark::Shape(s) = &mut over {
            s.alpha_lock = true;
        }
        let l = with(Layer::paint("l"), vec![rect(10.0, 10.0, 10.0, 10.0, "#ffffff"), over]);
        let c = render(&painting(vec![("l", l)])).unwrap();
        assert_eq!(px(&c, 15, 15), Colour::rgb(0, 255, 0));
        assert_eq!(px(&c, 5, 5), Colour::CLEAR);
    }

    #[test]
    fn a_mask_hides_the_layer_outside_it() {
        let mut l = with(Layer::paint("l"), vec![rect(0.0, 0.0, 40.0, 40.0, "#ff0000")]);
        l.mask = Some(vec![rect(0.0, 0.0, 20.0, 40.0, "#ffffff")]);
        let c = render(&painting(vec![("l", l)])).unwrap();
        assert_eq!(px(&c, 10, 10), Colour::rgb(255, 0, 0));
        assert_eq!(px(&c, 30, 10), Colour::CLEAR);
    }

    #[test]
    fn a_bucket_fills_inside_line_art_on_another_layer() {
        let lines = with(
            Layer::paint("lines"),
            vec![Mark::Shape(ShapeMark {
                geometry: Geometry::Ellipse { cx: 20.0, cy: 20.0, rx: 14.0, ry: 14.0, angle: 0.0 },
                fill: Fill::Colour(Colour::CLEAR),
                fill_rule: FillRule::Nonzero,
                feather: 0.0,
                outline: Some(Outline { colour: Colour::BLACK, width: 2.0 }),
                opacity: 1.0,
                blend: None,
                erase: false,
                alpha_lock: false,
                tag: None,
            })],
        );
        let flats = with(
            Layer::paint("flats"),
            vec![Mark::Bucket(Bucket {
                at: [20.0, 20.0],
                fill: Fill::Colour("#f3d9c8".parse().unwrap()),
                tolerance: 24.0,
                gap: 0,
                grow: 1,
                sample: vec!["lines".into()],
                feather: 0.0,
                opacity: 1.0,
                blend: None,
                alpha_lock: false,
                tag: None,
            })],
        );
        let c = render(&painting(vec![("flats", flats), ("lines", lines)])).unwrap();
        assert_eq!(px(&c, 20, 20), "#f3d9c8".parse().unwrap());
        assert_eq!(px(&c, 2, 2), Colour::CLEAR);
        assert_eq!(px(&c, 20, 6).a, 255, "the line itself");
    }

    #[test]
    fn an_isolated_group_composites_as_one() {
        let mut g = Layer::paint("g");
        g.kind = LayerKind::Group;
        g.blend = Some(BlendMode::Normal);
        g.opacity = 0.5;
        let mut a = with(Layer::paint("a"), vec![rect(0.0, 0.0, 40.0, 40.0, "#ff0000")]);
        a.parent = Some("g".into());
        let mut b = with(Layer::paint("b"), vec![rect(0.0, 0.0, 40.0, 40.0, "#0000ff")]);
        b.parent = Some("g".into());
        let c = render(&painting(vec![("g", g), ("a", a), ("b", b)])).unwrap();
        // Isolated: blue fully covers red inside, then the whole is faded, so no red shows through.
        let got = px(&c, 5, 5);
        assert_eq!((got.r, got.b, got.a), (0, 255, 128));
    }

    #[test]
    fn a_stroke_and_an_eraser() {
        let ink = |erase: bool, y: f64| {
            Mark::Stroke(Stroke {
                brush: "hard_round".into(),
                settings: None,
                colour: Colour::BLACK,
                size: 6.0,
                opacity: 1.0,
                points: vec![vec![5.0, y], vec![35.0, y]],
                smooth: true,
                closed: false,
                seed: 0,
                blend: None,
                erase,
                alpha_lock: false,
                tag: None,
            })
        };
        let l = with(Layer::paint("l"), vec![rect(0.0, 0.0, 40.0, 40.0, "#ffffff"), ink(false, 10.0), ink(true, 30.0)]);
        let c = render(&painting(vec![("l", l)])).unwrap();
        assert_eq!(px(&c, 20, 10), Colour::BLACK);
        assert_eq!(px(&c, 20, 30), Colour::CLEAR);
        assert_eq!(px(&c, 20, 20), Colour::WHITE);
    }

    #[test]
    fn a_blurred_layer_softens_its_edge_and_keeps_its_paint() {
        let mut l = with(Layer::paint("l"), vec![rect(10.0, 10.0, 20.0, 20.0, "#ffffff")]);
        let sharp = render(&painting(vec![("l", l.clone())])).unwrap();
        l.blur = 2.0;
        let soft = render(&painting(vec![("l", l)])).unwrap();
        let alpha = |c: &Canvas| c.pixels().iter().map(|p| f64::from(p[3])).sum::<f64>();
        assert!((alpha(&soft) - alpha(&sharp)).abs() < 1.0);
        let edge = soft.get(10, 20)[3];
        assert!(edge > 0.3 && edge < 0.7, "{edge}");
        assert!(soft.get(8, 20)[3] > 0.0 && sharp.get(8, 20)[3] == 0.0);
    }

    #[test]
    fn rendering_is_deterministic() {
        let l = with(Layer::paint("l"), vec![rect(3.3, 4.4, 20.0, 9.9, "#336699")]);
        let p = painting(vec![("l", l)]);
        assert_eq!(render(&p).unwrap(), render(&p).unwrap());
    }
}
