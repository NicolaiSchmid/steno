# Tests

One test target per module (`Tests/<Module>Tests`), `Tests/StenoEndToEndTests`
for the real-pipeline runs, and `Tests/Fixtures/` for shared data (its own
README lists the folders). Everything runs with `swift test`; the protocol
core of `StenoHandover` also runs in the Linux container, the TLS and
Network.framework paths only on the macOS runner.

## Shared sources in `Support/`

Two `Support/` directories hold symbolic links instead of copies, so one
file is compiled by every target that needs it and the copies cannot drift.
SwiftPM follows the links; EAS, which builds `mobile/`, does not, which is
why the links point out of `mobile/` and never into it.

| Link | Canonical file | Why it is shared |
|---|---|---|
| `StenoHandoverTests/Support/PinnedTrustEvaluator.swift` | `mobile/modules/steno-link/ios/PinnedTrustEvaluator.swift` | `PinningTests` and the pinned `LoopbackClient` run the phone's own pin check against the Mac's listener, byte for byte |
| `StenoEndToEndTests/Support/PinnedTrustEvaluator.swift` | the same iOS file | the end-to-end handover test pins the same way |
| `StenoEndToEndTests/Support/LoopbackClient.swift` | `StenoHandoverTests/Support/LoopbackClient.swift` | the phone as the Mac sees it: a `URLSession` client over the evaluator, plain on Linux |
| `StenoEndToEndTests/Support/Phone.swift` | `StenoHandoverTests/Support/Phone.swift` | pair, announce, upload, complete as the coordinator makes the calls |
| `StenoEndToEndTests/Support/TestIdentity.swift` | `StenoHandoverTests/Support/TestIdentity.swift` | the committed test-only TLS identity, imported in memory; it lives in test support so no product module can present it |

Edit the canonical file; the link shows the change everywhere. The evaluator
imports Foundation, Security and CryptoKit only, so it compiles in a test
target without Expo; keep it that way. `Package.swift` excludes the evaluator
links on Linux (`linuxOnlyExclusions`), where Security does not exist.

`StenoHandoverTests/Support/` also holds the helpers only that target uses:
`TestService` (one service on loopback with an advanceable wall clock and
`run { test in … }`), `RawClient` (byte-exact requests for the limit and
timeout tests) and `EngineClient` (the protocol core driven without a
listener, for the ordering tests).
