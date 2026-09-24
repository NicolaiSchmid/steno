# Phone handover: Bonjour, pairing, pinned TLS, queued upload, recorder UI

Status: implementation plan, written 2026-09-25. Binding program:
[`2026-09-25-v1-program.md`](2026-09-25-v1-program.md). Scope authority:
[`2026-09-24-initial-scope.md`](2026-09-24-initial-scope.md).
Owns `Sources/StenoHandover` (Mac) and the feature work inside `mobile/`
(iOS). The Expo scaffold in `mobile/` is extended, never replaced.

Facts are tagged `(verified: source)` when read from documentation or the
packages in `mobile/node_modules`, `(unverified)` when a spike must confirm them.

## Goal

The phone records a meeting as an AAC `.m4a`, keeps it in a local queue, and
hands it to the paired Mac over the local network without any server or
account. The Mac advertises `_steno._tcp`, shows a QR code once for pairing,
terminates TLS with a self-signed certificate the phone pins, receives the
file in resumable chunks with idempotency by recording id, and stores it as an
`AudioAsset` with source `.phone` behind a `Meeting` in state `.queued` so the
normal pipeline picks it up. The settings screen lists paired phones and can
revoke them.

## Non-goals

- Any relay, cloud, push service, account, or internet path. Local network only.
- Mac-to-phone data: nothing on the phone beyond "delivered". No playback,
  trimming, renaming or titles on the phone.
- Certificate rotation or several Macs per phone. Losing the Mac identity
  means re-pairing every phone.
- Android, iPad layout, widgets, Live Activities, Siri; encryption at rest
  beyond iOS file protection; third-party HTTP server or client libraries.

## Decisions

1. **Discovery on iOS: a local Expo module `mobile/modules/steno-link` on
   `NWBrowser`, not `react-native-zeroconf`.** zeroconf 0.14.0 (2025-12-30)
   is an old-architecture `RCTBridgeModule` on the deprecated
   `NSNetServiceBrowser`, 46 open issues (verified: npm registry,
   `ios/RNZeroconf/RNZeroconf.h`), and pinning (decision 2) needs a native
   module anyway. Local modules autolink from `./modules` without config
   (verified: `expo-modules-autolinking` 57.0.13).
2. **Pinning on iOS needs native code.** `fetch` and expo-file-system 57
   `File.upload` / `UploadTask` use `URLSession` with system trust and expose
   no `didReceive challenge` hook (verified: installed `ios/*.swift`); ATS
   exceptions do not make a self-signed leaf trusted and TrustKit-style
   libraries validate the chain before pins (both unverified). So
   `steno-link` owns a `URLSession` whose delegate accepts the server trust
   iff SHA-256 of the leaf certificate DER equals the pinned fingerprint.
3. **Fingerprint = SHA-256 of the leaf certificate DER**, not the SPKI: same
   bytes on both sides via `SecCertificateCopyData`, no ASN.1 assembly on iOS,
   and rotation is a non-goal.
4. **Background uploads via a background `URLSession`** in `steno-link`,
   identifier `uno.schmid.steno.upload`, one task per chunk from a temp file.
   Completion after relaunch arrives through an `ExpoAppDelegateSubscriber`;
   expo-modules-core 57.0.18 forwards
   `application(_:handleEventsForBackgroundURLSession:completionHandler:)`
   (verified: `ExpoAppDelegateSubscriberManager.swift`).
5. **Mac TLS and HTTP on Network.framework only.** `NWListener`,
   `NWParameters(tls:tcp:)`, `sec_protocol_options_set_local_identity`,
   TLS 1.3 minimum (verified: Apple docs). A minimal HTTP/1.1 parser
   (`Content-Length` bodies, `Connection: close`, `Expect: 100-continue`)
   lives in the module. No Vapor, no NIO.
6. **Identity minted with `swift-certificates`** (Apple; pulls `swift-crypto`,
   `swift-asn1`): P-256, self-signed, ten years, `CN=Steno on <Mac name>`.
   The signing initializer `Certificate(version:serialNumber:publicKey:
   notValidBefore:notValidAfter:issuer:subject:signatureAlgorithm:extensions:
   issuerPrivateKey:)` exists (verified: `Sources/X509/Certificate.swift`).
   Stored as a `SecIdentity` in the data-protection keychain. The one extra
   package: Security.framework cannot mint certificates.
7. **Phone queue = JSON index in Documents**, not expo-sqlite: tens of rows,
   one writer, atomic temp-and-rename; SQLite adds a native dependency for no
   query we need.
8. **Recording preset**: `.m4a`, AAC, mono, 44.1 kHz, 64 kbps, `directory:
   "document"` (verified: expo-audio 57.0.5 `RecordingOptions`). One hour is
   about 29 MB; chunk size 4 MiB.
9. **Pairing secret travels once over the pinned channel** as
   `Authorization: Pairing <secret>`; no HMAC. Single use, five-minute
   expiry, constant-time compare on the Mac.
10. **Strings**: `expo-localization` for the device language plus a typed
    `t()` over `de.ts` / `en.ts`. No i18n library.
11. **Local network privacy (verified: TN3179)**: browsing, resolving and
    registering Bonjour services need the Local Network privilege on iOS and
    macOS 15; listening and accepting TCP do not. A backgrounded iOS app in
    undetermined state is denied silently, so pairing runs in the foreground.
    The Mac app prompts on first advertise. No simulator support; iOS 18
    before 18.6 had a state-sync bug, so require 18.6 or later.

## Wire protocol (v1)

Service `_steno._tcp`, instance name = Mac computer name, TXT `v=1`,
`id=<macID uuid>`. Port chosen by the system and published by Bonjour.
QR payload and deep link: `steno://pair/v1?mac=<uuid>&name=<pct>`
`&fp=<base64url sha256 of leaf DER>&secret=<base64url 32 bytes>&exp=<unix>`.

| Method and path | Auth | Body / result |
|---|---|---|
| `GET /v1/hello` | none | `{macID, protocol: 1}`; reachability probe |
| `POST /v1/pair` | `Pairing <secret>` | `{deviceID, deviceName}` -> `{token, macID, macName}`; 403 on bad or used secret |
| `PUT /v1/recordings/{id}` | Bearer | `RecordingMetadata` -> 201 new or 200 existing with `{state, receivedChunks}` |
| `PUT /v1/recordings/{id}/chunks/{n}` | Bearer | raw bytes, `X-Steno-Chunk-SHA256` -> 204; duplicates 204 |
| `GET /v1/recordings/{id}` | Bearer | `{state, receivedChunks}` for resume |
| `POST /v1/recordings/{id}/complete` | Bearer | verifies chunk set and whole-file SHA-256 -> 200 `{meetingID}`; 409 missing chunks; 422 hash mismatch (partial deleted) |
| `DELETE /v1/pairing` | Bearer | phone-side unpair |

`RecordingMetadata`: `{recordingID, startedAt, durationSeconds, byteCount,
sha256, chunkSize, format: "m4a", deviceName}`. Limits: body = chunk size plus
64 KiB, headers 16 KiB; unauthenticated requests are answered before any body
is read. 401 tells the phone the Mac revoked it.

## Public API, Mac side (`StenoHandover`)

```swift
public struct HandoverConfiguration: Sendable {
    public var serviceName: String, advertise: Bool, chunkSize: Int, inboxDirectory: URL, pairingWindow: TimeInterval
    // defaults: Host.current().localizedName, true (false in tests: loopback only), 4 MiB, Application Support/Steno/handover-inbox, 300
}
public struct PairedDevice: Codable, Sendable, Equatable, Identifiable {
    public let id: UUID; public var name: String; public let pairedAt: Date; public var lastSeenAt: Date?
}
public struct PairingPayload: Codable, Sendable, Equatable {
    public let macID: UUID, macName: String, fingerprint: Data, secret: Data, expiresAt: Date
    public var urlString: String { get }
    public init(parsing url: URL) throws
}
public struct HandoverTransfer: Sendable, Identifiable, Equatable {
    public let id: UUID                   // recordingID
    public let deviceID: UUID, byteCount: Int64, receivedBytes: Int64, state: State
    public enum State: Sendable, Equatable { case receiving, verifying, complete(meetingID: UUID), failed(String) }
}
public struct RecordingMetadata: Codable, Sendable, Equatable { /* fields as in the wire protocol */ }
public struct HandoverReceipt: Codable, Sendable, Equatable {
    public let recordingID: UUID, deviceID: UUID; public var meetingID: UUID?
    public var state: HandoverTransfer.State, byteCount: Int64, sha256: Data, chunkSize: Int, receivedChunks: [Int]
    public let createdAt: Date; public var updatedAt: Date
}
public struct ServerIdentity: Sendable {
    public let fingerprint: Data          // SHA-256 of leaf certificate DER
    public static func loadOrCreate(label: String) throws -> ServerIdentity
    public static func delete(label: String) throws
}
@MainActor @Observable public final class HandoverService {
    public init(configuration: HandoverConfiguration, store: any HandoverStore, intake: any HandoverIntake, identity: ServerIdentity)
    public private(set) var listeningPort: UInt16?, pairedDevices: [PairedDevice], transfers: [HandoverTransfer], lastError: String?
    public func start() async throws
    public func stop() async
    public func beginPairing() -> PairingPayload   // replaces any open session
    public func cancelPairing()
    public func revoke(_ deviceID: UUID) async throws
}
public protocol HandoverStore: Sendable {          // GRDB implementation in StenoCore
    func pairedDevices() async throws -> [PairedDevice]
    func insert(_ device: PairedDevice, tokenHash: Data) async throws
    func device(forTokenHash: Data) async throws -> PairedDevice?
    func delete(deviceID: UUID) async throws
    func receipt(_ recordingID: UUID) async throws -> HandoverReceipt?
    func upsert(_ receipt: HandoverReceipt) async throws
}
public protocol HandoverIntake: Sendable {         // implementation in StenoCore
    func admit(file: URL, metadata: RecordingMetadata, device: PairedDevice) async throws -> UUID   // Meeting.id
}
```

## Public API, phone side (TypeScript)

```ts
// mobile/modules/steno-link/src/StenoLink.types.ts
export type MacService = { name: string; macID: string | null };
export type ResolvedMac = { host: string; port: number };
export type PinnedRequest = { url: string; method: "GET" | "POST" | "PUT" | "DELETE"; headers: Record<string, string>; body?: string; fingerprint: string; timeoutMs: number };
export type PinnedResponse = { status: number; headers: Record<string, string>; body: string };
export type UploadSpec = { taskID: string; url: string; headers: Record<string, string>; fingerprint: string; filePath: string; offset: number; length: number };
export type StenoLinkEvents = {
    serviceFound: (s: MacService) => void; serviceLost: (s: MacService) => void;
    browserState: (s: { state: "ready" | "waiting" | "failed" | "cancelled"; policyDenied: boolean }) => void;
    uploadProgress: (p: { taskID: string; bytesSent: number; totalBytes: number }) => void;
    uploadFinished: (r: { taskID: string; status: number; body: string }) => void;
    uploadFailed: (e: { taskID: string; message: string; retryable: boolean }) => void;
};
export interface StenoLinkModule {
    startBrowsing(): void; stopBrowsing(): void;
    resolve(serviceName: string): Promise<ResolvedMac>;
    request(req: PinnedRequest): Promise<PinnedResponse>;  // foreground, small JSON
    startUpload(spec: UploadSpec): Promise<void>;           // background session
    cancelUpload(taskID: string): Promise<void>;
    pendingUploads(): Promise<string[]>;                    // task ids alive in the background session
}

// mobile/src/features/queue/queue-index.ts (pure, vitest-covered)
export type SyncState = "recording" | "queued" | "uploading" | "delivered" | "failed" | "unpaired";
export type QueuedRecording = { id: string; fileName: string; startedAt: string; durationSeconds: number; byteCount: number; sha256: string | null; chunkSize: number; uploadedChunks: number[]; state: SyncState; attempts: number; nextAttemptAt: string | null; lastError: string | null; meetingID: string | null };
export type QueueIndex = { version: 1; recordings: QueuedRecording[] };
export function addRecording(index: QueueIndex, rec: Pick<QueuedRecording, "id" | "fileName" | "startedAt" | "durationSeconds" | "byteCount" | "sha256" | "chunkSize">): QueueIndex;
export function markChunk(index: QueueIndex, id: string, chunk: number): QueueIndex;
export function setState(index: QueueIndex, id: string, state: SyncState, patch?: Partial<QueuedRecording>): QueueIndex;
export function chunkPlan(byteCount: number, chunkSize: number): { index: number; offset: number; length: number }[];
export function nextUploadable(index: QueueIndex, now: Date): QueuedRecording | null;

// mobile/src/features/pairing/pairing-payload.ts (pure)
export type PairingPayload = { macID: string; macName: string; fingerprint: string; secret: string; expiresAt: number };
export function parsePairingPayload(text: string, now: Date): { ok: true; payload: PairingPayload } | { ok: false; reason: "not-steno" | "version" | "missing-field" | "bad-encoding" | "expired" };

// mobile/src/features/pairing/pairing-store.ts
export type PairedMac = { macID: string; macName: string; fingerprint: string; pairedAt: string };
export const pairingStore: { load(): Promise<{ mac: PairedMac; token: string } | null>; save(mac: PairedMac, token: string): Promise<void>; clear(): Promise<void> };

// mobile/src/features/sync/upload-coordinator.ts
export type MacState = { reachable: boolean; serviceName: string | null };
export type Action = { kind: "idle" } | { kind: "announce"; id: string } | { kind: "upload-chunk"; id: string; chunk: number } | { kind: "complete"; id: string } | { kind: "wait"; until: string };
export function planNext(index: QueueIndex, mac: MacState, inFlight: Set<string>, now: Date): Action;  // pure
export function backoffMs(attempt: number): number;   // 5 s doubling to 5 min, jittered
export function useUploadCoordinator(): { status: SyncState | "searching"; retryNow(): void };
```

## Files, Mac side

```
Package.swift                                       add StenoHandover, StenoHandoverTests, swift-certificates
Sources/StenoHandover/HandoverService.swift         façade; owns listener, pairing session, transfer state
Sources/StenoHandover/HandoverConfiguration.swift   configuration and defaults
Sources/StenoHandover/Identity/ServerIdentity.swift mint with swift-certificates, keychain load/store
Sources/StenoHandover/Identity/Fingerprint.swift    SHA-256 of leaf DER, base64url helpers
Sources/StenoHandover/Pairing/PairingPayload.swift  URL encode/parse, expiry
Sources/StenoHandover/Pairing/PairingSession.swift  single-use secret, window, constant-time compare
Sources/StenoHandover/Pairing/DeviceTokens.swift    token mint, SHA-256 hashing, lookup
Sources/StenoHandover/Network/HandoverListener.swift  NWListener, TLS options, Bonjour service and TXT
Sources/StenoHandover/Network/HTTPConnection.swift, HTTPRequest.swift, HTTPResponse.swift   read loop and body sink; request line, headers, limits; status and JSON body
Sources/StenoHandover/Routing/Router.swift, PairingHandler.swift, RecordingHandler.swift   auth gate and dispatch; hello/pair/unpair; announce/chunk/status/complete
Sources/StenoHandover/Upload/ReceivingFile.swift, Inbox.swift   sparse partial file with offset writes and chunk hash check; partial lifecycle, orphan cleanup
Sources/StenoHandover/Upload/RecordingMetadata.swift  metadata and validation; HandoverReceipt
Sources/steno/Commands/HandoverCommand.swift        `steno handover serve --pair` (CLI target owned by core; coordinated)
Tests/StenoHandoverTests/Support/                   LoopbackClient.swift (pinning URLSession client, reference for iOS), InMemoryStore.swift
Tests/StenoHandoverTests/                           HTTPRequestTests, PairingPayloadTests, PairingFlowTests, ChunkUploadTests,
                                                    IdempotencyTests, ServerIdentityTests (keychain part behind STENO_KEYCHAIN_TESTS=1)
```

## Files, phone side (`mobile/`)

```
modules/steno-link/expo-module.config.json, index.ts, src/StenoLink.types.ts   module config, typed wrapper, types above
modules/steno-link/ios/StenoLink.podspec, StenoLinkModule.swift   iOS 16.4 minimum; Module definition, functions, events
modules/steno-link/ios/Browser.swift                NWBrowser lifecycle, TXT parsing, resolve via NWConnection
modules/steno-link/ios/PinnedSessionDelegate.swift  server-trust challenge, leaf DER SHA-256 compare
modules/steno-link/ios/UploadSession.swift, BackgroundSessionSubscriber.swift   background URLSession, chunk temp files, relaunch completion
src/features/recording/recorder.ts, recording-options.ts   expo-audio wrapper (audio mode, record, stop, interruption); preset
src/features/queue/queue-index.ts, queue-index.test.ts   pure index operations and vitest
src/features/queue/queue-storage.ts, use-queue.ts   Documents/queue/index.json atomic write, sha256 via expo-crypto; React state
src/features/discovery/use-mac-discovery.ts         browsing state, filter by paired macID, reachability
src/features/pairing/pairing-payload.ts, pairing-payload.test.ts   parser and vitest
src/features/pairing/pairing-store.ts, pairing-client.ts   secure-store token plus JSON metadata; hello and pair requests
src/features/pairing/PairingSheet.tsx               camera QR scan, progress, errors, unpair
src/features/sync/upload-coordinator.ts, upload-coordinator.test.ts, recording-client.ts   planner, backoff, hook, vitest; announce/status/complete
src/features/recorder/RecorderScreen.tsx, RecordButton.tsx, RecordingList.tsx, SyncStatusBadge.tsx   the one screen
src/lib/i18n.ts; src/locales/de.ts, en.ts; src/navigation/types.ts   t(), all strings, Pairing sheet route
app.config.ts, package.json, README.md              expo-camera (cameraPermission de/en), expo-crypto, expo-localization; status paragraph
```

## Steps

Each step is at most one day. M1 to M4 and P1 to P4 run in parallel; P5
needs M3, P6 needs M4, P8 is last.

- **M1 Target and identity** (after S1). Target, swift-certificates, mint,
  store, reload, fingerprint. Accept: `swift test --filter ServerIdentity`
  passes with `STENO_KEYCHAIN_TESTS=1`; fingerprint identical across runs.
- **M2 TLS listener and HTTP.** Loopback listener with `advertise: false`,
  `HTTPConnection`, `HTTPRequest`, `HTTPResponse`, `/v1/hello`. Accept:
  `LoopbackClient` with the right fingerprint gets 200; a wrong fingerprint
  fails the handshake; oversized headers get 431.
- **M3 Pairing.** Session, payload, tokens, `/v1/pair`, `DELETE /v1/pairing`,
  `revoke`. Accept: `PairingFlowTests`: first use pairs, second use of the
  secret is 403, expired is 403, revoked token is 401.
- **M4 Chunked upload.** `ReceivingFile`, `Inbox`, `RecordingHandler`,
  receipts. Accept: `ChunkUploadTests` upload 12 MiB of random bytes in 4 MiB
  chunks with a mid-chunk disconnect, resume from `GET` status, a duplicate
  chunk, then complete; bytes on disk equal the source; hash mismatch is 422.
- **M5 Intake and Bonjour.** `HandoverIntake` call on complete, transfer
  state, Bonjour service with TXT, orphan cleanup on start. Accept:
  `IdempotencyTests` return the same `meetingID` twice; manual on a Mac:
  `dns-sd -B _steno._tcp` lists the service and the macOS prompt shows once.
- **M6 CLI command.** `steno handover serve --pair` with an in-memory store.
  Accept: prints payload URL and QR; a phone dev build pairs against it.
- **P1 Module skeleton and browsing** (after S2). Local module, `Browser`,
  events, `resolve`. Accept: a device dev build lists a service published
  with `dns-sd -R Test _steno._tcp . 9000 id=<uuid>`; the prompt appears
  exactly once; denial yields `browserState.policyDenied === true`.
- **P2 Pinned request and background upload** (after S3). Delegate,
  `UploadSession`, subscriber, `request`, `startUpload`, `pendingUploads`.
  Accept: against the M4 listener a 30 MB file uploads with the phone
  locked; a wrong fingerprint sends no body bytes; after force-quit and
  relaunch `pendingUploads()` reports the tasks iOS kept.
- **P3 Recording** (after S4). `recorder.ts`, options, audio mode
  `{allowsRecording: true, playsInSilentMode: true, interruptionMode:
  "doNotMix"}`, file moved into `Documents/queue/`, sha256 after stop.
  Accept: a 60-minute recording with the screen locked yields one `.m4a`
  with matching duration; an incoming call leaves a playable file queued.
- **P4 Queue.** `queue-index.ts`, storage, hook. Accept: vitest for add,
  markChunk, chunkPlan (exact boundaries, short last chunk), nextUploadable
  ordering and backoff gating; killing the app during a write leaves the
  previous valid index.
- **P5 Pairing UI.** `PairingSheet` with `CameraView` (`barcodeTypes:
  ["qr"]`, `onBarcodeScanned`; verified: expo-camera SDK 57 docs), parser,
  store, client. Accept: vitest for the parser (valid, expired, non-steno,
  missing field, bad base64url); manual pair against M6 stores token and
  fingerprint; a second scan replaces the pairing.
- **P6 Upload coordinator.** Planner, backoff, retries on foreground and
  `serviceFound`, delete the local file after `complete` 200, 401 marks all
  `unpaired`. Accept: vitest for `planNext` (announce before chunks, at most
  two chunks in flight, complete only when all chunks are uploaded, wait while
  unreachable) and `backoffMs` bounds; manual: Wi-Fi off for a minute
  mid-upload, the upload finishes without restarting from chunk 0.
- **P7 Screen and strings.** Recorder screen, list, badges, i18n, motion
  tokens. Accept: `pnpm check` passes; de and en screenshots show no raw
  key; VoiceOver labels on record and retry.
- **P8 Delivery.** `app.config.ts`, `package.json`, README. Accept: the
  `mobile-ci.yml` sticky comment says TestFlight lane (fingerprint moves);
  the build installs and passes the P1 to P7 manual checks.

## Tests

- Swift unit, loopback only (`advertise: false`, nothing leaves 127.0.0.1):
  the six test files listed above; payloads are random bytes, the intake fake
  records calls. Runs in the `macos-15` job of `swift-ci.yml`.
- vitest in `mobile/`: `queue-index`, `pairing-payload`, `upload-coordinator`
  tests. Pure functions, no native imports.
- Integration: `LoopbackClient` in Swift is the executable specification of
  the iOS pinning delegate.
- The one manual check: a real iPhone (iOS 18.6+) and a Mac on one Wi-Fi.
  Pair by QR, record 60 minutes locked, stop, pocket the phone, toggle Wi-Fi
  off and on once during upload. The Mac shows one `.queued` meeting with
  source `.phone`; the phone shows "delivered" and has deleted the file.
  Revoke on the Mac; the phone shows the unpaired state on its next attempt.

## Spikes (go/no-go before the dependent step)

- **S1 Mac identity in the keychain (before M1).** Mint with
  swift-certificates, `SecItemAdd` certificate and key with
  `kSecUseDataProtectionKeychain`, fetch as `kSecClassIdentity`, pass to
  `sec_identity_create`. Go: reload on second launch without a keychain
  prompt on a Developer ID signed build. No-go: keep key and DER as a
  generic-password item and rebuild the identity in memory each launch; if
  `sec_identity_create` still needs a keychain-backed key, escalate.
  (unverified)
- **S2 NWBrowser inside the Expo dev client (before P1).** Go: prompt once,
  results arrive, `resolve` returns an IPv4 `hostPort` from
  `NWConnection.currentPath?.remoteEndpoint`, and `URLSession` connects to
  it. On IPv6-only Wi-Fi, if scoped link-local addresses fail in URLs, fall
  back to the Mac `.local` hostname carried in TXT. No-go on both: the phone
  speaks HTTP over `NWConnection` inside the module. (unverified)
- **S3 Background URLSession with custom trust (before P2).** Go: the
  server-trust challenge reaches the delegate for background upload tasks; a
  task started in the foreground finishes while suspended; after relaunch the
  subscriber receives `handleEventsForBackgroundURLSession`. No-go: uploads
  run in a foreground session extended by `beginBackgroundTask`, noted as
  reduced reliability. (delivery forwarding verified, the rest unverified)
- **S4 Hour-long background recording with expo-audio (before P3).** Go: 60
  minutes locked, one file, no truncation; a call interruption leaves a valid
  file and the UI shows stopped. No-go: recording moves into `steno-link` on
  `AVAudioRecorder` behind the same JS API. (unverified)

## Needs from other workstreams

- **core-foundation**: `Meeting`, `AudioAsset`, `Meeting.Source.phone`,
  `Meeting.State.queued`; `HandoverStore` over GRDB with tables
  `pairedDevice` and `handoverReceipt` in `Storage/Migrations.swift`;
  `HandoverIntake` moving the file into the audio directory and inserting
  `AudioAsset` (`m4a`, lanes `[.mixed]`, retention from settings) plus
  `Meeting` (`.queued`, title from `startedAt`) in one transaction, then
  enqueuing in `ProcessingPipeline`; decode accepting AAC `.m4a`;
  `Sources/steno` registering `HandoverCommand`.
- **macos-app-and-release**: render `PairingPayload.urlString` as a QR
  (`CIQRCodeGenerator`) in a pairing sheet; settings pane bound to
  `HandoverService` (devices, revoke, port, transfers); onboarding text for
  the macOS local network prompt; `NSLocalNetworkUsageDescription` and
  `NSBonjourServices: ["_steno._tcp"]` in the app Info.plist; menu bar
  progress from `transfers`.
- **audio-capture, speech-and-speakers, llm-and-templates,
  adapters-obsidian**: nothing.

## Risks

S3 (background trust challenge) and S1 (keychain identity) have untested
fallbacks. Local network denial is silent on both devices: the UI points to
Settings on `policyDenied`, and the Mac keeps listening through its own
first-advertise prompt. The hand-written HTTP parser is a LAN attack surface:
strict limits, auth before body, `Connection: close`, edge-case tests.

## Requested changes to the program document

1. Add `PairedDevice` and `HandoverReceipt` to the canonical model list,
   owned by StenoCore, persisted in the append-only migrations file, used by
   StenoHandover through `HandoverStore`.
2. Add `swift-certificates` (with transitive `swift-crypto` and `swift-asn1`)
   to the third-party package list, scoped to StenoHandover, because
   Security.framework cannot mint a self-signed certificate.
3. State in the pipeline section that step 1 accepts AAC `.m4a` input.
4. Add `HandoverStore` and `HandoverIntake` to the protocol list as
   StenoCore-owned protocols implemented in StenoCore and consumed by
   StenoHandover, keeping the dependency direction one way.
