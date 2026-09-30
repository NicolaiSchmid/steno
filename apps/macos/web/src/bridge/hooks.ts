import {
	createContext,
	createElement,
	type ReactNode,
	useContext,
	useEffect,
	useState,
} from "react";
import { type BridgeClient, createBridgeClient } from "./client";
import type {
	BridgeMethod,
	BridgeTopic,
	MethodParams,
	TopicSnapshot,
} from "./contract";
import { bridge } from "./index";

/**
 * React's face of the bridge. One `BridgeClient` per page, created lazily
 * over the shared transport; tests wrap a tree in `BridgeProvider` with a
 * client over the mock transport instead.
 */

let sharedClient: BridgeClient | undefined;

/** The page's one client; created on first use. */
export function sharedBridgeClient(): BridgeClient {
	sharedClient ??= createBridgeClient(bridge());
	return sharedClient;
}

const BridgeContext = createContext<BridgeClient | null>(null);

export interface BridgeProviderProps {
	client: BridgeClient;
	children?: ReactNode;
}

/** Supplies a client to `useBridge` and `useSnapshot` below it. */
export function BridgeProvider({ client, children }: BridgeProviderProps) {
	return createElement(BridgeContext.Provider, { value: client }, children);
}

/** The client for `call`; the provided one, else the page's shared one. */
export function useBridge(): BridgeClient {
	return useContext(BridgeContext) ?? sharedBridgeClient();
}

/**
 * The latest parsed snapshot of `topic`, or `undefined` until the host has
 * sent one. Subscribes on mount and unsubscribes on unmount.
 */
export function useSnapshot<T extends BridgeTopic>(
	topic: T,
): TopicSnapshot<T> | undefined {
	const client = useBridge();
	const [snapshot, setSnapshot] = useState<TopicSnapshot<T> | undefined>(
		undefined,
	);
	useEffect(() => {
		return client.subscribe(topic, (next) => {
			setSnapshot(() => next);
		});
	}, [client, topic]);
	return snapshot;
}

/**
 * Fires a command without awaiting it. A rejected call is a host or
 * contract problem, reported to the console; the page keeps rendering the
 * snapshots it has.
 */
export function send<M extends BridgeMethod>(
	client: BridgeClient,
	method: M,
	...params: MethodParams<M> extends undefined ? [] : [MethodParams<M>]
): void {
	client.call(method, ...params).catch((cause: unknown) => {
		console.error(`bridge: ${method} failed`, cause);
	});
}
