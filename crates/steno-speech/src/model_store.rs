//! Where the models live and how they get there. The manifest names every
//! file of an asset with its size and SHA-256; the store checks a directory
//! against it, downloads what is missing into `<name>.partial` while
//! hashing, and renames only a verified, synced file into place. Models
//! are never committed (`.gitignore` covers `*.onnx`).
//! Swift: `Sources/StenoSpeech/Models/ModelAsset.swift`,
//! `ModelStore.swift` and `ModelDownloading.swift`, whose downloads go
//! through `FluidAudio` and `WhisperKit` instead.
//!
//! A store's root holds one folder per asset id, `<root>/<asset id>/`;
//! where Steno keeps it: the crate docs' Models section. The ONNX models
//! have a store at the models directory's `onnx/`
//! ([`ModelStore::in_models_directory`]), the Mac's `CoreML` Parakeet one
//! at its `fluidaudio/` ([`ModelStore::coreml_in_models_directory`]).
//!
//! # Hosts
//!
//! A file's [`ModelSource`] is a plain URL or a file in a Hugging Face
//! model repository at a pinned commit,
//! `https://huggingface.co/<repo>/resolve/<revision>/<path>`. GitHub
//! release assets cap at 2 GB per file, so the small file stays there
//! (Silero VAD, from the sherpa-onnx `asr-models` release) and the fp32
//! Parakeet export (2.6 GB, of which `encoder.weights` is 2.4 GB) goes to
//! Hugging Face, uploaded by `scripts/upload-models.sh` into
//! [`STENO_MODELS_REPO`] and fetched at the commit
//! [`PARAKEET_V3_FP32_REVISION`]. The checksums are those of the export
//! `spikes/onnx-speech/export/` produces with torch 2.14.1 and `NeMo`
//! 3.0.0, and the hosted copy matches them; uploading a different export
//! means changing them in the manifest too. `steno-diarize` describes its
//! two models as an asset of this store, id `diarization`
//! (`crates/steno-diarize/src/models.rs`), one file from a Hugging Face
//! repository and one from a sherpa-onnx GitHub release, and installs
//! them through it. Every file Steno ships has a source (a test checks
//! it). Without a mirror, a file the manifest gives no source has to be
//! put in place by hand: [`ModelStore::ensure`] reports
//! [`SpeechError::NotHosted`] when it is missing.
//!
//! The Mac's `CoreML` Parakeet ([`ModelAsset::parakeet_v3_coreml`]) comes
//! from the repository `FluidAudio` reads, [`PARAKEET_V3_COREML_REPO`], at
//! the commit [`PARAKEET_V3_COREML_REVISION`]: 23 files, most of them in
//! the four `.mlmodelc` bundles, about 483 MB. Their checksums are those
//! of the tree the Swift app installed from it.
//!
//! A mirror ([`ModelStore::with_mirror`], the speech setting
//! `modelsMirror`) replaces the hosts of every asset the store installs,
//! the speech models (Parakeet, Silero VAD) and the diarizer's, with no
//! fallback to the hosts. With one, a file is fetched from
//! `<mirror>/<asset id>/<file name>`, the layout of a store root, so a
//! whole store root (`parakeet-tdt-0.6b-v3-fp32/`, `silero-vad/`,
//! `diarization/`) served over HTTP is a mirror. The Hugging Face
//! repository [`STENO_MODELS_REPO`] holds only the Parakeet export, so a
//! copy of it alone is not. The `CoreML` store takes the same mirror, so
//! `<mirror>/parakeet-tdt-0.6b-v3/` serves the `CoreML` Parakeet. A mirror
//! should answer `Range` requests (see Downloads).
//!
//! # On disk
//!
//! An asset's folder holds, for each file `<name>` of the manifest (a name
//! may hold `/`, for a file in a folder of the asset, whose companions
//! below then sit in that folder too):
//!
//! - `<name>`: the installed file, renamed into place only once verified
//!   and synced. Only these count.
//! - `<name>.lock`: empty. A download of `<name>` locks it while it runs,
//!   and no download deletes it.
//! - `<name>.partial`: an unfinished download's bytes, which the next
//!   download resumes; deleted once `<name>` is installed.
//! - `<name>.partial.<pid>.<call>`: only where the file system has no
//!   locks. It belongs to one call and is deleted when that call ends, or
//!   by a later call once it has been untouched for 10 minutes.
//!
//! With no download running, everything but the installed files can be
//! deleted (a `.partial` only costs its bytes again). While one runs,
//! deleting its `.lock` lets a second download write the same `.partial`.
//! [`ModelStore::remove`] deletes the folder; do not call it during a
//! download.
//!
//! # Downloads
//!
//! A download holds the lock on `<name>.lock` while it opens, writes and
//! renames `<name>.partial`. A file over 64 MiB comes in `Range` requests
//! of 64 MiB, each with a body timeout of at most 128 s, so a connection
//! that goes silent costs minutes, not hours; a request cut short after it
//! moved the file on is followed at once by one for the rest. A dropped
//! connection resumes with a `Range` request on the next attempt, and a
//! partial left by a killed process is picked up by the next run after its
//! prefix is hashed again. A host that ignores the range is asked for the
//! whole file, which is written from the start under a timeout for the
//! whole size (10 h for 2.4 GB), so a mirror should answer `Range`
//! requests. A resumed file whose checksum fails is downloaded once more
//! from zero. A second download of the same file, in this process or
//! another, waits for the lock and reports the first one's progress
//! meanwhile: it then finds the file installed and returns, or resumes
//! what the first left. It gives up once the first has written nothing for
//! 10 minutes (a stopped process), or for longer where the first may still
//! be waiting on its connection: each attempt may take the connect and
//! response timeouts of both hops of the host's redirect and the body
//! timeout of its longest request (about 42 minutes in all for the 47 MB
//! decoder, which is one request, and about 12 minutes for a file of
//! several chunks). Once a file is installed, by whatever path, the next
//! call deletes its `<name>.partial`.

use std::collections::BTreeSet;
use std::fs::{self, File, TryLockError};
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use steno_core::StenoPaths;

use crate::error::SpeechError;

/// Where a file is downloaded from when no mirror is set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelSource {
    /// A plain HTTPS URL, such as a GitHub release asset (2 GB at most).
    Url(String),
    /// A file in a Hugging Face model repository at one commit, so the
    /// bytes behind the URL never change under the manifest's checksum.
    HuggingFace {
        /// `<owner>/<name>`.
        repo: String,
        /// A full commit hash, not a branch.
        revision: String,
        /// The file's path inside the repository.
        path: String,
    },
}

impl ModelSource {
    /// Hugging Face's download host.
    pub const HUGGING_FACE: &'static str = "https://huggingface.co";

    /// The URL a download fetches.
    #[must_use]
    pub fn url(&self) -> String {
        match self {
            ModelSource::Url(url) => url.clone(),
            ModelSource::HuggingFace {
                repo,
                revision,
                path,
            } => format!("{}/{repo}/resolve/{revision}/{path}", Self::HUGGING_FACE),
        }
    }
}

/// One file of an asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelFile {
    pub name: String,
    /// `None` for a file without a host; a mirror serves it regardless.
    pub source: Option<ModelSource>,
    /// Lower-case hex.
    pub sha256: String,
    pub size: u64,
}

/// The Hugging Face repository that holds the fp32 export,
/// <https://huggingface.co/nicolaischmid/steno-models>;
/// `scripts/upload-models.sh` uploads to it.
pub const STENO_MODELS_REPO: &str = "nicolaischmid/steno-models";

/// The commit of [`STENO_MODELS_REPO`] that holds the export, as
/// `scripts/upload-models.sh` printed it.
pub const PARAKEET_V3_FP32_REVISION: Option<&str> =
    Some("4a133253481bfd2cb38dc3e77c3f748199562488");

/// The Hugging Face repository `FluidAudio` downloads Parakeet v3 for
/// `CoreML` from, <https://huggingface.co/FluidInference/parakeet-tdt-0.6b-v3-coreml>,
/// where the Swift app's copy came from.
pub const PARAKEET_V3_COREML_REPO: &str = "FluidInference/parakeet-tdt-0.6b-v3-coreml";

/// The commit of [`PARAKEET_V3_COREML_REPO`] the `CoreML` Parakeet is
/// fetched at (2026-08-19). Its 23 files are those the Swift app installed
/// on 2026-09-25, byte for byte (`crates/steno-speech/tests/fixtures/parakeet-v3-coreml.sha256`).
pub const PARAKEET_V3_COREML_REVISION: &str = "7dd20fe6b1797d35f5e3307e8b1732d9a178edfe";

/// One downloadable model bundle; the settings pane shows the display
/// name, the total size and the licence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelAsset {
    /// The directory name under the root.
    pub id: String,
    pub display_name: String,
    pub licence: String,
    /// What the app and the download show next to the model.
    pub attribution: String,
    pub files: Vec<ModelFile>,
}

impl ModelAsset {
    /// Silero VAD v4 from the sherpa-onnx `asr-models` release.
    #[must_use]
    pub fn silero_vad() -> Self {
        ModelAsset {
            id: "silero-vad".to_owned(),
            display_name: "Silero VAD".to_owned(),
            licence: "MIT".to_owned(),
            attribution: "Silero VAD (Silero Team), MIT, as packaged by sherpa-onnx".to_owned(),
            files: vec![ModelFile {
                name: "silero_vad.onnx".to_owned(),
                source: Some(ModelSource::Url("https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/silero_vad.onnx".to_owned())),
                sha256: "9e2449e1087496d8d4caba907f23e0bd3f78d91fa552479bb9c23ac09cbb1fd6".to_owned(),
                size: 643_854,
            }],
        }
    }

    /// Our fp32 ONNX export of Parakeet TDT 0.6B v3 with the 10000-frame
    /// position table (decision 3 of the speech-stack plan), from
    /// [`STENO_MODELS_REPO`] at [`PARAKEET_V3_FP32_REVISION`].
    #[must_use]
    pub fn parakeet_v3_fp32() -> Self {
        match PARAKEET_V3_FP32_REVISION {
            Some(revision) => Self::parakeet_v3_fp32_from(STENO_MODELS_REPO, revision),
            None => Self::parakeet_v3_fp32_manifest(|_, _| None),
        }
    }

    /// The export from a Hugging Face repository at `revision`, laid out
    /// `<asset id>/<file name>` the way `scripts/upload-models.sh` uploads it.
    #[must_use]
    pub fn parakeet_v3_fp32_from(repo: &str, revision: &str) -> Self {
        Self::parakeet_v3_fp32_manifest(|id, name| {
            Some(ModelSource::HuggingFace {
                repo: repo.to_owned(),
                revision: revision.to_owned(),
                path: format!("{id}/{name}"),
            })
        })
    }

    fn parakeet_v3_fp32_manifest(source: impl Fn(&str, &str) -> Option<ModelSource>) -> Self {
        const ID: &str = "parakeet-tdt-0.6b-v3-fp32";
        let file = |name: &str, sha256: &str, size: u64| ModelFile {
            name: name.to_owned(),
            source: source(ID, name),
            sha256: sha256.to_owned(),
            size,
        };
        ModelAsset {
            id: ID.to_owned(),
            display_name: "Parakeet TDT 0.6B v3 (fp32, ONNX)".to_owned(),
            licence: "CC-BY-4.0".to_owned(),
            attribution: "Parakeet TDT 0.6B v3 by NVIDIA (https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3), CC-BY-4.0, converted to ONNX".to_owned(),
            files: vec![
                file("encoder.onnx", "9dc761845d0c7774d222203afca9ec3e3a6372da06999817d1eeb353efd59ca8", 82_798_411),
                file("encoder.weights", "9a22d372c51455c34f13405da2520baefb7125bd16981397561423ed32d24f36", 2_435_420_160),
                file("decoder.onnx", "4340de52b5fed87d7441650cedbb2648af93bcca4836e7b062b245590119fe79", 47_234_123),
                file("joiner.onnx", "468a21c3656cabbefff979a2c1f5cd7d2b1e353b13ba016cbcf2d8ff1a6fc3b1", 25_286_710),
                file("tokens.txt", "d58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d", 93_939),
            ],
        }
    }

    /// Parakeet TDT 0.6B v3 for `CoreML` as `FluidAudio` ships it, the
    /// files the Mac's `CoreML` backend (`steno-speech-coreml`) and the
    /// Swift app load: the four `.mlmodelc` bundles, the vocabulary under
    /// both its names, and `config.json`, from [`PARAKEET_V3_COREML_REPO`]
    /// at [`PARAKEET_V3_COREML_REVISION`]. Its id is the folder `FluidAudio`
    /// gives it, so a store rooted at the models directory's `fluidaudio/`
    /// ([`ModelStore::coreml_in_models_directory`]) installs it where the
    /// Swift app does. The other bundles of the repository (the int4 and v2
    /// encoders, the older joints, the `.mlpackage` sources) are not
    /// fetched. Swift: `ModelAsset.modelFolder` and `requiredFiles` in
    /// `Sources/StenoSpeech/Models/ModelAsset.swift`.
    #[must_use]
    pub fn parakeet_v3_coreml() -> Self {
        let file = |name: &str, sha256: &str, size: u64| ModelFile {
            name: name.to_owned(),
            source: Some(ModelSource::HuggingFace {
                repo: PARAKEET_V3_COREML_REPO.to_owned(),
                revision: PARAKEET_V3_COREML_REVISION.to_owned(),
                path: name.to_owned(),
            }),
            sha256: sha256.to_owned(),
            size,
        };
        ModelAsset {
            id: "parakeet-tdt-0.6b-v3".to_owned(),
            display_name: "Parakeet TDT 0.6B v3 (int8)".to_owned(),
            licence: "CC-BY-4.0".to_owned(),
            attribution: "Parakeet TDT 0.6B v3 by NVIDIA (https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3), CC-BY-4.0, converted to CoreML by FluidInference".to_owned(),
            files: vec![
                file("Preprocessor.mlmodelc/analytics/coremldata.bin", "c9beeb989c8d66f8be11df59bc6df277ec76cee404f6865b46243835ef562f6d", 243),
                file("Preprocessor.mlmodelc/coremldata.bin", "dbde3f2300842c1fd51ef3ff948a0bcffe65ffd2dca10707f2509f32c1d65b1d", 486),
                file("Preprocessor.mlmodelc/metadata.json", "2a98699e22d279dd37fa1d238aeb1c6db1df0d6fad687775324157689d8f3acf", 2_841),
                file("Preprocessor.mlmodelc/model.mil", "4b8518a956450fec57f06c2a21bdffc26973f7f1fa6842fb38fe917f896b6b93", 28_181),
                file("Preprocessor.mlmodelc/weights/weight.bin", "129b76e3aeafa8afa3ea76d995b964b145fe83700d579f6ff42c4c38fa0968ea", 491_072),
                file("Encoder.mlmodelc/analytics/coremldata.bin", "42e638870d73f26b332918a3496ce36793fbb413a81cbd3d16ba01328637a105", 243),
                file("Encoder.mlmodelc/coremldata.bin", "d48034a167a82e88fc3df64f60af963ab3983538271175b8319e7d5720a0fb86", 485),
                file("Encoder.mlmodelc/metadata.json", "da24da9cca943fb29d7fa8e376d57fca7cb3aa08ca51b956b0b0e56813f087e9", 2_921),
                file("Encoder.mlmodelc/model.mil", "ed7b19156ca29fa7dfd6891deb9fda4b0e8893f68597c985d135736546a43808", 959_769),
                file("Encoder.mlmodelc/weights/weight.bin", "e2020f323703477a5b21d7c2d282c403e371afb5962e79877e3033e73ba6f421", 445_187_200),
                file("Decoder.mlmodelc/analytics/coremldata.bin", "4238c4e81ecd0dc94bd7dfbb60f7e2cc824107c1ffe0387b8607b72833dba350", 243),
                file("Decoder.mlmodelc/coremldata.bin", "18647af085d87bd8f3121c8a9b4d4564c1ede038dab63d295b4e745cf2d7fb99", 554),
                file("Decoder.mlmodelc/metadata.json", "a39e93cd8371b8ded92635c7804fcd0590f0d1dd9415c6d19a0484be073077d9", 3_427),
                file("Decoder.mlmodelc/model.mil", "ef2a0a281695398a62fde86ac269c68f73d5b578d7ed3b31f2ba91a2d1ea1f35", 13_110),
                file("Decoder.mlmodelc/weights/weight.bin", "48adf0f0d47c406c8253d4f7fef967436a39da14f5a65e66d5a4b407be355d41", 23_604_992),
                file("JointDecisionv3.mlmodelc/analytics/coremldata.bin", "26def4bf73dd56d29dee21c8ef97cb8969e62f6120ed1adc91e46828e2737b6c", 243),
                file("JointDecisionv3.mlmodelc/coremldata.bin", "f5fc08b741400f0088492c9e839418b1e18522f19cba28d361dd030c5f398342", 521),
                file("JointDecisionv3.mlmodelc/metadata.json", "d9307211b9a37e0f0ac260c7660b1571a3de25841035cfdf9b58fd40425f890f", 3_453),
                file("JointDecisionv3.mlmodelc/model.mil", "be60732943389a047175111a83f8839f3eb39d4803adafa828a0871b2f39818d", 11_775),
                file("JointDecisionv3.mlmodelc/weights/weight.bin", "4e0e63d840032f7f07ddb1d64446051166281e5491bf22da8a945c41f6eedb3e", 12_642_764),
                file("config.json", "97f19ecccd0fdc730d76fb918090fa8bc64bce8ea4ad43715d5e9c2c7350db66", 475),
                file("parakeet_v3_vocab.json", "7ec60e05f1b24480736ec0eed40900f4626bce1fa9a60fd700ec7e2a59198735", 151_122),
                file("parakeet_vocab.json", "7ec60e05f1b24480736ec0eed40900f4626bce1fa9a60fd700ec7e2a59198735", 151_122),
            ],
        }
    }

    /// Every ONNX asset the crate knows: what the speech sidecar loads.
    /// The `CoreML` Parakeet ([`Self::parakeet_v3_coreml`]) lives in its
    /// own store and is not among them.
    #[must_use]
    pub fn onnx() -> Vec<Self> {
        vec![Self::silero_vad(), Self::parakeet_v3_fp32()]
    }

    /// Bytes on disk once installed.
    #[must_use]
    pub fn total_size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }

    /// Refuses an id that is not exactly one normal path component (`..`,
    /// an absolute path, an empty string, a slash), and a file name that
    /// is not one or more of them joined by `/` (a file inside a folder of
    /// the asset, as in a `CoreML` bundle), so the manifest can never name
    /// a path outside the root.
    pub fn validate(&self) -> Result<(), SpeechError> {
        let invalid = |name: &str| SpeechError::InvalidName {
            asset: self.id.clone(),
            name: name.to_owned(),
        };
        if !is_plain_name(&self.id) {
            return Err(invalid(&self.id));
        }
        for file in &self.files {
            if !file.name.split('/').all(is_plain_name) {
                return Err(invalid(&file.name));
            }
        }
        Ok(())
    }
}

/// True when `name` is one `Component::Normal` and nothing else.
fn is_plain_name(name: &str) -> bool {
    let mut components = Path::new(name).components();
    matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(_)), None)
    )
}

/// One progress report of a download.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadProgress<'a> {
    pub file: &'a str,
    pub received: u64,
    pub total: u64,
}

/// How long a download waits for its connection, the TLS handshake
/// included; without a limit a stalled host would hang `prepare`, and every
/// `transcribe` waiting behind it.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a download then waits for the response headers.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

/// The shortest body timeout, for a small file.
const MIN_BODY_TIMEOUT: Duration = Duration::from_secs(60);

/// The slowest average rate, in bytes per second, a body may arrive at.
const MIN_BODY_RATE: u64 = 64 * 1024;

/// The longest body timeout of a `Range` request. A chunk that takes
/// longer is cut there and, since it moved the file on, the rest is asked
/// for at once, so a slow link still finishes and a silent one is given up
/// after a few minutes.
const MAX_CHUNK_TIMEOUT: Duration = Duration::from_secs(128);

/// The most bytes one request asks for. A file of one chunk is fetched
/// with a plain request; a larger one with `Range` requests, one chunk
/// after another. Each request costs a round trip through the host's
/// redirect (about 0.4 s to Hugging Face), so chunks are large: the 2.6 GB
/// export takes about 40 requests.
const CHUNK: u64 = 64 << 20;

/// The read buffer of a download and of a hash.
const READ_BUFFER: usize = 1 << 16;

/// Whether a model's user may download it when a file is missing: the
/// speech sidecar's ([`SidecarConfig::install`](crate::SidecarConfig::install))
/// and `steno-diarize`'s, which re-exports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Install {
    /// A missing file is downloaded first ([`ModelStore::ensure`]): the
    /// `steno` command's explicit commands, `steno process` among them.
    Allowed,
    /// A missing file is [`SpeechError::NotInstalled`] and no request is
    /// made ([`ModelStore::installed_directory`]): the app's pipelines,
    /// which check for missing models before a job and never download
    /// during one; Settings and onboarding install the models.
    Never,
}

/// The store root, the mirror, the HTTP client, and the chunk size, body
/// timeouts and clock the tests change.
#[derive(Debug, Clone)]
pub struct ModelStore {
    root: PathBuf,
    mirror: Option<String>,
    agent: ureq::Agent,
    /// [`CHUNK`], smaller in tests.
    chunk: u64,
    /// [`MIN_BODY_TIMEOUT`], changed only by the tests of a stalled body.
    min_body_timeout: Duration,
    /// [`MAX_CHUNK_TIMEOUT`], likewise.
    max_chunk_timeout: Duration,
    /// What a download waiting for another one's lock goes by.
    clock: Clock,
}

impl ModelStore {
    /// Names the models directory in place of the default one, for the app
    /// (without one in its settings), the CLI, the example and the tests.
    pub const ENVIRONMENT_VARIABLE: &'static str = "STENO_MODELS_DIR";

    /// Names the mirror [`ModelStore::from_environment`] sets.
    pub const MIRROR_ENVIRONMENT_VARIABLE: &'static str = "STENO_MODELS_MIRROR";

    /// The folder of the models directory the ONNX models live in, beside
    /// the `CoreML` ones the Swift app keeps in `fluidaudio/`.
    pub const ONNX_FOLDER: &'static str = "onnx";

    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        ModelStore {
            root: root.into(),
            mirror: None,
            agent: ureq::Agent::config_builder()
                .timeout_connect(Some(CONNECT_TIMEOUT))
                .timeout_recv_response(Some(RESPONSE_TIMEOUT))
                .build()
                .new_agent(),
            chunk: CHUNK,
            min_body_timeout: MIN_BODY_TIMEOUT,
            max_chunk_timeout: MAX_CHUNK_TIMEOUT,
            clock: Clock::System,
        }
    }

    /// Fetches every file from `<mirror>/<asset id>/<file name>` instead of
    /// its source, unhosted files included; `None` or an empty string
    /// restores the sources. Checksums and sizes still come from the
    /// manifest, so a mirror cannot change what is installed.
    #[must_use]
    pub fn with_mirror(mut self, mirror: Option<String>) -> Self {
        self.mirror = mirror
            .map(|m| m.trim().trim_end_matches('/').to_owned())
            .filter(|m| !m.is_empty());
        self
    }

    /// The mirror set by [`ModelStore::with_mirror`], trimmed.
    #[must_use]
    pub fn mirror(&self) -> Option<&str> {
        self.mirror.as_deref()
    }

    /// Where `file` of `asset` is downloaded from: the mirror when one is
    /// set, else the file's source; `None` when neither exists.
    #[must_use]
    pub fn url_for(&self, asset: &ModelAsset, file: &ModelFile) -> Option<String> {
        match &self.mirror {
            Some(mirror) => Some(format!("{mirror}/{}/{}", asset.id, file.name)),
            None => file.source.as_ref().map(ModelSource::url),
        }
    }

    /// The folder of the models directory the `CoreML` models live in, as
    /// the Swift app's `FluidAudio` lays them out.
    /// Swift: `ModelAsset.frameworkRoot`
    /// (`Sources/StenoSpeech/Models/ModelAsset.swift`).
    pub const FLUIDAUDIO_FOLDER: &'static str = "fluidaudio";

    /// The store in the models directory `models_directory`:
    /// `<models directory>/onnx`.
    #[must_use]
    pub fn in_models_directory(models_directory: &Path) -> Self {
        Self::new(models_directory.join(Self::ONNX_FOLDER))
    }

    /// The store of the `CoreML` models in the models directory
    /// `models_directory`: `<models directory>/fluidaudio`, where
    /// [`ModelAsset::parakeet_v3_coreml`] installs into
    /// `fluidaudio/parakeet-tdt-0.6b-v3`, the Swift app's folder.
    #[must_use]
    pub fn coreml_in_models_directory(models_directory: &Path) -> Self {
        Self::new(models_directory.join(Self::FLUIDAUDIO_FOLDER))
    }

    /// For the `transcribe` example and the FLEURS test, which have no
    /// settings: the store in the models directory `STENO_MODELS_DIR`
    /// names, else in the default one, as the app and the CLI resolve it
    /// without a models directory in the settings, with the mirror
    /// `STENO_MODELS_MIRROR` names.
    #[must_use]
    pub fn from_environment() -> Self {
        Self::in_models_directory(
            &Self::environment_models_directory().unwrap_or_else(Self::default_models_directory),
        )
        .with_mirror(
            std::env::var(Self::MIRROR_ENVIRONMENT_VARIABLE)
                .ok()
                .filter(|v| !v.is_empty()),
        )
    }

    /// The models directory `STENO_MODELS_DIR` names, made absolute
    /// against the current directory when it is relative; `None` when unset
    /// or empty.
    #[must_use]
    pub fn environment_models_directory() -> Option<PathBuf> {
        Self::models_directory_named(std::env::var_os(Self::ENVIRONMENT_VARIABLE))
    }

    /// [`Self::environment_models_directory`] for the variable's `value`.
    #[must_use]
    pub fn models_directory_named(value: Option<std::ffi::OsString>) -> Option<PathBuf> {
        let path = PathBuf::from(value.filter(|v| !v.is_empty())?);
        Some(match std::env::current_dir() {
            Ok(cwd) if path.is_relative() => cwd.join(path),
            _ => path,
        })
    }

    /// The default models directory, `<support directory>/Models`
    /// ([`steno_core::StenoPaths`]).
    #[must_use]
    pub fn default_models_directory() -> PathBuf {
        StenoPaths::default_support_directory().join("Models")
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `<root>/<asset id>`.
    #[must_use]
    pub fn directory(&self, asset: &ModelAsset) -> PathBuf {
        self.root.join(&asset.id)
    }

    /// Files absent or of the wrong size, by name; a file whose name
    /// fails [`ModelAsset::validate`] is never looked up and counts as
    /// missing.
    #[must_use]
    pub fn missing_files(&self, asset: &ModelAsset) -> Vec<String> {
        let directory = self.directory(asset);
        let invalid = asset.validate().is_err();
        asset
            .files
            .iter()
            .filter(|file| invalid || !is_complete(&directory.join(&file.name), file))
            .map(|file| file.name.clone())
            .collect()
    }

    /// True when every file is present with its manifest size. Content is
    /// checked at download time and by [`ModelStore::verify`].
    #[must_use]
    pub fn is_installed(&self, asset: &ModelAsset) -> bool {
        self.missing_files(asset).is_empty()
    }

    /// The asset's directory when [`ModelStore::is_installed`], else
    /// [`SpeechError::NotInstalled`] naming the missing files. It hashes
    /// nothing and fetches nothing, so a loader that must not download
    /// and a gate that asks whether a job can run give the same answer.
    pub fn installed_directory(&self, asset: &ModelAsset) -> Result<PathBuf, SpeechError> {
        asset.validate()?;
        let directory = self.directory(asset);
        let missing = self.missing_files(asset);
        if missing.is_empty() {
            Ok(directory)
        } else {
            Err(SpeechError::NotInstalled {
                asset: asset.id.clone(),
                directory,
                missing,
            })
        }
    }

    /// Hashes every installed file against the manifest.
    pub fn verify(&self, asset: &ModelAsset) -> Result<(), SpeechError> {
        let directory = self.installed_directory(asset)?;
        for file in &asset.files {
            let path = directory.join(&file.name);
            check_digest(file, &path, sha256_of(&path)?)?;
        }
        Ok(())
    }

    /// Installs the asset if needed and returns its directory. Missing
    /// files with a URL ([`ModelStore::url_for`]) are downloaded and
    /// verified, tried again when the host answers 5xx, the connection
    /// drops or the body stalls, each attempt resuming where the last one
    /// stopped, until [`DOWNLOAD_ATTEMPTS`] attempts in a row fetched
    /// nothing. `progress` also reports a download of the same file that
    /// this one waits for, which fails the call once that one has written
    /// nothing for 10 minutes or more (see the module's Downloads section).
    /// A missing file without a URL is [`SpeechError::NotHosted`]. Another
    /// process's per-call partials that have gone stale are removed first;
    /// `<name>.partial` stays and is resumed, unless its file is installed
    /// already, when it is deleted.
    ///
    /// ```no_run
    /// use steno_speech::{ModelAsset, ModelStore};
    ///
    /// let store = ModelStore::from_environment();
    /// let directory = store.ensure(&ModelAsset::silero_vad(), &mut |p| {
    ///     eprintln!("{}: {} of {} bytes", p.file, p.received, p.total);
    /// })?;
    /// assert!(directory.join("silero_vad.onnx").is_file());
    /// # Ok::<(), steno_speech::SpeechError>(())
    /// ```
    pub fn ensure(
        &self,
        asset: &ModelAsset,
        progress: &mut dyn FnMut(DownloadProgress<'_>),
    ) -> Result<PathBuf, SpeechError> {
        asset.validate()?;
        let directory = self.directory(asset);
        for file in &asset.files {
            remove_stale_partials(&directory, &file.name);
            if is_complete(&directory.join(&file.name), file) {
                remove_finished_partial(&directory, &file.name);
            }
        }
        let missing = self.missing_files(asset);
        if missing.is_empty() {
            return Ok(directory);
        }
        fs::create_dir_all(&directory).map_err(|e| SpeechError::io(&directory, e))?;
        for file in asset.files.iter().filter(|f| missing.contains(&f.name)) {
            let Some(url) = self.url_for(asset, file) else {
                return Err(SpeechError::NotHosted {
                    asset: asset.id.clone(),
                    directory,
                });
            };
            let destination = directory.join(&file.name);
            if let Some(folder) = destination.parent() {
                fs::create_dir_all(folder).map_err(|e| SpeechError::io(folder, e))?;
            }
            self.download_with_retries(&url, file, &destination, progress)?;
        }
        Ok(directory)
    }

    /// Deletes the asset's directory, lock files included. Not while a
    /// download of the asset runs: deleting the lock it holds lets a second
    /// download write its partial too (see the module's On disk section).
    pub fn remove(&self, asset: &ModelAsset) -> Result<(), SpeechError> {
        asset.validate()?;
        let directory = self.directory(asset);
        if directory.exists() {
            fs::remove_dir_all(&directory).map_err(|e| SpeechError::io(&directory, e))?;
        }
        Ok(())
    }

    /// [`ModelStore::download`], tried again after a transient failure.
    /// Every attempt continues the one partial file. One that moved it on
    /// is followed at once and starts the count of failures again, so a
    /// long download over a poor or slow connection may break many times
    /// and still finish. A resumed file whose checksum fails is truncated
    /// and fetched once more from zero.
    fn download_with_retries(
        &self,
        url: &str,
        file: &ModelFile,
        destination: &Path,
        progress: &mut dyn FnMut(DownloadProgress<'_>),
    ) -> Result<(), SpeechError> {
        let limit = self.lock_wait_limit(file.size);
        let Some(mut partial) = Partial::open(destination, file, &self.clock, limit, progress)?
        else {
            return Ok(());
        };
        let mut failures = 0;
        let mut started_over = false;
        loop {
            let before = partial.len;
            match self.download(url, file, destination, &mut partial, progress) {
                Ok(()) => return Ok(()),
                Err(error) if is_transient(&error) && partial.len > before => {
                    tracing::info!(%error, received = partial.len, "model download interrupted, resuming");
                    failures = 0;
                }
                Err(error) if is_transient(&error) => {
                    failures += 1;
                    if failures >= DOWNLOAD_ATTEMPTS {
                        return Err(error);
                    }
                    tracing::warn!(%error, failures, received = partial.len, "model download failed, trying again");
                    std::thread::sleep(RETRY_DELAY * failures);
                }
                Err(SpeechError::Checksum { .. }) if partial.resumed && !started_over => {
                    tracing::warn!(file = %file.name, "resumed model download failed its checksum, starting over");
                    partial.restart()?;
                    started_over = true;
                }
                Err(error) => {
                    // Bytes that are wrong or too many cannot be resumed;
                    // a shortfall can.
                    let overrun = matches!(error, SpeechError::Size { expected, actual, .. } if actual > expected);
                    if overrun || matches!(error, SpeechError::Checksum { .. }) {
                        partial.discard();
                    }
                    return Err(error);
                }
            }
        }
    }

    /// Streams the rest of `url` into `partial`, verifies and syncs it and
    /// renames it into place while its lock is still held, so no other
    /// download can pick the file up between the rename and the unlock.
    fn download(
        &self,
        url: &str,
        file: &ModelFile,
        destination: &Path,
        partial: &mut Partial,
        progress: &mut dyn FnMut(DownloadProgress<'_>),
    ) -> Result<(), SpeechError> {
        self.stream_to(url, file, partial, progress)?;
        if partial.len != file.size {
            return Err(SpeechError::Size {
                path: destination.to_path_buf(),
                expected: file.size,
                actual: partial.len,
            });
        }
        check_digest(
            file,
            destination,
            format!("{:x}", partial.hasher.clone().finalize()),
        )?;
        // A freshly written model is what an antivirus client scans first,
        // so on Windows the rename is tried again while it holds the file.
        steno_core::busy_file::rename(&partial.path, destination)
            .map_err(|e| SpeechError::io(destination, e))?;
        partial.done = true;
        sync_parent(destination);
        Ok(())
    }

    /// How long a response may take to deliver `size` bytes of body once
    /// the headers are in: `max(MIN_BODY_TIMEOUT, size / MIN_BODY_RATE)`.
    /// It bounds a stalled body without cutting one that keeps moving at
    /// 64 KiB/s or more. `ureq` has no timeout between two reads, so this
    /// is what ends a silent connection: 10 h for a whole 2.4 GB file,
    /// which only a host that ignores `Range` is given.
    fn body_timeout(&self, size: u64) -> Duration {
        self.min_body_timeout
            .max(Duration::from_secs(size / MIN_BODY_RATE))
    }

    /// The body timeout of a `Range` request for the bytes `offset..end`
    /// of a file of `size` bytes: [`Self::body_timeout`] of their length,
    /// at most [`MAX_CHUNK_TIMEOUT`]. A file of one chunk keeps the timeout
    /// of its whole size, as its range request may be answered with the
    /// whole file.
    fn range_timeout(&self, size: u64, offset: u64, end: u64) -> Duration {
        if size <= self.chunk {
            return self.body_timeout(size);
        }
        self.body_timeout(end - offset).min(self.max_chunk_timeout)
    }

    /// How long a download waiting for the lock on a file of `size` bytes
    /// lets the holder's partial stand still before it gives up on the
    /// holder: [`STALE_PARTIAL`], or longer where a live holder can write
    /// nothing for longer. Each of its [`DOWNLOAD_ATTEMPTS`] attempts may
    /// spend the connect and response timeouts twice, as the host answers
    /// through one redirect (as Hugging Face's does) and `ureq` times each
    /// hop afresh, and the body timeout of its longest request without a
    /// byte, with the retry waits between them: about 42 minutes for the
    /// 47 MB decoder, which is one request, and about 12 minutes for a file
    /// of several chunks. A holder whose host ignored `Range` for a larger
    /// file waits out the whole file's body timeout and can stay silent
    /// longer; a mirror should answer `Range`.
    fn lock_wait_limit(&self, size: u64) -> Duration {
        let hop = CONNECT_TIMEOUT + RESPONSE_TIMEOUT;
        let attempt = 2 * hop + self.range_timeout(size, 0, size.min(self.chunk));
        let waits: Duration = (1..DOWNLOAD_ATTEMPTS).map(|n| RETRY_DELAY * n).sum();
        STALE_PARTIAL.max(attempt * DOWNLOAD_ATTEMPTS + waits)
    }

    /// One `GET` of `url`, with a `Range` header when `range` is set and
    /// `body` as its body timeout. It asks for the bytes uncompressed: the
    /// client would otherwise offer gzip, and a range of a compressed body
    /// is no range of the file.
    fn get(
        &self,
        url: &str,
        range: Option<String>,
        body: Duration,
    ) -> Result<ureq::http::Response<ureq::Body>, SpeechError> {
        let request = self.agent.get(url).header("Accept-Encoding", "identity");
        let request = match range {
            Some(range) => request.header("Range", range),
            None => request,
        };
        request
            .config()
            .timeout_recv_body(Some(body))
            .build()
            .call()
            .map_err(|source| SpeechError::Download {
                url: url.to_owned(),
                source: Box::new(source),
            })
    }

    /// Requests the next bytes of `partial`, from its length up to
    /// [`CHUNK`] more: a plain request when that is the whole file, else a
    /// `Range` request, honoured only by a `206` that starts at its length.
    /// A `200` whose length is not the file's (a sign-in page, say) is a
    /// transient failure that keeps the partial. Another `200` (the host
    /// ignored the range) restarts the file with that body when the file
    /// is one chunk. Any other answer restarts it and asks for the whole
    /// body under the whole file's timeout: a `200` to a larger file,
    /// whose range request had at most [`MAX_CHUNK_TIMEOUT`], a `206`
    /// elsewhere or a `416`. Returns the response and whether it carries
    /// the whole file.
    fn request(
        &self,
        url: &str,
        file: &ModelFile,
        partial: &mut Partial,
    ) -> Result<(ureq::http::Response<ureq::Body>, bool), SpeechError> {
        let offset = partial.len;
        let end = file.size.min(offset.saturating_add(self.chunk));
        if offset > 0 || end < file.size {
            let range = if end == file.size {
                format!("bytes={offset}-")
            } else {
                format!("bytes={offset}-{}", end - 1)
            };
            // A file of one chunk has the timeout of its whole size, so a
            // `200` to it can stand for the whole file.
            let one_chunk = file.size <= self.chunk;
            match self.get(url, Some(range), self.range_timeout(file.size, offset, end)) {
                Ok(response) if response.status().as_u16() == 206 => {
                    if content_range_start(&response) == Some(offset) {
                        return Ok((response, false));
                    }
                }
                Ok(response) => {
                    if let Some(length) = content_length(&response)
                        && length != file.size
                    {
                        return Err(cut_short(
                            url,
                            format!(
                                "the host answered a range with {length} bytes, not the {} byte file",
                                file.size
                            ),
                        ));
                    }
                    if one_chunk {
                        partial.restart()?;
                        return Ok((response, true));
                    }
                }
                Err(SpeechError::Download { source, .. })
                    if matches!(*source, ureq::Error::StatusCode(416)) => {}
                Err(error) => return Err(error),
            }
            partial.restart()?;
        }
        Ok((self.get(url, None, self.body_timeout(file.size))?, true))
    }

    /// Streams the rest of the file into `partial` while hashing, one
    /// response after another, and syncs it. A partial that is complete
    /// already is not asked for again. Stops at the first byte over the
    /// manifest size, so a misbehaving host cannot fill the disk; a `206`
    /// shorter than asked for is followed by a request for the rest, and
    /// one that brings nothing is a transient failure.
    fn stream_to(
        &self,
        url: &str,
        file: &ModelFile,
        partial: &mut Partial,
        progress: &mut dyn FnMut(DownloadProgress<'_>),
    ) -> Result<(), SpeechError> {
        // Cleared by a restart, so a checksum failure then is the host's.
        partial.resumed = partial.len > 0;
        let mut buffer = vec![0u8; READ_BUFFER];
        let report = |partial: &Partial, progress: &mut dyn FnMut(DownloadProgress<'_>)| {
            progress(DownloadProgress {
                file: &file.name,
                received: partial.len,
                total: file.size,
            });
        };
        report(partial, progress);
        while partial.len < file.size {
            let (response, whole) = self.request(url, file, partial)?;
            let before = partial.len;
            let mut body = response.into_body();
            // One byte over the manifest size is enough to tell a long body.
            let mut reader = body
                .with_config()
                .limit(file.size.saturating_sub(before) + 1)
                .reader();
            loop {
                let n = reader
                    .read(&mut buffer)
                    .map_err(|e| SpeechError::io(&partial.path, e))?;
                if n == 0 {
                    break;
                }
                if partial.len + n as u64 > file.size {
                    // Counted, not written: the size check names the overrun.
                    partial.len += n as u64;
                    break;
                }
                partial.append(&buffer[..n])?;
                report(partial, progress);
            }
            // The size check names a whole body's shortfall or overrun.
            if whole || partial.len > file.size {
                break;
            }
            // Asked for again, it would bring nothing forever.
            if partial.len == before {
                return Err(cut_short(
                    url,
                    "the host sent no bytes of the range".to_owned(),
                ));
            }
        }
        // Without the sync a power loss could leave a file of the right
        // length but lost contents, which `is_installed` (sizes only) takes.
        partial
            .file
            .sync_all()
            .map_err(|e| SpeechError::io(&partial.path, e))
    }
}

/// True when `path` is a file of the manifest size.
fn is_complete(path: &Path, file: &ModelFile) -> bool {
    fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() == file.size)
}

/// A response that ended before it brought what was asked: as transient
/// as a dropped connection, so the partial stays for the next attempt.
fn cut_short(url: &str, detail: String) -> SpeechError {
    SpeechError::Download {
        url: url.to_owned(),
        source: Box::new(ureq::Error::Io(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            detail,
        ))),
    }
}

/// The `Content-Length` of a response, when it has one.
fn content_length(response: &ureq::http::Response<ureq::Body>) -> Option<u64> {
    response
        .headers()
        .get("content-length")?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// The first byte of a `Content-Range: bytes <first>-<last>/<total>`.
fn content_range_start(response: &ureq::http::Response<ureq::Body>) -> Option<u64> {
    let value = response.headers().get("content-range")?.to_str().ok()?;
    let range = value.trim().strip_prefix("bytes")?.trim_start();
    range.split_once('-')?.0.trim().parse().ok()
}

/// Attempts in a row that fetch nothing before a download fails; one that
/// moves the file on starts the count again. A 5xx answer or a dropped
/// connection is often gone a few seconds later.
pub const DOWNLOAD_ATTEMPTS: u32 = 3;

/// The wait before the second attempt; each later one waits this much
/// longer (10 ms under test).
const RETRY_DELAY: Duration = Duration::from_millis(if cfg!(test) { 10 } else { 1000 });

/// Whether a failed download may succeed when tried again: a 5xx answer, a
/// connection that could not be made, dropped or timed out, or a body that
/// stalled past [`ModelStore::body_timeout`]. A wrong size or checksum, a
/// 4xx answer and a disk error fail at once.
fn is_transient(error: &SpeechError) -> bool {
    let dropped = |error: &std::io::Error| {
        use std::io::ErrorKind::{
            ConnectionAborted, ConnectionRefused, ConnectionReset, TimedOut, UnexpectedEof,
        };
        matches!(
            error.kind(),
            ConnectionAborted | ConnectionRefused | ConnectionReset | TimedOut | UnexpectedEof
        )
    };
    match error {
        SpeechError::Download { source, .. } => match source.as_ref() {
            ureq::Error::StatusCode(status) => (500..600).contains(status),
            ureq::Error::ConnectionFailed | ureq::Error::Timeout(_) => true,
            ureq::Error::Io(error) => dropped(error),
            _ => false,
        },
        // The body is read through `std::io`; its timeout arrives as
        // `io::Error::other(ureq::Error::Timeout(_))`, of kind `Other`.
        SpeechError::Io { source, .. } => {
            dropped(source)
                || source
                    .get_ref()
                    .and_then(|inner| inner.downcast_ref::<ureq::Error>())
                    .is_some_and(|inner| matches!(inner, ureq::Error::Timeout(_)))
        }
        _ => false,
    }
}

/// Another process's partial download nothing has written to for this long
/// is stale; a live download writes it many times a second. A download
/// waiting for a lock gives up on a holder whose partial has not changed
/// for at least as long ([`ModelStore::lock_wait_limit`]).
const STALE_PARTIAL: Duration = Duration::from_secs(10 * 60);

/// Numbers this process's downloads, so each has a partial file of its own.
static NEXT_CALL: AtomicU64 = AtomicU64::new(0);

/// `<name>.partial.`, followed by `<pid>.<call>`.
fn partial_prefix(name: &str) -> String {
    format!("{name}.partial.")
}

/// How often a download that waits for another one's lock tries it again
/// and reports the other one's progress.
const LOCK_POLL: Duration = Duration::from_millis(200);

/// The time a download that waits for another one's lock goes by: the
/// system's, or under test one the test moves on, so that a wait of
/// [`STALE_PARTIAL`] or longer takes no time and no wait depends on how
/// fast the machine runs the test.
#[derive(Debug, Clone)]
enum Clock {
    System,
    #[cfg(test)]
    Manual(std::sync::Arc<tests::ManualClock>),
}

impl Clock {
    fn now(&self) -> Instant {
        match self {
            Clock::System => Instant::now(),
            #[cfg(test)]
            Clock::Manual(clock) => clock.now(),
        }
    }

    fn sleep(&self, duration: Duration) {
        match self {
            Clock::System => std::thread::sleep(duration),
            #[cfg(test)]
            Clock::Manual(clock) => clock.sleep(),
        }
    }
}

/// The file a download writes, with the bytes it holds and their hash.
/// `<name>.partial`, opened under the lock on `<name>.lock`, survives a
/// failed call for the next one to resume; a per-call partial is deleted
/// with it. A partial that ends up wrong is deleted either way.
struct Partial {
    path: PathBuf,
    file: File,
    /// Bytes in the file, all of them hashed into `hasher`.
    len: u64,
    hasher: Sha256,
    /// `<name>.partial` under this call's lock, rather than a per-call file.
    resumable: bool,
    /// The bytes in the file predate the current attempt.
    resumed: bool,
    /// Renamed into place, or left to the download that did.
    done: bool,
    /// `<name>.lock`, locked; `None` for a per-call partial. Declared last,
    /// so it is closed, and the lock released, after the partial is
    /// deleted or renamed.
    _lock: Option<DownloadLock>,
}

impl Partial {
    /// `<name>.partial` beside `destination` with its bytes hashed, opened
    /// once the lock on `<name>.lock` is held; `None` when `destination`
    /// is installed by then. A download of the same file that holds the
    /// lock is waited for on `clock`, `progress` reporting the bytes its
    /// partial has, so its bytes are never fetched twice, until it has
    /// written nothing for `limit`. Where the file system has no locks, a
    /// per-call partial of its own. Blocking: the callers run on a blocking
    /// thread.
    fn open(
        destination: &Path,
        file: &ModelFile,
        clock: &Clock,
        limit: Duration,
        progress: &mut dyn FnMut(DownloadProgress<'_>),
    ) -> Result<Option<Self>, SpeechError> {
        let path = beside(destination, ".partial");
        let Some(lock) = lock_download(destination, file, &path, clock, limit, progress)? else {
            return Self::per_call(destination).map(Some);
        };
        if is_complete(destination, file) {
            // Installed by the download this one waited for, or by another
            // between `missing_files` and the lock. Nothing at
            // `<name>.partial` is needed now, so it goes while the lock is
            // held.
            let _ = fs::remove_file(&path);
            return Ok(None);
        }
        let handle = open_or_create(&path).map_err(|e| SpeechError::io(&path, e))?;
        let mut partial = Partial::new(path, handle, Some(lock));
        partial.adopt(file.size)?;
        Ok(Some(partial))
    }

    fn new(path: PathBuf, file: File, lock: Option<DownloadLock>) -> Self {
        Partial {
            path,
            file,
            len: 0,
            hasher: Sha256::new(),
            resumable: lock.is_some(),
            resumed: false,
            done: false,
            _lock: lock,
        }
    }

    /// Hashes what an earlier call left; a file longer than the manifest
    /// size cannot be a prefix and is emptied.
    fn adopt(&mut self, size: u64) -> Result<(), SpeechError> {
        let len = self
            .file
            .metadata()
            .map_err(|e| SpeechError::io(&self.path, e))?
            .len();
        if len > size {
            return self.restart();
        }
        if len > 0 {
            tracing::info!(path = %self.path.display(), len, "resuming a model download");
        }
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|e| SpeechError::io(&self.path, e))?;
        self.len = hash_into(&mut self.hasher, (&self.file).take(len))
            .map_err(|e| SpeechError::io(&self.path, e))?;
        self.file
            .seek(SeekFrom::Start(self.len))
            .map_err(|e| SpeechError::io(&self.path, e))?;
        Ok(())
    }

    /// Creates this call's partial download beside `destination`,
    /// `<name>.partial.<pid>.<call>`. A name that exists already, left by
    /// a dead process that had the same pid, is passed over for the next
    /// call number.
    fn per_call(destination: &Path) -> Result<Self, SpeechError> {
        loop {
            let path = beside(
                destination,
                &format!(
                    ".partial.{}.{}",
                    std::process::id(),
                    NEXT_CALL.fetch_add(1, Ordering::Relaxed)
                ),
            );
            match File::options().write(true).create_new(true).open(&path) {
                Ok(file) => return Ok(Partial::new(path, file, None)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(SpeechError::io(&path, e)),
            }
        }
    }

    fn append(&mut self, bytes: &[u8]) -> Result<(), SpeechError> {
        self.file
            .write_all(bytes)
            .map_err(|e| SpeechError::io(&self.path, e))?;
        self.hasher.update(bytes);
        self.len += bytes.len() as u64;
        Ok(())
    }

    /// Empties the file for a download from zero.
    fn restart(&mut self) -> Result<(), SpeechError> {
        self.file
            .set_len(0)
            .and_then(|()| self.file.seek(SeekFrom::Start(0)).map(drop))
            .map_err(|e| SpeechError::io(&self.path, e))?;
        self.len = 0;
        self.hasher = Sha256::new();
        self.resumed = false;
        Ok(())
    }

    /// Marks the bytes as unwanted, so the file is deleted even if
    /// resumable.
    fn discard(&mut self) {
        self.resumable = false;
    }
}

impl Drop for Partial {
    /// Deletes a per-call or discarded partial, and an empty one. Runs
    /// before the fields drop, so `<name>.partial` goes while the lock is
    /// still held.
    fn drop(&mut self) {
        if !self.done && (!self.resumable || self.len == 0) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// The `<name>.lock` files this process's downloads hold. A file lock
/// alone does not keep two downloads of one process apart where the file
/// system emulates it with locks the process owns (NFS and CIFS on Linux),
/// and closing any handle to the file there releases them; so a path in
/// this set is neither locked again nor even opened.
static HELD: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());

/// `<name>.lock`, locked by a download of this process.
struct DownloadLock {
    file: Option<File>,
    path: PathBuf,
}

impl Drop for DownloadLock {
    /// Closes the file, releasing the lock, before the path leaves
    /// [`HELD`], so no other download of this process opens it meanwhile.
    fn drop(&mut self) {
        drop(self.file.take());
        HELD.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.path);
    }
}

/// Why [`try_lock_download`] returned no lock.
enum NotLocked {
    /// Another download, of this process or another, holds it.
    Busy,
    /// The lock file could not be opened or created.
    Open(std::io::Error),
    /// Locking it failed: an interrupted call, or a file system without
    /// locks.
    Lock(std::io::Error),
}

/// Locks `<name>.lock` at `path`, creating it, unless a download holds it.
fn try_lock_download(path: &Path) -> Result<DownloadLock, NotLocked> {
    let mut held = HELD.lock().unwrap_or_else(PoisonError::into_inner);
    if held.contains(path) {
        return Err(NotLocked::Busy);
    }
    let file = open_or_create(path).map_err(NotLocked::Open)?;
    match file.try_lock() {
        Ok(()) => {
            held.insert(path.to_path_buf());
            Ok(DownloadLock {
                file: Some(file),
                path: path.to_path_buf(),
            })
        }
        Err(TryLockError::WouldBlock) => Err(NotLocked::Busy),
        Err(TryLockError::Error(error)) => Err(NotLocked::Lock(error)),
    }
}

/// Takes the lock on `<name>.lock` beside `destination`, which every
/// download of the file holds while it opens, writes and renames
/// `partial`. The lock file stays when the download ends: were it deleted,
/// a download waiting on it would wake holding a lock on a file no longer
/// at the path, beside a third that locked a new one. A lock held by
/// another download is tried again every [`LOCK_POLL`] of `clock`,
/// `progress` reporting the length of `partial` meanwhile, until `partial`
/// has not changed for `limit` (the holder was stopped, say), which is an
/// error naming the lock. `None` where the file system has no locks.
fn lock_download(
    destination: &Path,
    file: &ModelFile,
    partial: &Path,
    clock: &Clock,
    limit: Duration,
    progress: &mut dyn FnMut(DownloadProgress<'_>),
) -> Result<Option<DownloadLock>, SpeechError> {
    let path = beside(destination, ".lock");
    let mut waiting = false;
    let mut seen: Option<(u64, Instant)> = None;
    loop {
        match try_lock_download(&path) {
            Ok(lock) => return Ok(Some(lock)),
            Err(NotLocked::Busy) => {
                if !waiting {
                    tracing::info!(path = %path.display(), "another download of this file is running, waiting for it");
                    waiting = true;
                }
                let len = fs::metadata(partial).map_or(0, |m| m.len());
                match seen {
                    Some((last, since)) if last == len => {
                        if clock.now().duration_since(since) >= limit {
                            return Err(SpeechError::io(
                                &path,
                                std::io::Error::new(
                                    std::io::ErrorKind::TimedOut,
                                    format!(
                                        "another download of this file holds the lock and has written nothing for {} s",
                                        limit.as_secs()
                                    ),
                                ),
                            ));
                        }
                    }
                    _ => seen = Some((len, clock.now())),
                }
                progress(DownloadProgress {
                    file: &file.name,
                    received: len.min(file.size),
                    total: file.size,
                });
                clock.sleep(LOCK_POLL);
            }
            Err(NotLocked::Open(error)) => return Err(SpeechError::io(&path, error)),
            Err(NotLocked::Lock(error)) if error.kind() == std::io::ErrorKind::Interrupted => {}
            // Once another download was seen to hold the lock, a failure
            // is not a file system without locks: going on without the
            // lock would fetch the file a second time beside it.
            Err(NotLocked::Lock(error)) if waiting => {
                return Err(SpeechError::io(&path, error));
            }
            Err(NotLocked::Lock(error)) => {
                tracing::debug!(%error, path = %path.display(), "no file locks here, the download will not resume");
                return Ok(None);
            }
        }
    }
}

/// `destination` with `suffix` after its file name, in the same folder:
/// `<name>.lock`, `<name>.partial`, `<name>.partial.<pid>.<call>`.
fn beside(destination: &Path, suffix: &str) -> PathBuf {
    let mut path = destination.as_os_str().to_owned();
    path.push(suffix);
    PathBuf::from(path)
}

/// Opens `path` to read and write, creating it, keeping what it holds:
/// `<name>.lock` and `<name>.partial`.
fn open_or_create(path: &Path) -> std::io::Result<File> {
    File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

/// Deletes `<name>.partial` in `directory` once `<name>` is installed: a
/// download killed before the file arrived another way (by hand, from a
/// copy) leaves it behind, up to 2.4 GB. Only under the lock on
/// `<name>.lock`, so a download still writing it keeps it. Best effort,
/// like [`remove_stale_partials`].
fn remove_finished_partial(directory: &Path, name: &str) {
    let path = directory.join(format!("{name}.partial"));
    if !path.exists() {
        return;
    }
    if let Ok(_lock) = try_lock_download(&directory.join(format!("{name}.lock"))) {
        let _ = fs::remove_file(&path);
    }
}

/// The pid in `file_name` when it is a partial download of the file
/// `prefix` belongs to, `<prefix><pid>.<call>`; `None` for any other name.
fn partial_pid(file_name: &str, prefix: &str) -> Option<u32> {
    let (pid, call) = file_name.strip_prefix(prefix)?.split_once('.')?;
    call.parse::<u64>().ok()?;
    pid.parse().ok()
}

/// Removes the stale partial downloads of `name` in `directory`, which a
/// killed process leaves behind (2.4 GB for `encoder.weights`). This
/// process's own partials belong to downloads still running and are never
/// removed. Another process's is removed after [`STALE_PARTIAL`] without a
/// write; should that process still be alive, its rename then fails and
/// nothing is installed. Best effort: a file that cannot be read or
/// removed stays.
fn remove_stale_partials(directory: &Path, name: &str) {
    // A file in a folder of the asset has its partials in that folder.
    let path = directory.join(name);
    let (Some(folder), Some(base)) = (path.parent(), path.file_name()) else {
        return;
    };
    let prefix = partial_prefix(&base.to_string_lossy());
    let Ok(entries) = fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(pid) = partial_pid(&entry.file_name().to_string_lossy(), &prefix) else {
            continue;
        };
        if pid == std::process::id() {
            continue;
        }
        // Not `DirEntry::metadata`: on Windows that is the cached directory
        // entry, whose write time can lag for a file open for writing.
        let stale = fs::symlink_metadata(entry.path())
            .and_then(|m| m.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age >= STALE_PARTIAL);
        if stale {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// Syncs the directory holding `path`, so the rename survives a power loss.
/// Best effort: some FUSE and CIFS mounts refuse to sync a directory, and
/// the file itself is synced already, so a failure is logged and the
/// install stands. Not on Windows, which cannot open a directory as a file;
/// NTFS journals the rename.
fn sync_parent(path: &Path) {
    if cfg!(unix)
        && let Some(parent) = path.parent()
        && let Err(error) = File::open(parent).and_then(|directory| directory.sync_all())
    {
        tracing::warn!(
            directory = %parent.display(),
            %error,
            "could not sync the models directory after installing a file"
        );
    }
}

/// Compares `actual` with `file`'s manifest digest; `path` names the file
/// in the error.
fn check_digest(file: &ModelFile, path: &Path, actual: String) -> Result<(), SpeechError> {
    if actual.eq_ignore_ascii_case(&file.sha256) {
        Ok(())
    } else {
        Err(SpeechError::Checksum {
            path: path.to_path_buf(),
            expected: file.sha256.clone(),
            actual,
        })
    }
}

/// Streams `input` into `hasher` and returns the bytes read.
fn hash_into(hasher: &mut Sha256, input: impl Read) -> std::io::Result<u64> {
    std::io::copy(&mut BufReader::with_capacity(READ_BUFFER, input), hasher)
}

/// The lower-case hex SHA-256 of a file, streamed.
pub fn sha256_of(path: &Path) -> Result<String, SpeechError> {
    let mut hasher = Sha256::new();
    File::open(path)
        .and_then(|file| hash_into(&mut hasher, file))
        .map_err(|e| SpeechError::io(path, e))?;
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use std::io::BufRead;
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex, mpsc};

    use super::*;

    /// A whole HTTP response carrying `body`.
    fn response(status: &str, body: &[u8]) -> Vec<u8> {
        let head = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        [head.as_bytes(), body].concat()
    }

    /// Answers one connection per entry of `responses`, in order, with
    /// those bytes (none: the connection is closed unanswered), and returns
    /// the URL.
    fn serve(responses: Vec<Vec<u8>>) -> String {
        let handlers = responses
            .into_iter()
            .map(|bytes| Box::new(move |stream: &TcpStream, _| send(stream, &bytes)) as Handler)
            .collect();
        serve_each(handlers).0
    }

    /// Writes `bytes` to `stream`, whether or not the client still reads.
    fn send(mut stream: &TcpStream, bytes: &[u8]) {
        let _ = stream.write_all(bytes);
    }

    /// Serves `body` once and returns the URL.
    fn serve_once(body: &[u8]) -> String {
        serve(vec![response("200 OK", body)])
    }

    /// Answers one connection per entry of `bodies`, in order of arrival,
    /// with a `200` carrying that body, whatever the range asked: the first
    /// `head` bytes at once, the rest once the k-th sender returned fires
    /// or is dropped. Returns the URL, the senders and the `Range` header
    /// of every request so far.
    fn serve_held(bodies: Vec<(Vec<u8>, usize)>) -> (String, Vec<mpsc::Sender<()>>, Requests) {
        let (senders, handlers): (Vec<_>, Vec<Handler>) = bodies
            .into_iter()
            .map(|(body, head)| {
                let (sender, release) = mpsc::channel();
                let handler: Handler =
                    Box::new(move |stream, _| answer(stream, &body, None, Some((head, release))));
                (sender, handler)
            })
            .unzip();
        let (url, requests) = serve_each(handlers);
        (url, senders, requests)
    }

    /// What a connection of [`serve_each`] does: its stream and the
    /// request's `Range` header.
    type Handler = Box<dyn FnOnce(&TcpStream, Option<String>) + Send>;

    /// The `Range` header of every request a test server has read.
    type Requests = Arc<Mutex<Vec<Option<String>>>>;

    /// Answers one connection per handler, in order of arrival, each on a
    /// thread of its own, and returns the URL and the `Range` header of
    /// every request so far.
    fn serve_each(handlers: Vec<Handler>) -> (String, Requests) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let ranges = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&ranges);
        std::thread::spawn(move || {
            for handler in handlers {
                let (stream, _) = listener.accept().unwrap();
                let range = read_request_head(&stream);
                seen.lock().unwrap().push(range.clone());
                std::thread::spawn(move || handler(&stream, range));
            }
        });
        (format!("http://{address}/model.onnx"), ranges)
    }

    /// The reply to `range` (`bytes=<first>-[<last>]`): the part it asks
    /// for under `206`, or `body` under `200` without one. Returns the
    /// reply's bytes and the length of its body.
    fn reply(body: &[u8], range: Option<&str>) -> (Vec<u8>, usize) {
        match range.and_then(bounds) {
            Some((first, last)) => {
                let last = last.map_or(body.len() - 1, |l| l.min(body.len() - 1));
                let part = &body[first..=last];
                let head = format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {first}-{last}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len(),
                    part.len()
                );
                ([head.as_bytes(), part].concat(), part.len())
            }
            None => (response("200 OK", body), body.len()),
        }
    }

    /// The first byte of `range` (`bytes=<first>-[<last>]`) and its last,
    /// when it names one.
    fn bounds(range: &str) -> Option<(usize, Option<usize>)> {
        let (first, last) = range.strip_prefix("bytes=")?.split_once('-')?;
        Some((first.parse().ok()?, last.parse().ok()))
    }

    /// [`reply`] to `range`, all of it if `stall` is `None`, else that
    /// many bytes of the body at once and the rest once `stall` fires or
    /// is dropped.
    fn answer(
        stream: &TcpStream,
        body: &[u8],
        range: Option<&str>,
        stall: Option<(usize, mpsc::Receiver<()>)>,
    ) {
        let (bytes, length) = reply(body, range);
        let (sent, release) = match stall {
            Some((sent, release)) => (sent, Some(release)),
            None => (length, None),
        };
        let (now, later) = bytes.split_at(bytes.len() - length + sent);
        send(stream, now);
        if let Some(release) = release {
            let _ = release.recv();
        }
        send(stream, later);
    }

    /// Reads a request up to the blank line that ends its head and
    /// returns its `Range` header.
    fn read_request_head(stream: &TcpStream) -> Option<String> {
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        let mut range = None;
        while reader.read_line(&mut line).unwrap() > 0 && line != "\r\n" {
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("range")
            {
                range = Some(value.trim().to_owned());
            }
            line.clear();
        }
        range
    }

    fn asset(url: Option<String>, body: &[u8], sha256: &str) -> ModelAsset {
        ModelAsset {
            id: "test-asset".to_owned(),
            display_name: "Test".to_owned(),
            licence: "MIT".to_owned(),
            attribution: String::new(),
            files: vec![ModelFile {
                name: "model.onnx".to_owned(),
                source: url.map(ModelSource::Url),
                sha256: sha256.to_owned(),
                size: body.len() as u64,
            }],
        }
    }

    fn digest(body: &[u8]) -> String {
        format!("{:x}", Sha256::digest(body))
    }

    /// What a download on a [`ManualClock`] tells its test.
    #[derive(Debug)]
    enum Step {
        /// It waits for a lock, asleep until the test moves the clock on.
        Asleep,
        /// `ensure` returned.
        Done(Result<PathBuf, SpeechError>),
    }

    /// The clock of [`Clock::Manual`]. `sleep` tells the test and blocks
    /// until the test says how far the clock moves on.
    #[derive(Debug)]
    pub(super) struct ManualClock {
        now: Mutex<Instant>,
        steps: mpsc::Sender<Step>,
        moves: Mutex<mpsc::Receiver<Duration>>,
    }

    impl ManualClock {
        pub(super) fn now(&self) -> Instant {
            *self.now.lock().unwrap()
        }

        /// Panics once the test has gone, which ends the download's thread
        /// rather than leave it polling.
        pub(super) fn sleep(&self) {
            let gone = "the test stopped driving the clock";
            self.steps.send(Step::Asleep).expect(gone);
            let by = self.moves.lock().unwrap().recv().expect(gone);
            *self.now.lock().unwrap() += by;
        }
    }

    /// A download of [`drive`], seen from its test.
    struct Driven {
        steps: mpsc::Receiver<Step>,
        moves: mpsc::Sender<Duration>,
        progress: mpsc::Receiver<u64>,
    }

    /// Runs `ensure` of `asset` on a thread of its own, on a copy of
    /// `store` whose clock the test moves.
    fn drive(store: &ModelStore, asset: &ModelAsset) -> Driven {
        let (step, steps) = mpsc::channel();
        let (move_on, moves) = mpsc::channel();
        let (report, progress) = mpsc::channel();
        let mut store = store.clone();
        store.clock = Clock::Manual(Arc::new(ManualClock {
            now: Mutex::new(Instant::now()),
            steps: step.clone(),
            moves: Mutex::new(moves),
        }));
        let asset = asset.clone();
        std::thread::spawn(move || {
            let result = store.ensure(&asset, &mut |p| {
                let _ = report.send(p.received);
            });
            let _ = step.send(Step::Done(result));
        });
        Driven {
            steps,
            moves: move_on,
            progress,
        }
    }

    impl Driven {
        /// The next step. A minute without one fails the test instead of
        /// hanging it; no step takes more than a moment.
        fn next(&self) -> Step {
            self.steps
                .recv_timeout(Duration::from_secs(60))
                .expect("the download neither waited nor returned")
        }

        /// Expects the download to wait for its lock.
        fn asleep(&self) {
            if let Step::Done(result) = self.next() {
                panic!("the download returned instead of waiting: {result:?}");
            }
        }

        fn move_on(&self, by: Duration) {
            self.moves.send(by).unwrap();
        }

        /// Expects `ensure` to return, without waiting again.
        fn done(&self) -> Result<PathBuf, SpeechError> {
            match self.next() {
                Step::Done(result) => result,
                Step::Asleep => panic!("the download waited again"),
            }
        }

        /// The progress reported since the last call.
        fn reported(&self) -> Vec<u64> {
            self.progress.try_iter().collect()
        }

        /// Waits for a report of at least `bytes`.
        fn received(&self, bytes: u64) {
            while self
                .progress
                .recv_timeout(Duration::from_secs(60))
                .expect("the download stopped reporting")
                < bytes
            {}
        }
    }

    #[test]
    fn a_download_is_verified_and_moved_into_place() {
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path());
        let body = b"not really a model".to_vec();
        let asset = asset(Some(serve_once(&body)), &body, &digest(&body));
        assert!(!store.is_installed(&asset));
        assert_eq!(store.missing_files(&asset), ["model.onnx"]);
        let mut reports = Vec::new();
        let mut partials = Vec::new();
        let directory = store
            .ensure(&asset, &mut |p| {
                reports.push((p.received, p.total));
                partials.extend(names(&store, &asset));
            })
            .unwrap();
        // The download writes the resumable partial, not the target.
        assert!(
            !partials.is_empty()
                && partials
                    .iter()
                    .all(|name| name == "model.onnx.partial" || name == "model.onnx.lock"),
            "{partials:?}"
        );
        assert_eq!(directory, dir.path().join("test-asset"));
        assert_eq!(fs::read(directory.join("model.onnx")).unwrap(), body);
        assert!(store.is_installed(&asset));
        assert_eq!(
            reports.last(),
            Some(&(body.len() as u64, body.len() as u64))
        );
        store.verify(&asset).unwrap();
        // A second ensure is a no-op (the server is gone).
        store.ensure(&asset, &mut |_| {}).unwrap();
        // Removing the asset takes its lock file with it.
        assert_eq!(names(&store, &asset), ["model.onnx", "model.onnx.lock"]);
        store.remove(&asset).unwrap();
        assert!(!directory.exists());
    }

    /// The names in the asset's directory, sorted.
    fn names(store: &ModelStore, asset: &ModelAsset) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(store.directory(asset))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_second_download_of_one_file_waits_for_the_first_and_fetches_nothing() {
        // The second download starts while the first is half done. It
        // waits for the lock, reporting the first one's progress, finds
        // the file installed and returns without a request: the bytes cross
        // the wire once.
        const WAIT: Duration = Duration::from_secs(30);
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path());
        let body: Vec<u8> = (0..2_000_000u32).map(|i| (i % 251) as u8 + 1).collect();
        let (url, release, requests) = serve_held(vec![(body.clone(), 50_000), (body.clone(), 0)]);
        let asset = asset(Some(url), &body, &digest(&body));
        let (first_started, first_waits) = mpsc::channel();
        let mut first_received = 0;
        std::thread::scope(|scope| {
            // Owned here, so a failed assertion drops the senders and frees
            // the held downloads instead of leaving the scope waiting.
            let release = release;
            let first = scope.spawn(|| {
                store.ensure(&asset, &mut |p| {
                    first_received = p.received;
                    if p.received >= 50_000 {
                        let _ = first_started.send(());
                    }
                })
            });
            first_waits.recv_timeout(WAIT).unwrap();
            let second = drive(&store, &asset);
            second.asleep();
            // While it waits, the second shows what the first has.
            let seen = second.reported();
            assert!(seen.iter().any(|&r| r >= 50_000), "{seen:?}");
            release[0].send(()).unwrap();
            first.join().unwrap().unwrap();
            // Frees a second transfer, should one have started.
            drop(release);
            second.move_on(Duration::ZERO);
            second.done().unwrap();
        });
        assert_eq!(first_received, body.len() as u64);
        assert_eq!(
            requests.lock().unwrap().len(),
            1,
            "the second download fetched the file again"
        );
        store.verify(&asset).unwrap();
        assert_eq!(names(&store, &asset), ["model.onnx", "model.onnx.lock"]);
    }

    #[test]
    fn a_download_that_waited_installs_the_file_after_the_first_threw_its_partial_away() {
        // The first download gets wrong bytes, fails its checksum and
        // deletes `<name>.partial` while it holds the lock.
        // The second, which waited, must write a partial at the path, not
        // the deleted one, and install it; a third that arrives meanwhile
        // waits for the second and fetches nothing (its connection would
        // carry wrong bytes).
        const WAIT: Duration = Duration::from_secs(30);
        let good: Vec<u8> = (0..1_000_000u32).map(|i| (i % 251) as u8).collect();
        let wrong: Vec<u8> = good.iter().map(|b| b ^ 0xff).collect();
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path());
        let (url, release, requests) = serve_held(vec![
            (wrong.clone(), 50_000),
            (good.clone(), 10_000),
            (wrong, 0),
        ]);
        let mut asset = asset(Some(url), &good, &digest(&good));
        let (first_started, first_waits) = mpsc::channel();
        std::thread::scope(|scope| {
            let release = release;
            let first = scope.spawn(|| {
                store.ensure(&asset, &mut |p| {
                    if p.received >= 50_000 {
                        let _ = first_started.send(());
                    }
                })
            });
            first_waits.recv_timeout(WAIT).unwrap();
            let second = drive(&store, &asset);
            second.asleep();
            // What it showed of the first's partial while waiting.
            second.reported();
            release[0].send(()).unwrap();
            let first = first.join().unwrap();
            assert!(
                matches!(first, Err(SpeechError::Checksum { .. })),
                "{first:?}"
            );
            second.move_on(Duration::ZERO);
            // The second has the lock and its own request.
            second.received(10_000);
            let third = drive(&store, &asset);
            third.asleep();
            release[1].send(()).unwrap();
            second.done().unwrap();
            drop(release);
            third.move_on(Duration::ZERO);
            third.done().unwrap();
        });
        assert_eq!(requests.lock().unwrap().len(), 2);
        store.verify(&asset).unwrap();
        assert_eq!(names(&store, &asset), ["model.onnx", "model.onnx.lock"]);
        // The lock file counts for nothing: removed, the file is missing.
        fs::remove_file(store.directory(&asset).join("model.onnx")).unwrap();
        assert_eq!(store.missing_files(&asset), ["model.onnx"]);
        asset.files[0].source = None;
        assert!(matches!(
            store.ensure(&asset, &mut |_| {}),
            Err(SpeechError::NotHosted { .. })
        ));
    }

    #[test]
    fn a_download_resumes_what_a_failed_one_left() {
        // The first call is cut half way and then refused (a 404 is no
        // reason to try again): it fails and leaves `<name>.partial`. The
        // next call asks for the rest only.
        let body: Vec<u8> = (0..100_000u32).map(|i| (i % 249) as u8).collect();
        let half = body.len() / 2;
        let (cut, b) = (body.clone(), body.clone());
        let (url, ranges) = serve_each(vec![
            Box::new(move |stream, _| {
                let bytes = response("200 OK", &cut);
                send(stream, &bytes[..bytes.len() - half]);
            }),
            Box::new(|stream, _| send(stream, &response("404 Not Found", b""))),
            Box::new(move |stream, range| answer(stream, &b, range.as_deref(), None)),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path());
        let asset = asset(Some(url), &body, &digest(&body));
        let error = store.ensure(&asset, &mut |_| {}).unwrap_err();
        assert!(matches!(error, SpeechError::Download { .. }), "{error}");
        let partial = store.directory(&asset).join("model.onnx.partial");
        assert_eq!(fs::metadata(&partial).unwrap().len(), half as u64);
        store.ensure(&asset, &mut |_| {}).unwrap();
        store.verify(&asset).unwrap();
        assert_eq!(
            *ranges.lock().unwrap(),
            [
                None,
                Some(format!("bytes={half}-")),
                Some(format!("bytes={half}-"))
            ]
        );
        assert!(!partial.exists());
    }

    #[test]
    fn a_5xx_answer_or_a_dropped_connection_is_tried_again_a_few_times() {
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path());
        let body = b"not really a model".to_vec();
        let ok = response("200 OK", &body);
        let unavailable = response("503 Service Unavailable", b"");
        let url = serve(vec![unavailable.clone(), Vec::new(), ok.clone()]);
        let flaky = asset(Some(url), &body, &digest(&body));
        store.ensure(&flaky, &mut |_| {}).unwrap();
        store.verify(&flaky).unwrap();
        store.remove(&flaky).unwrap();
        // The attempts are bounded, and a 4xx answer is final.
        let attempts = DOWNLOAD_ATTEMPTS as usize;
        let mut responses = vec![unavailable; attempts];
        responses.push(ok.clone());
        let down = asset(Some(serve(responses)), &body, &digest(&body));
        let error = store.ensure(&down, &mut |_| {}).unwrap_err();
        assert!(matches!(&error, SpeechError::Download { .. }), "{error}");
        let url = serve(vec![response("404 Not Found", b""), ok]);
        let missing = asset(Some(url), &body, &digest(&body));
        let error = store.ensure(&missing, &mut |_| {}).unwrap_err();
        assert!(matches!(&error, SpeechError::Download { .. }), "{error}");
        assert!(!store.is_installed(&missing));
        // A partial without a byte in it is not kept.
        assert_eq!(names(&store, &missing), ["model.onnx.lock"]);
    }

    #[test]
    fn a_body_that_stalls_times_out_and_is_tried_again() {
        // The first answer sends half the body and then nothing; the second
        // sends it all. Without the body timeout the first read never ends.
        // The timeout bounds the second, good answer too, so it is long
        // enough for a slow machine to read 18 bytes.
        let dir = tempfile::tempdir().unwrap();
        let mut store = ModelStore::new(dir.path());
        store.min_body_timeout = Duration::from_secs(2);
        let body = b"not really a model".to_vec();
        let (url, release, _) = serve_held(vec![
            (body.clone(), body.len() / 2),
            (body.clone(), body.len()),
        ]);
        let asset = asset(Some(url), &body, &digest(&body));
        let result = ensure_within(
            &store,
            &asset,
            Duration::from_secs(30),
            "the stalled body was never given up",
        );
        drop(release);
        result.unwrap();
        store.verify(&asset).unwrap();
    }

    /// `ensure` of `asset` on a thread of its own; fails the test with
    /// `hung` when it has not returned after `limit`.
    fn ensure_within(
        store: &ModelStore,
        asset: &ModelAsset,
        limit: Duration,
        hung: &str,
    ) -> Result<PathBuf, SpeechError> {
        let (done, finished) = mpsc::channel();
        let (store, asset) = (store.clone(), asset.clone());
        std::thread::spawn(move || {
            let _ = done.send(store.ensure(&asset, &mut |_| {}));
        });
        finished.recv_timeout(limit).expect(hung)
    }

    #[test]
    fn a_large_file_comes_in_chunks_and_a_chunk_cut_short_is_resumed_alone() {
        // 64 chunks of 64 KiB. Three answers end the connection half way
        // through their chunk, more often than `DOWNLOAD_ATTEMPTS`, but
        // each cut follows progress, so the download goes on at once from
        // where it stopped.
        const CHUNK: u32 = 64 << 10;
        let body: Vec<u8> = (0..64 * CHUNK)
            .map(|i| (i.wrapping_mul(7919) % 251) as u8)
            .collect();
        let shared = Arc::new(body.clone());
        let cut = [2, 10, 20];
        let handlers: Vec<Handler> = (0..80)
            .map(|k| {
                let body = Arc::clone(&shared);
                let cut = cut.contains(&k);
                Box::new(move |stream: &TcpStream, range: Option<String>| {
                    if cut {
                        let (bytes, length) = reply(&body, range.as_deref());
                        send(stream, &bytes[..bytes.len() - length / 2]);
                    } else {
                        answer(stream, &body, range.as_deref(), None);
                    }
                }) as Handler
            })
            .collect();
        let (url, ranges) = serve_each(handlers);
        let (store, asset, _dir) = chunked(url, &body);
        store.ensure(&asset, &mut |_| {}).unwrap();
        store.verify(&asset).unwrap();
        let ranges = ranges.lock().unwrap();
        // Each cut leaves half a chunk, so one more request in all.
        assert_eq!(ranges.len(), 63 + cut.len(), "{ranges:?}");
        assert_eq!(ranges[0].as_deref(), Some("bytes=0-65535"));
        // A chunk cut short goes on from where it stopped.
        assert_eq!(ranges[3].as_deref(), Some("bytes=163840-229375"));
        assert_eq!(ranges.last().unwrap().as_deref(), Some("bytes=4161536-"));
    }

    /// `range` (`bytes=<first>-[<last>]`) cut to at most `cap` bytes.
    fn capped(range: &str, cap: usize) -> String {
        let (first, last) = bounds(range).unwrap();
        let last = last.unwrap_or(usize::MAX);
        format!("bytes={first}-{}", last.min(first.saturating_add(cap - 1)))
    }

    /// Answers each request with the part it asks for, at most `cap`
    /// bytes of it.
    fn capping(body: &Arc<Vec<u8>>, cap: usize) -> Handler {
        let body = Arc::clone(body);
        Box::new(move |stream, range| {
            let range = range.map(|r| capped(&r, cap));
            answer(stream, &body, range.as_deref(), None);
        })
    }

    /// A store of 64 KiB chunks and an asset of `body` at `url`.
    fn chunked(url: String, body: &[u8]) -> (ModelStore, ModelAsset, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mut store = ModelStore::new(dir.path());
        store.chunk = 65_536;
        let asset = asset(Some(url), body, &digest(body));
        (store, asset, dir)
    }

    #[test]
    fn a_host_that_sends_less_than_a_range_asks_for_is_asked_for_the_rest() {
        // Every answer stops after 40,000 bytes, the last one, to the
        // open-ended request for the rest of the file, too.
        let body: Vec<u8> = (0..4 * 65_536u32).map(|i| (i % 239) as u8).collect();
        let shared = Arc::new(body.clone());
        let (url, ranges) = serve_each((0..8).map(|_| capping(&shared, 40_000)).collect());
        let (store, asset, _dir) = chunked(url, &body);
        store.ensure(&asset, &mut |_| {}).unwrap();
        store.verify(&asset).unwrap();
        let ranges = ranges.lock().unwrap();
        assert_eq!(ranges.len(), 7, "{ranges:?}");
        assert_eq!(ranges[1].as_deref(), Some("bytes=40000-105535"));
        assert_eq!(ranges[5].as_deref(), Some("bytes=200000-"));
        assert_eq!(ranges[6].as_deref(), Some("bytes=240000-"));
    }

    #[test]
    fn an_odd_answer_to_a_range_keeps_the_partial() {
        // A sign-in page under `200` and an empty `206` each fetch nothing
        // and are tried again; neither throws the first chunk away.
        let body: Vec<u8> = (0..3 * 65_536u32).map(|i| (i % 233) as u8).collect();
        let shared = Arc::new(body.clone());
        let page: Handler =
            Box::new(|stream, _| send(stream, &response("200 OK", b"<html>sign in</html>")));
        // A `206` that starts where it was asked to and brings nothing.
        let empty = || -> Handler {
            Box::new(|stream, range| {
                let (first, _) = bounds(&range.unwrap()).unwrap();
                send(
                    stream,
                    format!("HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {first}-{first}/196608\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes(),
                );
            })
        };
        let good = || capping(&shared, usize::MAX);
        let (url, ranges) = serve_each(vec![good(), page, empty(), good(), good()]);
        let (store, asset, _dir) = chunked(url, &body);
        store.ensure(&asset, &mut |_| {}).unwrap();
        store.verify(&asset).unwrap();
        assert_eq!(
            *ranges.lock().unwrap(),
            [
                Some("bytes=0-65535".to_owned()),
                Some("bytes=65536-131071".to_owned()),
                Some("bytes=65536-131071".to_owned()),
                Some("bytes=65536-131071".to_owned()),
                Some("bytes=131072-".to_owned()),
            ]
        );
        // A host that keeps sending nothing fails the download after
        // `DOWNLOAD_ATTEMPTS` attempts that fetch nothing, not never, and
        // the chunk stays. The first attempt fetched the chunk, so its
        // empty answer does not count.
        let mut handlers = vec![good()];
        handlers.extend((0..3 * DOWNLOAD_ATTEMPTS).map(|_| empty()));
        let (url, ranges) = serve_each(handlers);
        let (store, asset, _dir) = chunked(url, &body);
        let error = store.ensure(&asset, &mut |_| {}).unwrap_err();
        assert!(error.to_string().contains("sent no bytes"), "{error}");
        assert_eq!(ranges.lock().unwrap().len(), 2 + DOWNLOAD_ATTEMPTS as usize);
        let partial = store.directory(&asset).join("model.onnx.partial");
        assert_eq!(fs::metadata(partial).unwrap().len(), 65_536);
    }

    #[test]
    fn a_range_s_body_timeout_follows_its_length_up_to_the_longest() {
        let store = ModelStore::new("/models");
        let mib = 1 << 20;
        // 64 KiB/s, but never under a minute.
        assert_eq!(store.body_timeout(18), Duration::from_secs(60));
        assert_eq!(store.body_timeout(16 * mib), Duration::from_secs(256));
        // The whole of `encoder.weights`, for a host that ignores `Range`.
        assert_eq!(
            store.body_timeout(2_435_420_160),
            Duration::from_secs(37_161)
        );
        // A chunk of a larger file stops at the longest, 128 s; the last,
        // small one keeps its own.
        let weights = 2_435_420_160;
        assert_eq!(
            store.range_timeout(weights, 0, CHUNK),
            Duration::from_secs(128)
        );
        assert_eq!(
            store.range_timeout(weights, weights - mib, weights),
            Duration::from_secs(60)
        );
        // A file of one chunk, whose range may be answered with all of it,
        // keeps the timeout of its whole size, wherever the range starts.
        assert_eq!(
            store.range_timeout(CHUNK, mib, CHUNK),
            Duration::from_secs(1024)
        );
    }

    #[test]
    fn a_silent_chunk_is_given_up_at_the_longest_timeout_not_its_own() {
        // 1 MiB chunks of a 2 MiB file and a shortest body timeout of an
        // hour: by its length a chunk would have an hour, but none gets
        // more than the longest chunk timeout, here 50 ms. Every answer
        // sends its head and then nothing, so each attempt fetches nothing
        // and the download fails after `DOWNLOAD_ATTEMPTS` of them.
        let body: Vec<u8> = (0..2u32 << 20).map(|i| (i % 229) as u8).collect();
        let shared = Arc::new(body.clone());
        let mut holds = Vec::new();
        let handlers: Vec<Handler> = (0..DOWNLOAD_ATTEMPTS)
            .map(|_| {
                let (hold, wait) = mpsc::channel::<()>();
                holds.push(hold);
                let body = Arc::clone(&shared);
                Box::new(move |stream: &TcpStream, range: Option<String>| {
                    answer(stream, &body, range.as_deref(), Some((0, wait)));
                }) as Handler
            })
            .collect();
        let (url, ranges) = serve_each(handlers);
        let dir = tempfile::tempdir().unwrap();
        let mut store = ModelStore::new(dir.path());
        store.chunk = 1 << 20;
        store.min_body_timeout = Duration::from_secs(3600);
        store.max_chunk_timeout = Duration::from_millis(50);
        let asset = asset(Some(url), &body, &digest(&body));
        // Only a hang guard: the hour would hold the test, 50 ms does not.
        let error = ensure_within(
            &store,
            &asset,
            Duration::from_secs(60),
            "a silent chunk waited for its length's timeout",
        )
        .unwrap_err();
        drop(holds);
        assert!(is_transient(&error), "{error}");
        assert_eq!(
            *ranges.lock().unwrap(),
            vec![Some("bytes=0-1048575".to_owned()); DOWNLOAD_ATTEMPTS as usize]
        );
    }

    #[test]
    fn a_lock_this_process_holds_is_neither_locked_again_nor_opened() {
        // Where locks belong to the process (NFS, CIFS on Linux), the file
        // lock cannot tell two downloads of one process apart; the set of
        // held paths does. Shown here by deleting the lock file: a second
        // attempt would lock a new one.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("model.onnx.lock");
        let held = try_lock_download(&path).ok().unwrap();
        fs::remove_file(&path).unwrap();
        assert!(matches!(try_lock_download(&path), Err(NotLocked::Busy)));
        assert!(!path.exists(), "a held path was opened again");
        drop(held);
        let again = try_lock_download(&path).ok().unwrap();
        assert!(path.exists());
        drop(again);
    }

    /// The lock file at `path`, locked the way another download holds it.
    fn locked(path: &Path) -> File {
        let file = open_or_create(path).unwrap();
        file.lock().unwrap();
        file
    }

    #[test]
    fn a_download_waits_while_the_holder_writes_and_gives_up_once_it_stops() {
        // A holder that keeps writing is waited for however long it takes,
        // here 30 polls five minutes apart, and its bytes are resumed; one
        // that writes nothing (a stopped process) fails the waiter after
        // `STALE_PARTIAL`, with an error naming the lock. The test writes
        // the holder's bytes and moves the waiter's clock.
        let body: Vec<u8> = (0..60_000u32).map(|i| (i % 227) as u8).collect();
        let shared = Arc::new(body.clone());
        let (url, ranges) = serve_each(vec![capping(&shared, usize::MAX)]);
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path());
        let asset = asset(Some(url), &body, &digest(&body));
        let directory = store.directory(&asset);
        fs::create_dir_all(&directory).unwrap();
        let lock = directory.join("model.onnx.lock");
        let partial = directory.join("model.onnx.partial");
        let held = locked(&lock);
        let mut holder = File::create(&partial).unwrap();
        holder.write_all(&body[..1000]).unwrap();
        let waiter = drive(&store, &asset);
        for k in 1..30 {
            waiter.asleep();
            holder.write_all(&body[k * 1000..(k + 1) * 1000]).unwrap();
            waiter.move_on(Duration::from_secs(300));
        }
        waiter.asleep();
        drop(holder);
        drop(held);
        waiter.move_on(Duration::ZERO);
        waiter.done().unwrap();
        store.verify(&asset).unwrap();
        assert_eq!(*ranges.lock().unwrap(), [Some("bytes=30000-".to_owned())]);
        // The holder's progress while waiting, then its own from there.
        let reported = waiter.reported();
        let holders: Vec<u64> = (1..=30).map(|k| k * 1000).collect();
        assert_eq!(reported[..31], [&holders[..], &[30_000]].concat()[..]);
        assert_eq!(reported.last(), Some(&60_000));
        // Stopped: 1000 bytes and then nothing.
        fs::remove_file(directory.join("model.onnx")).unwrap();
        fs::write(&partial, &body[..1000]).unwrap();
        let held = locked(&lock);
        let waiter = drive(&store, &asset);
        waiter.asleep();
        waiter.move_on(STALE_PARTIAL.checked_sub(Duration::from_secs(1)).unwrap());
        waiter.asleep();
        waiter.move_on(Duration::from_secs(1));
        let error = waiter.done().unwrap_err();
        assert!(
            matches!(&error, SpeechError::Io { path, .. } if *path == lock),
            "{error}"
        );
        assert!(
            error.to_string().contains("written nothing for 600 s"),
            "{error}"
        );
        assert_eq!(waiter.reported(), [1000, 1000]);
        assert_eq!(fs::metadata(&partial).unwrap().len(), 1000);
        drop(held);
    }

    #[test]
    fn a_holder_s_growth_starts_the_wait_for_a_stopped_holder_again() {
        // 1000 bytes, then 1000 more at the first poll, then nothing: the
        // waiter gives up `STALE_PARTIAL` after the growth, not after its
        // first look.
        let body: Vec<u8> = (0..10_000u32).map(|i| (i % 227) as u8).collect();
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path());
        let asset = asset(
            Some("http://127.0.0.1:9/never".to_owned()),
            &body,
            &digest(&body),
        );
        let directory = store.directory(&asset);
        fs::create_dir_all(&directory).unwrap();
        let held = locked(&directory.join("model.onnx.lock"));
        let mut holder = File::create(directory.join("model.onnx.partial")).unwrap();
        holder.write_all(&body[..1000]).unwrap();
        let waiter = drive(&store, &asset);
        waiter.asleep();
        holder.write_all(&body[1000..2000]).unwrap();
        let half = STALE_PARTIAL / 2;
        waiter.move_on(half);
        waiter.asleep();
        // `STALE_PARTIAL` since the first look, half of it since the growth.
        waiter.move_on(half);
        waiter.asleep();
        waiter.move_on(half);
        assert!(waiter.done().is_err());
        assert_eq!(waiter.reported(), [1000, 2000, 2000]);
        drop(held);
    }

    #[test]
    fn a_waiter_outlasts_a_holder_whose_one_request_may_still_be_silent() {
        // A file of one chunk comes in one request, which may spend the
        // connect and response timeouts of both hops of a redirect and its
        // body timeout without a byte, on every attempt. The waiter waits
        // that out, and the retry waits between the attempts, before it
        // gives up on the holder, on the clock the test moves.
        let defaults = ModelStore::new("/models");
        let decoder = 47_234_123;
        assert_eq!(
            defaults.lock_wait_limit(decoder),
            Duration::from_secs(3 * (120 + 720)) + RETRY_DELAY * 3
        );
        assert_eq!(defaults.lock_wait_limit(643_854), STALE_PARTIAL);
        assert_eq!(
            defaults.lock_wait_limit(2_435_420_160),
            Duration::from_secs(3 * (120 + 128)) + RETRY_DELAY * 3
        );
        let body: Vec<u8> = (0..10_000u32).map(|i| (i % 227) as u8).collect();
        let dir = tempfile::tempdir().unwrap();
        let mut store = ModelStore::new(dir.path());
        store.min_body_timeout = Duration::from_secs(3600);
        let limit = store.lock_wait_limit(body.len() as u64);
        assert_eq!(limit.as_secs(), 3 * (120 + 3600));
        let asset = asset(
            Some("http://127.0.0.1:9/never".to_owned()),
            &body,
            &digest(&body),
        );
        let directory = store.directory(&asset);
        fs::create_dir_all(&directory).unwrap();
        let held = locked(&directory.join("model.onnx.lock"));
        fs::write(directory.join("model.onnx.partial"), &body[..1000]).unwrap();
        let waiter = drive(&store, &asset);
        waiter.asleep();
        waiter.move_on(STALE_PARTIAL);
        waiter.asleep();
        let short = STALE_PARTIAL + Duration::from_secs(1);
        waiter.move_on(limit.checked_sub(short).unwrap());
        waiter.asleep();
        waiter.move_on(Duration::from_secs(1));
        let error = waiter.done().unwrap_err();
        assert!(
            error
                .to_string()
                .contains(&format!("written nothing for {} s", limit.as_secs())),
            "{error}"
        );
        drop(held);
    }

    #[test]
    fn a_416_to_a_resumed_range_restarts_the_file_from_zero() {
        // The host refuses the range of a 20,000-byte partial; the whole
        // file is asked for and replaces it.
        let body: Vec<u8> = (0..50_000u32).map(|i| (i % 211) as u8).collect();
        let whole = body.clone();
        let (url, ranges) = serve_each(vec![
            Box::new(|stream, _| {
                send(
                    stream,
                    b"HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
            }),
            Box::new(move |stream, range| answer(stream, &whole, range.as_deref(), None)),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path());
        let asset = asset(Some(url), &body, &digest(&body));
        fs::create_dir_all(store.directory(&asset)).unwrap();
        fs::write(
            store.directory(&asset).join("model.onnx.partial"),
            &body[..20_000],
        )
        .unwrap();
        store.ensure(&asset, &mut |_| {}).unwrap();
        store.verify(&asset).unwrap();
        assert_eq!(
            *ranges.lock().unwrap(),
            [Some("bytes=20000-".to_owned()), None]
        );
    }

    #[test]
    fn a_host_that_ignores_the_range_of_a_chunk_is_asked_for_the_whole_file() {
        // Its `200` came under a chunk's body timeout, so it is dropped
        // and the whole file asked for under the whole file's.
        let body: Vec<u8> = (0..3 * 65_536u32).map(|i| (i % 241) as u8).collect();
        let (first, second) = (body.clone(), body.clone());
        let (url, ranges) = serve_each(vec![
            Box::new(move |stream, _| answer(stream, &first, None, None)),
            Box::new(move |stream, _| answer(stream, &second, None, None)),
        ]);
        let (store, asset, _dir) = chunked(url, &body);
        store.ensure(&asset, &mut |_| {}).unwrap();
        store.verify(&asset).unwrap();
        assert_eq!(
            *ranges.lock().unwrap(),
            [Some("bytes=0-65535".to_owned()), None]
        );
    }

    #[test]
    fn a_range_follows_a_redirect_to_another_host() {
        // As at Hugging Face, whose download URL answers `302` with its
        // CDN: the `Range` reaches the host the redirect names, and the
        // partial a run left is resumed from there.
        let body: Vec<u8> = (0..200_000u32).map(|i| (i % 223) as u8).collect();
        let shared = Arc::new(body.clone());
        let (target, served) = serve_each(vec![capping(&shared, usize::MAX)]);
        let target = target.replace("127.0.0.1", "localhost");
        let redirect: Handler = Box::new(move |stream, _| {
            send(
                stream,
                format!("HTTP/1.1 302 Found\r\nLocation: {target}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes(),
            );
        });
        let (url, asked) = serve_each(vec![redirect]);
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path());
        let asset = asset(Some(url), &body, &digest(&body));
        fs::create_dir_all(store.directory(&asset)).unwrap();
        fs::write(
            store.directory(&asset).join("model.onnx.partial"),
            &body[..70_000],
        )
        .unwrap();
        let mut first = None;
        store
            .ensure(&asset, &mut |p| {
                first.get_or_insert(p.received);
            })
            .unwrap();
        store.verify(&asset).unwrap();
        assert_eq!(first, Some(70_000));
        assert_eq!(*asked.lock().unwrap(), [Some("bytes=70000-".to_owned())]);
        assert_eq!(*served.lock().unwrap(), [Some("bytes=70000-".to_owned())]);
    }

    #[test]
    #[ignore = "fetches from huggingface.co"]
    fn a_partial_of_the_hosted_export_resumes_through_its_redirect_in_chunks() {
        // `tokens.txt` (94 KB) of the pinned export, fetched once, cut back
        // to a partial of 30,000 bytes and fetched again in 16 KiB chunks:
        // every range goes through Hugging Face's redirect, and the file
        // must still match the manifest.
        let mut asset = ModelAsset::parakeet_v3_fp32();
        asset.files.retain(|f| f.name == "tokens.txt");
        let dir = tempfile::tempdir().unwrap();
        let mut store = ModelStore::new(dir.path());
        let directory = store.ensure(&asset, &mut |_| {}).unwrap();
        let installed = directory.join("tokens.txt");
        let partial = directory.join("tokens.txt.partial");
        fs::rename(&installed, &partial).unwrap();
        File::options()
            .write(true)
            .open(&partial)
            .unwrap()
            .set_len(30_000)
            .unwrap();
        store.chunk = 16 << 10;
        let mut reports = Vec::new();
        store
            .ensure(&asset, &mut |p| reports.push(p.received))
            .unwrap();
        store.verify(&asset).unwrap();
        assert_eq!(reports.first(), Some(&30_000));
        assert_eq!(reports.last(), Some(&asset.files[0].size));
        assert!(!partial.exists());
    }

    #[test]
    fn a_partial_name_a_dead_process_with_this_pid_left_is_passed_over() {
        // Per-call partials are for file systems without locks, which a
        // test cannot conjure; the naming is checked on its own.
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("model.onnx");
        // The next call numbers, with room for other tests drawing some.
        let next = NEXT_CALL.load(Ordering::Relaxed);
        let own = std::process::id();
        for call in next..next + 64 {
            fs::write(
                dir.path().join(format!("model.onnx.partial.{own}.{call}")),
                b"old",
            )
            .unwrap();
        }
        let mut partial = Partial::per_call(&destination).unwrap();
        assert!(!partial.resumable);
        partial.append(b"new").unwrap();
        let path = partial.path.clone();
        assert!(path.starts_with(dir.path()));
        let name = path.file_name().unwrap().to_string_lossy();
        assert_eq!(partial_pid(&name, "model.onnx.partial."), Some(own));
        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert_eq!(
            fs::read(dir.path().join(format!("model.onnx.partial.{own}.{next}"))).unwrap(),
            b"old"
        );
        // Deleted with the call.
        drop(partial);
        assert!(!path.exists());
    }

    #[test]
    fn a_wrong_checksum_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path());
        let body = b"tampered".to_vec();
        let asset = asset(Some(serve_once(&body)), &body, &digest(b"original"));
        let error = store.ensure(&asset, &mut |_| {}).unwrap_err();
        assert!(matches!(error, SpeechError::Checksum { .. }), "{error}");
        assert_eq!(names(&store, &asset), ["model.onnx.lock"]);
        assert!(!store.is_installed(&asset));
    }

    #[test]
    fn a_wrong_size_on_the_wire_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path());
        let body = b"short".to_vec();
        let mut short = asset(Some(serve_once(&body)), &body, &digest(&body));
        short.files[0].size += 1;
        assert!(matches!(
            store.ensure(&short, &mut |_| {}),
            Err(SpeechError::Size { .. })
        ));
        // A shortfall keeps what came, for the next call to resume.
        assert_eq!(
            names(&store, &short),
            ["model.onnx.lock", "model.onnx.partial"]
        );
        store.remove(&short).unwrap();
        // A body longer than the manifest says is cut off at the first
        // byte over, before the checksum is even looked at.
        let long = vec![0u8; 1 << 20];
        let mut long_asset = asset(Some(serve_once(&long)), &long, &digest(&long));
        long_asset.files[0].size = 1000;
        let mut last = 0;
        let error = store
            .ensure(&long_asset, &mut |p| last = p.received)
            .unwrap_err();
        let destination = dir.path().join("test-asset").join("model.onnx");
        assert!(
            matches!(&error, SpeechError::Size { actual, path, .. } if *actual > 1000 && *path == destination),
            "{error}"
        );
        assert!(last <= 1000);
        assert_eq!(names(&store, &long_asset), ["model.onnx.lock"]);
    }

    #[test]
    fn files_without_a_url_report_not_hosted_and_verify_catches_tampering() {
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path());
        let body = b"local only".to_vec();
        let asset = asset(None, &body, &digest(&body));
        assert!(matches!(
            store.ensure(&asset, &mut |_| {}),
            Err(SpeechError::NotHosted { .. })
        ));
        assert!(matches!(
            store.verify(&asset),
            Err(SpeechError::NotInstalled { .. })
        ));
        let directory = store.directory(&asset);
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("model.onnx"), &body).unwrap();
        assert_eq!(store.ensure(&asset, &mut |_| {}).unwrap(), directory);
        store.verify(&asset).unwrap();
        // Lower-case, as the manifest writes it.
        assert_eq!(
            sha256_of(&directory.join("model.onnx")).unwrap(),
            digest(&body)
        );
        fs::write(directory.join("model.onnx"), b"tampered!!").unwrap();
        assert!(matches!(
            store.verify(&asset),
            Err(SpeechError::Checksum { .. })
        ));
    }

    #[test]
    fn a_partial_is_deleted_once_its_file_is_installed_another_way() {
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path());
        let body = b"local only".to_vec();
        let asset = asset(None, &body, &digest(&body));
        let directory = store.directory(&asset);
        fs::create_dir_all(&directory).unwrap();
        let partial = directory.join("model.onnx.partial");
        // Killed half way, then copied in by hand.
        fs::write(&partial, &body[..4]).unwrap();
        fs::write(directory.join("model.onnx"), &body).unwrap();
        // While another download holds the lock, it stays.
        let held = locked(&directory.join("model.onnx.lock"));
        store.ensure(&asset, &mut |_| {}).unwrap();
        assert!(partial.exists());
        drop(held);
        store.ensure(&asset, &mut |_| {}).unwrap();
        assert!(!partial.exists());
        store.verify(&asset).unwrap();
    }

    #[test]
    fn a_download_that_finds_its_file_installed_leaves_no_partial() {
        // Installed by another download between the check and the lock:
        // this one returns at once (the URL is never fetched) and must not
        // leave the partial it opened, whatever it held.
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path());
        let body = b"not really a model".to_vec();
        let asset = asset(None, &body, &digest(&body));
        let directory = store.directory(&asset);
        fs::create_dir_all(&directory).unwrap();
        let destination = directory.join("model.onnx");
        fs::write(&destination, &body).unwrap();
        // Bytes a killed run left, which only this call would delete.
        fs::write(directory.join("model.onnx.partial"), &body[..4]).unwrap();
        store
            .download_with_retries(
                "http://127.0.0.1:9/never",
                &asset.files[0],
                &destination,
                &mut |_| {},
            )
            .unwrap();
        assert_eq!(names(&store, &asset), ["model.onnx", "model.onnx.lock"]);
    }

    #[test]
    fn only_another_process_s_partial_untouched_for_the_stale_age_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path());
        let body = b"local only".to_vec();
        let mut asset = asset(None, &body, &digest(&body));
        asset.files.push(ModelFile {
            name: "notes.txt".to_owned(),
            source: None,
            sha256: digest(b"notes"),
            size: 5,
        });
        let directory = store.directory(&asset);
        fs::create_dir_all(&directory).unwrap();
        let minute = Duration::from_secs(60);
        let now = std::time::SystemTime::now();
        let own = std::process::id();
        let other = if own == 1 { 2 } else { 1 };
        // (name, contents, last write)
        let files = [
            ("model.onnx", &body[..], now - 2 * STALE_PARTIAL),
            ("notes.txt", b"notes", now - 2 * STALE_PARTIAL),
            (
                &*format!("model.onnx.partial.{other}.0"),
                b"half",
                now - 11 * minute,
            ),
            (
                &*format!("model.onnx.partial.{other}.1"),
                b"half",
                now - 9 * minute,
            ),
            (
                &*format!("model.onnx.partial.{other}.2"),
                b"half",
                now + 60 * minute,
            ),
            (
                &*format!("model.onnx.partial.{own}.0"),
                b"half",
                now - 2 * STALE_PARTIAL,
            ),
            (
                &*format!("model.onnx.partial.{other}"),
                b"half",
                now - 2 * STALE_PARTIAL,
            ),
            ("model.onnx.partial.x.0", b"half", now - 2 * STALE_PARTIAL),
            ("other.onnx.partial.1.0", b"half", now - 2 * STALE_PARTIAL),
        ];
        for (name, contents, modified) in files {
            let path = directory.join(name);
            fs::write(&path, contents).unwrap();
            File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(modified)
                .unwrap();
        }
        store.ensure(&asset, &mut |_| {}).unwrap();
        let left = names(&store, &asset);
        let mut expected: Vec<_> = files[..]
            .iter()
            .map(|(name, _, _)| (*name).to_owned())
            .filter(|name| *name != format!("model.onnx.partial.{other}.0"))
            .collect();
        expected.sort();
        assert_eq!(left, expected);
    }

    /// The largest file a GitHub release takes.
    const GITHUB_RELEASE_ASSET_LIMIT: u64 = 2 * 1024 * 1024 * 1024;

    #[test]
    fn hosts_fit_their_limits_and_hugging_face_urls_pin_a_commit() {
        // GitHub release assets cap at 2 GB a file, so a file that large
        // can only come from Hugging Face.
        let hosted = ModelAsset::parakeet_v3_fp32();
        for asset in ModelAsset::onnx() {
            for file in &asset.files {
                match &file.source {
                    Some(ModelSource::Url(url)) => {
                        assert!(url.starts_with("https://"), "{url}");
                        assert!(file.size < GITHUB_RELEASE_ASSET_LIMIT, "{}", file.name);
                    }
                    // A full commit, not a branch or a short hash.
                    Some(ModelSource::HuggingFace { revision, .. }) => assert!(
                        revision.len() == 40
                            && revision
                                .bytes()
                                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                        "{revision}"
                    ),
                    None => panic!("{} has no host; every file Steno ships has one", file.name),
                }
            }
        }
        let weights = hosted
            .files
            .iter()
            .find(|f| f.name == "encoder.weights")
            .unwrap();
        assert!(weights.size > GITHUB_RELEASE_ASSET_LIMIT);
        assert_eq!(
            weights.source.as_ref().unwrap().url(),
            "https://huggingface.co/nicolaischmid/steno-models/resolve/4a133253481bfd2cb38dc3e77c3f748199562488/parakeet-tdt-0.6b-v3-fp32/encoder.weights"
        );
        // Same files, sizes and checksums whether hosted or not.
        let unhosted = ModelAsset::parakeet_v3_fp32_manifest(|_, _| None);
        let strip = |asset: &ModelAsset| {
            asset
                .files
                .iter()
                .map(|f| (f.name.clone(), f.sha256.clone(), f.size))
                .collect::<Vec<_>>()
        };
        assert_eq!(strip(&hosted), strip(&unhosted));
        // A mirror replaces every host, unhosted files included.
        let store = ModelStore::new("/models");
        let weights = &unhosted.files[1];
        assert_eq!(weights.name, "encoder.weights");
        assert_eq!(store.url_for(&unhosted, weights), None);
        let silero = ModelAsset::silero_vad();
        assert_eq!(
            store.url_for(&silero, &silero.files[0]).unwrap(),
            "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/silero_vad.onnx"
        );
        let mirrored = store.with_mirror(Some(" http://mirror.local/models/ ".to_owned()));
        assert_eq!(mirrored.mirror(), Some("http://mirror.local/models"));
        assert_eq!(
            mirrored.url_for(&unhosted, weights).unwrap(),
            "http://mirror.local/models/parakeet-tdt-0.6b-v3-fp32/encoder.weights"
        );
        assert_eq!(mirrored.with_mirror(Some(String::new())).mirror(), None);
    }

    #[test]
    fn names_that_could_leave_the_root_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path());
        let body = b"x".to_vec();
        for (id, name) in [
            ("../escape", "model.onnx"),
            ("test-asset", "../model.onnx"),
            ("test-asset", "/abs/model.onnx"),
            ("test-asset", ""),
            ("", "model.onnx"),
            ("a/b", "model.onnx"),
            (".", "model.onnx"),
            ("test-asset", "bundle/../model.onnx"),
            ("test-asset", "bundle//model.onnx"),
            ("test-asset", "bundle/"),
            ("test-asset", "./model.onnx"),
        ] {
            let mut bad = asset(None, &body, &digest(&body));
            bad.id = id.to_owned();
            bad.files[0].name = name.to_owned();
            let error = bad.validate().unwrap_err();
            assert!(
                matches!(error, SpeechError::InvalidName { .. }),
                "{id} {name}: {error}"
            );
            assert!(matches!(
                store.ensure(&bad, &mut |_| {}),
                Err(SpeechError::InvalidName { .. })
            ));
            assert!(matches!(
                store.verify(&bad),
                Err(SpeechError::InvalidName { .. })
            ));
            assert!(matches!(
                store.remove(&bad),
                Err(SpeechError::InvalidName { .. })
            ));
            assert!(!store.is_installed(&bad));
        }
        for asset in ModelAsset::onnx() {
            asset.validate().unwrap();
        }
        ModelAsset::parakeet_v3_coreml().validate().unwrap();
        assert!(fs::read_dir(dir.path()).unwrap().next().is_none());
        // An invalid asset lists every file as missing, even one a join of
        // its id would find: "." resolves to the root itself.
        let mut dot = asset(None, &body, &digest(&body));
        dot.id = ".".to_owned();
        fs::write(dir.path().join("model.onnx"), &body).unwrap();
        assert_eq!(store.missing_files(&dot), ["model.onnx"]);
    }

    #[test]
    fn the_manifest_is_consistent_and_the_root_follows_the_support_directory() {
        for asset in ModelAsset::onnx() {
            assert!(!asset.files.is_empty());
            for file in &asset.files {
                assert_eq!(file.sha256.len(), 64, "{}", file.name);
                assert!(file.sha256.chars().all(|c| c.is_ascii_hexdigit()));
                assert!(file.size > 0);
            }
        }
        assert!(ModelAsset::parakeet_v3_fp32().total_size() > 2_500_000_000);
        assert!(ModelAsset::silero_vad().files[0].source.is_some());
        assert_eq!(
            ModelStore::default_models_directory(),
            StenoPaths::default_support_directory().join("Models")
        );
        assert_eq!(
            ModelStore::in_models_directory(Path::new("/models")).root(),
            Path::new("/models/onnx")
        );
        let store = ModelStore::new("/models");
        assert_eq!(
            store.directory(&ModelAsset::silero_vad()),
            PathBuf::from("/models/silero-vad")
        );
    }

    /// The `CoreML` Parakeet's manifest is the tree the Swift app installed
    /// on 2026-09-25 (the fixture, `<sha256>  <size>  <path>` per file, as
    /// listed on the Mac it was installed on), every file fetched from the
    /// `FluidAudio` repository at the pinned commit, into the folder the
    /// Swift app and the `CoreML` backend read.
    #[test]
    fn the_coreml_parakeet_is_the_tree_the_swift_app_installed_at_the_pinned_commit() {
        let fixture = include_str!("../tests/fixtures/parakeet-v3-coreml.sha256");
        let mut expected: Vec<(String, u64, String)> = fixture
            .lines()
            .map(|line| {
                let mut fields = line.split("  ");
                let (sha256, size, path) = (
                    fields.next().unwrap(),
                    fields.next().unwrap(),
                    fields.next().unwrap(),
                );
                (path.to_owned(), size.parse().unwrap(), sha256.to_owned())
            })
            .collect();
        expected.sort();
        assert_eq!(expected.len(), 23);
        let asset = ModelAsset::parakeet_v3_coreml();
        let mut manifest: Vec<_> = asset
            .files
            .iter()
            .map(|f| (f.name.clone(), f.size, f.sha256.clone()))
            .collect();
        manifest.sort();
        assert_eq!(manifest, expected);
        assert_eq!(asset.total_size(), 483_257_242);
        assert_eq!(PARAKEET_V3_COREML_REVISION.len(), 40);
        assert!(PARAKEET_V3_COREML_REVISION.starts_with("7dd20fe6b1"));
        for file in &asset.files {
            assert_eq!(
                file.source,
                Some(ModelSource::HuggingFace {
                    repo: PARAKEET_V3_COREML_REPO.to_owned(),
                    revision: PARAKEET_V3_COREML_REVISION.to_owned(),
                    path: file.name.clone(),
                }),
                "{}",
                file.name
            );
        }
        assert_eq!(
            ModelStore::new("/m")
                .url_for(&asset, &asset.files[9])
                .unwrap(),
            "https://huggingface.co/FluidInference/parakeet-tdt-0.6b-v3-coreml/resolve/7dd20fe6b1797d35f5e3307e8b1732d9a178edfe/Encoder.mlmodelc/weights/weight.bin"
        );
        assert_eq!(
            ModelStore::coreml_in_models_directory(Path::new("/models")).directory(&asset),
            Path::new("/models/fluidaudio/parakeet-tdt-0.6b-v3")
        );
        assert!(
            ModelAsset::onnx().iter().all(|onnx| onnx.id != asset.id),
            "the ONNX assets stay the sidecar's"
        );
    }
}
