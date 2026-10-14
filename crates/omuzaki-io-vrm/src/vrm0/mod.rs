//! VRM 0.x: the single `VRM` glTF extension, converted to and from the scene's 1.0-shaped avatar layer, with the model
//! turned between facing −Z (0.x) and +Z (the scene). The glTF around it is `omuzaki-gltf`'s; this module plugs into it
//! through [`ImportHooks`] and [`ExportHooks`].

mod materials;
mod meta;
mod read;
mod spring;
mod wire;
mod write;

use omuzaki_gltf::schema::Document;
use omuzaki_gltf::{Carrier, Disposition, ExportHooks, ImportHooks, RootCx, Tally, TargetNames, Unusable};
use omuzaki_io_api::{IoError, IoScene, Report, Severity};
use serde_json::Value;

/// The VRM 0.x extension.
pub(crate) const EXTENSION: &str = "VRM";

/// Reading a VRM 0.x file: interprets the `VRM` extension (everything is on the root, in one place, so the work is
/// done in the root hook) and collects the findings that are the VRM layer's.
pub(crate) struct Import {
    file: Option<String>,
    /// Findings of the avatar layer, to be merged after the generic ones.
    pub(crate) report: Report,
}

impl Import {
    /// Hooks for importing the file called `file`, which the provenance record names.
    pub(crate) fn new(file: Option<&str>) -> Self {
        Self { file: file.map(str::to_owned), report: Report::new() }
    }
}

impl ImportHooks for Import {
    fn supported_extensions(&self) -> &[&str] {
        &[EXTENSION]
    }

    fn extension(&mut self, carrier: Carrier, name: &str, _body: &Value) -> Disposition {
        if carrier == Carrier::Root && name == EXTENSION {
            Disposition::Interpreted
        } else if name.starts_with("VRMC_") {
            Disposition::Discard {
                severity: Severity::Info,
                message: format!("the VRM 1.0 extension {name} was ignored: this file is read as VRM 0.x"),
                hint: Some("import the file as vrm1 to read the 1.0 data"),
            }
        } else {
            Disposition::Carry
        }
    }

    /// Thumbnails and expressions refer to textures and materials by file index, so none may move.
    fn unusable(&self) -> Unusable {
        Unusable::Refuse
    }

    fn root(&mut self, cx: &mut RootCx<'_>) -> Result<(), IoError> {
        read::read(cx.document, self.file.as_deref(), cx.scene, &mut self.report)
    }
}

/// Writing a VRM 0.x file: adds the `VRM` extension to the generic document. The scene it is given has already been
/// turned to face −Z.
#[derive(Default)]
pub(crate) struct Export {
    /// Findings of the avatar layer, to be merged after the generic ones.
    pub(crate) report: Report,
}

impl ExportHooks for Export {
    /// 0.x exporters put morph target names on the primitives.
    fn target_names(&self) -> TargetNames {
        TargetNames::OnPrimitives
    }

    fn root(&mut self, scene: &IoScene, document: &mut Document, _tally: &mut Tally) {
        write::write(scene, document, &mut self.report);
    }
}
