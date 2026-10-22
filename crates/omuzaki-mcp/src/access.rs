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
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use constant_time_eq::constant_time_eq;
use hyper::HeaderMap;
use hyper::header::AUTHORIZATION;
use omuzaki_ops_api::{Actor, EndReason, Grants};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{SessionManager, StreamableHttpServerConfig, StreamableHttpService};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::agent::Credential;
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
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TokenId(u64);

impl fmt::Display for TokenId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "token-{}", self.0)
    }
}

/// One live access token, as the "connected apps" panel shows it. Never carries the secret or its hash.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct TokenInfo {
    /// Names the token for [`HttpServer::revoke`](crate::HttpServer::revoke).
    pub id: TokenId,
    /// What the user called this client.
    pub label: String,
    /// The scopes every session opened with the token holds.
    pub grants: Grants,
    /// When a request last presented this token and passed the check; `None` if none has.
    pub last_used: Option<SystemTime>,
    /// How many sessions the token has open now.
    pub open_sessions: usize,
    /// Who issued the token: the principal of the sessions opened with it.
    pub issuer: Actor,
    /// Whether the person has paired this credential in the studio window.
    pub paired: bool,
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

/// The newest [`TokenRecord::version`] this build reads. A bridge credential's record is version 2, so a build that
/// doesn't know [`TokenRecord::bridge`] refuses it rather than admit it without its pairing; other records stay
/// version 1.
pub const RECORD_VERSION: u32 = 2;

/// How many bridge credentials may wait to be paired at once. A bridge stores its credential before it connects, so
/// one client needs one; more than this is something requesting credentials in a loop.
pub(crate) const MAX_UNPAIRED_BRIDGES: usize = 8;

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
    /// True for a bridge credential: one `omuzaki mcp` asked for to relay a client's stdio into the studio
    /// (`agent-sessions.md` 10.2). Its sessions are stdio sessions, and each waits for a person to pair the credential
    /// in Omuzaki's window until it is [`paired`](Self::paired). Version 2.
    #[serde(default)]
    pub bridge: bool,
    /// The keyed BLAKE3 hash of the secret, 64 lowercase hex digits.
    pub hash: String,
}

const fn first_version() -> u32 {
    1
}

impl TokenRecord {
    fn new(label: String, grants: Grants, issuer: Actor, secret: &str) -> Self {
        let hash = to_hex(&hash_secret(secret));
        Self { version: 1, label, grants, issuer, paired: false, revoked: false, bridge: false, hash }
    }

    fn new_bridge(client: String, grants: Grants, issuer: Actor, secret: &str) -> Self {
        Self { version: 2, bridge: true, ..Self::new(client, grants, issuer, secret) }
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
    /// What the token's sessions are admitted with, and how the port ends them (see [`Credential`]).
    credential: Arc<Credential>,
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

    /// Ends every session of this endpoint for `reason`. Each session ends through the registry, which abandons its open
    /// transaction (without a registry the handler rolls it back as it drops).
    pub(crate) async fn close(&self, reason: EndReason) {
        self.credential.will_end_with(reason);
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
    /// Unix milliseconds of the last request that presented this token, 0 for never. Atomic so [`Registry::authorize`]
    /// can record it under the read lock it already holds, and nothing else is shared with it.
    last_used: AtomicU64,
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
    /// Where a session's credential revocation (the registry asked for it) is sent; the port revokes the token.
    revocations: mpsc::UnboundedSender<TokenId>,
    /// Where a bridge credential the person paired is sent; the port marks it paired and tells the host.
    pairings: mpsc::UnboundedSender<TokenId>,
    /// The key that lets a local `omuzaki mcp` ask for a bridge credential, once the host has enabled pairing.
    pairing: RwLock<Option<PairingKey>>,
}

/// What [`Registry::enable_pairing`] set up: the hash of the key, and what the credentials it hands out hold.
struct PairingKey {
    hash: [u8; HASH_BYTES],
    grants: Grants,
    issuer: Actor,
}

impl Registry {
    pub(crate) fn new(
        server: McpServer,
        allowed_origins: Vec<String>,
        limits: Limits,
        cancel: CancellationToken,
        revocations: mpsc::UnboundedSender<TokenId>,
        pairings: mpsc::UnboundedSender<TokenId>,
    ) -> Self {
        Self {
            server,
            entries: RwLock::default(),
            next_id: AtomicU64::new(1),
            cancel,
            allowed_origins,
            limits,
            revocations,
            pairings,
            pairing: RwLock::default(),
        }
    }

    /// Why the port closes sessions on behalf of the person: they revoked the token (`credential`) or changed what it
    /// may do.
    fn revoked(&self, credential: bool) -> EndReason {
        EndReason::Revoked { by: self.server.person(), credential }
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

    /// Lets a local bridge ask for its own credential (see [`issue_bridge`](Self::issue_bridge)) by presenting the
    /// returned key; the credentials it gets hold `grants` on behalf of `issuer`. A second call replaces the key.
    pub(crate) fn enable_pairing(&self, grants: Grants, issuer: Actor) -> Result<String, ServerError> {
        let key = new_secret()?;
        let pairing = PairingKey { hash: hash_secret(&key), grants, issuer };
        *self.pairing.write().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(pairing);
        Ok(key)
    }

    /// Whether the request presents the pairing key.
    pub(crate) fn authorize_pairing(&self, headers: &HeaderMap) -> Result<(), Denied> {
        let presented = hash_secret(bearer(headers).ok_or(Denied::Missing)?);
        let pairing = self.pairing.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        match &*pairing {
            Some(key) if constant_time_eq(&key.hash, &presented) => Ok(()),
            _ => Err(Denied::Unknown),
        }
    }

    /// Creates a bridge credential for `client`, unpaired: every session opened with it waits for a person to pair it.
    /// `None` when pairing isn't enabled or [`MAX_UNPAIRED_BRIDGES`] already wait.
    pub(crate) fn issue_bridge(&self, client: String) -> Result<Option<AccessToken>, ServerError> {
        let (grants, issuer) = {
            let pairing = self.pairing.read().unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(key) = &*pairing else { return Ok(None) };
            (key.grants.clone(), key.issuer.clone())
        };
        let waiting = self.read().iter().filter(|e| e.record.bridge && !e.record.paired).count();
        if waiting >= MAX_UNPAIRED_BRIDGES {
            return Ok(None);
        }
        let secret = new_secret()?;
        let record = TokenRecord::new_bridge(client, grants, issuer, &secret);
        let id = self.insert(record.clone())?;
        Ok(Some(AccessToken { id, secret, record }))
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
        let endpoint = Arc::new(self.endpoint(id, &record)?);
        let mut entries = self.write();
        // Two live tokens with one hash would make `authorize` pick whichever is last: refuse rather than guess.
        if entries.iter().any(|e| constant_time_eq(&e.hash, &hash)) {
            return Err(ServerError::BadTokenRecord {
                label: record.label,
                reason: "a token with the same secret is already admitted".to_owned(),
            });
        }
        entries.push(Entry { id, hash, record, endpoint, last_used: AtomicU64::new(0) });
        Ok(id)
    }

    /// The record of a live token, as the host should store it now.
    pub(crate) fn record(&self, id: TokenId) -> Option<TokenRecord> {
        self.read().iter().find(|e| e.id == id).map(|e| e.record.clone())
    }

    /// Marks a live token as paired (or not). False if there is no such token.
    pub(crate) fn set_paired(&self, id: TokenId, paired: bool) -> bool {
        let mut entries = self.write();
        let Some(entry) = entries.iter_mut().find(|e| e.id == id) else { return false };
        entry.record.paired = paired;
        entry.endpoint.credential.set_paired(paired);
        true
    }

    /// Replaces a live token's grants. Its open sessions end, because they were opened under the old ones; new
    /// sessions get the new grants. False if there is no such token.
    pub(crate) async fn set_grants(&self, id: TokenId, grants: Grants) -> Result<bool, ServerError> {
        let Some(mut record) = self.record(id) else { return Ok(false) };
        record.grants = grants.clone();
        let fresh = Arc::new(self.endpoint(id, &record)?);
        let old = {
            let mut entries = self.write();
            entries.iter_mut().find(|e| e.id == id).map(|entry| {
                entry.record.grants = grants;
                std::mem::replace(&mut entry.endpoint, Arc::clone(&fresh))
            })
        };
        // Close outside the lock. If the token was revoked meanwhile, the fresh endpoint was never installed.
        let live = old.is_some();
        old.unwrap_or(fresh).close(self.revoked(false)).await;
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
        entry.endpoint.close(self.revoked(true)).await;
        tracing::info!(token = %entry.id, label = %entry.record.label, "access token revoked");
        Some(TokenRecord { revoked: true, ..entry.record })
    }

    /// Ends every session of every token.
    pub(crate) async fn close_all(&self) {
        let endpoints: Vec<_> = self.read_endpoints();
        for endpoint in endpoints {
            endpoint.close(EndReason::Closed).await;
        }
    }

    /// The endpoint for the token in the request's `Authorization` header.
    pub(crate) fn authorize(&self, headers: &HeaderMap) -> Result<Arc<Endpoint>, Denied> {
        let presented = bearer(headers).ok_or(Denied::Missing)?;
        // Compare against every token, without stopping at the first match, so timing doesn't reveal how close a guess
        // was or which slot matched. Hashing first makes every comparison the same length, so even that is hidden.
        let presented = hash_secret(presented);
        let entries = self.read();
        let mut found = None;
        for entry in entries.iter() {
            if constant_time_eq(&entry.hash, &presented) {
                found = Some(entry);
            }
        }
        let entry = found.ok_or(Denied::Unknown)?;
        // Wall-clock time, shown to people and never replayed: ADR 0009 only forbids clock reads on replayed paths.
        record_use(&entry.last_used, now_millis());
        Ok(Arc::clone(&entry.endpoint))
    }

    /// The live tokens in id order, for the connected-apps panel. Each row's open sessions are counted after the lock is
    /// released, so no lock is held across an await.
    pub(crate) async fn tokens(&self) -> Vec<TokenInfo> {
        let mut rows: Vec<(TokenInfo, Arc<Endpoint>)> = self
            .read()
            .iter()
            .map(|e| {
                let info = TokenInfo {
                    id: e.id,
                    label: e.record.label.clone(),
                    grants: e.record.grants.clone(),
                    last_used: millis_to_time(e.last_used.load(Ordering::Relaxed)),
                    open_sessions: 0,
                    issuer: e.record.issuer.clone(),
                    paired: e.record.paired,
                };
                (info, Arc::clone(&e.endpoint))
            })
            .collect();
        rows.sort_by_key(|(info, _)| info.id);
        let mut infos = Vec::with_capacity(rows.len());
        for (mut info, endpoint) in rows {
            info.open_sessions = endpoint.open_sessions().await;
            infos.push(info);
        }
        infos
    }

    fn endpoint(&self, id: TokenId, record: &TokenRecord) -> Result<Endpoint, ServerError> {
        let label = record.label.clone();
        let revocations = self.revocations.clone();
        let mut credential = Credential::new(
            label.clone(),
            record.issuer.clone(),
            record.paired,
            self.limits.idle_timeout,
            Some(Box::new(move || {
                // The port is gone when nobody listens; then there is nothing left to revoke.
                let _ = revocations.send(id);
            })),
        );
        if record.bridge {
            let pairings = self.pairings.clone();
            credential = credential.bridge(Box::new(move || {
                let _ = pairings.send(id);
            }));
        }
        let credential = Arc::new(credential);
        let server = self.server.with_credential(record.grants.clone(), Some(Arc::clone(&credential)))?;
        let cancel = self.cancel.child_token();
        // Sessions that go quiet (a crashed client never sends DELETE) are dropped after the idle timeout, which also
        // rolls back whatever transaction they left open and removes the session from the table the cap counts.
        let mut sessions = LocalSessionManager::default();
        sessions.session_config.keep_alive = Some(self.limits.idle_timeout);
        let sessions = Arc::new(sessions);
        credential.attach(Arc::clone(&sessions));
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
        Ok(Endpoint { id, label, credential, service, sessions, slots, cancel })
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

/// Unix milliseconds now. A clock set before 1970 reads as 0, which is "never", so it records nothing rather than a
/// wrong time.
fn now_millis() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).ok().and_then(|d| u64::try_from(d.as_millis()).ok()).unwrap_or(0)
}

/// Stores a use time without moving the recorded time backward. Two requests can read the clock in one order and store
/// in the other, so an unconditional store could overwrite a later time with an earlier one; `fetch_max` never does.
fn record_use(last_used: &AtomicU64, millis: u64) {
    last_used.fetch_max(millis, Ordering::Relaxed);
}

fn millis_to_time(millis: u64) -> Option<SystemTime> {
    (millis != 0).then(|| UNIX_EPOCH + Duration::from_millis(millis))
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
    use std::sync::mpsc::{Receiver, channel};

    use hyper::header::HeaderValue;
    use omuzaki_ops_api::{Event, OpError, OpInfo, Outcome, Request as OpRequest, Runner, TxId};

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
        for field in ["version", "paired", "revoked", "bridge"] {
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

    /// A runner with no ops: the registry's own bookkeeping doesn't depend on what a tool does.
    struct Idle;

    impl Runner for Idle {
        fn ops(&self) -> Vec<OpInfo> {
            Vec::new()
        }
        fn run(&self, _: OpRequest) -> Result<Outcome, OpError> {
            Err(OpError::unsupported("this runner has no ops"))
        }
        fn begin(&self, _: &str, _: &Actor) -> TxId {
            TxId(1)
        }
        fn commit(&self, _: TxId, _: &Actor) -> Result<(), OpError> {
            Ok(())
        }
        fn rollback(&self, _: TxId, _: &Actor) -> Result<(), OpError> {
            Ok(())
        }
        fn undo(&self, _: &Actor, _: &Grants) -> Result<Option<String>, OpError> {
            Ok(None)
        }
        fn redo(&self, _: &Actor, _: &Grants) -> Result<Option<String>, OpError> {
            Ok(None)
        }
        fn subscribe(&self, _: &Grants) -> Result<Receiver<Event>, OpError> {
            Ok(channel().1)
        }
        fn get(&self, _: &omuzaki_doc_api::DocPath, _: &Grants) -> Result<Option<serde_json::Value>, OpError> {
            Ok(None)
        }
        fn log(&self, _: u64, _: usize, _: &Grants) -> Result<Vec<omuzaki_ops_api::OpRecord>, OpError> {
            Ok(Vec::new())
        }
    }

    fn registry() -> Registry {
        let server = McpServer::new(Arc::new(Idle), crate::server::McpOptions::default()).unwrap();
        let limits = Limits {
            idle_timeout: Duration::from_mins(30),
            max_sessions_per_token: 32,
            max_body_bytes: 4 * 1024 * 1024,
            body_timeout: Duration::from_secs(30),
        };
        let (revocations, pairings) = (tokio::sync::mpsc::unbounded_channel().0, tokio::sync::mpsc::unbounded_channel().0);
        Registry::new(server, Vec::new(), limits, CancellationToken::new(), revocations, pairings)
    }

    #[test]
    fn authorize_records_a_use_only_when_the_secret_is_right() {
        let registry = registry();
        let token = registry.issue("codex".to_owned(), Grants::agent_default()).unwrap();
        let last_used = || registry.read()[0].last_used.load(Ordering::Relaxed);
        let headers = |value: &str| {
            let mut map = HeaderMap::new();
            map.insert(AUTHORIZATION, HeaderValue::from_str(value).unwrap());
            map
        };
        assert_eq!(last_used(), 0, "a token starts never used");
        assert!(registry.authorize(&headers("Bearer omuzaki_mcp_wrong")).is_err());
        assert_eq!(last_used(), 0, "a failed check records nothing");
        let before = now_millis();
        assert!(registry.authorize(&headers(&token.bearer_header())).is_ok());
        let used = last_used();
        assert!(before <= used && used <= now_millis(), "{used} is not between {before} and now");
        assert_eq!(millis_to_time(0), None);
    }

    #[test]
    fn a_later_use_time_is_never_overwritten_by_an_earlier_one() {
        let last_used = AtomicU64::new(0);
        record_use(&last_used, 5_000);
        record_use(&last_used, 4_000);
        assert_eq!(last_used.load(Ordering::Relaxed), 5_000, "an earlier use must not move the time back");
        record_use(&last_used, 6_000);
        assert_eq!(last_used.load(Ordering::Relaxed), 6_000, "a later use must move the time forward");
    }

    #[test]
    fn concurrent_uses_keep_the_latest_time() {
        let last_used = Arc::new(AtomicU64::new(0));
        let handles: Vec<_> = (1..=8u64)
            .map(|t| {
                let last_used = Arc::clone(&last_used);
                std::thread::spawn(move || {
                    for i in 0..1_000u64 {
                        record_use(&last_used, t * 10_000 + i);
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(last_used.load(Ordering::Relaxed), 8 * 10_000 + 999);
    }
}
