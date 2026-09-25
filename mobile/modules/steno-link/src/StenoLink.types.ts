/**
 * Types of the `StenoLink` native module (iOS, `modules/steno-link/ios`).
 * Bonjour browsing on `NWBrowser`, pinned foreground requests, background
 * chunk uploads and streaming SHA-256. Everything binary crosses the bridge
 * as a standard base64 string.
 */

/** One `_steno._tcp` instance. `macID` comes from the TXT record `id=`. */
export type MacService = { name: string; macID: string | null };

/**
 * Resolved endpoint of a service. `host` is a literal IPv4 address, a
 * bracketed IPv6 literal, or the `.local` hostname when resolution returned a
 * name; it is safe to interpolate into a URL as-is.
 */
export type ResolvedMac = { host: string; port: number };

export type BrowserState = {
	state: "ready" | "waiting" | "failed" | "cancelled";
	/** True when iOS denied the Local Network privilege (TN3179). */
	policyDenied: boolean;
};

/** Foreground request over a URLSession whose delegate pins the leaf certificate. */
export type PinnedRequest = {
	url: string;
	method: "GET" | "POST" | "PUT" | "DELETE";
	headers: Record<string, string>;
	body?: string;
	/** SHA-256 of the Mac's leaf certificate DER, standard base64. */
	fingerprint: string;
	timeoutMs: number;
};

export type PinnedResponse = {
	status: number;
	headers: Record<string, string>;
	body: string;
};

/**
 * One chunk upload in the background session. The module copies
 * `[offset, offset + length)` of `filePath` into a temp file, hashes it and
 * adds the `X-Steno-Chunk-SHA256` header itself.
 */
export type UploadSpec = {
	taskID: string;
	url: string;
	headers: Record<string, string>;
	fingerprint: string;
	filePath: string;
	offset: number;
	length: number;
};

export type UploadProgress = {
	taskID: string;
	bytesSent: number;
	totalBytes: number;
};
export type UploadFinished = { taskID: string; status: number; body: string };
export type UploadFailed = {
	taskID: string;
	/** A rejected pin reads "The Mac's certificate does not match the pairing". */
	message: string;
	/**
	 * False only when the app cancelled the task itself. The coordinator backs
	 * off either way; the flag is informational.
	 */
	retryable: boolean;
};

export type StenoLinkEvents = {
	serviceFound: (service: MacService) => void;
	serviceLost: (service: MacService) => void;
	browserState: (state: BrowserState) => void;
	uploadProgress: (progress: UploadProgress) => void;
	uploadFinished: (result: UploadFinished) => void;
	uploadFailed: (failure: UploadFailed) => void;
};
