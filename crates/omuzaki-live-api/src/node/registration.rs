//! The registry of node kinds (ADR 0015): what [`register_node!`](crate::register_node) submits, the lookups, and
//! [`DynNodeKind`], the object the engine and the surfaces hold for static and plugin kinds alike (design D15).

use schemars::Schema;
use serde_json::Value;
use serde_path_to_error::Segment;

use super::{Node, NodeKind, NodeMeta, ParamErrors, Ports, validate_ports};
use crate::ids::{EventKind, IdError, NodeKindId};
use crate::l10n;
use crate::need::Need;

/// One registered node kind: function pointers, so [`register_node!`](crate::register_node) works at link time. Every
/// function takes the parameters as JSON and decodes them the way [`check`](Self::check) does.
#[non_exhaustive]
pub struct NodeRegistration {
    /// The kind's identity.
    pub meta: fn() -> NodeMeta,
    /// The parameters' JSON Schema, inlined like op schemas (no `$ref`).
    pub schema: fn() -> Schema,
    /// The default parameters.
    pub defaults: fn() -> Value,
    /// Decodes the parameters (missing top-level keys from the defaults, unknown keys refused), runs the kind's
    /// check, and returns them normalised.
    pub check: fn(&Value) -> Result<Value, ParamErrors>,
    /// The ports for the parameters, checked by [`validate_ports`].
    pub ports: fn(&Value) -> Result<Ports, ParamErrors>,
    /// The event kinds a trigger listens to.
    pub listens: fn(&Value) -> Result<Vec<EventKind>, ParamErrors>,
    /// What running it needs (design D11).
    pub needs: fn(&Value) -> Result<Vec<Need>, ParamErrors>,
    /// A new instance.
    pub new: fn(&Value) -> Result<Box<dyn Node>, ParamErrors>,
}

impl NodeRegistration {
    /// The registration of `K`.
    pub const fn of<K: NodeKind>() -> Self {
        Self {
            meta: K::meta,
            schema: schema_of::<K>,
            defaults: defaults_of::<K>,
            check: check_as::<K>,
            ports: ports_as::<K>,
            listens: listens_as::<K>,
            needs: needs_as::<K>,
            new: new_as::<K>,
        }
    }

    /// The kind's id, checked.
    ///
    /// # Errors
    /// The [`IdError`] if the id in its [`NodeMeta`] isn't a valid [`NodeKindId`].
    pub fn kind_id(&self) -> Result<NodeKindId, IdError> {
        NodeKindId::new((self.meta)().id)
    }
}

impl std::fmt::Debug for NodeRegistration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeRegistration").field("id", &(self.meta)().id).finish_non_exhaustive()
    }
}

/// Inlined like op schemas: agents and the inspector read flat schemas more reliably.
fn schema_of<K: NodeKind>() -> Schema {
    schemars::generate::SchemaSettings::draft2020_12()
        .with(|s| s.inline_subschemas = true)
        .into_generator()
        .into_root_schema_for::<K::Params>()
}

fn defaults_of<K: NodeKind>() -> Value {
    serde_json::to_value(K::Params::default()).unwrap_or(Value::Null)
}

/// The parameters as `K::Params`: the defaults' top-level keys, overlaid by the given ones, decoded (so unknown keys
/// are refused at their path), then the kind's own check.
fn decode<K: NodeKind>(params: &Value) -> Result<K::Params, ParamErrors> {
    let mut merged = match defaults_of::<K>() {
        Value::Object(map) => map,
        // Parameters that aren't a table have no keys to fill: decode what was given.
        _ => serde_json::Map::new(),
    };
    match params {
        Value::Null => {}
        Value::Object(given) => merged.extend(given.iter().map(|(k, v)| (k.clone(), v.clone()))),
        _ => return Err(ParamErrors::one("", l10n::params_not_object())),
    }
    let decoded: K::Params = serde_path_to_error::deserialize(Value::Object(merged))
        .map_err(|e| ParamErrors::one(path_of(&e), l10n::params_invalid(e.inner().to_string())))?;
    K::check(&decoded)?;
    Ok(decoded)
}

/// The path to the bad value, dotted (`where.reward_title`); a missing or unknown key is named too, since serde
/// reports it at its parent.
fn path_of(e: &serde_path_to_error::Error<serde_json::Error>) -> String {
    let mut segments: Vec<String> = e
        .path()
        .iter()
        .filter_map(|segment| match segment {
            Segment::Seq { index } => Some(index.to_string()),
            Segment::Map { key } | Segment::Enum { variant: key } => Some(key.clone()),
            Segment::Unknown => None,
        })
        .collect();
    let message = e.inner().to_string();
    let named = ["missing field `", "unknown field `"]
        .iter()
        .find_map(|prefix| message.strip_prefix(prefix)?.split_once('`').map(|(key, _)| key.to_owned()));
    if let Some(key) = named
        && segments.last() != Some(&key)
    {
        segments.push(key);
    }
    segments.join(".")
}

fn check_as<K: NodeKind>(params: &Value) -> Result<Value, ParamErrors> {
    let decoded = decode::<K>(params)?;
    serde_json::to_value(&decoded).map_err(|e| ParamErrors::one("", l10n::internal(e.to_string())))
}

fn ports_as<K: NodeKind>(params: &Value) -> Result<Ports, ParamErrors> {
    let ports = K::ports(&decode::<K>(params)?);
    validate_ports(&ports, K::meta().class)?;
    Ok(ports)
}

fn listens_as<K: NodeKind>(params: &Value) -> Result<Vec<EventKind>, ParamErrors> {
    Ok(K::listens(&decode::<K>(params)?))
}

fn needs_as<K: NodeKind>(params: &Value) -> Result<Vec<Need>, ParamErrors> {
    Ok(K::needs(&decode::<K>(params)?))
}

fn new_as<K: NodeKind>(params: &Value) -> Result<Box<dyn Node>, ParamErrors> {
    Ok(Box::new(K::new(decode::<K>(params)?)))
}

inventory::collect!(NodeRegistration);

/// Registers a node kind (ADR 0015), next to it: `register_node!(Wait);`.
#[macro_export]
macro_rules! register_node {
    ($ty:ty) => {
        $crate::__inventory::submit! { $crate::node::NodeRegistration::of::<$ty>() }
        $crate::__record_registration!("node", $ty);
    };
}

/// Every registered node kind, in link order. Surfaces sort by id before showing them.
pub fn node_kinds() -> impl Iterator<Item = &'static NodeRegistration> {
    inventory::iter::<NodeRegistration>.into_iter()
}

/// The registration of the kind `id`. Linear: the engine builds an index once.
pub fn find_node_kind(id: &str) -> Option<&'static NodeRegistration> {
    node_kinds().find(|r| (r.meta)().id == id)
}

/// A node kind as the engine and the surfaces hold it: a static [`NodeRegistration`] or a plugin's kind (design D15).
pub trait DynNodeKind: Send + Sync {
    /// The kind's identity.
    fn meta(&self) -> NodeMeta;
    /// The parameters' JSON Schema, inlined.
    fn schema(&self) -> Schema;
    /// The default parameters.
    fn defaults(&self) -> Value;
    /// The parameters, checked and normalised.
    ///
    /// # Errors
    /// Everything wrong with them.
    fn check(&self, params: &Value) -> Result<Value, ParamErrors>;
    /// The ports for the parameters.
    ///
    /// # Errors
    /// As [`check`](Self::check), and invalid ports.
    fn ports(&self, params: &Value) -> Result<Ports, ParamErrors>;
    /// The event kinds a trigger listens to.
    ///
    /// # Errors
    /// As [`check`](Self::check).
    fn listens(&self, params: &Value) -> Result<Vec<EventKind>, ParamErrors>;
    /// What running it needs.
    ///
    /// # Errors
    /// As [`check`](Self::check).
    fn needs(&self, params: &Value) -> Result<Vec<Need>, ParamErrors>;
    /// A new instance.
    ///
    /// # Errors
    /// As [`check`](Self::check).
    fn instantiate(&self, params: &Value) -> Result<Box<dyn Node>, ParamErrors>;
}

impl DynNodeKind for NodeRegistration {
    fn meta(&self) -> NodeMeta {
        (self.meta)()
    }

    fn schema(&self) -> Schema {
        (self.schema)()
    }

    fn defaults(&self) -> Value {
        (self.defaults)()
    }

    fn check(&self, params: &Value) -> Result<Value, ParamErrors> {
        (self.check)(params)
    }

    fn ports(&self, params: &Value) -> Result<Ports, ParamErrors> {
        (self.ports)(params)
    }

    fn listens(&self, params: &Value) -> Result<Vec<EventKind>, ParamErrors> {
        (self.listens)(params)
    }

    fn needs(&self, params: &Value) -> Result<Vec<Need>, ParamErrors> {
        (self.needs)(params)
    }

    fn instantiate(&self, params: &Value) -> Result<Box<dyn Node>, ParamErrors> {
        (self.new)(params)
    }
}
