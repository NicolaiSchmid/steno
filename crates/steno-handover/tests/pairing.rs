//! Pairing: the QR payload, a secret that pairs once inside its window,
//! bearer tokens, revocation, and credentials kept out of debug output.

#![allow(clippy::large_futures)]

mod common;

use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use chrono::DateTime;
use common::{Phone, TestService, execute_batch, metadata_for, seeded_bytes};
use steno_handover::engine::{HandoverRequest, HandoverResponse, Principal};
use steno_handover::pairing::DeviceTokens;
use steno_handover::route::Route;
use steno_handover::{PairingPayload, PairingPayloadError, base64url, wire};
use uuid::Uuid;

fn mac_id() -> Uuid {
    Uuid::parse_str("6F9619FF-8B86-D011-B42D-00C04FC964FF").unwrap()
}

fn payload(name: &str) -> PairingPayload {
    PairingPayload::new(
        mac_id(),
        name,
        (0u8..32).map(|index| index.wrapping_mul(7)).collect(),
        (0u8..32).map(|index| 255 - index * 5).collect(),
        DateTime::from_timestamp_millis(1_790_000_300_700).unwrap(),
    )
}

#[test]
fn the_qr_url_has_the_wire_shape_and_round_trips() {
    let payload = payload("Nicolai's MacBook Pro + Office");
    let url = payload.url_string();
    assert!(url.starts_with("steno://pair/v1?mac=6f9619ff-8b86-d011-b42d-00c04fc964ff&name="));
    assert!(url.contains("&name=Nicolai%27s%20MacBook%20Pro%20%2B%20Office&fp="));
    assert!(url.ends_with("&exp=1790000300"), "whole seconds");
    let query = url.split_once('?').unwrap().1;
    assert!(!query.contains(['+', '/']), "base64url alphabet only");
    assert!(!query.contains("=&"), "unpadded");
    assert_eq!(PairingPayload::parse(&url).unwrap(), payload);

    assert_eq!(base64url::decode("AQID=="), Some(vec![1, 2, 3]));
    assert_eq!(base64url::decode("AQ+D"), None, "not the standard alphabet");
}

#[test]
fn a_malformed_qr_url_fails_as_the_phone_parser_does() {
    let good = payload("Mac").url_string();
    let fp = format!("fp={}", base64url::encode(&payload("Mac").fingerprint));
    let short_fp = format!(
        "fp={}",
        base64url::encode(&payload("Mac").fingerprint[..31])
    );
    let bad = |field: &str| Some(PairingPayloadError::BadEncoding(field.into()));
    let cases = [
        (
            "https://example.com/pair/v1?mac=1".to_owned(),
            Some(PairingPayloadError::NotSteno),
        ),
        (
            good.replace("pair/v1", "pair/v2"),
            Some(PairingPayloadError::Version),
        ),
        (
            good.replace("&exp=1790000300", ""),
            Some(PairingPayloadError::MissingField),
        ),
        (good.replace("&name=Mac", "&name="), bad("name")),
        (good.replace("&exp=1790000300", "&exp=abc"), bad("exp")),
        (
            good.replace("mac=6f9619ff-8b86-d011-b42d-00c04fc964ff", "mac=nope"),
            bad("mac"),
        ),
        (good.replace(&fp, &short_fp), bad("fp")),
        (format!("{good}&exp=1"), bad("duplicate exp")),
    ];
    for (url, expected) in cases {
        assert_eq!(PairingPayload::parse(&url).err(), expected, "{url}");
    }
}

#[test]
fn debug_output_shows_no_secret_and_no_token() {
    let payload = payload("Mac");
    let shown = format!("{payload:?}");
    assert!(
        !shown.contains(&base64url::encode(&payload.secret)),
        "{shown}"
    );
    assert!(!shown.contains(&format!("{:?}", payload.secret)), "{shown}");

    let token = "dGhlIGJlYXJlciB0b2tlbg==";
    let request = HandoverRequest::new(Route::Pair, Principal::Pairing(1))
        .with_header("authorization", &format!("Bearer {token}"))
        .with_body(r#"{"deviceName":"Phone"}"#);
    let paired = wire::PairResponse {
        token: token.to_owned(),
        mac_id: mac_id(),
        mac_name: "Mac".to_owned(),
    };
    let response = HandoverResponse::json(http::StatusCode::OK, &paired);
    for shown in [
        format!("{request:?}"),
        format!("{paired:?}"),
        format!("{response:?}"),
    ] {
        assert!(
            !shown.contains(token) && !shown.contains("deviceName"),
            "{shown}"
        );
    }
}

#[tokio::test]
async fn a_secret_pairs_once_within_its_window_and_the_token_is_stored_hashed() {
    let test = TestService::start().await;
    let payload = test.service.begin_pairing();
    assert_eq!(payload.mac_id, test.service.identity.mac_id());
    assert_eq!(payload.fingerprint, test.fingerprint());
    let device_id = Uuid::new_v4();

    let first = Phone::try_pair(
        &test,
        &payload.secret,
        device_id,
        " \u{200B}Nicolai's iPhone\n",
    )
    .await;
    assert_eq!(first.status, 200);
    let paired: wire::PairResponse = first.json();
    assert_eq!(paired.mac_id, test.service.identity.mac_id());
    assert_eq!(paired.mac_name, "Test Mac");
    assert_eq!(STANDARD.decode(&paired.token).unwrap().len(), 32);
    let devices = test.service.paired_devices().await.unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].name, "Nicolai's iPhone", "trimmed");
    let stored = test
        .store
        .paired_device_for_token_hash(&DeviceTokens::hash(&paired.token))
        .unwrap();
    assert_eq!(
        stored.map(|device| device.id),
        Some(device_id),
        "only the hash is kept"
    );

    let again = Phone::try_pair(&test, &payload.secret, Uuid::new_v4(), "Other").await;
    assert_eq!(again.status, 403, "single use");
    assert_eq!(test.service.paired_devices().await.unwrap().len(), 1);
    test.stop().await;
}

#[tokio::test]
async fn a_wrong_expired_cancelled_or_replaced_secret_is_403() {
    let test = TestService::start().await;
    let try_pair = |secret: Vec<u8>| {
        let test = &test;
        async move {
            Phone::try_pair(test, &secret, Uuid::new_v4(), "iPhone")
                .await
                .status
        }
    };
    assert_eq!(try_pair(vec![7; 32]).await, 403, "no window open");

    let open = test.service.begin_pairing();
    assert_eq!(try_pair(vec![7; 32]).await, 403, "wrong secret");
    assert!(
        test.service.engine.pairing_is_open(),
        "a wrong secret burns nothing"
    );

    let replaced = test.service.begin_pairing();
    assert_eq!(
        try_pair(open.secret).await,
        403,
        "replaced by a newer window"
    );

    test.service.cancel_pairing();
    assert_eq!(try_pair(replaced.secret).await, 403, "cancelled");

    let expiring = test.service.begin_pairing();
    test.clock.advance(Duration::from_secs(299));
    assert!(test.service.engine.pairing_is_open());
    test.clock.advance(Duration::from_secs(2));
    assert_eq!(try_pair(expiring.secret).await, 403, "expired after 300 s");
    assert_eq!(test.service.paired_devices().await.unwrap(), []);
    test.stop().await;
}

#[tokio::test]
async fn a_malformed_pair_request_is_400_and_keeps_the_window_open() {
    let test = TestService::start().await;
    let payload = test.service.begin_pairing();
    let authorization = common::pairing(&payload.secret);
    for body in [&b"{"[..], br#"{"deviceID":"x","deviceName":"a"}"#] {
        let response = test
            .client()
            .send(
                "POST",
                "/v1/pair",
                &[("Authorization", &authorization)],
                Some(body.to_vec()),
            )
            .await;
        assert_eq!(response.status, 400);
    }
    let blank = Phone::try_pair(&test, &payload.secret, Uuid::new_v4(), " \n").await;
    assert_eq!(blank.status, 400, "a blank device name");
    let paired = Phone::try_pair(&test, &payload.secret, Uuid::new_v4(), "iPhone").await;
    assert_eq!(paired.status, 200, "the window stayed open");
    test.stop().await;
}

#[tokio::test]
async fn a_revoked_phone_loses_its_upload_and_every_bearer_route_is_401() {
    let test = TestService::with_chunk_size(64 * 1024).await;
    let revoked = Phone::pair(&test).await;
    let other = Phone::pair(&test).await;
    let bytes = seeded_bytes(128 * 1024, 1);
    let ours = metadata_for(&bytes, 64 * 1024);
    let theirs = metadata_for(&bytes, 64 * 1024);
    for (phone, metadata) in [(&revoked, &ours), (&other, &theirs)] {
        assert_eq!(phone.announce(metadata).await.status, 201);
        assert_eq!(
            phone
                .upload(metadata.recording_id, 0, &bytes[..64 * 1024])
                .await
                .status,
            204
        );
    }

    test.service.revoke(revoked.device_id).await.unwrap();
    assert!(
        !test.inbox().has_partial(ours.recording_id),
        "the partial is gone"
    );
    assert_eq!(
        test.store.handover_receipt(ours.recording_id).unwrap(),
        None
    );
    assert!(
        test.service
            .receipts()
            .borrow()
            .iter()
            .all(|receipt| receipt.device_id == other.device_id)
    );
    let id = ours.recording_id;
    let statuses = [
        revoked.announce(&ours).await.status,
        revoked.status(id).await.status,
        revoked.upload(id, 1, &bytes[64 * 1024..]).await.status,
        revoked.complete(id).await.status,
        revoked.unpair().await.status,
    ];
    assert_eq!(statuses, [401; 5]);

    assert!(
        test.inbox().has_partial(theirs.recording_id),
        "another phone's upload stays"
    );
    assert_eq!(
        other.status_of(theirs.recording_id).await.received_chunks,
        [0]
    );

    // The phone unpairs itself, then pairs again under the same id.
    assert_eq!(other.unpair().await.status, 204);
    assert_eq!(other.status(theirs.recording_id).await.status, 401);
    assert_eq!(test.service.paired_devices().await.unwrap(), []);
    let again = Phone::pair_as(&test, revoked.device_id).await;
    assert_eq!(
        again.status(id).await.status,
        404,
        "paired again, with nothing of before"
    );
    test.stop().await;
}

/// The phone takes a 401 for a revoke and unpairs, so a store that cannot
/// read the device answers 500.
#[tokio::test]
async fn a_failed_device_read_is_500_not_401() {
    let test = TestService::start().await;
    let phone = Phone::pair(&test).await;
    execute_batch(
        &test.store,
        "CREATE TEMP TABLE pairedDevice (unreadable INTEGER)",
    );
    assert_eq!(phone.status(Uuid::new_v4()).await.status, 500);
    execute_batch(&test.store, "DROP TABLE temp.pairedDevice");
    assert_eq!(phone.status(Uuid::new_v4()).await.status, 404);
    test.stop().await;
}
