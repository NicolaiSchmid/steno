//! The speech model catalogue the Transcription section shows: which
//! engines the user may pick, which model each loads, and what every model
//! bundle is called, weighs and is licensed under. Copied from
//! `Sources/StenoSpeech/{Models/ModelAsset,Engines/SpeechEngineID}.swift`
//! until `steno-speech` (WP4) owns the table; the host then imports it.

use steno_core::string_enum;

string_enum! {
    /// One downloadable model bundle. Swift: `ModelAsset`.
    pub enum ModelAsset {
        ParakeetV3 = "parakeetV3",
        ParakeetUltra = "parakeetUltra",
        ParakeetDe = "parakeetDE",
        WhisperLargeV3Turbo = "whisperLargeV3Turbo",
        OfflineDiarizer = "offlineDiarizer",
    }
}

impl ModelAsset {
    /// The assets the Rust app offers: Parakeet v3 and the diarizer. The
    /// Swift app's Ultra, German and Whisper models are not part of this
    /// version (a stored one becomes Parakeet v3 at launch,
    /// `steno_core::Store::retire_speech_engine`), so Settings shows no row
    /// for them. Rust only.
    pub const OFFERED: [ModelAsset; 2] = [ModelAsset::ParakeetV3, ModelAsset::OfflineDiarizer];

    /// The Hugging Face repository the files come from.
    #[must_use]
    pub fn source_repo(self) -> &'static str {
        match self {
            ModelAsset::ParakeetV3 => "FluidInference/parakeet-tdt-0.6b-v3-coreml",
            ModelAsset::ParakeetUltra => "FluidInference/parakeet-ultra-coreml",
            ModelAsset::ParakeetDe => "ValentinWeyer/parakeet-primeline-de-coreml",
            ModelAsset::WhisperLargeV3Turbo => "argmaxinc/whisperkit-coreml",
            ModelAsset::OfflineDiarizer => "FluidInference/speaker-diarization-coreml",
        }
    }

    /// Size on disk after the download, measured from the repository trees
    /// on 2026-09-25. Shown before a download, not asserted.
    #[must_use]
    pub fn approximate_bytes(self) -> i64 {
        match self {
            ModelAsset::ParakeetV3 => 485_000_000,
            ModelAsset::ParakeetUltra => 632_000_000,
            ModelAsset::ParakeetDe => 1_220_000_000,
            ModelAsset::WhisperLargeV3Turbo => 1_640_000_000,
            ModelAsset::OfflineDiarizer => 22_000_000,
        }
    }

    /// The licence of the model behind the asset. The diarizer's is that
    /// of the two ONNX models every Rust platform runs, pyannote
    /// segmentation 3.0 (MIT) and `WeSpeaker` ResNet34-LM (CC-BY-4.0, from
    /// its `VoxCeleb` training data): `steno_diarize::models::LICENCE`,
    /// which a `steno-services` test pins it to. Swift's line named the
    /// Apache-2.0 of its `CoreML` diarizer's upstream. The diarizer's
    /// [`display_name`](Self::display_name) and
    /// [`source_repo`](Self::source_repo) stay Swift's copy; the services
    /// override all three with the ONNX models'.
    #[must_use]
    pub fn licence(self) -> &'static str {
        match self {
            ModelAsset::ParakeetV3 | ModelAsset::ParakeetUltra | ModelAsset::ParakeetDe => {
                "CC-BY-4.0"
            }
            ModelAsset::WhisperLargeV3Turbo => "MIT (WhisperKit), OpenAI weights",
            ModelAsset::OfflineDiarizer => "MIT AND CC-BY-4.0",
        }
    }

    #[must_use]
    pub fn display_name(self) -> &'static str {
        match self {
            ModelAsset::ParakeetV3 => "Parakeet TDT 0.6B v3 (int8)",
            ModelAsset::ParakeetUltra => "Parakeet Ultra (int8)",
            ModelAsset::ParakeetDe => "Parakeet German fine-tune (fp16)",
            ModelAsset::WhisperLargeV3Turbo => "Whisper large-v3 turbo",
            ModelAsset::OfflineDiarizer => "Speaker diarization (pyannote community-1)",
        }
    }
}

string_enum! {
    /// The engines the speech module can build; the raw value is
    /// `Settings::speech_engine_id`. Swift: `SpeechEngineID`.
    pub enum SpeechEngineId {
        ParakeetV3 = "parakeet-v3",
        ParakeetUltra = "parakeet-ultra",
        ParakeetDe = "parakeet-de",
        WhisperKitLargeV3Turbo = "whisperkit-large-v3-turbo",
    }
}

impl SpeechEngineId {
    /// The engines the settings pane offers: Parakeet v3 alone, so the
    /// picker shows no row. The Swift app also offered Whisper; a stored
    /// Whisper, Ultra or German id becomes Parakeet v3 at launch
    /// (`Store::retire_speech_engine`). Rust only.
    pub const USER_SELECTABLE: [SpeechEngineId; 1] = [SpeechEngineId::ParakeetV3];

    /// The model the engine loads; the diarizer is separate.
    #[must_use]
    pub fn asset(self) -> ModelAsset {
        match self {
            SpeechEngineId::ParakeetV3 => ModelAsset::ParakeetV3,
            SpeechEngineId::ParakeetUltra => ModelAsset::ParakeetUltra,
            SpeechEngineId::ParakeetDe => ModelAsset::ParakeetDe,
            SpeechEngineId::WhisperKitLargeV3Turbo => ModelAsset::WhisperLargeV3Turbo,
        }
    }

    /// How many languages the engine supports: Parakeet v3's 25 European
    /// languages, the German fine-tune's one, Whisper's 99.
    #[must_use]
    pub fn supported_language_count(self) -> usize {
        match self {
            SpeechEngineId::ParakeetV3 | SpeechEngineId::ParakeetUltra => 25,
            SpeechEngineId::ParakeetDe => 1,
            SpeechEngineId::WhisperKitLargeV3Turbo => 99,
        }
    }
}
