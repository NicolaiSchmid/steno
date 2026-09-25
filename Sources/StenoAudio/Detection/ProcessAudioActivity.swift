import Foundation

/// One process the HAL knows about and whether it currently has an input
/// (microphone) stream running.
public struct ProcessAudioActivity: Sendable, Equatable, Hashable {
  public var pid: pid_t
  public var bundleID: String?
  public var isRunningInput: Bool
  public var isRunningOutput: Bool

  public init(
    pid: pid_t, bundleID: String?, isRunningInput: Bool, isRunningOutput: Bool = false
  ) {
    self.pid = pid
    self.bundleID = bundleID
    self.isRunningInput = isRunningInput
    self.isRunningOutput = isRunningOutput
  }
}

/// The seam under `MeetingDetector`: a snapshot of every process's microphone
/// state, plus a stream that fires whenever the snapshot may have changed.
/// `LiveProcessAudioActivity` reads the HAL; `FakeProcessAudioActivity` in
/// `Testing/` is scripted.
public protocol ProcessAudioActivitySource: Sendable {
  func snapshot() throws -> [ProcessAudioActivity]
  /// One element per HAL notification (`DeviceIsRunningSomewhere` on an input
  /// device, the process list, the device list). Payload-free: the detector
  /// re-reads `snapshot()`. Registering happens on the first iteration and
  /// ends when the stream is cancelled.
  func changes() -> AsyncStream<Void>
}

#if canImport(CoreAudio)
  import CoreAudio

  /// Process objects (`kAudioHardwarePropertyProcessObjectList`) with their PID,
  /// bundle id and `IsRunningInput` flag, and listeners on
  /// `kAudioDevicePropertyDeviceIsRunningSomewhere` for every input device.
  public struct LiveProcessAudioActivity: ProcessAudioActivitySource {
    public init() {}

    /// The HAL's process object for this process, or `kAudioObjectUnknown`.
    public static func ownProcessObject() -> AudioObjectID {
      (try? processObject(pid: ProcessInfo.processInfo.processIdentifier)) ?? .unknown
    }

    public static func processObject(pid: pid_t) throws -> AudioObjectID {
      try AudioObjectID.system.read(
        AudioObjectPropertyAddress(kAudioHardwarePropertyTranslatePIDToProcessObject),
        qualifier: pid, defaultValue: AudioObjectID.unknown)
    }

    public static func processObjects() throws -> [AudioObjectID] {
      try AudioObjectID.system.readArray(
        AudioObjectPropertyAddress(kAudioHardwarePropertyProcessObjectList), of: AudioObjectID.self
      )
    }

    public static func activity(of object: AudioObjectID) -> ProcessAudioActivity? {
      guard
        let pid = try? object.read(
          AudioObjectPropertyAddress(kAudioProcessPropertyPID), defaultValue: pid_t(0))
      else { return nil }
      let bundle = try? object.readString(AudioObjectPropertyAddress(kAudioProcessPropertyBundleID))
      return ProcessAudioActivity(
        pid: pid,
        bundleID: (bundle?.isEmpty ?? true) ? nil : bundle,
        isRunningInput: (try? object.readBool(
          AudioObjectPropertyAddress(kAudioProcessPropertyIsRunningInput))) ?? false,
        isRunningOutput: (try? object.readBool(
          AudioObjectPropertyAddress(kAudioProcessPropertyIsRunningOutput))) ?? false)
    }

    public func snapshot() throws -> [ProcessAudioActivity] {
      try Self.processObjects().compactMap(Self.activity(of:))
    }

    public func changes() -> AsyncStream<Void> {
      AsyncStream { continuation in
        let queue = DispatchQueue(label: "uno.schmid.steno.audio.activity")
        let registry = ListenerRegistry()
        let fire: @Sendable () -> Void = { continuation.yield(()) }
        let system = AudioObjectID.system
        registry.add(
          try? system.addListener(
            AudioObjectPropertyAddress(kAudioHardwarePropertyProcessObjectList), queue: queue,
            fire))
        // Input devices come and go; re-register their running listeners on
        // every device-list change.
        let rebuild: @Sendable () -> Void = {
          registry.replaceDeviceListeners(
            ((try? AudioDevices.inputs()) ?? []).compactMap { device in
              try? AudioObjectID(device.id).addListener(
                AudioObjectPropertyAddress(
                  kAudioDevicePropertyDeviceIsRunningSomewhere), queue: queue, fire)
            })
          fire()
        }
        registry.add(
          try? system.addListener(
            AudioObjectPropertyAddress(kAudioHardwarePropertyDevices), queue: queue, rebuild))
        rebuild()
        continuation.onTermination = { _ in registry.removeAll() }
      }
    }
  }

  /// Holds listener tokens for one `changes()` stream.
  private final class ListenerRegistry: @unchecked Sendable {
    private let lock = NSLock()
    private var fixed: [AudioPropertyListenerToken] = []
    private var devices: [AudioPropertyListenerToken] = []

    func add(_ token: AudioPropertyListenerToken?) {
      guard let token else { return }
      lock.lock()
      fixed.append(token)
      lock.unlock()
    }

    func replaceDeviceListeners(_ tokens: [AudioPropertyListenerToken]) {
      lock.lock()
      let old = devices
      devices = tokens
      lock.unlock()
      for token in old { token.remove() }
    }

    func removeAll() {
      lock.lock()
      let all = fixed + devices
      fixed = []
      devices = []
      lock.unlock()
      for token in all { token.remove() }
    }
  }
#endif
