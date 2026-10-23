//! The process-wide cap on requests in flight, in a test binary of its own so no other test shares the count.

#![allow(clippy::unwrap_used)] // helpers outside `#[test]` fns: a panic is a clear test failure

use std::io::Read;
use std::net::TcpListener;
use std::sync::Arc;
use std::time::{Duration, Instant};

use omuzaki_http::{ConnectHttp, MAX_IN_FLIGHT, in_flight};
use omuzaki_http_api::{CancelFlag, HttpBody, HttpError, HttpRequest, HttpTransport, Method};

#[test]
fn one_request_past_the_cap_is_refused_and_all_return_once_cancelled() {
    // A server that accepts and never answers.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/hang", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let mut held = Vec::new();
        while let Ok((mut stream, _)) = listener.accept() {
            let _ = stream.read(&mut [0; 1024]);
            held.push(stream);
        }
    });
    let transport = Arc::new(ConnectHttp::new());
    let flag = CancelFlag::new();
    let req = HttpRequest {
        method: Method::Get,
        url,
        headers: Vec::new(),
        body: HttpBody::Empty,
        timeout_s: 30,
        max_response: 1024,
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let callers: Vec<_> = (0..MAX_IN_FLIGHT)
        .map(|_| {
            let (transport, flag, req, tx) = (Arc::clone(&transport), flag.clone(), req.clone(), tx.clone());
            std::thread::spawn(move || {
                let _ = tx.send(());
                transport.send(&req, &flag)
            })
        })
        .collect();
    for _ in 0..MAX_IN_FLIGHT {
        rx.recv_timeout(Duration::from_secs(10)).unwrap();
    }
    // Every caller has started; wait for their requests to hold every slot. A deadline, not a count of tries: on a
    // loaded runner the callers can take far longer than a few yields to reach their slots.
    let deadline = Instant::now() + Duration::from_secs(10);
    while in_flight() < MAX_IN_FLIGHT && Instant::now() < deadline {
        std::thread::yield_now();
    }
    let ninth = transport.send(&req, &CancelFlag::new());
    if !matches!(&ninth, Err(HttpError::Refused(m)) if m.contains("in flight")) {
        // Say why the slots never filled: a caller whose request ended early gave its slot back.
        flag.cancel();
        let ended: Vec<String> = callers.into_iter().map(|caller| format!("{:?}", caller.join())).collect();
        panic!("the ninth request got {ninth:?} with {} in flight; the callers ended with {ended:?}", in_flight());
    }
    // A cancelled request is refused as cancelled, without taking a slot.
    let probe = CancelFlag::new();
    probe.cancel();
    assert_eq!(transport.send(&req, &probe), Err(HttpError::Cancelled));
    flag.cancel();
    for caller in callers {
        assert_eq!(caller.join().unwrap(), Err(HttpError::Cancelled));
    }
}
