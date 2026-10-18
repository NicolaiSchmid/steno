//! The dispatcher as the web transports expect it: a request envelope in, a
//! reply envelope out, typed errors for every way a call can go wrong.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use steno_bridge::*;
use uuid::Uuid;

/// A spy host that answers a handful of methods and records what it was asked.
#[derive(Default)]
struct SpyHost {
    calls: Mutex<Vec<String>>,
}

impl SpyHost {
    fn record(&self, call: impl Into<String>) {
        self.calls.lock().unwrap().push(call.into());
    }

    /// What the host was asked, in order.
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl BridgeHost for SpyHost {
    fn page_ready(&self) -> Outcome<()> {
        self.record("page.ready");
        Ok(())
    }

    fn meetings_select(&self, params: MeetingIdParams) -> Outcome<()> {
        self.record(format!(
            "meetings.select {}",
            steno_core::json::uuid_string(params.meeting_id)
        ));
        if params.meeting_id.is_nil() {
            return Err(BridgeError::not_found("No meeting with that id."));
        }
        Ok(())
    }

    fn meeting_process_again(&self) -> Outcome<()> {
        self.record("meeting.processAgain");
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

/// The spy and a dispatcher over it; the test keeps the spy to read its calls.
fn dispatcher() -> (Arc<SpyHost>, Dispatcher<Arc<SpyHost>>) {
    let host = Arc::new(SpyHost::default());
    (Arc::clone(&host), Dispatcher::new(host))
}

fn call<H: BridgeHost>(dispatcher: &Dispatcher<H>, body: &Value) -> Value {
    serde_json::from_str(&dispatcher.dispatch_json(&body.to_string())).unwrap()
}

/// Each request's reply and what the host was asked: no params, typed
/// results with their optionals omitted, decoded params, host errors with
/// their code, and requests refused before the host sees them.
#[test]
fn every_request_gets_its_reply() {
    let speaker = "00000000-0000-0000-0000-000000000017";
    let nil = Uuid::nil().to_string();
    for (body, reply, calls) in [
        (
            json!({"id": "req-1", "method": "page.ready", "params": null}),
            json!({"id": "req-1"}),
            vec!["page.ready".to_owned()],
        ),
        (
            json!({"id": "req-2", "method": "settings.export.chooseVault", "params": null}),
            json!({"id": "req-2", "result": {"path": "/Users/nicolai/Notes"}}),
            vec!["settings.export.chooseVault".to_owned()],
        ),
        (
            json!({"id": "req-3", "method": "speakers.options",
                   "params": {"speakerID": speaker, "query": "an"}}),
            json!({"id": "req-3", "result": {"options": [{"kind": "create", "label": "Add \u{201c}an\u{201d}"}]}}),
            vec!["speakers.options an".to_owned()],
        ),
        (
            json!({"id": "r", "method": "meetings.select",
                   "params": {"meetingID": "00000000-0000-0000-0000-00000000000c"}}),
            json!({"id": "r"}),
            vec!["meetings.select 00000000-0000-0000-0000-00000000000C".to_owned()],
        ),
        (
            json!({"id": "r", "method": "meetings.select", "params": {"meetingID": nil}}),
            json!({"id": "r", "error": {"code": "notFound", "message": "No meeting with that id."}}),
            vec![format!("meetings.select {nil}")],
        ),
        (
            json!({"id": "r2", "method": "ui.confirmDestructive",
                   "params": {"title": "Delete?", "message": "Gone.", "confirmTitle": "Delete"}}),
            json!({"id": "r2", "error": {"code": "cancelled", "message": "The user cancelled."}}),
            vec!["ui.confirmDestructive Delete?".to_owned()],
        ),
        (
            json!({"id": "r", "method": "meetings.explode", "params": null}),
            json!({"id": "r", "error": {"code": "unknownMethod", "message": "Unknown method 'meetings.explode'."}}),
            vec![],
        ),
        (
            json!({"id": "r", "method": "recording.stop"}),
            json!({"id": "r", "error": {"code": "unknownMethod", "message": "The host does not answer recording.stop."}}),
            vec![],
        ),
        (
            json!([1, 2]),
            json!({"id": "", "error": {"code": "invalidParams", "message": "The message is not an object."}}),
            vec![],
        ),
        (
            json!({"id": "r", "params": {}}),
            json!({"id": "r", "error": {"code": "invalidParams", "message": "The message is not a bridge request."}}),
            vec![],
        ),
        (
            json!({"id": 7, "method": "page.ready"}),
            json!({"id": "", "error": {"code": "invalidParams", "message": "The message is not a bridge request."}}),
            vec![],
        ),
    ] {
        let (host, dispatcher) = dispatcher();
        assert_eq!(call(&dispatcher, &body), reply, "{body}");
        assert_eq!(host.calls(), calls, "{body}");
    }
    let (_, dispatcher) = dispatcher();
    let garbage: Value = serde_json::from_str(&dispatcher.dispatch_json("{not json")).unwrap();
    assert_eq!(
        garbage,
        json!({"id": "", "error": {"code": "invalidParams", "message": "The message is not JSON."}})
    );
    // A host behind an `Arc<dyn BridgeHost>` is a host too.
    let erased: Arc<dyn BridgeHost> = Arc::new(SpyHost::default());
    assert_eq!(
        call(
            &Dispatcher::new(erased),
            &json!({"id": "r", "method": "page.ready"})
        ),
        json!({"id": "r"})
    );
}

/// "Process again" takes no params, and its request envelope prints back
/// byte for byte.
#[test]
fn process_again_routes_without_params() {
    let (host, dispatcher) = dispatcher();
    let request = "{\n  \"id\" : \"req-9\",\n  \"method\" : \"meeting.processAgain\"\n}";
    let decoded: BridgeRequest = serde_json::from_str(request).unwrap();
    assert_eq!(decoded.method, BridgeMethod::MeetingProcessAgain);
    assert_eq!(json::to_canonical_string(&decoded).unwrap(), request);
    assert_eq!(
        call(&dispatcher, &serde_json::from_str(request).unwrap()),
        json!({"id": "req-9"})
    );
    assert_eq!(host.calls(), ["meeting.processAgain"]);
}

#[test]
fn missing_or_bad_params_are_invalid_params() {
    let (host, dispatcher) = dispatcher();
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
        host.calls().is_empty(),
        "the host was called with bad params"
    );
}

/// The Tauri command's path: a method and its params, no envelope; the
/// outcome is the `result` value or `None`, and the error keeps its code.
#[test]
fn call_answers_a_method_without_an_envelope() {
    let (host, dispatcher) = dispatcher();
    assert_eq!(
        dispatcher
            .call(BridgeMethod::SettingsExportChooseVault, None)
            .unwrap(),
        Some(json!({"path": "/Users/nicolai/Notes"}))
    );
    assert_eq!(
        dispatcher
            .call(BridgeMethod::PageReady, Some(Value::Null))
            .unwrap(),
        None
    );
    let error = dispatcher
        .call(BridgeMethod::MeetingsSelect, None)
        .unwrap_err();
    assert_eq!(error.code, BridgeErrorCode::InvalidParams);
    assert_eq!(error.message, "meetings.select needs params.");
    assert_eq!(host.calls(), ["settings.export.chooseVault", "page.ready"]);
}

#[test]
fn replies_are_compact_with_sorted_keys() {
    let (_, dispatcher) = dispatcher();
    let nil = Uuid::nil().to_string();
    let body =
        json!({"id": "r", "method": "meetings.select", "params": {"meetingID": nil}}).to_string();
    assert_eq!(
        dispatcher.dispatch_json(&body),
        r#"{"error":{"code":"notFound","message":"No meeting with that id."},"id":"r"}"#
    );
}

#[derive(Default)]
struct SpySink {
    events: Mutex<Vec<BridgeEvent>>,
}

impl EventSink for SpySink {
    fn emit(&self, event: BridgeEvent) {
        self.events.lock().unwrap().push(event);
    }
}

#[test]
fn an_event_sink_publishes_snapshots_on_their_topic() {
    let sink = SpySink::default();
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
