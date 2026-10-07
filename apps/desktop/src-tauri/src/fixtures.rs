//! The recorded bridge fixtures (`apps/macos/web/fixtures/bridge/`), embedded
//! with `include_str!` in `index.json` order, and the mock transport's rules
//! for serving them: topic files become snapshots, `<method>.reply.json` and
//! the two generic reply shapes answer commands, everything else is `null`.
//! Temporary: WP6 wires the real host and deletes this module with the
//! `fixture-host` feature.

use serde_json::Value;

/// Embeds every listed fixture as a `(key, json)` pair; the test below keeps
/// the list equal to `index.json`.
macro_rules! fixtures {
    ($($key:literal),* $(,)?) => {
        /// Every recorded fixture by key, in `index.json` order.
        pub static FIXTURES: &[(&str, &str)] = &[
            $(($key, include_str!(concat!("../../../macos/web/fixtures/bridge/", $key, ".json"))),)*
        ];
    };
}

fixtures![
    "app",
    "recording",
    "recording.live",
    "progress",
    "meetings.list",
    "meeting.detail",
    "settings.general",
    "settings.recording",
    "settings.transcription",
    "settings.summaries",
    "settings.summaries.codex",
    "settings.summaries.fileKey",
    "settings.export",
    "settings.iphone",
    "settings.iphone.pairing",
    "onboarding",
    "onboarding.setup",
    "envelope.request",
    "envelope.reply",
    "envelope.error",
    "envelope.event",
    "speakers.options.reply",
    "params.page.layout",
    "params.meetings.setFilter",
    "params.meetings.setTagFilter",
    "params.meetings.setQuery",
    "params.meetingID",
    "params.meeting.setTab",
    "params.meeting.setTags",
    "params.meeting.setTemplate",
    "params.bool",
    "params.string",
    "params.meeting.saveNotes",
    "params.speakers.options",
    "params.speakers.select",
    "params.speakerID",
    "params.recording.start",
    "params.settings.recording.setRetention",
    "params.permissionKind",
    "params.assetID",
    "params.settings.general.setAutomaticUpdates",
    "params.settings.summaries.update",
    "params.settings.export.update",
    "params.deviceID",
    "params.onboarding.setupStep",
    "params.system.openURL",
    "params.window",
    "params.ui.confirmDestructive",
    "reply.confirm",
    "reply.chosenPath",
    "onboarding.import",
];

/// The contract's topics (`bridgeTopics` in `contract.ts`). The other
/// snapshot-shaped fixtures (`recording.live`, `onboarding.setup`, ...) feed
/// the mock's scenarios and are never emitted under their own names.
pub const TOPICS: [&str; 12] = [
    "app",
    "recording",
    "progress",
    "meetings.list",
    "meeting.detail",
    "settings.general",
    "settings.recording",
    "settings.transcription",
    "settings.summaries",
    "settings.export",
    "settings.iphone",
    "onboarding",
];

/// The generic reply shapes and the methods they answer, as
/// `REPLY_ALIASES` in `mock-transport.ts`: the native alerts and folder
/// panels resolve as confirmed, with the recorded path.
const REPLY_ALIASES: &[(&str, &[&str])] = &[
    (
        "reply.confirm",
        &[
            "ui.confirmDestructive",
            "meetings.delete",
            "meeting.deleteRecordingNow",
            "meeting.setKeepAudio",
        ],
    ),
    (
        "reply.chosenPath",
        &[
            "settings.recording.chooseFolder",
            "settings.export.chooseVault",
            "onboarding.chooseVault",
        ],
    ),
];

/// The recorded JSON for a fixture key.
pub fn fixture(key: &str) -> Option<&'static str> {
    FIXTURES
        .iter()
        .find(|(name, _)| *name == key)
        .map(|(_, json)| *json)
}

fn parsed(key: &str) -> Option<Value> {
    fixture(key).map(|json| serde_json::from_str(json).expect("a recorded fixture is valid JSON"))
}

/// Every topic's snapshot, in contract order.
pub fn snapshots() -> impl Iterator<Item = (&'static str, Value)> {
    TOPICS
        .iter()
        .filter_map(|topic| parsed(topic).map(|snapshot| (*topic, snapshot)))
}

/// The reply to a command: its own reply file, an aliased shape, or `null`.
pub fn reply(method: &str) -> Value {
    if let Some(direct) = parsed(&format!("{method}.reply")) {
        return direct;
    }
    REPLY_ALIASES
        .iter()
        .find(|(_, methods)| methods.contains(&method))
        .and_then(|(shape, _)| parsed(shape))
        .unwrap_or(Value::Null)
}

/// The two `app` snapshots a deep link takes: the recorded one with the
/// request field set (`requestedMeetingID` or `requestedSettingsSection`),
/// which the page follows, then the recorded one unchanged, as the Swift
/// host clears the request with the publish that carried it and publishes
/// again.
pub fn app_snapshots_requesting(field: &str, value: &str) -> Vec<Value> {
    let Some(clean) = parsed("app") else {
        return Vec::new();
    };
    let mut requesting = clean.clone();
    requesting[field] = Value::String(value.to_owned());
    vec![requesting, clean]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_topic_has_a_recorded_snapshot() {
        let emitted: Vec<&str> = snapshots().map(|(topic, _)| topic).collect();
        assert_eq!(emitted, TOPICS);
    }

    #[test]
    fn the_table_is_index_json() {
        let index: Vec<String> = serde_json::from_str(include_str!(
            "../../../macos/web/fixtures/bridge/index.json"
        ))
        .expect("index.json is a list of keys");
        let keys: Vec<&str> = FIXTURES.iter().map(|(key, _)| *key).collect();
        assert_eq!(keys, index);
    }

    #[test]
    fn every_fixture_parses() {
        for (key, json) in FIXTURES {
            let value: Value = serde_json::from_str(json).unwrap_or_else(|e| panic!("{key}: {e}"));
            assert!(
                value.is_object() || value.is_array(),
                "{key} is not a JSON document"
            );
        }
    }

    #[test]
    fn scenario_fixtures_are_not_topics() {
        for key in [
            "recording.live",
            "settings.summaries.codex",
            "settings.summaries.fileKey",
            "settings.iphone.pairing",
            "onboarding.setup",
            "onboarding.import",
        ] {
            assert!(fixture(key).is_some(), "{key} is recorded");
            assert!(
                !TOPICS.contains(&key),
                "{key} must not be emitted as a topic"
            );
        }
    }

    #[test]
    fn replies_follow_the_mock_transport() {
        assert!(
            reply("speakers.options").is_object(),
            "the recorded speakers.options reply"
        );
        assert_eq!(
            reply("ui.confirmDestructive")["confirmed"],
            Value::Bool(true)
        );
        assert_eq!(reply("meetings.delete")["confirmed"], Value::Bool(true));
        assert_eq!(
            reply("onboarding.chooseVault")["path"],
            Value::String("/Users/nicolai/Notes".into())
        );
        assert_eq!(reply("recording.start"), Value::Null);
        assert_eq!(reply("not.a.method"), Value::Null);
    }

    #[test]
    fn a_deep_link_rides_on_the_app_snapshot_and_is_cleared_right_after() {
        let snapshots = app_snapshots_requesting("requestedSettingsSection", "summaries");
        let [requesting, clean] = snapshots.as_slice() else {
            panic!("two app snapshots, got {}", snapshots.len());
        };
        assert_eq!(requesting["requestedSettingsSection"], "summaries");
        assert_eq!(requesting["version"], clean["version"]);
        assert!(clean.get("requestedSettingsSection").is_none());
        assert_eq!(*clean, parsed("app").unwrap());
        let meeting = app_snapshots_requesting("requestedMeetingID", "m-1");
        assert_eq!(meeting.len(), 2);
        assert_eq!(meeting[0]["requestedMeetingID"], "m-1");
        assert!(meeting[1].get("requestedMeetingID").is_none());
    }
}
