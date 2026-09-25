import { type NativeModule, requireNativeModule } from "expo";

import type {
	PinnedRequest,
	PinnedResponse,
	ResolvedMac,
	StenoLinkEvents,
	UploadSpec,
} from "./src/StenoLink.types";

export type * from "./src/StenoLink.types";
export * from "./src/wire";

declare class StenoLinkNativeModule extends NativeModule<StenoLinkEvents> {
	startBrowsing(): void;
	stopBrowsing(): void;
	resolve(serviceName: string): Promise<ResolvedMac>;
	/** Foreground, small JSON bodies only. */
	request(request: PinnedRequest): Promise<PinnedResponse>;
	/** Background session; completion arrives as `uploadFinished` / `uploadFailed`. */
	startUpload(spec: UploadSpec): Promise<void>;
	cancelUpload(taskID: string): Promise<void>;
	/** Task ids still alive in the background session (survives relaunch). */
	pendingUploads(): Promise<string[]>;
	/** Whole-file SHA-256, standard base64, streamed natively. */
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
