//! The contract of `omuzaki-secrets-api`: label rules, object safety, and a `SecretError` that never shows a value.

use omuzaki_secrets_api::{SecretError, SecretLabel, SecretStore, SecretValue};

fn assert_send_sync<T: Send + Sync + ?Sized>() {}

#[test]
fn the_store_is_object_safe_and_thread_safe() {
    assert_send_sync::<dyn SecretStore>();
    assert_send_sync::<SecretLabel>();
    assert_send_sync::<SecretValue>();
}

#[test]
fn good_labels_are_accepted() {
    for ok in
        ["a", "automation/streamerbot", "connector/openai", "a.b/c_d-e", "x...y", &"a".repeat(SecretLabel::MAX_LEN)]
    {
        let label = SecretLabel::new(ok).unwrap();
        assert_eq!(label.as_str(), ok);
        assert_eq!(label.to_string(), ok);
    }
}

#[test]
fn bad_labels_are_refused_with_the_rule_they_break() {
    let too_long = "a".repeat(SecretLabel::MAX_LEN + 1);
    for bad in ["", &too_long, "Upper", "has space", "a//b", "/a", "a/", ".", "..", "a/../b", "a/./b", "ü", "a\nb"] {
        let err = SecretLabel::new(bad).unwrap_err();
        assert!(matches!(&err, SecretError::BadLabel { label, .. } if label == bad), "{bad:?} gave {err:?}");
    }
    assert!(SecretLabel::new("a".repeat(SecretLabel::MAX_LEN)).is_ok());
}

#[test]
fn labels_order_alphabetically() {
    let mut labels = ["b/x", "a/z", "a/y"].map(|l| SecretLabel::new(l).unwrap());
    labels.sort();
    let text: Vec<_> = labels.iter().map(SecretLabel::as_str).collect();
    assert_eq!(text, ["a/y", "a/z", "b/x"]);
}

#[test]
fn a_value_is_read_only_through_expose() {
    let value = SecretValue::new("hunter2");
    assert_eq!(value.expose(), "hunter2");
    assert!(value.check_storable().is_ok());
    assert!(matches!(SecretValue::new("").check_storable(), Err(SecretError::Rejected { .. })));
}

/// One of every variant with the text its `Display` must carry.
fn every_error() -> Vec<(SecretError, &'static str)> {
    vec![
        (SecretError::unavailable("start a Secret Service daemon"), "start a Secret Service daemon"),
        (SecretError::denied("locked"), "locked"),
        (SecretError::rejected("too long"), "too long"),
        (SecretError::bad_label("Bad", "use a-z"), "`Bad` is not a valid secret label: use a-z"),
        (SecretError::corrupt("automation/x"), "automation/x"),
        (SecretError::backend("dbus failed"), "dbus failed"),
    ]
}

#[test]
fn every_error_variant_says_what_happened_and_what_to_do() {
    for (err, carries) in every_error() {
        let text = err.to_string();
        assert!(text.contains(carries), "{text:?} should contain {carries:?}");
        assert_eq!(err.clone(), err);
    }
}
