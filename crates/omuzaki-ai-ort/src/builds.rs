//! The pinned ONNX Runtime builds: Microsoft's official release archives, one per platform and flavour, all at the
//! version `omuzaki-audio` already loads (`ORT_RUNTIME_ID` in its catalogue) and `ort` is written for (`api-27`).
//!
//! Each build is a [`PinnedFile`] (an ordered URL list, Microsoft's release first; never Pyke's binaries, register row
//! `ort`) that the app's [`FileSource`](omuzaki_ai_api::FileSource) fetches and verifies on first use. Hashes and sizes
//! were taken by streaming the release assets, and each archive's `LICENSE` (MIT, Copyright (c) Microsoft Corporation)
//! has a row in `docs/licensing/register.md`.
//!
//! **Not pinned, because Microsoft publishes no such archive for 1.28.3:** a `DirectML` build (the GitHub release has
//! none, the `NuGet` package `Microsoft.ML.OnnxRuntime.DirectML` stops at 1.24.4 and the plain `win-x64` archive has no
//! `DirectML`) and a WebGPU build. Asking for those providers falls back to the CPU and says so.

use omuzaki_ai_api::{PinnedFile, Provider, Sha256};

/// The ONNX Runtime version every build is pinned at.
pub const ORT_VERSION: &str = "1.28.3";

/// How a build's archive is packed.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Archive {
    /// A gzip-compressed tar (`.tgz`).
    TarGz,
    /// A zip.
    Zip,
}

/// One pinned build.
#[derive(Debug, PartialEq, Eq)]
pub struct Build {
    /// Stable name, used in messages and as the extraction folder's name.
    pub id: &'static str,
    /// The operating system, as `std::env::consts::OS` spells it.
    pub os: &'static str,
    /// The CPU architecture, as `std::env::consts::ARCH` spells it.
    pub arch: &'static str,
    /// The non-CPU providers this build was compiled with. The CPU provider is in every build.
    pub providers: &'static [Provider],
    /// Release asset file name; also the folder inside the archive.
    pub asset: &'static str,
    /// How the archive is packed.
    pub archive: Archive,
    /// The library to load, inside the archive's `lib/` folder.
    pub library: &'static str,
    /// SHA-256 of the archive.
    pub sha256: Sha256,
    /// Exact archive length in bytes.
    pub size: u64,
}

/// A hash from its hex, checked when the table is built (a malformed one stops the compile).
const fn hash(hex: &str) -> Sha256 {
    match Sha256::from_hex(hex) {
        Some(h) => h,
        None => panic!("malformed SHA-256 in the build table"),
    }
}

const RELEASE: &str = "https://github.com/microsoft/onnxruntime/releases/download/v1.28.3";

impl Build {
    /// The pin the file source fetches: Microsoft's release URL first.
    pub fn pin(&self) -> PinnedFile {
        PinnedFile::new([format!("{RELEASE}/{}", self.asset)], self.sha256, self.size)
    }

    /// Whether this build can run `provider` (the CPU always).
    pub fn has(&self, provider: Provider) -> bool {
        provider == Provider::Cpu || self.providers.contains(&provider)
    }

    /// The folder inside the archive that holds the library: `<asset without extension>/lib`.
    pub fn lib_dir(&self) -> String {
        let stem = self.asset.strip_suffix(".tgz").or_else(|| self.asset.strip_suffix(".zip")).unwrap_or(self.asset);
        format!("{stem}/lib")
    }
}

/// Every pinned build.
pub static BUILDS: &[Build] = &[
    Build {
        id: "linux-x64",
        os: "linux",
        arch: "x86_64",
        providers: &[],
        asset: "onnxruntime-linux-x64-1.28.3.tgz",
        archive: Archive::TarGz,
        library: "libonnxruntime.so.1.28.3",
        sha256: hash("db14e4863bd37893fc59729d986ab2a0d043d10b7d44da1913c4982b7e3d009c"),
        size: 9_130_098,
    },
    Build {
        id: "linux-x64-cuda12",
        os: "linux",
        arch: "x86_64",
        providers: &[Provider::Cuda],
        asset: "onnxruntime-linux-x64-gpu_cuda12-1.28.3.tgz",
        archive: Archive::TarGz,
        library: "libonnxruntime.so.1.28.3",
        sha256: hash("4e16d2ec66521a24fe917bd848eb8ae3a9107cb8b4aec978fb2b3ab1a640fcb7"),
        size: 423_745_424,
    },
    Build {
        id: "win-x64",
        os: "windows",
        arch: "x86_64",
        providers: &[],
        asset: "onnxruntime-win-x64-1.28.3.zip",
        archive: Archive::Zip,
        library: "onnxruntime.dll",
        sha256: hash("1d6fab48e85f948436af7c8c971d2c145cf224e2c444755dec894f8b0de11a83"),
        size: 78_606_946,
    },
    Build {
        id: "win-x64-cuda12",
        os: "windows",
        arch: "x86_64",
        providers: &[Provider::Cuda],
        asset: "onnxruntime-win-x64-gpu_cuda12-1.28.3.zip",
        archive: Archive::Zip,
        library: "onnxruntime.dll",
        sha256: hash("65d7f956649df80a8ccdcafa7843e4b02f38bfc9e6de28711ed8384508040330"),
        size: 454_803_675,
    },
    Build {
        id: "osx-arm64",
        os: "macos",
        arch: "aarch64",
        providers: &[Provider::CoreMl],
        asset: "onnxruntime-osx-arm64-1.28.3.tgz",
        archive: Archive::TarGz,
        library: "libonnxruntime.1.28.3.dylib",
        sha256: hash("c436bd9f47dbce6f6311ccc21de97f829b3c862c09d24aa321958cb72f0a1d32"),
        size: 33_057_743,
    },
];

/// The build for `os` and `arch` that suits `wanted`: the first non-CPU provider in `wanted` that some build for the
/// platform has picks that build (CUDA only on request, never by default); otherwise the platform's default build.
/// `None` when no build exists for the platform.
pub fn select(os: &str, arch: &str, wanted: &[Provider]) -> Option<&'static Build> {
    let platform = || BUILDS.iter().filter(move |b| b.os == os && b.arch == arch);
    wanted
        .iter()
        .filter(|p| **p != Provider::Cpu)
        .find_map(|p| platform().find(|b| b.has(*p)))
        .or_else(|| platform().find(|b| b.id == default_id(os)))
}

/// The id of the default build of each OS: the one a person gets without opting in to anything.
fn default_id(os: &str) -> &'static str {
    match os {
        "windows" => "win-x64",
        "macos" => "osx-arm64",
        _ => "linux-x64",
    }
}

/// [`select`] for the machine this runs on.
pub fn select_here(wanted: &[Provider]) -> Option<&'static Build> {
    select(std::env::consts::OS, std::env::consts::ARCH, wanted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_pin_is_well_formed_and_microsofts_release_comes_first() {
        for build in BUILDS {
            let pin = build.pin();
            assert_eq!(pin.sha256, build.sha256, "{}", build.id);
            assert_eq!(pin.urls.len(), 1);
            assert!(pin.urls[0].starts_with("https://github.com/microsoft/onnxruntime/releases/download/v1.28.3/"));
            assert!(pin.urls[0].ends_with(build.asset));
            assert!(!pin.urls[0].to_lowercase().contains("pyke"), "never Pyke's binaries");
            assert!(build.asset.contains(ORT_VERSION));
        }
    }

    #[test]
    fn the_linux_cpu_pin_is_the_one_omuzaki_audio_already_loads() {
        let linux = &BUILDS[0];
        assert_eq!(linux.sha256.to_string(), "db14e4863bd37893fc59729d986ab2a0d043d10b7d44da1913c4982b7e3d009c");
        assert_eq!(linux.size, 9_130_098);
    }

    #[test]
    fn cpu_unless_the_settings_ask_for_cuda() {
        let pick = |os, wanted: &[Provider]| select(os, "x86_64", wanted).map(|b| b.id);
        assert_eq!(pick("linux", &[Provider::Cpu]), Some("linux-x64"));
        assert_eq!(pick("linux", &[Provider::DirectMl, Provider::Cpu]), Some("linux-x64"));
        assert_eq!(pick("linux", &[Provider::Cuda, Provider::Cpu]), Some("linux-x64-cuda12"));
        assert_eq!(pick("windows", &[Provider::DirectMl, Provider::Cuda]), Some("win-x64-cuda12"));
        assert_eq!(pick("windows", &[]), Some("win-x64"));
        assert_eq!(select("macos", "aarch64", &[Provider::CoreMl]).map(|b| b.id), Some("osx-arm64"));
        assert_eq!(select("freebsd", "x86_64", &[]), None);
    }
}
