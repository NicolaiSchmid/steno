//! Pairing on the injected wall clock: the first use of a secret pairs,
//! the second is 403, an expired window is 403, a revoked token is 401,
//! the name is stored trimmed, a failed save reopens the window but not
//! one cancelled or replaced while it ran, and a head authorised against a
//! closed window does not pair against the next.

#![allow(
    clippy::assert_is_empty,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::large_futures,
    clippy::too_many_lines
)]

mod common;

use std::sync::mpsc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use common::{Phone, TestService, bearer};
use steno_handover::engine::{AuthOutcome, Engine, Principal, RequestHandling};
use steno_handover::route::Route;
use steno_handover::{base64url, wire};
use uuid::Uuid;

fn device_id() -> Uuid {
    Uuid::parse_str("0BADF00D-0000-4000-8000-000000000001").unwrap()
}

#[tokio::test]
async fn first_use_pairs_and_the_second_is_forbidden() {
    let test = TestService::start().await;
    let payload = test.service.begin_pairing();
    assert_eq!(payload.mac_id, test.service.identity.mac_id());
    assert_eq!(payload.fingerprint, test.fingerprint());
    assert_eq!(payload.mac_name, "Test Mac");
    assert_eq!(
        payload.expires_at,
        test.now + chrono::Duration::seconds(300)
    );
    assert!(payload.url_string().starts_with("steno://pair/v1?mac="));

    let first = Phone::try_pair(&test, &payload.secret, device_id(), "Nicolai's iPhone").await;
    assert_eq!(first.status, 200);
    let response: wire::PairResponse = first.json();
    assert_eq!(response.mac_id, test.service.identity.mac_id());
    assert_eq!(response.mac_name, "Test Mac");
    assert_eq!(STANDARD.decode(&response.token).unwrap().len(), 32);

    let devices = test.service.paired_devices().await.unwrap();
    assert_eq!(
        devices.iter().map(|device| device.id).collect::<Vec<_>>(),
        vec![device_id()]
    );
    assert_eq!(devices[0].name, "Nicolai's iPhone");
    assert_eq!(devices[0].paired_at, test.now);

    let second = Phone::try_pair(&test, &payload.secret, device_id(), "Test iPhone").await;
    assert_eq!(second.status, 403);
    assert_eq!(
        test.metrics().handled_requests,
        1,
        "the second attempt fails at the gate"
    );

    // The token works for a bearer route, and the phone can unpair itself.
    let client = test.client();
    let unpair = client
        .request(
            "DELETE",
            "/v1/pairing",
            &[("Authorization", &bearer(&response.token))],
            None,
        )
        .await
        .unwrap();
    assert_eq!(unpair.status, 204);
    assert!(test.service.paired_devices().await.unwrap().is_empty());
    let after = client
        .request(
            "DELETE",
            "/v1/pairing",
            &[("Authorization", &bearer(&response.token))],
            None,
        )
        .await
        .unwrap();
    assert_eq!(after.status, 401, "the computer already forgot the phone");
    test.stop().await;
}

#[tokio::test]
async fn the_window_closes_after_300_seconds_on_the_injected_clock() {
    let test = TestService::start().await;
    let payload = test.service.begin_pairing();

    test.advance(Duration::from_secs(299));
    assert!(test.service.engine.pairing_is_open());
    test.advance(Duration::from_secs(2));
    assert!(!test.service.engine.pairing_is_open());

    let late = Phone::try_pair(&test, &payload.secret, device_id(), "Test iPhone").await;
    assert_eq!(late.status, 403);
    assert!(test.service.paired_devices().await.unwrap().is_empty());
    test.stop().await;
}

#[tokio::test]
async fn wrong_secrets_and_no_session_are_forbidden() {
    let test = TestService::start().await;
    let no_session = Phone::try_pair(&test, &[1u8; 32], device_id(), "x").await;
    assert_eq!(no_session.status, 403);

    let payload = test.service.begin_pairing();
    let mut wrong = payload.secret.clone();
    wrong[5] ^= 0xFF;
    assert_eq!(
        Phone::try_pair(&test, &wrong, device_id(), "x")
            .await
            .status,
        403
    );
    assert_eq!(
        Phone::try_pair(&test, &[], device_id(), "x").await.status,
        403
    );
    let client = test.client();
    let bearer_instead = client
        .json(
            "POST",
            "/v1/pair",
            &[("Authorization", &bearer(&STANDARD.encode(&payload.secret)))],
            &wire::PairRequest {
                device_id: device_id(),
                device_name: "x".to_owned(),
            },
        )
        .await;
    assert_eq!(bearer_instead.status, 403);

    // A wrong secret does not burn the session; the right one still pairs,
    // and base64url (the QR form) is accepted too.
    let url_form = client
        .json(
            "POST",
            "/v1/pair",
            &[(
                "Authorization",
                &format!("Pairing {}", base64url::encode(&payload.secret)),
            )],
            &wire::PairRequest {
                device_id: device_id(),
                device_name: "iPhone".to_owned(),
            },
        )
        .await;
    assert_eq!(url_form.status, 200);

    test.service.cancel_pairing();
    let cancelled = test.service.begin_pairing();
    test.service.cancel_pairing();
    assert_eq!(
        Phone::try_pair(&test, &cancelled.secret, device_id(), "x")
            .await
            .status,
        403
    );
    test.stop().await;
}

#[tokio::test]
async fn malformed_pair_requests_are_400_and_keep_the_window_open() {
    let test = TestService::start().await;
    let payload = test.service.begin_pairing();
    let client = test.client();

    let garbage = client
        .request(
            "POST",
            "/v1/pair",
            &[("Authorization", &common::pairing(&payload.secret))],
            Some(b"not json".to_vec()),
        )
        .await
        .unwrap();
    assert_eq!(garbage.status, 400);
    let blank = Phone::try_pair(&test, &payload.secret, device_id(), "   ").await;
    assert_eq!(blank.status, 400);
    assert!(test.service.paired_devices().await.unwrap().is_empty());

    let good = Phone::try_pair(&test, &payload.secret, device_id(), "Test iPhone").await;
    assert_eq!(good.status, 200);
    test.stop().await;
}

#[tokio::test]
async fn revoked_tokens_are_401_and_repairing_replaces_the_device() {
    let test = TestService::start().await;
    let client = test.client();

    let secret = test.service.begin_pairing().secret;
    let first = Phone::try_pair(&test, &secret, device_id(), "Test iPhone").await;
    let token = first.json::<wire::PairResponse>().token;
    let probe = client
        .get(
            &format!("/v1/recordings/{}", Uuid::new_v4()),
            &[("Authorization", &bearer(&token))],
        )
        .await;
    assert_ne!(probe.status, 401, "a fresh token passes the gate");

    test.service.revoke(device_id()).await.unwrap();
    assert!(test.service.paired_devices().await.unwrap().is_empty());
    let revoked = client
        .get(
            &format!("/v1/recordings/{}", Uuid::new_v4()),
            &[("Authorization", &bearer(&token))],
        )
        .await;
    assert_eq!(revoked.status, 401);

    let secret = test.service.begin_pairing().secret;
    let again = Phone::try_pair(&test, &secret, device_id(), "New phone").await;
    assert_eq!(again.status, 200);
    let new_token = again.json::<wire::PairResponse>().token;
    assert_ne!(new_token, token);
    let devices = test.service.paired_devices().await.unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].name, "New phone");
    let still_revoked = client
        .get(
            &format!("/v1/recordings/{}", Uuid::new_v4()),
            &[("Authorization", &bearer(&token))],
        )
        .await;
    assert_eq!(still_revoked.status, 401);
    test.stop().await;
}

#[test]
fn credential_parsing_is_case_insensitive_on_the_scheme() {
    assert_eq!(
        Engine::credential("Bearer", Some("Bearer abc")),
        Some("abc")
    );
    assert_eq!(
        Engine::credential("Bearer", Some("bearer abc")),
        Some("abc")
    );
    assert_eq!(Engine::credential("Bearer", Some("Pairing abc")), None);
    assert_eq!(Engine::credential("Bearer", Some("Bearer")), None);
    assert_eq!(Engine::credential("Bearer", Some("Bearer  ")), None);
    assert_eq!(Engine::credential("Bearer", None), None);
}

#[tokio::test]
async fn the_device_name_is_stored_trimmed() {
    let test = TestService::with(common::Options {
        start: false,
        ..common::Options::default()
    })
    .await;
    let payload = test.service.begin_pairing();
    let id = Uuid::new_v4();
    let response =
        common::engine_pair(&test, &payload, id, " \u{200B}Nicolai's iPhone\n\u{3000}").await;
    assert_eq!(response.status.as_u16(), 200);
    let stored = test.store.paired_device(id).unwrap().unwrap();
    assert_eq!(stored.name, "Nicolai's iPhone");
}

#[tokio::test]
async fn a_failed_device_save_reopens_the_window_for_the_same_secret() {
    // The secret was taken before the save; a save that fails must give
    // the window back, or the phone holds a QR code that pairs nothing.
    let test = TestService::with(common::Options {
        start: false,
        ..common::Options::default()
    })
    .await;
    let payload = test.service.begin_pairing();
    common::execute_batch(
        &test.store,
        "CREATE TEMP TRIGGER refuse_pairing BEFORE INSERT ON pairedDevice \
         BEGIN SELECT RAISE(ABORT, 'refused'); END",
    );
    let failed = common::engine_pair(&test, &payload, Uuid::new_v4(), "iPhone").await;
    assert_eq!(failed.status.as_u16(), 500);
    assert!(test.service.engine.pairing_is_open(), "the window is back");

    common::execute_batch(&test.store, "DROP TRIGGER temp.refuse_pairing");
    let id = Uuid::new_v4();
    let paired = common::engine_pair(&test, &payload, id, "iPhone").await;
    assert_eq!(paired.status.as_u16(), 200);
    assert!(!test.service.engine.pairing_is_open(), "and now spent");
    assert!(test.store.paired_device(id).unwrap().is_some());
}

#[tokio::test]
async fn a_head_authorised_against_a_closed_window_does_not_pair_against_the_next() {
    // The phone's head passed the gate, then its body trickled in while
    // the user cancelled and opened a window for another phone.
    let test = TestService::with(common::Options {
        start: false,
        ..common::Options::default()
    })
    .await;
    let first = test.service.begin_pairing();
    let principal = common::pairing_principal(&test, &first).await;
    test.service.cancel_pairing();
    let _second = test.service.begin_pairing();

    let late = common::engine_pair_as(&test, principal, Uuid::new_v4(), "iPhone").await;
    assert_eq!(late.status.as_u16(), 403);
    assert!(
        test.service.engine.pairing_is_open(),
        "the new window is kept"
    );
    assert!(test.store.paired_devices().unwrap().is_empty());
}

/// Makes every device save fail until `DROP TRIGGER temp.refuse_pairing`.
fn refuse_pairing(test: &TestService) {
    common::execute_batch(
        &test.store,
        "CREATE TEMP TRIGGER refuse_pairing BEFORE INSERT ON pairedDevice \
         BEGIN SELECT RAISE(ABORT, 'refused'); END",
    );
}

/// Whether the gate turns `secret` away as it does with no session open.
async fn gate_refuses(test: &TestService, secret: &[u8]) -> bool {
    match test
        .service
        .engine
        .authenticate(Route::Pair, Some(&common::pairing(secret)))
        .await
    {
        AuthOutcome::Allowed(_) => false,
        AuthOutcome::Rejected(response) => response.status.as_u16() == 403,
    }
}

/// The body of `POST /v1/pair` as `principal` while a thread holds the
/// store, so the device save waits; `meanwhile` runs once the engine has
/// taken the session, then the save goes ahead. Returns the status.
async fn pair_during_a_held_save(
    test: &TestService,
    principal: Principal,
    meanwhile: impl FnOnce(),
) -> u16 {
    let (held, held_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel::<()>();
    let store = test.store.clone();
    let holder = std::thread::spawn(move || {
        store
            .read(|_| {
                held.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            })
            .unwrap();
    });
    held_rx.recv().unwrap();

    let pair = common::engine_pair_as(test, principal, Uuid::new_v4(), "iPhone");
    let drive = async {
        while test.service.engine.pairing_is_open() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        meanwhile();
        release.send(()).unwrap();
    };
    let (response, ()) =
        tokio::time::timeout(Duration::from_secs(10), async { tokio::join!(pair, drive) })
            .await
            .expect("the save was held and released");
    holder.join().unwrap();
    response.status.as_u16()
}

#[tokio::test]
async fn a_window_cancelled_while_its_save_runs_stays_closed_when_the_save_fails() {
    // The user cancels while the phone's pair is saving; the save then
    // fails. The QR code on screen must stay dead.
    let test = TestService::with(common::Options {
        start: false,
        ..common::Options::default()
    })
    .await;
    let payload = test.service.begin_pairing();
    let principal = common::pairing_principal(&test, &payload).await;
    refuse_pairing(&test);

    let status = pair_during_a_held_save(&test, principal.clone(), || {
        test.service.cancel_pairing();
    })
    .await;
    assert_eq!(status, 500);
    assert!(!test.service.engine.pairing_is_open(), "the cancel holds");

    common::execute_batch(&test.store, "DROP TRIGGER temp.refuse_pairing");
    assert!(gate_refuses(&test, &payload.secret).await);
    let late = common::engine_pair_as(&test, principal, Uuid::new_v4(), "iPhone").await;
    assert_eq!(late.status.as_u16(), 403);
    assert!(test.store.paired_devices().unwrap().is_empty());
}

#[tokio::test]
async fn a_window_replaced_and_cancelled_while_a_save_runs_brings_neither_back() {
    // The phone's pair is saving; the user opens a window for another
    // phone, then cancels it; the save fails. Neither QR code pairs.
    let test = TestService::with(common::Options {
        start: false,
        ..common::Options::default()
    })
    .await;
    let first = test.service.begin_pairing();
    let principal = common::pairing_principal(&test, &first).await;
    refuse_pairing(&test);

    let mut second = None;
    let status = pair_during_a_held_save(&test, principal.clone(), || {
        second = Some(test.service.begin_pairing());
        test.service.cancel_pairing();
    })
    .await;
    let second = second.expect("opened during the save");
    assert_eq!(status, 500);
    assert!(!test.service.engine.pairing_is_open(), "the cancel holds");

    common::execute_batch(&test.store, "DROP TRIGGER temp.refuse_pairing");
    assert!(gate_refuses(&test, &first.secret).await, "the first QR");
    assert!(gate_refuses(&test, &second.secret).await, "the second QR");
    let late = common::engine_pair_as(&test, principal, Uuid::new_v4(), "iPhone").await;
    assert_eq!(late.status.as_u16(), 403);
    assert!(test.store.paired_devices().unwrap().is_empty());
}
