//! The authentication answer against the protocol document's example inputs, and that no secret leaks into `Debug`.

use omuzaki_obs::auth::{auth_response, secret};
use omuzaki_obs::protocol::{Frame, Identify, Message, ProtocolError};
use serde_json::json;

const PASSWORD: &str = "supersecretpassword";
const SALT: &str = "lM1GncleQOaCu9lT1yeUZhFYnqhsLLP1G5lAGo3ixaI=";
const CHALLENGE: &str = "+IxH4CnCiqpX1rM9scsNynZzbOe4KhDeYcTNS3PDaeY=";
// Recomputed independently with Python's `hashlib` and `base64`, and with `openssl dgst -sha256 -binary | openssl base64`.
const SECRET: &str = "H1IfVz1pSREUQzbFTVnX/Tyb+gMhMik5x7yUBCY0PTs=";
const ANSWER: &str = "1Ct943GAT+6YQUUX47Ia/ncufilbe6+oD6lY+5kaCu4=";

#[test]
fn matches_the_worked_example() {
    assert_eq!(secret(PASSWORD, SALT), SECRET);
    assert_eq!(auth_response(PASSWORD, SALT, CHALLENGE), ANSWER);
}

#[test]
fn a_different_password_gives_a_different_answer() {
    assert_ne!(auth_response("other", SALT, CHALLENGE), ANSWER);
}

#[test]
fn debug_never_shows_the_authentication_string() {
    let identify = Identify::new().with_authentication(ANSWER.to_owned());
    let message = Message::Identify(identify.clone());
    let frame = message.to_frame().expect("serialises");
    for text in [format!("{identify:?}"), format!("{message:?}"), format!("{frame:?}"), format!("{identify:#?}")] {
        assert!(!text.contains(ANSWER), "{text}");
        assert!(!text.contains(&ANSWER[..8]), "{text}");
    }
    // The wire form still carries it.
    assert_eq!(frame.d["authentication"], ANSWER);
}

#[test]
fn a_malformed_identify_error_does_not_carry_the_data() {
    let frame = Frame { op: 1, d: json!({ "rpcVersion": 1, "authentication": 12_345_678 }) };
    let err = Message::from_frame(&frame).expect_err("authentication must be a string");
    assert_eq!(err, ProtocolError::BadData { op: 1 });
    assert!(!format!("{err} {err:?}").contains("12345678"));
}
