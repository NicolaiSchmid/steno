import { type NativeModule, requireNativeModule } from "expo";

import type {
	PinnedRequest,
	PinnedResponse,
	ResolvedMac,
	StenoLinkEvents,
	StenoLinkModule,
	UploadSpec,
} from "./src/StenoLink.types";

export type * from "./src/StenoLink.types";
export * from "./src/wire";

declare class StenoLinkNativeModule
	extends NativeModule<StenoLinkEvents>
	implements StenoLinkModule
{
	startBrowsing(): void;
	stopBrowsing(): void;
	resolve(serviceName: string): Promise<ResolvedMac>;
	request(request: PinnedRequest): Promise<PinnedResponse>;
	startUpload(spec: UploadSpec): Promise<void>;
	cancelUpload(taskID: string): Promise<void>;
	pendingUploads(): Promise<string[]>;
	sha256(filePath: string): Promise<string>;
}

let cached: StenoLinkNativeModule | null = null;

/**
 * The native module, resolved on first use so that files importing only the
 * wire types or the pure helpers stay testable on Node.
 */
export function stenoLink(): StenoLinkNativeModule {
	if (!cached) {
		cached = requireNativeModule<StenoLinkNativeModule>("StenoLink");
	}
	return cached;
}
