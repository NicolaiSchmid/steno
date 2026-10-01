import Foundation
import StenoBridge
import StenoCore

// The onboarding window's topic as a pure mapping from `OnboardingViewModel`
// (and the two Settings view models it owns) to the contract snapshot (plan
// Decision 6). The model owns every rule: which steps are required, when
// page 1 is complete, what a saved row says, when onboarding is finished.
// This spells that state in the wire vocabulary and nothing more.

extension OnboardingSnapshot {
  @MainActor
  init(model: OnboardingViewModel) {
    self.init(
      page: model.page == .permissions ? .permissions : .setup,
      permissions: model.steps.map { step in
        PermissionStep(
          kind: SettingsSnapshots.kind(step.kind), state: SettingsSnapshots.state(step.state),
          isRequired: step.isRequired, isRequesting: model.requesting == step.kind,
          isSkipped: model.skipped.contains(step.kind))
      },
      permissionsComplete: model.isComplete,
      setup: OnboardingViewModel.SetupStep.allCases.map { step in
        SetupStep(
          kind: step == .summaries ? .summaries : .vault, modelState: model.setupState(of: step))
      },
      canSaveSummaries: model.canSaveSummaries,
      // The Settings page's Summaries snapshot without a sidebar subtitle,
      // so the consent card and the endpoint form are one component.
      summaries: model.llm.map { SummariesSettingsSnapshot(llm: $0, subtitle: "") },
      vault: model.obsidian.map { Vault(obsidian: $0) },
      retentionSentence: model.retentionSentence, finished: model.finished)
  }
}

extension OnboardingSnapshot.SetupStep {
  /// The row's wire state from the model's; the saved line rides along.
  init(kind: Kind, modelState: OnboardingViewModel.SetupState) {
    switch modelState {
    case .open: self.init(kind: kind, state: State.open)
    case .saved(let line): self.init(kind: kind, state: State.saved, savedLine: line)
    case .skipped: self.init(kind: kind, state: State.skipped)
    }
  }
}

extension OnboardingSnapshot.Vault {
  @MainActor
  init(obsidian: ObsidianSettingsViewModel) {
    let vault = obsidian.vaultURL
    self.init(
      path: vault?.path, name: vault == nil ? nil : obsidian.vaultName,
      validationMessage: obsidian.validationMessage, error: obsidian.error,
      errorDetails: obsidian.errorDetails)
  }
}
