//! The contract of `omuzaki-http-api`: object safety, thread safety, `Display` text, the cancel handles and the
//! `io::Error` carriage.

use std::io::{self, Read};
use std::sync::Arc;

use omuzaki_http_api::{
    Cancel, CancelFlag, HttpBody, HttpError, HttpRequest, HttpResponse, HttpStream, HttpTransport, Method, NeverCancel,
};

/// Answers 200 with the URL as the body; fails with `Cancelled` when the handle is already cancelled.
struct Stub;

impl HttpTransport for Stub {
    fn send(&self, req: &HttpRequest, cancel: &dyn Cancel) -> Result<HttpResponse, HttpError> {
        if cancel.is_cancelled() {
            return Err(HttpError::Cancelled);
        }
        Ok(HttpResponse { status: 200, headers: vec![], body: req.url.clone().into_bytes() })
    }

    fn open(&self, req: &HttpRequest, cancel: Arc<dyn Cancel>) -> Result<HttpStream, HttpError> {
        if cancel.is_cancelled() {
            return Err(HttpError::Cancelled);
        }
        Ok(HttpStream { status: 200, headers: vec![], body: Box::new(io::Cursor::new(req.url.clone().into_bytes())) })
    }
}

fn request() -> HttpRequest {
    HttpRequest {
        method: Method::Get,
        url: "http://x".into(),
        headers: vec![],
        body: HttpBody::Empty,
        timeout_s: 5,
        max_response: 100,
    }
}

fn assert_send_sync<T: Send + Sync + ?Sized>() {}

#[test]
fn stub_is_usable_as_arc_and_ref_dyn() {
    let arc: Arc<dyn HttpTransport> = Arc::new(Stub);
    let by_ref: &dyn HttpTransport = &*arc;
    assert_eq!(by_ref.send(&request(), &NeverCancel).unwrap().body, b"http://x");
    let mut stream = arc.open(&request(), Arc::new(NeverCancel)).unwrap();
    let mut text = String::new();
    stream.body.read_to_string(&mut text).unwrap();
    assert_eq!(text, "http://x");
    let flag = CancelFlag::new();
    flag.cancel();
    assert_eq!(arc.send(&request(), &flag), Err(HttpError::Cancelled));
    assert_eq!(arc.open(&request(), Arc::new(flag)).unwrap_err(), HttpError::Cancelled);
}

#[test]
fn contract_types_are_send_sync() {
    assert_send_sync::<dyn Cancel>();
    assert_send_sync::<dyn HttpTransport>();
    assert_send_sync::<HttpError>();
}

#[test]
fn error_display_is_exact() {
    assert_eq!(HttpError::Cancelled.to_string(), "the request was cancelled");
    assert_eq!(HttpError::Timeout.to_string(), "the server did not answer in time; try again, or raise the timeout");
    assert_eq!(
        HttpError::Network("dns".into()).to_string(),
        "the network call failed (dns); check the address and your connection"
    );
    assert_eq!(
        HttpError::TooLarge { limit: 9 }.to_string(),
        "the answer is larger than 9 bytes; ask for less or raise the limit"
    );
    assert_eq!(HttpError::Refused("ftp".into()).to_string(), "refused: ftp");
}

#[test]
fn cancel_flag_and_never_cancel() {
    let flag = CancelFlag::new();
    let clone = flag.clone();
    assert!(!flag.is_cancelled());
    flag.cancel();
    assert!(flag.is_cancelled());
    assert!(clone.is_cancelled());
    assert!(!NeverCancel.is_cancelled());
    let handles: [Arc<dyn Cancel>; 2] = [Arc::new(clone), Arc::new(NeverCancel)];
    assert!(handles[0].is_cancelled());
    assert!(!handles[1].is_cancelled());
}

#[test]
fn http_error_rides_an_io_error() {
    let e = io::Error::from(HttpError::TooLarge { limit: 7 });
    assert_eq!(e.kind(), io::ErrorKind::Other);
    assert_eq!(HttpError::of_io(&e), Some(&HttpError::TooLarge { limit: 7 }));
    assert_eq!(HttpError::of_io(&io::Error::other("nope")), None);
    assert_eq!(HttpError::of_io(&io::Error::from(io::ErrorKind::UnexpectedEof)), None);
}
