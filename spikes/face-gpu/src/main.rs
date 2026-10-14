//! Spike: draws face draw lists on the GPU with Vello and with a custom coverage rasteriser, through the
//! renderer's `PaintTarget::native()` convention, and measures time, error against `SkiaRasterizer`, and memory.
//!
//!     face-gpu <auto|software> <out-dir> [frames]
#![allow(clippy::all, clippy::pedantic, unsafe_code)]

mod custom;
mod geom;
mod probes;
mod synth;
mod vello_path;

use std::fmt::Write as _;
use std::time::Instant;

use image::RgbaImage;
use omuzaki_anim_api::ChannelBuf;
use omuzaki_core::glam::UVec2;
use omuzaki_core::time::Seconds;
use omuzaki_face::raster::SkiaRasterizer;
use omuzaki_face_api::{
    DEFAULT_PX_PER_M, DrawList, ExpressionId, ExpressionSet, FaceDrawing, FaceInput, FaceLayerId, FaceLayerSpec,
    FaceMapping, FacePainter, FaceRasterizer, FaceView, Raster,
};
use omuzaki_face_mock::{MockPainter, mock_profile};
use omuzaki_render::{AdapterPreference, HeadlessOptions, NativePaintTarget, WgpuRenderer};
use omuzaki_render_api::{AlphaEncoding, ColourSpace, PaintTarget, Renderer, TextureDesc};

type R<T> = Result<T, Box<dyn std::error::Error>>;

struct Scenario {
    name: String,
    spec: FaceLayerSpec,
    px_per_m: f32,
    list: DrawList,
    mips: bool,
}

fn scenarios() -> R<Vec<Scenario>> {
    let mut out = Vec::new();
    for (name, probe) in probes::PROBES {
        let mut list = DrawList::new();
        probe(&mut list);
        out.push(Scenario { name: format!("probe/{name}"), spec: probes::spec(), px_per_m: probes::PX_PER_M, list, mips: false });
    }
    let profile = mock_profile();
    let mapping = FaceMapping::default();
    let set = ExpressionSet::starter();
    let happy = set.blend(&ChannelBuf::NEUTRAL, &[(&ExpressionId::new("happy")?, 1.0)])?;
    for (label, buf) in [("neutral", ChannelBuf::NEUTRAL), ("happy", happy)] {
        let mut painter = MockPainter::default();
        let params = mapping.apply(&buf);
        let input = FaceInput { profile: &profile, params: &params, time: Seconds(0.0), view: FaceView::default() };
        let mut drawing = FaceDrawing::new();
        painter.paint(&input, &mut drawing)?;
        let rect = omuzaki_face_api::FaceRect { x0: -0.215, x1: 0.215, y0: -0.2825, y1: 0.025 };
        for (layer, ppm) in [(FaceLayerId::Base, DEFAULT_PX_PER_M), (FaceLayerId::Brows, DEFAULT_PX_PER_M * 0.5)] {
            let spec = FaceLayerSpec::for_rect(rect, layer, ppm);
            out.push(Scenario { name: format!("mock/{label}-{layer:?}").to_lowercase(), spec, px_per_m: ppm, list: drawing[layer].clone(), mips: true });
        }
    }
    let rect = omuzaki_face_api::FaceRect { x0: -0.215, x1: 0.215, y0: -0.2825, y1: 0.025 };
    for (n, ppm, tag) in [(300, DEFAULT_PX_PER_M, "300"), (900, DEFAULT_PX_PER_M, "900"), (300, DEFAULT_PX_PER_M * 0.5, "300-half")] {
        let spec = FaceLayerSpec::for_rect(rect, FaceLayerId::Base, ppm);
        out.push(Scenario { name: format!("synth/{tag}"), spec, px_per_m: ppm, list: synth::face(n, 7), mips: true });
    }
    Ok(out)
}

fn percentile(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    v[((v.len() - 1) as f64 * p).round() as usize]
}

/// GPU time of whatever `work` submits, bracketed by two timestamps (ordered on the one queue), plus wall time.
struct Timer {
    qs: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    read: wgpu::Buffer,
    period: f32,
}

impl Timer {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Option<Self> {
        if !device.features().contains(wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS) {
            return None;
        }
        let qs = device.create_query_set(&wgpu::QuerySetDescriptor { label: None, ty: wgpu::QueryType::Timestamp, count: 2 });
        let resolve = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 16,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let read = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 16,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        Some(Self { qs, resolve, read, period: queue.get_timestamp_period() })
    }

    /// Returns (gpu_ms, wall_ms).
    fn time(&self, device: &wgpu::Device, queue: &wgpu::Queue, work: impl FnOnce()) -> (f64, f64) {
        let mut a = device.create_command_encoder(&Default::default());
        a.write_timestamp(&self.qs, 0);
        queue.submit([a.finish()]);
        let t0 = Instant::now();
        work();
        let mut b = device.create_command_encoder(&Default::default());
        b.write_timestamp(&self.qs, 1);
        b.resolve_query_set(&self.qs, 0..2, &self.resolve, 0);
        b.copy_buffer_to_buffer(&self.resolve, 0, &self.read, 0, 16);
        queue.submit([b.finish()]);
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        let wall = t0.elapsed().as_secs_f64() * 1e3;
        let slice = self.read.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        let ts: [u64; 2] = {
            let data = slice.get_mapped_range().expect("map");
            let t: &[u64] = bytemuck::cast_slice(&data);
            [t[0], t[1]]
        };
        self.read.unmap();
        (ts[1].saturating_sub(ts[0]) as f64 * f64::from(self.period) / 1e6, wall)
    }
}

fn readback(device: &wgpu::Device, queue: &wgpu::Queue, tex: &wgpu::Texture) -> Raster {
    let (w, h) = (tex.width(), tex.height());
    let bpr = (w * 4).div_ceil(256) * 256;
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(bpr * h),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo { texture: tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bpr), rows_per_image: Some(h) } },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    queue.submit([enc.finish()]);
    let slice = buf.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    let data = slice.get_mapped_range().expect("map");
    let mut r = Raster::new(w, h);
    for y in 0..h as usize {
        let src = &data[y * bpr as usize..y * bpr as usize + w as usize * 4];
        r.pixels[y * w as usize * 4..(y + 1) * w as usize * 4].copy_from_slice(src);
    }
    r
}

struct Err {
    psnr: f64,
    max: u8,
    bad_frac: f64,
}

fn compare(gpu: &Raster, cpu: &Raster) -> Err {
    let mut se = 0.0_f64;
    let mut max = 0u8;
    let mut bad = 0usize;
    for (g, c) in gpu.pixels.chunks_exact(4).zip(cpu.pixels.chunks_exact(4)) {
        let mut worst = 0u8;
        for k in 0..4 {
            let d = g[k].abs_diff(c[k]);
            se += f64::from(d) * f64::from(d);
            worst = worst.max(d);
        }
        max = max.max(worst);
        if worst > 6 {
            bad += 1;
        }
    }
    let n = gpu.pixels.len() as f64;
    let mse = se / n;
    Err { psnr: if mse == 0.0 { 99.0 } else { 10.0 * (255.0_f64 * 255.0 / mse).log10() }, max, bad_frac: bad as f64 / (n / 4.0) }
}

/// Premultiplied over mid-grey, for looking at.
fn over_grey(r: &Raster) -> RgbaImage {
    let mut img = RgbaImage::new(r.width, r.height);
    for (i, p) in r.pixels.chunks_exact(4).enumerate() {
        let inv = 255 - u32::from(p[3]);
        let c = |v: u8| (u32::from(v) + 128 * inv / 255).min(255) as u8;
        img.put_pixel((i as u32) % r.width, (i as u32) / r.width, image::Rgba([c(p[0]), c(p[1]), c(p[2]), 255]));
    }
    img
}

fn diff_image(a: &Raster, b: &Raster) -> RgbaImage {
    let mut img = RgbaImage::new(a.width, a.height);
    for (i, (g, c)) in a.pixels.chunks_exact(4).zip(b.pixels.chunks_exact(4)).enumerate() {
        let d = (0..4).map(|k| g[k].abs_diff(c[k])).max().unwrap_or(0);
        let v = 255u8.saturating_sub(d.saturating_mul(8));
        img.put_pixel((i as u32) % a.width, (i as u32) / a.width, image::Rgba([255, v, v, 255]));
    }
    img
}

fn main() -> R<()> {
    let mut args = std::env::args().skip(1);
    let which = args.next().unwrap_or_else(|| "auto".into());
    let out_dir = args.next().unwrap_or_else(|| "face-gpu-out".into());
    let frames: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(100);
    let only = args.next().unwrap_or_default();
    std::fs::create_dir_all(&out_dir)?;
    let adapter = if which == "software" { AdapterPreference::Software } else { AdapterPreference::Auto };

    let mut renderer = WgpuRenderer::headless(&HeadlessOptions { adapter })?;
    let caps = renderer.capabilities();
    println!("adapter/backend: {}", caps.backend);
    let mut report = String::new();
    writeln!(report, "adapter: {}", caps.backend)?;
    writeln!(
        report,
        "| scenario | size | cmds | edges | candidate | prep ms (cpu) | gpu p50 | gpu p95 | wall p50 + prep | wall p95 + prep | PSNR dB | max diff | bad px % |"
    )?;
    writeln!(report, "|---|---|---|---|---|---|---|---|---|---|---|---|---|")?;

    let mut cpu = SkiaRasterizer::new();
    let mut custom_state: Option<custom::Custom> = None;
    let mut vello_state: Option<vello_path::VelloPath> = None;
    let mut vello_err: Option<String> = None;
    let mut prep = geom::Prepared::default();

    for sc in scenarios()? {
        let (w, h) = sc.spec.rect.size_px(sc.px_per_m);
        let desc = TextureDesc {
            size: UVec2::new(w, h),
            colour_space: ColourSpace::Srgb,
            alpha: AlphaEncoding::Premultiplied,
            mips: sc.mips,
            usage: omuzaki_render_api::TextureUsage::Dynamic,
            sampler: Default::default(),
        };
        let tex = renderer.create_texture(&desc, None)?;
        let mut reference = Raster::new(1, 1);
        cpu.rasterize(&sc.spec, &sc.list, sc.px_per_m, &mut reference)?;

        for cand in ["custom", "vello"] {
            if !only.is_empty() && only != cand {
                continue;
            }
            let mut gpu_ms = Vec::new();
            let mut wall_ms = Vec::new();
            let mut prep_ms = Vec::new();
            let mut result: Option<Raster> = None;
            let mut failure: Option<String> = None;
            let n_frames = frames;
            for frame in 0..(n_frames + 3) {
                let t0 = Instant::now();
                let prepared_ok = match cand {
                    "custom" => prep.build(&sc.list, &sc.spec, sc.px_per_m).map_err(|e| format!("{e:?}")),
                    _ => Ok(()),
                };
                let mut vello_ready = true;
                if cand == "vello" {
                    if vello_state.is_none() && vello_err.is_none() {
                        // Created lazily from the renderer's own device (found through `native()` below).
                        vello_ready = false;
                    }
                }
                let prep_time = t0.elapsed().as_secs_f64() * 1e3;
                if let Err(e) = prepared_ok {
                    failure = Some(e);
                    break;
                }
                let mut timing: Option<(f64, f64)> = None;
                let mut vello_prep = 0.0_f64;
                let mut frame_err: Option<String> = None;
                let cand_name = cand;
                let prep_ref = &prep;
                let sc_ref = &sc;
                let (custom_ref, vello_ref, verr) = (&mut custom_state, &mut vello_state, &mut vello_err);
                let measure = frame >= 3;
                let mut painter = |target: &mut dyn PaintTarget| -> Result<(), omuzaki_render_api::RenderError> {
                    let Some((device, queue, texture)) = target.native().downcast_mut::<NativePaintTarget>() else {
                        return Err(omuzaki_render_api::RenderError::invalid("native", "not a wgpu target"));
                    };
                    let timer = Timer::new(device, queue);
                    match cand_name {
                        "custom" => {
                            let c = custom_ref.get_or_insert_with(|| custom::Custom::new(device, wgpu::TextureFormat::Rgba8Unorm));
                            let mut run = || c.draw(device, queue, texture, prep_ref);
                            timing = Some(match &timer {
                                Some(t) => t.time(device, queue, run),
                                None => {
                                    let s = Instant::now();
                                    run();
                                    let _ = device.poll(wgpu::PollType::wait_indefinitely());
                                    (f64::NAN, s.elapsed().as_secs_f64() * 1e3)
                                }
                            });
                        }
                        _ => {
                            if vello_ref.is_none() && verr.is_none() {
                                match vello_path::VelloPath::new(device) {
                                    Ok(v) => *vello_ref = Some(v),
                                    Err(e) => *verr = Some(e),
                                }
                            }
                            if let Some(v) = vello_ref.as_mut() {
                                let s = Instant::now();
                                v.build(&sc_ref.list, &sc_ref.spec, sc_ref.px_per_m);
                                let build = s.elapsed().as_secs_f64() * 1e3;
                                let mut res = Ok(());
                                let mut run = || res = v.draw(device, queue, texture);
                                let (g, wll) = match &timer {
                                    Some(t) => t.time(device, queue, run),
                                    None => {
                                        let s = Instant::now();
                                        run();
                                        let _ = device.poll(wgpu::PollType::wait_indefinitely());
                                        (f64::NAN, s.elapsed().as_secs_f64() * 1e3)
                                    }
                                };
                                if let Err(e) = res {
                                    frame_err = Some(e);
                                }
                                timing = Some((g, wll));
                                vello_prep = build;
                            }
                        }
                    }
                    Ok(())
                };
                use omuzaki_render_api::Renderer as _;
                renderer.paint_texture(tex, &mut painter)?;
                let _ = vello_ready;
                if let Some(e) = frame_err.or_else(|| vello_err.clone()) {
                    failure = Some(e);
                    break;
                }
                if measure {
                    if let Some((g, wl)) = timing {
                        gpu_ms.push(g);
                        // Preparation counts the same way for both paths: it is reported on its own and included in wall.
                        let prep_cpu = if cand == "vello" { vello_prep } else { prep_time };
                        wall_ms.push(wl + prep_cpu);
                        prep_ms.push(prep_cpu);
                    }
                }
            }
            let size = format!("{w}x{h}");
            if let Some(f) = failure {
                writeln!(report, "| {} | {size} | {} | - | {cand} | FAILED: {f} | | | | | | | |", sc.name, sc.list.cmds().len())?;
                continue;
            }
            // Read the last frame back through the renderer's own native handle.
            {
                struct Hold(Option<Raster>);
                let mut hold = Hold(None);
                let mut grab = |target: &mut dyn PaintTarget| -> Result<(), omuzaki_render_api::RenderError> {
                    if let Some((d, q, t)) = target.native().downcast_mut::<NativePaintTarget>() {
                        hold.0 = Some(readback(d, q, t));
                    }
                    Ok(())
                };
                use omuzaki_render_api::Renderer as _;
                renderer.paint_texture(tex, &mut grab)?;
                result = hold.0;
            }
            let Some(res) = result else { continue };
            let e = compare(&res, &reference);
            let slug = sc.name.replace('/', "-");
            over_grey(&res).save(format!("{out_dir}/{cand}-{slug}.png"))?;
            diff_image(&res, &reference).save(format!("{out_dir}/{cand}-{slug}-diff.png"))?;
            if cand == "custom" {
                over_grey(&reference).save(format!("{out_dir}/cpu-{slug}.png"))?;
            }
            let (mut g, mut wl, mut pr) = (gpu_ms.clone(), wall_ms.clone(), prep_ms.clone());
            let edges = if cand == "custom" { prep.edges.len().to_string() } else { "-".into() };
            writeln!(
                report,
                "| {} | {size} | {} | {edges} | {cand} | {:.3} | {:.3} | {:.3} | {:.3} | {:.3} | {:.1} | {} | {:.3} |",
                sc.name,
                sc.list.cmds().len(),
                percentile(&mut pr, 0.5),
                percentile(&mut g, 0.5),
                percentile(&mut g, 0.95),
                percentile(&mut wl, 0.5),
                percentile(&mut wl, 0.95),
                e.psnr,
                e.max,
                e.bad_frac * 100.0
            )?;
            println!("{}", report.lines().last().unwrap_or(""));
        }
    }
    {
        use omuzaki_render_api::Renderer as _;
        let mut mem = String::new();
        let mut probe = |target: &mut dyn PaintTarget| -> Result<(), omuzaki_render_api::RenderError> {
            if let Some((d, _, _)) = target.native().downcast_mut::<NativePaintTarget>() {
                if let Some(r) = d.generate_allocator_report() {
                    let _ = write!(mem, "gpu memory allocated by this process: {:.1} MiB in {} blocks", r.total_allocated_bytes as f64 / 1048576.0, r.blocks.len());
                }
            }
            Ok(())
        };
        if let Some(sc) = scenarios()?.first() {
            let desc = TextureDesc { size: UVec2::new(8, 8), colour_space: ColourSpace::Srgb, alpha: AlphaEncoding::Premultiplied, mips: false, usage: omuzaki_render_api::TextureUsage::Dynamic, sampler: Default::default() };
            let t = renderer.create_texture(&desc, None)?;
            renderer.paint_texture(t, &mut probe)?;
            let _ = sc;
        }
        writeln!(report, "\n{} (only={})", mem, only)?;
        println!("{mem}");
    }
    std::fs::write(format!("{out_dir}/report.md"), &report)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_image_does_not_wrap_for_differences_of_32_and_more() {
        for d in [31u8, 32, 33, 255] {
            let a = Raster { width: 1, height: 1, pixels: vec![d, 0, 0, 0] };
            let b = Raster { width: 1, height: 1, pixels: vec![0, 0, 0, 0] };
            let v = diff_image(&a, &b).get_pixel(0, 0).0[1];
            assert_eq!(v, 255u8.saturating_sub(d.saturating_mul(8)), "difference {d}");
        }
        let a = Raster { width: 1, height: 1, pixels: vec![32, 0, 0, 0] };
        assert_eq!(diff_image(&a, &Raster::new(1, 1)).get_pixel(0, 0).0[1], 0, "32 x 8 = 256 is black, not white");
    }
}
