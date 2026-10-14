use super::super::path::Seg;
use super::*;

/// The reference: tiny-skia's `PathStroker`, the port of Skia's this one is a port of.
fn reference(els: &[PathEl], width: f32, cap: Cap, join: Join, res_scale: f32) -> Option<Vec<Seg>> {
    use tiny_skia as ts;
    let mut pb = ts::PathBuilder::new();
    for el in els {
        match *el {
            PathEl::Move(p) => pb.move_to(p.x, p.y),
            PathEl::Line(p) => pb.line_to(p.x, p.y),
            PathEl::Quad(c, p) => pb.quad_to(c.x, c.y, p.x, p.y),
            PathEl::Cubic(a, b, p) => pb.cubic_to(a.x, a.y, b.x, b.y, p.x, p.y),
            PathEl::Close => pb.close(),
        }
    }
    let stroke = ts::Stroke {
        width,
        miter_limit: 10.0,
        line_cap: match cap {
            Cap::Butt => ts::LineCap::Butt,
            Cap::Round => ts::LineCap::Round,
        },
        line_join: match join {
            Join::Miter => ts::LineJoin::Miter,
            Join::Round => ts::LineJoin::Round,
        },
        dash: None,
    };
    let outline = pb.finish()?.stroke(&stroke, res_scale)?;
    let v = |p: ts::Point| Vec2::new(p.x, p.y);
    Some(
        outline
            .segments()
            .map(|s| match s {
                ts::PathSegment::MoveTo(p) => Seg::Move(v(p)),
                ts::PathSegment::LineTo(p) => Seg::Line(v(p)),
                ts::PathSegment::QuadTo(c, p) => Seg::Quad(v(c), v(p)),
                ts::PathSegment::CubicTo(a, b, p) => Seg::Cubic(v(a), v(b), v(p)),
                ts::PathSegment::Close => Seg::Close,
            })
            .collect(),
    )
}

/// Our outline with every conic replaced by quads the way tiny-skia does it at stroke time (at least two quads),
/// so the two can be compared segment by segment.
fn with_conics_as_quads(ours: &Outline) -> Vec<Seg> {
    let mut out = Vec::new();
    let mut pen = Vec2::ZERO;
    for seg in ours.segments() {
        match seg {
            Seg::Move(p) | Seg::Line(p) | Seg::Quad(_, p) | Seg::Cubic(_, _, p) => {
                pen = p;
                out.push(seg);
            }
            Seg::Conic(c, p, w) => {
                let conic = super::super::geom::Conic { points: [pen, c, p], weight: w };
                let mut quads = [Vec2::ZERO; 33];
                let n = conic.quads_at_least(1, &mut quads);
                out.extend((0..n).map(|i| Seg::Quad(quads[2 * i + 1], quads[2 * i + 2])));
                pen = p;
            }
            Seg::Close => out.push(seg),
        }
    }
    out
}

fn close(a: &[Seg], b: &[Seg]) -> Result<(), String> {
    if a.len() != b.len() {
        return Err(format!("{} segments against {}:\n{a:?}\n{b:?}", a.len(), b.len()));
    }
    let near = |p: Vec2, q: Vec2| (p - q).abs().max_element() <= 1e-3 * (1.0 + p.abs().max_element());
    for (i, (x, y)) in a.iter().zip(b).enumerate() {
        let ok = match (x, y) {
            (Seg::Move(p), Seg::Move(q)) | (Seg::Line(p), Seg::Line(q)) => near(*p, *q),
            (Seg::Quad(c, p), Seg::Quad(d, q)) => near(*c, *d) && near(*p, *q),
            (Seg::Cubic(a, b, p), Seg::Cubic(c, d, q)) => near(*a, *c) && near(*b, *d) && near(*p, *q),
            (Seg::Close, Seg::Close) => true,
            _ => false,
        };
        if !ok {
            return Err(format!("segment {i} differs: {x:?} against {y:?}"));
        }
    }
    Ok(())
}

fn v(x: f32, y: f32) -> Vec2 {
    Vec2::new(x, y)
}

/// Paths of every kind a face draws, in pixel-ish units so the resolution scale is realistic.
fn paths() -> Vec<Vec<PathEl>> {
    vec![
        vec![PathEl::Move(v(10.0, 10.0)), PathEl::Line(v(80.0, 12.0))],
        vec![PathEl::Move(v(10.0, 10.0)), PathEl::Line(v(80.0, 12.0)), PathEl::Line(v(20.0, 60.0))],
        vec![
            PathEl::Move(v(10.0, 10.0)),
            PathEl::Line(v(60.0, 10.0)),
            PathEl::Line(v(60.0, 50.0)),
            PathEl::Line(v(10.0, 50.0)),
            PathEl::Close,
        ],
        vec![PathEl::Move(v(10.0, 10.0)), PathEl::Line(v(90.0, 20.0)), PathEl::Line(v(12.0, 22.0))],
        vec![PathEl::Move(v(10.0, 40.0)), PathEl::Quad(v(50.0, -10.0), v(90.0, 40.0))],
        vec![PathEl::Move(v(10.0, 40.0)), PathEl::Quad(v(50.0, 40.0), v(90.0, 40.0))],
        vec![PathEl::Move(v(10.0, 40.0)), PathEl::Quad(v(100.0, 40.0), v(30.0, 40.0))],
        vec![PathEl::Move(v(10.0, 40.0)), PathEl::Cubic(v(30.0, -20.0), v(70.0, 100.0), v(90.0, 40.0))],
        vec![PathEl::Move(v(10.0, 40.0)), PathEl::Cubic(v(120.0, 10.0), v(-20.0, 10.0), v(90.0, 40.0))],
        vec![PathEl::Move(v(10.0, 40.0)), PathEl::Cubic(v(60.0, 10.0), v(40.0, 10.0), v(90.0, 40.0)), PathEl::Close],
        vec![PathEl::Move(v(10.0, 40.0)), PathEl::Cubic(v(30.0, 40.0), v(70.0, 40.0), v(90.0, 40.0))],
        vec![PathEl::Move(v(10.0, 40.0)), PathEl::Cubic(v(10.0, 40.0), v(60.0, 80.0), v(90.0, 40.0))],
        vec![
            PathEl::Move(v(10.0, 40.0)),
            PathEl::Cubic(v(30.0, -30.0), v(50.0, 110.0), v(70.0, 10.0)),
            PathEl::Line(v(95.0, 60.0)),
            PathEl::Quad(v(60.0, 90.0), v(20.0, 70.0)),
        ],
        vec![PathEl::Move(v(30.0, 30.0)), PathEl::Line(v(30.0, 30.0))],
        vec![PathEl::Move(v(30.0, 30.0)), PathEl::Close],
        vec![
            PathEl::Move(v(10.0, 10.0)),
            PathEl::Line(v(40.0, 10.0)),
            PathEl::Move(v(10.0, 30.0)),
            PathEl::Line(v(40.0, 35.0)),
        ],
    ]
}

#[test]
fn it_strokes_like_skia_with_miter_joins_and_butt_caps() {
    for res_scale in [1.0, 3.6] {
        for (i, els) in paths().iter().enumerate() {
            let mut stroker = Stroker::default();
            stroker.stroke(els, 7.0, Cap::Butt, Join::Miter, 10.0, res_scale);
            // A near-reversing curve is joined with a round join, whose conics become quads for the comparison.
            let ours = with_conics_as_quads(stroker.outline());
            let theirs = reference(els, 7.0, Cap::Butt, Join::Miter, res_scale).unwrap_or_default();
            close(&ours, &theirs).unwrap_or_else(|e| panic!("path {i} at scale {res_scale}: {e}"));
        }
    }
}

#[test]
fn it_strokes_like_skia_with_round_caps_and_joins() {
    for (i, els) in paths().iter().enumerate() {
        let mut stroker = Stroker::default();
        stroker.stroke(els, 9.0, Cap::Round, Join::Round, 10.0, 2.0);
        let ours = with_conics_as_quads(stroker.outline());
        let theirs = reference(els, 9.0, Cap::Round, Join::Round, 2.0).unwrap_or_default();
        close(&ours, &theirs).unwrap_or_else(|e| panic!("path {i}: {e}"));
    }
}

#[test]
fn it_strokes_like_skia_with_round_caps_and_miter_joins_at_many_widths() {
    for width in [0.5, 2.0, 20.0, 60.0] {
        for (i, els) in paths().iter().enumerate() {
            let mut stroker = Stroker::default();
            stroker.stroke(els, width, Cap::Round, Join::Miter, 10.0, 1.0);
            let ours = with_conics_as_quads(stroker.outline());
            let theirs = reference(els, width, Cap::Round, Join::Miter, 1.0).unwrap_or_default();
            close(&ours, &theirs).unwrap_or_else(|e| panic!("path {i} at width {width}: {e}"));
        }
    }
}

#[test]
fn a_straight_stroke_is_a_rectangle() {
    let mut stroker = Stroker::default();
    let els = [PathEl::Move(v(0.0, 0.0)), PathEl::Line(v(10.0, 0.0))];
    stroker.stroke(&els, 4.0, Cap::Butt, Join::Miter, 10.0, 1.0);
    let corners: Vec<Vec2> = stroker
        .outline()
        .segments()
        .into_iter()
        .filter_map(|s| match s {
            Seg::Move(p) | Seg::Line(p) => Some(p),
            _ => None,
        })
        .collect();
    // The outer edge is to the left of the direction of travel (y is down here), then the inner edge back, closed.
    assert_eq!(corners, [v(0.0, -2.0), v(10.0, -2.0), v(10.0, 2.0), v(0.0, 2.0), v(0.0, -2.0)]);
}
