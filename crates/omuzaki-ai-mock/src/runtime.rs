//! The `fake` runtime: its "model" is a small TOML file naming the ports and an affine function, so the registry's
//! probe, slot matching, `io` renames, refusals and crash fallback can be tested without ONNX Runtime or weights.
//!
//! ```toml
//! scale = 1.0          # y = scale * x + bias over input 0
//! bias = 0.0
//! crash_after = 3      # optional: every run after the third crashes, like a dying sandboxed worker
//! refuse = "custom op domain com.example"   # optional: the probe refuses the model with this reason
//!
//! [[inputs]]
//! name = "x"
//! dtype = "f32"
//! shape = ["any"]
//!
//! [[outputs]]
//! name = "y"
//! dtype = "f32"
//! shape = ["any"]
//! ```

use std::collections::BTreeMap;

use omuzaki_ai_api::{
    LoadError, ModelSource, Probe, Runtime, RuntimeId, Session, SessionOptions, Signature, TensorSpec, register_runtime,
};
use serde::Deserialize;

use crate::FakeSession;

/// A fake model file, as the `fake` runtime reads it.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct FakeModel {
    /// Multiplier.
    pub scale: f32,
    /// Offset.
    pub bias: f32,
    /// Runs before every further run crashes.
    #[serde(default)]
    pub crash_after: Option<u64>,
    /// When set, probing refuses the model with this reason (stands in for custom ops or native code).
    #[serde(default)]
    pub refuse: Option<String>,
    /// The model's inputs, with its own names.
    pub inputs: Vec<TensorSpec>,
    /// Its outputs.
    pub outputs: Vec<TensorSpec>,
}

impl FakeModel {
    fn read(source: &ModelSource<'_>) -> Result<Self, LoadError> {
        let main = source.files.first().ok_or_else(|| LoadError::MissingFile("(main file)".into()))?;
        let text = std::fs::read_to_string(&main.path).map_err(|e| LoadError::Format(e.to_string()))?;
        toml::from_str(&text).map_err(|e| LoadError::Format(e.to_string()))
    }

    fn signature(&self) -> Signature {
        Signature { inputs: self.inputs.clone(), outputs: self.outputs.clone() }
    }
}

/// The `fake` runtime. CPU only.
#[derive(Default)]
pub struct FakeRuntime;

impl FakeRuntime {
    /// Its id.
    pub const ID: &'static str = "fake";
}

impl Runtime for FakeRuntime {
    fn id(&self) -> RuntimeId {
        RuntimeId::new(Self::ID).unwrap_or_else(|_| unreachable!("a literal slug"))
    }

    fn probe(&self, source: &ModelSource<'_>) -> Result<Probe, LoadError> {
        let model = FakeModel::read(source)?;
        if let Some(reason) = model.refuse {
            return Err(LoadError::Refused(reason));
        }
        Ok(Probe { signature: model.signature(), opset: None })
    }

    fn load(
        &self,
        source: &ModelSource<'_>,
        slot: &Signature,
        io: &BTreeMap<String, String>,
        _options: &SessionOptions,
    ) -> Result<Box<dyn Session>, LoadError> {
        let model = FakeModel::read(source)?;
        if let Some(reason) = model.refuse {
            return Err(LoadError::Refused(reason));
        }
        slot.accepts(&model.signature().with_slot_names(io))
            .map_err(|m| LoadError::Format(format!("doesn't fit the slot: {m:?}")))?;
        let session = FakeSession::affine(slot.clone(), model.scale, model.bias).reporting(source.model.clone());
        Ok(Box::new(match model.crash_after {
            Some(n) => session.crashing_after(n),
            None => session,
        }))
    }
}

register_runtime!(FakeRuntime);
