//! Renders a short file performed by a 2D puppet to a `WebM`, outside any project: the quickest way to see a puppet
//! perform a script.
//!
//! ```text
//! cargo run --release -p omuzaki-short --example puppet_short -- <short.toml> <puppet> <out.webm> [WxH]
//! ```
//!
//! `<puppet>` is a bundle folder (holding `puppet.json`) or a `*.layers.toml` manifest of layered art, rigged on the
//! spot. A silent or file voice only: a spoken short needs the studio's model registry.

#![allow(clippy::disallowed_methods, reason = "an example reads and writes the paths it is given")]

use std::path::Path;

use omuzaki_puppet::autorig::autorig;
use omuzaki_puppet::bundle::{self, Bundle};
use omuzaki_puppet::layers::LayeredArt;
use omuzaki_short::ShortDef;
use omuzaki_short::encode::ShortWriter;
use omuzaki_short::puppet::render_puppet_short;
use omuzaki_short::timeline::Timeline;
use omuzaki_short::voice::{Synthesis, read_wav_from, voice_for};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [short, puppet, out, rest @ ..] = args.as_slice() else {
        return Err("usage: puppet_short <short.toml> <puppet> <out.webm> [WxH]".into());
    };
    let short = Path::new(short);
    let mut def = ShortDef::from_toml(&std::fs::read_to_string(short)?)?;
    if let Some(size) = rest.first() {
        let (w, h) = size.split_once('x').ok_or("a size is WxH")?;
        def.size = [w.parse()?, h.parse()?];
        def.check()?;
    }
    def.puppet = Some(puppet.clone());
    let bundle = if Path::new(puppet).extension().is_some_and(|e| e.eq_ignore_ascii_case("toml")) {
        let art = LayeredArt::load(Path::new(puppet))?;
        let rigged = autorig(&art)?;
        Bundle::from_rigged(rigged)
    } else {
        bundle::load(Path::new(puppet))?
    };
    let folder = short.parent().unwrap_or_else(|| Path::new("."));
    let voice = voice_for(
        &def,
        |path| {
            let file = std::fs::File::open(folder.join(path))
                .map_err(|e| omuzaki_short::ShortError::Voice(format!("{path}: {e}")))?;
            read_wav_from(std::io::BufReader::new(file), path)
        },
        Synthesis::Estimate,
    )?;
    let timeline = Timeline::new(&def, &voice)?;
    let has_voice = !voice.pcm.is_empty() && !matches!(def.voice, omuzaki_short::def::VoiceDef::Silent { .. });
    let file = std::fs::File::create(out)?;
    let mut writer =
        ShortWriter::create(file, Path::new(out), def.size, def.fps, has_voice.then_some(&voice.pcm[..]), false)?;
    let started = std::time::Instant::now();
    let report = render_puppet_short(bundle, &def, &timeline, &voice, &mut writer, &mut |_| Ok(()))?;
    let written = writer.finish()?;
    let seconds = started.elapsed().as_secs_f64();
    let per_frame = 1000.0 / f64::from(u32::try_from(report.frames.max(1)).unwrap_or(u32::MAX));
    println!(
        "{out}: {} frames, {:.1} s of video, {} bytes, audio {}; {:.1} s ({:.1} ms a frame drawing, {:.1} ms encoding)",
        report.frames,
        timeline.duration,
        written.bytes,
        written.audio_in_video,
        seconds,
        report.render_s * per_frame,
        report.encode_s * per_frame,
    );
    Ok(())
}
