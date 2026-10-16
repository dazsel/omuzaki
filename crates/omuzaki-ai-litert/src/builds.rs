//! The pinned LiteRT builds: one archive per platform, built by `.github/workflows/runtime-litert.yml` from
//! `tools/litert/pins.toml` (design §8.4).
//!
//! **No SHA-256, size or URL yet.** CI rebuilds the archive on a cache miss, so its trust root is the run's own build
//! output: whoever hands an archive over checks it against that run's SHA-256 first (`OMUZAKI_LITERT_SHA256` in the
//! tests). A fixed pin for people's machines comes only with publishing (#1748).

/// LiteRT's release tag, without the `v`.
pub const LITERT_VERSION: &str = "2.3.0";

/// What the library's `TfLiteVersion()` returns at that tag: TensorFlow's version from LiteRT's workspace, with the
/// suffix of a build without a wheel type (`tflite_version` in `tools/litert/pins.toml`).
pub const TFLITE_VERSION: &str = "2.22.0-dev0+selfbuilt";

/// `OMZ_LITERT_ABI` of the shim compiled into the library (`tools/litert/omz_litert_shim.c`).
pub const OMZ_LITERT_ABI: u32 = 1;

/// The TFLite schema version the scan reads, and what `TfLiteSchemaVersion()` must return.
pub const SCHEMA_VERSION: u32 = 3;

/// How a build's archive is packed.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Archive {
    /// A gzip-compressed tar (`.tar.gz`).
    TarGz,
    /// A zip.
    Zip,
}

/// One platform's build.
#[derive(Debug, PartialEq, Eq)]
pub struct Build {
    /// The platform, as the workflow names it.
    pub id: &'static str,
    /// The operating system, as `std::env::consts::OS` spells it.
    pub os: &'static str,
    /// The CPU architecture, as `std::env::consts::ARCH` spells it.
    pub arch: &'static str,
    /// The archive's file name.
    pub asset: &'static str,
    /// How the archive is packed.
    pub archive: Archive,
    /// The library inside the archive's `lib/` folder.
    pub library: &'static str,
}

impl Build {
    /// The folder inside the archive that holds the library: `<asset without extension>/lib`.
    pub fn lib_dir(&self) -> String {
        let stem = self.asset.strip_suffix(".tar.gz").or_else(|| self.asset.strip_suffix(".zip")).unwrap_or(self.asset);
        format!("{stem}/lib")
    }
}

/// Every pinned build.
pub static BUILDS: &[Build] = &[
    Build {
        id: "linux-x64",
        os: "linux",
        arch: "x86_64",
        asset: "omuzaki-litert-2.3.0-linux-x64.tar.gz",
        archive: Archive::TarGz,
        library: "libomuzaki_litert.so",
    },
    Build {
        id: "win-x64",
        os: "windows",
        arch: "x86_64",
        asset: "omuzaki-litert-2.3.0-win-x64.zip",
        archive: Archive::Zip,
        library: "omuzaki_litert.dll",
    },
    Build {
        id: "macos-arm64",
        os: "macos",
        arch: "aarch64",
        asset: "omuzaki-litert-2.3.0-macos-arm64.tar.gz",
        archive: Archive::TarGz,
        library: "libomuzaki_litert.dylib",
    },
];

/// The build for `os` and `arch`, or `None` when there is none.
pub fn select(os: &str, arch: &str) -> Option<&'static Build> {
    BUILDS.iter().find(|b| b.os == os && b.arch == arch)
}

/// [`select`] for the machine this runs on.
pub fn select_here() -> Option<&'static Build> {
    select(std::env::consts::OS, std::env::consts::ARCH)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `tools/litert/pins.toml`, which the build workflow checks the archives against.
    const PINS: &str = include_str!("../../../tools/litert/pins.toml");

    /// The value of `key` in `[table]` of the pins, as written (quotes removed).
    fn pin(table: &str, key: &str) -> String {
        let section = PINS.split(&format!("\n[{table}]\n")).nth(1).unwrap().split("\n[").next().unwrap();
        let line = section.lines().find(|l| l.starts_with(&format!("{key} = "))).unwrap();
        line.split_once(" = ").unwrap().1.trim_matches('"').to_owned()
    }

    #[test]
    fn the_table_matches_the_pins_the_workflow_builds_from() {
        assert_eq!(pin("litert", "version"), LITERT_VERSION);
        assert_eq!(pin("litert", "tflite_version"), TFLITE_VERSION);
        assert_eq!(pin("shim", "abi"), OMZ_LITERT_ABI.to_string());
        for build in BUILDS {
            let table = format!("platform.{}", build.id);
            assert_eq!(pin(&table, "library"), build.library);
            let ext = match build.archive {
                Archive::TarGz => "tar.gz",
                Archive::Zip => "zip",
            };
            assert_eq!(pin(&table, "archive"), ext);
            assert_eq!(build.asset, format!("omuzaki-litert-{LITERT_VERSION}-{}.{ext}", build.id));
        }
    }

    #[test]
    fn the_shims_abi_number_is_the_one_in_its_source() {
        let shim = include_str!("../../../tools/litert/omz_litert_shim.c");
        assert!(shim.contains(&format!("#define OMZ_LITERT_ABI {OMZ_LITERT_ABI}u")));
    }

    #[test]
    fn each_platform_has_its_build_and_its_library_folder() {
        assert_eq!(select("linux", "x86_64").map(|b| b.id), Some("linux-x64"));
        assert_eq!(select("windows", "x86_64").map(|b| b.id), Some("win-x64"));
        assert_eq!(select("macos", "aarch64").map(|b| b.id), Some("macos-arm64"));
        assert_eq!(select("linux", "aarch64"), None);
        assert_eq!(BUILDS[0].lib_dir(), "omuzaki-litert-2.3.0-linux-x64/lib");
        assert_eq!(BUILDS[1].lib_dir(), "omuzaki-litert-2.3.0-win-x64/lib");
    }
}
