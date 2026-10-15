//! `HttpFetcher` against a local hyper server on 127.0.0.1:0 (no outside network). The server counts requests.

#![allow(clippy::expect_used)] // helpers outside `#[test]` fns: a panic is a clear test failure

use std::convert::Infallible;
use std::io::Read;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use omuzaki_ai_api::{FetchError, Fetcher};
use omuzaki_http::HttpFetcher;
use tokio::net::TcpListener;

type Reply = Response<Full<Bytes>>;
type Handler = dyn Fn(usize, &Request<Incoming>) -> Reply + Send + Sync;

/// A running server: its address, how many requests it has seen and the `User-Agent` of the last one.
struct Server {
    addr: SocketAddr,
    hits: Arc<AtomicUsize>,
    agent: Arc<Mutex<String>>,
}

impl Server {
    /// Serves `handler(n, request)` for the n-th request (from 0) on a thread of its own; the thread ends with the test
    /// process.
    fn start(handler: impl Fn(usize, &Request<Incoming>) -> Reply + Send + Sync + 'static) -> Self {
        let handler: Arc<Handler> = Arc::new(handler);
        let hits = Arc::new(AtomicUsize::new(0));
        let agent = Arc::new(Mutex::new(String::new()));
        let (tx, rx) = std::sync::mpsc::channel();
        let (h, a) = (hits.clone(), agent.clone());
        std::thread::spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
            rt.block_on(async move {
                let Ok(listener) = TcpListener::bind("127.0.0.1:0").await else { return };
                if let Ok(addr) = listener.local_addr() {
                    let _ = tx.send(addr);
                }
                while let Ok((stream, _)) = listener.accept().await {
                    let (handler, hits, agent) = (handler.clone(), h.clone(), a.clone());
                    tokio::spawn(async move {
                        let service = service_fn(move |req: Request<Incoming>| {
                            let n = hits.fetch_add(1, Ordering::SeqCst);
                            let ua = req.headers().get("user-agent").and_then(|v| v.to_str().ok());
                            if let (Some(ua), Ok(mut slot)) = (ua, agent.lock()) {
                                ua.clone_into(&mut slot);
                            }
                            let reply = handler(n, &req);
                            async move { Ok::<_, Infallible>(reply) }
                        });
                        let _ = hyper::server::conn::http1::Builder::new()
                            .serve_connection(TokioIo::new(stream), service)
                            .await;
                    });
                }
            });
        });
        let addr = rx.recv_timeout(Duration::from_secs(10)).expect("the test server did not start");
        Self { addr, hits, agent }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

fn reply(status: u16, body: &'static str) -> Reply {
    Response::builder().status(status).body(Full::new(Bytes::from_static(body.as_bytes()))).expect("a valid response")
}

/// The test fetcher: loopback http on, no backoff, and the system proxy left out of the way by `NO_PROXY` on CI.
fn fetcher() -> HttpFetcher {
    HttpFetcher::builder().allow_loopback_http(true).backoff(Duration::ZERO).build()
}

fn read_all(fetcher: &HttpFetcher, url: &str, from: u64) -> Result<(u64, Vec<u8>), FetchError> {
    let mut body = fetcher.open(url, from)?;
    let mut bytes = Vec::new();
    body.reader.read_to_end(&mut bytes).expect("the body reads to its end");
    Ok((body.start, bytes))
}

fn reason(error: FetchError) -> String {
    match error {
        FetchError::Network { reason, .. } => reason,
        other => panic!("expected a network error, got {other:?}"),
    }
}

#[test]
fn a_200_returns_the_whole_body_and_the_user_agent_is_sent() {
    let server = Server::start(|_, _| reply(200, "hello"));
    let (start, bytes) = read_all(&fetcher(), &server.url("/a"), 0).expect("fetched");
    assert_eq!((start, bytes.as_slice()), (0, b"hello".as_slice()));
    assert_eq!(server.agent.lock().expect("lock").as_str(), concat!("Omuzaki/", env!("CARGO_PKG_VERSION")));
}

#[test]
fn a_range_request_sends_the_header_and_a_206_starts_at_from() {
    let server = Server::start(|_, req| {
        let range = req.headers().get("range").and_then(|v| v.to_str().ok()).unwrap_or("none");
        assert_eq!(range, "bytes=3-");
        Response::builder()
            .status(206)
            .header("content-range", "bytes 3-4/5")
            .body(Full::new(Bytes::from_static(b"lo")))
            .expect("response")
    });
    let (start, bytes) = read_all(&fetcher(), &server.url("/a"), 3).expect("fetched");
    assert_eq!((start, bytes.as_slice()), (3, b"lo".as_slice()));
}

#[test]
fn a_200_answering_a_range_request_starts_at_zero() {
    let server = Server::start(|_, _| reply(200, "hello"));
    let (start, bytes) = read_all(&fetcher(), &server.url("/a"), 3).expect("fetched");
    assert_eq!((start, bytes.len()), (0, 5));
}

#[test]
fn a_206_with_the_wrong_content_range_is_an_error() {
    let server = Server::start(|_, _| {
        Response::builder()
            .status(206)
            .header("content-range", "bytes 0-4/5")
            .body(Full::new(Bytes::from_static(b"hello")))
            .expect("response")
    });
    let error = read_all(&fetcher(), &server.url("/a"), 3).expect_err("a wrong range must fail");
    assert!(reason(error).contains("Content-Range"));
}

#[test]
fn a_416_and_a_404_are_errors_naming_the_status() {
    let server = Server::start(|n, _| if n == 0 { reply(416, "") } else { reply(404, "") });
    let fetcher = fetcher();
    assert!(reason(read_all(&fetcher, &server.url("/a"), 9).expect_err("416")).contains("416"));
    assert!(reason(read_all(&fetcher, &server.url("/b"), 0).expect_err("404")).contains("404"));
    assert_eq!(server.hits(), 2, "client errors are not retried");
}

#[test]
fn two_503s_then_a_200_succeeds_after_retrying() {
    let server = Server::start(|n, _| if n < 2 { reply(503, "") } else { reply(200, "ok") });
    let (_, bytes) = read_all(&fetcher(), &server.url("/a"), 0).expect("fetched");
    assert_eq!(bytes, b"ok");
    assert_eq!(server.hits(), 3);
}

#[test]
fn a_500_that_never_stops_gives_up_after_three_tries() {
    let server = Server::start(|_, _| reply(500, ""));
    let error = read_all(&fetcher(), &server.url("/a"), 0).expect_err("must give up");
    assert!(reason(error).contains("500"));
    assert_eq!(server.hits(), 3);
}

#[test]
fn a_refused_connection_is_an_error() {
    // Bind and drop a listener to get a port nothing listens on.
    let port = std::net::TcpListener::bind("127.0.0.1:0").and_then(|l| l.local_addr()).expect("a free port").port();
    let error = read_all(&fetcher(), &format!("http://127.0.0.1:{port}/"), 0).expect_err("refused");
    assert!(matches!(error, FetchError::Network { .. }));
}

/// A one-connection server that writes `head`, then each `(pause, bytes)` step, over a raw socket so the body can
/// trickle. The thread ends with the test process.
fn trickle_server(head: &'static str, steps: Vec<(Duration, &'static str)>) -> SocketAddr {
    use std::io::Write;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else { return };
        let mut request = [0u8; 1024];
        let _ = stream.read(&mut request);
        let _ = stream.write_all(head.as_bytes());
        for (pause, bytes) in steps {
            std::thread::sleep(pause);
            let _ = stream.write_all(bytes.as_bytes());
            let _ = stream.flush();
        }
    });
    addr
}

#[test]
fn the_read_timeout_is_per_read_so_a_slow_steady_body_outlives_it() {
    // Six bytes in three chunks 200 ms apart: 600+ ms in all, but never silent for longer than the 500 ms timeout.
    let step = (Duration::from_millis(200), "ab");
    let addr = trickle_server("HTTP/1.1 200 OK\r\ncontent-length: 6\r\n\r\n", vec![step, step, step]);
    let fetcher =
        HttpFetcher::builder().allow_loopback_http(true).read_timeout(Duration::from_millis(500)).retries(1).build();
    let (_, bytes) = read_all(&fetcher, &format!("http://{addr}/"), 0).expect("a steady body is not cut off");
    assert_eq!(bytes, b"ababab");
}

#[test]
fn a_body_that_stalls_longer_than_the_read_timeout_fails() {
    let addr = trickle_server(
        "HTTP/1.1 200 OK\r\ncontent-length: 6\r\n\r\n",
        vec![(Duration::ZERO, "ab"), (Duration::from_secs(5), "cdef")],
    );
    let fetcher =
        HttpFetcher::builder().allow_loopback_http(true).read_timeout(Duration::from_millis(300)).retries(1).build();
    let mut body = fetcher.open(&format!("http://{addr}/"), 0).expect("the headers arrive in time");
    let started = std::time::Instant::now();
    let mut bytes = Vec::new();
    assert!(body.reader.read_to_end(&mut bytes).is_err(), "a stalled body must time out");
    assert!(started.elapsed() < Duration::from_secs(4), "it must not wait for the stalled server");
}
