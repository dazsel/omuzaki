//! The `EventSub` socket: a WebSocket over TCP, with TLS (rustls, checked against the operating system's trust store)
//! for `wss://`. Plain `ws://` is for a test server on this machine only.
//!
//! Reads time out every [`READ_TICK`], so the connection thread can watch its stop flag and the keepalive deadline
//! without a second thread.

use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, StreamOwned};
use rustls_platform_verifier::BuilderVerifierExt;
use tungstenite::WebSocket;
use tungstenite::client::IntoClientRequest;

use crate::error::TwitchError;

/// How long a read waits before the thread looks around.
pub(crate) const READ_TICK: Duration = Duration::from_millis(100);
/// How long connecting may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// A TCP stream, encrypted or (to a loopback test server) not.
pub(crate) enum Stream {
    Plain(TcpStream),
    Tls(Box<StreamOwned<ClientConnection, TcpStream>>),
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(buf),
            Stream::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(buf),
            Stream::Tls(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            Stream::Tls(s) => s.flush(),
        }
    }
}

/// The TLS settings: the `ring` provider and the OS trust store.
fn tls_config() -> Result<Arc<ClientConfig>, TwitchError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .and_then(BuilderVerifierExt::with_platform_verifier)
        .map_err(|e| TwitchError::Network(format!("TLS couldn't be set up: {e}")))?
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// Whether `host` is this machine.
fn loopback(host: &str) -> bool {
    host == "localhost" || host.trim_matches(['[', ']']).parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Opens the WebSocket at `url`.
///
/// # Errors
/// [`TwitchError::Network`] when the address is bad, can't be reached, or the handshake fails; plain `ws://` to any
/// host but this machine is refused.
pub(crate) fn connect(url: &str) -> Result<WebSocket<Stream>, TwitchError> {
    let net = |what: &str, e: &dyn std::fmt::Display| TwitchError::Network(format!("{what}: {e}"));
    let request = url.into_client_request().map_err(|e| net("not a WebSocket address", &e))?;
    let uri = request.uri().clone();
    let host = uri.host().ok_or_else(|| TwitchError::Network("the address has no host".to_owned()))?.to_owned();
    let secure = match uri.scheme_str() {
        Some("wss") => true,
        Some("ws") if loopback(&host) => false,
        _ => return Err(TwitchError::Network("only wss:// addresses are allowed".to_owned())),
    };
    let port = uri.port_u16().unwrap_or(if secure { 443 } else { 80 });
    let addr = (host.trim_matches(['[', ']']), port)
        .to_socket_addrs()
        .map_err(|e| net("couldn't look up the host", &e))?
        .next()
        .ok_or_else(|| TwitchError::Network("the host has no address".to_owned()))?;
    let tcp = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).map_err(|e| net("couldn't connect", &e))?;
    tcp.set_nodelay(true).map_err(|e| net("couldn't set up the socket", &e))?;
    tcp.set_read_timeout(Some(CONNECT_TIMEOUT)).map_err(|e| net("couldn't set up the socket", &e))?;
    let stream = if secure {
        let name =
            ServerName::try_from(host.trim_matches(['[', ']']).to_owned()).map_err(|e| net("not a host name", &e))?;
        let conn = ClientConnection::new(tls_config()?, name).map_err(|e| net("TLS failed", &e))?;
        Stream::Tls(Box::new(StreamOwned::new(conn, tcp)))
    } else {
        Stream::Plain(tcp)
    };
    let (socket, _) =
        tungstenite::client::client(request, stream).map_err(|e| net("the WebSocket handshake failed", &e))?;
    let tcp = match socket.get_ref() {
        Stream::Plain(s) => s,
        Stream::Tls(s) => s.get_ref(),
    };
    tcp.set_read_timeout(Some(READ_TICK)).map_err(|e| net("couldn't set up the socket", &e))?;
    Ok(socket)
}

/// Whether an error is only a read that timed out.
pub(crate) fn timed_out(error: &tungstenite::Error) -> bool {
    matches!(error, tungstenite::Error::Io(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_websockets_reach_only_this_machine() {
        assert!(matches!(connect("ws://example.com/ws"), Err(TwitchError::Network(m)) if m.contains("wss")));
        assert!(loopback("127.0.0.1") && loopback("[::1]") && loopback("localhost") && !loopback("10.0.0.1"));
    }

    #[test]
    fn tls_sets_up_with_the_os_trust_store() {
        assert!(tls_config().is_ok());
    }
}
