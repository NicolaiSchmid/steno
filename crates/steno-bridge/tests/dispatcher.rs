//! Host behaviour as `BridgeDispatcher.swift` defines it and as the web
//! transports expect it (`apps/macos/web/src/bridge/webkit-transport.test.ts`):
//! a request envelope in, a reply envelope out, typed errors for every way a
//! call can go wrong.

use std::sync::Mutex;

use serde_json::{Value, json};
use steno_bridge::*;
use uuid::Uuid;

/// A host that answers the main window's reply-bearing methods and records
/// what it was asked.
#[derive(Default)]
struct RecordingHost {
    calls: Mutex<Vec<String>>,
}

impl RecordingHost {
    fn record(&self, call: impl Into<String>) {
        self.calls.lock().unwrap().push(call.into());
    }
}

impl BridgeHost for RecordingHost {
    fn page_ready(&self) -> Outcome<()> {
        self.record("page.ready");
        Ok(())
    }

    fn meetings_select(&self, params: MeetingIdParams) -> Outcome<()> {
        self.record(format!(
            "meetings.select {}",
            json::uuid::format(&params.meeting_id)
        ));
        if params.meeting_id.is_nil() {
            return Err(BridgeError::not_found("No meeting with that id."));
        }
        Ok(())
    }

    fn settings_export_choose_vault(&self) -> Outcome<ChosenPathReply> {
        self.record("settings.export.chooseVault");
        Ok(ChosenPathReply {
            path: Some("/Users/nicolai/Notes".into()),
        })
    }

    fn speakers_options(&self, params: SpeakerOptionsParams) -> Outcome<SpeakerOptionsReply> {
        self.record(format!("speakers.options {}", params.query));
        Ok(SpeakerOptionsReply {
            prefill: None,
            options: vec![SpeakerOption {
                kind: SpeakerOptionKind::Create,
                label: format!("Add \u{201c}{}\u{201d}", params.query),
                detail: None,
                person_id: None,
            }],
        })
    }

    fn ui_confirm_destructive(&self, params: ConfirmDestructiveParams) -> Outcome<ConfirmReply> {
        self.record(format!("ui.confirmDestructive {}", params.title));
        Err(BridgeError::cancelled("The user cancelled."))
    }
}

fn dispatcher() -> Dispatcher<RecordingHost> {
    Dispatcher::new(RecordingHost::default())
}

fn call(dispatcher: &Dispatcher<RecordingHost>, body: &Value) -> Value {
    serde_json::from_str(&dispatcher.dispatch_json(&body.to_string())).unwrap()
}

#[test]
fn routes_a_method_without_params_and_replies_with_the_id_only() {
    let dispatcher = dispatcher();
    let reply = call(
        &dispatcher,
        &json!({"id": "req-1", "method": "page.ready", "params": null}),
    );
    assert_eq!(reply, json!({"id": "req-1"}));
    assert_eq!(
        dispatcher.host().calls.lock().unwrap().as_slice(),
        ["page.ready"]
    );
}

#[test]
fn unwraps_a_typed_reply_into_result() {
    let dispatcher = dispatcher();
    let reply = call(
        &dispatcher,
        &json!({"id": "req-2", "method": "settings.export.chooseVault", "params": null}),
    );
    assert_eq!(
        reply,
        json!({"id": "req-2", "result": {"path": "/Users/nicolai/Notes"}})
    );
    let reply = call(
        &dispatcher,
        &json!({"id": "req-3", "method": "speakers.options",
               "params": {"speakerID": "00000000-0000-0000-0000-000000000017", "query": "an"}}),
    );
    assert_eq!(
        reply,
        json!({"id": "req-3", "result": {"options": [{"kind": "create", "label": "Add \u{201c}an\u{201d}"}]}})
    );
}

#[test]
fn decodes_params_as_the_contract_type() {
    let dispatcher = dispatcher();
    let id = "00000000-0000-0000-0000-00000000000c";
    let reply = call(
        &dispatcher,
        &json!({"id": "r", "method": "meetings.select", "params": {"meetingID": id}}),
    );
    assert_eq!(reply, json!({"id": "r"}));
    assert_eq!(
        dispatcher.host().calls.lock().unwrap().as_slice(),
        ["meetings.select 00000000-0000-0000-0000-00000000000C"]
    );
}

#[test]
fn a_host_error_becomes_the_error_envelope_with_its_code() {
    let dispatcher = dispatcher();
    let nil = Uuid::nil().to_string();
    let reply = call(
        &dispatcher,
        &json!({"id": "r", "method": "meetings.select", "params": {"meetingID": nil}}),
    );
    assert_eq!(
        reply,
        json!({"id": "r", "error": {"code": "notFound", "message": "No meeting with that id."}})
    );
    let reply = call(
        &dispatcher,
        &json!({"id": "r2", "method": "ui.confirmDestructive",
               "params": {"title": "Delete?", "message": "Gone.", "confirmTitle": "Delete"}}),
    );
    assert_eq!(reply["error"]["code"], "cancelled");
}

#[test]
fn unknown_method_is_rejected_before_the_host_sees_it() {
    let dispatcher = dispatcher();
    let reply = call(
        &dispatcher,
        &json!({"id": "r", "method": "meetings.explode", "params": null}),
    );
    assert_eq!(
        reply,
        json!({"id": "r", "error": {"code": "unknownMethod", "message": "Unknown method 'meetings.explode'."}})
    );
    assert!(dispatcher.host().calls.lock().unwrap().is_empty());
}

#[test]
fn a_method_the_host_does_not_answer_is_unknown_method() {
    let dispatcher = dispatcher();
    let reply = call(&dispatcher, &json!({"id": "r", "method": "recording.stop"}));
    assert_eq!(
        reply,
        json!({"id": "r", "error": {"code": "unknownMethod", "message": "The host does not answer recording.stop."}})
    );
}

#[test]
fn missing_or_bad_params_are_invalid_params() {
    let dispatcher = dispatcher();
    let reply = call(
        &dispatcher,
        &json!({"id": "r", "method": "meetings.select"}),
    );
    assert_eq!(
        reply,
        json!({"id": "r", "error": {"code": "invalidParams", "message": "meetings.select needs params."}})
    );
    let reply = call(
        &dispatcher,
        &json!({"id": "r", "method": "meetings.select", "params": null}),
    );
    assert_eq!(reply["error"]["code"], "invalidParams");
    let reply = call(
        &dispatcher,
        &json!({"id": "r", "method": "meetings.select", "params": {"meetingID": "x"}}),
    );
    assert_eq!(reply["error"]["code"], "invalidParams");
    let message = reply["error"]["message"].as_str().unwrap();
    assert!(message.starts_with("meetings.select: "), "{message}");
    assert!(
        dispatcher.host().calls.lock().unwrap().is_empty(),
        "the host was called with bad params"
    );
}

#[test]
fn a_body_that_is_not_a_request_still_gets_a_reply() {
    let dispatcher = dispatcher();
    assert_eq!(
        call(&dispatcher, &json!([1, 2])),
        json!({"id": "", "error": {"code": "invalidParams", "message": "The message is not an object."}})
    );
    assert_eq!(
        call(&dispatcher, &json!({"id": "r", "params": {}})),
        json!({"id": "r", "error": {"code": "invalidParams", "message": "The message is not a bridge request."}})
    );
    assert_eq!(
        call(&dispatcher, &json!({"id": 7, "method": "page.ready"})),
        json!({"id": "", "error": {"code": "invalidParams", "message": "The message is not a bridge request."}})
    );
    let garbage: Value = serde_json::from_str(&dispatcher.dispatch_json("{not json")).unwrap();
    assert_eq!(garbage["error"]["code"], "invalidParams");
    assert_eq!(garbage["error"]["message"], "The message is not JSON.");
}

#[test]
fn decode_tells_requests_from_rejections() {
    let decoded = Dispatcher::<RecordingHost>::decode(
        r#"{"id":"r","method":"recording.stop","params":null}"#,
    );
    assert_eq!(
        decoded,
        Decoding::Request(BridgeRequest::new("r", BridgeMethod::RecordingStop, None))
    );
    let Decoding::Rejected(reply) =
        Dispatcher::<RecordingHost>::decode(r#"{"id":"r","method":"nope"}"#)
    else {
        panic!("an unknown method is rejected");
    };
    assert_eq!(reply.error.unwrap().code, BridgeErrorCode::UnknownMethod);
}

#[test]
fn replies_are_compact_with_sorted_keys() {
    let dispatcher = dispatcher();
    let nil = Uuid::nil().to_string();
    let body =
        json!({"id": "r", "method": "meetings.select", "params": {"meetingID": nil}}).to_string();
    assert_eq!(
        dispatcher.dispatch_json(&body),
        r#"{"error":{"code":"notFound","message":"No meeting with that id."},"id":"r"}"#
    );
}

#[derive(Default)]
struct RecordingSink {
    events: Mutex<Vec<BridgeEvent>>,
}

impl EventSink for RecordingSink {
    fn emit(&self, event: BridgeEvent) {
        self.events.lock().unwrap().push(event);
    }
}

#[test]
fn an_event_sink_publishes_snapshots_on_their_topic() {
    let sink = RecordingSink::default();
    let snapshot = RecordingSnapshot {
        state: RecordingState::Idle,
        started_at: None,
        mode: None,
        call_app: None,
        meeting_id: None,
        level: None,
        auto_stop: None,
        denied_permissions: vec![],
        warning: None,
        error: None,
    };
    sink.publish(&snapshot).unwrap();
    sink.publish(&None::<MeetingDetailSnapshot>).unwrap();
    sink.emit(BridgeEvent::new(BridgeTopic::App, json!({"version": "1"})));
    let events = sink.events.lock().unwrap();
    assert_eq!(events[0].topic, BridgeTopic::Recording);
    assert_eq!(
        events[0].payload,
        json!({"state": "idle", "deniedPermissions": []})
    );
    assert_eq!(
        events[1],
        BridgeEvent::new(BridgeTopic::MeetingDetail, Value::Null)
    );
    assert_eq!(events[2].payload["version"], "1");
}
