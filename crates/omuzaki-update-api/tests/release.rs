//! `Url` is an https URL with a host, checked on construction and on deserialization: a `Release` that offers `http:`,
//! `file:` or a non-URL does not parse.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::unwrap_used)]

use omuzaki_update_api::release::{Release, Url, UrlError};

const ACCEPTED: [&str; 4] = [
    "https://example.com/notes",
    "HTTPS://example.com/notes",
    "https://example.com:8443/a?b=c#d",
    "https://192.0.2.1/",
];

const REJECTED: [&str; 9] = [
    "http://example.com/notes",
    "file:///etc/notes",
    "ftp://example.com/",
    "not a url",
    "https://",
    "https://exa mple.com/",
    "https://user@example.com/",
    "https://example.com:port/",
    "https://example.com/caf\u{e9}",
];

#[test]
fn https_urls_with_a_host_are_accepted_and_kept_as_written() {
    for text in ACCEPTED {
        let url = Url::new(text).unwrap();
        assert_eq!(url.as_str(), text);
    }
}

#[test]
fn anything_else_is_rejected_on_construction() {
    for text in REJECTED {
        assert!(Url::new(text).is_err(), "accepted {text:?}");
    }
}

#[test]
fn the_error_says_whether_the_scheme_or_the_rest_was_wrong() {
    assert_eq!(Url::new("http://example.com/").unwrap_err(), UrlError::NotHttps);
    assert_eq!(Url::new("file:///etc/notes").unwrap_err(), UrlError::NotHttps);
    assert_eq!(Url::new("not a url").unwrap_err(), UrlError::NotHttps);
    assert_eq!(Url::new("https://").unwrap_err(), UrlError::Malformed);
    assert_eq!(Url::new("https://user@example.com/").unwrap_err(), UrlError::Malformed);
}

fn release_json(notes_url: &str) -> String {
    format!(
        r#"{{"version":"1.2.3","channel":"stable","sequence":7,"package":{{"type":"full"}},
            "download_bytes":10,"installed_bytes":20,"notes_url":{notes_url},"recommended":false}}"#
    )
}

#[test]
fn a_release_with_an_https_notes_page_parses_and_round_trips() {
    let release: Release = serde_json::from_str(&release_json(r#""https://example.com/notes""#)).unwrap();
    assert_eq!(release.notes_url.as_ref().map(Url::as_str), Some("https://example.com/notes"));
    let back = serde_json::to_string(&release).unwrap();
    let again: Release = serde_json::from_str(&back).unwrap();
    assert_eq!(again, release);
}

#[test]
fn a_release_offering_a_rejected_notes_url_does_not_parse() {
    for text in REJECTED {
        let json = release_json(&serde_json::to_string(text).unwrap());
        assert!(serde_json::from_str::<Release>(&json).is_err(), "parsed notes_url {text:?}");
    }
}

#[test]
fn a_release_without_notes_still_parses() {
    let json = release_json("null");
    let release: Release = serde_json::from_str(&json).unwrap();
    assert!(release.notes_url.is_none());
}

#[test]
fn numeric_hosts_must_be_valid_dotted_quads() {
    for text in [
        "https://256.0.0.1/",
        "https://9999999999999/",
        "https://1.2.3/",
        "https://1.2.3.4.5/",
        "https://01.2.3.4/",
        "https://127.0.0.256:443/",
    ] {
        assert_eq!(Url::new(text).unwrap_err(), UrlError::Malformed, "accepted {text:?}");
    }
    // Loopback and private addresses are valid syntax: the contract has no network policy.
    for text in ["https://127.0.0.1/", "https://10.0.0.1:8443/notes", "https://255.255.255.255/"] {
        assert!(Url::new(text).is_ok(), "rejected {text:?}");
    }
}

#[test]
fn ports_are_one_to_65535() {
    for text in ["https://example.com:0/", "https://example.com:65536/", "https://example.com:99999999999/"] {
        assert!(Url::new(text).is_err(), "accepted {text:?}");
    }
    for text in ["https://example.com:1/", "https://example.com:65535/"] {
        assert!(Url::new(text).is_ok(), "rejected {text:?}");
    }
}

#[test]
fn the_check_is_syntax_only_and_does_not_model_browser_host_forms() {
    // Documented limits: these hold, so a caller must not read `Url` as "a browser will open this".
    for text in ["https://0x7f.1/", "https://no-such-host.invalid/", "HTTPS://EXAMPLE.COM/"] {
        assert!(Url::new(text).is_ok(), "rejected {text:?}");
    }
    // IDN must be given in its ASCII form.
    assert!(Url::new("https://b\u{fc}cher.example/").is_err());
    assert!(Url::new("https://xn--bcher-kva.example/").is_ok());
}
