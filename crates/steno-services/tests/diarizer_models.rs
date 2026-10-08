//! A meeting processed while the diarizer's models are missing, through the
//! diarizer the app builds (`steno_services::speech::diarizer`) over a
//! loopback mirror. The speech engine is core's fake. No network beyond
//! 127.0.0.1.
//!
//! - Under `Install::Allowed` (what `steno process` builds, and
//!   `SpeechEngines` until the pipeline gains its models-missing gate), a
//!   download cut off mid-file, then refused, ends the job `ready` with the
//!   one room speaker. The recording's files stay byte for byte, the
//!   cut-off download stays as a partial in `onnx/diarization/`, and
//!   processing the meeting again resumes it rather than repeating a
//!   cached failure.
//! - Under `Install::Never`, with today's pipeline, the job falls back
//!   the same way and the mirror sees no request; files of the right size
//!   in the folder Settings installs that fail to load and fail their
//!   checksum are deleted, so Settings offers Download.
//!
//! The test's recordings are kept forever (`KeepForever`). Under
//! `DeleteAfterProcessing`, a meeting that ends `ready` without its
//! speakers loses its audio, and the speakers can never be computed again;
//! deferring that meeting's retention is the pipeline's change (item P14
//! of `.plans/2026-10-07-stable-promotion.md`), not this crate's.
//!
//! With `STENO_MODEL_TESTS=1` and `STENO_MODELS_DIR` holding
//! `onnx/diarization/`, the mirror then serves the real files: the job run
//! again resumes the partial and diarizes, and the pipeline's diarizer
//! under `Install::Never` loads what Settings downloaded without a request.

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
    AudioAsset, AudioRetention, Destination, Meeting, MeetingSource, MeetingState, StenoPaths,
    Store, TitleOrigin,
};
use steno_diarize::Install;
use steno_diarize::models::{self, ModelPaths, SEGMENTATION_FILE};
use steno_host::services::SpeechModels;
use steno_host::speech::ModelAsset;
use steno_pipeline::{
    MeetingEventBus, PipelineDependencies, ProcessingPipeline, RetentionSweep, StoreSpeakerMemory,
};
use steno_services::speech::{ModelStoreSpeechModels, SpeechSetup};
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

/// The store, the pipeline over the app's diarizer with the mirror and
/// `install`, the recording's folder and its asset, the setup and the
/// model store the diarizer reads, in an empty models directory. The
/// meeting is enqueued by [`World::start`].
struct World {
    store: Arc<Store>,
    pipeline: ProcessingPipeline,
    meeting: Meeting,
    audio: PathBuf,
    asset: AudioAsset,
    setup: SpeechSetup,
    models: ModelStore,
    _dir: tempfile::TempDir,
}

/// A six-second call, kept forever (see the module doc).
fn world(mirror: &Mirror, install: Install) -> World {
    let dir = tempfile::tempdir().unwrap();
    let models_directory = dir.path().join("models");
    let store = Arc::new(Store::open(dir.path().join("steno.sqlite")).unwrap());
    let audio = dir.path().join("audio");
    let mut setup =
        SpeechSetup::in_models_directory(models_directory.clone(), &StenoPaths::new(dir.path()));
    setup.speech_settings.models_mirror = Some(mirror.url.clone());
    let models = setup.model_store();
    assert_eq!(
        models.directory(&models::asset()),
        models_directory.join("onnx").join("diarization")
    );
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
        steno_services::speech::diarizer(&setup, install),
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
    let asset =
        steno_pipeline::fixtures::two_lane_call(&audio, meeting_id, AudioRetention::KeepForever)
            .unwrap();
    World {
        store,
        pipeline,
        meeting,
        audio,
        asset,
        setup,
        models,
        _dir: dir,
    }
}

impl World {
    /// Enqueues the meeting and waits for its job; the recording as it
    /// was before.
    async fn run_the_job(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        let before = contents(&self.audio);
        assert!(!before.is_empty());
        self.pipeline.enqueue(&self.meeting, &self.asset).unwrap();
        self.pipeline.wait_until_idle().await;
        before
    }

    fn state(&self) -> MeetingState {
        self.store
            .meeting(self.asset.meeting_id)
            .unwrap()
            .unwrap()
            .state
    }

    /// The meeting is `ready` with the fallback's speakers: `Me` and the
    /// one room speaker, nothing the diarizer found.
    fn assert_ready_with_the_room_speaker(&self) {
        assert_eq!(self.state(), MeetingState::Ready);
        let mut labels: Vec<String> = self
            .store
            .speakers(self.asset.meeting_id)
            .unwrap()
            .into_iter()
            .map(|speaker| speaker.cluster_label)
            .collect();
        labels.sort();
        assert_eq!(labels, ["Me", "Speaker 1"]);
    }

    /// The meeting is `ready` and diarized: a speaker carries the
    /// diarizer's embedding and sample clip, which the fallback never
    /// stores.
    fn assert_diarized(&self) {
        assert_eq!(self.state(), MeetingState::Ready);
        let speakers = self.store.speakers(self.asset.meeting_id).unwrap();
        assert!(
            speakers
                .iter()
                .any(|speaker| speaker.embedding.is_some() && speaker.sample_clip_range.is_some()),
            "{speakers:?}"
        );
    }

    /// Nothing of the recording is gone or changed (a finished run adds
    /// its mixdown beside it), and nothing marks it for deletion: the asset
    /// has no expiry, and a sweep a year from now removes nothing.
    fn assert_recording_kept(&self, before: &BTreeMap<PathBuf, Vec<u8>>) {
        let after = contents(&self.audio);
        for (path, bytes) in before {
            assert!(after.get(path) == Some(bytes), "{} changed", path.display());
        }
        let asset = self.store.asset_by_id(self.asset.id).unwrap().unwrap();
        assert_eq!(asset.expires_at, None);
        assert_eq!(asset.retention, AudioRetention::KeepForever);
        let removed = RetentionSweep::new(self.store.clone())
            .run(Utc::now() + Duration::days(365))
            .unwrap();
        assert!(removed.is_empty(), "{removed:?}");
    }

    /// The first job under `Install::Allowed`, against a mirror that cuts
    /// the first download off and then refuses: it ends `ready` with the
    /// room speaker, keeps the recording and leaves the cut-off download's
    /// bytes in `onnx/diarization/` for the next attempt. The mirror sees
    /// three requests for `diarization/<segmentation file>`: the cut, the
    /// resume the store tries at once, and the diarize stage's own try after
    /// the warm-up's. Returns the recording as it was.
    async fn fall_back_on_the_cut_off_download(
        &self,
        mirror: &Mirror,
    ) -> BTreeMap<PathBuf, Vec<u8>> {
        let before = self.run_the_job().await;
        self.assert_ready_with_the_room_speaker();
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
        let resume = format!("bytes={CUT_AFTER}-");
        let path = format!("diarization/{SEGMENTATION_FILE}");
        assert_eq!(
            mirror.seen(),
            [
                (path.clone(), None),
                (path.clone(), Some(resume.clone())),
                (path, Some(resume)),
            ]
        );
        before
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_diarizer_download_cut_off_mid_job_falls_back_and_keeps_the_recording() {
    let mirror = Mirror::start(None);
    let world = world(&mirror, Install::Allowed);
    let before = world.fall_back_on_the_cut_off_download(&mirror).await;

    // Processing the meeting again asks the mirror again: the failed load
    // is not cached, the download resumes where it was cut, and the
    // recording is still all there.
    world.pipeline.process(world.asset.id).await.unwrap();
    world.assert_ready_with_the_room_speaker();
    let seen = mirror.seen();
    assert!(seen.len() > 3, "{seen:?}");
    assert_eq!(
        seen[3].1.as_deref(),
        Some(format!("bytes={CUT_AFTER}-").as_str()),
        "the download resumes where it was cut"
    );
    world.assert_recording_kept(&before);
}

/// The same job, then the mirror serves the real files: processing the
/// meeting again resumes the cut-off download and diarizes.
#[tokio::test(flavor = "multi_thread")]
async fn once_the_models_arrive_the_meeting_is_diarized_when_processed_again() {
    let Some(installed) = real_models() else {
        return;
    };
    let mirror = Mirror::start(Some(installed.root().to_path_buf()));
    let world = world(&mirror, Install::Allowed);
    world.fall_back_on_the_cut_off_download(&mirror).await;

    mirror.recover();
    world.pipeline.process(world.asset.id).await.unwrap();
    world.assert_diarized();
    let asset = models::asset();
    world.models.verify(&asset).unwrap();
    let seen = mirror.seen();
    assert_eq!(
        seen[3],
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

/// Under `Install::Never` a job without the models falls back to the room
/// speaker, keeps the recording and makes no request; it does not even
/// create the models folder.
#[tokio::test(flavor = "multi_thread")]
async fn a_pipeline_that_may_not_download_falls_back_without_a_request() {
    let mirror = Mirror::start(None);
    let world = world(&mirror, Install::Never);
    let before = world.run_the_job().await;
    world.assert_ready_with_the_room_speaker();
    world.assert_recording_kept(&before);
    world.pipeline.process(world.asset.id).await.unwrap();
    assert_eq!(mirror.seen(), []);
    assert!(!world.models.directory(&models::asset()).exists());
}

/// Files of the right size where Settings installs the diarizer's models
/// (`ModelStoreSpeechModels`) are the files the pipeline's diarizer and
/// `models::installed` read. When they fail to load and fail their
/// checksum, the job falls back, the files are deleted and Settings' row
/// reads Not downloaded, so it offers Download; no request is made.
#[tokio::test(flavor = "multi_thread")]
async fn junk_where_settings_installs_is_deleted_so_settings_offers_download() {
    let mirror = Mirror::start(None);
    let world = world(&mirror, Install::Never);
    let settings = ModelStoreSpeechModels::new(&world.setup);
    let asset = models::asset();
    let folder = settings.speech.directory(&asset);
    std::fs::create_dir_all(&folder).unwrap();
    for file in &asset.files {
        std::fs::File::create(folder.join(&file.name))
            .unwrap()
            .set_len(file.size)
            .unwrap();
    }
    assert!(settings.is_installed(ModelAsset::OfflineDiarizer));
    assert_eq!(
        models::installed(&world.models).unwrap(),
        ModelPaths::in_directory(&folder)
    );

    let before = world.run_the_job().await;
    world.assert_ready_with_the_room_speaker();
    world.assert_recording_kept(&before);
    for file in &asset.files {
        assert!(!folder.join(&file.name).exists(), "{}", file.name);
    }
    assert!(!settings.is_installed(ModelAsset::OfflineDiarizer));
    assert!(models::installed(&world.models).is_err());
    assert_eq!(mirror.seen(), []);
}

/// Settings downloads the models through the mirror; the pipeline's
/// diarizer under `Install::Never` then loads them from that folder
/// without a request, and the job diarizes.
#[tokio::test(flavor = "multi_thread")]
async fn the_pipelines_diarizer_loads_what_settings_downloaded_without_a_request() {
    let Some(installed) = real_models() else {
        return;
    };
    let mirror = Mirror::start(Some(installed.root().to_path_buf()));
    mirror.recover();
    let world = world(&mirror, Install::Never);
    let settings = ModelStoreSpeechModels::new(&world.setup);
    settings
        .download(ModelAsset::OfflineDiarizer, &mut |_, _| {})
        .unwrap();
    let downloads = mirror.seen().len();
    assert_eq!(downloads, 2, "{:?}", mirror.seen());

    world.run_the_job().await;
    world.assert_diarized();
    assert_eq!(mirror.seen().len(), downloads, "{:?}", mirror.seen());
}

/// The real models in `STENO_MODELS_DIR`, verified, when
/// `STENO_MODEL_TESTS=1`; else `None` and a note why the test did nothing.
fn real_models() -> Option<ModelStore> {
    if std::env::var("STENO_MODEL_TESTS").as_deref() != Ok("1") {
        eprintln!(
            "set STENO_MODEL_TESTS=1 and STENO_MODELS_DIR to serve the diarizer's real models from a loopback mirror"
        );
        return None;
    }
    let installed = ModelStore::from_environment();
    installed
        .verify(&models::asset())
        .expect("STENO_MODELS_DIR holds onnx/diarization/");
    Some(installed)
}
