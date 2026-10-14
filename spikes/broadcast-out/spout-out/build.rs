//! Builds the pinned Spout2 SDK (BSD-2-Clause) plus our small C shim for Windows targets.
//!
//! The SDK source is not vendored. `tools/fetch-spout2.sh` puts it in `third_party/Spout2`; `SPOUT2_DIR` overrides the
//! location. Other targets skip all of this so the pure-Rust parts still build and test on Linux.

use std::env;
use std::path::PathBuf;

/// The SDK files that make up `spoutDX` (the DirectX 11 sender and receiver class) and what it depends on.
const SPOUT_SOURCES: [&str; 6] = [
    "SPOUTSDK/SpoutGL/SpoutCopy.cpp",
    "SPOUTSDK/SpoutGL/SpoutDirectX.cpp",
    "SPOUTSDK/SpoutGL/SpoutFrameCount.cpp",
    "SPOUTSDK/SpoutGL/SpoutSenderNames.cpp",
    "SPOUTSDK/SpoutGL/SpoutSharedMemory.cpp",
    "SPOUTSDK/SpoutGL/SpoutUtils.cpp",
];

/// Directory holding the cross compiler's `libstdc++.a`, asked of the compiler itself rather than hard-coded.
fn static_runtime_dir(build: &cc::Build) -> PathBuf {
    let output = build.get_compiler().to_command().arg("-print-file-name=libstdc++.a").output();
    let path = output.ok().map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()));
    path.and_then(|p| p.parent().map(PathBuf::from)).unwrap_or_default()
}

fn main() {
    println!("cargo:rerun-if-changed=csrc");
    println!("cargo:rerun-if-env-changed=SPOUT2_DIR");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let spout = env::var_os("SPOUT2_DIR").map_or_else(|| manifest.join("../third_party/Spout2"), PathBuf::from);
    assert!(
        spout.join("SPOUTSDK/SpoutDirectX/SpoutDX/SpoutDX.cpp").exists(),
        "Spout2 source not found in {}: run spikes/broadcast-out/tools/fetch-spout2.sh or set SPOUT2_DIR",
        spout.display()
    );

    let sdk = spout.join("SPOUTSDK");
    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .warnings(false)
        .opt_level(2)
        // The Spout sources target the Windows 10 SDK; mingw-w64 10 needs the version macros, a `timeapi.h` stand-in and
        // SSSE3 for Spout's pixel-swizzle routines. `spout_compat.h` is force-included into every translation unit.
        .define("WINVER", "0x0A00")
        .define("_WIN32_WINNT", "0x0A00")
        .define("NDEBUG", None)
        .flag("-mssse3")
        .flag("-include")
        .flag(manifest.join("csrc/compat/spout_compat.h").to_string_lossy().as_ref())
        .include(manifest.join("csrc/compat"))
        .include(sdk.join("SpoutGL"))
        .include(sdk.join("SpoutDirectX/SpoutDX"))
        .file(sdk.join("SpoutDirectX/SpoutDX/SpoutDX.cpp"))
        .file(manifest.join("csrc/bo_shim.cpp"))
        .cpp_link_stdlib(None);
    for source in SPOUT_SOURCES {
        build.file(spout.join(source));
    }
    build.compile("spoutdx");

    // The C++ runtime goes in statically so the exe has no libstdc++ DLL to carry around. rustc links with the C driver,
    // so the library is named explicitly (cc's default would pick the DLL import library).
    println!("cargo:rustc-link-search=native={}", static_runtime_dir(&build).display());
    println!("cargo:rustc-link-lib=static=stdc++");
    // libstdc++ pulls in libgcc's win32 thread glue, which needs a symbol from libmingw32 that rustc's own link line
    // lists too early; naming it again at the very end resolves it.
    println!("cargo:rustc-link-arg=-lmingw32");
    for lib in [
        "d3d11", "dxgi", "psapi", "shell32", "ole32", "advapi32", "version", "user32", "gdi32", "winmm", "comdlg32",
        "shlwapi", "dwmapi", "comctl32",
    ] {
        println!("cargo:rustc-link-lib={lib}");
    }
}
