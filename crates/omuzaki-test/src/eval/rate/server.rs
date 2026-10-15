//! The rating page's server: a few requests over plain HTTP on `127.0.0.1`, one connection at a time. Nothing is uploaded and
//! nothing leaves the machine.
//!
//! What stands between a request and the ratings file, in order:
//! 1. **Loopback only.** The listener is the caller's, and [`bind`] binds `127.0.0.1`.
//! 2. **`Host`.** Only `127.0.0.1:<port>` and `localhost:<port>` are served, so a page that rebinds its own name to the
//!    loopback address is refused.
//! 3. **`Origin`.** A request that carries one must carry ours, so a page on another origin cannot post a rating.
//! 4. **A token.** A rating is accepted only with the random token the page read from `/api/items`. Another origin cannot
//!    read that response (no CORS header is ever sent), so it cannot learn the token.
//! 5. **Only what was asked.** A rating must name an item of this session, one of its questions, and an answer; a
//!    question is answered once. Images are addressed by position in the session, never by a path.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};

use super::sample::Item;
use super::{Answer, RateError, Rating, append};
use crate::eval::clock::utc_minute;

const PAGE: &str = include_str!("page/index.html");
const SCRIPT: &str = include_str!("page/app.js");
const STYLE: &str = include_str!("page/app.css");

/// The longest request head accepted.
const MAX_HEAD: usize = 16 * 1024;
/// The longest request body accepted.
const MAX_BODY: usize = 64 * 1024;
/// How long a connection may stay silent.
const READ_TIMEOUT: Duration = Duration::from_secs(5);
/// The two pictures an attempt's folder may hold.
const PICTURES: [&str; 2] = ["before", "after"];

/// Binds the loopback address on `port` (0 lets the system choose).
///
/// # Errors
/// [`RateError::Io`] when it cannot bind.
pub fn bind(port: u16) -> Result<TcpListener, RateError> {
    TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
        .map_err(|source| RateError::Io { path: PathBuf::from(format!("127.0.0.1:{port}")), source })
}

/// A random token for one session.
///
/// # Errors
/// [`RateError::Invalid`] when the system has no randomness to give.
pub fn new_token() -> Result<String, RateError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| RateError::Invalid(format!("no randomness for a session token: {e}")))?;
    Ok(format!("{:032x}", u128::from_le_bytes(bytes)))
}

/// One rating session.
pub struct Session {
    /// The results to rate.
    pub items: Vec<Item>,
    /// The ratings file.
    pub ratings: PathBuf,
    /// `<evals folder>/runs`, where an attempt's pictures are.
    pub runs: PathBuf,
    /// Who is rating.
    pub rater: String,
    /// The session token.
    pub token: String,
    /// The clock the ratings are stamped with.
    pub clock: fn() -> SystemTime,
}

/// How the session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// Every question of every item was answered.
    Done,
    /// The rater closed the page.
    Quit,
}

/// What a request was refused for: the status line, and a word for the body.
struct Refusal(u16, &'static str);

/// Serves `session` on `listener` until every question is answered or the rater quits.
///
/// # Errors
/// [`RateError::Io`] when the listener fails or a rating cannot be written.
pub fn serve(listener: &TcpListener, session: &Session) -> Result<Ended, RateError> {
    let port =
        listener.local_addr().map_err(|source| RateError::Io { path: PathBuf::from("127.0.0.1"), source })?.port();
    let mut answered: BTreeSet<(String, String)> = BTreeSet::new();
    // Answers wait here until the item's last question is answered, and only then go to the file together: an item that is
    // half rated when the rater leaves has no rating at all, so it is asked about again in full.
    let mut pending: BTreeMap<String, Vec<Rating>> = BTreeMap::new();
    let total: usize = session.items.iter().map(|i| i.questions.len()).sum();
    loop {
        let (mut stream, _) =
            listener.accept().map_err(|source| RateError::Io { path: PathBuf::from("127.0.0.1"), source })?;
        // A client that connects and says nothing, or hangs up, is not the session's concern.
        if stream.set_read_timeout(Some(READ_TIMEOUT)).is_err() {
            continue;
        }
        let Ok(request) = read_request(&mut stream) else {
            drop(respond(&mut stream, 400, "text/plain", b"bad request"));
            continue;
        };
        let outcome = handle(&request, session, port, &mut answered, &mut pending);
        let (status, kind, body, ended) = match outcome {
            Ok((kind, body, ended)) => (200, kind, body, ended),
            Err(Refusal(status, why)) => (status, "text/plain", why.as_bytes().to_vec(), None),
        };
        // A client that went away before the answer is read has nothing to be told.
        drop(respond(&mut stream, status, kind, &body));
        let finished = ended.or_else(|| (answered.len() >= total && total > 0).then_some(Ended::Done));
        if let Some(ended) = finished {
            return Ok(ended);
        }
    }
}

struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

fn read_request(stream: &mut TcpStream) -> io::Result<Request> {
    let bad = |why: &str| io::Error::new(io::ErrorKind::InvalidData, why.to_owned());
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 2048];
    let head_end = loop {
        if let Some(at) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break at;
        }
        if buffer.len() > MAX_HEAD {
            return Err(bad("the request head is too long"));
        }
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Err(bad("the request ended early"));
        }
        buffer.extend_from_slice(&chunk[..read]);
    };
    let head = std::str::from_utf8(&buffer[..head_end]).map_err(|_| bad("the request head is not text"))?;
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or_default().split(' ');
    let (method, path) = (first.next().unwrap_or_default().to_owned(), first.next().unwrap_or_default().to_owned());
    let headers: Vec<(String, String)> =
        lines.filter_map(|l| l.split_once(':')).map(|(n, v)| (n.trim().to_owned(), v.trim().to_owned())).collect();
    let length = headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case("content-length"))
        .map(|(_, v)| v.parse::<usize>().map_err(|_| bad("content-length is not a number")))
        .transpose()?
        .unwrap_or(0);
    if length > MAX_BODY {
        return Err(bad("the request body is too long"));
    }
    let mut body = buffer[head_end + 4..].to_vec();
    while body.len() < length {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Err(bad("the request body ended early"));
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(length);
    Ok(Request { method, path, headers, body })
}

fn respond(stream: &mut TcpStream, status: u16, kind: &str, body: &[u8]) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\nContent-Security-Policy: default-src 'none'; script-src 'self'; style-src 'self'; \
         img-src 'self'; connect-src 'self'\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

type Handled = Result<(&'static str, Vec<u8>, Option<Ended>), Refusal>;

fn handle(
    request: &Request,
    session: &Session,
    port: u16,
    answered: &mut BTreeSet<(String, String)>,
    pending: &mut BTreeMap<String, Vec<Rating>>,
) -> Handled {
    let host = request.header("host").unwrap_or_default();
    if host != format!("127.0.0.1:{port}") && host != format!("localhost:{port}") {
        return Err(Refusal(403, "this page is served to the local machine only"));
    }
    let text = |kind, body: &str| Ok((kind, body.as_bytes().to_vec(), None));
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/") => text("text/html; charset=utf-8", PAGE),
        ("GET", "/app.js") => text("text/javascript; charset=utf-8", SCRIPT),
        ("GET", "/app.css") => text("text/css; charset=utf-8", STYLE),
        ("GET", "/api/items") => {
            let items: Vec<Value> = session
                .items
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    json!({
                        "id": item.id, "task": item.task, "attempt": item.attempt, "prompt": item.prompt,
                        "questions": item.questions,
                        "pictures": PICTURES.iter().map(|p| picture_path(session, item).join(format!("{p}.png")).is_file()).collect::<Vec<_>>(),
                        "index": index,
                    })
                })
                .collect();
            let body = json!({ "token": session.token, "rater": session.rater, "pictures": PICTURES, "items": items });
            Ok(("application/json", body.to_string().into_bytes(), None))
        }
        ("GET", path) if path.starts_with("/img/") => picture(session, &path["/img/".len()..]),
        ("POST", "/api/rate") => {
            check_post(request, session, port)?;
            rate(request, session, answered, pending)
        }
        ("POST", "/api/quit") => {
            check_post(request, session, port)?;
            Ok(("application/json", b"{\"ok\":true}".to_vec(), Some(Ended::Quit)))
        }
        ("GET" | "POST", _) => Err(Refusal(404, "not found")),
        _ => Err(Refusal(405, "only GET and POST")),
    }
}

fn check_post(request: &Request, session: &Session, port: u16) -> Result<(), Refusal> {
    if let Some(origin) = request.header("origin")
        && origin != format!("http://127.0.0.1:{port}")
        && origin != format!("http://localhost:{port}")
    {
        return Err(Refusal(403, "this origin may not rate"));
    }
    if request.header("x-rating-token") != Some(session.token.as_str()) {
        return Err(Refusal(403, "the session token is missing or wrong"));
    }
    Ok(())
}

fn picture_path(session: &Session, item: &Item) -> PathBuf {
    session.runs.join(&item.run).join(&item.task).join(item.attempt.to_string())
}

/// `/img/<index>/<before|after>.png`: a picture of the item at that position in the session.
fn picture(session: &Session, rest: &str) -> Handled {
    let (index, name) = rest.split_once('/').ok_or(Refusal(404, "not found"))?;
    let item = index.parse::<usize>().ok().and_then(|i| session.items.get(i)).ok_or(Refusal(404, "not found"))?;
    let stem = name.strip_suffix(".png").filter(|s| PICTURES.contains(s)).ok_or(Refusal(404, "not found"))?;
    let bytes = fs::read(picture_path(session, item).join(format!("{stem}.png")))
        .map_err(|_| Refusal(404, "no picture kept"))?;
    Ok(("image/png", bytes, None))
}

fn rate(
    request: &Request,
    session: &Session,
    answered: &mut BTreeSet<(String, String)>,
    pending: &mut BTreeMap<String, Vec<Rating>>,
) -> Handled {
    let body: Value = serde_json::from_slice(&request.body).map_err(|_| Refusal(400, "the body is not JSON"))?;
    let field =
        |name: &str| body.get(name).and_then(Value::as_str).ok_or(Refusal(400, "item, question and answer are text"));
    let (item_id, question) = (field("item")?, field("question")?);
    let answer = match field("answer")? {
        "yes" => Answer::Yes,
        "no" => Answer::No,
        "unsure" => Answer::Unsure,
        _ => return Err(Refusal(400, "the answer is yes, no or unsure")),
    };
    let item =
        session.items.iter().find(|i| i.id == item_id).ok_or(Refusal(400, "that item is not in this session"))?;
    if !item.questions.iter().any(|q| q == question) {
        return Err(Refusal(400, "that question is not one of the item's"));
    }
    let key = (item.id.clone(), question.to_owned());
    if answered.contains(&key) {
        return Err(Refusal(409, "that question is answered"));
    }
    let rating = Rating {
        item: item.id.clone(),
        task: item.task.clone(),
        attempt: item.attempt,
        question: question.to_owned(),
        answer,
        rater: session.rater.clone(),
        at: utc_minute((session.clock)()),
    };
    let waiting = pending.entry(item.id.clone()).or_default();
    waiting.push(rating);
    if waiting.len() == item.questions.len() {
        for rating in waiting.iter() {
            append(&session.ratings, rating).map_err(|_| Refusal(500, "the ratings could not be written"))?;
        }
        pending.remove(&item.id);
    }
    answered.insert(key);
    let total: usize = session.items.iter().map(|i| i.questions.len()).sum();
    let left = total - answered.len();
    Ok(("application/json", json!({ "ok": true, "remaining": left, "done": left == 0 }).to_string().into_bytes(), None))
}
