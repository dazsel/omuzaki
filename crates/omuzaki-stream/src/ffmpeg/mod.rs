//! `FFmpeg` encoders and muxers, behind the `ffmpeg` feature: the libraries are loaded at run time.
pub mod encode;
pub mod hw;
pub mod mux;
pub mod probe;
