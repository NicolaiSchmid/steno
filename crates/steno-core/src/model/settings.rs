use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{AudioRetention, Meeting};
use crate::json;
use crate::paths::{StenoPaths, file_url};

string_enum! {
    /// Which service writes the summaries.
    pub enum LlmProvider {
        /// Any OpenAI-compatible chat completions server, base URL plus API key.
        Endpoint = "endpoint",
        /// `OpenAI`'s Codex backend with the `ChatGPT` sign-in the Codex CLI stored.
        Codex = "codex",
    }
}

/// The one settings type, persisted as one row per property in the
/// `setting` table (`Store::settings`). API keys never live here. URLs are
/// the `file://` strings Swift's `URL` encodes to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    /// Where recordings live.
    pub audio_folder: String,
    pub default_retention: AudioRetention,
    #[serde(
        rename = "inputDeviceUID",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub input_device_uid: Option<String>,
    pub meeting_detection_enabled: bool,
    #[serde(rename = "speechEngineID")]
    pub speech_engine_id: String,
    /// Cosine similarity a speaker match needs.
    pub speaker_match_threshold: f32,
    /// `None` means the speech module's default location.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models_directory: Option<String>,
    pub llm_provider: LlmProvider,
    #[serde(
        rename = "llmBaseURL",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub llm_base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_model: Option<String>,
    pub llm_context_tokens: i64,
    /// The Codex model slug; `None` until one is picked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_model: Option<String>,
    pub codex_context_tokens: i64,
    /// When the user confirmed that Steno may use the Codex sign-in stored
    /// on this computer. `None` means never.
    #[serde(
        default,
        with = "json::iso_time_opt",
        skip_serializing_if = "Option::is_none"
    )]
    pub codex_confirmed_at: Option<DateTime<Utc>>,
    #[serde(rename = "defaultTemplateID")]
    pub default_template_id: String,
    pub launch_at_login: bool,
    /// `None` means the Obsidian destination is not configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub obsidian: Option<ObsidianSettings>,
}

impl Settings {
    /// Until the model list has told us better.
    pub const DEFAULT_CODEX_CONTEXT_TOKENS: i64 = 128_000;

    /// The defaults for a new install, with `audio_folder` under `paths`'
    /// support directory (`<support>/Audio/`).
    #[must_use]
    pub fn defaults(paths: &StenoPaths) -> Self {
        Settings {
            audio_folder: file_url(&paths.support_directory.join("Audio"), true),
            default_retention: AudioRetention::KeepForever,
            input_device_uid: None,
            meeting_detection_enabled: true,
            speech_engine_id: "parakeet-v3".to_owned(),
            speaker_match_threshold: 0.60,
            models_directory: None,
            llm_provider: LlmProvider::Endpoint,
            llm_base_url: None,
            llm_model: None,
            llm_context_tokens: 32_000,
            codex_model: None,
            codex_context_tokens: Self::DEFAULT_CODEX_CONTEXT_TOKENS,
            codex_confirmed_at: None,
            default_template_id: Meeting::DEFAULT_TEMPLATE_ID.to_owned(),
            launch_at_login: true,
            obsidian: None,
        }
    }
}

impl Default for Settings {
    /// [`Settings::defaults`] for the default support directory.
    fn default() -> Self {
        Settings::defaults(&StenoPaths::new(StenoPaths::default_support_directory()))
    }
}

/// The Obsidian folder destination's typed settings.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObsidianSettings {
    /// Absolute path of the vault.
    pub vault_path: String,
    /// Folder inside the vault for per-person pages; `None` disables them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub people_folder: Option<String>,
    pub include_audio: bool,
    /// Tag appended to every task line; `None` for none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_tag: Option<String>,
}
