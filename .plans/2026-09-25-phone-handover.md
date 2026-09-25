# Phone handover: Bonjour, pairing, pinned TLS, queued upload, recorder UI

Status: implementation plan, written 2026-09-25, reconciled and then revised the same day after the three reviews
(program review application log). Binding program: [`2026-09-25-v1-program.md`](2026-09-25-v1-program.md). Scope
authority: [`2026-09-24-initial-scope.md`](2026-09-24-initial-scope.md). Owns `Sources/StenoHandover` (Mac), the
`steno dev handover` tool and the feature work inside `mobile/` (iOS). The Expo scaffold in `mobile/` is extended,
never replaced. `PairedDevice`, `HandoverReceipt`, `RecordingMetadata`, `HandoverIntake` and `StenoJSON` are StenoCore
types; `MeetingStore` holds the handover rows.

Facts are tagged `(verified: source)` when read from documentation, the correctness review's verified claims, or the
packages in `mobile/node_modules`, `(unverified)` when a spike must confirm them.

## Goal

The phone records a meeting as an AAC `.m4a`, keeps it in a local queue, and hands it to the paired Mac over the local
network without any server or account. The Mac advertises `_steno._tcp`, shows a QR code once for pairing, terminates
TLS with a self-signed certificate the phone pins, receives the file in resumable chunks with idempotency by recording
id, and enqueues it through `HandoverIntake` so a `Meeting` with source `.phone` in state `.queued` enters the normal
pipeline. The settings screen lists paired phones and can revoke them. Everything except the two device-only spikes is
proven on the hosted CI job over loopback.

## Non-goals

- Any relay, cloud, push service, account, or internet path. Local network only.
- Mac-to-phone data: nothing on the phone beyond "delivered". No playback, trimming, renaming or titles on the phone.
- Certificate rotation or several Macs per phone. Losing the Mac identity means re-pairing every phone.
- Android, iPad layout, widgets, Live Activities, Siri; encryption at rest beyond iOS file protection; a hand-written
  HTTP parser.
- Localised strings. English only, like the macOS app.

## Decisions

1. **Discovery on iOS: a local Expo module `mobile/modules/steno-link` on `NWBrowser`, not `react-native-zeroconf`.**
   zeroconf 0.14.0 (2025-12-30) is an old-architecture `RCTBridgeModule` on the deprecated `NSNetServiceBrowser`, 46
   open issues (verified: npm registry, `ios/RNZeroconf/RNZeroconf.h`), and pinning (decision 2) needs a native module
   anyway. Local modules autolink from `./modules` (verified: `expo-modules-autolinking` 57.0.13 defaults
   `nativeModulesDir` to `./modules`).
2. **Pinning on iOS needs native code and an ATS exception.** `fetch` and expo-file-system 57 `File.upload` /
   `UploadTask` use `URLSession` with system trust and expose no `didReceive challenge` hook (verified: installed
   `ios/*.swift`, no `URLAuthenticationChallenge`). With App Transport Security a delegate may tighten but not loosen
   trust, and since iOS 17 ATS rejects connections to IP addresses by default (verified: Apple "Preventing insecure
   network connections", `NSAllowsLocalNetworking` docs). So `app.config.ts` sets `NSAppTransportSecurity = {
   NSAllowsLocalNetworking: true }`, which re-enables `.local` names and IP addresses and needs no App Review
   justification but does not itself trust the leaf; `steno-link` owns a `URLSession` whose delegate accepts the
   server trust iff SHA-256 of the leaf certificate DER equals the pinned fingerprint. The comparison lives in one
   file, `PinnedTrustEvaluator.swift`, shared with the Swift tests (see Tests).
3. **Fingerprint = SHA-256 of the leaf certificate DER**, not the SPKI: same bytes on both sides via
   `SecCertificateCopyData`, no ASN.1 assembly on iOS, and rotation is a non-goal.
4. **Background uploads via a background `URLSession`** in `steno-link`, identifier `uno.schmid.steno.upload`, one
   task per chunk from a temp file. Server-trust challenges are delivered to a background session's delegate, and each
   one resumes the app and counts against the resume rate limiter (verified: Quinn, developer forums thread 28713),
   hence 16 MiB chunks (decision 8). Completion after relaunch arrives through an `ExpoAppDelegateSubscriber`;
   expo-modules-core 57.0.18 forwards `application(_:handleEventsForBackgroundURLSession:completionHandler:)`
   (verified: `ExpoAppDelegateSubscriberManager.swift:165-187`).
5. **Mac TLS and HTTP on swift-nio over Network.framework.** `NIOTSListenerBootstrap` (`swift-nio-transport-services`)
   with `NWProtocolTLS.Options` whose `securityProtocolOptions` receive
   `sec_protocol_options_set_local_identity(_:_:)` and TLS 1.3 minimum (both APIs macOS 10.14+, verified: Apple docs),
   and `NIOHTTP1`'s `configureHTTPServerPipeline()` for request parsing, `Expect: 100-continue` and header limits. A
   hand-written parser on the one port that accepts bytes from another device was the wrong trade; two packages
   replace three parser files and their edge-case tests. Bonjour advertising uses the listener's `NWListener.Service`
   if NIOTS exposes it, otherwise `DNSServiceRegister` on the bound port (unverified, settled in M2).
6. **Identity minted with `swift-certificates`** (Apple; pulls `swift-crypto`, `swift-asn1`): P-256, self-signed, ten
   years, `CN=Steno on <Mac name>`. The signing initializer
   `Certificate(version:serialNumber:publicKey:notValidBefore:notValidAfter:issuer:subject:
   signatureAlgorithm:extensions:issuerPrivateKey:)` exists (verified: `Sources/X509/Certificate.swift`). Certificate
   and key are stored in the **file-based login keychain** (`SecItemAdd` of `kSecClassCertificate` and `kSecClassKey`,
   fetched as `kSecClassIdentity`), never with `kSecUseDataProtectionKeychain`: the data-protection keychain needs an
   application-identifier entitlement authorised by a provisioning profile, which a Developer ID build has no profile
   for and `swift test` runs unsigned, so every `SecItemAdd` would fail with `errSecMissingEntitlement` (-34018)
   (verified: Quinn, forums thread 130264; field report Round-Tower/M1K3#319). Tests never touch a keychain:
   `Tests/Fixtures/handover/test-identity.p12` (generated once with `openssl`, test-only) is imported with
   `SecPKCS12Import` and `kSecImportToMemoryOnly` (macOS 15+ / iOS 18+, verified: Apple docs). Minting and
   fingerprinting are pure and CI-tested; the keychain round trip is opt-in.
7. **Phone queue = JSON index in Documents**, not expo-sqlite: tens of rows, one writer, atomic temp-and-rename
   through an injected file API; SQLite adds a native dependency for no query we need.
8. **Recording preset**: `.m4a`, AAC, mono, 44.1 kHz, 64 kbps, `directory: "document"` (verified: expo-audio 57.0.5
   `RecordingOptions.directory`). One hour is about 29 MB; chunk size **16 MiB** (one hour is two chunks, one app wake
   each; resume granularity stays acceptable). The whole-file SHA-256 is computed by `steno-link` over the file, not
   in JS.
9. **Pairing secret travels once over the pinned channel** as `Authorization: Pairing <secret>`; no HMAC. Single use,
   five-minute expiry on the injected clock, constant-time compare on the Mac.
10. **Local network privacy (verified: TN3179)**: browsing, resolving and registering Bonjour services need the Local
    Network privilege on iOS and macOS 15; listening and accepting TCP do not. Terminal-launched tools are
    auto-allowed and there is no way to reset the privilege on macOS (FB14944392), so the macOS prompt is checked only
    from the app (macOS plan S4). A backgrounded iOS app in undetermined state is denied silently, so pairing runs in
    the foreground. No simulator support. iOS 18 before 18.6 had a state-sync bug (FB14321888); P7 shows a blocking
    "update iOS" screen when `Platform.Version < 18.6`.
11. **One JSON encoding, core's `StenoJSON`**: camelCase keys, ISO 8601 with fractional seconds, `Data` as standard
    base64 in JSON bodies and headers. Only the QR URL uses base64url, because it is a query string.
    `mobile/modules/steno-link/src/wire.ts` mirrors the Swift names exactly.

## Wire protocol (v1)

Service `_steno._tcp`, instance name = Mac computer name, TXT `v=1`, `id=<macID uuid>`. Port chosen by the system and
published by Bonjour. QR payload and deep link: `steno://pair/v1?mac=<uuid>&name=<pct>&fp=<base64url sha256 of leaf
DER>&secret=<base64url 32 bytes>&exp=<unix>`.

| Method and path | Auth | Body / result |
|---|---|---|
| `GET /v1/hello` | none | `{macID, protocol: 1}`; reachability probe |
| `POST /v1/pair` | `Pairing <secret>` | `PairRequest {deviceID, deviceName}` -> `PairResponse {token, macID, macName}`; 403 on bad or used secret |
| `PUT /v1/recordings/{id}` | Bearer | `RecordingMetadata` -> 201 new or 200 existing with `RecordingStatus` |
| `PUT /v1/recordings/{id}/chunks/{n}` | Bearer | raw bytes, `X-Steno-Chunk-SHA256` (base64) -> 204; duplicates 204 |
| `GET /v1/recordings/{id}` | Bearer | `RecordingStatus {state, receivedChunks}` for resume |
| `POST /v1/recordings/{id}/complete` | Bearer | verifies chunk set and whole-file SHA-256 -> 200 `{meetingID}`; 409 missing chunks; 422 hash mismatch (partial deleted) |
| `DELETE /v1/pairing` | Bearer | phone-side unpair |

`RecordingMetadata` is core's type: `{recordingID, startedAt, durationSeconds, byteCount, sha256, chunkSize, format:
"m4aAAC", deviceName}`. Limits: body = chunk size plus 64 KiB, enforced by a counting handler that answers 413 and
closes; headers bounded by NIOHTTP1's parser; unauthenticated requests are answered 401 and closed before any body is
read. 401 tells the phone the Mac revoked it.

## Public API, Mac side (`StenoHandover`)

```swift
public struct HandoverConfiguration: Sendable {
    public var serviceName: String, advertise: Bool, chunkSize: Int, inboxDirectory: URL, pairingWindow: Duration
    // defaults: Host.current().localizedName, true (false in tests: loopback only), 16 MiB, Application Support/Steno/handover-inbox, .seconds(300)
}
public struct PairingPayload: Codable, Sendable, Equatable {
    public let macID: UUID, macName: String, fingerprint: Data, secret: Data, expiresAt: Date
    public var urlString: String { get }                       // base64url for fp and secret
    public init(parsing url: URL) throws
}
public struct MintedIdentity: Sendable { public let certificateDER: Data; public let privateKey: P256.Signing.PrivateKey; public var fingerprint: Data { get } }
public enum ServerIdentity {                                     // pure swift-certificates, no keychain
    public static func mint(commonName: String, now: Date) throws -> MintedIdentity
    public static func fingerprint(der: Data) -> Data           // SHA-256
}
public enum IdentityKeychain {                                   // file-based login keychain, kSecClassIdentity; opt-in tests
    public static func store(_ identity: MintedIdentity, label: String) throws
    public static func load(label: String) throws -> SecIdentity?
    public static func delete(label: String) throws
    public static func loadOrCreate(label: String, commonName: String) throws -> SecIdentity   // app and CLI entry point
}
public enum HandoverState: Sendable, Equatable { case stopped, listening(port: UInt16), failed(String) }
public actor HandoverService {
    public init(configuration: HandoverConfiguration, store: MeetingStore, intake: any HandoverIntake,
                identity: SecIdentity, clock: any Clock<Duration> = ContinuousClock())
    public var states: AsyncStream<HandoverState> { get }
    public var receipts: AsyncStream<[HandoverReceipt]> { get }    // the UI reads receipts directly; receivedBytes = receivedChunks.count * chunkSize
    public func start() async throws
    public func stop() async
    public func beginPairing() -> PairingPayload                   // replaces any open session
    public func cancelPairing()
    public func pairedDevices() async throws -> [PairedDevice]
    public func revoke(_ deviceID: UUID) async throws
}
// Testing/: TestIdentity.load() -> SecIdentity (SecPKCS12Import + kSecImportToMemoryOnly of Fixtures/handover/test-identity.p12),
//           LoopbackClient (URLSession over PinnedTrustEvaluator; the same file the iOS module compiles)
```

## Public API, phone side (TypeScript)

```ts
// mobile/modules/steno-link/src/wire.ts (mirrors StenoCore names; Data fields are standard base64 strings)
export type AudioFormat = "caf48kFloat32" | "m4aAAC" | "wav16kInt16";
export type RecordingMetadata = { recordingID: string; startedAt: string; durationSeconds: number; byteCount: number; sha256: string; chunkSize: number; format: AudioFormat; deviceName: string };
export type RecordingStatus = { state: "receiving" | "verifying" | "complete" | "failed"; receivedChunks: number[] };
export type PairRequest = { deviceID: string; deviceName: string };
export type PairResponse = { token: string; macID: string; macName: string };

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
    sha256(filePath: string): Promise<string>;              // whole file, base64; native, streaming
}

// mobile/src/features/queue/queue-index.ts (pure, vitest-covered)
export type SyncState = "recording" | "queued" | "uploading" | "delivered" | "failed" | "unpaired";
export type QueuedRecording = { recordingID: string; fileName: string; startedAt: string; durationSeconds: number; byteCount: number; sha256: string | null; chunkSize: number; uploadedChunks: number[]; state: SyncState; attempts: number; nextAttemptAt: string | null; lastError: string | null; meetingID: string | null };
export type QueueIndex = { version: 1; recordings: QueuedRecording[] };
export function addRecording(index: QueueIndex, rec: Pick<QueuedRecording, "recordingID" | "fileName" | "startedAt" | "durationSeconds" | "byteCount" | "sha256" | "chunkSize">): QueueIndex;
export function markChunk(index: QueueIndex, recordingID: string, chunk: number): QueueIndex;
export function setState(index: QueueIndex, recordingID: string, state: SyncState, patch?: Partial<QueuedRecording>): QueueIndex;
export function chunkPlan(byteCount: number, chunkSize: number): { index: number; offset: number; length: number }[];
export function nextUploadable(index: QueueIndex, now: Date): QueuedRecording | null;

// mobile/src/features/queue/queue-storage.ts: injected file API { read, writeAtomic(tmp, rename) } so vitest can make rename throw
// mobile/src/features/pairing/pairing-payload.ts (pure)
export type PairingPayload = { macID: string; macName: string; fingerprint: string; secret: string; expiresAt: number };   // base64url in, decoded to base64 out
export function parsePairingPayload(text: string, now: Date): { ok: true; payload: PairingPayload } | { ok: false; reason: "not-steno" | "version" | "missing-field" | "bad-encoding" | "expired" };

// mobile/src/features/pairing/pairing-store.ts
export type PairedMac = { macID: string; macName: string; fingerprint: string; pairedAt: string };
export const pairingStore: { load(): Promise<{ mac: PairedMac; token: string } | null>; save(mac: PairedMac, token: string): Promise<void>; clear(): Promise<void> };

// mobile/src/features/sync/upload-coordinator.ts
export type MacState = { reachable: boolean; serviceName: string | null };
export type Action = { kind: "idle" } | { kind: "announce"; recordingID: string } | { kind: "upload-chunk"; recordingID: string; chunk: number } | { kind: "complete"; recordingID: string } | { kind: "wait"; until: string };
export function planNext(index: QueueIndex, mac: MacState, inFlight: Set<string>, now: Date): Action;  // pure
export function backoffMs(attempt: number, random: () => number): number;   // 5 s doubling to 5 min, jittered by the injected RNG
export function useUploadCoordinator(): { status: SyncState | "searching"; retryNow(): void };
```

## Files, Mac side

```
Package.swift                                       add StenoHandover, StenoHandoverTests, swift-certificates, swift-nio (NIOHTTP1), swift-nio-transport-services
Sources/StenoHandover/HandoverService.swift         actor façade; owns server, pairing session, receipt stream
Sources/StenoHandover/HandoverConfiguration.swift   configuration and defaults
Sources/StenoHandover/Identity/ServerIdentity.swift mint with swift-certificates (pure)
Sources/StenoHandover/Identity/IdentityKeychain.swift   login keychain store/load/delete, kSecClassIdentity
Sources/StenoHandover/Identity/Fingerprint.swift    SHA-256 of leaf DER, base64url helpers for the QR
Sources/StenoHandover/Pairing/PairingPayload.swift  URL encode/parse, expiry
Sources/StenoHandover/Pairing/PairingSession.swift  single-use secret, window on the injected clock, constant-time compare
Sources/StenoHandover/Pairing/DeviceTokens.swift    token mint, SHA-256 hashing, lookup
Sources/StenoHandover/Network/HandoverServer.swift  NIOTSListenerBootstrap, NWProtocolTLS.Options with the identity, HTTP pipeline, Bonjour advertise
Sources/StenoHandover/Routing/Router.swift          ChannelInboundHandler over HTTPServerRequestPart: auth gate before body, body limit (413), dispatch
Sources/StenoHandover/Routing/PairingHandler.swift, RecordingHandler.swift   hello/pair/unpair; announce/chunk/status/complete
Sources/StenoHandover/Upload/ReceivingFile.swift, Inbox.swift   sparse partial file with offset writes and chunk hash check; partial lifecycle, orphan cleanup
Sources/StenoHandover/Upload/MetadataValidation.swift  RecordingMetadata limits and checks
Sources/StenoHandover/Testing/TestIdentity.swift    SecPKCS12Import + kSecImportToMemoryOnly of the fixture p12
Sources/steno/Commands/DevHandoverCommand.swift     `steno dev handover serve --pair`
Tests/StenoHandoverTests/Support/PinnedTrustEvaluator.swift   symlink -> ../../../mobile/modules/steno-link/ios/PinnedTrustEvaluator.swift
Tests/StenoHandoverTests/Support/LoopbackClient.swift   URLSession client using PinnedTrustEvaluator; store is MeetingStore.inMemory(), intake is core's FakeHandoverIntake
Tests/StenoHandoverTests/                           ServerIdentityTests, PinningTests, RouterLimitsTests, PairingPayloadTests, PairingFlowTests,
                                                    ChunkUploadTests, IdempotencyTests, KeychainIdentityTests (STENO_KEYCHAIN_TESTS=1)
Tests/Fixtures/handover/test-identity.p12           test-only P-256 identity, generated once with openssl, password in the test source
```

## Files, phone side (`mobile/`)

```
modules/steno-link/expo-module.config.json, index.ts, src/StenoLink.types.ts, src/wire.ts   module config, typed wrapper, types above
modules/steno-link/ios/StenoLink.podspec, StenoLinkModule.swift   iOS 16.4 podspec minimum (the 18.6 gate is at runtime); Module definition, functions, events
modules/steno-link/ios/Browser.swift                NWBrowser lifecycle, TXT parsing, resolve via NWConnection
modules/steno-link/ios/PinnedTrustEvaluator.swift   Foundation + Security only: evaluate(trust, pinned) -> Bool by leaf DER SHA-256; the canonical copy
modules/steno-link/ios/PinnedSessionDelegate.swift  URLSession delegate calling PinnedTrustEvaluator
modules/steno-link/ios/UploadSession.swift, BackgroundSessionSubscriber.swift, FileHashing.swift   background URLSession, chunk temp files, relaunch completion, streaming SHA-256
src/features/recording/recorder.ts, recording-options.ts   expo-audio wrapper (audio mode, record, stop, interruption); preset
src/features/queue/queue-index.ts, queue-index.test.ts   pure index operations and vitest
src/features/queue/queue-storage.ts, queue-storage.test.ts, use-queue.ts   Documents/queue/index.json atomic write via injected file API; React state
src/features/discovery/use-mac-discovery.ts         browsing state, filter by paired macID, reachability
src/features/pairing/pairing-payload.ts, pairing-payload.test.ts   parser and vitest
src/features/pairing/pairing-store.ts, pairing-client.ts   secure-store token plus JSON metadata; hello and pair requests
src/features/pairing/PairingSheet.tsx               camera QR scan, progress, errors, unpair
src/features/sync/upload-coordinator.ts, upload-coordinator.test.ts, recording-client.ts   planner, backoff, hook, vitest; announce/status/complete
src/features/recorder/RecorderScreen.tsx, RecordButton.tsx, RecordingList.tsx, SyncStatusBadge.tsx, UpdateIOSScreen.tsx   the one screen plus the version gate
src/navigation/types.ts                             Pairing sheet route
app.config.ts, package.json, README.md              expo-camera (cameraPermission), NSAppTransportSecurity; status paragraph
```

## Steps

Each step is at most one day. M1 to M4 and P1 to P4 run in parallel; P5 needs M3, P6 needs M4, P8 is last. Checks are
`[ci]` (hosted `macos-15` job, loopback only, fresh temp directory per test), `[opt-in: STENO_KEYCHAIN_TESTS]`, or
`[manual]` (a phone or a human). vitest checks run in `mobile-ci.yml`.

- **M1 Target and identity** (after S1). Target, packages, `ServerIdentity.mint`, `Fingerprint`, `IdentityKeychain`,
  `TestIdentity`. Accept `[ci]`: `ServerIdentityTests`: mint yields a P-256 self-signed certificate with the expected
  CN and ten-year validity, fingerprint stable across two computations of one DER and different for two mints;
  `TestIdentity.load()` returns a `SecIdentity` whose certificate fingerprint matches the value recorded in the test.
  `[opt-in: STENO_KEYCHAIN_TESTS]`: `KeychainIdentityTests` store, load and delete under a unique label.
- **M2 TLS listener and HTTP.** `HandoverServer` on loopback with `advertise: false`, `Router`, `/v1/hello`. Accept
  `[ci]`: `PinningTests`: `LoopbackClient` with the right fingerprint gets 200; one flipped fingerprint bit fails the
  handshake and the server logged no request line; a second minted certificate is rejected. `RouterLimitsTests`: a
  body over chunk size plus 64 KiB gets 413 and the connection closes; an unauthenticated PUT is answered 401 before
  its body is read (server records bytes consumed).
- **M3 Pairing.** Session on the injected clock, payload, tokens, `/v1/pair`, `DELETE /v1/pairing`, `revoke`. Accept
  `[ci]`: `PairingFlowTests` on `ManualClock`: first use pairs, second use of the secret is 403, advancing 301 s makes
  it 403, revoked token is 401.
- **M4 Chunked upload.** `ReceivingFile`, `Inbox`, `RecordingHandler`, receipts. Accept `[ci]`: `ChunkUploadTests`
  with `chunkSize: 1 MiB` upload 3 MiB of seeded random bytes with a mid-chunk disconnect, resume from `GET` status, a
  duplicate chunk, then complete; bytes on disk equal the source; hash mismatch is 422 and the partial is gone.
- **M5 Intake and Bonjour.** `HandoverIntake` call on complete, receipt stream, Bonjour service with TXT, orphan
  cleanup on start. Accept `[ci]`: `IdempotencyTests` return the same `meetingID` twice; `receipts` yields
  `.complete`. `[manual]` on a Mac: `dns-sd -B _steno._tcp` lists the service (the prompt check is macOS plan S4;
  Terminal-launched tools are auto-allowed).
- **M6 CLI and end-to-end.** `steno dev handover serve --pair` over `MeetingStore.inMemory()` and core's
  `FakeHandoverIntake`; `Tests/StenoEndToEndTests.testPhoneUploadBecomesQueuedMeeting`: the real `HandoverService` on
  loopback with `TestIdentity`, `LoopbackClient` pairs and uploads a fixture `.m4a`, the real `RecordingIntake`
  enqueues, the meeting is `.queued` with source `.phone`. Accept `[ci]`: the end-to-end test passes. `[manual]`: the
  CLI prints payload URL and QR; a phone dev build pairs against it.
- **P1 Module skeleton and browsing** (after S2). Local module, `Browser`, events, `resolve`. Accept `[manual]`: a
  device dev build lists a service published with `dns-sd -R Test _steno._tcp . 9000 id=<uuid>`; the prompt appears
  exactly once; denial yields `browserState.policyDenied === true`.
- **P2 Pinned request and background upload** (after S3). `PinnedTrustEvaluator`, delegate, `UploadSession`,
  subscriber, `request`, `startUpload`, `pendingUploads`, `sha256`. Accept `[ci]`: `PinningTests` compile the same
  evaluator file (symlink) and pass. `[manual]`: against the M4 listener a 30 MB file uploads with the phone locked; a
  wrong fingerprint sends no body bytes; after force-quit and relaunch `pendingUploads()` reports the tasks iOS kept.
- **P3 Recording** (after S4). `recorder.ts`, options, audio mode `{allowsRecording: true, playsInSilentMode: true,
  interruptionMode: "doNotMix"}`, file moved into `Documents/queue/`, `sha256` via the module after stop. Accept
  `[manual]`: a 60-minute recording with the screen locked yields one `.m4a` with matching duration; an incoming call
  leaves a playable file queued.
- **P4 Queue.** `queue-index.ts`, `queue-storage.ts` with the injected file API, hook. Accept `[ci]` (vitest): add,
  markChunk, chunkPlan (exact boundaries, short last chunk), nextUploadable ordering and backoff gating;
  `queue-storage.test.ts` makes `rename` throw and asserts the previous index is still read back intact.
- **P5 Pairing UI.** `PairingSheet` with `CameraView` (`barcodeTypes: ["qr"]`, `onBarcodeScanned`; verified:
  expo-camera SDK 57 docs), parser, store, client. Accept `[ci]` (vitest): parser (valid, expired, non-steno, missing
  field, bad base64url). `[manual]`: pair against M6 stores token and fingerprint; a second scan replaces the pairing.
- **P6 Upload coordinator.** Planner, backoff, retries on foreground and `serviceFound`, delete the local file after
  `complete` 200, 401 marks all `unpaired`. Accept `[ci]` (vitest): `planNext` (announce before chunks, at most two
  chunks in flight, complete only when all chunks are uploaded, wait while unreachable); `backoffMs` bounds with
  `random` fixed at 0 and 1. `[manual]`: Wi-Fi off for a minute mid-upload, the upload finishes without restarting
  from chunk 0.
- **P7 Screen and version gate.** Recorder screen, list, badges, theme and motion tokens; `UpdateIOSScreen` shown
  instead of the recorder when `Platform.Version < 18.6`. Accept `[ci]`: `pnpm check` passes; a vitest for the version
  comparison. `[manual]`: VoiceOver labels on record and retry.
- **P8 Delivery.** `app.config.ts` (`NSAppTransportSecurity.NSAllowsLocalNetworking`, expo-camera plugin),
  `package.json`, README. Accept `[ci]`: the `mobile-ci.yml` sticky comment says TestFlight lane (fingerprint moves).
  `[manual]`: the build installs and passes the P1 to P7 manual checks.

## Tests

- Swift unit and integration `[ci]`, loopback only (`advertise: false`, nothing leaves 127.0.0.1, `TestIdentity`,
  fresh temp inbox per test): the eight test files listed above; payloads are seeded random bytes, the intake fake
  records calls. The pinning boundary is executed, not described: `PinnedTrustEvaluator.swift` lives in the Expo
  module (EAS uploads `mobile/` alone, so a symlink pointing out of it would dangle) and
  `Tests/StenoHandoverTests/Support/` holds the symlink into it; SwiftPM follows symlinked sources.
- vitest `[ci]` in `mobile/`: `queue-index`, `queue-storage`, `pairing-payload`, `upload-coordinator`, version gate.
  Pure functions and injected APIs, no native imports.
- Opt-in `[opt-in: STENO_KEYCHAIN_TESTS]`: `KeychainIdentityTests`.
- The one manual check `[manual]`: a real iPhone (iOS 18.6+) and a Mac on one Wi-Fi. Pair by QR, record 60 minutes
  locked, stop, pocket the phone, toggle Wi-Fi off and on once during upload. The Mac shows one `.queued` meeting with
  source `.phone`; the phone shows "delivered" and has deleted the file. Revoke on the Mac; the phone shows the
  unpaired state on its next attempt.

Reviewer trap: `RouterLimitsTests` (413 and auth-before-body) still asserted; `advertise: false` in every test; the
symlink target unchanged; any `kSecUseDataProtectionKeychain` in the diff.

## Spikes (go/no-go before the dependent step)

- **S1 In-memory identity for the listener (before M2, `[ci]`).** `TestIdentity.load()` on the hosted runner feeds
  `sec_identity_create` and `NWProtocolTLS.Options`, and `LoopbackClient` completes a handshake. Go: M2 proceeds with
  no keychain in tests. No-go: tests import the p12 into a temporary file keychain from `SecKeychainCreate` under a
  temp path, deleted in teardown.
- **S2 NWBrowser inside the Expo dev client (before P1).** Go: prompt once, results arrive, `resolve` returns an IPv4
  `hostPort` from `NWConnection.currentPath?.remoteEndpoint`, and `URLSession` with `NSAllowsLocalNetworking` connects
  to it. On IPv6-only Wi-Fi, if scoped link-local addresses fail in URLs, fall back to the Mac `.local` hostname
  carried in TXT. No-go on both: the phone speaks HTTP over `NWConnection` inside the module. (unverified)
- **S3 Background URLSession with custom trust (before P2).** Trust challenges reaching the delegate is verified
  (decision 4); still to confirm: a task started in the foreground finishes while suspended, and after relaunch the
  subscriber receives `handleEventsForBackgroundURLSession`. No-go: uploads run in a foreground session extended by
  `beginBackgroundTask`, noted as reduced reliability.
- **S4 Hour-long background recording with expo-audio (before P3).** Go: 60 minutes locked, one file, no truncation; a
  call interruption leaves a valid file and the UI shows stopped. No-go: recording moves into `steno-link` on
  `AVAudioRecorder` behind the same JS API. (unverified)

## Needs from other workstreams

- **core-foundation**: `PairedDevice`, `HandoverReceipt`, `RecordingMetadata` with `format: AudioFormat`, `StenoJSON`,
  `MeetingStore` handover rows, `RecordingIntake` as `HandoverIntake` (file move, `ProcessingPipeline.enqueue` with
  `Meeting(.queued, .phone)` and `AudioAsset(.m4aAAC, [.mixed])`, idempotent on `recordingID`), `FakeHandoverIntake`
  and `ManualClock` in `Testing/`, the `steno` root command and `dev` group, `Tests/StenoEndToEndTests`.
- **audio-capture**: `AVFoundationAudioCodec` decodes AAC `.m4a` (pipeline stage 1); nothing to do here.
- **macos-app-and-release**: render `PairingPayload.urlString` as a QR (`CIQRCodeGenerator`) in a pairing sheet;
  settings pane bound to `HandoverService` (devices, revoke, `states`, `receipts`); onboarding text for the macOS
  local network prompt; `NSLocalNetworkUsageDescription` and `NSBonjourServices: ["_steno._tcp"]` in the app
  Info.plist; menu bar progress from `receipts`; `IdentityKeychain.loadOrCreate` at launch.
- **speech-and-speakers, llm-and-templates, adapters-obsidian**: nothing.

## Risks

S3 (background completion) and S2 (browser in the dev client) have untested fallbacks. Every trust challenge wakes a
locked phone once per chunk; 16 MiB chunks keep that to a handful per meeting. Local network denial is silent on both
devices: the UI points to Settings on `policyDenied`, and the Mac keeps listening through its own first-advertise
prompt. Bonjour advertising through NIOTS is unverified; `DNSServiceRegister` is the fallback and costs one file.

## Deferred

- German and English strings on the phone (`expo-localization`, `t()`, locale files); the recorder ships in English
  like the macOS app.
- Certificate rotation, several Macs per phone, relay or cloud transport.
