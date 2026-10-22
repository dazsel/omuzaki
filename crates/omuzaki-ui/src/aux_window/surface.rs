//! [`transparency`]: whether a window can show a transparent backdrop on this platform and route, from the #133
//! research (`docs/research/2026-10-mascot-window.md`, sections 1, 6 and 9).
//!
//! The answer is "verified" only for a combination the research ran and saw blend correctly. Everything else falls
//! back to the key colour and says so, so a person never streams a black box thinking it is transparent.

/// The OS and windowing system.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Platform {
    /// Windows.
    Windows,
    /// macOS.
    MacOs,
    /// Linux under an X11 server.
    LinuxX11,
    /// Linux under a Wayland compositor.
    LinuxWayland,
    /// Anything else.
    Other,
}

impl Platform {
    /// The platform this build runs on, reading `WAYLAND_DISPLAY` on Linux.
    pub fn current() -> Self {
        if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "linux") {
            if std::env::var_os("WAYLAND_DISPLAY").is_some() { Self::LinuxWayland } else { Self::LinuxX11 }
        } else {
            Self::Other
        }
    }
}

/// The GPU backend presenting the window.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Backend {
    /// Vulkan.
    Vulkan,
    /// Direct3D 12.
    Dx12,
    /// Metal.
    Metal,
    /// OpenGL or GLES.
    Gl,
    /// Not known (no GPU context yet, or another backend).
    Unknown,
}

impl Backend {
    /// The backend eframe's wgpu adapter reports, by its name in `wgpu::AdapterInfo::backend` (`"vulkan"`, `"dx12"`,
    /// `"metal"`, `"gl"`).
    pub fn from_name(name: &str) -> Self {
        match name.to_ascii_lowercase().as_str() {
            "vulkan" => Self::Vulkan,
            "dx12" => Self::Dx12,
            "metal" => Self::Metal,
            "gl" | "gles" | "opengl" => Self::Gl,
            _ => Self::Unknown,
        }
    }
}

/// How the window is made.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SurfaceRoute {
    /// An egui secondary viewport under an opaque eframe root window. eframe configures every surface with the
    /// root's transparency (research section 6), so this route is transparent only where the platform ignores that.
    EguiViewport,
}

/// The answer for one platform, backend and route.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Transparency {
    /// The research saw this combination blend correctly.
    Verified,
    /// Not verified: the window shows the key colour instead, and says `fallback`.
    Fallback,
}

/// Whether `route` gives a verified transparent surface on `platform` with `backend`.
///
/// Verified: X11 with Vulkan, where the secondary viewport blended correctly under an opaque root (research 9.1,
/// cloud VM with lavapipe). Not verified, so falling back: Windows (DX12 needs the visual swap chain, and eframe applies
/// the opaque root's flag), macOS (Metal's post-multiplied layer, fringe unchecked), Wayland, GL (opaque only) and any
/// unknown backend.
pub fn transparency(platform: Platform, backend: Backend, route: SurfaceRoute) -> Transparency {
    match (platform, backend, route) {
        (Platform::LinuxX11, Backend::Vulkan, SurfaceRoute::EguiViewport) => Transparency::Verified,
        _ => Transparency::Fallback,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_combination_the_research_verified_is_transparent() {
        let route = SurfaceRoute::EguiViewport;
        assert_eq!(transparency(Platform::LinuxX11, Backend::Vulkan, route), Transparency::Verified);
        for (platform, backend) in [
            (Platform::Windows, Backend::Dx12),
            (Platform::Windows, Backend::Vulkan),
            (Platform::MacOs, Backend::Metal),
            (Platform::LinuxX11, Backend::Gl),
            (Platform::LinuxWayland, Backend::Vulkan),
            (Platform::LinuxX11, Backend::Unknown),
            (Platform::Other, Backend::Vulkan),
        ] {
            assert_eq!(transparency(platform, backend, route), Transparency::Fallback, "{platform:?} {backend:?}");
        }
    }

    #[test]
    fn backends_are_read_from_their_wgpu_names() {
        assert_eq!(Backend::from_name("Vulkan"), Backend::Vulkan);
        assert_eq!(Backend::from_name("dx12"), Backend::Dx12);
        assert_eq!(Backend::from_name("Metal"), Backend::Metal);
        assert_eq!(Backend::from_name("Gl"), Backend::Gl);
        assert_eq!(Backend::from_name("BrowserWebGpu"), Backend::Unknown);
    }
}
