import Foundation
import StenoCore
import Testing

@testable import StenoHandover

/// Pairing on the injected wall clock: the first use of a secret pairs, the
/// second is 403, an expired window is 403, a revoked token is 401.
@Suite struct PairingFlowTests {
  static let deviceID = UUID(uuidString: "0BADF00D-0000-4000-8000-000000000001")!

  @Test func firstUsePairsAndTheSecondIsForbidden() async throws {
    try await TestService.run { test in
      let payload = await test.service.beginPairing()
      #expect(payload.macID == test.service.identity.macID)
      #expect(payload.fingerprint == test.service.identity.fingerprint)
      #expect(payload.macName == "Test Mac")
      #expect(payload.expiresAt == test.now.addingTimeInterval(300))
      #expect(payload.urlString.hasPrefix("steno://pair/v1?mac="))

      let first = try await Phone.pair(
        test.service, secret: payload.secret, deviceID: Self.deviceID,
        deviceName: "Nicolai's iPhone")
      #expect(first.status == 200)
      let response = try first.json(Wire.PairResponse.self)
      #expect(response.macID == test.service.identity.macID)
      #expect(response.macName == "Test Mac")
      #expect(Data(base64Encoded: response.token)?.count == 32)

      let devices = try await test.service.pairedDevices()
      #expect(devices.map(\.id) == [Self.deviceID])
      #expect(devices.first?.name == "Nicolai's iPhone")
      #expect(devices.first?.pairedAt == test.now)

      let second = try await Phone.pair(
        test.service, secret: payload.secret, deviceID: Self.deviceID)
      #expect(second.status == 403)
      #expect(test.metrics.handledRequests == 1, "the second attempt fails at the gate")

      // The token works for a bearer route, and the phone can unpair itself.
      let client = try test.client()
      let unpair = try await client.request(
        "DELETE", "/v1/pairing", headers: LoopbackClient.bearer(response.token))
      #expect(unpair.status == 204)
      #expect(try await test.service.pairedDevices().isEmpty)
      let after = try await client.request(
        "DELETE", "/v1/pairing", headers: LoopbackClient.bearer(response.token))
      #expect(after.status == 401, "the Mac already forgot the phone")
    }
  }

  @Test func theWindowClosesAfter300SecondsOnTheInjectedClock() async throws {
    try await TestService.run { test in
      let payload = await test.service.beginPairing()

      test.advance(by: .seconds(299))
      #expect(await test.service.engine.pairingIsOpen)
      test.advance(by: .seconds(2))
      #expect(await test.service.engine.pairingIsOpen == false)

      let late = try await Phone.pair(
        test.service, secret: payload.secret, deviceID: Self.deviceID)
      #expect(late.status == 403)
      #expect(try await test.service.pairedDevices().isEmpty)
    }
  }

  @Test func wrongSecretsAndNoSessionAreForbidden() async throws {
    try await TestService.run { test in
      let noSession = try await Phone.pair(
        test.service, secret: Data(repeating: 1, count: 32), deviceID: Self.deviceID)
      #expect(noSession.status == 403)

      let payload = await test.service.beginPairing()
      var wrong = payload.secret
      wrong[5] ^= 0xFF
      #expect(
        try await Phone.pair(test.service, secret: wrong, deviceID: Self.deviceID).status == 403)
      #expect(
        try await Phone.pair(test.service, secret: Data(), deviceID: Self.deviceID).status == 403)
      let client = try test.client()
      let bearerInsteadOfPairing = try await client.json(
        "POST", "/v1/pair", headers: LoopbackClient.bearer(payload.secret.base64EncodedString()),
        body: Wire.PairRequest(deviceID: Self.deviceID, deviceName: "x"))
      #expect(bearerInsteadOfPairing.status == 403)

      // A wrong secret does not burn the session; the right one still pairs,
      // and base64url (the QR form) is accepted too.
      let urlForm = try await client.json(
        "POST", "/v1/pair",
        headers: ["Authorization": "Pairing \(Base64URL.encode(payload.secret))"],
        body: Wire.PairRequest(deviceID: Self.deviceID, deviceName: "iPhone"))
      #expect(urlForm.status == 200)

      await test.service.cancelPairing()
      let cancelled = await test.service.beginPairing()
      await test.service.cancelPairing()
      #expect(
        try await Phone.pair(test.service, secret: cancelled.secret, deviceID: Self.deviceID)
          .status == 403)
    }
  }

  @Test func malformedPairRequestsAre400AndKeepTheWindowOpen() async throws {
    try await TestService.run { test in
      let payload = await test.service.beginPairing()
      let client = try test.client()

      let garbage = try await client.request(
        "POST", "/v1/pair", headers: LoopbackClient.pairing(payload.secret),
        body: Data("not json".utf8))
      #expect(garbage.status == 400)
      let blankName = try await Phone.pair(
        test.service, secret: payload.secret, deviceID: Self.deviceID, deviceName: "   ")
      #expect(blankName.status == 400)
      #expect(try await test.service.pairedDevices().isEmpty)

      let good = try await Phone.pair(test.service, secret: payload.secret, deviceID: Self.deviceID)
      #expect(good.status == 200)
    }
  }

  @Test func revokedTokensAre401AndRepairingReplacesTheDevice() async throws {
    try await TestService.run { test in
      let client = try test.client()

      let first = try await Phone.pair(
        test.service, secret: await test.service.beginPairing().secret, deviceID: Self.deviceID)
      let token = try first.json(Wire.PairResponse.self).token
      let probe = try await client.request(
        "GET", "/v1/recordings/\(UUID().uuidString)", headers: LoopbackClient.bearer(token))
      #expect(probe.status != 401, "a fresh token passes the gate")

      try await test.service.revoke(Self.deviceID)
      #expect(try await test.service.pairedDevices().isEmpty)
      let revoked = try await client.request(
        "GET", "/v1/recordings/\(UUID().uuidString)", headers: LoopbackClient.bearer(token))
      #expect(revoked.status == 401)

      let again = try await Phone.pair(
        test.service, secret: await test.service.beginPairing().secret, deviceID: Self.deviceID,
        deviceName: "New phone")
      #expect(again.status == 200)
      let newToken = try again.json(Wire.PairResponse.self).token
      #expect(newToken != token)
      let devices = try await test.service.pairedDevices()
      #expect(devices.count == 1)
      #expect(devices.first?.name == "New phone")
      let stillRevoked = try await client.request(
        "GET", "/v1/recordings/\(UUID().uuidString)", headers: LoopbackClient.bearer(token))
      #expect(stillRevoked.status == 401)
    }
  }

  @Test func credentialParsingIsCaseInsensitiveOnTheScheme() {
    #expect(HandoverEngine.credential(scheme: "Bearer", in: "Bearer abc") == "abc")
    #expect(HandoverEngine.credential(scheme: "Bearer", in: "bearer abc") == "abc")
    #expect(HandoverEngine.credential(scheme: "Bearer", in: "Pairing abc") == nil)
    #expect(HandoverEngine.credential(scheme: "Bearer", in: "Bearer") == nil)
    #expect(HandoverEngine.credential(scheme: "Bearer", in: "Bearer  ") == nil)
    #expect(HandoverEngine.credential(scheme: "Bearer", in: nil) == nil)
  }
}
