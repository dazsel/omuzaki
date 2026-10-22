//! Self-registration (D3, ADR 0015, ADR 0017 decision 1): each connector crate calls
//! [`register_connector!`](crate::register_connector) next to its code with its embedded manifest, and the binary
//! carries exactly the connectors it links.

use crate::connector::Connector;
use crate::id::ConnectorId;
use crate::manifest::{ConnectorManifest, ManifestError};

/// One registered connector: its id, its manifest's TOML text and how to build it.
pub struct ConnectorRegistration {
    /// The id, as the macro's first argument names it.
    pub id: &'static str,
    /// The manifest, `include_str!` of `connectors/<id>.connector.toml`.
    pub manifest: &'static str,
    /// Builds a fresh connector.
    pub make: fn() -> Box<dyn Connector>,
}

inventory::collect!(ConnectorRegistration);

fn make_default<T: Connector + Default + 'static>() -> Box<dyn Connector> {
    Box::new(T::default())
}

impl ConnectorRegistration {
    /// The registration of `T`, built with `T::default()`.
    pub const fn of<T: Connector + Default + 'static>(id: &'static str, manifest: &'static str) -> Self {
        Self { id, manifest, make: make_default::<T> }
    }

    /// The manifest, read and checked against the registered id. [`ConnectorManifest::validate`] is separate, so a
    /// caller can show every rule a shipped manifest breaks.
    ///
    /// # Errors
    /// [`ManifestError::Parse`] if it does not read, [`ManifestError::Field`] on `id` if it names another connector.
    pub fn parse(&self) -> Result<ConnectorManifest, ManifestError> {
        let m = ConnectorManifest::from_toml(self.manifest)?;
        if m.id.as_str() == self.id {
            Ok(m)
        } else {
            Err(ManifestError::field(
                "id",
                format!("the registration names {} but the manifest says {}", self.id, m.id),
            ))
        }
    }
}

/// Registers a connector: `register_connector!("comfyui", include_str!("../connectors/comfyui.connector.toml"),
/// ComfyUi);`. The type implements [`Connector`] and `Default`. Also records the registration for the link gate (kind
/// `connector`, named by the id literal).
#[macro_export]
macro_rules! register_connector {
    ($id:literal, $manifest:expr, $ty:ty) => {
        $crate::__inventory::submit! { $crate::ConnectorRegistration::of::<$ty>($id, $manifest) }
        $crate::__record_registration!("connector", $id);
    };
}

/// Every connector registered in this binary, in no particular order.
pub fn registered() -> impl Iterator<Item = &'static ConnectorRegistration> {
    inventory::iter::<ConnectorRegistration>.into_iter()
}

/// The registration of `id`, if this binary has one.
pub fn find(id: &ConnectorId) -> Option<&'static ConnectorRegistration> {
    registered().find(|r| r.id == id.as_str())
}
