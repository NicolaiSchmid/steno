import { personColor } from "@/lib/person-color";
import type {
	ExportSettingsSnapshot,
	GeneralSettingsSnapshot,
	MeetingDetailSnapshot,
	MeetingRow,
	MeetingsListSnapshot,
	OnboardingSnapshot,
	PhoneSettingsSnapshot,
	ProgressSnapshot,
	RecordingSnapshot,
	SpeakerOption,
	SpeakerOptionsReply,
	SummariesSettingsSnapshot,
	TranscriptionSettingsSnapshot,
} from "./contract";
import { detailTab } from "./contract";
import { BridgeError, type BridgeTransport, SnapshotHub } from "./transport";

/**
 * Transport for the browser, Playwright and unit tests. Snapshots come from
 * `fixtures/bridge/<topic>.json`, replies from
 * `fixtures/bridge/<method>.reply.json`; both are recorded by the Swift side
 * (`BridgeContractTests`). The `params.*`, `envelope.*`, `reply.*` and
 * `index` files document the wire shapes and are not snapshots; the generic
 * `reply.*` files also stand in for the native panels (`REPLY_ALIASES`).
 * Commands without a recorded reply resolve to `undefined` and are logged in
 * `calls` so tests can assert on them. `applyScenario` bends the fixtures
 * into the states the screens need (`?scenario=`, `?tab=`).
 */

export type FixtureMap = Record<string, unknown>;

export interface MockTransportOptions {
	/** Snapshots by topic, emitted on the first subscribe. */
	snapshots?: FixtureMap | (() => Promise<FixtureMap>);
	/**
	 * Replies by method. A value that is an `Error` rejects the call; a
	 * function is called with the params and its result is the reply.
	 */
	replies?: FixtureMap | (() => Promise<FixtureMap>);
}

export interface RecordedCall {
	method: string;
	params: unknown;
}

export interface MockTransport extends BridgeTransport {
	/** Pushes a snapshot as the host would. */
	emit(topic: string, snapshot: unknown): void;
	/** Every `call` so far, oldest first. */
	readonly calls: readonly RecordedCall[];
	/** Resolves once the fixture snapshots have been loaded and emitted. */
	ready(): Promise<void>;
}

const FIXTURE_SUFFIX = ".json";
const REPLY_SUFFIX = ".reply";

/** `../../fixtures/bridge/meetings.list.json` -> `meetings.list`. */
export function fixtureKey(path: string): string {
	const name = path.slice(path.lastIndexOf("/") + 1);
	return name.endsWith(FIXTURE_SUFFIX)
		? name.slice(0, -FIXTURE_SUFFIX.length)
		: name;
}

/** Whether a fixture key names a topic snapshot (not a reply, params or envelope). */
export function isSnapshotKey(key: string): boolean {
	return (
		key !== "index" &&
		!key.endsWith(REPLY_SUFFIX) &&
		!key.startsWith("params.") &&
		!key.startsWith("envelope.") &&
		!key.startsWith("reply.")
	);
}

/** `speakers.options.reply` -> `speakers.options`, or null when not a reply. */
export function replyMethod(key: string): string | null {
	return key.endsWith(REPLY_SUFFIX) ? key.slice(0, -REPLY_SUFFIX.length) : null;
}

/**
 * The generic reply shapes stand in for the methods that answer with them,
 * so the native alerts and folder panels resolve in the browser (always
 * confirmed; the recorded path).
 */
const REPLY_ALIASES: Record<string, readonly string[]> = {
	"reply.confirm": [
		"ui.confirmDestructive",
		"meetings.delete",
		"meeting.processAgain",
		"meeting.deleteRecordingNow",
		"meeting.setKeepAudio",
		"settings.iphone.pairAgain",
	],
	"reply.chosenPath": [
		"settings.recording.chooseFolder",
		"settings.export.chooseVault",
		"onboarding.chooseVault",
	],
};

/** The methods a fixture key answers: a reply file, or an aliased shape. */
export function replyMethods(key: string): readonly string[] {
	const direct = replyMethod(key);
	if (direct !== null) {
		return [direct];
	}
	return REPLY_ALIASES[key] ?? [];
}

async function loadGlob(
	modules: Record<string, () => Promise<unknown>>,
	keysFor: (key: string) => readonly string[],
): Promise<FixtureMap> {
	const entries = await Promise.all(
		Object.entries(modules).map(async ([path, load]) => {
			const keys = keysFor(fixtureKey(path));
			if (keys.length === 0) {
				return [];
			}
			const mod = (await load()) as { default?: unknown };
			const value = mod.default ?? mod;
			return keys.map((key) => [key, value] as const);
		}),
	);
	return Object.fromEntries(entries.flat());
}

const fixtureModules = () =>
	import.meta.glob([
		"../../fixtures/bridge/*.json",
		"!**/index.json",
		"!**/params.*.json",
		"!**/envelope.*.json",
	]);

/** The recorded snapshots, bundled lazily so the production build stays lean. */
export function loadFixtureSnapshots(): Promise<FixtureMap> {
	return loadGlob(fixtureModules(), (key) => (isSnapshotKey(key) ? [key] : []));
}

/** The recorded replies by method. */
export function loadFixtureReplies(): Promise<FixtureMap> {
	return loadGlob(fixtureModules(), replyMethods);
}

// ─── Scenarios ────────────────────────────────────────────────────────────

export const scenarios = [
	"empty",
	"recording",
	"recording-elsewhere",
	"denied",
	"failed",
	"summary-failed",
	"processing",
	"export-failed",
	"summary-key-withheld",
	// Settings
	"settings-error",
	"download-failed",
	"summaries-connected",
	"summaries-failed",
	"codex",
	"codex-consent",
	"export-on",
	"pairing",
	"phone-unavailable",
	"phone-waiting",
	// Onboarding
	"onboarding-unknown",
	"onboarding-waits-for-recording",
	"onboarding-denied",
	"onboarding-granted",
	"onboarding-setup",
	"onboarding-setup-open",
	"onboarding-codex",
	"onboarding-vault-saved",
	"onboarding-import",
	"onboarding-import-waiting",
	"onboarding-handoff",
	"onboarding-handoff-downloading",
] as const;

/**
 * The page's query, wherever it sits: `?scenario=empty#/main` and
 * `#/main?scenario=empty` both work; the hash wins on a clash.
 */
export function queryFromLocation(
	target: Pick<Location, "search" | "hash"> | undefined = typeof location ===
	"undefined"
		? undefined
		: location,
): URLSearchParams {
	const params = new URLSearchParams(target?.search ?? "");
	const hashQuery = target?.hash.split("?")[1];
	if (hashQuery) {
		for (const [key, value] of new URLSearchParams(hashQuery)) {
			params.set(key, value);
		}
	}
	return params;
}

const PROCESSING_MEETING_ID = "00000000-0000-0000-0000-000000000002";
const FAILED_MEETING_ID = "00000000-0000-0000-0000-000000000044";
/** How long the live recording has run when the page opens: 12:34. */
const RECORDING_ELAPSED_MS = 754_000;
/** The meeting `recording-elsewhere` records: not the one selected. */
const RECORDING_ELSEWHERE_ID = "00000000-0000-0000-0000-000000000003";

function findRow(list: MeetingsListSnapshot, id: string): MeetingRow | null {
	for (const group of list.groups) {
		const row = group.meetings.find((meeting) => meeting.id === id);
		if (row) {
			return row;
		}
	}
	return null;
}

/** A detail with nothing produced yet, over the base fixture's shape. */
function bareDetail(
	base: MeetingDetailSnapshot,
	row: MeetingRow,
	overrides: Partial<MeetingDetailSnapshot>,
): MeetingDetailSnapshot {
	const { language: _language, failureReason: _reason, ...rest } = base;
	return {
		...rest,
		id: row.id,
		title: row.title,
		startedAt: row.startedAt,
		durationSeconds: row.durationSeconds,
		source: row.source,
		state: row.state,
		tags: row.tags,
		speakers: [],
		summary: [],
		transcript: [],
		tasks: [],
		decisions: [],
		notes: "",
		summaryStatus: { kind: "pending" },
		...overrides,
	};
}

/**
 * Overrides the fixture snapshots for a scenario named in the query:
 * `empty` (no meetings, no selection), `recording` (`recording.live` as the
 * recording, started 12:34 ago, its auto-stop counting down, and the
 * meeting it records selected and live: nothing produced, no duration yet),
 * `recording-elsewhere` (that, while another meeting is selected; with
 * `recordingTitle=` for its title and `query=` for a search), `denied` (idle
 * with the microphone denied), `failed` (the failed meeting selected),
 * `processing` (a meeting in the progress entry, selected), `export-failed`
 * (the selected meeting's export failed), `damaged-audio`
 * (`meeting.detail.damagedAudio`: parts of the recording replaced by
 * silence), `kept-incomplete` (`meeting.detail.keptIncomplete`: the
 * recording kept because the speakers or the transcript may be
 * incomplete), `summary-key-withheld` (the selected meeting's summary
 * skipped while the API key was withheld, from
 * `meeting.detail.keyWithheld`), and the speaker naming states
 * (`speakersScenario`), and `speakers-finding` (the processing meeting
 * in the Diarize stage). `tab=` picks the detail tab. Without either, the
 * fixtures pass through unchanged.
 */
export function applyScenario(
	snapshots: FixtureMap,
	params: URLSearchParams,
): FixtureMap {
	const result = { ...snapshots };
	const list = snapshots["meetings.list"] as MeetingsListSnapshot | undefined;
	const detail = snapshots["meeting.detail"] as
		| MeetingDetailSnapshot
		| undefined;
	const scenario = params.get("scenario");

	if (scenario === "empty" && list) {
		const { selection: _selection, tagFilter: _tag, ...rest } = list;
		result["meetings.list"] = {
			...rest,
			counts: { all: 0, processing: 0, ready: 0, failed: 0 },
			tags: [],
			groups: [],
		} satisfies MeetingsListSnapshot;
		delete result["meeting.detail"];
	}

	if (scenario === "recording" || scenario === "recording-elsewhere") {
		const live = snapshots["recording.live"] as RecordingSnapshot | undefined;
		if (live) {
			// `recording-elsewhere` records another meeting, named as the
			// host names it, `recordingTitle=` included.
			const elsewhere =
				params.get("recordingTitle") ??
				(list ? findRow(list, RECORDING_ELSEWHERE_ID)?.title : undefined);
			result.recording = {
				...live,
				...(scenario === "recording-elsewhere"
					? { meetingID: RECORDING_ELSEWHERE_ID }
					: {}),
				...(scenario === "recording-elsewhere" && elsewhere
					? { meetingTitle: elsewhere }
					: {}),
				startedAt: new Date(Date.now() - RECORDING_ELAPSED_MS).toISOString(),
			} satisfies RecordingSnapshot;
		}
		const row = live && list ? findRow(list, live.meetingID ?? "") : null;
		if (scenario === "recording" && list && detail && row) {
			// The meeting being recorded, as the host shows it: live, nothing
			// produced, and no duration until it stops.
			const liveRow: MeetingRow = {
				...row,
				state: "recording",
				durationSeconds: 0,
				hasSummary: false,
				speakers: [],
			};
			result["meetings.list"] = {
				...list,
				groups: list.groups.map((group) => ({
					...group,
					meetings: group.meetings.map((meeting) =>
						meeting.id === row.id ? liveRow : meeting,
					),
				})),
				selection: row.id,
			} satisfies MeetingsListSnapshot;
			result["meeting.detail"] = bareDetail(detail, liveRow, {
				canProcessAgain: false,
				canRerunSummary: false,
			});
		}
	}

	if (scenario === "recording-elsewhere" && list) {
		// `recordingTitle=` renames the meeting that records; `query=` searches
		// the list as the host would, so a search can leave that meeting out.
		const title = params.get("recordingTitle");
		const query = params.get("query")?.toLowerCase();
		const current = result["meetings.list"] as MeetingsListSnapshot;
		result["meetings.list"] = {
			...current,
			query: query ?? current.query,
			groups: current.groups
				.map((group) => ({
					...group,
					meetings: group.meetings
						.map((meeting) =>
							title && meeting.id === RECORDING_ELSEWHERE_ID
								? { ...meeting, title }
								: meeting,
						)
						.filter(
							(meeting) =>
								!query || meeting.title.toLowerCase().includes(query),
						),
				}))
				.filter((group) => group.meetings.length > 0),
		} satisfies MeetingsListSnapshot;
	}

	if (scenario === "denied") {
		const idle = snapshots.recording as RecordingSnapshot | undefined;
		if (idle) {
			result.recording = {
				...idle,
				deniedPermissions: ["microphone"],
			} satisfies RecordingSnapshot;
		}
	}

	if (scenario === "export-failed" && detail) {
		result["meeting.detail"] = {
			...detail,
			export: {
				status: "failed",
				message: "Obsidian · Failed: the vault folder could not be written.",
				canReexport: true,
				canReveal: false,
			},
		} satisfies MeetingDetailSnapshot;
	}

	if (scenario?.startsWith("speakers-") && detail) {
		result["meeting.detail"] = speakersScenario(detail, scenario);
	}

	const damagedAudio = snapshots["meeting.detail.damagedAudio"];
	if (scenario === "damaged-audio" && damagedAudio) {
		result["meeting.detail"] = damagedAudio;
	}
	const keptIncomplete = snapshots["meeting.detail.keptIncomplete"];
	if (scenario === "kept-incomplete" && keptIncomplete) {
		result["meeting.detail"] = keptIncomplete;
	}

	const keyWithheld = snapshots["meeting.detail.keyWithheld"] as
		| MeetingDetailSnapshot
		| undefined;
	if (scenario === "summary-key-withheld" && keyWithheld) {
		result["meeting.detail"] = keyWithheld;
	}

	if (scenario === "failed" && list && detail) {
		const row = findRow(list, FAILED_MEETING_ID);
		if (row) {
			result["meetings.list"] = { ...list, selection: row.id };
			result["meeting.detail"] = bareDetail(detail, row, {
				...(row.failureReason ? { failureReason: row.failureReason } : {}),
				retention: {
					kind: "keptProcessingFailed",
					keepsAudio: true,
					showsKeepToggle: false,
					filesExist: true,
				},
				canProcessAgain: true,
				// No transcript, so no summary to re-run (the host's rule).
				canRerunSummary: false,
			});
		}
	}

	// The same meeting failed later, in the summary: its transcript and
	// speakers are there, the summary and tasks are not, and Re-run summary
	// is offered.
	if (scenario === "summary-failed" && list && detail) {
		const row = findRow(list, FAILED_MEETING_ID);
		if (row) {
			const failureReason =
				"Summary failed: the server answered 500. Try again later.";
			result["meetings.list"] = {
				...list,
				groups: list.groups.map((group) => ({
					...group,
					meetings: group.meetings.map((meeting) =>
						meeting.id === row.id ? { ...meeting, failureReason } : meeting,
					),
				})),
				selection: row.id,
			};
			result["meeting.detail"] = bareDetail(detail, row, {
				failureReason,
				speakers: detail.speakers,
				transcript: detail.transcript,
				canProcessAgain: true,
				canRerunSummary: true,
			});
		}
	}

	if (
		(scenario === "processing" || scenario === "speakers-finding") &&
		list &&
		detail
	) {
		const row: MeetingRow = {
			id: PROCESSING_MEETING_ID,
			title: "Standup",
			startedAt: "2026-09-29T12:06:00.000Z",
			durationSeconds: 724,
			source: "call",
			state: "processing",
			hasSummary: false,
			speakers: [],
			tags: [],
		};
		const [first, ...others] = list.groups;
		const groups = first
			? [{ ...first, meetings: [row, ...first.meetings] }, ...others]
			: [{ day: "2026-09-29", meetings: [row] }];
		result["meetings.list"] = {
			...list,
			counts: {
				...list.counts,
				all: list.counts.all + 1,
				processing: list.counts.processing + 1,
			},
			groups,
			selection: row.id,
		} satisfies MeetingsListSnapshot;
		result["meeting.detail"] = bareDetail(detail, row, {
			retention: {
				kind: "keptWhileProcessing",
				keepsAudio: false,
				showsKeepToggle: false,
				filesExist: true,
			},
			canRerunSummary: false,
		});
		if (scenario === "speakers-finding") {
			result.progress = {
				entries: [
					{
						meetingID: PROCESSING_MEETING_ID,
						stage: "diarize",
						title: "Finding speakers",
						fraction: 0.48,
						estimatedRemainingSeconds: 40,
					},
				],
			} satisfies ProgressSnapshot;
		}
	}

	const tab = detailTab.safeParse(params.get("tab"));
	const current = result["meeting.detail"] as MeetingDetailSnapshot | undefined;
	if (tab.success && current) {
		result["meeting.detail"] = { ...current, tab: tab.data };
	}

	applySettingsScenario(result, snapshots, scenario);
	applyOnboardingScenario(result, snapshots, scenario);

	// `recording.live`, `meeting.detail.damagedAudio`,
	// `meeting.detail.keptIncomplete`, `settings.summaries.codex`,
	// `settings.summaries.fileKey`, `settings.iphone.pairing`,
	// `settings.iphone.waiting`, `onboarding.setup`, `onboarding.import`,
	// `onboarding.handoff` and `meeting.detail.keyWithheld` are fixtures, not
	// topics; the page never sees them by those names.
	delete result["recording.live"];
	delete result["meeting.detail.damagedAudio"];
	delete result["meeting.detail.keptIncomplete"];
	delete result["settings.summaries.codex"];
	delete result["settings.summaries.fileKey"];
	delete result["settings.iphone.pairing"];
	delete result["settings.iphone.waiting"];
	delete result["onboarding.setup"];
	delete result["onboarding.import"];
	delete result["onboarding.handoff"];
	delete result["meeting.detail.keyWithheld"];
	return result;
}

/**
 * The onboarding states: `onboarding-unknown` (a fresh install, nothing
 * asked yet), `onboarding-waits-for-recording` (that, the system audio
 * test refused while Steno records; Rust host only), `onboarding-denied`
 * (the microphone refused),
 * `onboarding-granted` (every permission granted, Continue instead of Later),
 * `onboarding-setup` (page 2 from `onboarding.setup`: Summaries saved, a
 * vault chosen but refused), `onboarding-setup-open` (page 2 with both rows
 * open), `onboarding-codex` (page 2 with ChatGPT chosen and its consent
 * card), `onboarding-vault-saved` (page 2 with the vault saved and the
 * Summaries form open), `onboarding-import` (the import step before page 1,
 * not run yet, two prompts), `onboarding-import-waiting` (the step after
 * a denied export, from `onboarding.import`, with Try again),
 * `onboarding-handoff` (page 1 after the update from the Swift app, from
 * `onboarding.handoff`: the speaker models to download and a vault whose
 * folder macOS refused) and `onboarding-handoff-downloading` (that, the
 * models downloading and the vault writable).
 */
function applyOnboardingScenario(
	result: FixtureMap,
	snapshots: FixtureMap,
	scenario: string | null,
) {
	const onboarding = snapshots.onboarding as OnboardingSnapshot | undefined;
	const setup = snapshots["onboarding.setup"] as OnboardingSnapshot | undefined;
	const importStep = snapshots["onboarding.import"] as
		| OnboardingSnapshot
		| undefined;
	const handoff = snapshots["onboarding.handoff"] as
		| OnboardingSnapshot
		| undefined;
	if (scenario === "onboarding-handoff" && handoff) {
		result.onboarding = handoff;
	}
	if (
		scenario === "onboarding-handoff-downloading" &&
		handoff?.speechModels &&
		handoff.vaultAccess
	) {
		result.onboarding = {
			...handoff,
			speechModels: {
				state: "downloading",
				detail: "Downloading… 40%",
				assetIDs: [],
				downloadFraction: 0.4,
			},
			vaultAccess: { state: "writable", name: handoff.vaultAccess.name },
		} satisfies OnboardingSnapshot;
	}
	if (!onboarding || !setup) {
		return;
	}

	if (scenario === "onboarding-import-waiting" && importStep) {
		result.onboarding = importStep;
	}

	if (scenario === "onboarding-import" && importStep) {
		result.onboarding = {
			...importStep,
			swiftImport: { state: "pending", prompts: 2 },
		} satisfies OnboardingSnapshot;
	}
	const untouched = onboarding.permissions.map((step) => ({
		...step,
		state: "unknown" as const,
		isRequesting: false,
		isSkipped: false,
	}));
	const openRows = setup.setup.map((row) => {
		const { savedLine: _line, ...rest } = row;
		return { ...rest, state: "open" as const };
	});
	const { validationMessage: _refused, ...chosenVault } = setup.vault ?? {};
	// The saved row's form names its model; an open row has none yet.
	const openSummaries = setup.summaries
		? { ...setup.summaries, model: "", isConfigured: false }
		: undefined;

	if (scenario === "onboarding-unknown") {
		result.onboarding = {
			...onboarding,
			permissions: untouched,
		} satisfies OnboardingSnapshot;
	}

	if (scenario === "onboarding-waits-for-recording") {
		result.onboarding = {
			...onboarding,
			permissions: untouched.map((step) =>
				step.kind === "systemAudio"
					? { ...step, waitsForRecording: true }
					: step,
			),
		} satisfies OnboardingSnapshot;
	}

	if (scenario === "onboarding-denied") {
		result.onboarding = {
			...onboarding,
			permissions: untouched.map((step) =>
				step.kind === "microphone" ? { ...step, state: "denied" } : step,
			),
		} satisfies OnboardingSnapshot;
	}

	if (scenario === "onboarding-granted") {
		result.onboarding = {
			...onboarding,
			permissions: untouched.map((step) => ({ ...step, state: "granted" })),
			permissionsComplete: true,
		} satisfies OnboardingSnapshot;
	}

	if (scenario === "onboarding-setup") {
		result.onboarding = setup;
	}

	if (scenario === "onboarding-setup-open") {
		result.onboarding = {
			...setup,
			setup: openRows,
			canSaveSummaries: false,
			summaries: openSummaries,
			vault: {},
		} satisfies OnboardingSnapshot;
	}

	if (scenario === "onboarding-codex" && openSummaries) {
		result.onboarding = {
			...setup,
			setup: openRows,
			canSaveSummaries: false,
			summaries: {
				...openSummaries,
				presetID: "codex",
				codex: {
					confirmed: false,
					signIn: "signedIn",
					signInDetail: "nicolai@example.com (Plus)",
					model: "",
					models: [],
					isLoadingModels: false,
				},
			},
			vault: {},
		} satisfies OnboardingSnapshot;
	}

	if (scenario === "onboarding-vault-saved") {
		result.onboarding = {
			...setup,
			setup: openRows.map((row) =>
				row.kind === "vault"
					? { ...row, state: "saved", savedLine: "Saved: Work Vault" }
					: row,
			),
			canSaveSummaries: false,
			summaries: openSummaries,
			vault: chosenVault,
		} satisfies OnboardingSnapshot;
	}
}

/**
 * The Settings states: `settings-error` (General with an error and its
 * details, a login item awaiting approval, an update available),
 * `download-failed` (the speech model's download failed), `summaries-connected`
 * and `summaries-failed` (a configured OpenAI endpoint with its test result),
 * `summaries-key-in-file` (a saved key the host keeps in the secrets file,
 * from `settings.summaries.fileKey`) and `summaries-key-in-keyring` (the
 * same key in the keyring),
 * `codex` (ChatGPT confirmed, from `settings.summaries.codex`), `codex-consent`
 * (ChatGPT chosen, not yet confirmed), `export-on` (a vault chosen and saved),
 * `pairing` (a code open and a transfer arriving, from
 * `settings.iphone.pairing`), `phone-unavailable` (no handover service),
 * `phone-waiting` (phone handover waits for Try again or Pair again, from
 * `settings.iphone.waiting`).
 */
function applySettingsScenario(
	result: FixtureMap,
	snapshots: FixtureMap,
	scenario: string | null,
) {
	const general = snapshots["settings.general"] as
		| GeneralSettingsSnapshot
		| undefined;
	const transcription = snapshots["settings.transcription"] as
		| TranscriptionSettingsSnapshot
		| undefined;
	const summaries = snapshots["settings.summaries"] as
		| SummariesSettingsSnapshot
		| undefined;
	const exportSettings = snapshots["settings.export"] as
		| ExportSettingsSnapshot
		| undefined;
	const phone = snapshots["settings.iphone"] as
		| PhoneSettingsSnapshot
		| undefined;

	if (scenario === "settings-error" && general) {
		result["settings.general"] = {
			...general,
			subtitle: "Update available: 0.10.1",
			loginItem: "requiresApproval",
			updates: {
				...general.updates,
				outcome: "available",
				detail: "0.10.1",
			},
			error: "The setting could not be saved.",
			errorDetails:
				"SettingsStoreError.writeFailed: The file “settings.json” couldn’t be saved in the folder “Steno”.",
		} satisfies GeneralSettingsSnapshot;
	}

	if (scenario === "download-failed" && transcription) {
		result["settings.transcription"] = {
			...transcription,
			subtitle: "Download needed",
			assets: transcription.assets.map((asset, index) =>
				index === 0
					? {
							...asset,
							state: "failed",
							detail: "Not downloaded · 485 MB",
							failure:
								"URLError.notConnectedToInternet: The Internet connection appears to be offline.",
						}
					: asset,
			),
		} satisfies TranscriptionSettingsSnapshot;
	}

	if (
		(scenario === "summaries-connected" || scenario === "summaries-failed") &&
		summaries
	) {
		const ok = scenario === "summaries-connected";
		result["settings.summaries"] = {
			...summaries,
			subtitle: "OpenAI",
			presetID: "openAI",
			// The hosted preset hides its address; an empty one keeps the
			// production bundle free of fetchable URLs (scripts/check-offline.mjs).
			baseURL: "",
			model: "gpt-4.1-mini",
			hasAPIKey: true,
			isConfigured: true,
			testResult: ok
				? {
						ok: true,
						message: "Connected. 12 models listed; structured output works.",
					}
				: {
						ok: false,
						message:
							"HTTP 401 from the service's models list: Incorrect API key provided.",
					},
		} satisfies SummariesSettingsSnapshot;
	}

	const fileKey = snapshots["settings.summaries.fileKey"] as
		| SummariesSettingsSnapshot
		| undefined;
	if (scenario === "summaries-key-in-file" && fileKey) {
		result["settings.summaries"] = fileKey;
	}
	if (scenario === "summaries-key-in-keyring" && fileKey) {
		result["settings.summaries"] = {
			...fileKey,
			keyStore: "keyring",
		} satisfies SummariesSettingsSnapshot;
	}

	if (scenario === "codex") {
		const codex = snapshots["settings.summaries.codex"];
		if (codex) {
			result["settings.summaries"] = codex;
		}
	}

	if (scenario === "codex-consent" && summaries) {
		result["settings.summaries"] = {
			...summaries,
			presetID: "codex",
			codex: {
				confirmed: false,
				signIn: "signedIn",
				signInDetail: "nicolai@example.com (Plus)",
				model: "",
				models: [],
				isLoadingModels: false,
			},
		} satisfies SummariesSettingsSnapshot;
	}

	if (scenario === "export-on" && exportSettings) {
		result["settings.export"] = {
			...exportSettings,
			subtitle: "Work Vault",
			enabled: true,
			vaultPath: "/Users/nicolai/Notes/Work Vault",
			vaultName: "Work Vault",
			saved: true,
		} satisfies ExportSettingsSnapshot;
	}

	if (scenario === "pairing") {
		const pairing = snapshots["settings.iphone.pairing"] as
			| PhoneSettingsSnapshot
			| undefined;
		if (pairing?.pairing) {
			result["settings.iphone"] = {
				...pairing,
				pairing: {
					...pairing.pairing,
					expiresAt: new Date(Date.now() + 4 * 60_000).toISOString(),
				},
			} satisfies PhoneSettingsSnapshot;
		}
	}

	const waiting = snapshots["settings.iphone.waiting"] as
		| PhoneSettingsSnapshot
		| undefined;
	if (scenario === "phone-waiting" && waiting) {
		result["settings.iphone"] = waiting;
	}

	if (scenario === "phone-unavailable" && phone) {
		const { macID: _macID, ...rest } = phone;
		result["settings.iphone"] = {
			...rest,
			subtitle: "Unavailable",
			devices: [],
			listener: { state: "unavailable" },
		} satisfies PhoneSettingsSnapshot;
	}
}

type DetailSpeaker = MeetingDetailSnapshot["speakers"][number];
type Turn = MeetingDetailSnapshot["transcript"][number];

/** A confirmed name long enough to truncate in the header, the panel and a 400 px column. */
export const LONG_NAME = "Maximiliane Alexandra von Hohenzollern-Sigmaringen";

/**
 * The scenarios that bend how the host answers naming rather than the
 * speakers: the pick never acknowledged (`speakers-saving`), refused
 * (`speakers-select-failed`), and the options for typed text never
 * arriving (`speakers-options-slow`) or failing (`speakers-options-failed`).
 * Each starts from `speakers-none`.
 */
const SPEAKER_ANSWER_SCENARIOS = new Set([
	"speakers-saving",
	"speakers-select-failed",
	"speakers-options-slow",
	"speakers-options-failed",
]);

/**
 * The scenarios that bend how the host answers a move, a separate or a
 * paragraph's play: the move refused as stale (`speakers-move-stale`),
 * Separate again refused (`speakers-separate-failed`), and a paragraph
 * whose audio fails after the host replied (`speakers-play-failed`). Each
 * starts from `speakers-partly`.
 */
const MOVE_ANSWER_SCENARIOS = new Set([
	"speakers-move-stale",
	"speakers-separate-failed",
	"speakers-play-failed",
]);

/** The host's words for a refused pick (`SelectError::Store`). */
export const SELECT_FAILURE = "Speaker could not be named: database is locked";

/** The host's words for a move against a transcript that changed. */
export const STALE_TRANSCRIPT =
	"The transcript changed. Refresh to see it as it is now.";

/** The host's words for Separate again after the voice's paragraphs moved. */
export const NOTHING_TO_SEPARATE =
	"Its paragraphs moved since. Nothing to separate.";

/** What the hosts call a turn without a voice. */
const NO_VOICE = "Unknown";

/**
 * The selected meeting's speakers bent into a naming state, its turns
 * handed out among them: `speakers-none` (Me and two speakers, nobody
 * named or suggested), `speakers-me` (Me named as the fixture's first
 * person, one speaker left), `speakers-me-unnamed` (Me unnamed beside a
 * named person and an unnamed voice), `speakers-split` (the fixture's
 * unnamed speaker suggested as a person already named there: one person
 * heard as two speakers), `speakers-check` (only the voice match left),
 * `speakers-done` (everyone confirmed), `speakers-long` (the fixture
 * with a long confirmed name), `speakers-dismissed` (the unnamed voice not
 * a person), `speakers-not-people` (no voice a person),
 * `speakers-likely` (the voice match only likely), `speakers-context`
 * (the fixture's speakers, no voice's words exactly what its clip plays)
 * and `speakers-last-invitee` (Anna confirmed, so the unnamed voice is the
 * only one left). Without one of these the fixture's own speakers stay:
 * two named (Nicolai with a voice combined into his), a suggestion and an
 * unnamed voice without a clip; `speakers-partly` keeps them and gives
 * each a paragraph, `speakers-no-voice` takes the voice off one paragraph
 * and `speakers-no-recording` deletes the meeting's recording. All but
 * `speakers-none`, `speakers-me` and `speakers-me-unnamed` spread the
 * turns over every voice (`spread`). Each voice with a clip quotes its
 * first paragraph, as the host quotes its clip; one without a clip, or
 * any under `speakers-context`, has it as its nearby words instead.
 */
function speakersScenario(
	detail: MeetingDetailSnapshot,
	requested: string,
): MeetingDetailSnapshot {
	return withQuotes(
		bendSpeakers(detail, requested),
		requested === "speakers-context",
	);
}

/**
 * Every voice's evidence: its first paragraph, cut as the host cuts it, as
 * the `quote` of what its clip plays, or as `context` (nearby words) when
 * it has no clip or `inexact`.
 */
function withQuotes(
	detail: MeetingDetailSnapshot,
	inexact: boolean,
): MeetingDetailSnapshot {
	return {
		...detail,
		speakers: detail.speakers.map((speaker) => {
			const said = detail.transcript.find(
				(turn) => turn.speakerID === speaker.id,
			)?.text;
			const { quote: _quote, context: _context, ...rest } = speaker;
			if (!said) {
				return rest;
			}
			const words = said.length > 160 ? `${said.slice(0, 160).trim()}…` : said;
			return speaker.hasClip && !inexact
				? { ...rest, quote: words }
				: { ...rest, context: words };
		}),
	};
}

function bendSpeakers(
	detail: MeetingDetailSnapshot,
	requested: string,
): MeetingDetailSnapshot {
	const [first, second, third, fourth] = detail.speakers;
	if (!first || !second || !third || !fourth) {
		return detail;
	}
	const scenario = SPEAKER_ANSWER_SCENARIOS.has(requested)
		? "speakers-none"
		: MOVE_ANSWER_SCENARIOS.has(requested)
			? "speakers-partly"
			: requested;
	const confirmed = (speaker: DetailSpeaker, name: string, person: string) => {
		const { suggestionName: _name, ...rest } = speaker;
		return {
			...rest,
			assignment: "confirmed" as const,
			displayName: name,
			personID: person,
			colorIndex: personColor(person),
		};
	};
	const renamed = (speakers: DetailSpeaker[]) => ({
		...detail,
		speakers,
		transcript: detail.transcript.map((turn) => {
			const speaker = speakers.find((row) => row.id === turn.speakerID);
			return speaker ? { ...turn, speakerName: speaker.displayName } : turn;
		}),
	});
	if (scenario === "speakers-partly" || scenario === "speakers-context") {
		return spread({ ...detail });
	}
	if (scenario === "speakers-no-voice") {
		const spreadOut = spread({ ...detail });
		return {
			...spreadOut,
			transcript: spreadOut.transcript.map((turn, index) => {
				if (index !== NO_VOICE_TURN) {
					return turn;
				}
				const { speakerID: _speaker, ...rest } = turn;
				return { ...rest, speakerName: NO_VOICE };
			}),
		};
	}
	if (scenario === "speakers-no-recording") {
		return spread({
			...detail,
			retention: {
				kind: "deleted",
				keepsAudio: false,
				showsKeepToggle: false,
				filesExist: false,
			},
		});
	}
	if (scenario === "speakers-last-invitee") {
		return spread(
			renamed([
				first,
				second,
				confirmed(third, "Anna", third.personID ?? MOCK_PERSON_ID),
				fourth,
			]),
		);
	}
	if (scenario === "speakers-dismissed") {
		return answerSpeakerChange(spread({ ...detail }), "speakers.dismiss", {
			speakerID: fourth.id,
		});
	}
	if (scenario === "speakers-not-people") {
		return detail.speakers.reduce(
			(bent, speaker) =>
				answerSpeakerChange(bent, "speakers.dismiss", {
					speakerID: speaker.id,
				}),
			spread({ ...detail }),
		);
	}
	if (scenario === "speakers-likely") {
		return spread({
			...detail,
			speakers: [first, second, { ...third, suggestionStrength: "likely" }],
		});
	}
	if (scenario === "speakers-split") {
		const { suggestionName: _name, ...rest } = fourth;
		const split: DetailSpeaker = {
			...rest,
			assignment: "suggested",
			displayName: first.displayName,
			suggestionName: first.displayName,
			...(first.personID
				? { personID: first.personID, colorIndex: personColor(first.personID) }
				: {}),
			hasClip: true,
		};
		return spread(renamed([first, second, third, split]));
	}
	if (scenario === "speakers-check") {
		return spread({ ...detail, speakers: [first, second, third] });
	}
	if (scenario === "speakers-done") {
		return spread(
			renamed([
				first,
				second,
				confirmed(third, "Anna", third.personID ?? MOCK_PERSON_ID),
			]),
		);
	}
	if (scenario === "speakers-long") {
		return spread(
			renamed([
				first,
				confirmed(second, LONG_NAME, second.personID ?? MOCK_PERSON_ID),
				third,
				fourth,
			]),
		);
	}
	if (
		scenario !== "speakers-none" &&
		scenario !== "speakers-me" &&
		scenario !== "speakers-me-unnamed"
	) {
		return detail;
	}
	const unnamed = (speaker: DetailSpeaker, label: string): DetailSpeaker => ({
		id: speaker.id,
		clusterLabel: label,
		displayName: label,
		assignment: "unknown",
		// A voice without a person has its own colour, as the hosts fold it.
		colorIndex: personColor(speaker.id),
		hasClip: label !== "Me",
		isPlaying: false,
		dismissed: false,
		combined: [],
	});
	const me =
		scenario === "speakers-me"
			? { ...first, clusterLabel: "Me", hasClip: false }
			: unnamed(first, "Me");
	const speakers =
		scenario === "speakers-me"
			? [me, unnamed(second, "Speaker 1")]
			: scenario === "speakers-me-unnamed"
				? [me, second, unnamed(third, "Speaker 2")]
				: [me, unnamed(second, "Speaker 1"), unnamed(third, "Speaker 2")];
	const transcript = detail.transcript.map((turn, index) => {
		const speaker = speakers[index % speakers.length] ?? me;
		return {
			...turn,
			speakerID: speaker.id,
			speakerName: speaker.displayName,
		};
	});
	return { ...detail, speakers, transcript };
}

const MOCK_PERSON_ID = "00000000-0000-0000-0000-0000000000FF";

/** A person created outside the app: an ID of its own, from the name. */
function mockPersonID(name: string): string {
	let hash = 0;
	for (const char of name.trim().toLowerCase()) {
		hash = (hash * 31 + (char.codePointAt(0) ?? 0)) % 0xffffffffff;
	}
	return `00000000-0000-0000-0001-${hash.toString(16).padStart(12, "0")}`;
}
const SPREAD_TURNS = 6;
/** The paragraph `speakers-no-voice` takes the voice off: Nicolai's second. */
const NO_VOICE_TURN = 4;

/**
 * The meeting's turns repeated to six and handed round its speakers, so
 * every voice, the suggested one too, leads a paragraph; turn IDs keep the
 * fixture's form, numbered on from the last. A turn that keeps its
 * fixture ID keeps the segments it folds; a repeat is one segment.
 */
function spread(detail: MeetingDetailSnapshot): MeetingDetailSnapshot {
	const turns = detail.transcript;
	const speakers = detail.speakers;
	if (turns.length === 0 || speakers.length === 0) {
		return detail;
	}
	const base = Number.parseInt(turns[0]?.id.slice(-12) ?? "0", 16);
	let at = turns[0]?.startSeconds ?? 0;
	const transcript = Array.from({ length: SPREAD_TURNS }, (_, index) => {
		const turn = turns[index % turns.length] as Turn;
		const speaker = speakers[index % speakers.length] as DetailSpeaker;
		const startSeconds = at;
		at += turn.endSeconds - turn.startSeconds;
		const id = `00000000-0000-0000-0000-${(base + index).toString(16).padStart(12, "0")}`;
		return {
			...turn,
			id,
			segmentIDs: id === turn.id ? turn.segmentIDs : [id],
			speakerID: speaker.id,
			speakerName: speaker.displayName,
			startSeconds,
			endSeconds: at,
		};
	});
	return { ...detail, transcript };
}

/**
 * `speakers.options` answered the way the host ranks them, from the
 * recorded reply: its people whose name contains the query, then, once
 * something is typed and no one is called exactly that, a row creating the
 * typed name. The recorded create row is for the recording's own query
 * ("a", `crates/steno-host/tests/parity.rs`), so it is left out. Used by the
 * page outside the app; the unit tests keep the recording. Under
 * `speakers-options-slow` the answer for typed text never comes, under
 * `speakers-options-failed` it fails.
 */
export function answerSpeakerOptions(
	recorded: SpeakerOptionsReply | undefined,
	params: unknown,
	scenario = "",
	rejected: ReadonlySet<string> = new Set(),
): SpeakerOptionsReply | Promise<SpeakerOptionsReply> {
	const query =
		typeof params === "object" && params !== null && "query" in params
			? String(params.query).trim()
			: "";
	const speakerID =
		typeof params === "object" && params !== null && "speakerID" in params
			? String(params.speakerID)
			: "";
	if (query && scenario === "speakers-options-slow") {
		return new Promise(() => {});
	}
	if (query && scenario === "speakers-options-failed") {
		throw new BridgeError("speakers.options", "failed", "The store is busy.");
	}
	const fold = (text: string) =>
		text.normalize("NFD").replace(/\p{M}/gu, "").toLowerCase();
	const answeredNo = (option: SpeakerOption) =>
		rejected.has(`${speakerID} ${option.personID}`);
	const people: SpeakerOption[] = (recorded?.options ?? [])
		.filter(
			(option) =>
				option.kind === "person" &&
				fold(option.label).includes(fold(query)) &&
				// A person answered No to is found only when typed,
				!(answeredNo(option) && !query),
		)
		// and then with no reason: no longer a suggestion.
		.map((option) => {
			if (!answeredNo(option)) {
				return option;
			}
			const { reason: _reason, strength: _strength, ...rest } = option;
			return rest;
		});
	const exact = people.some((option) => fold(option.label) === fold(query));
	if (scenario === "speakers-last-invitee" && !query) {
		// Anna owns Speaker 3 now, so no voice match; Maya is the one
		// invitee nobody is named as, for the one voice left.
		people.splice(
			0,
			people.length,
			{ kind: "create", label: "Maya", reason: "lastInvitee" },
			...people.map(({ reason: _reason, strength: _strength, ...option }) => ({
				...option,
				reason: "inThisMeeting" as const,
			})),
		);
	}
	return {
		options:
			query && !exact ? [...people, { kind: "create", label: query }] : people,
	};
}

/**
 * `speakers.reject`, `speakers.clear`, `speakers.dismiss` and
 * `speakers.restore` outside the app, as the hosts republish the meeting:
 * a No or Remove name leaves the voice unnamed; Not a person also reads its
 * turns "Not a person"; Restore brings it back unnamed.
 */
export function answerSpeakerChange(
	detail: MeetingDetailSnapshot,
	method: string,
	params: unknown,
): MeetingDetailSnapshot {
	const { speakerID, personID } = params as {
		speakerID: string;
		personID?: string;
	};
	const speaker = detail.speakers.find((row) => row.id === speakerID);
	if (!speaker) {
		return detail;
	}
	if (
		method === "speakers.reject" &&
		(speaker.assignment !== "suggested" || speaker.personID !== personID)
	) {
		return detail;
	}
	const {
		suggestionName: _name,
		suggestionStrength: _strength,
		personID: _person,
		email: _email,
		...rest
	} = speaker;
	const changed: DetailSpeaker =
		method === "speakers.restore"
			? { ...speaker, dismissed: false }
			: {
					...rest,
					assignment: "unknown",
					displayName: speaker.clusterLabel,
					colorIndex: personColor(speaker.id),
					dismissed: method === "speakers.dismiss" || speaker.dismissed,
				};
	return {
		...detail,
		speakers: detail.speakers.map((row) =>
			row.id === speakerID ? changed : row,
		),
		transcript: detail.transcript.map((turn) =>
			turn.speakerID === speakerID
				? {
						...turn,
						speakerName: changed.dismissed
							? "Not a person"
							: changed.displayName,
					}
				: turn,
		),
	};
}

/**
 * The detail's invitees without the one just named: a voice is that
 * person now, as the hosts leave out an invitee somebody's voice is.
 */
function withoutInvitee(
	detail: MeetingDetailSnapshot,
	option: SpeakerOption,
): MeetingDetailSnapshot {
	const { invitees: all = [], ...rest } = detail;
	const fold = (text: string) => text.trim().toLowerCase();
	const invitees = all.filter((invitee) =>
		option.personID && invitee.personID
			? invitee.personID !== option.personID
			: fold(invitee.label) !== fold(option.label),
	);
	return invitees.length > 0 ? { ...rest, invitees } : rest;
}

/**
 * `speakers.select` outside the app, as the hosts answer it: a pick of a
 * person who already owns another confirmed voice here is refused as
 * `wouldCombine` unless it asks to `combine`, and then folds the voice
 * into the owner's; any other pick confirms the voice as the person. The
 * returned detail is the meeting as the host republishes it. Under
 * `speakers-saving` the pick is never acknowledged, under
 * `speakers-select-failed` it is refused with the host's message.
 */
export function answerSpeakerSelect(
	detail: MeetingDetailSnapshot,
	params: unknown,
	scenario = "",
): MeetingDetailSnapshot | Promise<never> {
	if (scenario === "speakers-saving") {
		return new Promise(() => {});
	}
	if (scenario === "speakers-select-failed") {
		throw new BridgeError("speakers.select", "failed", SELECT_FAILURE);
	}
	const { speakerID, option, combine } = params as {
		speakerID: string;
		option: SpeakerOption;
		combine: boolean;
	};
	const speaker = detail.speakers.find((row) => row.id === speakerID);
	if (!speaker) {
		return detail;
	}
	const fold = (text: string) => text.trim().toLowerCase();
	const owner = detail.speakers.find(
		(row) =>
			row.id !== speakerID &&
			row.assignment === "confirmed" &&
			(option.personID
				? row.personID === option.personID
				: fold(row.displayName) === fold(option.label)),
	);
	if (owner && !combine) {
		throw new BridgeError(
			"speakers.select",
			"wouldCombine",
			`${owner.displayName} already speaks as ${owner.clusterLabel} in this meeting.`,
		);
	}
	if (owner) {
		// The voice nests under the owner's, its own clip and words kept,
		// so Separate again (`answerSpeakerSeparate`) can bring it back.
		const nested = {
			id: speaker.id,
			clusterLabel: speaker.clusterLabel,
			...(speaker.quote ? { quote: speaker.quote } : {}),
			hasClip: speaker.hasClip,
			isPlaying: false,
		};
		return {
			...detail,
			speakers: detail.speakers
				.filter((row) => row.id !== speakerID)
				.map((row) =>
					row.id === owner.id
						? {
								...row,
								combined: [...row.combined, ...speaker.combined, nested],
							}
						: row,
				),
			transcript: detail.transcript.map((turn) =>
				turn.speakerID === speakerID
					? { ...turn, speakerID: owner.id, speakerName: owner.displayName }
					: turn,
			),
		};
	}
	const { suggestionName: _name, ...rest } = speaker;
	const personID = option.personID ?? mockPersonID(option.label);
	const named: DetailSpeaker = {
		...rest,
		assignment: "confirmed",
		displayName: option.label,
		personID,
		// A voice takes its person's colour, as the hosts fold it.
		colorIndex: personColor(personID),
	};
	return withoutInvitee(
		{
			...detail,
			speakers: detail.speakers.map((row) =>
				row.id === speakerID ? named : row,
			),
			transcript: detail.transcript.map((turn) =>
				turn.speakerID === speakerID
					? { ...turn, speakerName: option.label }
					: turn,
			),
		},
		option,
	);
}

/** What a combine took away, for Separate again to put back as it was. */
export interface CombineRecord {
	speaker: DetailSpeaker;
	turnIDs: readonly string[];
}

/**
 * The record `answerSpeakerSeparate` needs to undo a combine of `params`'
 * voice in `before`: the voice as it was and the turns it led. None when
 * the pick does not combine.
 */
export function combineRecord(
	before: MeetingDetailSnapshot,
	params: unknown,
): CombineRecord | undefined {
	const { speakerID } = params as { speakerID: string };
	const speaker = before.speakers.find((row) => row.id === speakerID);
	return speaker
		? {
				speaker,
				turnIDs: before.transcript
					.filter((turn) => turn.speakerID === speakerID)
					.map((turn) => turn.id),
			}
		: undefined;
}

/**
 * `speakers.separate` outside the app (Undo and Separate again): the voice
 * leaves the person's `combined` and is its own row again, as `record`
 * kept it (its turns back), or unnamed with its clip and words when the
 * combine predates the page. Refused as `stale` with the host's words
 * under `speakers-separate-failed`, as `stale` when the voice is not
 * combined.
 */
export function answerSpeakerSeparate(
	detail: MeetingDetailSnapshot,
	params: unknown,
	scenario = "",
	record?: CombineRecord,
): MeetingDetailSnapshot {
	if (scenario === "speakers-separate-failed") {
		throw new BridgeError("speakers.separate", "stale", NOTHING_TO_SEPARATE);
	}
	const { speakerID } = params as { speakerID: string };
	const owner = detail.speakers.find((row) =>
		row.combined.some((voice) => voice.id === speakerID),
	);
	const voice = owner?.combined.find((row) => row.id === speakerID);
	if (!owner || !voice) {
		throw new BridgeError(
			"speakers.separate",
			"stale",
			"This voice is no longer combined.",
		);
	}
	const { quote: _quote, ...restVoice } = voice;
	const restored: DetailSpeaker = record?.speaker ?? {
		...restVoice,
		...(voice.quote ? { quote: voice.quote } : {}),
		displayName: voice.clusterLabel,
		assignment: "unknown",
		colorIndex: personColor(voice.id),
		dismissed: false,
		combined: [],
	};
	const at = detail.speakers.indexOf(owner);
	const speakers = detail.speakers.map((row) =>
		row === owner
			? {
					...row,
					combined: row.combined.filter((entry) => entry.id !== speakerID),
				}
			: row,
	);
	speakers.splice(at + 1, 0, restored);
	const turns = new Set(record?.turnIDs ?? []);
	return {
		...detail,
		speakers,
		transcript: detail.transcript.map((turn) =>
			turns.has(turn.id)
				? {
						...turn,
						speakerID: restored.id,
						speakerName: restored.displayName,
					}
				: turn,
		),
	};
}

/**
 * `transcript.setTurnSpeaker` outside the app: refused as `stale` when the
 * generation or a segment's voice is not what the page sent (and always
 * under `speakers-move-stale`); otherwise the turns of those segments go
 * to the meeting's voice confirmed as the person, or to a new voice
 * ("Speaker N", no clip), and a voice left without a turn goes.
 */
export function answerTurnSpeaker(
	detail: MeetingDetailSnapshot,
	params: unknown,
	scenario = "",
): MeetingDetailSnapshot {
	const { generation, segments, option } = params as {
		generation: number;
		segments: { id: string; speakerID?: string }[];
		option: SpeakerOption;
	};
	const stale = () =>
		new BridgeError("transcript.setTurnSpeaker", "stale", STALE_TRANSCRIPT);
	if (
		scenario === "speakers-move-stale" ||
		generation !== detail.transcriptGeneration
	) {
		throw stale();
	}
	const moved = new Set<string>();
	const left = new Set<string>();
	for (const segment of segments) {
		const turn = detail.transcript.find((entry) =>
			entry.segmentIDs.includes(segment.id),
		);
		if (!turn || turn.speakerID !== segment.speakerID) {
			throw stale();
		}
		moved.add(turn.id);
		if (turn.speakerID) {
			left.add(turn.speakerID);
		}
	}
	const fold = (text: string) => text.trim().toLowerCase();
	const personID = option.personID ?? mockPersonID(option.label);
	const existing = detail.speakers.find(
		(row) =>
			row.assignment === "confirmed" &&
			(row.personID === personID ||
				(!option.personID && fold(row.displayName) === fold(option.label))),
	);
	// The next free label among every voice, combined ones included.
	const labels = detail.speakers
		.flatMap((row) => [row, ...row.combined])
		.map((row) => /^Speaker (\d+)$/.exec(row.clusterLabel)?.[1])
		.map(Number)
		.filter(Number.isFinite);
	const destination: DetailSpeaker = existing ?? {
		id: mockPersonID(`voice ${option.label}`),
		clusterLabel: `Speaker ${Math.max(0, ...labels) + 1}`,
		displayName: option.label,
		assignment: "confirmed",
		personID,
		colorIndex: personColor(personID),
		hasClip: false,
		isPlaying: false,
		dismissed: false,
		combined: [],
	};
	const transcript = detail.transcript.map((turn) =>
		moved.has(turn.id)
			? {
					...turn,
					speakerID: destination.id,
					speakerName: destination.displayName,
				}
			: turn,
	);
	// A voice the move left without a paragraph goes, as the hosts prune it.
	const speakers = [
		...detail.speakers,
		...(existing ? [] : [destination]),
	].filter(
		(row) =>
			!left.has(row.id) ||
			row.combined.length > 0 ||
			transcript.some((turn) => turn.speakerID === row.id),
	);
	return withoutInvitee({ ...detail, speakers, transcript }, option);
}

/**
 * `transcript.playTurn`, `speakers.play` and `speakers.stop` outside the
 * app: the one sound playing is marked, the rest stop. `null` stops all.
 * A turn that `fails` (`speakers-play-failed`) plays nothing and reads as
 * failed, as the hosts publish a cut that failed after they replied; any
 * play or stop clears that.
 */
export function answerPlaying(
	detail: MeetingDetailSnapshot,
	playing: { turn: string; fails?: boolean } | { voice: string } | null,
): MeetingDetailSnapshot {
	const voice = playing && "voice" in playing ? playing.voice : undefined;
	const turn = playing && "turn" in playing ? playing.turn : undefined;
	const fails = playing !== null && "fails" in playing && playing.fails;
	return {
		...detail,
		speakers: detail.speakers.map((row) => ({
			...row,
			isPlaying: row.id === voice,
			combined: row.combined.map((entry) => ({
				...entry,
				isPlaying: entry.id === voice,
			})),
		})),
		transcript: detail.transcript.map((entry) => ({
			...entry,
			isPlaying: !fails && entry.segmentIDs[0] === turn,
			playFailed: fails === true && entry.segmentIDs[0] === turn,
		})),
	};
}

export function createMockTransport(
	options: MockTransportOptions = {},
): MockTransport {
	const hub = new SnapshotHub();
	const calls: RecordedCall[] = [];
	// One load shared by every call, so a command that arrives while the
	// replies are still loading waits for them instead of seeing an empty map.
	const recorded = options.replies;
	let replies: Promise<FixtureMap> | undefined =
		typeof recorded === "function"
			? recorded()
			: recorded
				? Promise.resolve(recorded)
				: undefined;

	const loading: Promise<void> = (async () => {
		const source = options.snapshots;
		if (!source) {
			return;
		}
		const snapshots = typeof source === "function" ? await source() : source;
		for (const [topic, snapshot] of Object.entries(snapshots)) {
			hub.emit(topic, snapshot);
		}
	})();

	return {
		calls,
		emit(topic, snapshot) {
			hub.emit(topic, snapshot);
		},
		ready() {
			return loading;
		},
		async call(method: string, params: unknown): Promise<unknown> {
			calls.push({ method, params: params ?? null });
			replies ??= loadFixtureReplies().catch(() => ({}));
			const reply = (await replies)[method];
			if (typeof reply === "function") {
				return reply(params);
			}
			if (reply instanceof Error) {
				throw new BridgeError(method, "mock", reply.message);
			}
			return reply;
		},
		subscribe: hub.subscribe.bind(hub),
	};
}
