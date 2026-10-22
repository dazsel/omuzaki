//! Motion: a BVH text file with a two-joint hierarchy (`Hips`, `Spine`) at [`FPS`] frames a second, `min(seconds or 1,
//! MAX_SECONDS) * FPS` frames of sine-driven rotations phased by the seed.

use std::f64::consts::TAU;
use std::fmt::Write as _;

use omuzaki_connect_api::{Artifact, Capability, ConnectError, MediaKind, Output, Request};

use crate::fake::{Generator, seed_of, wrong_kind};

/// Frames a second.
pub const FPS: u32 = 30;
/// The longest fake motion, seconds.
pub const MAX_SECONDS: u32 = 2;
/// The model fake motion reports.
pub const MODEL: &str = "mock-motion@1";

const HIERARCHY: &str = "HIERARCHY
ROOT Hips
{
  OFFSET 0.0 0.0 0.0
  CHANNELS 6 Xposition Yposition Zposition Zrotation Xrotation Yrotation
  JOINT Spine
  {
    OFFSET 0.0 10.0 0.0
    CHANNELS 3 Zrotation Xrotation Yrotation
    End Site
    {
      OFFSET 0.0 10.0 0.0
    }
  }
}
";

pub(crate) struct MotionGen;

impl Generator for MotionGen {
    fn capability(&self) -> Capability {
        Capability::Motion
    }

    fn make(&self, req: &Request, index: u32) -> Result<Output, ConnectError> {
        let Request::Motion(r) = req else {
            return Err(wrong_kind(Capability::Motion, req));
        };
        let seed = seed_of(req, index);
        let frames = r.seconds.unwrap_or(1).min(MAX_SECONDS) * FPS;
        let phase = crate::gen_audio::small(seed % 360).to_radians();
        let bvh =
            bvh(frames, phase).map_err(|e| ConnectError::failed(format!("the fake could not write its BVH: {e}")))?;
        Ok(Output::Asset(Artifact {
            bytes: bvh.into_bytes(),
            media_type: "text/plain".into(),
            kind: MediaKind::Motion,
            name: Some("motion.bvh".into()),
            model: MODEL.into(),
            seed: Some(seed),
            rights: None,
        }))
    }
}

/// The BVH text: the hierarchy, then `frames` frames of rotations phased by `phase` radians.
fn bvh(frames: u32, phase: f64) -> Result<String, std::fmt::Error> {
    let mut text = String::from(HIERARCHY);
    write!(text, "MOTION\nFrames: {frames}\nFrame Time: {:.6}\n", 1.0 / f64::from(FPS))?;
    for f in 0..frames {
        let t = f64::from(f) / f64::from(FPS);
        let swing = |amp: f64, k: f64| amp * (TAU * t * k + phase).sin();
        writeln!(
            text,
            "0.0000 {:.4} 0.0000 {:.4} 0.0000 {:.4} {:.4} {:.4} 0.0000",
            90.0 + swing(1.0, 2.0),
            swing(5.0, 1.0),
            swing(10.0, 0.5),
            swing(8.0, 1.0),
            swing(4.0, 2.0),
        )?;
    }
    Ok(text)
}
