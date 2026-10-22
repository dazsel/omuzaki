//! `ConnectHttp`, the shared `HttpTransport`, against a raw HTTP/1.1 server on 127.0.0.1:0 that writes scripted
//! bytes (so it can stall mid-body): the request as sent, a non-2xx answer, the size limit, a stream cut at the limit,
//! cancelling a silent server, the timeout, and the https-only rule.

#![allow(clippy::expect_used, clippy::unwrap_used)] // helpers outside `#[test]` fns: a panic is a clear test failure

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use omuzaki_http::ConnectHttp;
use omuzaki_http_api::{CancelFlag, HttpBody, HttpError, HttpRequest, HttpTransport, Method, NeverCancel, Part};
use serde_json::json;

/// A request as the server read it.
struct Seen {
    head: String,
    body: Vec<u8>,
}

/// Serves one connection: reads the request, writes `parts` (a `None` part stalls until the client hangs up).
fn server(parts: Vec<Option<&'static [u8]>>) -> (String, Receiver<Seen>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/x", listener.local_addr().unwrap());
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let Ok((stream, _)) = listener.accept() else { return };
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut head = String::new();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                break;
            }
            head.push_str(&line);
        }
        let length = head
            .lines()
            .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap()))
            .unwrap_or(0);
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        let _ = tx.send(Seen { head, body });
        write(stream, &parts);
    });
    (url, rx)
}

fn write(mut stream: TcpStream, parts: &[Option<&[u8]>]) {
    for part in parts {
        let Some(bytes) = part else {
            // Stall: wait for the client to go away.
            let mut sink = [0; 64];
            let _ = stream.set_read_timeout(Some(Duration::from_secs(20)));
            while matches!(stream.read(&mut sink), Ok(n) if n > 0) {}
            return;
        };
        if stream.write_all(bytes).and_then(|()| stream.flush()).is_err() {
            return;
        }
    }
}

fn transport() -> ConnectHttp {
    ConnectHttp::new()
}

fn request(method: Method, url: &str, body: HttpBody) -> HttpRequest {
    HttpRequest {
        method,
        url: url.to_owned(),
        headers: vec![("Authorization".to_owned(), "Bearer test-only".to_owned())],
        body,
        timeout_s: 10,
        max_response: 1024,
    }
}

const OK: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\n\r\n{\"ok\":true}";

#[test]
fn send_carries_the_method_headers_and_json_and_returns_the_whole_answer() {
    let (url, seen) = server(vec![Some(OK)]);
    let req = request(Method::Post, &url, HttpBody::Json(json!({ "type": "channel.follow" })));
    let answer = transport().send(&req, &NeverCancel).expect("answered");
    assert_eq!(answer.status, 200);
    assert_eq!(answer.body, b"{\"ok\":true}");
    assert!(answer.headers.contains(&("content-type".to_owned(), "application/json".to_owned())));
    let seen = seen.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(seen.head.starts_with("POST /x HTTP/1.1\r\n"), "{}", seen.head);
    let head = seen.head.to_ascii_lowercase();
    assert!(head.contains("authorization: bearer test-only\r\n"));
    assert!(head.contains("content-type: application/json\r\n"));
    assert_eq!(seen.body, br#"{"type":"channel.follow"}"#);
}

#[test]
fn a_non_2xx_status_is_an_answer_not_an_error() {
    let (url, _seen) = server(vec![Some(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 4\r\n\r\nnope")]);
    let answer = transport().send(&request(Method::Get, &url, HttpBody::Empty), &NeverCancel).expect("answered");
    assert_eq!((answer.status, answer.body.as_slice()), (401, b"nope".as_slice()));
}

#[test]
fn an_answer_past_max_response_is_too_large() {
    let (url, _seen) = server(vec![Some(OK)]);
    let mut req = request(Method::Get, &url, HttpBody::Empty);
    req.max_response = 4;
    assert_eq!(transport().send(&req, &NeverCancel), Err(HttpError::TooLarge { limit: 4 }));
}

#[test]
fn a_stream_delivers_up_to_the_limit_then_fails() {
    let (url, _seen) = server(vec![Some(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n0123456789")]);
    let mut req = request(Method::Get, &url, HttpBody::Empty);
    req.max_response = 6;
    let mut stream = transport().open(&req, Arc::new(NeverCancel)).expect("opened");
    assert_eq!(stream.status, 200);
    let mut got = Vec::new();
    let error = stream.body.read_to_end(&mut got).expect_err("the limit is passed");
    assert_eq!(got, b"012345");
    assert_eq!(HttpError::of_io(&error), Some(&HttpError::TooLarge { limit: 6 }));
}

#[test]
fn cancelling_a_stalled_stream_stops_it_within_50_ms() {
    let head: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\ndata: one\n\n";
    let (url, _seen) = server(vec![Some(head), None]);
    let flag = CancelFlag::new();
    let mut stream = transport().open(&request(Method::Get, &url, HttpBody::Empty), Arc::new(flag.clone())).unwrap();
    let mut first = [0; 11];
    stream.body.read_exact(&mut first).expect("the first event arrives");
    assert_eq!(&first, b"data: one\n\n");
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        flag.cancel();
        Instant::now()
    });
    let error = stream.body.read(&mut [0; 16]).expect_err("cancelled");
    let stopped = Instant::now();
    let flipped = canceller.join().unwrap();
    assert_eq!(HttpError::of_io(&error), Some(&HttpError::Cancelled));
    assert!(stopped.duration_since(flipped) < Duration::from_millis(50));
}

#[test]
fn a_silent_server_times_out() {
    let (url, _seen) = server(vec![None]);
    let mut req = request(Method::Get, &url, HttpBody::Empty);
    req.timeout_s = 1;
    let started = Instant::now();
    assert_eq!(transport().send(&req, &NeverCancel), Err(HttpError::Timeout));
    let took = started.elapsed();
    assert!(took >= Duration::from_secs(1) && took < Duration::from_millis(1400), "{took:?}");
}

#[test]
fn a_redirect_is_returned_not_followed() {
    let (elsewhere, other) = server(vec![Some(OK)]);
    let redirect: &'static [u8] = Box::leak(
        format!("HTTP/1.1 302 Found\r\nLocation: {elsewhere}\r\nContent-Length: 0\r\n\r\n").into_bytes().into_boxed_slice(),
    );
    let (url, _seen) = server(vec![Some(redirect)]);
    let answer = transport().send(&request(Method::Get, &url, HttpBody::Empty), &NeverCancel).expect("answered");
    assert_eq!(answer.status, 302);
    assert!(other.recv_timeout(Duration::from_millis(300)).is_err(), "the other host saw nothing");
}

#[test]
fn the_user_agent_says_omuzaki_and_nothing_else() {
    let (url, seen) = server(vec![Some(OK)]);
    transport().send(&request(Method::Get, &url, HttpBody::Empty), &NeverCancel).expect("answered");
    let head = seen.recv_timeout(Duration::from_secs(10)).unwrap().head.to_ascii_lowercase();
    let agent = head.lines().find_map(|l| l.strip_prefix("user-agent: ")).unwrap();
    assert_eq!(agent, concat!("omuzaki/", env!("CARGO_PKG_VERSION")));
}

#[test]
fn a_multipart_body_reaches_the_server_with_its_parts() {
    let (url, seen) = server(vec![Some(OK)]);
    let parts = vec![
        Part { name: "prompt".into(), file_name: None, content_type: None, data: b"a fox".to_vec() },
        Part { name: "image".into(), file_name: Some("in.png".into()), content_type: Some("image/png".into()), data: vec![0, 1, 2] },
    ];
    transport().send(&request(Method::Post, &url, HttpBody::Multipart(parts)), &NeverCancel).expect("answered");
    let seen = seen.recv_timeout(Duration::from_secs(10)).unwrap();
    let head = seen.head.to_ascii_lowercase();
    let boundary = head.lines().find_map(|l| l.split_once("boundary=").map(|(_, b)| b.trim().to_owned())).unwrap();
    let body = String::from_utf8_lossy(&seen.body).into_owned();
    let parts: Vec<&str> = body.split(&format!("--{boundary}")).filter(|p| !p.is_empty() && *p != "--\r\n").collect();
    assert_eq!(parts.len(), 2);
    assert!(parts[0].contains("name=\"prompt\"\r\n\r\na fox\r\n"));
    assert!(parts[1].contains("name=\"image\"; filename=\"in.png\"\r\nContent-Type: image/png\r\n\r\n\u{0}\u{1}\u{2}\r\n"));
}

#[test]
fn cancelling_a_silent_send_returns_within_250_ms() {
    let (url, _seen) = server(vec![None]);
    let flag = CancelFlag::new();
    let canceller = {
        let flag = flag.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            flag.cancel();
        })
    };
    let started = Instant::now();
    assert_eq!(transport().send(&request(Method::Get, &url, HttpBody::Empty), &flag), Err(HttpError::Cancelled));
    assert!(started.elapsed() < Duration::from_millis(350));
    canceller.join().unwrap();
}

#[test]
fn plain_http_is_refused_without_sending() {
    let req = request(Method::Get, "http://example.com/x", HttpBody::Empty);
    assert!(matches!(transport().send(&req, &NeverCancel), Err(HttpError::Refused(_))));
    for url in ["not a url", "http://192.0.2.1/", "https://user:pass@example.com/", "ftp://127.0.0.1/"] {
        let started = Instant::now();
        let req = request(Method::Get, url, HttpBody::Empty);
        assert!(matches!(transport().send(&req, &NeverCancel), Err(HttpError::Refused(_))), "{url}");
        assert!(started.elapsed() < Duration::from_millis(100));
    }
    let mut req = request(Method::Get, "https://example.com/", HttpBody::Empty);
    req.headers.push(("Host".to_owned(), "elsewhere".to_owned()));
    assert!(matches!(transport().send(&req, &NeverCancel), Err(HttpError::Refused(_))));
}
