//! The adapter's polling thread: find the live chat, skip the backlog YouTube sends first, then poll at the interval
//! each page asks for, turn each item into an event and publish it.
//!
//! - **Quota.** Each call's cost is counted ([`crate::api::cost`]); a spent quota stops the adapter with a clear
//!   error instead of hammering YouTube.
//! - **Ends cleanly.** A refused token, the chat ending or the stream going offline stops the adapter and says why;
//!   a network failure is retried with a backoff (1 s doubling to 30 s).
//! - **Nothing is lost to a slow show.** A Super Chat or a membership the show can't take now (`Full`) is kept and
//!   retried in order; chat, which the show may shed, is not kept.
//! - **New subscribers** are polled every [`SUBSCRIBERS_EVERY`]; the first poll only learns who is already there.

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use omuzaki_core::time::Clock;
use omuzaki_live_api::event::{EventSink, LiveEvent, PublishError};
use omuzaki_live_api::limits::PRODUCER_BUFFER;
use omuzaki_secrets_api::value::SecretValue;
use schemars::JsonSchema;
use serde::Serialize;

use crate::api::{Api, cost};
use crate::chat::{Chat, normalise, subscriber};
use crate::error::YouTubeError;

/// How often new subscribers are checked.
pub const SUBSCRIBERS_EVERY: Duration = Duration::from_secs(60);
/// The longest wait between chat polls, whatever YouTube asks.
const MAX_POLL: Duration = Duration::from_secs(30);
/// The first wait after a network failure, doubled up to [`MAX_BACKOFF`].
const FIRST_BACKOFF: Duration = Duration::from_secs(1);
/// The longest wait after network failures.
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// How often a waiting thread looks at its stop flag.
const TICK: Duration = Duration::from_millis(20);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What `youtube.status` reports. Never the token.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct YouTubeStatus {
    /// Whether the chat is being read now.
    pub connected: bool,
    /// The broadcast's title.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub broadcast: Option<String>,
    /// Events received and published.
    pub received: u64,
    /// Chat polls made.
    pub polls: u64,
    /// Quota units spent since connecting.
    pub quota_used: u64,
    /// A project's default daily quota, for scale.
    pub quota_daily: u64,
    /// The wait between polls YouTube last asked for, milliseconds.
    pub poll_ms: u64,
    /// Important events dropped because the show couldn't take them in time.
    pub lost: u64,
    /// Why it stopped, or the last problem.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Where the adapter connects and how fast it may poll. YouTube's own API in the app; a test server's in tests.
#[derive(Clone, Debug)]
pub struct Endpoints {
    /// The Data API.
    pub api: Api,
    /// The shortest wait between chat polls, whatever YouTube asks: 1 s in the app.
    pub min_poll: Duration,
    /// How often new subscribers are checked.
    pub subscribers_every: Duration,
}

impl Endpoints {
    /// `api` at the app's poll limits.
    pub fn new(api: Api) -> Self {
        Self { api, min_poll: Duration::from_secs(1), subscribers_every: SUBSCRIBERS_EVERY }
    }
}

/// A running adapter: its thread and what it reports.
pub(crate) struct Adapter {
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<YouTubeStatus>>,
    thread: Option<JoinHandle<()>>,
}

impl Adapter {
    /// Starts the polling thread. The token was read from the secret store by the caller and lives only here.
    pub(crate) fn start(
        endpoints: Endpoints,
        token: SecretValue,
        video: Option<String>,
        sink: Arc<dyn EventSink>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, YouTubeError> {
        let stop = Arc::new(AtomicBool::new(false));
        let status =
            Arc::new(Mutex::new(YouTubeStatus { quota_daily: cost::DAILY_DEFAULT, ..YouTubeStatus::default() }));
        let worker = Worker {
            endpoints,
            token,
            video,
            sink,
            clock,
            stop: Arc::clone(&stop),
            status: Arc::clone(&status),
            pending: VecDeque::new(),
            subscribers: None,
        };
        let thread = std::thread::Builder::new()
            .name("youtube-chat".to_owned())
            .spawn(move || worker.run())
            .map_err(|e| YouTubeError::Network(format!("the polling thread didn't start: {e}")))?;
        Ok(Self { stop, status, thread: Some(thread) })
    }

    /// What it reports now.
    pub(crate) fn status(&self) -> YouTubeStatus {
        lock(&self.status).clone()
    }
}

impl Drop for Adapter {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Worker {
    endpoints: Endpoints,
    token: SecretValue,
    video: Option<String>,
    sink: Arc<dyn EventSink>,
    clock: Arc<dyn Clock>,
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<YouTubeStatus>>,
    /// Important events the show couldn't take yet, oldest first.
    pending: VecDeque<LiveEvent>,
    /// The subscribers seen so far; `None` before the first check.
    subscribers: Option<HashSet<String>>,
}

impl Worker {
    fn stopping(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    fn report(&self, change: impl FnOnce(&mut YouTubeStatus)) {
        change(&mut lock(&self.status));
    }

    fn spend(&self, units: u64) {
        self.report(|s| s.quota_used = s.quota_used.saturating_add(units));
    }

    /// Waits up to `wait`, waking early to stop.
    fn pause(&self, wait: Duration) {
        let until = Instant::now() + wait;
        while !self.stopping() && Instant::now() < until {
            std::thread::sleep(TICK.min(until.saturating_duration_since(Instant::now())));
        }
    }

    /// Stops for good, saying why.
    fn end(&self, why: &YouTubeError) {
        let text = why.to_string();
        self.report(|s| {
            s.connected = false;
            s.error = Some(text);
        });
    }

    fn run(mut self) {
        let mut backoff = FIRST_BACKOFF;
        let chat = loop {
            if self.stopping() {
                return;
            }
            self.spend(cost::LIST);
            match self.endpoints.api.find_chat(self.token.expose(), self.video.as_deref()) {
                Ok(chat) => break chat,
                Err(YouTubeError::Network(why)) => {
                    self.report(|s| s.error = Some(format!("YouTube couldn't be reached: {why}")));
                    self.pause(backoff);
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
                Err(other) => return self.end(&other),
            }
        };
        self.report(|s| {
            s.connected = true;
            s.broadcast = Some(chat.title.clone());
            s.error = None;
        });
        let mut page: Option<String> = None;
        let mut first = true;
        let mut next_subscribers = Instant::now();
        backoff = FIRST_BACKOFF;
        while !self.stopping() {
            if Instant::now() >= next_subscribers {
                next_subscribers = Instant::now() + self.endpoints.subscribers_every;
                self.check_subscribers(&chat);
            }
            self.spend(cost::CHAT_PAGE);
            let wait = match self.endpoints.api.messages(self.token.expose(), &chat.id, page.as_deref()) {
                Ok(answer) => {
                    backoff = FIRST_BACKOFF;
                    self.report(|s| s.polls += 1);
                    // The first page is what was said before we joined: alerts for it would be stale.
                    if !first {
                        let at = self.clock.now();
                        for item in &answer.items {
                            if let Some(event) = normalise(item, &chat, at) {
                                self.publish(event);
                            }
                        }
                    }
                    first = false;
                    if answer.offline_at.is_some() {
                        return self.end(&YouTubeError::Ended);
                    }
                    if answer.next_page_token.is_some() {
                        page = answer.next_page_token;
                    }
                    let asked = Duration::from_millis(answer.polling_interval_millis.unwrap_or(5_000));
                    let wait = asked.clamp(self.endpoints.min_poll, MAX_POLL);
                    self.report(|s| s.poll_ms = u64::try_from(wait.as_millis()).unwrap_or(u64::MAX));
                    wait
                }
                Err(YouTubeError::Network(why)) => {
                    self.report(|s| s.error = Some(format!("YouTube couldn't be reached: {why}")));
                    let wait = backoff;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                    wait
                }
                Err(other) => return self.end(&other),
            };
            self.retry_pending();
            self.pause(wait);
        }
        self.report(|s| s.connected = false);
    }

    /// New subscribers since the last check become `follow` events.
    fn check_subscribers(&mut self, chat: &Chat) {
        self.spend(cost::LIST);
        // A channel can hide its subscribers or the token can lack the scope: chat still works, so this is not fatal.
        let Ok(items) = self.endpoints.api.recent_subscribers(self.token.expose()) else { return };
        let at = self.clock.now();
        let ids: Vec<(String, &serde_json::Value)> = items
            .iter()
            .filter_map(|i| {
                i.pointer("/subscriberSnippet/channelId").and_then(|v| v.as_str()).map(|id| (id.to_owned(), i))
            })
            .collect();
        let Some(seen) = &mut self.subscribers else {
            self.subscribers = Some(ids.into_iter().map(|(id, _)| id).collect());
            return;
        };
        let fresh: Vec<LiveEvent> = ids
            .into_iter()
            .filter(|(id, _)| seen.insert(id.clone()))
            .filter_map(|(_, i)| subscriber(i, chat, at))
            .collect();
        for event in fresh {
            self.publish(event);
        }
    }

    fn publish(&mut self, event: LiveEvent) {
        if !self.pending.is_empty() && event.kind.as_str() != "chat.message" {
            // Keep the order of important events behind those still waiting.
            self.keep(event);
            return;
        }
        let chat = event.kind.as_str() == "chat.message";
        match self.sink.publish(event.clone()) {
            Ok(()) => self.report(|s| s.received += 1),
            Err(PublishError::Full) if !chat => self.keep(event),
            Err(_) => {}
        }
    }

    fn keep(&mut self, event: LiveEvent) {
        if self.pending.len() >= PRODUCER_BUFFER {
            self.pending.pop_front();
            self.sink.producer_dropped(1);
            self.report(|s| s.lost += 1);
        }
        self.pending.push_back(event);
    }

    fn retry_pending(&mut self) {
        while let Some(event) = self.pending.pop_front() {
            match self.sink.publish(event.clone()) {
                Ok(()) => self.report(|s| s.received += 1),
                Err(PublishError::Full) => {
                    self.pending.push_front(event);
                    return;
                }
                Err(_) => {}
            }
        }
    }
}
