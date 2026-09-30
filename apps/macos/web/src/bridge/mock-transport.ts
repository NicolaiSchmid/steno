import type {
	MeetingDetailSnapshot,
	MeetingRow,
	MeetingsListSnapshot,
	RecordingSnapshot,
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
 * so the native confirm and folder panels resolve in the browser (always
 * confirmed; the recorded path).
 */
const REPLY_ALIASES: Record<string, readonly string[]> = {
	"reply.confirm": ["ui.confirmDestructive"],
	"reply.chosenPath": [
		"settings.recording.chooseFolder",
		"settings.export.chooseVault",
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
	"failed",
	"processing",
] as const;
export type Scenario = (typeof scenarios)[number];

export function isScenario(value: string | null): value is Scenario {
	return scenarios.some((scenario) => scenario === value);
}

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
 * recording, started 12:34 ago), `failed` (the failed meeting selected),
 * `processing` (a meeting in the progress entry, selected). `tab=` picks the
 * detail tab. Without either, the fixtures pass through unchanged.
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

	// `recording.live` is a fixture, not a topic; the page never sees it.
	delete result["recording.live"];
	return result;
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
