//! Access tokens for the HTTP port: who may connect, and with which grants.
//!
//! A token is a random secret the user hands to one client (in its `Authorization: Bearer` header). The registry maps
//! it to an [`Endpoint`]: an rmcp Streamable HTTP service whose sessions all hold that token's grants. One service per
//! token, rather than one shared service that looks the grants up on every request, means a session id is only
//! meaningful to the token that opened it. A client can't present another token's session, and revoking a token ends
//! its sessions.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use constant_time_eq::constant_time_eq;
use hyper::HeaderMap;
use hyper::header::AUTHORIZATION;
use omuzaki_ops_api::{Actor, Grants};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{SessionManager, StreamableHttpServerConfig, StreamableHttpService};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::error::ServerError;
use crate::limits::{Admission, Limits, SessionSlots, SessionsFull};
use crate::server::McpServer;
use crate::session::Session;

/// Every secret starts with this, so a leaked one is recognisable (and findable by secret scanners).
const SECRET_PREFIX: &str = "omuzaki_mcp_";
/// 256 bits of OS randomness.
const SECRET_BYTES: usize = 32;
/// The size of a BLAKE3 hash.
const HASH_BYTES: usize = 32;
const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";
/// Domain separation for [`hash_secret`]. The derived key is public, not a secret: the secrets are 256 random bits, so
/// nothing needs salting or stretching, and the key only makes these hashes unusable for any other BLAKE3 purpose.
const HASH_CONTEXT: &str = "omuzaki-mcp 2026-10 http access token hash";
/// The only hosts a loopback-only server can legitimately be reached by. rmcp checks the `Host` header against these,
/// which is what stops a DNS-rebinding page (`Host: evil.example`) from reaching the port.
const LOOPBACK_HOSTS: [&str; 3] = ["localhost", "127.0.0.1", "::1"];

/// Names an access token in logs and for [`HttpServer::revoke`](crate::HttpServer::revoke). Not secret.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct TokenId(u64);

impl fmt::Display for TokenId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "token-{}", self.0)
    }
}

/// A secret that lets one client use the HTTP port with fixed grants. Returned once by
/// [`HttpServer::issue`](crate::HttpServer::issue): hand [`secret`](Self::secret) to the client and don't log it.
#[derive(Clone)]
pub struct AccessToken {
    id: TokenId,
    secret: String,
    record: TokenRecord,
}

impl AccessToken {
    /// Names this token for [`HttpServer::revoke`](crate::HttpServer::revoke).
    pub fn id(&self) -> TokenId {
        self.id
    }

    /// What the user called this client, e.g. "Codex on this laptop". Shown in logs.
    pub fn label(&self) -> &str {
        &self.record.label
    }

    /// The scopes every session opened with this token holds.
    pub fn grants(&self) -> &Grants {
        &self.record.grants
    }

    /// The record as it was when this token was issued: what the host stores to re-admit it after a restart. It holds
    /// no secret, only its hash.
    ///
    /// This is a snapshot that does not follow [`HttpServer::set_paired`](crate::HttpServer::set_paired) or
    /// [`HttpServer::set_grants`](crate::HttpServer::set_grants). After either, store
    /// [`HttpServer::record`](crate::HttpServer::record)`(token.id())` instead, or the change is lost on restart.
    pub fn record(&self) -> TokenRecord {
        self.record.clone()
    }

    /// The secret itself, for the client's configuration. Anyone holding it can use these grants.
    pub fn secret(&self) -> &str {
        &self.secret
    }

    /// The value of the `Authorization` header that presents this token.
    pub fn bearer_header(&self) -> String {
        format!("Bearer {}", self.secret)
    }
}

/// Never prints the secret, so a token that ends up in a log line or a panic message stays secret.
impl fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccessToken")
            .field("id", &self.id)
            .field("label", &self.record.label)
            .field("secret", &"<redacted>")
            .finish_non_exhaustive()
    }
}

/// The newest [`TokenRecord::version`] this build reads.
pub const RECORD_VERSION: u32 = 1;

/// What the host stores to re-admit an access token after a restart (see [`HttpServer::admit`](crate::HttpServer::admit)).
///
/// It holds a keyed BLAKE3 hash of the secret, never the secret: whoever reads the store can't connect. The host chooses
/// where it lives (keychain, project settings) and serialises it with serde.
///
/// **Revocation survives a restart in one of two ways, the host's choice.** Either it deletes the record when the token
/// is revoked, so nothing is left to admit (the default; the secret is then unknown to the next server), or it stores
/// the tombstone [`HttpServer::revoke_record`](crate::HttpServer::revoke_record) returns, which has
/// [`revoked`](Self::revoked) set and which `admit` refuses. The tombstone is for stores that keep history or can't
/// delete; a host that deletes needs nothing more.
///
/// New fields (a spending budget, say) are added with serde defaults and a higher [`version`](Self::version). A build
/// that meets a version it doesn't know refuses the record rather than admit a token without limits it can't read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenRecord {
    /// The format version; [`RECORD_VERSION`] when written by this build.
    #[serde(default = "first_version")]
    pub version: u32,
    /// What the user called this client.
    pub label: String,
    /// The scopes every session opened with the token holds.
    pub grants: Grants,
    /// Who issued the token: the principal of the sessions opened with it.
    pub issuer: Actor,
    /// Whether the person has paired this credential in the studio window (agents can't pair themselves).
    #[serde(default)]
    pub paired: bool,
    /// True on a tombstone: the token was revoked and must never be admitted again.
    #[serde(default)]
    pub revoked: bool,
    /// The keyed BLAKE3 hash of the secret, 64 lowercase hex digits.
    pub hash: String,
}

const fn first_version() -> u32 {
    1
}

impl TokenRecord {
    fn new(label: String, grants: Grants, issuer: Actor, secret: &str) -> Self {
        let hash = to_hex(&hash_secret(secret));
        Self { version: RECORD_VERSION, label, grants, issuer, paired: false, revoked: false, hash }
    }

    /// The hash as bytes, or why this record can't be admitted.
    fn admissible_hash(&self) -> Result<[u8; HASH_BYTES], ServerError> {
        let bad = |reason: &str| ServerError::BadTokenRecord { label: self.label.clone(), reason: reason.to_owned() };
        if self.version > RECORD_VERSION {
            return Err(bad(&format!("it is version {}, and this build reads up to {RECORD_VERSION}", self.version)));
        }
        if self.revoked {
            return Err(ServerError::TokenRevoked(self.label.clone()));
        }
        from_hex(&self.hash).ok_or_else(|| bad("its hash isn't 64 hexadecimal digits"))
    }
}

/// How a request failed to present a valid token.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Denied {
    /// No `Authorization` header, or not a (single) `Bearer` credential.
    Missing,
    /// A bearer credential that matches no live token: wrong, or revoked.
    Unknown,
}

/// The rmcp service for one token, and the session store behind it.
pub(crate) struct Endpoint {
    id: TokenId,
    label: String,
    service: StreamableHttpService<Session, LocalSessionManager>,
    sessions: Arc<LocalSessionManager>,
    /// How many sessions this token may have open (`max_sessions_per_token`).
    slots: SessionSlots,
    /// Ends this endpoint's open streams; a child of the server's.
    cancel: CancellationToken,
}

impl Endpoint {
    pub(crate) fn service(&self) -> &StreamableHttpService<Session, LocalSessionManager> {
        &self.service
    }

    /// How many sessions this token has open: the ones in rmcp's table, which ends and drops a session when it is
    /// deleted, goes idle or fails.
    pub(crate) async fn open_sessions(&self) -> usize {
        self.sessions.sessions.read().await.len()
    }

    /// Takes the turn to open a session, to be held until the request that opens it has been served.
    ///
    /// Refuses at the cap rather than closing an older session: a session in use is never evicted to admit a new one.
    /// The refusal is logged here, naming the limit and the token, once per episode of being at the cap.
    pub(crate) async fn admit_session(&self) -> Result<Admission<'_>, SessionsFull> {
        let refused = match self.slots.admit(async || self.open_sessions().await).await {
            Ok(admission) => return Ok(admission),
            Err(refused) => refused,
        };
        if self.slots.first_refusal() {
            // Open sessions are kept; further refusals while still at the cap go to `debug`.
            tracing::warn!(
                token = %self.id, label = %self.label, limit = "max_sessions_per_token", value = refused.max,
                open = refused.open, "refused a new session: the token is at its session limit",
            );
        } else {
            tracing::debug!(
                token = %self.id, limit = "max_sessions_per_token", value = refused.max, "refused a new session"
            );
        }
        Err(refused)
    }

    /// Ends every session of this endpoint. Each session's open transaction is rolled back as its handler drops.
    pub(crate) async fn close(&self) {
        self.cancel.cancel();
        let ids: Vec<_> = self.sessions.sessions.read().await.keys().cloned().collect();
        for id in ids {
            if let Err(error) = self.sessions.close_session(&id).await {
                tracing::warn!(session = %id, %error, "couldn't close a session");
            }
        }
    }
}

/// A live token. Holds the hash of the secret, never the secret, so a memory dump of the host reveals no credential.
struct Entry {
    id: TokenId,
    hash: [u8; HASH_BYTES],
    record: TokenRecord,
    endpoint: Arc<Endpoint>,
}

/// The live tokens.
pub(crate) struct Registry {
    server: McpServer,
    entries: RwLock<Vec<Entry>>,
    next_id: AtomicU64,
    /// Parent of every endpoint's token: cancelling it ends all open streams.
    cancel: CancellationToken,
    allowed_origins: Vec<String>,
    limits: Limits,
}

impl Registry {
    pub(crate) fn new(
        server: McpServer,
        allowed_origins: Vec<String>,
        limits: Limits,
        cancel: CancellationToken,
    ) -> Self {
        Self { server, entries: RwLock::default(), next_id: AtomicU64::new(1), cancel, allowed_origins, limits }
    }

    /// The limits every token's endpoint is held to.
    pub(crate) fn limits(&self) -> &Limits {
        &self.limits
    }

    /// Creates a token whose sessions hold `grants`, issued by the system.
    pub(crate) fn issue(&self, label: String, grants: Grants) -> Result<AccessToken, ServerError> {
        self.issue_for(label, grants, Actor::System)
    }

    /// Creates a token whose sessions hold `grants`, recording who issued it.
    pub(crate) fn issue_for(&self, label: String, grants: Grants, issuer: Actor) -> Result<AccessToken, ServerError> {
        let secret = new_secret()?;
        let record = TokenRecord::new(label, grants, issuer, &secret);
        let id = self.insert(record.clone())?;
        Ok(AccessToken { id, secret, record })
    }

    /// Registers a stored token without ever seeing its secret. The client's old secret works again.
    ///
    /// # Errors
    /// [`ServerError::BadTokenRecord`] for an unreadable record or one already admitted, and
    /// [`ServerError::TokenRevoked`] for a tombstone.
    pub(crate) fn admit(&self, record: TokenRecord) -> Result<TokenId, ServerError> {
        self.insert(record)
    }

    fn insert(&self, record: TokenRecord) -> Result<TokenId, ServerError> {
        let hash = record.admissible_hash()?;
        let id = TokenId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let endpoint = Arc::new(self.endpoint(id, record.label.clone(), record.grants.clone())?);
        let mut entries = self.write();
        // Two live tokens with one hash would make `authorize` pick whichever is last: refuse rather than guess.
        if entries.iter().any(|e| constant_time_eq(&e.hash, &hash)) {
            return Err(ServerError::BadTokenRecord {
                label: record.label,
                reason: "a token with the same secret is already admitted".to_owned(),
            });
        }
        entries.push(Entry { id, hash, record, endpoint });
        Ok(id)
    }

    /// The record of a live token, as the host should store it now.
    pub(crate) fn record(&self, id: TokenId) -> Option<TokenRecord> {
        self.read().iter().find(|e| e.id == id).map(|e| e.record.clone())
    }

    /// Marks a live token as paired (or not). False if there is no such token.
    pub(crate) fn set_paired(&self, id: TokenId, paired: bool) -> bool {
        self.write().iter_mut().find(|e| e.id == id).map(|e| e.record.paired = paired).is_some()
    }

    /// Replaces a live token's grants. Its open sessions end, because they were opened under the old ones; new
    /// sessions get the new grants. False if there is no such token.
    pub(crate) async fn set_grants(&self, id: TokenId, grants: Grants) -> Result<bool, ServerError> {
        let Some(label) = self.record(id).map(|r| r.label) else { return Ok(false) };
        let fresh = Arc::new(self.endpoint(id, label, grants.clone())?);
        let old = {
            let mut entries = self.write();
            entries.iter_mut().find(|e| e.id == id).map(|entry| {
                entry.record.grants = grants;
                std::mem::replace(&mut entry.endpoint, Arc::clone(&fresh))
            })
        };
        // Close outside the lock. If the token was revoked meanwhile, the fresh endpoint was never installed.
        let live = old.is_some();
        old.unwrap_or(fresh).close().await;
        Ok(live)
    }

    /// Forgets the token, so its secret is refused from now on, and ends its sessions. False if there was no such live
    /// token.
    pub(crate) async fn revoke(&self, id: TokenId) -> bool {
        self.revoke_record(id).await.is_some()
    }

    /// Like [`revoke`](Self::revoke), and returns the tombstone the host may store (see [`TokenRecord`]).
    pub(crate) async fn revoke_record(&self, id: TokenId) -> Option<TokenRecord> {
        let removed = {
            let mut entries = self.write();
            entries.iter().position(|e| e.id == id).map(|i| entries.remove(i))
        };
        let entry = removed?;
        entry.endpoint.close().await;
        tracing::info!(token = %entry.id, label = %entry.record.label, "access token revoked");
        Some(TokenRecord { revoked: true, ..entry.record })
    }

    /// Ends every session of every token.
    pub(crate) async fn close_all(&self) {
        let endpoints: Vec<_> = self.read_endpoints();
        for endpoint in endpoints {
            endpoint.close().await;
        }
    }

    /// The endpoint for the token in the request's `Authorization` header.
    pub(crate) fn authorize(&self, headers: &HeaderMap) -> Result<Arc<Endpoint>, Denied> {
        let presented = bearer(headers).ok_or(Denied::Missing)?;
        // Compare against every token, without stopping at the first match, so timing doesn't reveal how close a guess
        // was or which slot matched. Hashing first makes every comparison the same length, so even that is hidden.
        let presented = hash_secret(presented);
        let mut found = None;
        for entry in self.read().iter() {
            if constant_time_eq(&entry.hash, &presented) {
                found = Some(Arc::clone(&entry.endpoint));
            }
        }
        found.ok_or(Denied::Unknown)
    }

    fn endpoint(&self, id: TokenId, label: String, grants: Grants) -> Result<Endpoint, ServerError> {
        let server = self.server.with_grants(grants)?;
        let cancel = self.cancel.child_token();
        // Sessions that go quiet (a crashed client never sends DELETE) are dropped after the idle timeout, which also
        // rolls back whatever transaction they left open and removes the session from the table the cap counts.
        let mut sessions = LocalSessionManager::default();
        sessions.session_config.keep_alive = Some(self.limits.idle_timeout);
        let sessions = Arc::new(sessions);
        // Origin validation is on even with an empty list, so a request that carries any `Origin` (a web page) is
        // refused unless the host listed it. Requests without one (every non-browser client) are checked by token.
        let config = StreamableHttpServerConfig::default()
            .with_allowed_hosts(LOOPBACK_HOSTS)
            .with_allowed_origins(self.allowed_origins.clone())
            .enforce_origin_validation()
            // The gate has already read the body within this limit; rmcp's own check is a second line of defence.
            .with_max_request_body_bytes(self.limits.max_body_bytes)
            .with_cancellation_token(cancel.clone());
        let service = StreamableHttpService::new(move || Ok(server.http_session()), Arc::clone(&sessions), config);
        let slots = SessionSlots::new(self.limits.max_sessions_per_token);
        Ok(Endpoint { id, label, service, sessions, slots, cancel })
    }

    fn read_endpoints(&self) -> Vec<Arc<Endpoint>> {
        self.read().iter().map(|e| Arc::clone(&e.endpoint)).collect()
    }

    // A poisoned lock only means another thread panicked while holding it; the list itself is always consistent.
    fn read(&self) -> std::sync::RwLockReadGuard<'_, Vec<Entry>> {
        self.entries.read().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Vec<Entry>> {
        self.entries.write().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// The credential of a single `Authorization: Bearer <credential>` header (the scheme is case-insensitive, RFC 7235).
/// Two `Authorization` headers are ambiguous, so they count as none.
fn bearer(headers: &HeaderMap) -> Option<&str> {
    let mut values = headers.get_all(AUTHORIZATION).iter();
    let value = values.next()?.to_str().ok()?;
    if values.next().is_some() {
        return None;
    }
    let (scheme, credential) = value.split_once(' ')?;
    let credential = credential.trim();
    (scheme.eq_ignore_ascii_case("bearer") && !credential.is_empty()).then_some(credential)
}

fn new_secret() -> Result<String, ServerError> {
    let mut bytes = [0_u8; SECRET_BYTES];
    getrandom::fill(&mut bytes).map_err(|e| ServerError::Entropy(e.to_string()))?;
    Ok(format!("{SECRET_PREFIX}{}", to_hex(&bytes)))
}

/// The keyed BLAKE3 hash a record stores in place of the secret.
fn hash_secret(secret: &str) -> [u8; HASH_BYTES] {
    *blake3::keyed_hash(&blake3::derive_key(HASH_CONTEXT, &[]), secret.as_bytes()).as_bytes()
}

fn to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(2 * bytes.len());
    for byte in bytes {
        out.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

/// Exactly [`HASH_BYTES`] bytes of hex (either case), or `None`.
fn from_hex(text: &str) -> Option<[u8; HASH_BYTES]> {
    let digits = text.as_bytes();
    if digits.len() != 2 * HASH_BYTES {
        return None;
    }
    let value = |d: u8| char::from(d).to_digit(16);
    let mut out = [0_u8; HASH_BYTES];
    let (pairs, _) = digits.as_chunks::<2>();
    for (byte, pair) in out.iter_mut().zip(pairs) {
        *byte = u8::try_from(value(pair[0])? << 4 | value(pair[1])?).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use hyper::header::HeaderValue;

    use super::*;

    fn headers(values: &[&str]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for value in values {
            map.append(AUTHORIZATION, HeaderValue::from_str(value).unwrap());
        }
        map
    }

    #[test]
    fn secrets_are_long_random_and_prefixed() {
        let (a, b) = (new_secret().unwrap(), new_secret().unwrap());
        assert_ne!(a, b);
        for secret in [&a, &b] {
            let hex = secret.strip_prefix(SECRET_PREFIX).expect("prefix");
            assert_eq!(hex.len(), 2 * SECRET_BYTES);
            assert!(hex.chars().all(|c| c.is_ascii_hexdigit()), "{secret}");
        }
    }

    #[test]
    fn only_a_single_bearer_credential_counts() {
        assert_eq!(bearer(&headers(&["Bearer abc"])), Some("abc"));
        assert_eq!(bearer(&headers(&["bearer abc"])), Some("abc"), "the scheme is case-insensitive");
        assert_eq!(bearer(&headers(&["BEARER   abc  "])), Some("abc"));
        for bad in [&[][..], &["Basic abc"], &["Bearer"], &["Bearer "], &["abc"], &["Bearer a", "Bearer b"]] {
            assert_eq!(bearer(&headers(bad)), None, "{bad:?}");
        }
    }

    #[test]
    fn a_token_never_shows_its_secret_when_debug_printed() {
        let secret = "omuzaki_mcp_deadbeef";
        let record = TokenRecord::new("codex".to_owned(), Grants::agent_default(), Actor::System, secret);
        let token = AccessToken { id: TokenId(7), secret: secret.to_owned(), record };
        let printed = format!("{token:?}");
        assert!(!printed.contains("deadbeef") && printed.contains("codex"), "{printed}");
        assert_eq!(token.bearer_header(), "Bearer omuzaki_mcp_deadbeef");
    }

    fn record() -> TokenRecord {
        TokenRecord::new("codex".to_owned(), Grants::agent_default(), Actor::System, "omuzaki_mcp_secret")
    }

    #[test]
    fn a_record_holds_the_hash_and_never_the_secret() {
        let json = serde_json::to_string(&record()).unwrap();
        assert!(!json.contains("omuzaki_mcp_secret"), "{json}");
        assert_eq!(record().hash.len(), 2 * HASH_BYTES);
        assert_eq!(from_hex(&record().hash), Some(hash_secret("omuzaki_mcp_secret")));
        assert_ne!(hash_secret("omuzaki_mcp_secret"), hash_secret("omuzaki_mcp_secreu"));
    }

    #[test]
    fn a_record_round_trips_and_older_ones_without_new_fields_still_read() {
        let back: TokenRecord = serde_json::from_str(&serde_json::to_string(&record()).unwrap()).unwrap();
        assert_eq!(back, record());
        let mut value = serde_json::to_value(record()).unwrap();
        for field in ["version", "paired", "revoked"] {
            value.as_object_mut().unwrap().remove(field);
        }
        let old: TokenRecord = serde_json::from_value(value).unwrap();
        assert_eq!((old.version, old.paired, old.revoked), (1, false, false));
    }

    #[test]
    fn unreadable_tombstoned_and_future_records_are_not_admissible() {
        assert!(record().admissible_hash().is_ok());
        let tombstone = TokenRecord { revoked: true, ..record() };
        assert!(matches!(tombstone.admissible_hash(), Err(ServerError::TokenRevoked(_))));
        let future = TokenRecord { version: RECORD_VERSION + 1, ..record() };
        assert!(matches!(future.admissible_hash(), Err(ServerError::BadTokenRecord { .. })));
        for hash in ["", "zz", &"g".repeat(64), &"a".repeat(63), &"é".repeat(32)] {
            let bad = TokenRecord { hash: hash.to_owned(), ..record() };
            assert!(matches!(bad.admissible_hash(), Err(ServerError::BadTokenRecord { .. })), "{hash:?}");
        }
    }
}
