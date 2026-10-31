//! The painter's hot paths at a 1080 by 1920 frame: a long ink stroke, a soft airbrush stroke, a large shape with a
//! feather, and a bucket fill inside line art.

#![allow(clippy::unwrap_used)] // a bench that cannot set up should stop

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_paint::{Canvas, Painting, Rect, StrokeInput, preset, render, stroke_mask};

fn line(n: usize) -> Vec<[f64; 3]> {
    #[allow(clippy::cast_precision_loss)]
    (0..n).map(|i| [100.0 + i as f64 * 8.0, 900.0 + (i as f64 * 0.3).sin() * 200.0, 1.0]).collect()
}

fn bench(c: &mut Criterion) {
    let clip = Rect { x0: 0, y0: 0, x1: 1080, y1: 1920 };
    let points = line(110);
    for name in ["ink", "airbrush", "pencil"] {
        let brush = preset(name).unwrap();
        c.bench_function(&format!("stroke {name} 900 px, 12 px wide"), |b| {
            b.iter(|| {
                stroke_mask(
                    &StrokeInput { points: &points, brush: &brush, size: 12.0, smooth: true, closed: false, seed: 1 },
                    clip,
                )
            });
        });
    }
    let painting: Painting = serde_json::from_value(serde_json::json!({
        "name": "bench", "width": 1080, "height": 1920, "order": ["lines", "flats"],
        "layers": {
            "lines": { "name": "lines", "marks": [
                { "type": "shape", "geometry": { "kind": "ellipse", "cx": 540, "cy": 800, "rx": 400, "ry": 520 },
                  "fill": "#00000000", "outline": { "colour": "#1b1420", "width": 4 } }
            ] },
            "flats": { "name": "flats", "marks": [
                { "type": "bucket", "at": [540, 800], "fill": "#f3d9c8", "sample": ["lines"] },
                { "type": "shape", "geometry": { "kind": "ellipse", "cx": 540, "cy": 600, "rx": 300, "ry": 200 },
                  "fill": "#c08080", "feather": 24, "blend": "multiply" }
            ] }
        }
    }))
    .unwrap();
    c.bench_function("render: line art, bucket fill, feathered shape at 1080x1920", |b| b.iter(|| render(&painting)));
    c.bench_function("canvas 1080x1920 to PNG", |b| {
        let canvas = Canvas::new(1080, 1920).unwrap();
        b.iter(|| canvas.to_png());
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
