//! `input.vmc_out`: send the performed pose and face to another app over VMC (PF-04's "out").

use std::net::SocketAddr;

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{changed, host, mode, op_error};

/// The address VMC receivers listen on by default: an app on this computer, port 39539.
pub const DEFAULT_TO: &str = "127.0.0.1:39539";

/// Send the character's performed pose and face to another app over VMC (another avatar app, a streaming tool, a game
/// engine's VMC plugin), or stop with `to: null`. `to` is the receiver's `host:port`; 127.0.0.1:39539 is an app on
/// this computer listening on the usual port. The body goes as humanoid bones and the face as perfect-sync
/// blendshapes, 60 times a second. Sending to another address moves the output.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(extend("examples" = [{ "to": "127.0.0.1:39539" }, { "to": null }]))]
pub struct VmcOut {
    /// The receiver's `host:port`, or `null` to stop.
    pub to: Option<String>,
}

impl Op for VmcOut {
    fn meta() -> OpMeta {
        OpMeta {
            name: "input.vmc_out",
            summary: "Send the performed pose and face to another app over VMC (to: host:port), or stop (to: null).",
            kind: OpKind::Edit,
            scope: Scope::Perform,
            undo: Undo::Explicit,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let target = self.to.as_deref().map(parse).transpose()?;
        // Stopping only takes the stream away, so anyone may; sending the performance out is the person's to confirm.
        if target.is_some() {
            ctx.require_person("VMC out sends the performance to another app")?;
        }
        let before = host(ctx)?.set_pose_out(target, mode(ctx)).map_err(op_error)?;
        let before = before.target.map(|t| t.to_string());
        let after = target.map(|t| t.to_string());
        let answer = json!({ "to": after, "before": before, "dry_run": ctx.dry_run });
        let summary = after.as_ref().map_or_else(|| "VMC out stopped".to_owned(), |to| format!("VMC out to {to}"));
        changed(Self::meta().name, "to", before, after, summary, answer)
    }
}

/// `to` as an address: `host:port` with an IP host, or `localhost`.
fn parse(to: &str) -> Result<SocketAddr, OpError> {
    let to = to.trim();
    let resolved = to.strip_prefix("localhost:").map_or_else(|| to.to_owned(), |port| format!("127.0.0.1:{port}"));
    resolved
        .parse()
        .map_err(|_| OpError::invalid("to", format!("`{to}` is not a host:port address")).hint(format!("for example {DEFAULT_TO}")))
}

omuzaki_ops_api::register_op!(VmcOut);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_are_ip_and_port_or_localhost() {
        assert_eq!(parse(DEFAULT_TO).unwrap(), "127.0.0.1:39539".parse().unwrap());
        assert_eq!(parse(" localhost:39540 ").unwrap(), "127.0.0.1:39540".parse().unwrap());
        assert_eq!(parse("[::1]:39539").unwrap(), "[::1]:39539".parse().unwrap());
        assert!(parse("127.0.0.1").is_err());
        assert!(parse("example:39539").is_err());
    }
}
