//! The two address rules every part of the connector path shares (D3, D8, D12): what host an endpoint names, and
//! whether a host is this computer. One copy, so the manifest check, the target and the host's key rule agree.

use std::net::IpAddr;

/// The scheme of an endpoint. Only these two are ever reached.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Scheme {
    /// `http://`: never carries a key off this computer (D8).
    Http,
    /// `https://`.
    Https,
}

/// The parts of an endpoint URL the rules read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    /// The scheme.
    pub scheme: Scheme,
    /// The host, lowercased, without brackets for an IPv6 address.
    pub host: String,
}

impl Endpoint {
    /// Reads `url`: `http://` or `https://`, then a host, an optional port and an optional path. A URL with user-info
    /// (`user@host`), another scheme, an empty host or a port that is not a number is `None`.
    pub fn parse(url: &str) -> Option<Self> {
        let (scheme, rest) = url.split_once("://")?;
        let scheme = match scheme.to_ascii_lowercase().as_str() {
            "http" => Scheme::Http,
            "https" => Scheme::Https,
            _ => return None,
        };
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        if authority.contains('@') {
            return None;
        }
        let (host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
            let (host, after) = bracketed.split_once(']')?;
            (host, after.strip_prefix(':'))
        } else {
            match authority.split_once(':') {
                Some((host, port)) => (host, Some(port)),
                None => (authority, None),
            }
        };
        if port.is_some_and(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit())) {
            return None;
        }
        let ok_host = !host.is_empty() && host.chars().all(|c| c.is_ascii_alphanumeric() || "-._:".contains(c));
        ok_host.then(|| Self { scheme, host: host.to_ascii_lowercase() })
    }

    /// Whether the host is this computer.
    pub fn is_loopback(&self) -> bool {
        is_loopback(&self.host)
    }
}

/// Whether `host` is this computer: `localhost` or a loopback address (`127.0.0.1`, `::1`).
pub fn is_loopback(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// The destination a manifest names for a program the host launches (MCP over stdio).
pub const STDIO: &str = "stdio";

/// Whether a manifest destination stays on this computer: a loopback host or [`STDIO`].
pub fn is_local_destination(destination: &str) -> bool {
    destination == STDIO || is_loopback(destination)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_parse_to_scheme_and_host() {
        let e = Endpoint::parse("http://127.0.0.1:8188/prompt").unwrap();
        assert_eq!((e.scheme, e.host.as_str(), e.is_loopback()), (Scheme::Http, "127.0.0.1", true));
        let e = Endpoint::parse("HTTPS://API.Example.com").unwrap();
        assert_eq!((e.scheme, e.host.as_str(), e.is_loopback()), (Scheme::Https, "api.example.com", false));
        assert_eq!(Endpoint::parse("http://[::1]:9/x").unwrap().host, "::1");
        assert!(Endpoint::parse("http://localhost").unwrap().is_loopback());
        for bad in ["ftp://x", "http://", "http://u:p@host", "http://host:", "http://host:8a", "host", "http://a b"] {
            assert_eq!(Endpoint::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn local_destinations_are_loopback_or_stdio() {
        for d in ["localhost", "127.0.0.1", "::1", "stdio"] {
            assert!(is_local_destination(d), "{d}");
        }
        for d in ["192.168.1.4", "api.example.com", "mock.invalid"] {
            assert!(!is_local_destination(d), "{d}");
        }
    }
}
