//! [`FakeHttp`]: the scripted transport. It answers each request from its routes and records what it was asked.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use omuzaki_http_api::{Cancel, HttpBody, HttpError, HttpRequest, HttpResponse, HttpStream, HttpTransport, Method};
use serde_json::Value;

use crate::cassette::{self, CassetteError};
use crate::stream::{self, Chunk, StreamScript};

/// One scripted answer. A route hands its replies out in order; see [`FakeHttp::route`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    /// A whole answer. Its body must fit the request's `max_response`.
    Response(HttpResponse),
    /// A failure, returned as it is.
    Error(HttpError),
    /// No answer: the call waits until its cancel handle turns true, or gives up after 1,000 polls.
    Hang,
    /// A streamed answer, read chunk by chunk.
    Stream(StreamScript),
}

/// One logged request: what the caller sent, byte for byte.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Logged {
    /// The method.
    pub method: Method,
    /// The exact URL, query string included.
    pub url: String,
    /// The headers, in the order sent.
    pub headers: Vec<(String, String)>,
    /// The body bytes. A JSON body is its compact serialisation; a multipart body is its parts' data, concatenated.
    pub body: Vec<u8>,
}

/// A transport that answers from a script and counts what it was asked. It opens no sockets and never sleeps: a hang
/// or a stall polls its cancel handle a fixed number of times instead.
///
/// Build it with [`route`](Self::route) and [`json`](Self::json), or load a hand-written cassette with
/// [`cassette`](Self::cassette). Share it as an `Arc<dyn HttpTransport>` like the real client.
#[derive(Debug, Default)]
pub struct FakeHttp {
    state: Mutex<State>,
}

/// Everything the fake remembers: its routes and the two logs.
#[derive(Debug, Default)]
struct State {
    routes: Vec<Route>,
    /// Requests a route answered, in order.
    calls: Vec<Logged>,
    /// Requests no route matched, in order.
    unmatched: Vec<Logged>,
}

/// A route: a method, an exact URL, an optional body substring and the replies it hands out. The first route that
/// matches a request answers it.
#[derive(Debug)]
pub(crate) struct Route {
    method: Method,
    url: String,
    body_contains: Option<String>,
    replies: Vec<Reply>,
    /// Index of the next reply to hand out; the last reply repeats once the list runs out.
    cursor: usize,
}

impl Route {
    /// A route that has not answered anything yet.
    pub(crate) fn new(method: Method, url: String, body_contains: Option<String>, replies: Vec<Reply>) -> Self {
        Self { method, url, body_contains, replies, cursor: 0 }
    }

    /// Whether this route answers the logged request: same method, same URL, and the body substring if it has one.
    fn matches(&self, logged: &Logged) -> bool {
        self.method == logged.method
            && self.url == logged.url
            && self.body_contains.as_deref().is_none_or(|needle| contains(&logged.body, needle.as_bytes()))
    }

    /// The next reply to hand out. A route with no replies refuses: it has nothing scripted to send.
    fn next_reply(&mut self) -> Reply {
        let Some(last) = self.replies.len().checked_sub(1) else {
            return Reply::Error(HttpError::Refused(format!(
                "no reply is scripted for {} {}",
                method_name(self.method),
                self.url
            )));
        };
        let reply = self.replies[self.cursor.min(last)].clone();
        self.cursor = self.cursor.saturating_add(1);
        reply
    }
}

impl FakeHttp {
    /// An empty fake: every request is unmatched until a route is added.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a route. `replies` are handed out in order and the last one repeats, so a route with one reply always
    /// answers the same way. The first route that matches a request wins.
    #[must_use]
    pub fn route(mut self, method: Method, url: &str, replies: Vec<Reply>) -> Self {
        let state = self.state.get_mut().unwrap_or_else(PoisonError::into_inner);
        state.routes.push(Route::new(method, url.to_owned(), None, replies));
        self
    }

    /// Adds a route that answers with one `status` and the JSON `body`, sent as `application/json`.
    #[must_use]
    // The signature takes the body by value, as the contract states; it is serialised once and then dropped.
    #[allow(clippy::needless_pass_by_value)]
    pub fn json(self, method: Method, url: &str, status: u16, body: Value) -> Self {
        let response = HttpResponse {
            status,
            headers: vec![("content-type".to_owned(), "application/json".to_owned())],
            body: body.to_string().into_bytes(),
        };
        self.route(method, url, vec![Reply::Response(response)])
    }

    /// Loads routes from a hand-written cassette, in document order. The format:
    ///
    /// ```text
    /// {"routes":[{"method":"POST","url":"...","body_contains":"...","replies":[
    ///   {"status":200,"headers":{"content-type":"application/json"},"body_json":{...} | "body_text" | "body_base64"
    ///    | "chunks":[{"text":"..."} | {"base64":"..."} | "stall"]} ]}]}
    /// ```
    ///
    /// A reply holds at most one body kind; a reply with none has an empty body. `body_contains` is matched against
    /// the request body bytes, where a JSON request body is compact.
    ///
    /// # Errors
    /// [`CassetteError::Json`] when the text is not a document with a `routes` array, and
    /// [`CassetteError::Route`] for the first route that is malformed, naming its index.
    pub fn cassette(json: &str) -> Result<Self, CassetteError> {
        let routes = cassette::parse(json)?;
        Ok(Self { state: Mutex::new(State { routes, ..State::default() }) })
    }

    /// The requests that routes answered, in the order they arrived. Unmatched requests are in
    /// [`unmatched`](Self::unmatched).
    #[must_use]
    pub fn calls(&self) -> Vec<Logged> {
        self.locked().calls.clone()
    }

    /// How many answered requests have a URL containing `url_part`.
    #[must_use]
    pub fn calls_to(&self, url_part: &str) -> usize {
        self.locked().calls.iter().filter(|call| call.url.contains(url_part)).count()
    }

    /// The requests no route matched, in the order they arrived. Each one failed with [`HttpError::Network`].
    #[must_use]
    pub fn unmatched(&self) -> Vec<Logged> {
        self.locked().unmatched.clone()
    }

    fn locked(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Logs the request and returns the reply of the route that matched it, or `None` (the request is logged as
    /// unmatched).
    fn take_reply(&self, req: &HttpRequest) -> Option<Reply> {
        let logged = Logged {
            method: req.method,
            url: req.url.clone(),
            headers: req.headers.clone(),
            body: body_bytes(&req.body),
        };
        let mut guard = self.locked();
        let State { routes, calls, unmatched } = &mut *guard;
        if let Some(route) = routes.iter_mut().find(|route| route.matches(&logged)) {
            let reply = route.next_reply();
            calls.push(logged);
            Some(reply)
        } else {
            unmatched.push(logged);
            None
        }
    }
}

impl HttpTransport for FakeHttp {
    fn send(&self, req: &HttpRequest, cancel: &dyn Cancel) -> Result<HttpResponse, HttpError> {
        if cancel.is_cancelled() {
            return Err(HttpError::Cancelled);
        }
        let Some(reply) = self.take_reply(req) else {
            return Err(unmatched_error(req));
        };
        match reply {
            Reply::Response(response) => {
                if stream::over_limit(response.body.len(), req.max_response) {
                    return Err(HttpError::TooLarge { limit: req.max_response });
                }
                Ok(response)
            }
            Reply::Error(error) => Err(error),
            Reply::Hang => Err(stream::poll_until_cancelled(cancel)),
            Reply::Stream(script) => stream::join_chunks(script, req.max_response, cancel),
        }
    }

    fn open(&self, req: &HttpRequest, cancel: Arc<dyn Cancel>) -> Result<HttpStream, HttpError> {
        if cancel.is_cancelled() {
            return Err(HttpError::Cancelled);
        }
        let Some(reply) = self.take_reply(req) else {
            return Err(unmatched_error(req));
        };
        match reply {
            // A whole response streams as one chunk, so the same read and limit rules apply to both.
            Reply::Response(response) => Ok(stream::open_body(
                response.status,
                response.headers,
                vec![Chunk::Data(response.body)],
                req.max_response,
                cancel,
            )),
            Reply::Stream(script) => {
                Ok(stream::open_body(script.status, script.headers, script.chunks, req.max_response, cancel))
            }
            Reply::Error(error) => Err(error),
            Reply::Hang => Err(stream::poll_until_cancelled(cancel.as_ref())),
        }
    }
}

/// The error an unmatched request fails with. It names the request so the test that failed can see which one it was.
fn unmatched_error(req: &HttpRequest) -> HttpError {
    HttpError::Network(format!("no route for {} {}", method_name(req.method), req.url))
}

fn method_name(method: Method) -> &'static str {
    match method {
        Method::Get => "GET",
        Method::Post => "POST",
        Method::Put => "PUT",
        Method::Patch => "PATCH",
        Method::Delete => "DELETE",
    }
}

/// The bytes a request body puts on the wire, as the fake records them.
fn body_bytes(body: &HttpBody) -> Vec<u8> {
    match body {
        HttpBody::Empty => Vec::new(),
        HttpBody::Bytes { data, .. } => data.clone(),
        HttpBody::Json(value) => value.to_string().into_bytes(),
        HttpBody::Multipart(parts) => parts.iter().flat_map(|part| part.data.iter().copied()).collect(),
    }
}

/// True when `needle` occurs in `haystack`. The empty needle occurs everywhere.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    needle.is_empty() || haystack.windows(needle.len()).any(|window| window == needle)
}
