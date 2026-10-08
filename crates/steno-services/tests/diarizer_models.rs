//! A meeting processed while the diarizer's models are missing, through the
//! diarizer the app builds (`steno_services::speech::diarizer`) over a
//! loopback mirror: a download cut off mid-file, then refused, fails the
//! job in `diarize` and loses nothing. The recording's files stay byte for
//! byte, the asset gets no expiry, so no retention sweep removes them, and
//! the cut-off download stays as a partial for the next attempt. Processing
//! the meeting again tries the download again rather than repeating a
//! cached failure. The speech engine is core's fake. No network beyond
//! 127.0.0.1.
//!
//! With `STENO_MODEL_TESTS=1` and `STENO_MODELS_DIR` holding
//! `onnx/diarization/`, the mirror then serves the real files and the job
//! run again resumes the partial and ends `ready`.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{Duration, Utc};
use steno_audio::SymphoniaAudioCodec;
use steno_core::testing::{FakeDestination, FakeSpeechEngine};
use steno_core::{
    AudioAsset, AudioRetention, Destination, Meeting, MeetingSource, MeetingState, PipelineStage,
    StenoPaths, Store, TitleOrigin,
};
use steno_diarize::models::{self, SEGMENTATION_FILE};
use steno_pipeline::{
    MeetingEventBus, PipelineDependencies, ProcessingPipeline, RetentionSweep, StoreSpeakerMemory,
};
use steno_services::speech::SpeechSetup;
use steno_speech::ModelStore;

/// The body bytes the first response sends before the mirror cuts it.
const CUT_AFTER: usize = 256 * 1024;

/// One request the mirror saw: its path and `Range` header.
type Seen = (String, Option<String>);

/// A loopback mirror. Until [`Mirror::recover`] it cuts the first
/// response off after [`CUT_AFTER`] bytes (a dropped connection
/// mid-download) and answers 404 to every request after it (the mirror is
/// gone). Then it serves `<served>/<path>`, honouring `Range`.
struct Mirror {
    url: String,
    seen: Arc<Mutex<Vec<Seen>>>,
    healthy: Arc<AtomicBool>,
}

impl Mirror {
    fn start(served: Option<PathBuf>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let healthy = Arc::new(AtomicBool::new(false));
        let (log, state) = (Arc::clone(&seen), Arc::clone(&healthy));
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let _ = answer(&stream, served.as_deref(), &log, &state);
            }
        });
        Mirror { url, seen, healthy }
    }

    fn recover(&self) {
        self.healthy.store(true, Ordering::SeqCst);
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

fn answer(
    stream: &TcpStream,
    served: Option<&Path>,
    log: &Mutex<Vec<Seen>>,
    healthy: &AtomicBool,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let path = line
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .trim_start_matches('/')
        .to_owned();
    let mut range = None;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 || header == "\r\n" {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("range")
        {
            range = Some(value.trim().to_owned());
        }
    }
    let first = {
        let mut log = log.lock().unwrap();
        log.push((path.clone(), range.clone()));
        log.len() == 1
    };
    let mut out = stream;
    let not_found = b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    let healthy = healthy.load(Ordering::SeqCst);
    if !healthy && !first {
        return out.write_all(not_found);
    }
    let bytes = served.and_then(|root| std::fs::read(root.join(&path)).ok());
    if !healthy {
        // The size the manifest declares, so the store takes the cut for
        // a dropped connection, not for a short file.
        let size = models::asset()
            .files
            .iter()
            .find(|file| path.ends_with(&file.name))
            .map_or(CUT_AFTER * 2, |file| usize::try_from(file.size).unwrap());
        let body = bytes.unwrap_or_else(|| vec![0; size]);
        write!(
            out,
            "HTTP/1.1 200 OK\r\nContent-Length: {size}\r\nConnection: close\r\n\r\n"
        )?;
        out.write_all(&body[..CUT_AFTER])?;
        out.flush()?;
        return stream.shutdown(Shutdown::Both);
    }
    let Some(body) = bytes else {
        return out.write_all(not_found);
    };
    let start = range
        .as_deref()
        .and_then(|r| r.strip_prefix("bytes=")?.strip_suffix('-')?.parse().ok());
    match start {
        Some(start) if start < body.len() => {
            write!(
                out,
                "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len() - 1,
                body.len(),
                body.len() - start
            )?;
            out.write_all(&body[start..])
        }
        _ => {
            write!(
                out,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )?;
            out.write_all(&body)
        }
    }
}

/// Every file under `directory` with its bytes.
fn contents(directory: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(&next).unwrap().flatten() {
            if entry.path().is_dir() {
                pending.push(entry.path());
            } else {
                files.insert(entry.path(), std::fs::read(entry.path()).unwrap());
            }
        }
    }
    files
}

/// The store, the pipeline over the app's diarizer with the mirror, the
/// recording's folder and its asset, and the model store the diarizer
/// installs into, in an empty models directory.
struct World {
    store: Arc<Store>,
    pipeline: ProcessingPipeline,
    audio: PathBuf,
    asset: AudioAsset,
    models: ModelStore,
    _dir: tempfile::TempDir,
}

/// A six-second call, enqueued, whose audio goes as soon as the meeting is
/// processed: the retention that would lose the recording if a failure
/// were taken for the end of processing.
fn world(mirror: &Mirror) -> World {
    let dir = tempfile::tempdir().unwrap();
    let models_directory = dir.path().join("models");
    let store = Arc::new(Store::open(dir.path().join("steno.sqlite")).unwrap());
    let audio = dir.path().join("audio");
    let mut setup =
        SpeechSetup::in_models_directory(models_directory, &StenoPaths::new(dir.path()));
    setup.speech_settings.models_mirror = Some(mirror.url.clone());
    let models = setup.model_store();
    let vault = dir.path().join("vault");
    let destination: Arc<dyn Destination> = Arc::new(FakeDestination::new(&vault));
    let dispatcher = Arc::new(steno_adapters::DeliveryCoordinator::with_destinations(
        store.clone(),
        Box::new(move |_| vec![destination.clone()]),
        Box::new(Utc::now),
    ));
    let pipeline = ProcessingPipeline::new(PipelineDependencies::new(
        Arc::new(SymphoniaAudioCodec::new()),
        Arc::new(FakeSpeechEngine::default()),
        steno_services::speech::diarizer(&setup),
        Arc::new(StoreSpeakerMemory::new(store.clone())),
        dispatcher,
        store.clone(),
        MeetingEventBus::new(),
    ));
    let meeting_id = uuid::Uuid::new_v4();
    let now = Utc::now();
    let meeting = Meeting {
        id: meeting_id,
        title: "conversation".to_owned(),
        started_at: now - Duration::seconds(6),
        duration: 6.0,
        language: None,
        source: MeetingSource::MacCall,
        calendar_event_id: None,
        tags: Vec::new(),
        state: MeetingState::Queued,
        end_reason: None,
        title_origin: TitleOrigin::Default,
        template_id: Meeting::DEFAULT_TEMPLATE_ID.to_owned(),
        summary: None,
        scratchpad: String::new(),
        llm_usage: None,
        created_at: now,
        updated_at: now,
    };
    let asset = steno_pipeline::fixtures::two_lane_call(
        &audio,
        meeting_id,
        AudioRetention::DeleteAfterProcessing,
    )
    .unwrap();
    pipeline.enqueue(&meeting, &asset).unwrap();
    World {
        store,
        pipeline,
        audio,
        asset,
        models,
        _dir: dir,
    }
}

impl World {
    fn state(&self) -> MeetingState {
        self.store
            .meeting(self.asset.meeting_id)
            .unwrap()
            .unwrap()
            .state
    }

    /// Nothing of the recording is gone or changed, and nothing marks it
    /// for deletion: the asset has no expiry, and a sweep a year from now
    /// removes nothing.
    fn assert_recording_kept(&self, before: &BTreeMap<PathBuf, Vec<u8>>) {
        assert_eq!(&contents(&self.audio), before, "the recording's files");
        let asset = self.store.asset_by_id(self.asset.id).unwrap().unwrap();
        assert_eq!(asset.expires_at, None);
        assert_eq!(asset.retention, AudioRetention::DeleteAfterProcessing);
        let removed = RetentionSweep::new(self.store.clone())
            .run(Utc::now() + Duration::days(365))
            .unwrap();
        assert!(removed.is_empty(), "{removed:?}");
    }

    /// The first job, against a mirror that cuts the first download off
    /// and then refuses: it fails in `diarize`, naming the file it could
    /// not fetch, keeps the recording and leaves the cut-off download's
    /// bytes for the next attempt. Returns the recording as it was.
    async fn fail_on_the_cut_off_download(&self, mirror: &Mirror) -> BTreeMap<PathBuf, Vec<u8>> {
        let before = contents(&self.audio);
        assert!(!before.is_empty());
        self.pipeline.wait_until_idle().await;
        let MeetingState::Failed { reason } = self.state() else {
            panic!("the job failed: {:?}", self.state());
        };
        let url = format!("{}/diarization/{SEGMENTATION_FILE}", mirror.url);
        assert!(
            reason.starts_with("diarize: ") && reason.contains(&url),
            "{reason}"
        );
        self.assert_recording_kept(&before);
        let asset = models::asset();
        assert!(!self.models.is_installed(&asset));
        let folder = self.models.directory(&asset);
        assert!(!folder.join(SEGMENTATION_FILE).exists());
        assert_eq!(
            std::fs::metadata(folder.join(format!("{SEGMENTATION_FILE}.partial")))
                .unwrap()
                .len(),
            CUT_AFTER as u64,
            "the cut-off download is kept to resume"
        );
        // The cut, then the resume the store tries at once, refused.
        assert_eq!(mirror.seen().len(), 2, "{:?}", mirror.seen());
        before
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_diarizer_download_cut_off_mid_job_fails_the_meeting_and_keeps_the_recording() {
    let mirror = Mirror::start(None);
    let world = world(&mirror);
    let before = world.fail_on_the_cut_off_download(&mirror).await;

    // Processing the meeting again asks the mirror again: the failed load
    // is not cached, and the recording is still all there.
    let failure = world.pipeline.process(world.asset.id).await.unwrap_err();
    assert_eq!(failure.stage, PipelineStage::Diarize, "{failure}");
    assert!(matches!(world.state(), MeetingState::Failed { .. }));
    let seen = mirror.seen();
    assert_eq!(seen.len(), 3, "{seen:?}");
    assert_eq!(
        seen[2].1.as_deref(),
        Some(format!("bytes={CUT_AFTER}-").as_str()),
        "the download resumes where it was cut"
    );
    world.assert_recording_kept(&before);
}

/// The same job, then the mirror serves the real files: processing the
/// meeting again resumes the cut-off download and the meeting ends
/// `ready`.
#[tokio::test(flavor = "multi_thread")]
async fn once_the_models_arrive_the_failed_meeting_is_processed_again() {
    if std::env::var("STENO_MODEL_TESTS").as_deref() != Ok("1") {
        eprintln!(
            "set STENO_MODEL_TESTS=1 and STENO_MODELS_DIR to resume the diarizer's models from a mirror over the real files"
        );
        return;
    }
    let installed = ModelStore::from_environment();
    let asset = models::asset();
    installed
        .verify(&asset)
        .expect("STENO_MODELS_DIR holds onnx/diarization/");
    let mirror = Mirror::start(Some(installed.root().to_path_buf()));
    let world = world(&mirror);
    world.fail_on_the_cut_off_download(&mirror).await;

    mirror.recover();
    world.pipeline.process(world.asset.id).await.unwrap();
    assert_eq!(world.state(), MeetingState::Ready);
    world.models.verify(&asset).unwrap();
    let seen = mirror.seen();
    assert_eq!(
        seen[2],
        (
            format!("diarization/{SEGMENTATION_FILE}"),
            Some(format!("bytes={CUT_AFTER}-"))
        ),
        "{seen:?}"
    );
    assert!(
        world
            .models
            .directory(&asset)
            .read_dir()
            .unwrap()
            .flatten()
            .all(|entry| !entry.file_name().to_string_lossy().ends_with(".partial")),
        "no partial is left once installed"
    );
}
