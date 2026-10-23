//! The stdio bridge (`agent-sessions.md` 10.2): while the studio holds a project, `omuzaki mcp` doesn't open it a
//! second time. It relays its client's stdio to the studio's loopback port instead, with a bridge credential of its own,
//! so the agent works in the studio's executor and the person sees it at once.
//!
//! The pieces, in the order a bridge uses them: [`request_credential`] asks the studio for a credential with the
//! pairing key the studio wrote for it; [`credential_accepted`] checks a stored one still works; [`relay`] carries the
//! client's messages until either side ends. Pairing itself happens in the studio: the first session with a new
//! credential waits on a card until the person pairs it, and agents can't pair themselves.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use reqwest::StatusCode;
use reqwest::header::AUTHORIZATION;
use rmcp::model::{ClientJsonRpcMessage, ServerJsonRpcMessage};
use rmcp::service::RoleClient;
use rmcp::transport::Transport;
use rmcp::transport::streamable_http_client::{StreamableHttpClientTransport, StreamableHttpClientTransportConfig};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

/// How long a request to the studio (for a credential, or the check) may take: it is on this machine.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Why the bridge couldn't reach the studio or carry a message.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BridgeError {
    /// The studio's port didn't answer: it closed, or isn't the studio any more.
    #[error("can't reach Omuzaki at {url}: {reason}")]
    Unreachable {
        /// The URL that was tried.
        url: String,
        /// What went wrong.
        reason: String,
    },
    /// The studio answered, and refused.
    #[error("Omuzaki refused the bridge ({status}): {message}")]
    Refused {
        /// The HTTP status.
        status: u16,
        /// What the studio said.
        message: String,
    },
    /// A line from the client isn't a JSON-RPC message.
    #[error("the client sent a line that isn't a JSON-RPC message: {0}")]
    Malformed(String),
    /// Reading from the client or writing to it failed.
    #[error("the client's stdio failed: {0}")]
    Io(#[from] std::io::Error),
}

/// A client for the studio's loopback port. Never through a proxy: the port is on this machine.
fn http_client() -> Result<reqwest::Client, BridgeError> {
    install_crypto_provider();
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .map_err(|e| BridgeError::Unreachable { url: "127.0.0.1".to_owned(), reason: e.to_string() })
}

/// reqwest may be built with rustls and no crypto provider (another crate in the binary asks for that), and then a
/// client can't be built until one is installed. Plain HTTP never uses it; installing `ring` once makes the build work.
fn install_crypto_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // Err means a provider is already installed, which is fine.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// Asks the studio at `pair_url` for a new bridge credential for `client`, presenting the pairing `key`. The credential
/// is unpaired: the first session with it waits until the person pairs it in Omuzaki's window. Store it (owner-only)
/// and reuse it, so the person pairs each client once.
///
/// # Errors
/// [`BridgeError::Unreachable`] if the studio doesn't answer; [`BridgeError::Refused`] if it refuses (a wrong key,
/// too many credentials waiting).
pub async fn request_credential(pair_url: &str, key: &str, client: &str) -> Result<String, BridgeError> {
    let unreachable = |e: reqwest::Error| BridgeError::Unreachable { url: pair_url.to_owned(), reason: e.to_string() };
    let response = http_client()?
        .post(pair_url)
        .header(AUTHORIZATION, format!("Bearer {key}"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(serde_json::json!({ "client": client }).to_string())
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(unreachable)?;
    let status = response.status();
    let body = response.text().await.map_err(unreachable)?;
    if status != StatusCode::OK {
        return Err(BridgeError::Refused { status: status.as_u16(), message: body });
    }
    serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("token").and_then(serde_json::Value::as_str).map(str::to_owned))
        .ok_or_else(|| BridgeError::Refused { status: status.as_u16(), message: format!("no token in {body}") })
}

/// Whether the studio's port at `url` still accepts `token` (false: it was revoked, or the studio forgot it, so ask
/// for a new one).
///
/// # Errors
/// [`BridgeError::Unreachable`] if the studio doesn't answer.
pub async fn credential_accepted(url: &str, token: &str) -> Result<bool, BridgeError> {
    let response = http_client()?
        .get(url)
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .header(reqwest::header::ACCEPT, "text/event-stream")
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|e| BridgeError::Unreachable { url: url.to_owned(), reason: e.to_string() })?;
    // Without a session the port refuses the stream, but only a bad token gets 401.
    Ok(response.status() != StatusCode::UNAUTHORIZED)
}

/// The client's name from its `initialize` request (one JSON-RPC line), if that is what `line` is.
pub fn client_name(line: &str) -> Option<String> {
    let message: serde_json::Value = serde_json::from_str(line).ok()?;
    (message.get("method")? == "initialize")
        .then(|| message.pointer("/params/clientInfo/name")?.as_str().map(str::to_owned))
        .flatten()
}

type Sending = Pin<Box<dyn Future<Output = Result<(), BridgeError>> + Send>>;

/// Relays one MCP client between `input` and `output` (newline-delimited JSON-RPC, as on stdio) and the studio's port
/// at `url`, presenting `token`, until the client closes its input or the studio ends the session. `first` is a line
/// already read from `input` (the bridge reads the `initialize` to learn the client's name), sent before the rest.
///
/// # Errors
/// [`BridgeError::Malformed`] for a line that isn't JSON-RPC, [`BridgeError::Unreachable`] if the studio's port stops
/// answering, and [`BridgeError::Io`] if the client's stdio fails.
pub async fn relay<R, W>(
    url: &str,
    token: &str,
    first: Option<String>,
    input: R,
    mut output: W,
) -> Result<(), BridgeError>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let config = StreamableHttpClientTransportConfig::with_uri(url.to_owned()).auth_header(token.to_owned());
    let mut http: StreamableHttpClientTransport<reqwest::Client> =
        StreamableHttpClientTransport::with_client(http_client()?, config);
    // Sends go out in order, one at a time, on their own task, so a slow one never holds up the studio's answers.
    let (queue, mut queued) = mpsc::unbounded_channel::<Sending>();
    let mut sender = tokio::spawn(async move {
        while let Some(sending) = queued.recv().await {
            sending.await?;
        }
        Ok::<(), BridgeError>(())
    });
    let forward = |http: &mut StreamableHttpClientTransport<reqwest::Client>, line: &str| {
        let message: ClientJsonRpcMessage =
            serde_json::from_str(line).map_err(|e| BridgeError::Malformed(e.to_string()))?;
        let url = url.to_owned();
        let sending = Transport::<RoleClient>::send(http, message);
        let _ = queue.send(Box::pin(async move {
            sending.await.map_err(|e| BridgeError::Unreachable { url, reason: e.to_string() })
        }));
        Ok::<(), BridgeError>(())
    };
    if let Some(first) = first {
        forward(&mut http, &first)?;
    }
    let mut lines = input.lines();
    let result = loop {
        tokio::select! {
            line = lines.next_line() => match line {
                Ok(Some(line)) if line.trim().is_empty() => {}
                Ok(Some(line)) => if let Err(e) = forward(&mut http, &line) { break Err(e) },
                Ok(None) => break Ok(()),
                Err(e) => break Err(e.into()),
            },
            message = Transport::<RoleClient>::receive(&mut http) => match message {
                Some(message) => if let Err(e) = write(&mut output, &message).await { break Err(e) },
                None => break Ok(()),
            },
            sent = &mut sender => break match sent {
                Ok(result) => result,
                Err(join) => Err(BridgeError::Unreachable { url: url.to_owned(), reason: join.to_string() }),
            },
        }
    };
    // Ends the HTTP session (`DELETE`), so the studio ends the agent's session at once rather than at its idle timeout.
    if let Err(error) = Transport::<RoleClient>::close(&mut http).await {
        tracing::debug!(%error, "the bridge's HTTP transport didn't close cleanly");
    }
    sender.abort();
    result
}

async fn write<W: AsyncWrite + Unpin>(output: &mut W, message: &ServerJsonRpcMessage) -> Result<(), BridgeError> {
    let mut line = serde_json::to_vec(message).map_err(|e| BridgeError::Malformed(e.to_string()))?;
    line.push(b'\n');
    output.write_all(&line).await?;
    output.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_client_name_comes_from_initialize_only() {
        let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"clientInfo":{"name":"claude-code","version":"1"}}}"#;
        assert_eq!(client_name(init).as_deref(), Some("claude-code"));
        assert_eq!(client_name(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#), None);
        assert_eq!(client_name("not json"), None);
    }
}
