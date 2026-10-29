//! Rigs layered art and renders a sheet of poses: `cargo run --release -p omuzaki-puppet --example rig -- ART.layers.toml
//! OUT_DIR`. Writes the puppet bundle to `OUT_DIR/<name>.puppet/` and the sheet to `OUT_DIR/sheet.png`.

#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, reason = "a demo's image sizes")]

use std::path::PathBuf;

use omuzaki_core::glam::{Affine2, Vec2};
use omuzaki_puppet::autorig::autorig;
use omuzaki_puppet::layers::LayeredArt;
use omuzaki_puppet::{Canvas, CpuRenderer, Evaluator, bundle};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (Some(manifest), Some(out)) = (args.next(), args.next()) else {
        return Err("usage: rig ART.layers.toml OUT_DIR".into());
    };
    let out = PathBuf::from(out);
    let art = LayeredArt::load(&PathBuf::from(manifest))?;
    let started = std::time::Instant::now();
    let rigged = autorig(&art)?;
    println!(
        "rigged in {:.2} s: {} nodes, {} params",
        started.elapsed().as_secs_f32(),
        rigged.def.nodes.len(),
        rigged.def.params.len()
    );
    for note in &rigged.notes {
        println!("  {note}");
    }
    let dir = out.join(format!("{}.puppet", rigged.def.meta.name));
    bundle::save(&dir, &rigged.def, &rigged.mapping, &rigged.images)?;
    let loaded = bundle::load(&dir)?;
    let poses: &[(&str, &[(&str, f32)])] = &[
        ("rest", &[]),
        ("AngleX -30", &[("ParamAngleX", -30.0)]),
        ("AngleX +30", &[("ParamAngleX", 30.0)]),
        ("AngleY +30", &[("ParamAngleY", 30.0)]),
        ("AngleY -30", &[("ParamAngleY", -30.0)]),
        ("AngleZ +20", &[("ParamAngleZ", 20.0)]),
        ("blink", &[("ParamEyeLOpen", 0.0), ("ParamEyeROpen", 0.0)]),
        ("half blink", &[("ParamEyeLOpen", 0.4), ("ParamEyeROpen", 0.4)]),
        ("gaze", &[("ParamEyeBallX", 1.0), ("ParamEyeBallY", 0.6)]),
        ("talk", &[("ParamMouthOpenY", 1.0), ("ParamMouthForm", 1.0)]),
        (
            "angry",
            &[("ParamBrowLAngle", 1.0), ("ParamBrowRAngle", 1.0), ("Expr custom00", 1.0), ("ParamMouthOpenY", 0.6)],
        ),
        ("squeeze", &[("Expr eyesSqueeze", 1.0), ("ParamMouthOpenY", 0.8)]),
        ("shock", &[("Expr eyesShock", 1.0), ("ParamBrowLY", 1.0), ("ParamBrowRY", 1.0)]),
        ("gloom", &[("Expr eyesGloom", 1.0), ("ParamAngleY", -15.0)]),
        ("body", &[("ParamBodyAngleX", 10.0), ("ParamBodyAngleZ", 8.0), ("ParamAngleX", 20.0)]),
        ("hair swing", &[("ParamHairFront", 1.0), ("ParamHairSide", 1.0), ("ParamHairBack", -1.0)]),
    ];
    let (tw, th) = (384u32, 576u32);
    let cols = 4u32;
    let rows = (poses.len() as u32).div_ceil(cols);
    let mut sheet = image::RgbaImage::from_pixel(tw * cols, th * rows, image::Rgba([24, 24, 28, 255]));
    let mut renderer = CpuRenderer::default();
    let mut eval = Evaluator::new(loaded.def.clone())?;
    let mut render_s = 0.0;
    for (i, (label, values)) in poses.iter().enumerate() {
        eval.reset();
        for (name, v) in *values {
            match eval.def().param(name) {
                Some(p) => eval.set(p, [*v, 0.0]),
                None => println!("  {label}: no parameter {name}"),
            }
        }
        eval.evaluate();
        let mut canvas = Canvas::new(tw, th);
        canvas.fill([24.0 / 255.0, 24.0 / 255.0, 28.0 / 255.0, 1.0]);
        let scale = th as f32 / loaded.def.canvas[1];
        let view =
            Affine2::from_scale_angle_translation(Vec2::splat(scale), 0.0, Vec2::new(tw as f32, th as f32) * 0.5);
        let t = std::time::Instant::now();
        renderer.draw(eval.draw_list(), &loaded.textures, view, &mut canvas);
        render_s += t.elapsed().as_secs_f64();
        let img = image::RgbaImage::from_raw(tw, th, canvas.to_straight_rgba8()).ok_or("canvas size")?;
        let (x, y) = ((i as u32 % cols) * tw, (i as u32 / cols) * th);
        image::imageops::overlay(&mut sheet, &img, i64::from(x), i64::from(y));
        println!("  {label}");
    }
    println!("rendered {} poses at {tw}x{th} in {:.1} ms each", poses.len(), render_s * 1000.0 / poses.len() as f64);
    // A full-size frame for timing.
    let (fw, fh) = (1080u32, 1920u32);
    let mut canvas = Canvas::new(fw, fh);
    let scale = fh as f32 / loaded.def.canvas[1];
    let view = Affine2::from_scale_angle_translation(Vec2::splat(scale), 0.0, Vec2::new(fw as f32, fh as f32) * 0.5);
    eval.reset();
    eval.evaluate();
    let t = std::time::Instant::now();
    for _ in 0..5 {
        canvas.fill([0.0, 0.0, 0.0, 1.0]);
        renderer.draw(eval.draw_list(), &loaded.textures, view, &mut canvas);
    }
    println!("a 1080x1920 frame: {:.1} ms", t.elapsed().as_secs_f64() * 1000.0 / 5.0);
    let img = image::RgbaImage::from_raw(fw, fh, canvas.to_straight_rgba8()).ok_or("canvas size")?;
    img.save(out.join("full.png"))?;
    sheet.save(out.join("sheet.png"))?;
    println!("wrote {}", out.join("sheet.png").display());
    Ok(())
}
