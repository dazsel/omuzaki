//! The stage's picture reaching egui on the app's own device (`NativePresenter`), and the clean program frame the output
//! window keys (#2144): the registration is made once and kept, the frame path takes no lock on egui's renderer, and
//! the character composites over a key colour within two levels of the reference, soft edges included.
//!
//! The device is egui's: the renderer, the stage's target and egui's painter share one `RenderState`, as in the app.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::sync::mpsc;
use std::time::Duration;

use egui::{Color32, Context, vec2};
use egui_kittest::Harness;
use egui_kittest::wgpu::WgpuTestRenderer;
use omuzaki_core::glam::UVec2;
use omuzaki_ops_api::{Actor, Grants, Mode, Request};
use omuzaki_render::WgpuRenderer;
use omuzaki_render_api::{AlphaEncoding, Renderer, TargetDesc};
use omuzaki_track_mock::FakeTracker;
use omuzaki_ui::aux_window::Transparency;
use omuzaki_ui::live::viewport::{NativePresenter, Presenter, kit_stage, published};
use omuzaki_ui::live::wiring::{LiveApp, LiveScene, RealClock};
use omuzaki_ui::output_window::composite::image_over_key;
use omuzaki_ui::output_window::paint_output;

/// The most a window pixel may differ from the reference composite (`tests/output_window.rs` allows the same).
const TOLERANCE: u8 = 2;
/// The key the output is composited over in these tests.
const KEY: Color32 = Color32::from_rgb(0, 177, 64);

fn renderer_on(gpu: &eframe::egui_wgpu::RenderState) -> WgpuRenderer {
    WgpuRenderer::from_device(&gpu.adapter, gpu.device.clone(), gpu.queue.clone()).expect("a renderer on egui's device")
}

#[test]
fn the_registration_with_egui_is_made_once_and_again_only_when_the_target_is_replaced() {
    let gpu = support::render_state();
    let mut renderer = renderer_on(&gpu);
    let target =
        renderer.create_target(&TargetDesc { size: UVec2::new(64, 64), alpha: AlphaEncoding::Premultiplied }).unwrap();
    let mut presenter = NativePresenter::new(gpu.clone());
    let ctx = Context::default();

    let id = presenter.present(&ctx, &mut renderer, target).expect("the first picture registers");
    for frame in 0..8 {
        assert_eq!(presenter.present(&ctx, &mut renderer, target).unwrap(), id, "frame {frame} keeps the id");
    }
    assert_eq!(presenter.registrations(), 1, "nine frames, one view, one sampler, one bind group");

    // A resized target is a new texture: egui must be pointed at it, under the same id.
    renderer.resize_target(target, UVec2::new(32, 32)).unwrap();
    assert_eq!(presenter.present(&ctx, &mut renderer, target).unwrap(), id, "the id survives a resize");
    assert_eq!(presenter.registrations(), 2, "re-registered once for the new texture");
    presenter.present(&ctx, &mut renderer, target).unwrap();
    assert_eq!(presenter.registrations(), 2, "and then kept");
    assert!(gpu.renderer.read().texture(&id).is_some(), "egui holds the texture under that id");
}

#[test]
fn a_frame_takes_no_lock_on_eguis_renderer_once_the_texture_is_registered() {
    let gpu = support::render_state();
    let mut renderer = renderer_on(&gpu);
    let target =
        renderer.create_target(&TargetDesc { size: UVec2::new(32, 32), alpha: AlphaEncoding::Premultiplied }).unwrap();
    let mut presenter = NativePresenter::new(gpu.clone());
    let ctx = Context::default();
    presenter.present(&ctx, &mut renderer, target).expect("registered");

    // egui's painter is write-locked for the whole test, as it is while the app paints a frame. A presenter that
    // takes the lock per frame would wait here; the timeout only bounds that failure.
    let painter = gpu.renderer.write();
    let (tx, rx) = mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            for _ in 0..5 {
                presenter.present(&ctx, &mut renderer, target).expect("a frame");
            }
            tx.send(()).expect("the test is waiting");
        });
        let done = rx.recv_timeout(Duration::from_secs(10));
        assert!(done.is_ok(), "five frames presented while the painter was locked");
        drop(painter);
    });
}

#[test]
fn the_character_alone_composites_over_the_key_within_two_levels_with_soft_edges() {
    omuzaki_ui::link();
    let gpu = support::render_state();
    let size = UVec2::new(320, 400);
    let mut renderer = renderer_on(&gpu);
    let stage = kit_stage(&mut renderer, size).expect("the kit stage starts");
    let base = LiveScene::base_pose(&stage);
    let mut app = LiveApp::start(
        Box::new(FakeTracker::scripted(Vec::new())),
        Box::new(RealClock::new()),
        Transparency::Fallback,
        base,
    )
    .expect("the live slice starts");
    app.attach_viewport(renderer, stage, size, Box::new(NativePresenter::new(gpu.clone()))).expect("attached");
    let runner = app.runner();
    let mut harness = Harness::builder()
        .with_size(vec2(320.0, 400.0))
        .with_pixels_per_point(1.0)
        .renderer(WgpuTestRenderer::from_render_state(gpu.clone()))
        .build_ui_state(
            |ui, app: &mut LiveApp| {
                app.show(ui.ctx());
                let program = published(ui.ctx());
                let _ = paint_output(ui.painter(), ui.ctx().content_rect(), KEY, program, 1.0);
            },
            app,
        );
    // A context that can open OS windows, so the open window counts.
    harness.ctx.set_embed_viewports(false);
    // The output window is open (over a key, here): the backdrop goes, the character stays. (Opened only now: a context
    // that embeds viewports closes the window again.)
    let open = Request {
        op: "output.window.open".to_owned(),
        args: serde_json::json!({ "backdrop": "key" }),
        mode: Mode::Apply,
        actor: Actor::Person { name: "performer".into() },
        grants: Grants::person(),
        cause: None,
        tx: None,
        base: None,
    };
    runner.run(open).expect("the output window opens");

    // Step until the stage has drawn a picture under the open window's (clear) backdrop: the first frames of the
    // harness are drawn before the window counts as open.
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    let (picture, window) = loop {
        harness.run_steps(1);
        let picture = harness.state_mut().viewport_mut().expect("attached").snapshot().expect("the picture reads back");
        if published(&harness.ctx).is_some() && picture.rgba8[3] == 0 {
            // Nothing renders between this read-back and the window's own render below.
            break (picture, harness.render().expect("a software or hardware adapter renders"));
        }
        assert!(std::time::Instant::now() < deadline, "the stage never drew a clear backdrop");
        // Paces the loop; what it waits for is the next perform frame.
        std::thread::sleep(Duration::from_millis(8));
    };
    assert_eq!(window.dimensions(), (320, 400));
    let reference = image_over_key(&picture.rgba8, KEY);
    if let Some(dir) = std::env::var_os("OMUZAKI_LIVE_NATIVE_DIR") {
        let dir = std::path::PathBuf::from(dir);
        window.save(dir.join("output-over-key.png")).expect("saves");
        let flat = image::RgbImage::from_raw(320, 400, reference.clone()).expect("rgb");
        flat.save(dir.join("output-over-key-reference.png")).expect("saves");
    }
    let mut worst = 0_u8;
    for (got, want) in window.pixels().zip(reference.as_chunks::<3>().0) {
        worst = worst.max((0..3).map(|i| got.0[i].abs_diff(want[i])).max().unwrap_or(0));
    }

    let (pixels, _) = picture.rgba8.as_chunks::<4>();
    assert_eq!(pixels[0][3], 0, "nothing is behind the character: the corner is clear");
    assert!(pixels.iter().any(|px| px[3] == 255), "the character is opaque where it is solid");
    let soft = pixels.iter().filter(|px| (1..255).contains(&px[3])).count();
    assert!(soft > 50, "the silhouette has soft edges to composite ({soft} pixels)");
    assert!(
        pixels.iter().all(|px| px[..3].iter().all(|channel| *channel <= px[3].saturating_add(1))),
        "the picture is premultiplied: no colour is brighter than its alpha, as egui blends it"
    );
    println!("clean program over the key: {soft} soft-edge pixels, worst difference {worst} levels");
    assert!(worst <= TOLERANCE, "the window is {worst} levels from the reference composite");
    harness.state_mut().shutdown().expect("a clean shutdown");
}
