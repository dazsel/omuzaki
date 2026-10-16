//! Choosing the surface's composite alpha mode and the matching shader output convention.
//!
//! wgpu reports per backend which [`CompositeAlphaMode`]s a surface can use. What it means for the picture differs:
//! the shader must write premultiplied colour for `PreMultiplied` and straight colour for `PostMultiplied`, and
//! `Inherit` leaves the choice to the window system. These are pure functions so the choice can be unit tested without
//! a GPU.

use wgpu::CompositeAlphaMode;

/// Whether the chosen mode can produce a see-through window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transparency {
    /// The mode blends the window with what is behind it.
    Yes,
    /// `Inherit`: depends on how the OS window was created, so only a look at the screen can tell.
    Unknown,
    /// `Opaque` (or `Auto` resolving to it): the window will be a solid rectangle.
    No,
}

/// What the fragment shader writes to the swap chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Output {
    /// Colour already multiplied by alpha.
    Premultiplied,
    /// Colour divided by alpha.
    Straight,
}

/// A mode picked from the surface's capabilities.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Picked {
    /// The mode to configure the surface with.
    pub mode: CompositeAlphaMode,
    /// Whether it can be transparent.
    pub transparency: Transparency,
}

/// The requested mode is not offered by this surface.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("alpha mode {wanted:?} is not offered; the surface supports {offered:?}")]
pub struct Unsupported {
    /// What was asked for.
    pub wanted: CompositeAlphaMode,
    /// What the surface offers.
    pub offered: Vec<CompositeAlphaMode>,
}

/// Mode preference when none is requested: premultiplied first, because the renderer's frames are premultiplied.
const PREFERENCE: [CompositeAlphaMode; 4] = [
    CompositeAlphaMode::PreMultiplied,
    CompositeAlphaMode::PostMultiplied,
    CompositeAlphaMode::Inherit,
    CompositeAlphaMode::Opaque,
];

fn transparency(mode: CompositeAlphaMode) -> Transparency {
    match mode {
        CompositeAlphaMode::PreMultiplied | CompositeAlphaMode::PostMultiplied => Transparency::Yes,
        CompositeAlphaMode::Inherit => Transparency::Unknown,
        _ => Transparency::No,
    }
}

/// Picks `wanted` if the surface offers it, otherwise the best offered mode (or an error when `wanted` was explicit).
///
/// # Errors
/// [`Unsupported`] when `wanted` is `Some` and not in `offered`.
pub fn pick(offered: &[CompositeAlphaMode], wanted: Option<CompositeAlphaMode>) -> Result<Picked, Unsupported> {
    let mode = match wanted {
        Some(w) if offered.contains(&w) => w,
        Some(w) => return Err(Unsupported { wanted: w, offered: offered.to_vec() }),
        None => PREFERENCE
            .into_iter()
            .find(|m| offered.contains(m))
            .or_else(|| offered.first().copied())
            .unwrap_or(CompositeAlphaMode::Opaque),
    };
    Ok(Picked { mode, transparency: transparency(mode) })
}

/// The shader output for a mode, unless the user forces one: straight for `PostMultiplied`, premultiplied otherwise.
pub fn output_for(mode: CompositeAlphaMode, forced: Option<Output>) -> Output {
    forced.unwrap_or(match mode {
        CompositeAlphaMode::PostMultiplied => Output::Straight,
        _ => Output::Premultiplied,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use CompositeAlphaMode::{Inherit, Opaque, PostMultiplied, PreMultiplied};

    #[test]
    fn prefers_premultiplied() {
        let p = pick(&[Opaque, PostMultiplied, PreMultiplied], None).unwrap();
        assert_eq!((p.mode, p.transparency), (PreMultiplied, Transparency::Yes));
    }

    #[test]
    fn falls_back_through_the_list() {
        assert_eq!(pick(&[Opaque, PostMultiplied], None).unwrap().mode, PostMultiplied);
        assert_eq!(pick(&[Opaque, Inherit], None).unwrap().transparency, Transparency::Unknown);
        let p = pick(&[Opaque], None).unwrap();
        assert_eq!((p.mode, p.transparency), (Opaque, Transparency::No));
    }

    #[test]
    fn an_explicit_request_is_honoured_or_refused() {
        assert_eq!(pick(&[Opaque, PostMultiplied], Some(Opaque)).unwrap().mode, Opaque);
        let err = pick(&[Opaque], Some(PreMultiplied)).unwrap_err();
        assert_eq!(err.offered, vec![Opaque]);
        assert!(err.to_string().contains("PreMultiplied"));
    }

    #[test]
    fn output_follows_the_mode_unless_forced() {
        assert_eq!(output_for(PreMultiplied, None), Output::Premultiplied);
        assert_eq!(output_for(PostMultiplied, None), Output::Straight);
        assert_eq!(output_for(Inherit, None), Output::Premultiplied);
        assert_eq!(output_for(PostMultiplied, Some(Output::Premultiplied)), Output::Premultiplied);
    }
}
