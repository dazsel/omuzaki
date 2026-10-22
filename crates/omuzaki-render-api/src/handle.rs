//! Opaque handles to renderer-owned resources.

/// Declares a handle type: a `u64` that only the renderer that issued it can interpret.
///
/// The layout (slot index in the low 32 bits, generation in the high 32) is a convention shared by every renderer so
/// logs print the same way everywhere. Callers must never build or take apart a handle; only renderer implementations
/// use [`from_parts`](MeshId::from_parts) and friends. Why a generation: a released slot is reused, and without it a
/// stale handle would silently draw somebody else's mesh instead of failing with `UnknownHandle`.
macro_rules! handle {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(u64);

        impl $name {
            /// Packs a slot index and generation. For renderer implementations only.
            pub const fn from_parts(index: u32, generation: u32) -> Self {
                Self(((generation as u64) << 32) | index as u64)
            }

            /// The slot index. For renderer implementations only.
            #[allow(clippy::cast_possible_truncation)] // the low 32 bits are the index by construction
            pub const fn index(self) -> u32 {
                self.0 as u32
            }

            /// The generation. For renderer implementations only.
            #[allow(clippy::cast_possible_truncation)] // the high 32 bits are the generation by construction
            pub const fn generation(self) -> u32 {
                (self.0 >> 32) as u32
            }
        }

        impl ::std::fmt::Debug for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                write!(f, "{}(#{}.{})", stringify!($name), self.index(), self.generation())
            }
        }
    };
}

handle!(
    /// A mesh uploaded with [`Renderer::create_mesh`](crate::Renderer::create_mesh).
    MeshId
);
handle!(
    /// A texture created with [`Renderer::create_texture`](crate::Renderer::create_texture).
    TextureId
);
handle!(
    /// A material created with [`Renderer::create_material`](crate::Renderer::create_material).
    MaterialId
);
handle!(
    /// A render target created with [`Renderer::create_target`](crate::Renderer::create_target).
    TargetId
);

handle!(
    /// An environment (image-based light) created with
    /// [`Renderer::create_environment`](crate::Renderer::create_environment).
    EnvironmentId
);

handle!(
    /// A particle system created with
    /// [`Renderer::create_particle_system`](crate::Renderer::create_particle_system).
    ParticleSystemId
);

/// Any resource, for [`Renderer::release`](crate::Renderer::release).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ResourceRef {
    /// A mesh.
    Mesh(MeshId),
    /// A texture.
    Texture(TextureId),
    /// A material.
    Material(MaterialId),
    /// A render target.
    Target(TargetId),
    /// An environment.
    Environment(EnvironmentId),
    /// A particle system.
    ParticleSystem(ParticleSystemId),
}

macro_rules! resource_from {
    ($($ty:ident => $variant:ident),* $(,)?) => {
        $(impl From<$ty> for ResourceRef {
            fn from(id: $ty) -> Self {
                Self::$variant(id)
            }
        })*
    };
}
resource_from!(MeshId => Mesh, TextureId => Texture, MaterialId => Material, TargetId => Target, EnvironmentId => Environment,
    ParticleSystemId => ParticleSystem);

impl ResourceRef {
    /// The kind of resource, for error messages.
    pub const fn kind(self) -> &'static str {
        match self {
            Self::Mesh(_) => "mesh",
            Self::Texture(_) => "texture",
            Self::Material(_) => "material",
            Self::Target(_) => "target",
            Self::Environment(_) => "environment",
            Self::ParticleSystem(_) => "particle system",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parts_round_trip_and_print_readably() {
        let id = MeshId::from_parts(7, 3);
        assert_eq!((id.index(), id.generation()), (7, 3));
        assert_eq!(format!("{id:?}"), "MeshId(#7.3)");
        assert_ne!(MeshId::from_parts(7, 3), MeshId::from_parts(7, 4), "a reused slot is a different handle");
    }

    #[test]
    fn any_handle_converts_to_a_resource_ref() {
        assert_eq!(ResourceRef::from(TextureId::from_parts(1, 1)).kind(), "texture");
        assert_eq!(ResourceRef::from(TargetId::from_parts(1, 1)).kind(), "target");
        assert_eq!(ResourceRef::from(EnvironmentId::from_parts(1, 1)).kind(), "environment");
        assert_eq!(ResourceRef::from(ParticleSystemId::from_parts(1, 1)).kind(), "particle system");
    }
}
