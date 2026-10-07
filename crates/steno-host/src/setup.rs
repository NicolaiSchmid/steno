//! What the main window says about the setup state, after
//! `apps/macos/Steno/Main/SetupStatus.swift` and `Services/Settings+Setup.swift`:
//! whether the endpoint and the vault are configured, why the Summary and
//! Tasks tabs have nothing to show, what the export footer shows, what the
//! setup banner says, and the copy all of them share.

use steno_bridge::{DetailTab, Platform};
use steno_core::{Delivery, DeliveryStatus, LlmProvider, Meeting, MeetingState, Settings};

/// The chosen provider is set up (endpoint: URL and model stored; Codex:
/// confirmed and a model picked); the pipeline runs the LLM passes.
/// Swift: `Settings.llmConfigured`, through `LLMEndpoint(settings:)`.
#[must_use]
pub fn llm_configured(settings: &Settings) -> bool {
    match settings.llm_provider {
        LlmProvider::Endpoint => {
            settings.llm_base_url.is_some()
                && settings
                    .llm_model
                    .as_deref()
                    .is_some_and(|model| !model.is_empty())
        }
        LlmProvider::Codex => {
            settings.codex_confirmed_at.is_some()
                && settings
                    .codex_model
                    .as_deref()
                    .is_some_and(|model| !model.is_empty())
        }
    }
}

/// An Obsidian vault is stored; the deliver stage has a destination.
/// Swift: `Settings.vaultConfigured`.
#[must_use]
pub fn vault_configured(settings: &Settings) -> bool {
    settings.obsidian.is_some()
}

/// Why the Summary and Tasks tabs have nothing to show. A `ready` meeting
/// whose `summary` is none was processed without an LLM endpoint; whether
/// one exists now comes from `Settings`, so the row can offer the fix or
/// the re-run. Swift: `SummaryStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummaryStatus {
    /// Recording, queued, processing or failed without a summary.
    Pending,
    /// The summary exists; the tabs render it.
    Present,
    /// Ready without a summary and no endpoint configured now.
    SkippedUnconfigured,
    /// Ready without a summary; an endpoint exists now, so "Run summary" works.
    SkippedRunnable,
}

impl SummaryStatus {
    #[must_use]
    pub fn of(meeting: Option<&Meeting>, llm_configured: bool) -> Self {
        let Some(meeting) = meeting else {
            return SummaryStatus::Pending;
        };
        if meeting.summary.is_some() {
            SummaryStatus::Present
        } else if meeting.state == MeetingState::Ready {
            if llm_configured {
                SummaryStatus::SkippedRunnable
            } else {
                SummaryStatus::SkippedUnconfigured
            }
        } else {
            SummaryStatus::Pending
        }
    }

    /// The empty-tab row for a skipped summary; `None` for `Pending` and
    /// `Present`, which keep the pending text.
    #[must_use]
    pub fn skipped_row(self, tab: DetailTab) -> Option<SkippedRow> {
        match (self, tab) {
            (SummaryStatus::SkippedUnconfigured, DetailTab::Summary) => Some(SkippedRow {
                title: copy::SUMMARY_SKIPPED_TITLE,
                body: copy::SUMMARY_SKIPPED_BODY,
                action: SkippedAction::SetUpSummaries,
                footnote: None,
            }),
            (SummaryStatus::SkippedRunnable, DetailTab::Summary) => Some(SkippedRow {
                title: copy::SUMMARY_RUNNABLE_TITLE,
                body: copy::SUMMARY_RUNNABLE_BODY,
                action: SkippedAction::RunSummary,
                footnote: Some(copy::SUMMARY_RUNNABLE_FOOTNOTE),
            }),
            (SummaryStatus::SkippedUnconfigured, DetailTab::Tasks) => Some(SkippedRow {
                title: copy::TASKS_SKIPPED_TITLE,
                body: copy::TASKS_SKIPPED_BODY,
                action: SkippedAction::SetUpSummaries,
                footnote: None,
            }),
            (SummaryStatus::SkippedRunnable, DetailTab::Tasks) => Some(SkippedRow {
                title: copy::TASKS_SKIPPED_TITLE,
                body: copy::TASKS_SKIPPED_BODY,
                action: SkippedAction::RunSummary,
                footnote: None,
            }),
            _ => None,
        }
    }
}

/// The one action of a skipped row. Swift: `SummaryStatus.SkippedRow.Action`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkippedAction {
    /// Opens Settings > Summaries.
    SetUpSummaries,
    /// Re-runs the summary.
    RunSummary,
}

impl SkippedAction {
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            SkippedAction::SetUpSummaries => copy::SET_UP_SUMMARIES,
            SkippedAction::RunSummary => copy::RUN_SUMMARY,
        }
    }
}

/// What a tab shows for a skipped summary. Swift: `SummaryStatus.SkippedRow`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkippedRow {
    pub title: &'static str,
    pub body: &'static str,
    pub action: SkippedAction,
    pub footnote: Option<&'static str>,
}

/// What the detail footer shows. One vocabulary: "export", never
/// "delivered". Swift: `ExportStatus`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportStatus {
    /// No delivery rows and no vault.
    NoVault,
    /// No delivery rows, a vault exists.
    NotExported,
    /// One badge per delivery row.
    Exported(Vec<Delivery>),
}

impl ExportStatus {
    #[must_use]
    pub fn of(deliveries: &[Delivery], vault_configured: bool) -> Self {
        if !deliveries.is_empty() {
            ExportStatus::Exported(deliveries.to_vec())
        } else if vault_configured {
            ExportStatus::NotExported
        } else {
            ExportStatus::NoVault
        }
    }
}

/// Every delivery is `delivered`. Swift: `[Delivery].allDelivered`.
#[must_use]
pub fn all_delivered(deliveries: &[Delivery]) -> bool {
    deliveries
        .iter()
        .all(|delivery| delivery.status == DeliveryStatus::Delivered)
}

/// What the main window's setup banner says; `None` when both the endpoint
/// and the vault are configured. Swift: `SetupBannerMessage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupBannerMessage {
    BothMissing,
    EndpointMissing,
    VaultMissing,
}

impl SetupBannerMessage {
    #[must_use]
    pub fn of(settings: &Settings) -> Option<Self> {
        match (llm_configured(settings), vault_configured(settings)) {
            (true, true) => None,
            (false, false) => Some(SetupBannerMessage::BothMissing),
            (false, true) => Some(SetupBannerMessage::EndpointMissing),
            (true, false) => Some(SetupBannerMessage::VaultMissing),
        }
    }

    /// The banner's first sentence.
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            SetupBannerMessage::BothMissing => copy::BANNER_BOTH_MISSING_TITLE,
            SetupBannerMessage::EndpointMissing => copy::BANNER_ENDPOINT_MISSING_TITLE,
            SetupBannerMessage::VaultMissing => copy::BANNER_VAULT_MISSING_TITLE,
        }
    }

    /// The rest of the banner, in `platform`'s word for the machine.
    #[must_use]
    pub fn body(self, platform: Platform) -> &'static str {
        match self {
            SetupBannerMessage::BothMissing => platform.mac_or(
                copy::BANNER_BOTH_MISSING_BODY,
                copy::BANNER_BOTH_MISSING_BODY_ELSEWHERE,
            ),
            SetupBannerMessage::EndpointMissing => copy::BANNER_ENDPOINT_MISSING_BODY,
            SetupBannerMessage::VaultMissing => platform.mac_or(
                copy::BANNER_VAULT_MISSING_BODY,
                copy::BANNER_VAULT_MISSING_BODY_ELSEWHERE,
            ),
        }
    }

    /// The "Set up summaries" button.
    #[must_use]
    pub fn offers_summaries(self) -> bool {
        self != SetupBannerMessage::VaultMissing
    }

    /// The "Choose a vault" button.
    #[must_use]
    pub fn offers_vault(self) -> bool {
        self != SetupBannerMessage::EndpointMissing
    }
}

/// The onboarding plan's user-facing strings for the main window, in one
/// place so the banner, the tabs and the footer cannot drift. Swift:
/// `SetupCopy`.
pub mod copy {
    pub const BANNER_BOTH_MISSING_TITLE: &str = "Summaries and export are off.";
    pub const BANNER_BOTH_MISSING_BODY: &str = "Steno has no LLM endpoint and no Obsidian vault yet, so meetings keep a raw transcript on this Mac.";
    /// Windows and Linux: "this computer" for "this Mac".
    pub const BANNER_BOTH_MISSING_BODY_ELSEWHERE: &str = "Steno has no LLM endpoint and no Obsidian vault yet, so meetings keep a raw transcript on this computer.";
    pub const BANNER_ENDPOINT_MISSING_TITLE: &str = "Summaries are off.";
    pub const BANNER_ENDPOINT_MISSING_BODY: &str =
        "Steno has no LLM endpoint yet, so meetings keep a raw transcript.";
    pub const BANNER_VAULT_MISSING_TITLE: &str = "Export is off.";
    pub const BANNER_VAULT_MISSING_BODY: &str =
        "Steno has no Obsidian vault yet, so meetings stay on this Mac.";
    /// Windows and Linux: "this computer" for "this Mac".
    pub const BANNER_VAULT_MISSING_BODY_ELSEWHERE: &str =
        "Steno has no Obsidian vault yet, so meetings stay on this computer.";
    pub const SUMMARY_SKIPPED_TITLE: &str = "Summary skipped";
    pub const SUMMARY_SKIPPED_BODY: &str =
        "No LLM endpoint is configured. The transcript is complete.";
    pub const SUMMARY_RUNNABLE_TITLE: &str = "No summary yet";
    pub const SUMMARY_RUNNABLE_BODY: &str =
        "This meeting was processed before an LLM endpoint was configured.";
    pub const SUMMARY_RUNNABLE_FOOTNOTE: &str = "Summary only; the transcript stays as recorded.";
    pub const TASKS_SKIPPED_TITLE: &str = "No tasks";
    pub const TASKS_SKIPPED_BODY: &str = "The summary was skipped.";
    pub const SET_UP_SUMMARIES: &str = "Set up summaries";
    pub const RUN_SUMMARY: &str = "Run summary";
    pub const NOT_EXPORTED_NO_VAULT: &str = "Not exported: no Obsidian vault is configured.";
    pub const NOT_EXPORTED_YET: &str = "Not exported yet.";
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(url: Option<&str>, model: Option<&str>, vault: bool) -> Settings {
        Settings {
            llm_base_url: url.map(str::to_owned),
            llm_model: model.map(str::to_owned),
            obsidian: vault.then(|| steno_core::ObsidianSettings {
                vault_path: "/vault".to_owned(),
                people_folder: None,
                include_audio: false,
                task_tag: None,
                extra: serde_json::Map::new(),
            }),
            ..Settings::default()
        }
    }

    /// Swift: `SetupStatusTests.testConfiguredFlagsCoverTheFourCombinations`
    /// and `testBannerMessageFollowsWhatIsMissing`.
    #[test]
    fn the_banner_follows_what_is_missing() {
        let none = settings(None, None, false);
        assert!(!llm_configured(&none));
        assert_eq!(
            SetupBannerMessage::of(&none),
            Some(SetupBannerMessage::BothMissing)
        );
        let half = settings(Some("http://127.0.0.1:1234/v1"), None, false);
        assert!(!llm_configured(&half), "a URL without a model is off");
        let endpoint = settings(Some("http://127.0.0.1:1234/v1"), Some("m"), false);
        assert!(llm_configured(&endpoint));
        assert_eq!(
            SetupBannerMessage::of(&endpoint),
            Some(SetupBannerMessage::VaultMissing)
        );
        let vault = settings(None, None, true);
        assert_eq!(
            SetupBannerMessage::of(&vault),
            Some(SetupBannerMessage::EndpointMissing)
        );
        assert!(!SetupBannerMessage::EndpointMissing.offers_vault());
        assert!(!SetupBannerMessage::VaultMissing.offers_summaries());
        assert_eq!(
            SetupBannerMessage::of(&settings(Some("http://x/v1"), Some("m"), true)),
            None
        );
        let mut codex = settings(None, None, false);
        codex.llm_provider = LlmProvider::Codex;
        codex.codex_model = Some("gpt-5.1-codex".to_owned());
        assert!(!llm_configured(&codex), "unconfirmed");
        codex.codex_confirmed_at = Some(chrono::Utc::now());
        assert!(llm_configured(&codex));
    }

    /// Swift: `SetupStatusTests.testSummaryStatusKeysOffTheSummaryTheStateAndTheEndpoint`
    /// and `testSkippedRowsCarryTheActions`.
    #[test]
    fn summary_status_keys_off_the_summary_the_state_and_the_endpoint() {
        assert_eq!(SummaryStatus::of(None, true), SummaryStatus::Pending);
        let mut meeting = steno_core::Meeting {
            id: uuid::Uuid::nil(),
            title: String::new(),
            started_at: chrono::Utc::now(),
            duration: 0.0,
            language: None,
            source: steno_core::MeetingSource::MacCall,
            calendar_event_id: None,
            tags: Vec::new(),
            state: MeetingState::Ready,
            end_reason: None,
            title_origin: steno_core::TitleOrigin::Default,
            template_id: "default".to_owned(),
            summary: None,
            scratchpad: String::new(),
            llm_usage: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        assert_eq!(
            SummaryStatus::of(Some(&meeting), false),
            SummaryStatus::SkippedUnconfigured
        );
        assert_eq!(
            SummaryStatus::of(Some(&meeting), true),
            SummaryStatus::SkippedRunnable
        );
        meeting.state = MeetingState::Processing;
        assert_eq!(
            SummaryStatus::of(Some(&meeting), true),
            SummaryStatus::Pending
        );
        meeting.summary = Some(steno_core::SummaryDocument {
            template_id: "default".to_owned(),
            language: None,
            sections: Vec::new(),
        });
        assert_eq!(
            SummaryStatus::of(Some(&meeting), false),
            SummaryStatus::Present
        );

        let row = SummaryStatus::SkippedRunnable
            .skipped_row(DetailTab::Summary)
            .unwrap();
        assert_eq!(row.action, SkippedAction::RunSummary);
        assert_eq!(row.footnote, Some(copy::SUMMARY_RUNNABLE_FOOTNOTE));
        let tasks = SummaryStatus::SkippedUnconfigured
            .skipped_row(DetailTab::Tasks)
            .unwrap();
        assert_eq!(tasks.action.title(), "Set up summaries");
        assert_eq!(tasks.footnote, None);
        assert_eq!(
            SummaryStatus::SkippedRunnable.skipped_row(DetailTab::Transcript),
            None
        );
        assert_eq!(SummaryStatus::Present.skipped_row(DetailTab::Summary), None);
    }

    /// Swift: `SetupStatusTests.testExportStatusKeysOffTheDeliveriesAndTheVault`.
    #[test]
    fn export_status_keys_off_the_deliveries_and_the_vault() {
        assert_eq!(ExportStatus::of(&[], false), ExportStatus::NoVault);
        assert_eq!(ExportStatus::of(&[], true), ExportStatus::NotExported);
        let delivery = Delivery {
            id: uuid::Uuid::nil(),
            meeting_id: uuid::Uuid::nil(),
            destination_id: "obsidian-folder".to_owned(),
            status: DeliveryStatus::Pending,
            last_attempt_at: None,
            receipt: None,
        };
        assert_eq!(
            ExportStatus::of(std::slice::from_ref(&delivery), false),
            ExportStatus::Exported(vec![delivery.clone()])
        );
        assert!(!all_delivered(&[delivery]));
        assert!(all_delivered(&[]));
    }
}
