//! What a node needs before it may run (design D11): [`Need`], declared statically from its parameters.
//!
//! This is a different type from the executor's approval need, `omuzaki_ops_api::Need`; code that uses both names
//! them by path (design D21). The engine unions the needs of a show at compile: [`Need::Variable`] and [`Need::Group`]
//! are checked against the show's own declarations ([`Need::is_declaration`]), every other need against what the
//! person approved for this show on this machine. A node whose needs aren't all met is inert.

use omuzaki_ops_api::Scope;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ids::{GroupId, PortName, VarName};
use crate::show::VarType;

/// One thing a node needs. Ordered, so a show's needs form a set with a stable order.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Need {
    /// A scope beyond the base `read` and `perform`, through `op.run`'s target op (`broadcast`).
    Scope(Scope),
    /// An op `op.run` calls, so the approval card can say what it does ("switches OBS scenes").
    #[non_exhaustive]
    Op {
        /// The op's name (`obs.set_scene`).
        op: String,
    },
    /// A host the node sends to: `net.http`, `net.udp_send`. Scheme, host and port are literal parameters.
    #[non_exhaustive]
    Host {
        /// How it is reached.
        scheme: NetScheme,
        /// The host name or address, as written.
        host: String,
        /// The port.
        port: u16,
    },
    /// A file the node reads or writes: `file.*` nodes and file conditions. The path is literal.
    #[non_exhaustive]
    File {
        /// The path, as written.
        path: String,
        /// Whether the node writes it.
        write: bool,
    },
    /// A program the node runs: `exec.run`. Approved with its content hash ([`crate::services::ApprovedProgram`]).
    Program(Invocation),
    /// A secret the node sends to a host: `{secret:<name>}` in a `net.http` header. A secret only ever goes to the
    /// host it was approved for.
    #[non_exhaustive]
    Secret {
        /// The secret's name in the show (`streamerbot`).
        name: String,
        /// The host it may be sent to.
        host: String,
    },
    /// A process name the node asks about: `cond.process_running`. It never lists other processes.
    #[non_exhaustive]
    Process {
        /// The process name, as written.
        name: String,
    },
    /// A variable the node reads or writes, with the type it expects: checked against the show's declarations.
    #[non_exhaustive]
    Variable {
        /// The variable.
        name: VarName,
        /// The type the node expects.
        ty: VarType,
    },
    /// A group the node switches or reads: checked against the show's declared groups.
    #[non_exhaustive]
    Group {
        /// The group.
        group: GroupId,
    },
}

impl Need {
    /// An op `op.run` calls.
    pub fn op(op: impl Into<String>) -> Self {
        Need::Op { op: op.into() }
    }

    /// A host the node sends to.
    pub fn host(scheme: NetScheme, host: impl Into<String>, port: u16) -> Self {
        Need::Host { scheme, host: host.into(), port }
    }

    /// A file the node reads, or writes when `write`.
    pub fn file(path: impl Into<String>, write: bool) -> Self {
        Need::File { path: path.into(), write }
    }

    /// A secret sent to `host`.
    pub fn secret(name: impl Into<String>, host: impl Into<String>) -> Self {
        Need::Secret { name: name.into(), host: host.into() }
    }

    /// A process name the node asks about.
    pub fn process(name: impl Into<String>) -> Self {
        Need::Process { name: name.into() }
    }

    /// A variable of type `ty`.
    pub fn variable(name: VarName, ty: VarType) -> Self {
        Need::Variable { name, ty }
    }

    /// A group.
    pub fn group(group: GroupId) -> Self {
        Need::Group { group }
    }

    /// True for the needs the show's own declarations meet ([`Need::Variable`], [`Need::Group`]); the others need
    /// the person's approval.
    pub fn is_declaration(&self) -> bool {
        matches!(self, Need::Variable { .. } | Need::Group { .. })
    }
}

/// How a host is reached.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum NetScheme {
    /// Plain HTTP.
    Http,
    /// HTTP over TLS.
    Https,
    /// UDP datagrams.
    Udp,
}

/// A program and how it is called: what `exec.run` runs and what a person approves (design D11).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct Invocation {
    /// The program, as an absolute path: there is no `PATH` lookup, so "the exact program" is one file.
    pub program: String,
    /// A script the program runs, hashed with it, when the program is an interpreter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
    /// The arguments: literals and whole-argument placeholders. There is no shell anywhere.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<ArgTemplate>,
    /// The working folder; the program's own folder when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

impl Invocation {
    /// `program` with no script, no arguments and its own folder as the working folder.
    pub fn new(program: impl Into<String>) -> Self {
        Self { program: program.into(), script: None, args: Vec::new(), cwd: None }
    }

    /// With the script the program runs.
    #[must_use]
    pub fn with_script(mut self, script: impl Into<String>) -> Self {
        self.script = Some(script.into());
        self
    }

    /// With these arguments.
    #[must_use]
    pub fn with_args(mut self, args: Vec<ArgTemplate>) -> Self {
        self.args = args;
        self
    }

    /// With this working folder.
    #[must_use]
    pub fn with_cwd(mut self, cwd: impl Into<String>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }
}

/// One argument of an [`Invocation`].
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ArgTemplate {
    /// Exactly this text.
    Literal(String),
    /// The value of this text input, as exactly one argument: never split, never joined.
    Input(PortName),
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn needs_serialise_by_variant_name() {
        let host = Need::host(NetScheme::Http, "127.0.0.1", 7474);
        assert_eq!(
            serde_json::to_value(&host).unwrap(),
            json!({"host": {"scheme": "http", "host": "127.0.0.1", "port": 7474}})
        );
        assert_eq!(serde_json::to_value(Need::Scope(Scope::Net)).unwrap(), json!({"scope": "net"}));
        let program = Need::Program(
            Invocation::new("/usr/bin/obs").with_args(vec![ArgTemplate::Input(PortName::from_static("user"))]),
        );
        let back: Need = serde_json::from_value(serde_json::to_value(&program).unwrap()).unwrap();
        assert_eq!(back, program);
    }

    #[test]
    fn only_variables_and_groups_are_declarations() {
        assert!(Need::group(GroupId::new("redeems").unwrap()).is_declaration());
        assert!(Need::variable(VarName::new("bonks").unwrap(), VarType::Number).is_declaration());
        assert!(!Need::op("obs.set_scene").is_declaration() && !Need::Scope(Scope::Broadcast).is_declaration());
    }
}
