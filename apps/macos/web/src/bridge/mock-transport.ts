import type {
	ExportSettingsSnapshot,
	GeneralSettingsSnapshot,
	MeetingDetailSnapshot,
	MeetingRow,
	MeetingsListSnapshot,
	OnboardingSnapshot,
	PhoneSettingsSnapshot,
	RecordingSnapshot,
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
	/** Replies by method. A value that is an `Error` rejects the call. */
	replies?: FixtureMap;
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
		"meeting.deleteRecordingNow",
		"meeting.setKeepAudio",
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
	"denied",
	"failed",
	"processing",
	"export-failed",
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
	// Onboarding
	"onboarding-unknown",
	"onboarding-denied",
	"onboarding-granted",
	"onboarding-setup",
	"onboarding-setup-open",
	"onboarding-codex",
	"onboarding-vault-saved",
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
 * recording, started 12:34 ago, its auto-stop counting down), `denied` (idle
 * with the microphone denied), `failed` (the failed meeting selected),
 * `processing` (a meeting in the progress entry, selected), `export-failed`
 * (the selected meeting's export failed), `damaged-audio`
 * (`meeting.detail.damagedAudio`: parts of the recording replaced by
 * silence). `tab=` picks the detail tab. Without either, the fixtures pass
 * through unchanged.
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

	if (scenario === "recording") {
		const live = snapshots["recording.live"] as RecordingSnapshot | undefined;
		if (live) {
			result.recording = {
				...live,
				startedAt: new Date(Date.now() - RECORDING_ELAPSED_MS).toISOString(),
			} satisfies RecordingSnapshot;
		}
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

	const damagedAudio = snapshots["meeting.detail.damagedAudio"];
	if (scenario === "damaged-audio" && damagedAudio) {
		result["meeting.detail"] = damagedAudio;
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
				canRerunSummary: true,
			});
		}
	}

	if (scenario === "processing" && list && detail) {
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
	}

	const tab = detailTab.safeParse(params.get("tab"));
	const current = result["meeting.detail"] as MeetingDetailSnapshot | undefined;
	if (tab.success && current) {
		result["meeting.detail"] = { ...current, tab: tab.data };
	}

	applySettingsScenario(result, snapshots, scenario);
	applyOnboardingScenario(result, snapshots, scenario);

	// `recording.live`, `meeting.detail.damagedAudio`,
	// `settings.summaries.codex`, `settings.summaries.fileKey`,
	// `settings.iphone.pairing` and `onboarding.setup` are fixtures, not
	// topics; the page never sees them by those names.
	delete result["recording.live"];
	delete result["meeting.detail.damagedAudio"];
	delete result["settings.summaries.codex"];
	delete result["settings.summaries.fileKey"];
	delete result["settings.iphone.pairing"];
	delete result["onboarding.setup"];
	return result;
}

/**
 * The onboarding states: `onboarding-unknown` (a fresh install, nothing
 * asked yet), `onboarding-denied` (the microphone refused),
 * `onboarding-granted` (every permission granted, Continue instead of Later),
 * `onboarding-setup` (page 2 from `onboarding.setup`: Summaries saved, a
 * vault chosen but refused), `onboarding-setup-open` (page 2 with both rows
 * open), `onboarding-codex` (page 2 with ChatGPT chosen and its consent
 * card), `onboarding-vault-saved` (page 2 with the vault saved and the
 * Summaries form open).
 */
function applyOnboardingScenario(
	result: FixtureMap,
	snapshots: FixtureMap,
	scenario: string | null,
) {
	const onboarding = snapshots.onboarding as OnboardingSnapshot | undefined;
	const setup = snapshots["onboarding.setup"] as OnboardingSnapshot | undefined;
	if (!onboarding || !setup) {
		return;
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
 * `settings.iphone.pairing`), `phone-unavailable` (no handover service).
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

export function createMockTransport(
	options: MockTransportOptions = {},
): MockTransport {
	const hub = new SnapshotHub();
	const calls: RecordedCall[] = [];
	// One load shared by every call, so a command that arrives while the
	// replies are still loading waits for them instead of seeing an empty map.
	let replies: Promise<FixtureMap> | undefined = options.replies
		? Promise.resolve(options.replies)
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
			if (reply instanceof Error) {
				throw new BridgeError(method, "mock", reply.message);
			}
			return reply;
		},
		subscribe: hub.subscribe.bind(hub),
	};
}
