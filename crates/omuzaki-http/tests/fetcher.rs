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

/// The test fetcher: loopback http on (which also bypasses any proxy) and no backoff.
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

fn redirect(location: &str) -> Reply {
    Response::builder().status(302).header("location", location).body(Full::new(Bytes::new())).expect("a redirect")
}

/// A server whose `/hop/<k>` redirects to `/hop/<k+1>` until `k == hops`, which answers 200.
fn hop_server(hops: usize) -> Server {
    Server::start(move |_, req| {
        let k = req.uri().path().strip_prefix("/hop/").and_then(|k| k.parse::<usize>().ok()).unwrap_or(0);
        if k < hops { redirect(&format!("/hop/{}", k + 1)) } else { reply(200, "arrived") }
    })
}

#[test]
fn two_redirects_are_followed() {
    let server = hop_server(2);
    let (_, bytes) = read_all(&fetcher(), &server.url("/hop/0"), 0).expect("two redirects are fine");
    assert_eq!(bytes, b"arrived");
    assert_eq!(server.hits(), 3);
}

#[test]
fn five_redirects_are_followed_and_six_are_refused() {
    let five = hop_server(5);
    assert!(read_all(&fetcher(), &five.url("/hop/0"), 0).is_ok(), "the limit itself is allowed");
    let six = hop_server(6);
    let error = read_all(&fetcher(), &six.url("/hop/0"), 0).expect_err("six redirects must fail");
    assert!(matches!(error, FetchError::Network { .. }));
    assert_eq!(six.hits(), 6, "the sixth redirect is not followed");
}

#[test]
fn a_redirect_to_plain_http_is_refused() {
    // A non-loopback http target: refused by the redirect policy, so nothing connects to it.
    let server = Server::start(|_, _| redirect("http://192.0.2.1/model.bin"));
    let error = read_all(&fetcher(), &server.url("/a"), 0).expect_err("an http redirect must fail");
    assert!(matches!(error, FetchError::Network { .. }));
    assert_eq!(server.hits(), 1);
}

#[test]
fn plain_http_to_a_non_loopback_host_fails_without_connecting() {
    // Loopback http is refused too when the test aid is off, and the server never sees a connection.
    let server = Server::start(|_, _| reply(200, "never"));
    let strict = HttpFetcher::builder().backoff(Duration::ZERO).build();
    assert!(reason(strict.open(&server.url("/a"), 0).map(|_| ()).expect_err("refused")).contains("https"));
    assert_eq!(server.hits(), 0);
    // A non-loopback host with the test aid on: refused before any connection (192.0.2.1 is reserved, so a connect
    // attempt would hang for the connect timeout rather than fail at once).
    let started = std::time::Instant::now();
    let error = fetcher().open("http://192.0.2.1/a", 0).map(|_| ()).expect_err("refused");
    assert!(reason(error).contains("https"));
    assert!(started.elapsed() < Duration::from_secs(2), "it must fail without trying to connect");
}

#[test]
fn an_endless_body_is_dropped_at_the_limit_and_the_server_sees_the_close() {
    use std::io::Write;
    use std::sync::mpsc::channel;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else { return };
        let mut request = [0u8; 1024];
        let _ = stream.read(&mut request);
        if stream.write_all(b"HTTP/1.1 200 OK\r\nconnection: close\r\n\r\n").is_err() {
            return;
        }
        let chunk = [b'x'; 8192];
        let mut written = 0usize;
        // Writes until the client closes (a write error); an endless body never ends on its own.
        while stream.write_all(&chunk).is_ok() {
            written += chunk.len();
        }
        let _ = tx.send(written);
    });
    let wanted = 10_000;
    let mut body = fetcher().open(&format!("http://{addr}/"), 0).expect("opened");
    let mut got = vec![0u8; wanted];
    body.reader.read_exact(&mut got).expect("the first bytes arrive");
    drop(body);
    let written = rx.recv_timeout(Duration::from_secs(10)).expect("the server sees the connection close");
    // The kernel's socket buffers hold some of what was written before the close; the bound is that the writer
    // stopped, far short of anything unbounded.
    assert!(written < 32 * 1024 * 1024, "the server wrote {written} bytes before the close");
}

/// Re-run by `loopback_bypasses_the_proxy_settings` in a child process with dead proxies in the environment; run on
/// its own it passes trivially, because the environment is the machine's.
#[test]
#[ignore = "run by loopback_bypasses_the_proxy_settings in a child process"]
fn loopback_fetch_child() {
    let server = Server::start(|_, _| reply(200, "direct"));
    let (_, bytes) = read_all(&fetcher(), &server.url("/a"), 0).expect("loopback must not go through the proxy");
    assert_eq!(bytes, b"direct");
}

#[test]
fn loopback_bypasses_the_proxy_settings() {
    // Process environment is shared by parallel tests, so the dead proxies go to a child copy of this test binary.
    let exe = std::env::current_exe().expect("the test binary");
    let dead = "http://127.0.0.1:1";
    let status = std::process::Command::new(exe)
        .args(["--ignored", "--exact", "loopback_fetch_child"])
        .env("HTTP_PROXY", dead)
        .env("HTTPS_PROXY", dead)
        .env("ALL_PROXY", dead)
        .env("http_proxy", dead)
        .env("https_proxy", dead)
        .env("all_proxy", dead)
        .env_remove("NO_PROXY")
        .env_remove("no_proxy")
        .output()
        .expect("the child test ran");
    assert!(status.status.success(), "{}", String::from_utf8_lossy(&status.stdout));
}

/// Construction is documented as infallible, so it must not panic where reqwest's blocking builder would: on a thread
/// that runs async tasks.
#[tokio::test(flavor = "multi_thread")]
async fn building_a_fetcher_inside_an_async_runtime_does_not_panic() {
    let from_new = HttpFetcher::new();
    let from_builder = HttpFetcher::builder().allow_loopback_http(true).build();
    drop((from_new, from_builder));
}

fn throttled(status: u16, retry_after: &'static str) -> Reply {
    Response::builder()
        .status(status)
        .header("retry-after", retry_after)
        .body(Full::new(Bytes::new()))
        .expect("a valid response")
}

#[test]
fn a_503_with_retry_after_waits_that_long_before_the_next_try() {
    let server = Server::start(|n, _| if n == 0 { throttled(503, "1") } else { reply(200, "ok") });
    let started = std::time::Instant::now();
    let (_, bytes) = read_all(&fetcher(), &server.url("/a"), 0).expect("fetched after waiting");
    assert_eq!(bytes, b"ok");
    assert!(started.elapsed() >= Duration::from_secs(1), "waited only {:?}", started.elapsed());
    assert_eq!(server.hits(), 2);
}

#[test]
fn a_429_with_an_http_date_in_the_past_retries_without_waiting() {
    let server =
        Server::start(|n, _| if n == 0 { throttled(429, "Sun, 06 Nov 1994 08:49:37 GMT") } else { reply(200, "ok") });
    let (_, bytes) = read_all(&fetcher(), &server.url("/a"), 0).expect("fetched");
    assert_eq!((bytes.as_slice(), server.hits()), (b"ok".as_slice(), 2));
}

#[test]
fn a_retry_after_over_the_cap_fails_at_once_without_a_second_request() {
    let server = Server::start(|_, _| throttled(429, "3600"));
    let started = std::time::Instant::now();
    let error = read_all(&fetcher(), &server.url("/a"), 0).expect_err("over the cap");
    let text = reason(error);
    assert!(text.contains("429") && text.contains("3600"), "{text}");
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(server.hits(), 1);
}

#[test]
fn the_cap_is_configurable() {
    let server = Server::start(|n, _| if n == 0 { throttled(503, "2") } else { reply(200, "ok") });
    let capped = HttpFetcher::builder()
        .allow_loopback_http(true)
        .backoff(Duration::ZERO)
        .max_retry_after(Duration::from_secs(1))
        .build();
    assert!(reason(read_all(&capped, &server.url("/a"), 0).expect_err("2 s is over a 1 s cap")).contains("503"));
    assert_eq!(server.hits(), 1);
}

#[test]
fn a_malformed_retry_after_falls_back_to_the_backoff() {
    let server = Server::start(|n, _| if n < 2 { throttled(503, "soon") } else { reply(200, "ok") });
    let started = std::time::Instant::now();
    let (_, bytes) = read_all(&fetcher(), &server.url("/a"), 0).expect("fetched");
    assert_eq!(bytes, b"ok");
    assert!(started.elapsed() < Duration::from_secs(1), "the zero backoff was used");
    assert_eq!(server.hits(), 3);
}

#[test]
fn retry_after_on_a_500_is_ignored() {
    let server = Server::start(|n, _| if n == 0 { throttled(500, "3600") } else { reply(200, "ok") });
    let (_, bytes) = read_all(&fetcher(), &server.url("/a"), 0).expect("retried on the backoff");
    assert_eq!((bytes.as_slice(), server.hits()), (b"ok".as_slice(), 2));
}

#[test]
fn a_throttled_last_try_gives_up_without_sleeping_again() {
    let server = Server::start(|_, _| throttled(503, "1"));
    let one_try = HttpFetcher::builder().allow_loopback_http(true).retries(1).build();
    let started = std::time::Instant::now();
    assert!(reason(read_all(&one_try, &server.url("/a"), 0).expect_err("gives up")).contains("gave up after 1"));
    assert!(started.elapsed() < Duration::from_millis(900));
}
