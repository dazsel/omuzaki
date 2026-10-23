//! `omuzaki mcp` while the studio has the project open (`agent-sessions.md` 10.2): instead of opening the project a
//! second time, relay the client's stdio into the studio, so the agent works in the person's executor, in front of
//! them, and undoes as itself.

use std::path::Path;

use omuzaki_mcp::bridge::{self, BridgeError};
use omuzaki_ops_api::OpError;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, BufReader};

use crate::AppError;
use crate::studio::{self, Credentials};

/// The name a client that doesn't say who it is gets.
const UNNAMED_CLIENT: &str = "mcp-client";

/// Relays this process's stdin and stdout into the studio that has `project` open, until the client closes stdin or
/// the studio ends the session. `data` is the user's local data folder, where the bridge keeps each client's
/// credential for this project, so the person pairs a client once.
///
/// # Errors
/// [`AppError::Op`] (`Conflict`) when no studio serves this project; [`AppError::Bridge`] when the studio refuses
/// the bridge or stops answering, or the client's stdio fails.
pub fn serve_stdio(project: &Path, data: &Path) -> Result<(), AppError> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(AppError::Runtime)?;
    let served = runtime.block_on(relay(project, data, BufReader::new(tokio::io::stdin()), tokio::io::stdout()));
    // Standard input is read on a blocking thread that only a new line or EOF would end: don't wait for it.
    runtime.shutdown_background();
    served
}

/// [`serve_stdio`] over any newline-delimited JSON-RPC stream.
///
/// # Errors
/// As [`serve_stdio`].
pub async fn relay<R, W>(project: &Path, data: &Path, mut input: R, output: W) -> Result<(), AppError>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let studio = studio::rendezvous(project).ok_or_else(|| {
        OpError::conflict("the project is open in another Omuzaki that isn't taking agents")
            .hint("open it in Omuzaki Studio (which takes agents), or close the other Omuzaki first")
    })?;
    // The first line is the client's `initialize`: it names the client, and the credential is per client.
    let mut first = String::new();
    if input.read_line(&mut first).await.map_err(BridgeError::Io)? == 0 {
        return Ok(());
    }
    let client = bridge::client_name(&first).unwrap_or_else(|| UNNAMED_CLIENT.to_owned());
    let mut kept = Credentials::load(data, project);
    let token = match kept.get(&client) {
        Some(token) if bridge::credential_accepted(&studio.url, token).await? => token.to_owned(),
        _ => {
            let token = bridge::request_credential(&studio.pair_url, &studio.key, &client).await?;
            kept.set(&client, token.clone());
            eprintln!("omuzaki: Omuzaki has this project open. Pair \"{client}\" in its window to let it work there.");
            token
        }
    };
    bridge::relay(&studio.url, &token, Some(first), input, output).await?;
    Ok(())
}
