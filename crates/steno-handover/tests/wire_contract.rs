//! The phone's half of the wire, read as text from `mobile/` and held
//! against what the server encodes and decodes: the field names of every
//! JSON body, the value kinds the phone's `decodeShape` demands, the enum
//! values, the constants, the seven paths and the QR query names. The iOS
//! side runs the mirror image in `native-contract.test.ts`, so a rename on
//! either side fails one CI or the other. Swift:
//! `Tests/StenoHandoverTests/WireContractTests.swift`.

#![allow(
    clippy::assert_is_empty,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::large_futures,
    clippy::too_many_lines
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use base64::Engine as _;
use chrono::{DateTime, Utc};
use http::Method;
use regex::Regex;
use serde::Serialize;
use serde_json::Value;
use steno_core::{AudioFormat, HandoverStateKind, RecordingMetadata};
use steno_handover::route::Route;
use steno_handover::upload::MetadataValidation;
use steno_handover::{PairingPayload, wire};
use uuid::Uuid;

fn repository() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn phone_source(relative: &str) -> String {
    std::fs::read_to_string(repository().join(relative))
        .unwrap_or_else(|error| panic!("{relative}: {error}"))
}

struct Phone {
    wire: String,
    pairing: String,
    recorder: String,
}

impl Phone {
    fn read() -> Self {
        Phone {
            wire: phone_source("mobile/modules/steno-link/src/wire.ts"),
            pairing: phone_source("mobile/src/features/pairing/pairing-payload.ts"),
            recorder: phone_source("mobile/src/features/recorder/recording-options.ts"),
        }
    }

    /// `export type Name = { a: string; b: number[] }` → `["a", "b"]`.
    fn object_fields(&self, name: &str) -> Vec<String> {
        let body = first(&format!(r"export type {name} = \{{([^}}]*)\}}"), &self.wire)
            .unwrap_or_else(|| panic!("no object type {name}"));
        captures(r"(\w+)\??:\s", &body)
            .into_iter()
            .map(|groups| groups[0].clone())
            .collect()
    }

    /// `export type Name = "a" | "b";` → `["a", "b"]`.
    fn union_values(&self, name: &str) -> Vec<String> {
        let body = first(&format!(r"export type {name} =([^;]*);"), &self.wire).unwrap();
        captures(r#""([^"]+)""#, &body)
            .into_iter()
            .map(|groups| groups[0].clone())
            .collect()
    }

    /// `decodeShape<Name>(text, { a: "string", b: "number[]" })` → the shape.
    fn decode_shape(&self, name: &str) -> BTreeMap<String, String> {
        let body = first(
            &format!(r"decodeShape<{name}>\(text,\s*\{{([^}}]*)\}}"),
            &self.wire,
        )
        .unwrap_or_else(|| panic!("no decodeShape for {name}"));
        captures(r#"(\w+):\s*"([^"]+)""#, &body)
            .into_iter()
            .map(|groups| (groups[0].clone(), groups[1].clone()))
            .collect()
    }
}

/// Capture groups of every match, in source order.
fn captures(pattern: &str, source: &str) -> Vec<Vec<String>> {
    let regex = Regex::new(&format!("(?s){pattern}")).unwrap();
    regex
        .captures_iter(source)
        .map(|found| {
            (1..found.len())
                .map(|index| {
                    found
                        .get(index)
                        .map_or(String::new(), |m| m.as_str().to_owned())
                })
                .collect()
        })
        .collect()
}

fn first(pattern: &str, source: &str) -> Option<String> {
    captures(pattern, source)
        .into_iter()
        .next()?
        .into_iter()
        .next()
}

fn encoded<T: Serialize>(value: &T) -> serde_json::Map<String, Value> {
    match serde_json::to_value(value).unwrap() {
        Value::Object(map) => map,
        other => panic!(
            "{} does not encode as a JSON object: {other}",
            std::any::type_name::<T>()
        ),
    }
}

fn keys(object: &serde_json::Map<String, Value>) -> Vec<String> {
    object.keys().cloned().collect()
}

fn sorted(mut values: Vec<String>) -> Vec<String> {
    values.sort();
    values
}

/// The object satisfies the phone's `decodeShape` for it.
fn expect_shape(
    object: &serde_json::Map<String, Value>,
    shape: &BTreeMap<String, String>,
    name: &str,
) {
    for (key, kind) in shape {
        let value = object
            .get(key)
            .unwrap_or_else(|| panic!("{name}.{key} is missing from the server's JSON"));
        match kind.as_str() {
            "string" => assert!(value.is_string(), "{name}.{key} must be a string"),
            "number" => assert!(value.is_number(), "{name}.{key} must be a number"),
            "number[]" => assert!(
                value
                    .as_array()
                    .is_some_and(|items| items.iter().all(Value::is_i64)),
                "{name}.{key} must be integers"
            ),
            other => panic!("unknown decodeShape kind {other} for {name}.{key}"),
        }
    }
}

fn mac_id() -> Uuid {
    Uuid::parse_str("0F8FAD5B-D9CB-469F-A165-70867728950E").unwrap()
}

fn sample_metadata() -> RecordingMetadata {
    RecordingMetadata {
        recording_id: Uuid::parse_str("6F9619FF-8B86-D011-B42D-00C04FC964FF").unwrap(),
        started_at: DateTime::from_timestamp_millis(1_790_000_000_250).unwrap(),
        duration_seconds: 3600.25,
        byte_count: 28_800_000,
        sha256: vec![1; 32],
        chunk_size: 16 * 1024 * 1024,
        format: AudioFormat::M4aAac,
        device_name: "iPhone".to_owned(),
    }
}

#[test]
fn wire_declares_exactly_the_six_bodies_the_server_encodes() {
    let phone = Phone::read();
    let declared: Vec<String> = captures(r"export type (\w+) = \{", &phone.wire)
        .into_iter()
        .map(|groups| groups[0].clone())
        .collect();
    assert_eq!(
        sorted(declared),
        [
            "CompleteResponse",
            "Hello",
            "PairRequest",
            "PairResponse",
            "RecordingMetadata",
            "RecordingStatus"
        ]
    );
}

#[test]
fn response_bodies_carry_the_phones_field_names_and_kinds() {
    let phone = Phone::read();
    let hello = encoded(&wire::Hello::new(mac_id()));
    assert_eq!(sorted(keys(&hello)), sorted(phone.object_fields("Hello")));
    expect_shape(&hello, &phone.decode_shape("Hello"), "Hello");
    assert_eq!(hello["protocol"], Value::from(wire::PROTOCOL_VERSION));

    let paired = encoded(&wire::PairResponse {
        token: "t".to_owned(),
        mac_id: mac_id(),
        mac_name: "Studio".to_owned(),
    });
    assert_eq!(
        sorted(keys(&paired)),
        sorted(phone.object_fields("PairResponse"))
    );
    expect_shape(&paired, &phone.decode_shape("PairResponse"), "PairResponse");

    let status = encoded(&wire::RecordingStatus {
        state: HandoverStateKind::Receiving,
        received_chunks: vec![0, 2],
    });
    assert_eq!(
        sorted(keys(&status)),
        sorted(phone.object_fields("RecordingStatus"))
    );
    expect_shape(
        &status,
        &phone.decode_shape("RecordingStatus"),
        "RecordingStatus",
    );
    assert_eq!(status["state"], Value::from("receiving"));

    let complete = encoded(&wire::CompleteResponse {
        meeting_id: mac_id(),
    });
    assert_eq!(
        sorted(keys(&complete)),
        sorted(phone.object_fields("CompleteResponse"))
    );
    expect_shape(
        &complete,
        &phone.decode_shape("CompleteResponse"),
        "CompleteResponse",
    );
}

#[test]
fn request_bodies_use_the_phones_field_names() {
    let phone = Phone::read();
    let request = encoded(&wire::PairRequest {
        device_id: mac_id(),
        device_name: "iPhone".to_owned(),
    });
    assert_eq!(
        sorted(keys(&request)),
        sorted(phone.object_fields("PairRequest"))
    );

    let metadata = encoded(&sample_metadata());
    assert_eq!(
        sorted(keys(&metadata)),
        sorted(phone.object_fields("RecordingMetadata"))
    );
    // The value shapes `wire.test.ts` asserts for its own encoding.
    let uuid = Regex::new(
        r"^[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}$",
    )
    .unwrap();
    assert!(uuid.is_match(metadata["recordingID"].as_str().unwrap()));
    let iso = Regex::new(r"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z$").unwrap();
    assert!(
        iso.is_match(metadata["startedAt"].as_str().unwrap()),
        "ISO 8601 with three fractional digits: {}",
        metadata["startedAt"]
    );
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(metadata["sha256"].as_str().unwrap())
            .unwrap()
            .len(),
        32
    );
    assert!(metadata["byteCount"].is_i64());
    assert_eq!(metadata["format"], Value::from("m4aAAC"));
}

#[test]
fn the_server_decodes_bodies_exactly_as_the_phone_encodes_them() {
    // `metadataFor` in `recording-client.ts` and `JSON.stringify`, as the
    // phone's own tests pin them: lowercase UUID, `.000Z`, standard base64.
    let sha = base64::engine::general_purpose::STANDARD.encode([3u8; 32]);
    let phone_metadata = format!(
        r#"{{"recordingID":"6f9619ff-8b86-d011-b42d-00c04fc964ff","startedAt":"2026-09-25T09:00:00.000Z","durationSeconds":61.5,"byteCount":3000,"sha256":"{sha}","chunkSize":1024,"format":"m4aAAC","deviceName":"Nicolai's iPhone"}}"#
    );
    let decoded: RecordingMetadata = serde_json::from_str(&phone_metadata).unwrap();
    assert_eq!(
        decoded.recording_id,
        Uuid::parse_str("6F9619FF-8B86-D011-B42D-00C04FC964FF").unwrap()
    );
    assert_eq!(
        decoded.started_at,
        DateTime::<Utc>::from_timestamp(1_790_326_800, 0).unwrap()
    );
    assert_eq!(decoded.duration_seconds, 61.5);
    assert_eq!(decoded.byte_count, 3000);
    assert_eq!(decoded.sha256, vec![3u8; 32]);
    assert_eq!(decoded.chunk_size, 1024);
    assert_eq!(decoded.format, AudioFormat::M4aAac);
    assert_eq!(decoded.device_name, "Nicolai's iPhone");

    let phone_pair = r#"{"deviceID":"0f8fad5b-d9cb-469f-a165-70867728950e","deviceName":"iPhone"}"#;
    let pair: wire::PairRequest = serde_json::from_str(phone_pair).unwrap();
    assert_eq!(
        pair,
        wire::PairRequest {
            device_id: mac_id(),
            device_name: "iPhone".to_owned()
        }
    );
}

#[test]
fn enum_values_match_core() {
    let phone = Phone::read();
    let kinds: Vec<String> = HandoverStateKind::ALL
        .iter()
        .map(|kind| kind.as_str().to_owned())
        .collect();
    assert_eq!(phone.union_values("RecordingState"), kinds);
    let states = first(r"RECORDING_STATES[^=]*=\s*\[([^\]]*)\]", &phone.wire).unwrap();
    assert_eq!(
        captures(r#""([^"]+)""#, &states)
            .into_iter()
            .map(|g| g[0].clone())
            .collect::<Vec<_>>(),
        kinds
    );
    let formats: BTreeSet<String> = AudioFormat::ALL
        .iter()
        .map(|format| format.as_str().to_owned())
        .collect();
    assert_eq!(
        phone
            .union_values("AudioFormat")
            .into_iter()
            .collect::<BTreeSet<_>>(),
        formats
    );
    let recording_format = first(
        r#"RECORDING_FORMAT: AudioFormat = "([^"]+)""#,
        &phone.recorder,
    )
    .unwrap();
    let format: AudioFormat = recording_format.parse().unwrap();
    assert!(
        MetadataValidation::ACCEPTED_FORMATS.contains(&format),
        "the server accepts what the phone records"
    );
}

#[test]
fn constants_match() {
    let phone = Phone::read();
    assert_eq!(
        first(r"export const PROTOCOL_VERSION = (\d+);", &phone.wire).as_deref(),
        Some("1")
    );
    assert_eq!(wire::PROTOCOL_VERSION, 1);
    assert_eq!(
        first(r#"export const SERVICE_TYPE = "([^"]+)";"#, &phone.wire).as_deref(),
        Some(wire::SERVICE_TYPE)
    );
    assert_eq!(
        first(
            r#"export const CHUNK_HASH_HEADER = "([^"]+)";"#,
            &phone.wire
        )
        .as_deref(),
        Some(wire::CHUNK_HASH_HEADER)
    );
    let schemes = first(r#"authorizationHeader\(\s*scheme: ("[^)]+"),"#, &phone.wire).unwrap();
    assert_eq!(schemes.replace(['"', ' '], ""), "Pairing|Bearer");
}

#[test]
fn the_phones_paths_match_the_servers_routes() {
    let phone = Phone::read();
    let id = Uuid::new_v4();
    let path = |name: &str| {
        let literal = first(&format!(r#"\b{name}:[^`"]*[`"]([^`"]+)[`"]"#), &phone.wire)
            .unwrap_or_else(|| panic!("no path {name}"));
        literal
            .replace(
                "${encodeURIComponent(recordingID)}",
                &id.hyphenated().to_string(),
            )
            .replace("${index}", "3")
    };
    assert_eq!(
        Route::matches(&Method::GET, &path("hello")),
        Some(Route::Hello)
    );
    assert_eq!(
        Route::matches(&Method::POST, &path("pair")),
        Some(Route::Pair)
    );
    assert_eq!(
        Route::matches(&Method::DELETE, &path("pairing")),
        Some(Route::Unpair)
    );
    assert_eq!(
        Route::matches(&Method::PUT, &path("recording")),
        Some(Route::Announce(id))
    );
    assert_eq!(
        Route::matches(&Method::GET, &path("recording")),
        Some(Route::Status(id))
    );
    assert_eq!(
        Route::matches(&Method::PUT, &path("chunk")),
        Some(Route::Chunk(id, 3))
    );
    assert_eq!(
        Route::matches(&Method::POST, &path("complete")),
        Some(Route::Complete(id))
    );
    assert_eq!(
        first(r"return `(https)://", &phone.wire).as_deref(),
        Some("https"),
        "the phone speaks https only"
    );
}

#[test]
fn the_qr_payload_uses_the_names_the_phone_parses() {
    let phone = Phone::read();
    let payload = PairingPayload::new(
        mac_id(),
        "Mac",
        vec![7; 32],
        vec![9; 32],
        DateTime::<Utc>::from_timestamp(1_790_000_300, 0).unwrap(),
    );
    let url = payload.url_string();
    let query = url.split_once('?').unwrap().1;
    let mac_query: BTreeSet<String> = query
        .split('&')
        .map(|pair| pair.split_once('=').unwrap().0.to_owned())
        .collect();
    let phone_query: BTreeSet<String> = captures(r#"params\.get\("(\w+)"\)"#, &phone.pairing)
        .into_iter()
        .map(|groups| groups[0].clone())
        .collect();
    assert_eq!(mac_query, phone_query);
    assert_eq!(
        mac_query,
        ["mac", "name", "fp", "secret", "exp"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    );

    assert_eq!(
        first(r#"const PREFIX = "([^"]+)";"#, &phone.pairing).unwrap(),
        format!("{}://pair/", PairingPayload::SCHEME)
    );
    assert_eq!(
        first(r#"version !== "([^"]+)""#, &phone.pairing).as_deref(),
        Some(PairingPayload::VERSION)
    );
    assert_eq!(
        first(r"const FINGERPRINT_BYTES = (\d+);", &phone.pairing).as_deref(),
        Some("32")
    );
    assert_eq!(
        first(r"const SECRET_BYTES = (\d+);", &phone.pairing).as_deref(),
        Some("32")
    );
    assert!(
        phone.pairing.contains(r"/^\d{1,12}$/"),
        "exp is at most twelve digits on both sides"
    );
    assert!(
        url.starts_with("steno://pair/v1?mac=0f8fad5b-"),
        "lowercase mac in the QR"
    );
}
