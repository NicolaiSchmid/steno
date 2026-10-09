//! What the tests without a shell share, after
//! `apps/macos/StenoTests/BridgeTestSupport.swift` and
//! `apps/macos/StenoTests/TestSupport.swift`: a host over a temporary
//! database and the fakes, a sink that records what the host publishes,
//! the sample meeting the fixtures describe, and the request shapes.

#![allow(dead_code, clippy::too_many_arguments, clippy::too_many_lines)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use serde_json::Value;
use steno_bridge::{
    BridgeEvent, BridgeHost, BridgeTopic, ConfirmDestructiveParams, EventSink, Platform,
};
use steno_core::paths::{file_url, file_url_path};
use steno_core::*;
use steno_host::fakes::FakeServices;
use steno_host::{Host, HostConfig};
use uuid::Uuid;

/// 2026-09-29 14:50 in Europe/Berlin: the fixtures' `startedAt`.
pub const NOW: &str = "2026-09-29T12:50:00.000Z";
pub const VERSION: &str = "0.10.0";

pub fn date(text: &str) -> DateTime<Utc> {
    steno_core::json::parse_date(text).unwrap()
}

/// `00000000-0000-0000-0000-0000000000NN`. Swift: `SampleData.uuid`.
pub fn uuid(n: u32) -> Uuid {
    Uuid::parse_str(&format!("00000000-0000-0000-0000-{n:012X}")).unwrap()
}

pub fn now() -> DateTime<Utc> {
    date(NOW)
}

/// Records every event the host publishes. Swift: `RecordingSink`.
#[derive(Default)]
pub struct RecordingSink {
    pub events: Mutex<Vec<BridgeEvent>>,
}

impl RecordingSink {
    pub fn topics(&self) -> Vec<BridgeTopic> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .map(|event| event.topic)
            .collect()
    }

    pub fn last(&self, topic: BridgeTopic) -> Option<Value> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|event| event.topic == topic)
            .map(|event| event.payload.clone())
    }

    pub fn all(&self, topic: BridgeTopic) -> Vec<Value> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| event.topic == topic)
            .map(|event| event.payload.clone())
            .collect()
    }

    pub fn clear(&self) {
        self.events.lock().unwrap().clear();
    }

    pub fn count(&self, topic: BridgeTopic) -> usize {
        self.all(topic).len()
    }
}

impl EventSink for RecordingSink {
    fn emit(&self, event: BridgeEvent) {
        self.events.lock().unwrap().push(event);
    }
}

/// The host over a temporary on-disk store with every fake, a recording
/// sink attached and `page.ready` sent, so each command's publishes are
/// on the sink. Swift: `TestSupport.environment()` plus the bridge under test.
pub struct Harness {
    pub dir: tempfile::TempDir,
    pub store: Arc<Store>,
    pub fakes: FakeServices,
    pub host: Host,
    pub sink: Arc<RecordingSink>,
    /// The host as the `confirm_with` prompt sees it; emptied on drop, so
    /// the prompt's handle does not keep the host (and its flush thread)
    /// alive past the test.
    prompt_host: Arc<Mutex<Option<Host>>>,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.prompt_host.lock().unwrap().take();
    }
}

type Seed = Box<dyn FnOnce(&Store, &FakeServices)>;

type ChangeServices = Box<dyn FnOnce(&mut steno_host::services::Services)>;

/// A destructive prompt that sees the host while it is up.
type Prompt = Box<dyn Fn(&Host, &ConfirmDestructiveParams) -> bool + Send + Sync>;

pub struct HarnessBuilder {
    fakes: FakeServices,
    confirm: bool,
    prompt: Option<Prompt>,
    chosen: Option<PathBuf>,
    seed: Vec<Seed>,
    change_services: Vec<ChangeServices>,
    page_ready: bool,
    platform: Platform,
    updates_managed: bool,
}

impl HarnessBuilder {
    pub fn with_handover(mut self, mac_id: &str, port: u16) -> Self {
        self.fakes = self.fakes.with_handover(mac_id, port);
        self
    }

    /// Adds a pending Swift import whose step brings up `prompts` prompts.
    pub fn with_swift_import(mut self, prompts: u8) -> Self {
        self.fakes.swift_import = Some(Arc::new(steno_host::fakes::FakeSwiftImport::new(prompts)));
        self
    }

    /// Adds a gate over the API key that withholds it while `withheld`.
    pub fn with_api_key_gate(mut self, withheld: bool) -> Self {
        self.fakes.api_key_gate = Some(Arc::new(steno_host::fakes::FakeApiKeyGate {
            withheld: std::sync::Mutex::new(withheld),
        }));
        self
    }

    /// What the destructive prompt answers.
    pub fn confirm(mut self, confirmed: bool) -> Self {
        self.confirm = confirmed;
        self
    }

    /// The destructive prompt runs `prompt`, which gets the host as it
    /// stands while the prompt is up and answers for the user.
    pub fn confirm_with(
        mut self,
        prompt: impl Fn(&Host, &ConfirmDestructiveParams) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.prompt = Some(Box::new(prompt));
        self
    }

    /// What the folder chooser returns; `None` cancels.
    pub fn choose(mut self, path: Option<&str>) -> Self {
        self.chosen = path.map(PathBuf::from);
        self
    }

    /// Runs before the host is built: store rows and fake state.
    pub fn seed(mut self, seed: impl FnOnce(&Store, &FakeServices) + 'static) -> Self {
        self.seed.push(Box::new(seed));
        self
    }

    /// Changes the services the fakes give before the host is built (a
    /// secret store of the test's own over the fake one).
    pub fn change_services(
        mut self,
        change: impl FnOnce(&mut steno_host::services::Services) + 'static,
    ) -> Self {
        self.change_services.push(Box::new(change));
        self
    }

    /// The host runs on `platform`; the Mac (Swift's words) by default.
    pub fn platform(mut self, platform: Platform) -> Self {
        self.platform = platform;
        self
    }

    /// A package manager delivers the updates (`HostConfig::updates_managed`).
    pub fn updates_managed(mut self) -> Self {
        self.updates_managed = true;
        self
    }

    /// Leaves the page not ready, for tests of the readiness gate.
    pub fn without_page_ready(mut self) -> Self {
        self.page_ready = false;
        self
    }

    pub fn build(self) -> Harness {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path().join("steno.sqlite")).unwrap());
        // A known audio folder, as the Swift preview wrote one.
        let mut settings = store.settings().unwrap();
        settings.audio_folder = file_url(&dir.path().join("audio"), true);
        settings.launch_at_login = false;
        store.save_settings(&settings).unwrap();
        for seed in self.seed {
            seed(&store, &self.fakes);
        }
        let confirmed = self.confirm;
        let prompt = self.prompt;
        let prompt_host = Arc::new(Mutex::new(None::<Host>));
        let prompt_slot = prompt_host.clone();
        let chosen = self.chosen;
        let mut services = self.fakes.services();
        for change in self.change_services {
            change(&mut services);
        }
        let host = Host::new(
            store.clone(),
            services,
            HostConfig {
                version: VERSION.to_owned(),
                zone: steno_host::labels::utc(),
                platform: self.platform,
                updates_managed: self.updates_managed,
            },
        )
        .unwrap()
        .with_dialogs(
            Box::new(move |params| match &prompt {
                Some(prompt) => {
                    let host = prompt_slot.lock().unwrap().clone();
                    host.is_some_and(|host| prompt(&host, params))
                }
                None => confirmed,
            }),
            Box::new(move |_| chosen.clone()),
        );
        *prompt_host.lock().unwrap() = Some(host.clone());
        let sink = Arc::new(RecordingSink::default());
        host.attach(sink.clone());
        if self.page_ready {
            host.page_ready().unwrap();
            sink.clear();
        }
        Harness {
            dir,
            store,
            fakes: self.fakes,
            host,
            sink,
            prompt_host,
        }
    }
}

impl Harness {
    pub fn builder() -> HarnessBuilder {
        HarnessBuilder {
            fakes: FakeServices::new(now()),
            confirm: true,
            prompt: None,
            chosen: None,
            seed: Vec::new(),
            change_services: Vec::new(),
            page_ready: true,
            platform: Platform::Macos,
            updates_managed: false,
        }
    }

    /// The topic's snapshot as the host builds it now.
    pub fn snapshot(&self, topic: BridgeTopic) -> Value {
        self.host.snapshot(topic).unwrap()
    }

    /// Polls until `condition` holds, for what the host finishes on another
    /// thread (a throttled publish, a download); panics after five seconds.
    pub fn wait_for(&self, what: &str, condition: impl Fn(&Harness) -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !condition(self) {
            assert!(
                std::time::Instant::now() < deadline,
                "waited five seconds for {what}"
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    /// Waits until the asset's `settings.transcription` state is final.
    pub fn wait_for_download(&self, asset_index: usize) {
        self.wait_for("the download to end", |harness| {
            harness
                .sink
                .last(BridgeTopic::SettingsTranscription)
                .is_some_and(|snapshot| {
                    matches!(
                        snapshot["assets"][asset_index]["state"].as_str(),
                        Some("installed" | "failed")
                    )
                })
        });
    }

    /// The audio folder the settings point at.
    pub fn audio_folder(&self) -> PathBuf {
        self.dir.path().join("audio")
    }
}

/// Runs `call` on its own thread and returns what it returned, failing the
/// test after five seconds instead of hanging it: for a call that must not
/// wait on something the test still holds (a gated download, a sink that
/// reads the host back).
pub fn within_five_seconds<R: Send + 'static>(
    what: &str,
    call: impl FnOnce() -> R + Send + 'static,
) -> R {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(call());
    });
    receiver
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap_or_else(|_| panic!("{what} did not return within five seconds"))
}

// The sample meeting the bridge fixtures describe
// (`Sources/StenoBridge/BridgeSamples.swift`),
// as rows the pipeline would have left: three persons, the ready meeting
// with its asset, four speakers, three turns, two tasks and a decision, an
// in-person meeting without a summary and a failed call.

pub const MEETING: u32 = 0x01;
pub const MEETING_IN_PERSON: u32 = 0x03;
pub const MEETING_FAILED: u32 = 0x44;
pub const PERSON_NICOLAI: u32 = 0x0A;
pub const PERSON_JEROME: u32 = 0x0B;
pub const PERSON_ANNA: u32 = 0x0C;
pub const SPEAKER_NICOLAI: u32 = 0x14;
pub const SPEAKER_JEROME: u32 = 0x15;
pub const SPEAKER_ANNA: u32 = 0x16;
pub const SPEAKER_UNKNOWN: u32 = 0x17;
pub const PHONE: u32 = 0x28;

pub fn person(n: u32, name: &str, email: Option<&str>, axis: usize) -> Person {
    let mut embedding = vec![0.0f32; Embedding::DIMENSION];
    embedding[axis] = 1.0;
    Person {
        id: uuid(n),
        display_name: name.to_owned(),
        email: email.map(str::to_owned),
        embedding: Some(Embedding(embedding)),
        sample_count: 1,
        created_at: date("2026-09-01T08:00:00.000Z"),
    }
}

pub fn sample_summary() -> SummaryDocument {
    SummaryDocument {
        template_id: "default".to_owned(),
        language: Some("de".into()),
        sections: vec![
            SummarySection {
                id: "executive-summary".to_owned(),
                heading: "Executive summary".to_owned(),
                bullets: vec![
                    SummaryBullet {
                        lead: "Fokus".to_owned(),
                        text: "Nicolai schlägt vor, 90 Prozent der Kapazität auf den Kern zu setzen und Nebenprojekte bis Q1 zu pausieren.".to_owned(),
                    },
                    SummaryBullet {
                        lead: "Budget".to_owned(),
                        text: "Jérôme prüft die Zahlen bis Freitag und bringt zwei Szenarien mit.".to_owned(),
                    },
                ],
            },
            SummarySection {
                id: "open-questions".to_owned(),
                heading: "Open questions".to_owned(),
                bullets: vec![SummaryBullet {
                    lead: "Zeitplan".to_owned(),
                    text: "Start im Oktober oder erst im November nach dem Investor-Update?".to_owned(),
                }],
            },
        ],
    }
}

pub fn meeting(
    n: u32,
    title: &str,
    origin: TitleOrigin,
    started_at: &str,
    duration: f64,
    source: MeetingSource,
    tags: &[&str],
    state: MeetingState,
    summary: Option<SummaryDocument>,
) -> Meeting {
    Meeting {
        id: uuid(n),
        title: title.to_owned(),
        started_at: date(started_at),
        duration,
        language: Some("de".into()),
        source,
        calendar_event_id: None,
        tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
        state,
        end_reason: None,
        title_origin: origin,
        template_id: "default".to_owned(),
        summary,
        scratchpad: String::new(),
        llm_usage: None,
        created_at: date(started_at),
        updated_at: date(started_at),
    }
}

pub fn sample_meeting() -> Meeting {
    meeting(
        MEETING,
        "Produktstrategie 90/10",
        TitleOrigin::Calendar,
        NOW,
        2738.0,
        MeetingSource::MacCall,
        &["strategie", "q4"],
        MeetingState::Ready,
        Some(sample_summary()),
    )
}

/// The master and the clips as paths under the harness's audio folder.
pub fn master_path(audio_folder: &std::path::Path) -> PathBuf {
    audio_folder
        .join(steno_core::json::uuid_string(uuid(MEETING)))
        .join("master.caf")
}

pub fn clip_path(audio_folder: &std::path::Path, speaker: u32) -> PathBuf {
    audio_folder
        .join(steno_core::json::uuid_string(uuid(MEETING)))
        .join("speakers")
        .join(format!(
            "{}.wav",
            steno_core::json::uuid_string(uuid(speaker))
        ))
}

fn speaker(
    n: u32,
    meeting_id: Uuid,
    label: &str,
    assignment: SpeakerAssignment,
    clip: Option<PathBuf>,
    axis: usize,
) -> Speaker {
    let mut embedding = vec![0.0f32; Embedding::DIMENSION];
    embedding[axis] = 1.0;
    Speaker {
        id: uuid(n),
        meeting_id,
        cluster_label: label.to_owned(),
        assignment,
        embedding: Some(Embedding(embedding)),
        sample_clip_range: Some(TimeRange {
            lower: 12.0,
            upper: 22.0,
        }),
        sample_clip_url: clip.map(|path| file_url(&path, false)),
        cluster_confidence: 0.9,
    }
}

fn segment(
    n: u32,
    meeting_id: Uuid,
    start: f64,
    end: f64,
    speaker: Option<u32>,
    lane: AudioLane,
    text: &str,
) -> TranscriptSegment {
    TranscriptSegment {
        id: uuid(n),
        meeting_id,
        start,
        end,
        speaker_id: speaker.map(uuid),
        lane,
        text: text.to_owned(),
        raw_text: text.to_lowercase(),
    }
}

/// Writes the sample into `store`, with the master and three clips under
/// the settings' audio folder marked as existing in the fake file system.
pub fn populate_sample(store: &Store, fakes: &FakeServices) {
    let audio_folder = &file_url_path(&store.settings().unwrap().audio_folder).unwrap();
    for person in [
        person(PERSON_NICOLAI, "Nicolai", Some("nicolai@example.com"), 0),
        person(PERSON_JEROME, "Jérôme", None, 1),
        person(PERSON_ANNA, "Anna", None, 2),
    ] {
        store.save_person(&person).unwrap();
    }
    let sample = sample_meeting();
    let master = master_path(audio_folder);
    let asset = AudioAsset {
        id: uuid(0x46),
        meeting_id: sample.id,
        url: file_url(&master, false),
        format: AudioFormat::Caf48kFloat32,
        lanes: vec![AudioLane::Mic, AudioLane::System],
        sidecars_16k: BTreeMap::new(),
        mixdown_url: None,
        retention: AudioRetention::KeepDays(30),
        expires_at: Some(date("2026-10-29T12:50:00.000Z")),
    };
    store.save_meeting_with_asset(&sample, &asset).unwrap();
    fakes.file_system.create(master);
    let clip = |n: u32| {
        let path = clip_path(audio_folder, n);
        fakes.file_system.create(path.clone());
        Some(path)
    };
    let speakers = vec![
        speaker(
            SPEAKER_NICOLAI,
            sample.id,
            "Speaker 1",
            SpeakerAssignment::Confirmed {
                person_id: uuid(PERSON_NICOLAI),
            },
            clip(SPEAKER_NICOLAI),
            0,
        ),
        speaker(
            SPEAKER_JEROME,
            sample.id,
            "Speaker 2",
            SpeakerAssignment::Confirmed {
                person_id: uuid(PERSON_JEROME),
            },
            clip(SPEAKER_JEROME),
            1,
        ),
        speaker(
            SPEAKER_ANNA,
            sample.id,
            "Speaker 3",
            SpeakerAssignment::Suggested {
                person_id: uuid(PERSON_ANNA),
                similarity: 0.87,
            },
            clip(SPEAKER_ANNA),
            2,
        ),
        speaker(
            SPEAKER_UNKNOWN,
            sample.id,
            "Speaker 4",
            SpeakerAssignment::Unknown,
            None,
            3,
        ),
    ];
    let segments = vec![
        segment(
            0x64,
            sample.id,
            12.0,
            41.0,
            Some(SPEAKER_NICOLAI),
            AudioLane::Mic,
            "Lass uns kurz auf die Prioritäten schauen. Ich würde vorschlagen, dass wir neunzig Prozent auf den Kern setzen.",
        ),
        segment(
            0x65,
            sample.id,
            41.0,
            65.0,
            Some(SPEAKER_JEROME),
            AudioLane::System,
            "Das geht nur, wenn wir das Budget entsprechend umschichten. Ich rechne bis Freitag zwei Szenarien.",
        ),
        segment(
            0x66,
            sample.id,
            65.0,
            90.0,
            Some(SPEAKER_UNKNOWN),
            AudioLane::System,
            "Die Partner sollten das nicht aus zweiter Hand hören.",
        ),
    ];
    store
        .replace_transcript(&sample, &segments, &speakers)
        .unwrap();
    let tasks = vec![
        MeetingTask {
            id: uuid(0xC8),
            meeting_id: sample.id,
            text: "Zahlen für beide Szenarien".to_owned(),
            assignee_person_id: Some(uuid(PERSON_JEROME)),
            assignee_name: Some("Jérôme".to_owned()),
            priority: TaskPriority::Normal,
            due_date: Some(date("2026-10-02T12:50:00.000Z")),
            done: false,
        },
        MeetingTask {
            id: uuid(0xC9),
            meeting_id: sample.id,
            text: "Entscheidung im Investor-Update ansprechen".to_owned(),
            assignee_person_id: Some(uuid(PERSON_NICOLAI)),
            assignee_name: Some("Nicolai".to_owned()),
            priority: TaskPriority::High,
            due_date: None,
            done: true,
        },
    ];
    store
        .replace_summary(
            &sample,
            &tasks,
            &["Wir setzen neunzig Prozent auf den Kern. Nebenprojekte pausieren bis zur Q1-Planung.".to_owned()],
            &[],
        )
        .unwrap();
    store
        .save_meeting(&meeting(
            MEETING_IN_PERSON,
            "Tuesday 10:08",
            TitleOrigin::User,
            "2026-09-29T08:06:40.000Z",
            2291.0,
            MeetingSource::MacInPerson,
            &[],
            MeetingState::Ready,
            None,
        ))
        .unwrap();
    store
        .save_meeting(&meeting(
            MEETING_FAILED,
            "Investor update prep",
            TitleOrigin::User,
            "2026-09-28T13:46:40.000Z",
            1650.0,
            MeetingSource::MacCall,
            &["investors"],
            MeetingState::Failed {
                reason: "Transcription failed: model not installed".to_owned(),
            },
            None,
        ))
        .unwrap();
}

/// The sample meeting as processed without the LLM passes: no summary, no
/// tasks, no decisions.
pub fn drop_sample_summary(store: &Store) {
    let mut bare = sample_meeting();
    bare.summary = None;
    store.replace_summary(&bare, &[], &[], &[]).unwrap();
}

/// The settings with an LM Studio endpoint configured, no vault.
pub fn configure_llm(store: &Store, model: &str) {
    let mut settings = store.settings().unwrap();
    settings.llm_base_url = Some("http://127.0.0.1:1234/v1".to_owned());
    settings.llm_model = Some(model.to_owned());
    store.save_settings(&settings).unwrap();
}

pub fn set_retention(store: &Store, retention: AudioRetention) {
    let mut settings = store.settings().unwrap();
    settings.default_retention = retention;
    store.save_settings(&settings).unwrap();
}

pub fn paired_phone() -> PairedDevice {
    PairedDevice {
        id: uuid(PHONE),
        name: "Nicolai's iPhone".to_owned(),
        paired_at: date("2026-09-24T12:50:00.000Z"),
        last_seen_at: Some(date("2026-09-29T12:48:00.000Z")),
    }
}
