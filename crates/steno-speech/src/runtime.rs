//! Which engine runs Parakeet where, and the speech settings that choose
//! it. On Linux and Windows the ONNX sidecar
//! ([`SidecarSpeechEngine`](crate::SidecarSpeechEngine)) is the only
//! engine the app runs: inference never shares the app's process there.
//! On macOS the in-process `CoreML` engine (`steno-speech-coreml`) is the
//! default, so the Mac stays one process; the ONNX sidecar is a fallback
//! behind [`SpeechSettings::onnx_sidecar_on_mac`], for a Mac where the
//! Neural Engine path fails. The in-process [`OnnxSpeechEngine`](crate::OnnxSpeechEngine)
//! is what the sidecar hosts and what the example and the FLEURS test
//! drive; the app never runs it on its own thread.
//! Swift: none; the Mac app runs `FluidAudio` in-process only.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::model_store::ModelStore;

/// Where Parakeet runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeechRuntime {
    /// `CoreML` inside the app (`steno-speech-coreml`); macOS only.
    CoreMlInProcess,
    /// ONNX Runtime in `steno-speech-sidecar`.
    OnnxSidecar,
}

/// The speech settings, which `steno-services` reads from `speech.json`
/// in the support directory (not the database's `setting` table, which
/// the Swift app rewrites whole). Absent keys take their defaults, so an
/// empty object is the default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SpeechSettings {
    /// macOS only: run the ONNX sidecar instead of `CoreML`. Ignored
    /// elsewhere, where the sidecar is the only choice.
    pub onnx_sidecar_on_mac: bool,
    /// A mirror every model is fetched from instead of its host
    /// ([`ModelStore::with_mirror`]); `None` uses the hosts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models_mirror: Option<String>,
}

impl SpeechSettings {
    /// The runtime on this platform.
    #[must_use]
    pub fn runtime(&self) -> SpeechRuntime {
        self.runtime_on(cfg!(target_os = "macos"))
    }

    /// The runtime on a Mac (`macos`) or elsewhere; [`Self::runtime`]
    /// for any platform, so the tests can check both.
    #[must_use]
    fn runtime_on(&self, macos: bool) -> SpeechRuntime {
        if macos && !self.onnx_sidecar_on_mac {
            SpeechRuntime::CoreMlInProcess
        } else {
            SpeechRuntime::OnnxSidecar
        }
    }

    /// The ONNX store of the models directory `models_directory` (its
    /// `onnx/` folder, [`ModelStore::in_models_directory`]) with these
    /// settings' mirror.
    #[must_use]
    pub fn model_store(&self, models_directory: &Path) -> ModelStore {
        ModelStore::in_models_directory(models_directory).with_mirror(self.models_mirror.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sidecar_is_the_default_off_the_mac_and_the_fallback_on_it() {
        let default = SpeechSettings::default();
        assert_eq!(default.runtime_on(true), SpeechRuntime::CoreMlInProcess);
        assert_eq!(default.runtime_on(false), SpeechRuntime::OnnxSidecar);
        let fallback = SpeechSettings {
            onnx_sidecar_on_mac: true,
            ..SpeechSettings::default()
        };
        assert_eq!(fallback.runtime_on(true), SpeechRuntime::OnnxSidecar);
        assert_eq!(fallback.runtime_on(false), SpeechRuntime::OnnxSidecar);
        let expected = if cfg!(target_os = "macos") {
            SpeechRuntime::CoreMlInProcess
        } else {
            SpeechRuntime::OnnxSidecar
        };
        assert_eq!(default.runtime(), expected);
    }

    #[test]
    fn settings_round_trip_in_camel_case_and_default_when_absent() {
        let settings = SpeechSettings {
            onnx_sidecar_on_mac: true,
            models_mirror: Some("http://mirror.example:8000/models".to_owned()),
        };
        let json = steno_core::json::to_column_string(&settings).unwrap();
        assert_eq!(
            json,
            r#"{"modelsMirror":"http://mirror.example:8000/models","onnxSidecarOnMac":true}"#
        );
        assert_eq!(
            serde_json::from_str::<SpeechSettings>(&json).unwrap(),
            settings
        );
        assert_eq!(
            serde_json::from_str::<SpeechSettings>("{}").unwrap(),
            SpeechSettings::default()
        );
        let store = settings.model_store(Path::new("/models"));
        assert_eq!(store.root(), Path::new("/models").join("onnx"));
        assert_eq!(store.mirror(), Some("http://mirror.example:8000/models"));
        assert_eq!(
            SpeechSettings::default()
                .model_store(Path::new("/m"))
                .mirror(),
            None
        );
    }
}
