//! The QR payload: its URL shape, base64url, and the phone parser's
//! failures.

#![allow(
    clippy::assert_is_empty,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::large_futures,
    clippy::too_many_lines
)]

use chrono::{DateTime, Utc};
use steno_handover::engine::{HandoverRequest, HandoverResponse, Principal};
use steno_handover::route::Route;
use steno_handover::{PairingPayload, PairingPayloadError, base64url, wire};
use uuid::Uuid;

fn fingerprint() -> Vec<u8> {
    (0u8..32)
        .map(|index| index.wrapping_mul(7).wrapping_add(3))
        .collect()
}

fn secret() -> Vec<u8> {
    (0u8..32).map(|index| 255 - index * 5).collect()
}

fn mac_id() -> Uuid {
    Uuid::parse_str("6F9619FF-8B86-D011-B42D-00C04FC964FF").unwrap()
}

fn at(seconds: f64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis((seconds * 1000.0).round() as i64).unwrap()
}

#[test]
fn url_string_has_the_wire_shape_and_round_trips() {
    let payload = PairingPayload::new(
        mac_id(),
        "Nicolai's MacBook Pro + Office",
        fingerprint(),
        secret(),
        at(1_790_000_300.7),
    );
    let url = payload.url_string();

    assert!(url.starts_with("steno://pair/v1?mac=6f9619ff-8b86-d011-b42d-00c04fc964ff&name="));
    assert!(url.contains("&name=Nicolai%27s%20MacBook%20Pro%20%2B%20Office&fp="));
    assert!(url.ends_with("&exp=1790000300"));
    let query = url.split_once('?').unwrap().1;
    assert!(
        !query.contains('+') && !query.contains('/'),
        "base64url alphabet only"
    );
    for field in query.split('&') {
        if field.starts_with("fp=") || field.starts_with("secret=") {
            assert!(!field.ends_with('='), "unpadded");
        }
    }

    let parsed = PairingPayload::parse(&url).unwrap();
    assert_eq!(parsed, payload);
    assert_eq!(parsed.expires_at, at(1_790_000_300.0));
    assert_eq!(parsed.mac_name, "Nicolai's MacBook Pro + Office");
}

#[test]
fn base64url_round_trips_and_rejects_foreign_characters() {
    for length in [0usize, 1, 2, 3, 31, 32, 33] {
        let data: Vec<u8> = (0..length)
            .map(|index| u8::try_from((index * 37 + 11) % 256).unwrap())
            .collect();
        let encoded = base64url::encode(&data);
        assert!(!encoded.contains('='));
        assert_eq!(base64url::decode(&encoded), Some(data));
    }
    assert_eq!(base64url::decode("AQID"), Some(vec![1, 2, 3]));
    assert_eq!(base64url::decode("AQID=="), Some(vec![1, 2, 3]));
    assert_eq!(
        base64url::decode("AQ+D"),
        None,
        "standard alphabet is not accepted"
    );
    assert_eq!(base64url::decode("A"), None);
    assert!(base64url::decode("-_-_").is_some());
}

#[test]
fn parse_failures_match_the_phone_parser() {
    let failure = |text: &str| PairingPayload::parse(text).err();
    let good = PairingPayload::new(
        mac_id(),
        "Mac",
        fingerprint(),
        secret(),
        at(1_790_000_300.0),
    )
    .url_string();
    assert_eq!(failure(&good), None);
    assert_eq!(
        failure("https://example.com/pair/v1?mac=1"),
        Some(PairingPayloadError::NotSteno)
    );
    assert_eq!(
        failure(&good.replace("pair/v1", "pair/v2")),
        Some(PairingPayloadError::Version)
    );
    assert_eq!(
        failure(&good.replace("&exp=1790000300", "")),
        Some(PairingPayloadError::MissingField)
    );
    assert_eq!(
        failure(&good.replace("&name=Mac", "&name=")),
        Some(PairingPayloadError::BadEncoding("name".into()))
    );
    assert_eq!(
        failure(&good.replace("&exp=1790000300", "&exp=abc")),
        Some(PairingPayloadError::BadEncoding("exp".into()))
    );
    assert_eq!(
        failure(&good.replace("mac=6f9619ff-8b86-d011-b42d-00c04fc964ff", "mac=nope")),
        Some(PairingPayloadError::BadEncoding("mac".into()))
    );
    let short = good.replace(
        &format!("fp={}", base64url::encode(&fingerprint())),
        &format!("fp={}", base64url::encode(&fingerprint()[..31])),
    );
    assert_eq!(
        failure(&short),
        Some(PairingPayloadError::BadEncoding("fp".into()))
    );
    assert_eq!(
        failure(&format!("{good}&exp=1")),
        Some(PairingPayloadError::BadEncoding("duplicate exp".into()))
    );
}

#[test]
fn debug_output_shows_no_secret_and_no_credential() {
    // `{:?}` is what ends up in a log line or a panic message.
    let payload = PairingPayload::new(mac_id(), "Mac", fingerprint(), secret(), at(1.0));
    let shown = format!("{payload:?}");
    assert!(shown.contains("[redacted]"), "{shown}");
    assert!(!shown.contains(&base64url::encode(&secret())), "{shown}");
    assert!(!shown.contains(&format!("{:?}", secret())), "{shown}");
    assert!(shown.contains(&base64url::encode(&fingerprint())));

    let token = "dGhlIGJlYXJlciB0b2tlbg==";
    let request = HandoverRequest::new(Route::Pair, Principal::Pairing(1))
        .with_header("authorization", &format!("Bearer {token}"))
        .with_body(format!(
            r#"{{"deviceId":"{}","deviceName":"Phone"}}"#,
            mac_id()
        ));
    let shown = format!("{request:?}");
    assert!(!shown.contains(token), "{shown}");
    assert!(!shown.contains("deviceName"), "{shown}");
    assert!(
        shown.contains("authorization") && shown.contains("body_len"),
        "{shown}"
    );

    let paired = wire::PairResponse {
        token: token.to_owned(),
        mac_id: mac_id(),
        mac_name: "Mac".to_owned(),
    };
    let shown = format!("{paired:?}");
    assert!(!shown.contains(token), "{shown}");
    assert!(
        shown.contains("[redacted]") && shown.contains("Mac"),
        "{shown}"
    );
    let response = HandoverResponse::json(http::StatusCode::OK, &paired);
    let shown = format!("{response:?}");
    assert!(!shown.contains(token), "{shown}");
    assert!(shown.contains("body_len"), "{shown}");
}
