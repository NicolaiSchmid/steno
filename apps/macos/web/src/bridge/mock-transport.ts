import { BridgeError, type BridgeTransport, SnapshotHub } from "./transport";

/**
 * Transport for the browser, Playwright and unit tests. Snapshots come from
 * `fixtures/bridge/<topic>.json`, replies from
 * `fixtures/bridge/<method>.reply.json`; both are recorded by the Swift side
 * (`BridgeContractTests`). The `params.*`, `envelope.*`, `reply.*` and
 * `index` files document the wire shapes and are not snapshots. Commands
 * without a recorded reply resolve to `undefined` and are logged in `calls`
 * so tests can assert on them.
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

async function loadGlob(
	modules: Record<string, () => Promise<unknown>>,
	keyFor: (key: string) => string | null,
): Promise<FixtureMap> {
	const entries = await Promise.all(
		Object.entries(modules).map(async ([path, load]) => {
			const key = keyFor(fixtureKey(path));
			if (key === null) {
				return null;
			}
			const mod = (await load()) as { default?: unknown };
			return [key, mod.default ?? mod] as const;
		}),
	);
	return Object.fromEntries(entries.filter((entry) => entry !== null));
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
	return loadGlob(fixtureModules(), (key) => (isSnapshotKey(key) ? key : null));
}

/** The recorded replies by method. */
export function loadFixtureReplies(): Promise<FixtureMap> {
	return loadGlob(fixtureModules(), replyMethod);
}

export function createMockTransport(
	options: MockTransportOptions = {},
): MockTransport {
	const hub = new SnapshotHub();
	const calls: RecordedCall[] = [];
	let replies: FixtureMap = options.replies ?? {};

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
		async call<TParams, TReply>(
			method: string,
			params: TParams,
		): Promise<TReply> {
			calls.push({ method, params });
			if (replies === options.replies && !options.replies) {
				replies = await loadFixtureReplies().catch(() => ({}));
			}
			const reply = replies[method];
			if (reply instanceof Error) {
				throw new BridgeError(method, "mock", reply.message);
			}
			return reply as TReply;
		},
		subscribe: hub.subscribe.bind(hub),
	};
}
