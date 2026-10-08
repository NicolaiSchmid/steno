import { z } from "zod";

/**
 * The JSON contract with the Swift host. Swift is the source of truth
 * (`Sources/StenoBridge`); it records one fixture per type into
 * `fixtures/bridge/`, and `contract.test.ts` parses every fixture with the
 * schema here, so a rename on either side fails one CI or the other.
 * Snapshots flow host to page as events; commands flow page to host as
 * method calls with a reply. Objects are strict: an unknown key is drift.
 */

const uuid = z
	.string()
	.regex(/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i);
/** `2026-09-29T12:50:00.000Z`: UTC, three fraction digits, as Swift writes. */
const isoDate = z
	.string()
	.regex(/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/);

type JsonValue =
	| string
	| number
	| boolean
	| null
	| JsonValue[]
	| { [key: string]: JsonValue };
const jsonValue: z.ZodType<JsonValue> = z.lazy(() =>
	z.union([
		z.string(),
		z.number(),
		z.boolean(),
		z.null(),
		z.array(jsonValue),
		z.record(z.string(), jsonValue),
	]),
);

// ─── Shared vocabulary ────────────────────────────────────────────────────

export const bridgeTopics = [
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
] as const;
export type BridgeTopic = (typeof bridgeTopics)[number];

export const bridgeMethods = [
	"page.ready",
	"page.layout",
	"meetings.setFilter",
	"meetings.setTagFilter",
	"meetings.setQuery",
	"meetings.select",
	"meetings.delete",
	"meeting.setTab",
	"meeting.setTags",
	"meeting.setTemplate",
	"meeting.rerunSummary",
	"meeting.reexport",
	"meeting.setKeepAudio",
	"meeting.deleteRecordingNow",
	"meeting.saveNotes",
	"meeting.revealRecording",
	"meeting.revealExport",
	"speakers.options",
	"speakers.select",
	"speakers.play",
	"speakers.stop",
	"recording.start",
	"recording.stop",
	"recording.toggle",
	"recording.keepGoing",
	"recording.clearMessages",
	"setup.dismissBanner",
	"settings.general.setLaunchAtLogin",
	"settings.general.setDetectionEnabled",
	"settings.general.setDefaultTemplate",
	"settings.general.requestCalendar",
	"settings.general.setAutomaticUpdates",
	"settings.general.openLoginItems",
	"settings.recording.setInputDevice",
	"settings.recording.refreshDevices",
	"settings.recording.chooseFolder",
	"settings.recording.revealFolder",
	"settings.recording.setRetention",
	"settings.recording.requestPermission",
	"settings.transcription.setEngine",
	"settings.transcription.download",
	"settings.transcription.remove",
	"settings.summaries.selectPreset",
	"settings.summaries.update",
	"settings.summaries.save",
	"settings.summaries.test",
	"settings.summaries.confirmCodex",
	"settings.summaries.refreshCodexStatus",
	"settings.summaries.refreshCodexModels",
	"settings.summaries.selectCodexModel",
	"settings.summaries.stopUsingCodex",
	"settings.export.setEnabled",
	"settings.export.chooseVault",
	"settings.export.update",
	"settings.export.save",
	"settings.iphone.beginPairing",
	"settings.iphone.cancelPairing",
	"settings.iphone.revoke",
	"onboarding.request",
	"onboarding.skip",
	"onboarding.refresh",
	"onboarding.advance",
	"onboarding.back",
	"onboarding.saveSummaries",
	"onboarding.confirmSummariesWithCodex",
	"onboarding.chooseVault",
	"onboarding.saveVault",
	"onboarding.skipSetup",
	"onboarding.finish",
	"updates.check",
	"system.openURL",
	"system.openSystemSettings",
	"window.open",
	"window.close",
	"ui.confirmDestructive",
] as const;
export type BridgeMethod = (typeof bridgeMethods)[number];

export const permissionKind = z.enum([
	"microphone",
	"systemAudio",
	"calendar",
	"localNetwork",
]);
export const permissionState = z.enum(["unknown", "granted", "denied"]);
export const settingsSection = z.enum([
	"general",
	"recording",
	"transcription",
	"summaries",
	"export",
	"iphone",
]);
export const bridgeWindow = z.enum(["main", "settings", "onboarding"]);
export const meetingSource = z.enum(["call", "inPerson", "phone"]);
export const meetingState = z.enum([
	"recording",
	"queued",
	"processing",
	"ready",
	"failed",
]);
export const captureMode = z.enum(["call", "inPerson"]);
export const listFilter = z.enum(["all", "processing", "ready", "failed"]);
export const detailTab = z.enum(["summary", "transcript", "tasks", "notes"]);
/**
 * The OS the host runs on. Not a message: the Tauri shell sets it as
 * `window.__STENO_PLATFORM__` before the page's scripts run, and the Swift
 * app sets nothing (`src/lib/platform.tsx`).
 */
export const platform = z.enum(["macos", "windows", "linux"]);
export type PlatformOS = z.infer<typeof platform>;

// ─── Envelope ─────────────────────────────────────────────────────────────

export const bridgeError = z
	.object({
		code: z.enum([
			"unknownMethod",
			"invalidParams",
			"notFound",
			"failed",
			"cancelled",
		]),
		message: z.string(),
	})
	.strict();

export const bridgeRequest = z
	.object({
		id: z.string(),
		method: z.enum(bridgeMethods),
		params: jsonValue.optional(),
	})
	.strict();

export const bridgeReply = z
	.object({
		id: z.string(),
		result: jsonValue.optional(),
		error: bridgeError.optional(),
	})
	.strict();

export type BridgeRequest = z.infer<typeof bridgeRequest>;
export type BridgeReply = z.infer<typeof bridgeReply>;

export const bridgeEvent = z
	.object({
		topic: z.enum(bridgeTopics),
		payload: jsonValue,
	})
	.strict();

// ─── Snapshots ────────────────────────────────────────────────────────────

export const appSnapshot = z
	.object({
		version: z.string(),
		setupBanner: z
			.object({
				title: z.string(),
				body: z.string(),
				offersSummaries: z.boolean(),
				offersVault: z.boolean(),
			})
			.strict()
			.optional(),
		phone: z
			.object({
				name: z.string(),
				lastSyncAt: isoDate.optional(),
				isReachable: z.boolean(),
			})
			.strict()
			.optional(),
		requestedMeetingID: uuid.optional(),
		requestedSettingsSection: settingsSection.optional(),
	})
	.strict();
export type AppSnapshot = z.infer<typeof appSnapshot>;

export const recordingSnapshot = z
	.object({
		state: z.enum(["idle", "starting", "recording", "stopping"]),
		startedAt: isoDate.optional(),
		mode: captureMode.optional(),
		callApp: z.string().optional(),
		meetingID: uuid.optional(),
		level: z
			.object({ mic: z.number(), system: z.number() })
			.strict()
			.optional(),
		autoStop: z
			.object({
				remainingSeconds: z.number(),
				totalSeconds: z.number(),
				reason: z.string(),
			})
			.strict()
			.optional(),
		deniedPermissions: z.array(permissionKind),
		warning: z.string().optional(),
		error: z.string().optional(),
	})
	.strict();
export type RecordingSnapshot = z.infer<typeof recordingSnapshot>;

export const progressSnapshot = z
	.object({
		entries: z.array(
			z
				.object({
					meetingID: uuid,
					stage: z.string(),
					title: z.string(),
					fraction: z.number(),
					estimatedRemainingSeconds: z.number().optional(),
				})
				.strict(),
		),
	})
	.strict();
export type ProgressSnapshot = z.infer<typeof progressSnapshot>;

export const speakerChip = z
	.object({
		id: uuid,
		initial: z.string(),
		colorIndex: z.number().int(),
		isConfirmed: z.boolean(),
	})
	.strict();
export type SpeakerChip = z.infer<typeof speakerChip>;

export const meetingRow = z
	.object({
		id: uuid,
		title: z.string(),
		startedAt: isoDate,
		durationSeconds: z.number(),
		source: meetingSource,
		state: meetingState,
		failureReason: z.string().optional(),
		preview: z.string().optional(),
		hasSummary: z.boolean(),
		speakers: z.array(speakerChip),
		tags: z.array(z.string()),
	})
	.strict();
export type MeetingRow = z.infer<typeof meetingRow>;

export const meetingsListSnapshot = z
	.object({
		filter: listFilter,
		tagFilter: z.string().optional(),
		query: z.string(),
		counts: z
			.object({
				all: z.number().int(),
				processing: z.number().int(),
				ready: z.number().int(),
				failed: z.number().int(),
			})
			.strict(),
		tags: z.array(
			z.object({ name: z.string(), count: z.number().int() }).strict(),
		),
		groups: z.array(
			z
				.object({
					day: z.string().regex(/^\d{4}-\d{2}-\d{2}$/),
					meetings: z.array(meetingRow),
				})
				.strict(),
		),
		selection: uuid.optional(),
		error: z.string().optional(),
	})
	.strict();
export type MeetingsListSnapshot = z.infer<typeof meetingsListSnapshot>;

const template = z.object({ id: z.string(), name: z.string() }).strict();
export type Template = z.infer<typeof template>;

export const meetingDetailSnapshot = z
	.object({
		id: uuid,
		title: z.string(),
		startedAt: isoDate,
		durationSeconds: z.number(),
		language: z.string().optional(),
		source: meetingSource,
		state: meetingState,
		failureReason: z.string().optional(),
		endReason: z.string().optional(),
		tags: z.array(z.string()),
		tab: detailTab,
		retention: z
			.object({
				kind: z.enum([
					"deleted",
					"deletesOn",
					"keptUntilExportSucceeds",
					"keptProcessingFailed",
					"keptWhileProcessing",
					"keptForever",
				]),
				deletesAt: isoDate.optional(),
				keepsAudio: z.boolean(),
				showsKeepToggle: z.boolean(),
				filesExist: z.boolean(),
			})
			.strict(),
		speakers: z.array(
			z
				.object({
					id: uuid,
					clusterLabel: z.string(),
					displayName: z.string(),
					assignment: z.enum(["unknown", "suggested", "confirmed"]),
					personID: uuid.optional(),
					email: z.string().optional(),
					suggestionName: z.string().optional(),
					colorIndex: z.number().int(),
					hasClip: z.boolean(),
					isPlaying: z.boolean(),
				})
				.strict(),
		),
		templates: z.array(template),
		templateID: z.string(),
		summaryStatus: z
			.object({
				kind: z.enum([
					"pending",
					"present",
					"skippedUnconfigured",
					"skippedRunnable",
				]),
				title: z.string().optional(),
				body: z.string().optional(),
				actionTitle: z.string().optional(),
			})
			.strict(),
		summary: z.array(
			z
				.object({
					id: z.string(),
					heading: z.string(),
					bullets: z.array(
						z.object({ lead: z.string(), text: z.string() }).strict(),
					),
				})
				.strict(),
		),
		transcript: z.array(
			z
				.object({
					id: uuid,
					speakerID: uuid.optional(),
					speakerName: z.string(),
					startSeconds: z.number(),
					endSeconds: z.number(),
					text: z.string(),
				})
				.strict(),
		),
		tasks: z.array(
			z
				.object({
					id: uuid,
					text: z.string(),
					assigneeName: z.string().optional(),
					assigneeColorIndex: z.number().int().optional(),
					dueDate: isoDate.optional(),
					priority: z.enum(["low", "normal", "high"]),
					done: z.boolean(),
				})
				.strict(),
		),
		decisions: z.array(z.string()),
		notes: z.string(),
		export: z
			.object({
				status: z.enum(["notConfigured", "pending", "delivered", "failed"]),
				message: z.string(),
				canReexport: z.boolean(),
				canReveal: z.boolean(),
			})
			.strict(),
		canRerunSummary: z.boolean(),
		isBusy: z.boolean(),
		error: z.string().optional(),
	})
	.strict();
export type MeetingDetailSnapshot = z.infer<typeof meetingDetailSnapshot>;

const errorFields = {
	error: z.string().optional(),
	errorDetails: z.string().optional(),
};

export const settingsTemplate = z
	.object({ id: z.string(), name: z.string(), description: z.string() })
	.strict();
export const acknowledgement = z
	.object({
		group: z.enum(["speechModels", "libraries"]),
		name: z.string(),
		licence: z.string(),
		source: z.string(),
	})
	.strict();
export type Acknowledgement = z.infer<typeof acknowledgement>;

export const generalSettingsSnapshot = z
	.object({
		subtitle: z.string(),
		version: z.string(),
		loginItem: z.enum([
			"notRegistered",
			"enabled",
			"requiresApproval",
			"notFound",
		]),
		detectionEnabled: z.boolean(),
		defaultTemplateID: z.string(),
		templates: z.array(settingsTemplate),
		calendarPermission: permissionState,
		requestingCalendar: z.boolean(),
		updates: z
			.object({
				canCheck: z.boolean(),
				automaticallyChecks: z.boolean(),
				automaticallyDownloads: z.boolean(),
				lastCheckAt: isoDate.optional(),
				outcome: z.enum(["notChecked", "upToDate", "available", "failed"]),
				detail: z.string().optional(),
			})
			.strict(),
		acknowledgements: z.array(acknowledgement),
		...errorFields,
	})
	.strict();
export type GeneralSettingsSnapshot = z.infer<typeof generalSettingsSnapshot>;

export const retentionMode = z.enum([
	"deleteAfterProcessing",
	"keepDays",
	"keepForever",
]);
export const retention = z
	.object({ mode: retentionMode, days: z.number().int() })
	.strict();

export const recordingSettingsSnapshot = z
	.object({
		subtitle: z.string(),
		devices: z.array(z.object({ uid: z.string(), name: z.string() }).strict()),
		inputDeviceUID: z.string().optional(),
		audioFolderPath: z.string(),
		audioFolderName: z.string(),
		/** Sent by the Rust host for a Windows drive that is neither NTFS nor ReFS, or a network drive. */
		audioFolderWarning: z.string().optional(),
		folderUsage: z.enum(["measuring", "measured", "unavailable"]),
		folderUsageBytes: z.number().optional(),
		retention,
		retentionFootnote: z.string(),
		keptForeverCount: z.number().int().optional(),
		permissions: z.array(
			z
				.object({
					kind: permissionKind,
					state: permissionState,
					isRequesting: z.boolean(),
				})
				.strict(),
		),
		...errorFields,
	})
	.strict();
export type RecordingSettingsSnapshot = z.infer<
	typeof recordingSettingsSnapshot
>;

export const transcriptionSettingsSnapshot = z
	.object({
		subtitle: z.string(),
		engineID: z.string(),
		engines: z.array(z.object({ id: z.string(), name: z.string() }).strict()),
		showsEnginePicker: z.boolean(),
		assets: z.array(
			z
				.object({
					id: z.string(),
					name: z.string(),
					detail: z.string(),
					state: z.enum(["absent", "downloading", "installed", "failed"]),
					downloadFraction: z.number().optional(),
					downloadPhase: z.string().optional(),
					installedBytes: z.number().optional(),
					failure: z.string().optional(),
				})
				.strict(),
		),
		allInstalled: z.boolean(),
		...errorFields,
	})
	.strict();
export type TranscriptionSettingsSnapshot = z.infer<
	typeof transcriptionSettingsSnapshot
>;

export const summariesSettingsSnapshot = z
	.object({
		subtitle: z.string(),
		presets: z.array(
			z
				.object({
					id: z.string(),
					title: z.string(),
					needsAPIKey: z.boolean(),
					showsServerField: z.boolean(),
					modelPlaceholder: z.string(),
				})
				.strict(),
		),
		presetID: z.string(),
		baseURL: z.string(),
		model: z.string(),
		contextTokens: z.string(),
		defaultContextTokens: z.number().int(),
		hasAPIKey: z.boolean(),
		isConfigured: z.boolean(),
		isTesting: z.boolean(),
		testResult: z
			.object({ ok: z.boolean(), message: z.string() })
			.strict()
			.optional(),
		validationMessage: z.string().optional(),
		codex: z
			.object({
				confirmed: z.boolean(),
				signIn: z.enum(["notChecked", "signedIn", "unavailable"]),
				signInDetail: z.string().optional(),
				model: z.string(),
				models: z.array(
					z.object({ slug: z.string(), name: z.string() }).strict(),
				),
				isLoadingModels: z.boolean(),
				modelsError: z.string().optional(),
			})
			.strict()
			.optional(),
		...errorFields,
	})
	.strict();
export type SummariesSettingsSnapshot = z.infer<
	typeof summariesSettingsSnapshot
>;

export const exportSettingsSnapshot = z
	.object({
		subtitle: z.string(),
		enabled: z.boolean(),
		vaultPath: z.string().optional(),
		vaultName: z.string().optional(),
		peopleFolder: z.string(),
		includeAudio: z.boolean(),
		taskTag: z.string(),
		validationMessage: z.string().optional(),
		saved: z.boolean(),
		...errorFields,
	})
	.strict();
export type ExportSettingsSnapshot = z.infer<typeof exportSettingsSnapshot>;

export const phoneSettingsSnapshot = z
	.object({
		subtitle: z.string(),
		macID: z.string().optional(),
		devices: z.array(
			z
				.object({
					id: uuid,
					name: z.string(),
					pairedAt: isoDate,
					lastSeenAt: isoDate.optional(),
				})
				.strict(),
		),
		listener: z
			.object({
				state: z.enum([
					"unavailable",
					"stopped",
					"starting",
					"listening",
					"failed",
				]),
				port: z.number().int().optional(),
				failure: z.string().optional(),
			})
			.strict(),
		pairing: z
			.object({ expiresAt: isoDate, qrPNGBase64: z.string() })
			.strict()
			.optional(),
		receipts: z.array(
			z
				.object({
					deviceID: uuid,
					recordingID: uuid,
					receivedBytes: z.number(),
					totalBytes: z.number().optional(),
				})
				.strict(),
		),
		...errorFields,
	})
	.strict();
export type PhoneSettingsSnapshot = z.infer<typeof phoneSettingsSnapshot>;

export const onboardingSnapshot = z
	.object({
		page: z.enum(["permissions", "setup"]),
		permissions: z.array(
			z
				.object({
					kind: permissionKind,
					state: permissionState,
					isRequired: z.boolean(),
					isRequesting: z.boolean(),
					isSkipped: z.boolean(),
				})
				.strict(),
		),
		permissionsComplete: z.boolean(),
		setup: z.array(
			z
				.object({
					kind: z.enum(["summaries", "vault"]),
					state: z.enum(["open", "saved", "skipped"]),
					savedLine: z.string().optional(),
				})
				.strict(),
		),
		canSaveSummaries: z.boolean(),
		// The Settings page's Summaries snapshot, its `subtitle` empty: the
		// consent card and the endpoint form are one component on both pages.
		summaries: summariesSettingsSnapshot.optional(),
		vault: z
			.object({
				path: z.string().optional(),
				name: z.string().optional(),
				validationMessage: z.string().optional(),
				...errorFields,
			})
			.strict()
			.optional(),
		retentionSentence: z.string().optional(),
		finished: z.boolean(),
	})
	.strict();
export type OnboardingSnapshot = z.infer<typeof onboardingSnapshot>;

/** Snapshot schema per topic; `subscribe` parses every event through it. */
export const topicSchemas = {
	app: appSnapshot,
	recording: recordingSnapshot,
	progress: progressSnapshot,
	"meetings.list": meetingsListSnapshot,
	// `null` while no meeting is selected or its export has not loaded.
	"meeting.detail": meetingDetailSnapshot.nullable(),
	"settings.general": generalSettingsSnapshot,
	"settings.recording": recordingSettingsSnapshot,
	"settings.transcription": transcriptionSettingsSnapshot,
	"settings.summaries": summariesSettingsSnapshot,
	"settings.export": exportSettingsSnapshot,
	"settings.iphone": phoneSettingsSnapshot,
	onboarding: onboardingSnapshot,
} as const satisfies Record<BridgeTopic, z.ZodTypeAny>;
export type TopicSnapshot<T extends BridgeTopic> = z.infer<
	(typeof topicSchemas)[T]
>;

// ─── Commands ─────────────────────────────────────────────────────────────

export const pageLayoutParams = z
	.object({ window: bridgeWindow, width: z.number(), height: z.number() })
	.strict();
export const setFilterParams = z.object({ filter: listFilter }).strict();
export const setTagFilterParams = z
	.object({ tag: z.string().optional() })
	.strict();
export const setQueryParams = z.object({ query: z.string() }).strict();
export const meetingIDParams = z.object({ meetingID: uuid }).strict();
export const setTabParams = z.object({ tab: detailTab }).strict();
export const setTagsParams = z.object({ tags: z.array(z.string()) }).strict();
export const setTemplateParams = z.object({ templateID: z.string() }).strict();
export const setBoolParams = z.object({ value: z.boolean() }).strict();
export const setStringParams = z.object({ value: z.string() }).strict();
export const saveNotesParams = z
	.object({ meetingID: uuid, text: z.string() })
	.strict();
export const speakerOptionsParams = z
	.object({ speakerID: uuid, query: z.string() })
	.strict();
export const speakerOption = z
	.object({
		kind: z.enum(["person", "create", "unknown"]),
		label: z.string(),
		detail: z.string().optional(),
		personID: uuid.optional(),
	})
	.strict();
export type SpeakerOption = z.infer<typeof speakerOption>;
export const speakerOptionsReply = z
	.object({ prefill: z.string().optional(), options: z.array(speakerOption) })
	.strict();
export type SpeakerOptionsReply = z.infer<typeof speakerOptionsReply>;
export const selectSpeakerParams = z
	.object({ speakerID: uuid, option: speakerOption })
	.strict();
export const speakerIDParams = z.object({ speakerID: uuid }).strict();
export const startRecordingParams = z.object({ mode: captureMode }).strict();
export const setRetentionParams = z.object({ retention }).strict();
export const permissionKindParams = z.object({ kind: permissionKind }).strict();
export const assetIDParams = z.object({ assetID: z.string() }).strict();
export const setAutomaticUpdatesParams = z
	.object({
		automaticallyChecks: z.boolean(),
		automaticallyDownloads: z.boolean(),
	})
	.strict();
export const summariesUpdateParams = z
	.object({
		baseURL: z.string().optional(),
		model: z.string().optional(),
		contextTokens: z.string().optional(),
		apiKey: z.string().optional(),
	})
	.strict();
export const exportUpdateParams = z
	.object({
		peopleFolder: z.string().optional(),
		includeAudio: z.boolean().optional(),
		taskTag: z.string().optional(),
	})
	.strict();
export const deviceIDParams = z.object({ deviceID: uuid }).strict();
export const setupStepParams = z
	.object({ step: z.enum(["summaries", "vault"]) })
	.strict();
export const openURLParams = z.object({ url: z.string() }).strict();
export const windowParams = z
	.object({
		window: bridgeWindow,
		section: settingsSection.optional(),
		meetingID: uuid.optional(),
	})
	.strict();
export const confirmDestructiveParams = z
	.object({ title: z.string(), message: z.string(), confirmTitle: z.string() })
	.strict();
export const confirmReply = z.object({ confirmed: z.boolean() }).strict();
export const chosenPathReply = z
	.object({ path: z.string().optional() })
	.strict();

/** Params schema per method; `null` means the method takes no params. */
export const methodParams = {
	"page.ready": null,
	"page.layout": pageLayoutParams,
	"meetings.setFilter": setFilterParams,
	"meetings.setTagFilter": setTagFilterParams,
	"meetings.setQuery": setQueryParams,
	"meetings.select": meetingIDParams,
	"meetings.delete": meetingIDParams,
	"meeting.setTab": setTabParams,
	"meeting.setTags": setTagsParams,
	"meeting.setTemplate": setTemplateParams,
	"meeting.rerunSummary": null,
	"meeting.reexport": null,
	"meeting.setKeepAudio": setBoolParams,
	"meeting.deleteRecordingNow": null,
	"meeting.saveNotes": saveNotesParams,
	"meeting.revealRecording": null,
	"meeting.revealExport": null,
	"speakers.options": speakerOptionsParams,
	"speakers.select": selectSpeakerParams,
	"speakers.play": speakerIDParams,
	"speakers.stop": null,
	"recording.start": startRecordingParams,
	"recording.stop": null,
	"recording.toggle": null,
	"recording.keepGoing": null,
	"recording.clearMessages": null,
	"setup.dismissBanner": null,
	"settings.general.setLaunchAtLogin": setBoolParams,
	"settings.general.setDetectionEnabled": setBoolParams,
	"settings.general.setDefaultTemplate": setTemplateParams,
	"settings.general.requestCalendar": null,
	"settings.general.setAutomaticUpdates": setAutomaticUpdatesParams,
	"settings.general.openLoginItems": null,
	"settings.recording.setInputDevice": setStringParams,
	"settings.recording.refreshDevices": null,
	"settings.recording.chooseFolder": null,
	"settings.recording.revealFolder": null,
	"settings.recording.setRetention": setRetentionParams,
	"settings.recording.requestPermission": permissionKindParams,
	"settings.transcription.setEngine": setStringParams,
	"settings.transcription.download": assetIDParams,
	"settings.transcription.remove": assetIDParams,
	"settings.summaries.selectPreset": setStringParams,
	"settings.summaries.update": summariesUpdateParams,
	"settings.summaries.save": null,
	"settings.summaries.test": null,
	"settings.summaries.confirmCodex": null,
	"settings.summaries.refreshCodexStatus": null,
	"settings.summaries.refreshCodexModels": null,
	"settings.summaries.selectCodexModel": setStringParams,
	"settings.summaries.stopUsingCodex": null,
	"settings.export.setEnabled": setBoolParams,
	"settings.export.chooseVault": null,
	"settings.export.update": exportUpdateParams,
	"settings.export.save": null,
	"settings.iphone.beginPairing": null,
	"settings.iphone.cancelPairing": null,
	"settings.iphone.revoke": deviceIDParams,
	"onboarding.request": permissionKindParams,
	"onboarding.skip": permissionKindParams,
	"onboarding.refresh": null,
	"onboarding.advance": null,
	"onboarding.back": null,
	"onboarding.saveSummaries": null,
	"onboarding.confirmSummariesWithCodex": null,
	"onboarding.chooseVault": null,
	"onboarding.saveVault": null,
	"onboarding.skipSetup": setupStepParams,
	"onboarding.finish": null,
	"updates.check": null,
	"system.openURL": openURLParams,
	"system.openSystemSettings": permissionKindParams,
	"window.open": windowParams,
	"window.close": windowParams,
	"ui.confirmDestructive": confirmDestructiveParams,
} as const satisfies Record<BridgeMethod, z.ZodTypeAny | null>;

/** Reply schema per method that returns a value; others resolve to `undefined`. */
export const methodReplies = {
	"speakers.options": speakerOptionsReply,
	"meetings.delete": confirmReply,
	"meeting.deleteRecordingNow": confirmReply,
	"meeting.setKeepAudio": confirmReply,
	"settings.recording.chooseFolder": chosenPathReply,
	"settings.export.chooseVault": chosenPathReply,
	"onboarding.chooseVault": chosenPathReply,
	"ui.confirmDestructive": confirmReply,
} as const satisfies Partial<Record<BridgeMethod, z.ZodTypeAny>>;

type ParamsOf<M extends BridgeMethod> =
	(typeof methodParams)[M] extends z.ZodTypeAny
		? z.infer<(typeof methodParams)[M]>
		: undefined;
type ReplyOf<M extends BridgeMethod> = M extends keyof typeof methodReplies
	? z.infer<(typeof methodReplies)[M]>
	: undefined;
export type MethodParams<M extends BridgeMethod> = ParamsOf<M>;
export type MethodReply<M extends BridgeMethod> = ReplyOf<M>;

/**
 * Fixture file name → schema. `BridgeSamples.fixtures` on the Swift side
 * lists the same names; `contract.test.ts` checks both directions.
 */
export const fixtureSchemas = {
	...topicSchemas,
	"recording.live": recordingSnapshot,
	"settings.summaries.codex": summariesSettingsSnapshot,
	"settings.iphone.pairing": phoneSettingsSnapshot,
	"onboarding.setup": onboardingSnapshot,
	"envelope.request": bridgeRequest,
	"envelope.reply": bridgeReply,
	"envelope.error": bridgeReply,
	"envelope.event": bridgeEvent,
	"speakers.options.reply": speakerOptionsReply,
	"params.page.layout": pageLayoutParams,
	"params.meetings.setFilter": setFilterParams,
	"params.meetings.setTagFilter": setTagFilterParams,
	"params.meetings.setQuery": setQueryParams,
	"params.meetingID": meetingIDParams,
	"params.meeting.setTab": setTabParams,
	"params.meeting.setTags": setTagsParams,
	"params.meeting.setTemplate": setTemplateParams,
	"params.bool": setBoolParams,
	"params.string": setStringParams,
	"params.meeting.saveNotes": saveNotesParams,
	"params.speakers.options": speakerOptionsParams,
	"params.speakers.select": selectSpeakerParams,
	"params.speakerID": speakerIDParams,
	"params.recording.start": startRecordingParams,
	"params.settings.recording.setRetention": setRetentionParams,
	"params.permissionKind": permissionKindParams,
	"params.assetID": assetIDParams,
	"params.settings.general.setAutomaticUpdates": setAutomaticUpdatesParams,
	"params.settings.summaries.update": summariesUpdateParams,
	"params.settings.export.update": exportUpdateParams,
	"params.deviceID": deviceIDParams,
	"params.onboarding.setupStep": setupStepParams,
	"params.system.openURL": openURLParams,
	"params.window": windowParams,
	"params.ui.confirmDestructive": confirmDestructiveParams,
	"reply.confirm": confirmReply,
	"reply.chosenPath": chosenPathReply,
} as const satisfies Record<string, z.ZodTypeAny>;
