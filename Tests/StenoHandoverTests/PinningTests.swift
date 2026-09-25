#if canImport(Security)
  import Foundation
  import StenoCore
  import Testing

  @testable import StenoHandover

  /// The trust boundary, executed with the iOS module's own
  /// `PinnedTrustEvaluator.swift` (symlinked into `Support/`): only the exact
  /// leaf fingerprint from the pairing completes a handshake.
  @Suite struct PinningTests {
    @Test func theRightFingerprintCompletesTheHandshake() async throws {
      let test = try await TestService.start()
      defer { Task { await test.stop() } }
      let client = try await test.client()

      let response = try await client.request("GET", "/v1/hello")
      #expect(response.status == 200)
      #expect(test.metrics.requestHeads == 1)
    }

    @Test func oneFlippedFingerprintBitFailsTheHandshakeBeforeAnyRequest() async throws {
      let test = try await TestService.start()
      defer { Task { await test.stop() } }
      var flipped = test.service.identity.fingerprint
      flipped[0] ^= 0x01
      let client = try await test.client(fingerprint: flipped)

      await #expect(throws: (any Error).self) {
        _ = try await client.request("GET", "/v1/hello")
      }
      #expect(test.metrics.requestHeads == 0, "the server never saw a request line")
      #expect(test.metrics.handledRequests == 0)
    }

    @Test func anotherMintedCertificateIsRejected() async throws {
      let test = try await TestService.start()
      defer { Task { await test.stop() } }
      let other = try ServerIdentity.mint(commonName: "Steno on Another Mac")
      let client = try await test.client(fingerprint: other.fingerprint)

      await #expect(throws: (any Error).self) {
        _ = try await client.request("GET", "/v1/hello")
      }
      #expect(test.metrics.requestHeads == 0)
    }

    @Test func theRawClientPinsTheSameWay() async throws {
      let test = try await TestService.start()
      defer { Task { await test.stop() } }
      let raw = try await test.rawClient()

      let exchange = try await raw.exchange(.GET, "/v1/hello")
      #expect(exchange.status == 200)

      // A rejected pin leaves the NWConnection waiting, so the connect
      // times out instead of failing fast; either way no request is made.
      var flipped = test.service.identity.fingerprint
      flipped[31] ^= 0x80
      let wrong = RawClient(port: raw.port, fingerprint: flipped)
      await #expect(throws: (any Error).self) {
        _ = try await wrong.exchange(.GET, "/v1/hello", timeout: .seconds(3))
      }
      #expect(test.metrics.requestHeads == 1)
    }
  }
#endif
