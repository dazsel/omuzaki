//! A node's ports: [`PortDecl`], the [`Ports`] builders a kind declares them with, and [`validate_ports`].

use std::collections::BTreeSet;

use super::{NodeClass, ParamError, ParamErrors};
use crate::ids::PortName;
use crate::l10n;
use crate::limits::MAX_PORTS_PER_NODE;
use crate::value::{Coercion, PortType, Val};

/// One port. Built through the [`Ports`] builders.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct PortDecl {
    /// Its name; inputs and outputs are separate namespaces.
    pub name: PortName,
    /// Its type.
    pub ty: PortType,
    /// What it is, in English: the `node-<id>--in-<port>` or `--out-<port>` text. `None` for a dynamic port.
    pub summary: Option<&'static str>,
    /// A dynamic port's label: the person's own text (a switch case), never translated.
    pub label: Option<String>,
    /// A value input's default.
    pub default: Option<Val>,
    /// A value output computed by [`Node::eval`](super::Node::eval) when read.
    pub live: bool,
}

impl PortDecl {
    fn fixed(name: PortName, ty: PortType, summary: &'static str) -> Self {
        Self { name, ty, summary: Some(summary), label: None, default: None, live: false }
    }

    fn dynamic(name: PortName, ty: PortType, label: String) -> Self {
        Self { name, ty, summary: None, label: Some(label), default: None, live: false }
    }
}

/// A kind's ports for one set of parameters, inputs and outputs each in display order.
///
/// ```
/// use omuzaki_live_api::node::Ports;
/// use omuzaki_live_api::value::{PortType, Val};
///
/// let ports = Ports::new()
///     .event_in("in", "Each event to delay.")
///     .value_in("seconds", PortType::Number, Val::Number(1.0), "How long to wait.")
///     .event_out("out", "The event, after the wait.");
/// assert_eq!((ports.inputs.len(), ports.outputs.len()), (2, 1));
/// ```
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct Ports {
    /// The inputs.
    pub inputs: Vec<PortDecl>,
    /// The outputs.
    pub outputs: Vec<PortDecl>,
    /// Names given to the builders that aren't valid port names; [`validate_ports`] reports them.
    invalid: Vec<String>,
}

impl Ports {
    /// No ports.
    pub fn new() -> Self {
        Self::default()
    }

    /// The input called `name`.
    pub fn input(&self, name: &str) -> Option<&PortDecl> {
        self.inputs.iter().find(|p| p.name.as_str() == name)
    }

    /// The output called `name`.
    pub fn output(&self, name: &str) -> Option<&PortDecl> {
        self.outputs.iter().find(|p| p.name.as_str() == name)
    }

    /// Adds an input, or records an invalid name for [`validate_ports`].
    fn push_in(mut self, name: &str, make: impl FnOnce(PortName) -> PortDecl) -> Self {
        match PortName::new(name) {
            Ok(name) => self.inputs.push(make(name)),
            Err(_) => self.invalid.push(name.to_owned()),
        }
        self
    }

    /// Adds an output, or records an invalid name for [`validate_ports`].
    fn push_out(mut self, name: &str, make: impl FnOnce(PortName) -> PortDecl) -> Self {
        match PortName::new(name) {
            Ok(name) => self.outputs.push(make(name)),
            Err(_) => self.invalid.push(name.to_owned()),
        }
        self
    }

    /// An event input.
    #[must_use]
    pub fn event_in(self, name: &'static str, summary: &'static str) -> Self {
        self.push_in(name, |name| PortDecl::fixed(name, PortType::Event, summary))
    }

    /// An event output.
    #[must_use]
    pub fn event_out(self, name: &'static str, summary: &'static str) -> Self {
        self.push_out(name, |name| PortDecl::fixed(name, PortType::Event, summary))
    }

    /// A value input with its default.
    #[must_use]
    pub fn value_in(self, name: &'static str, ty: PortType, default: Val, summary: &'static str) -> Self {
        self.push_in(name, |name| PortDecl { default: Some(default), ..PortDecl::fixed(name, ty, summary) })
    }

    /// A value output the node sets.
    #[must_use]
    pub fn value_out(self, name: &'static str, ty: PortType, summary: &'static str) -> Self {
        self.push_out(name, |name| PortDecl::fixed(name, ty, summary))
    }

    /// A value output computed by [`Node::eval`](super::Node::eval) when read.
    #[must_use]
    pub fn live_out(self, name: &'static str, ty: PortType, summary: &'static str) -> Self {
        self.push_out(name, |name| PortDecl { live: true, ..PortDecl::fixed(name, ty, summary) })
    }

    /// An input whose name and label come from the parameters (a switch case, a `text.format` placeholder).
    #[must_use]
    pub fn dynamic_in(mut self, name: PortName, ty: PortType, label: String) -> Self {
        self.inputs.push(PortDecl::dynamic(name, ty, label));
        self
    }

    /// An output whose name and label come from the parameters.
    #[must_use]
    pub fn dynamic_out(mut self, name: PortName, ty: PortType, label: String) -> Self {
        self.outputs.push(PortDecl::dynamic(name, ty, label));
        self
    }

    /// The same ports, with every output live: what a pure kind declares.
    #[must_use]
    pub fn all_live(mut self) -> Self {
        self.outputs.iter_mut().for_each(|p| p.live = true);
        self
    }
}

/// Checks a kind's ports against the rules every kind keeps (design D3, D4): valid and unique names in each
/// direction, at most [`MAX_PORTS_PER_NODE`] in all, no default or `live` on event ports, `live` only on value
/// outputs, defaults only on inputs and of the port's type; a [`NodeClass::Pure`] kind has no event ports and only live
/// outputs, a [`NodeClass::Trigger`] no event inputs.
///
/// # Errors
/// Every rule broken, each as a [`ParamError`] at `ports.<name>`.
pub fn validate_ports(ports: &Ports, class: NodeClass) -> Result<(), ParamErrors> {
    let mut errors = Vec::new();
    let mut fail = |port: &str, text| errors.push(ParamError::new(format!("ports.{port}"), text));
    for name in &ports.invalid {
        fail(name, l10n::port_bad_name(name.as_str()));
    }
    let total = ports.inputs.len() + ports.outputs.len() + ports.invalid.len();
    if total > MAX_PORTS_PER_NODE {
        fail(
            "",
            l10n::port_too_many(
                i64::try_from(total).unwrap_or(i64::MAX),
                i64::try_from(MAX_PORTS_PER_NODE).unwrap_or(i64::MAX),
            ),
        );
    }
    for (direction, list) in [("in", &ports.inputs), ("out", &ports.outputs)] {
        let mut seen = BTreeSet::new();
        for port in list {
            let name = port.name.as_str();
            if !seen.insert(name) {
                fail(name, l10n::port_duplicate(name));
            }
            if port.ty.is_event() && (port.default.is_some() || port.live) {
                fail(name, l10n::port_event_value(name));
            }
            if port.live && direction == "in" {
                fail(name, l10n::port_live_input(name));
            }
            if port.default.is_some() && direction == "out" {
                fail(name, l10n::port_output_default(name));
            }
            if let Some(default) = &port.default
                && !port.ty.is_event()
                && PortType::coercion(default.ty(), port.ty) != Coercion::Same
                && port.ty != PortType::Json
            {
                fail(name, l10n::port_default_type(name, port.ty.as_str()));
            }
            match class {
                NodeClass::Pure if port.ty.is_event() => fail(name, l10n::port_pure_event(name)),
                NodeClass::Pure if direction == "out" && !port.live => fail(name, l10n::port_pure_not_live(name)),
                NodeClass::Trigger if direction == "in" && port.ty.is_event() => {
                    fail(name, l10n::port_trigger_event_in(name));
                }
                NodeClass::Pure | NodeClass::Trigger | NodeClass::Active => {}
            }
        }
    }
    if errors.is_empty() { Ok(()) } else { Err(ParamErrors(errors)) }
}
