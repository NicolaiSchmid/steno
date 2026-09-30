import {
	type BridgeMethod,
	type BridgeTopic,
	type MethodParams,
	type MethodReply,
	methodParams,
	methodReplies,
	type TopicSnapshot,
	topicSchemas,
} from "./contract";
import type { BridgeTransport } from "./transport";

/**
 * The typed face of the bridge. Every snapshot is parsed with its topic's
 * schema before a handler sees it and every call's params and reply are
 * parsed with the method's schemas, so a host that drifts from the contract
 * fails loudly in development instead of rendering garbage. Parse failures
 * on snapshots are reported through `onInvalidSnapshot` and dropped.
 */
export interface BridgeClient {
	call<M extends BridgeMethod>(
		method: M,
		...params: MethodParams<M> extends undefined ? [] : [MethodParams<M>]
	): Promise<MethodReply<M>>;
	subscribe<T extends BridgeTopic>(
		topic: T,
		handler: (snapshot: TopicSnapshot<T>) => void,
	): () => void;
}

export interface BridgeClientOptions {
	onInvalidSnapshot?: (topic: BridgeTopic, issues: unknown) => void;
}

export class ContractViolation extends Error {
	readonly issues: unknown;

	constructor(what: string, issues: unknown) {
		super(`${what} does not match the bridge contract`);
		this.name = "ContractViolation";
		this.issues = issues;
	}
}

export function createBridgeClient(
	transport: BridgeTransport,
	options: BridgeClientOptions = {},
): BridgeClient {
	const onInvalidSnapshot =
		options.onInvalidSnapshot ??
		((topic, issues) => {
			console.error(`bridge: dropped invalid ${topic} snapshot`, issues);
		});

	return {
		async call(method, ...rest) {
			const paramsSchema = methodParams[method];
			const params = rest[0];
			if (paramsSchema) {
				const parsed = paramsSchema.safeParse(params);
				if (!parsed.success) {
					throw new ContractViolation(
						`params of ${method}`,
						parsed.error.issues,
					);
				}
			} else if (params !== undefined) {
				throw new ContractViolation(
					`params of ${method}`,
					"method takes no params",
				);
			}
			const reply = await transport.call<unknown, unknown>(
				method,
				params ?? null,
			);
			const replySchema = (
				methodReplies as Partial<
					Record<
						BridgeMethod,
						{
							safeParse: (v: unknown) => {
								success: boolean;
								data?: unknown;
								error?: { issues: unknown };
							};
						}
					>
				>
			)[method];
			if (!replySchema) {
				return undefined as MethodReply<typeof method>;
			}
			const parsed = replySchema.safeParse(reply);
			if (!parsed.success) {
				throw new ContractViolation(`reply of ${method}`, parsed.error?.issues);
			}
			return parsed.data as MethodReply<typeof method>;
		},
		subscribe(topic, handler) {
			const schema = topicSchemas[topic];
			return transport.subscribe<unknown>(topic, (snapshot) => {
				const parsed = schema.safeParse(snapshot);
				if (!parsed.success) {
					onInvalidSnapshot(topic, parsed.error.issues);
					return;
				}
				handler(parsed.data as TopicSnapshot<typeof topic>);
			});
		},
	};
}
