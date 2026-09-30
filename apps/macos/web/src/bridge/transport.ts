/**
 * The wire between the page and its host (plan Decisions 5 and 6): commands
 * out through `call`, full snapshots in through `subscribe`. Topic and method
 * names are plain strings here; the typed contract (`contract.ts`, authored
 * separately) wraps this interface.
 */
export interface BridgeTransport {
	/** Sends a command and resolves with the host's reply (or `undefined`). */
	call<TParams, TReply>(method: string, params: TParams): Promise<TReply>;
	/**
	 * Subscribes to a topic. The handler receives the latest snapshot at once
	 * when one is known, then every later snapshot. Returns the unsubscribe.
	 */
	subscribe<T>(topic: string, handler: (snapshot: T) => void): () => void;
}

/** A rejected `call`; `code` is the host's stable error identifier. */
export class BridgeError extends Error {
	readonly code: string;
	readonly method: string;

	constructor(method: string, code: string, message: string) {
		super(message);
		this.name = "BridgeError";
		this.code = code;
		this.method = method;
	}
}

export type SnapshotHandler<T> = (snapshot: T) => void;

/**
 * Keeps the latest snapshot per topic and fans it out to subscribers. Both
 * transports use it so late subscribers get the current state immediately.
 */
export class SnapshotHub {
	private readonly latest = new Map<string, unknown>();
	private readonly handlers = new Map<string, Set<SnapshotHandler<unknown>>>();

	emit(topic: string, snapshot: unknown): void {
		this.latest.set(topic, snapshot);
		const set = this.handlers.get(topic);
		if (!set) {
			return;
		}
		for (const handler of set) {
			handler(snapshot);
		}
	}

	subscribe<T>(topic: string, handler: SnapshotHandler<T>): () => void {
		let set = this.handlers.get(topic);
		if (!set) {
			set = new Set();
			this.handlers.set(topic, set);
		}
		const untyped = handler as SnapshotHandler<unknown>;
		set.add(untyped);
		if (this.latest.has(topic)) {
			handler(this.latest.get(topic) as T);
		}
		return () => {
			set?.delete(untyped);
		};
	}

	has(topic: string): boolean {
		return this.latest.has(topic);
	}

	topics(): string[] {
		return [...this.latest.keys()];
	}
}
