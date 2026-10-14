//! The contract of `omuzaki-http-mock`: routing and logs, cassettes, size limits, streams and cancellation. Everything
//! here runs without a socket, a thread or a sleep.

use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use omuzaki_http_api::{
    Cancel, CancelFlag, HttpBody, HttpError, HttpRequest, HttpResponse, HttpTransport, Method, NeverCancel,
};
use omuzaki_http_mock::{CassetteError, Chunk, FakeHttp, Reply, StreamScript};
use serde_json::json;

const EXAMPLE: &str = include_str!("cassettes/example.json");

fn get(url: &str) -> HttpRequest {
    HttpRequest {
        method: Method::Get,
        url: url.to_owned(),
        headers: vec![],
        body: HttpBody::Empty,
        timeout_s: 5,
        max_response: 1_000,
    }
}

fn post_json(body: serde_json::Value) -> HttpRequest {
    HttpRequest {
        method: Method::Post,
        url: "https://api.example.com/v1/messages".to_owned(),
        headers: vec![],
        body: HttpBody::Json(body),
        timeout_s: 5,
        max_response: 10_000,
    }
}

fn response(status: u16, body: &[u8]) -> Reply {
    Reply::Response(HttpResponse { status, headers: vec![], body: body.to_vec() })
}

fn stream(chunks: Vec<Chunk>) -> Reply {
    Reply::Stream(StreamScript { status: 200, headers: vec![], chunks })
}

fn data(bytes: &[u8]) -> Chunk {
    Chunk::Data(bytes.to_vec())
}

/// Turns true on its `after`-th poll, so a test can cancel a call from inside its own waiting.
struct CancelOnPoll {
    polls: AtomicU32,
    after: u32,
}

impl Cancel for CancelOnPoll {
    fn is_cancelled(&self) -> bool {
        let seen = self.polls.fetch_add(1, Ordering::SeqCst) + 1;
        seen >= self.after
    }
}

/// Never cancelled, and counts its polls.
#[derive(Default)]
struct CountingNever(AtomicU32);

impl Cancel for CountingNever {
    fn is_cancelled(&self) -> bool {
        self.0.fetch_add(1, Ordering::SeqCst);
        false
    }
}

/// Reads a stream to its end with `buf_len`-byte buffers. Each entry is one read's bytes, or the error that ended it.
fn read_all(body: &mut dyn Read, buf_len: usize) -> Vec<std::io::Result<Vec<u8>>> {
    let mut out = Vec::new();
    loop {
        let mut buf = vec![0_u8; buf_len];
        match body.read(&mut buf) {
            Ok(0) => return out,
            Ok(n) => out.push(Ok(buf[..n].to_vec())),
            Err(e) => {
                out.push(Err(e));
                return out;
            }
        }
    }
}

#[test]
fn a_route_hands_out_its_replies_in_order_then_repeats_the_last() {
    let fake = FakeHttp::new().route(
        Method::Get,
        "https://api.example.com/v1/a",
        vec![response(200, b"one"), response(201, b"two"), response(500, b"three")],
    );
    let statuses: Vec<u16> = (0..5)
        .map(|_| fake.send(&get("https://api.example.com/v1/a"), &NeverCancel).expect("answered").status)
        .collect();
    assert_eq!(statuses, [200, 201, 500, 500, 500]);
    assert_eq!(fake.calls_to("/v1/a"), 5);
    assert_eq!(fake.unmatched().len(), 0);
}

#[test]
fn the_log_holds_the_exact_method_url_headers_and_body_bytes() {
    let fake = FakeHttp::new().json(Method::Post, "https://api.example.com/v1/items?limit=2", 200, json!({"ok": true}));
    let req = HttpRequest {
        method: Method::Post,
        url: "https://api.example.com/v1/items?limit=2".to_owned(),
        headers: vec![("authorization".to_owned(), "Bearer test-token".to_owned()), ("x-n".into(), "1".into())],
        body: HttpBody::Json(json!({"name": "a", "n": 2})),
        timeout_s: 5,
        max_response: 1_000,
    };
    fake.send(&req, &NeverCancel).expect("answered");
    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].method, Method::Post);
    assert_eq!(calls[0].url, "https://api.example.com/v1/items?limit=2");
    assert_eq!(calls[0].headers, req.headers);
    assert_eq!(calls[0].body, br#"{"n":2,"name":"a"}"#.to_vec());
}

#[test]
fn raw_bytes_bodies_are_logged_exactly() {
    let fake = FakeHttp::new().route(Method::Put, "https://api.example.com/v1/blob", vec![response(204, b"")]);
    let req = HttpRequest {
        method: Method::Put,
        url: "https://api.example.com/v1/blob".to_owned(),
        headers: vec![],
        body: HttpBody::Bytes { content_type: "application/octet-stream".to_owned(), data: vec![0, 255, 7] },
        timeout_s: 5,
        max_response: 10,
    };
    fake.send(&req, &NeverCancel).expect("answered");
    assert_eq!(fake.calls()[0].body, vec![0, 255, 7]);
}

#[test]
fn body_contains_tells_two_routes_on_one_url_apart() {
    let fake = FakeHttp::cassette(EXAMPLE).expect("the example cassette parses");
    let answer = fake.send(&post_json(json!({"stream": false})), &NeverCancel).expect("answered");
    assert_eq!(answer.status, 200);
    assert!(String::from_utf8_lossy(&answer.body).contains("msg_example_1"));
    // The stream route ends in a stall, so a whole-body send waits and gives up.
    assert_eq!(fake.send(&post_json(json!({"stream": true})), &NeverCancel), Err(HttpError::Timeout));
    assert_eq!(fake.calls_to("/v1/messages"), 2);
    assert_eq!(fake.unmatched().len(), 0);
}

#[test]
fn an_unmatched_request_is_logged_and_fails_with_network() {
    let fake = FakeHttp::new().json(Method::Get, "https://api.example.com/v1/a", 200, json!({}));
    let req = get("https://api.example.com/v1/other?x=1");
    let err = fake.send(&req, &NeverCancel).expect_err("no route");
    assert_eq!(err, HttpError::Network("no route for GET https://api.example.com/v1/other?x=1".to_owned()));
    let unmatched = fake.unmatched();
    assert_eq!(unmatched.len(), 1);
    assert_eq!(unmatched[0].url, "https://api.example.com/v1/other?x=1");
    assert_eq!(fake.calls().len(), 0);
}

#[test]
fn a_method_mismatch_is_unmatched() {
    let fake = FakeHttp::new().json(Method::Get, "https://api.example.com/v1/a", 200, json!({}));
    let req = HttpRequest { method: Method::Delete, ..get("https://api.example.com/v1/a") };
    assert!(matches!(fake.send(&req, &NeverCancel), Err(HttpError::Network(_))));
    assert_eq!(fake.unmatched().len(), 1);
}

#[test]
fn a_cancelled_handle_fails_first_and_logs_nothing() {
    let fake = FakeHttp::new().json(Method::Get, "https://api.example.com/v1/a", 200, json!({}));
    let flag = CancelFlag::new();
    flag.cancel();
    assert_eq!(fake.send(&get("https://api.example.com/v1/a"), &flag), Err(HttpError::Cancelled));
    assert_eq!(fake.send(&get("https://api.example.com/nope"), &flag), Err(HttpError::Cancelled));
    assert_eq!(fake.calls().len(), 0);
    assert_eq!(fake.unmatched().len(), 0);
}

#[test]
fn a_hang_gives_cancelled_once_the_handle_turns_and_timeout_after_1000_polls() {
    let fake = FakeHttp::new().route(Method::Get, "https://api.example.com/v1/slow", vec![Reply::Hang]);
    let cancel = CancelOnPoll { polls: AtomicU32::new(0), after: 5 };
    assert_eq!(fake.send(&get("https://api.example.com/v1/slow"), &cancel), Err(HttpError::Cancelled));
    assert_eq!(cancel.polls.load(Ordering::SeqCst), 5, "the hang returns on the poll that sees the cancel");

    let never = CountingNever::default();
    assert_eq!(fake.send(&get("https://api.example.com/v1/slow"), &never), Err(HttpError::Timeout));
    // One poll for the check before anything, then exactly 1,000 while it waits.
    assert_eq!(never.0.load(Ordering::SeqCst), 1_001);
    assert_eq!(fake.calls_to("/v1/slow"), 2, "a hang is still a matched call");
}

#[test]
fn a_scripted_error_comes_back_as_it_is() {
    let fake = FakeHttp::new().route(
        Method::Get,
        "https://api.example.com/v1/a",
        vec![Reply::Error(HttpError::Refused("nope".into()))],
    );
    assert_eq!(fake.send(&get("https://api.example.com/v1/a"), &NeverCancel), Err(HttpError::Refused("nope".into())));
}

#[test]
fn the_checked_in_cassette_parses_and_replays() {
    let fake = FakeHttp::cassette(EXAMPLE).expect("the example cassette parses");
    let readme = get("https://api.example.com/v1/files/readme.txt");
    let file = fake.send(&readme, &NeverCancel).expect("answered");
    assert_eq!((file.status, file.body.as_slice()), (200, b"example file".as_slice()));
    let missing = fake.send(&readme, &NeverCancel).expect("answered");
    assert_eq!(missing.status, 404, "the second reply, then the last one repeats");
    let blob = fake.send(&get("https://api.example.com/v1/files/blob.bin"), &NeverCancel).expect("answered");
    assert_eq!(blob.body, vec![0, 1, 2, 3]);
    assert_eq!(blob.headers, vec![("content-type".to_owned(), "application/octet-stream".to_owned())]);

    let live = FakeHttp::cassette(EXAMPLE).expect("parses");
    let mut body = live.open(&post_json(json!({"stream": true})), Arc::new(NeverCancel)).expect("opened").body;
    let mut buf = [0_u8; 256];
    let n = body.read(&mut buf).expect("first event");
    assert!(buf[..n].starts_with(b"event: start"));
    assert!(body.read(&mut buf).expect("second event") > 0);
    let stalled = body.read(&mut buf).expect_err("the stall ends the stream");
    assert_eq!(HttpError::of_io(&stalled), Some(&HttpError::Timeout));
}

#[test]
fn malformed_cassettes_name_the_route_they_are_wrong_in() {
    let good = r#"{"method":"GET","url":"https://api.example.com/a","replies":[{"status":200}]}"#;
    let cases = [
        (r#"{"method":"GET","replies":[{"status":200}]}"#, "missing \"url\""),
        (
            r#"{"method":"GET","url":"https://api.example.com/a","replies":[{"status":200,"body_text":"a","body_json":{}}]}"#,
            "one body",
        ),
        (
            r#"{"method":"GET","url":"https://api.example.com/a","replies":[{"status":200,"body_base64":"Zm9*"}]}"#,
            "not valid base64",
        ),
    ];
    for (bad, words) in cases {
        let json = format!(r#"{{"routes":[{good},{bad}]}}"#);
        match FakeHttp::cassette(&json) {
            Err(CassetteError::Route { index, problem }) => {
                assert_eq!(index, 1, "the second route is the bad one");
                assert!(problem.contains(words), "{problem:?} should mention {words:?}");
            }
            other => panic!("expected a route error, got {other:?}"),
        }
    }
}

#[test]
fn a_cassette_that_is_not_a_routes_document_is_a_json_error() {
    assert!(matches!(FakeHttp::cassette("{not json"), Err(CassetteError::Json(_))));
    assert!(matches!(FakeHttp::cassette(r#"{"routes":{}}"#), Err(CassetteError::Json(_))));
}

#[test]
fn a_response_past_max_response_is_too_large_from_send() {
    let fake = FakeHttp::new().route(Method::Get, "https://api.example.com/v1/a", vec![response(200, b"0123456789")]);
    let req = HttpRequest { max_response: 6, ..get("https://api.example.com/v1/a") };
    assert_eq!(fake.send(&req, &NeverCancel), Err(HttpError::TooLarge { limit: 6 }));
}

#[test]
fn open_gives_each_chunk_as_its_own_read_in_order() {
    let fake = FakeHttp::new().route(
        Method::Get,
        "https://api.example.com/v1/events",
        vec![stream(vec![data(b"data: a\n\n"), data(b"data: b\n\n")])],
    );
    let mut body = fake.open(&get("https://api.example.com/v1/events"), Arc::new(NeverCancel)).expect("opened").body;
    let frames: Vec<Vec<u8>> = read_all(&mut body, 64).into_iter().map(|r| r.expect("read")).collect();
    assert_eq!(frames, vec![b"data: a\n\n".to_vec(), b"data: b\n\n".to_vec()]);
}

#[test]
fn a_short_buffer_takes_several_reads_of_one_chunk() {
    let fake = FakeHttp::new().route(
        Method::Get,
        "https://api.example.com/v1/events",
        vec![stream(vec![data(b"0123456789")])],
    );
    let mut body = fake.open(&get("https://api.example.com/v1/events"), Arc::new(NeverCancel)).expect("opened").body;
    let frames: Vec<Vec<u8>> = read_all(&mut body, 4).into_iter().map(|r| r.expect("read")).collect();
    assert_eq!(frames, vec![b"0123".to_vec(), b"4567".to_vec(), b"89".to_vec()]);
}

#[test]
fn a_streamed_body_stops_at_max_response_with_too_large() {
    let body_bytes = b"abcdefghijklmnopqrst";
    let fake = FakeHttp::new().route(Method::Get, "https://api.example.com/v1/big", vec![response(200, body_bytes)]);
    let req = HttpRequest { max_response: 12, ..get("https://api.example.com/v1/big") };
    let mut body = fake.open(&req, Arc::new(NeverCancel)).expect("opened").body;
    let reads = read_all(&mut body, 4);
    let delivered: Vec<u8> = reads.iter().filter_map(|r| r.as_ref().ok()).flatten().copied().collect();
    assert_eq!(delivered, body_bytes[..12].to_vec(), "exactly the first 12 bytes arrive");
    let failure = reads.last().and_then(|r| r.as_ref().err()).expect("the next read fails");
    assert_eq!(HttpError::of_io(failure), Some(&HttpError::TooLarge { limit: 12 }));
}

#[test]
fn a_body_of_exactly_max_response_ends_cleanly() {
    let fake = FakeHttp::new().route(Method::Get, "https://api.example.com/v1/exact", vec![response(200, b"123456")]);
    let req = HttpRequest { max_response: 6, ..get("https://api.example.com/v1/exact") };
    let mut body = fake.open(&req, Arc::new(NeverCancel)).expect("opened").body;
    let reads = read_all(&mut body, 64);
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].as_ref().expect("read").as_slice(), b"123456");
}

#[test]
fn a_stall_waits_for_cancel_or_times_out_and_the_read_before_it_delivers() {
    let fake = FakeHttp::new().route(
        Method::Get,
        "https://api.example.com/v1/stall",
        vec![stream(vec![data(b"x"), Chunk::Stall])],
    );
    let flag = CancelFlag::new();
    let mut body = fake.open(&get("https://api.example.com/v1/stall"), Arc::new(flag.clone())).expect("opened").body;
    let mut buf = [0_u8; 8];
    assert_eq!(body.read(&mut buf).expect("first read"), 1);
    assert_eq!(buf[0], b'x');
    flag.cancel();
    let err = body.read(&mut buf).expect_err("cancelled");
    assert_eq!(HttpError::of_io(&err), Some(&HttpError::Cancelled));

    let mut waiting = fake.open(&get("https://api.example.com/v1/stall"), Arc::new(NeverCancel)).expect("opened").body;
    assert_eq!(waiting.read(&mut buf).expect("first read"), 1);
    let err = waiting.read(&mut buf).expect_err("times out");
    assert_eq!(HttpError::of_io(&err), Some(&HttpError::Timeout));
}

#[test]
fn open_with_a_cancelled_handle_is_cancelled_and_logs_nothing() {
    let fake = FakeHttp::new().route(Method::Get, "https://api.example.com/v1/a", vec![response(200, b"x")]);
    let flag = CancelFlag::new();
    flag.cancel();
    let result = fake.open(&get("https://api.example.com/v1/a"), Arc::new(flag));
    assert_eq!(result.map(|_| ()), Err(HttpError::Cancelled));
    assert_eq!(fake.calls().len(), 0);
    assert_eq!(fake.unmatched().len(), 0);
}

#[test]
fn a_stream_reply_keeps_its_status_and_headers() {
    let script = StreamScript {
        status: 206,
        headers: vec![("content-type".into(), "text/event-stream".into())],
        chunks: vec![data(b"data: a\n\n")],
    };
    let fake = FakeHttp::new().route(Method::Get, "https://api.example.com/v1/sse", vec![Reply::Stream(script)]);
    let opened = fake.open(&get("https://api.example.com/v1/sse"), Arc::new(NeverCancel)).expect("opened");
    assert_eq!(opened.status, 206);
    assert_eq!(opened.headers, vec![("content-type".to_owned(), "text/event-stream".to_owned())]);
}

#[test]
fn a_base64_chunk_decodes_to_its_bytes() {
    let json = r#"{"routes":[{"method":"GET","url":"https://api.example.com/v1/b","replies":[
        {"status":200,"chunks":[{"base64":"AAEC"},"stall"]}]}]}"#;
    let fake = FakeHttp::cassette(json).expect("parses");
    let mut body = fake.open(&get("https://api.example.com/v1/b"), Arc::new(NeverCancel)).expect("opened").body;
    let mut buf = [0_u8; 8];
    let n = body.read(&mut buf).expect("first read");
    assert_eq!(&buf[..n], &[0, 1, 2]);
}

#[test]
fn fake_http_serves_as_a_shared_trait_object() {
    let fake = FakeHttp::new().json(Method::Get, "https://api.example.com/v1/a", 200, json!({"n": 1}));
    let shared: Arc<dyn HttpTransport> = Arc::new(fake);
    let answer = shared.send(&get("https://api.example.com/v1/a"), &NeverCancel);
    assert_eq!(answer.expect("answered").status, 200);
}
