#if canImport(Security)
  import Foundation
  import Security
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

    @Test func thePhonesEvaluatorComputesTheFingerprintTheQRCarries() async throws {
      // Decision 3: SHA-256 of the leaf DER on both sides. The phone hashes
      // `SecCertificateCopyData` of the presented leaf; the Mac hashes the
      // DER it minted or imported and puts it in the QR as `fp`.
      let test = try await TestService.start()
      defer { Task { await test.stop() } }
      let identity = test.service.identity
      let certificate = try #require(
        SecCertificateCreateWithData(nil, identity.certificateDER as CFData))
      #expect(PinnedTrustEvaluator.fingerprint(of: certificate) == identity.fingerprint)
      #expect(identity.fingerprint == ServerIdentity.fingerprint(der: identity.certificateDER))

      let payload = await test.service.beginPairing()
      let scanned = try PairingPayload(parsing: try #require(URL(string: payload.urlString)))
      #expect(scanned.fingerprint == PinnedTrustEvaluator.fingerprint(of: certificate))

      var trust: SecTrust?
      #expect(
        SecTrustCreateWithCertificates(certificate, SecPolicyCreateBasicX509(), &trust)
          == errSecSuccess)
      let presented = try #require(trust)
      #expect(PinnedTrustEvaluator.evaluate(presented, pinnedFingerprint: scanned.fingerprint))
      var flipped = scanned.fingerprint
      flipped[17] ^= 0x40
      #expect(!PinnedTrustEvaluator.evaluate(presented, pinnedFingerprint: flipped))
      #expect(
        !PinnedTrustEvaluator.evaluate(presented, pinnedFingerprint: scanned.fingerprint.prefix(31))
      )
      #expect(!PinnedTrustEvaluator.evaluate(presented, pinnedFingerprint: Data()))
    }

    @Test func plaintextHTTPToTheListenerNeverReachesTheRouter() async throws {
      // The phone builds `https://` origins only; a stray `http://` client
      // meets the TLS handshake and no request line is ever parsed.
      let test = try await TestService.start()
      defer { Task { await test.stop() } }
      let port = try #require(await test.service.port)
      let plain = LoopbackClient(
        baseURL: try #require(URL(string: "http://127.0.0.1:\(port)")),
        fingerprint: test.service.identity.fingerprint, timeout: 5)

      await #expect(throws: (any Error).self) {
        _ = try await plain.request("GET", "/v1/hello")
      }
      #expect(test.metrics.requestHeads == 0)
      #expect(test.metrics.handledRequests == 0)
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
