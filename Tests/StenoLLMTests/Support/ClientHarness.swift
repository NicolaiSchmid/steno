import Foundation
import StenoCore
import Synchronization

@testable import StenoLLM

/// A stub server, a `ManualClock`, an event log and a client wired to them.
/// `driveRetries()` advances the clock by every backoff the client announces,
/// so a retry test never sleeps on wall time.
final class ClientHarness: Sendable {
  static let apiKey = "sk-test-secret-0123456789"

  let server: StubChatServer
  let clock: ManualClock
  let client: OpenAICompatibleClient
  let endpoint: LLMEndpoint
  private let log: EventLog
  private let stream: AsyncStream<LLMClientEvent>
  private let continuation: AsyncStream<LLMClientEvent>.Continuation

  init(
    retry: RetryPolicy = .default,
    apiKey: String? = ClientHarness.apiKey,
    configure: (inout LLMEndpoint) -> Void = { _ in }
  ) throws {
    let server = try StubChatServer()
    var endpoint = LLMEndpoint(baseURL: server.baseURL, model: "stub-model")
    configure(&endpoint)
    let clock = ManualClock()
    let (stream, continuation) = AsyncStream<LLMClientEvent>.makeStream()
    let log = EventLog()
    self.server = server
    self.clock = clock
    self.endpoint = endpoint
    self.stream = stream
    self.continuation = continuation
    self.log = log
    self.client = OpenAICompatibleClient(
      endpoint: endpoint, apiKey: apiKey, retry: retry, clock: clock,
      observer: { event in
        log.append(event)
        continuation.yield(event)
      })
  }

  var events: [LLMClientEvent] { log.entries }

  func stop() {
    continuation.finish()
    server.stop()
  }

  /// The next event matching `predicate`, consuming earlier ones.
  func next(where predicate: (LLMClientEvent) -> Bool) async -> LLMClientEvent? {
    for await event in stream where predicate(event) {
      return event
    }
    return nil
  }

  /// Consumes events in the background and advances the clock by each
  /// announced backoff once the client is asleep.
  func driveRetries() -> Task<Void, Never> {
    Task { [clock, stream] in
      for await event in stream {
        if case .retrying(let delay, _, _) = event {
          _ = await clock.waitForSleepers(1)
          clock.advance(by: delay)
        }
      }
    }
  }

  static func request(
    purpose: String = "test", format: LLMResponseFormat = .jsonObject
  ) -> LLMRequest {
    LLMRequest(
      messages: [
        LLMMessage(role: .system, content: "You are a test."),
        LLMMessage(role: .user, content: "Say hi as JSON."),
      ],
      responseFormat: format,
      temperature: 0,
      maxTokens: 64,
      purpose: purpose)
  }
}

/// Events in arrival order, readable from any thread.
final class EventLog: Sendable {
  private let storage = Mutex<[LLMClientEvent]>([])

  func append(_ event: LLMClientEvent) {
    storage.withLock { $0.append(event) }
  }

  var entries: [LLMClientEvent] { storage.withLock { $0 } }
}
