//! The phone's half of the wire, read as text from `mobile/` and held
//! against what the server encodes and decodes: field names, value kinds,
//! enum values, constants, paths and QR query names. The phone runs the
//! mirror image in `native-contract.test.ts`.

#![allow(clippy::too_many_lines)]

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

fn phone_source(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{relative}: {error}"))
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
        groups(r"(\w+)\??:\s", &body)
    }

    /// `export type Name = "a" | "b";` → `["a", "b"]`.
    fn union_values(&self, name: &str) -> Vec<String> {
        let body = first(&format!(r"export type {name} =([^;]*);"), &self.wire).unwrap();
        groups(r#""([^"]+)""#, &body)
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

/// The first group of every match.
fn groups(pattern: &str, source: &str) -> Vec<String> {
    captures(pattern, source)
        .into_iter()
        .map(|groups| groups[0].clone())
        .collect()
}

fn first(pattern: &str, source: &str) -> Option<String> {
    groups(pattern, source).into_iter().next()
}

fn encoded<T: Serialize>(value: &T) -> serde_json::Map<String, Value> {
    match serde_json::to_value(value).unwrap() {
        Value::Object(map) => map,
        other => panic!("not a JSON object: {other}"),
    }
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
        let value = &object[key];
        let fits = match kind.as_str() {
            "string" => value.is_string(),
            "number" => value.is_number(),
            "number[]" => value
                .as_array()
                .is_some_and(|items| items.iter().all(Value::is_i64)),
            other => panic!("unknown decodeShape kind {other} for {name}.{key}"),
        };
        assert!(fits, "{name}.{key} must be {kind}");
    }
}

fn mac_id() -> Uuid {
    Uuid::parse_str("0F8FAD5B-D9CB-469F-A165-70867728950E").unwrap()
}

#[test]
fn every_body_carries_the_phones_field_names_and_kinds() {
    let phone = Phone::read();
    let metadata = RecordingMetadata {
        recording_id: Uuid::parse_str("6F9619FF-8B86-D011-B42D-00C04FC964FF").unwrap(),
        started_at: DateTime::from_timestamp_millis(1_790_000_000_250).unwrap(),
        duration_seconds: 3600.25,
        byte_count: 28_800_000,
        sha256: vec![1; 32],
        chunk_size: 16 * 1024 * 1024,
        format: AudioFormat::M4aAac,
        device_name: "iPhone".to_owned(),
    };
    let bodies = [
        (
            "CompleteResponse",
            encoded(&wire::CompleteResponse {
                meeting_id: mac_id(),
            }),
        ),
        ("Hello", encoded(&wire::Hello::new(mac_id()))),
        (
            "PairRequest",
            encoded(&wire::PairRequest {
                device_id: mac_id(),
                device_name: "iPhone".to_owned(),
            }),
        ),
        (
            "PairResponse",
            encoded(&wire::PairResponse {
                token: "t".to_owned(),
                mac_id: mac_id(),
                mac_name: "Studio".to_owned(),
            }),
        ),
        ("RecordingMetadata", encoded(&metadata)),
        (
            "RecordingStatus",
            encoded(&wire::RecordingStatus {
                state: HandoverStateKind::Receiving,
                received_chunks: vec![0, 2],
            }),
        ),
    ];
    let names: Vec<String> = bodies.iter().map(|(name, _)| (*name).to_owned()).collect();
    assert_eq!(
        sorted(groups(r"export type (\w+) = \{", &phone.wire)),
        names
    );
    for (name, object) in &bodies {
        let fields: Vec<String> = object.keys().cloned().collect();
        assert_eq!(sorted(fields), sorted(phone.object_fields(name)), "{name}");
        // The phone decodes the responses only.
        if !matches!(*name, "PairRequest" | "RecordingMetadata") {
            expect_shape(object, &phone.decode_shape(name), name);
        }
    }
    let metadata = &bodies[4].1;
    let iso = Regex::new(r"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z$").unwrap();
    assert!(
        iso.is_match(metadata["startedAt"].as_str().unwrap()),
        "{}",
        metadata["startedAt"]
    );
    let sha =
        base64::engine::general_purpose::STANDARD.decode(metadata["sha256"].as_str().unwrap());
    assert_eq!(sha.unwrap().len(), 32, "standard base64");
    assert!(metadata["byteCount"].is_i64());
    assert_eq!(metadata["format"], Value::from("m4aAAC"));
    assert_eq!(bodies[5].1["state"], Value::from("receiving"));
}

#[test]
fn the_server_decodes_bodies_exactly_as_the_phone_encodes_them() {
    // As the phone's `metadataFor` encodes: lowercase UUID, `.000Z`, standard
    // base64.
    let sha = base64::engine::general_purpose::STANDARD.encode([3u8; 32]);
    let phone_metadata = format!(
        r#"{{"recordingID":"6f9619ff-8b86-d011-b42d-00c04fc964ff","startedAt":"2026-09-25T09:00:00.000Z","durationSeconds":61.5,"byteCount":3000,"sha256":"{sha}","chunkSize":1024,"format":"m4aAAC","deviceName":"Nicolai's iPhone"}}"#
    );
    let decoded: RecordingMetadata = serde_json::from_str(&phone_metadata).unwrap();
    assert_eq!(
        decoded,
        RecordingMetadata {
            recording_id: Uuid::parse_str("6F9619FF-8B86-D011-B42D-00C04FC964FF").unwrap(),
            started_at: DateTime::<Utc>::from_timestamp(1_790_326_800, 0).unwrap(),
            duration_seconds: 61.5,
            byte_count: 3000,
            sha256: vec![3u8; 32],
            chunk_size: 1024,
            format: AudioFormat::M4aAac,
            device_name: "Nicolai's iPhone".to_owned(),
        }
    );
    let phone_pair = r#"{"deviceID":"0f8fad5b-d9cb-469f-a165-70867728950e","deviceName":"iPhone"}"#;
    let pair: wire::PairRequest = serde_json::from_str(phone_pair).unwrap();
    assert_eq!(pair.device_id, mac_id());
    assert_eq!(pair.device_name, "iPhone");
}

#[test]
fn enums_constants_and_paths_match() {
    let phone = Phone::read();
    let kinds: Vec<String> = HandoverStateKind::ALL
        .iter()
        .map(|kind| kind.as_str().to_owned())
        .collect();
    assert_eq!(phone.union_values("RecordingState"), kinds);
    let states = first(r"RECORDING_STATES[^=]*=\s*\[([^\]]*)\]", &phone.wire).unwrap();
    assert_eq!(groups(r#""([^"]+)""#, &states), kinds);
    let formats: BTreeSet<String> = AudioFormat::ALL
        .iter()
        .map(|format| format.as_str().to_owned())
        .collect();
    let phone_formats: BTreeSet<String> = phone.union_values("AudioFormat").into_iter().collect();
    assert_eq!(phone_formats, formats);
    let recorded = first(
        r#"RECORDING_FORMAT: AudioFormat = "([^"]+)""#,
        &phone.recorder,
    )
    .unwrap();
    let recorded: AudioFormat = recorded.parse().unwrap();
    assert!(
        MetadataValidation::ACCEPTED_FORMATS.contains(&recorded),
        "the server accepts what the phone records"
    );

    let constant = |pattern: &str| first(pattern, &phone.wire);
    assert_eq!(
        constant(r"export const PROTOCOL_VERSION = (\d+);"),
        Some(wire::PROTOCOL_VERSION.to_string())
    );
    assert_eq!(
        constant(r#"export const SERVICE_TYPE = "([^"]+)";"#).as_deref(),
        Some(wire::SERVICE_TYPE)
    );
    assert_eq!(
        constant(r#"export const CHUNK_HASH_HEADER = "([^"]+)";"#).as_deref(),
        Some(wire::CHUNK_HASH_HEADER)
    );
    let schemes = constant(r#"authorizationHeader\(\s*scheme: ("[^)]+"),"#).unwrap();
    assert_eq!(schemes.replace(['"', ' '], ""), "Pairing|Bearer");
    assert_eq!(
        constant(r"return `(https)://").as_deref(),
        Some("https"),
        "https only"
    );

    let id = Uuid::new_v4();
    let path = |name: &str| {
        constant(&format!(r#"\b{name}:[^`"]*[`"]([^`"]+)[`"]"#))
            .unwrap_or_else(|| panic!("no path {name}"))
            .replace(
                "${encodeURIComponent(recordingID)}",
                &id.hyphenated().to_string(),
            )
            .replace("${index}", "3")
    };
    let routes = [
        (Method::GET, "hello", Route::Hello),
        (Method::POST, "pair", Route::Pair),
        (Method::DELETE, "pairing", Route::Unpair),
        (Method::PUT, "recording", Route::Announce(id)),
        (Method::GET, "recording", Route::Status(id)),
        (Method::PUT, "chunk", Route::Chunk(id, 3)),
        (Method::POST, "complete", Route::Complete(id)),
    ];
    for (method, name, route) in routes {
        assert_eq!(Route::matches(&method, &path(name)), Some(route), "{name}");
    }
}

#[test]
fn the_qr_payload_uses_the_names_the_phone_parses() {
    let phone = Phone::read();
    let url = PairingPayload::new(
        mac_id(),
        "Mac",
        vec![7; 32],
        vec![9; 32],
        DateTime::<Utc>::from_timestamp(1_790_000_300, 0).unwrap(),
    )
    .url_string();
    assert!(
        url.starts_with("steno://pair/v1?mac=0f8fad5b-"),
        "lowercase mac"
    );
    let query: BTreeSet<String> = url
        .split_once('?')
        .unwrap()
        .1
        .split('&')
        .map(|pair| pair.split_once('=').unwrap().0.to_owned())
        .collect();
    let parsed: BTreeSet<String> = groups(r#"params\.get\("(\w+)"\)"#, &phone.pairing)
        .into_iter()
        .collect();
    assert_eq!(query, parsed);
    let constant = |pattern: &str| first(pattern, &phone.pairing);
    assert_eq!(
        constant(r#"const PREFIX = "([^"]+)";"#),
        Some(format!("{}://pair/", PairingPayload::SCHEME))
    );
    assert_eq!(
        constant(r#"version !== "([^"]+)""#).as_deref(),
        Some(PairingPayload::VERSION)
    );
    assert_eq!(
        constant(r"const FINGERPRINT_BYTES = (\d+);").as_deref(),
        Some("32")
    );
    assert_eq!(
        constant(r"const SECRET_BYTES = (\d+);").as_deref(),
        Some("32")
    );
    assert!(
        phone.pairing.contains(r"/^\d{1,12}$/"),
        "exp is at most twelve digits on both sides"
    );
}
