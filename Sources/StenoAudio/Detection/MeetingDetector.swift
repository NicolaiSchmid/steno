import Foundation
import StenoCore

/// Tells the app when another process opens the microphone (a call starting)
/// and when it lets go. Reads `ProcessAudioActivitySource.snapshot()` on
/// every HAL change notification and on a 2 s poll (the listener behaviour
/// is undocumented, so the poll is the safety net), ignores its own PID, and
/// debounces both edges by 2 s so a flapping input yields one
/// `microphoneOpened` and one `microphoneReleased`. Every timer runs on the
/// injected `Clock`, so tests drive `ManualClock` and never sleep.
public actor MeetingDetector {
  public enum Event: Sendable, Equatable {
    case microphoneOpened(bundleID: String?, pid: pid_t)
    case microphoneReleased
  }

  public let debounce: Duration
  public let pollInterval: Duration
  private let source: any ProcessAudioActivitySource
  private let clock: any Clock<Duration>
  private let ignoringPIDs: Set<pid_t>
  private var continuations: [UUID: AsyncStream<Event>.Continuation] = [:]
  private var listenTask: Task<Void, Never>?
  private var pollTask: Task<Void, Never>?
  private var pending: Task<Void, Never>?
  private var pendingGeneration = 0
  /// The process reported as holding the microphone, nil while released.
  public private(set) var holder: ProcessAudioActivity?
  /// What the last snapshot said, before debouncing.
  private var observedActive: ProcessAudioActivity?

  public init(
    source: any ProcessAudioActivitySource,
    clock: any Clock<Duration> = ContinuousClock(),
    ignoringPIDs: Set<pid_t> = [ProcessInfo.processInfo.processIdentifier],
    debounce: Duration = .seconds(2),
    pollInterval: Duration = .seconds(2)
  ) {
    self.source = source
    self.clock = clock
    self.ignoringPIDs = ignoringPIDs
    self.debounce = debounce
    self.pollInterval = pollInterval
  }

  #if canImport(CoreAudio)
    /// The live HAL source with the default clock.
    public init(
      clock: any Clock<Duration> = ContinuousClock(),
      ignoringPIDs: Set<pid_t> = [ProcessInfo.processInfo.processIdentifier]
    ) {
      self.init(source: LiveProcessAudioActivity(), clock: clock, ignoringPIDs: ignoringPIDs)
    }
  #endif

  public var isRunning: Bool { pollTask != nil }

  /// Events from now on. Finishes when the detector stops.
  public var events: AsyncStream<Event> {
    let id = UUID()
    return AsyncStream { continuation in
      continuations[id] = continuation
      continuation.onTermination = { [weak self] _ in
        Task { await self?.remove(id) }
      }
    }
  }

  private func remove(_ id: UUID) { continuations[id] = nil }

  private func emit(_ event: Event) {
    for continuation in continuations.values { continuation.yield(event) }
  }

  /// Reads a first snapshot (an already-open microphone is reported after
  /// the debounce like any other), then listens and polls.
  public func start() throws {
    guard pollTask == nil else { return }
    try evaluate()
    let source = self.source
    listenTask = Task { [weak self] in
      for await _ in source.changes() {
        guard let self else { return }
        await self.evaluateIgnoringErrors()
      }
    }
    let clock = self.clock
    let pollInterval = self.pollInterval
    pollTask = Task { [weak self] in
      while !Task.isCancelled {
        try? await clock.sleep(for: pollInterval)
        guard !Task.isCancelled, let self else { return }
        await self.evaluateIgnoringErrors()
      }
    }
  }

  public func stop() {
    listenTask?.cancel()
    listenTask = nil
    pollTask?.cancel()
    pollTask = nil
    pending?.cancel()
    pending = nil
    pendingGeneration += 1
    holder = nil
    observedActive = nil
    for continuation in continuations.values { continuation.finish() }
    continuations = [:]
  }

  private func evaluateIgnoringErrors() {
    try? evaluate()
  }

  /// Compares the snapshot with what was reported and arms or disarms the
  /// debounce timer.
  private func evaluate() throws {
    let active = try source.snapshot()
      .filter { $0.isRunningInput && !ignoringPIDs.contains($0.pid) }
      .sorted { $0.pid < $1.pid }
    // Keep reporting the same holder while it stays active.
    let current: ProcessAudioActivity? =
      active.first { $0.pid == holder?.pid } ?? active.first { $0.pid == observedActive?.pid }
      ?? active.first
    observedActive = current

    let wantOpen = current != nil
    let isOpen = holder != nil
    if wantOpen == isOpen {
      // Back to the reported state within the debounce: forget the edge.
      cancelPending()
      return
    }
    guard pending == nil else { return }
    pendingGeneration += 1
    let generation = pendingGeneration
    pending = Task { [weak self, clock, debounce] in
      try? await clock.sleep(for: debounce)
      guard !Task.isCancelled, let self else { return }
      await self.debounceElapsed(generation)
    }
  }

  private func cancelPending() {
    pending?.cancel()
    pending = nil
    pendingGeneration += 1
  }

  private func debounceElapsed(_ generation: Int) {
    guard generation == pendingGeneration else { return }
    pending = nil
    if let process = observedActive, holder == nil {
      holder = process
      emit(.microphoneOpened(bundleID: process.bundleID, pid: process.pid))
    } else if observedActive == nil, holder != nil {
      holder = nil
      emit(.microphoneReleased)
    }
  }
}
