//! Wrappers that put each library behind the same 10 ms, 48 kHz, mono `Canceller` interface.

use crate::Canceller;
use crate::signals::{FRAME, RATE};

/// AEC3 through the `sonora` crate (pure Rust port of WebRTC's audio processing module), echo canceller only: no gain
/// control, noise suppression or high-pass.
pub struct Sonora {
    apm: sonora::AudioProcessing,
    scratch: Vec<f32>,
}

impl Sonora {
    pub fn new() -> Self {
        let stream = sonora::StreamConfig::new(RATE as u32, 1);
        let config = sonora::Config {
            // The forced 100 Hz high-pass is a separate stage in `fx.aec`'s chain; leave it out so the numbers are the
            // canceller's own.
            echo_canceller: Some(sonora::config::EchoCanceller {
                enforce_high_pass_filtering: false,
                ..Default::default()
            }),
            ..Default::default()
        };
        let apm =
            sonora::AudioProcessing::builder().config(config).capture_config(stream).render_config(stream).build();
        Self { apm, scratch: vec![0.0; FRAME] }
    }
}

impl Canceller for Sonora {
    fn name(&self) -> String {
        "sonora-aec3".to_string()
    }

    fn process(&mut self, far: &[f32], mic: &[f32], out: &mut [f32]) {
        // A failure here is a harness bug (a wrong frame size), so say so loudly rather than skipping the frame.
        let r = self.apm.process_render_f32(&[far], &mut [&mut self.scratch[..]]);
        assert!(r.is_ok(), "sonora render: {r:?}");
        let c = self.apm.process_capture_f32(&[mic], &mut [out]);
        assert!(c.is_ok(), "sonora capture: {c:?}");
    }
}

/// `SpeexDSP`'s MDF echo canceller (through `aec-rs`), with its residual echo suppressor in the preprocessor when asked.
pub struct Speex {
    aec: aec_rs::Aec,
    filter_ms: usize,
    preprocess: bool,
    // Scratch buffers, so the harness adds no allocations of its own to the count.
    mic_i16: Vec<i16>,
    far_i16: Vec<i16>,
    out_i16: Vec<i16>,
}

impl Speex {
    pub fn new(filter_ms: usize, preprocess: bool) -> Self {
        let aec = aec_rs::Aec::new(&aec_rs::AecConfig {
            frame_size: FRAME,
            filter_length: i32::try_from(filter_ms * RATE / 1000).unwrap_or(i32::MAX),
            sample_rate: RATE as u32,
            enable_preprocess: preprocess,
        });
        Self { aec, filter_ms, preprocess, mic_i16: vec![0; FRAME], far_i16: vec![0; FRAME], out_i16: vec![0; FRAME] }
    }
}

fn to_i16(x: &[f32], out: &mut [i16]) {
    for (o, v) in out.iter_mut().zip(x) {
        *o = (v * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
    }
}

impl Canceller for Speex {
    fn name(&self) -> String {
        format!("speexdsp-mdf-{}ms{}", self.filter_ms, if self.preprocess { "+pp" } else { "" })
    }

    fn process(&mut self, far: &[f32], mic: &[f32], out: &mut [f32]) {
        to_i16(mic, &mut self.mic_i16);
        to_i16(far, &mut self.far_i16);
        self.aec.cancel_echo(&self.mic_i16, &self.far_i16, &mut self.out_i16);
        for (d, s) in out.iter_mut().zip(&self.out_i16) {
            *d = f32::from(*s) / 32768.0;
        }
    }
}

/// The second AEC3 port (`aec3` crate), through its linear voice pipeline with every other stage turned off.
pub struct Aec3Port {
    pipe: aec3::pipelines::linear::LinearPipeline,
}

impl Aec3Port {
    pub fn new() -> Result<Self, String> {
        let format = aec3::nodes::audio::AudioFormat::ten_ms(RATE as u32, 1);
        let pipe = aec3::pipelines::linear::builder(format, format)
            .enable_noise_suppression(false)
            .enable_high_pass_filter(false)
            .enable_gain_controller2(false)
            .enable_post_filter(false)
            .build()
            .map_err(|e| format!("{e:?}"))?;
        Ok(Self { pipe })
    }
}

impl Canceller for Aec3Port {
    fn name(&self) -> String {
        "aec3-port".to_string()
    }

    fn process(&mut self, far: &[f32], mic: &[f32], out: &mut [f32]) {
        let r = self.pipe.handle_render_frame(far);
        assert!(r.is_ok(), "aec3 render: {r:?}");
        let c = self.pipe.process_capture_frame(mic, out);
        assert!(c.is_ok(), "aec3 capture: {c:?}");
    }
}

/// WebRTC's own C++ AEC3 through the `webrtc-audio-processing` binding (the `PulseAudio` repackaging of the module).
#[cfg(feature = "webrtc-cpp")]
pub struct WebrtcCpp {
    ap: webrtc_audio_processing::Processor,
    render: Vec<f32>,
}

#[cfg(feature = "webrtc-cpp")]
impl WebrtcCpp {
    pub fn new() -> Result<Self, String> {
        use webrtc_audio_processing::config::{Config, EchoCanceller};
        let ap = webrtc_audio_processing::Processor::new(RATE as u32).map_err(|e| format!("{e:?}"))?;
        // Echo canceller only, as for the Rust ports; no high-pass, noise suppression or gain control.
        ap.set_config(Config { echo_canceller: Some(EchoCanceller::default()), ..Default::default() });
        Ok(Self { ap, render: vec![0.0; FRAME] })
    }
}

#[cfg(feature = "webrtc-cpp")]
impl Canceller for WebrtcCpp {
    fn name(&self) -> String {
        "webrtc-cpp-aec3".to_string()
    }

    fn process(&mut self, far: &[f32], mic: &[f32], out: &mut [f32]) {
        self.render.copy_from_slice(far);
        let r = self.ap.process_render_frame([self.render.as_mut_slice()]);
        assert!(r.is_ok(), "webrtc render: {r:?}");
        out.copy_from_slice(mic);
        let c = self.ap.process_capture_frame([&mut *out]);
        assert!(c.is_ok(), "webrtc capture: {c:?}");
    }
}
