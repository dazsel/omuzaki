//! Syphon (macOS): the picture as a Syphon server that OBS's Syphon Client source and other apps list by name.
//!
//! Syphon's protocol is private to its framework (BSD-3-Clause, whose notice we keep), so the server is the framework's
//! own `SyphonMetalServer`, loaded at run time: Omuzaki.app carries `Syphon.framework` in `Contents/Frameworks`, and
//! a build run from source finds it where [`framework_candidates`] says. Without it the kind reports itself
//! unavailable with that hint, and nothing else changes.
//!
//! Each frame is written into a Metal texture and published on a command buffer the server encodes its copy into; the
//! thread waits for that buffer before the next write, so the copy never reads a half-written frame.

#[cfg(target_os = "macos")]
mod mac;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use omuzaki_stream_api::error::StreamError;
use omuzaki_stream_api::output::{AlphaNeed, OutputConfig};

use crate::share::{Opener, Publisher, Share};

/// The registered output kind.
pub const KIND: &str = "syphon";
/// Names the `Syphon.framework` folder to load, ahead of the usual places.
pub const FRAMEWORK_ENV: &str = "OMUZAKI_SYPHON_FRAMEWORK";

/// The idle `syphon` output: straight-alpha frames, published by a Syphon server.
pub fn output() -> Share {
    Share::new(KIND, AlphaNeed::Straight, Arc::new(open) as Opener)
}

/// Where `Syphon.framework` is looked for, in order: the folder `env` names ([`FRAMEWORK_ENV`]), the app bundle's
/// `Contents/Frameworks` (the executable is in `Contents/MacOS`), next to the executable, and `/Library/Frameworks`.
pub fn framework_candidates(env: Option<PathBuf>, exe: Option<&Path>) -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = env.into_iter().collect();
    if let Some(dir) = exe.and_then(Path::parent) {
        candidates.push(dir.join("../Frameworks/Syphon.framework"));
        candidates.push(dir.join("Syphon.framework"));
    }
    candidates.push(PathBuf::from("/Library/Frameworks/Syphon.framework"));
    candidates
}

/// Whether this computer can run a Syphon server: a Mac with `Syphon.framework` where [`framework_candidates`] looks.
///
/// # Errors
/// [`StreamError::Unavailable`] with what to do: get the framework, or use the route that works on this OS.
pub fn available() -> Result<(), StreamError> {
    #[cfg(target_os = "macos")]
    {
        mac::load()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(StreamError::Unavailable {
            what: "Syphon".into(),
            hint: "Syphon is for macOS; on Windows use the Spout output, elsewhere the virtual camera or the output \
                   window"
                .into(),
        })
    }
}

/// Opens a Syphon server for `cfg`: its name, as receivers will list it, and its size.
fn open(cfg: &OutputConfig) -> Result<Box<dyn Publisher>, StreamError> {
    available()?;
    #[cfg(target_os = "macos")]
    {
        Ok(Box::new(mac::Server::open(&cfg.name, cfg.size)?))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = cfg;
        Err(StreamError::Unavailable { what: "Syphon".into(), hint: "Syphon is for macOS".into() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_framework_is_looked_for_in_the_override_the_bundle_and_the_system() {
        let exe = Path::new("/Applications/Omuzaki.app/Contents/MacOS/omuzaki");
        let found = framework_candidates(Some(PathBuf::from("/opt/Syphon.framework")), Some(exe));
        assert_eq!(
            found,
            [
                PathBuf::from("/opt/Syphon.framework"),
                PathBuf::from("/Applications/Omuzaki.app/Contents/MacOS/../Frameworks/Syphon.framework"),
                PathBuf::from("/Applications/Omuzaki.app/Contents/MacOS/Syphon.framework"),
                PathBuf::from("/Library/Frameworks/Syphon.framework"),
            ]
        );
        assert_eq!(framework_candidates(None, None), [PathBuf::from("/Library/Frameworks/Syphon.framework")]);
    }

    #[test]
    fn syphon_sends_straight_alpha_for_obs() {
        use omuzaki_stream_api::output::Output as _;
        assert_eq!(output().info().alpha, AlphaNeed::Straight);
        assert_eq!(output().info().kind, KIND);
    }

    #[test]
    fn off_macos_the_kind_points_at_the_route_that_works() {
        if !cfg!(target_os = "macos") {
            let Err(StreamError::Unavailable { hint, .. }) = available() else { panic!("Syphon is macOS-only") };
            assert!(hint.contains("Spout"), "{hint}");
        }
    }
}
