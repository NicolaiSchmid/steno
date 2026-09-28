import Foundation
import StenoCore

/// The devices a capture runs on, as resolved at one moment: the default
/// system output's UID (the aggregate's clock master), the default output's
/// UID (where the call plays, which the tap mirrors), the microphone's UID
/// (nil when no microphone lane is recorded, or none resolves), whether the
/// two devices the capture started on are still alive, and the aggregate's
/// rate. Pure, so the comparison the live backend makes after a notification
/// burst is unit-tested without a HAL.
struct DeviceSnapshot: Sendable, Equatable {
  var outputUID: String?
  var defaultOutputUID: String?
  var inputUID: String?
  var outputAlive: Bool
  var inputAlive: Bool
  var sampleRate: Double

  /// The first thing that differs from `baseline`, or nil when the devices
  /// are the same, alive and at the same rate. Loss comes before movement:
  /// a dead device is why a default moved.
  func difference(from baseline: DeviceSnapshot) -> DeviceChangeReason? {
    if baseline.outputAlive, !outputAlive { return .outputDeviceGone }
    if baseline.inputAlive, !inputAlive { return .inputDeviceGone }
    if outputUID != baseline.outputUID { return .defaultOutputChanged }
    if defaultOutputUID != baseline.defaultOutputUID { return .defaultOutputChanged }
    if inputUID != baseline.inputUID { return .defaultInputChanged }
    if sampleRate != baseline.sampleRate { return .sampleRateChanged }
    return nil
  }
}

#if canImport(CoreAudio)
  import CoreAudio
  import os

  /// The real backend: process tap + private aggregate device + one IOProc.
  /// `.system` comes from the tap, `.mic` and `.mixed` from the first channel
  /// of the selected input device, which the aggregate resamples to the
  /// output device's 48 kHz clock.
  ///
  /// Device notifications (a default device moving, a sub-device dying, the
  /// aggregate leaving 48 kHz) are coalesced for `coalesceDelay` on
  /// `listenerQueue`, then the devices are resolved again and compared with
  /// what the capture started on. Nothing changed means the burst is logged
  /// and ignored; otherwise the sink gets one `DeviceChangeReason` and the
  /// session rebuilds by calling `stop()` and `start` again. Nothing here
  /// runs on the IO thread.
  ///
  /// Teardown order: `AudioDeviceStop`, `AudioDeviceDestroyIOProcID`,
  /// `AudioHardwareDestroyAggregateDevice`, `AudioHardwareDestroyProcessTap`.
  public final class LiveCaptureBackend: CaptureBackend, @unchecked Sendable {
    /// What one started capture holds.
    struct Active {
      var tap: ProcessTap?
      var aggregate: AggregateDevice
      var runner: IOProcRunner
      var listeners: [AudioPropertyListenerToken]
      var sink: LaneFrameSink
      /// Which `start` this is, so a look at the devices that began under
      /// the previous capture cannot report on this one: the session hands
      /// the same sink to the rebuilt backend, so the sink cannot tell them
      /// apart.
      var generation: Int
      /// The devices the capture started on.
      var baseline: DeviceSnapshot
      /// The HAL objects to ask about them again.
      var probe: DeviceProbe
      /// The coalesced look at the devices, if one is scheduled.
      var pending: DispatchWorkItem?
    }

    /// The HAL reads behind one `DeviceSnapshot`, fixed at `start` so every
    /// look after a notification asks about the objects the capture began
    /// on. The comparison itself lives on `DeviceSnapshot`, without a HAL.
    struct DeviceProbe {
      var outputID: AudioObjectID
      var micID: AudioObjectID?
      var inputDeviceUID: String?
      var aggregateID: AudioObjectID

      /// The default output device's UID, nil when none resolves.
      static func defaultOutputUID() -> String? {
        try? AudioDevices.uid(
          of: AudioDevices.defaultDevice(kAudioHardwarePropertyDefaultOutputDevice))
      }

      /// The devices as they are now: the defaults resolved again (or the
      /// explicit input by UID), the started devices' `DeviceIsAlive`, the
      /// aggregate's rate (0 once it is gone).
      func resolve() -> DeviceSnapshot {
        let output = try? AudioDevices.defaultSystemOutput()
        var input: AudioDeviceInfo?
        if micID != nil {
          if let inputDeviceUID {
            input = try? AudioDevices.device(uid: inputDeviceUID)
          } else {
            input = try? AudioDevices.defaultInput()
          }
        }
        let rate =
          (try? aggregateID.readFloat64(
            AudioObjectPropertyAddress(kAudioDevicePropertyNominalSampleRate))) ?? 0
        return DeviceSnapshot(
          outputUID: output?.uid, defaultOutputUID: Self.defaultOutputUID(), inputUID: input?.uid,
          outputAlive: AudioDevices.isAlive(outputID),
          inputAlive: micID.map(AudioDevices.isAlive) ?? false,
          sampleRate: rate)
      }
    }

    /// How long a burst of notifications settles before the devices are
    /// resolved once. A Bluetooth profile switch fires several within it.
    static let coalesceDelay: DispatchTimeInterval = .milliseconds(500)
    private static let log = Logger(subsystem: "uno.schmid.steno", category: "capture")

    private let lock = NSLock()
    private var active: Active?
    /// `start` calls so far; `Active.generation` for the next one.
    private var startGeneration = 0
    private let listenerQueue = DispatchQueue(label: "uno.schmid.steno.audio.devices")

    public init() {}

    /// A session dropped without `stop()` still tears the HAL objects down in
    /// order (IOProc, listeners, aggregate, tap) instead of leaving it to
    /// property destruction order.
    deinit { stop() }

    /// Returns the stream it opened: the confirmed 48 kHz rate, both device
    /// latencies for the far-end delay, and the resolved `StreamLayout` so
    /// `steno dev capture-spike` can attribute buffers to lanes.
    public func start(lanes: [AudioLane], inputDeviceUID: String?, sink: LaneFrameSink) throws
      -> CaptureStream
    {
      lock.lock()
      defer { lock.unlock() }
      guard active == nil else { throw CaptureError.invalidState("backend already started") }
      startGeneration += 1

      let needsMic = lanes.contains(.mic) || lanes.contains(.mixed)
      let needsTap = lanes.contains(.system)

      guard let output = try? AudioDevices.defaultSystemOutput() else {
        throw CaptureError.outputDeviceUnavailable
      }
      var mic: AudioDeviceInfo?
      if needsMic {
        if let inputDeviceUID {
          mic = try? AudioDevices.device(uid: inputDeviceUID)
        } else {
          mic = try? AudioDevices.defaultInput()
        }
        guard let resolved = mic, resolved.isInput else {
          throw CaptureError.inputDeviceUnavailable
        }
      }

      var tap: ProcessTap?
      if needsTap {
        let ownProcess: AudioObjectID
        do {
          ownProcess = try LiveProcessAudioActivity.ownProcessObject()
        } catch let error as CaptureError {
          throw error
        } catch {
          throw CaptureError.backendFailed("own process object: \(error)")
        }
        tap = try ProcessTap(excluding: [ownProcess])
      }

      // Sub-devices in aggregate order: the output device (clock master), then
      // the microphone unless it is the same physical device.
      var subDeviceUIDs = [output.uid]
      var subDeviceCounts = [AudioDevices.inputChannelCounts(of: AudioObjectID(output.id))]
      var micSubDevice: Int?
      if let mic {
        if mic.uid == output.uid {
          micSubDevice = 0
        } else {
          subDeviceUIDs.append(mic.uid)
          subDeviceCounts.append(AudioDevices.inputChannelCounts(of: AudioObjectID(mic.id)))
          micSubDevice = 1
        }
      }

      let aggregate: AggregateDevice
      do {
        aggregate = try AggregateDevice(
          name: "Steno capture", mainSubDeviceUID: output.uid, subDeviceUIDs: subDeviceUIDs,
          tapUIDs: tap.map { [$0.uid] } ?? [])
      } catch {
        tap?.destroy()
        throw error
      }
      // The aggregate inherits the clock master's rate. Ask for 48 kHz, then
      // read it back: the HAL applies the change asynchronously and a device
      // that cannot run at 48 kHz keeps its own, which would leave a
      // pitch-shifted master labelled 48 kHz. Fail loud instead.
      if aggregate.nominalSampleRate != StenoAudio.sampleRate {
        try? aggregate.setNominalSampleRate(StenoAudio.sampleRate)
      }
      let sampleRate = NominalSampleRate.settle(
        to: StenoAudio.sampleRate, read: { aggregate.nominalSampleRate },
        wait: { Thread.sleep(forTimeInterval: NominalSampleRate.interval) })
      guard sampleRate == StenoAudio.sampleRate else {
        aggregate.destroy()
        tap?.destroy()
        throw CaptureError.sampleRateMismatch(actual: sampleRate)
      }

      let layout: StreamLayout
      do {
        layout = try StreamLayout.resolve(
          lanes: lanes, aggregate: aggregate.inputChannelCounts, subDevices: subDeviceCounts,
          tap: tap?.bufferChannelCounts ?? [], micSubDevice: micSubDevice)
      } catch {
        aggregate.destroy()
        tap?.destroy()
        throw error
      }

      let runner = IOProcRunner(deviceID: aggregate.deviceID, sink: sink, layout: layout)
      do {
        try runner.start()
      } catch {
        aggregate.destroy()
        tap?.destroy()
        throw error
      }

      // Device changes: the default devices moving, a sub-device dying, the
      // aggregate leaving 48 kHz (a Bluetooth profile switch can change the
      // rate without moving a default). The tap mirrors the default output
      // device (where the call plays), the clock follows the system output
      // device (alerts); a change of either moves the far-end alignment, so
      // both are watched. The listener carries no value, so every
      // notification is judged by resolving the devices again after the
      // burst settles.
      var watched: [(AudioObjectID, AudioObjectPropertySelector)] = [
        (.system, kAudioHardwarePropertyDefaultSystemOutputDevice),
        (.system, kAudioHardwarePropertyDefaultOutputDevice),
        (AudioObjectID(output.id), kAudioDevicePropertyDeviceIsAlive),
        (aggregate.deviceID, kAudioDevicePropertyNominalSampleRate),
      ]
      if let mic {
        watched.append((AudioObjectID(mic.id), kAudioDevicePropertyDeviceIsAlive))
        if inputDeviceUID == nil {
          watched.append((.system, kAudioHardwarePropertyDefaultInputDevice))
        }
      }
      let listeners = watched.compactMap { object, selector in
        try? object.addListener(AudioObjectPropertyAddress(selector), queue: listenerQueue) {
          [weak self] in self?.noteNotification(selector)
        }
      }

      // The far-end delay: the microphone's input path plus the loudspeaker's
      // output path, each latency plus safety offset, read on the devices
      // themselves rather than the aggregate.
      let inputLatency =
        mic.map {
          AudioDevices.latencyFrames(
            of: AudioObjectID($0.id), scope: kAudioObjectPropertyScopeInput)
        } ?? 0
      let outputLatency = AudioDevices.latencyFrames(
        of: AudioObjectID(output.id), scope: kAudioObjectPropertyScopeOutput)
      active = Active(
        tap: tap, aggregate: aggregate, runner: runner, listeners: listeners, sink: sink,
        generation: startGeneration,
        baseline: DeviceSnapshot(
          outputUID: output.uid, defaultOutputUID: DeviceProbe.defaultOutputUID(),
          inputUID: mic?.uid, outputAlive: true, inputAlive: mic != nil, sampleRate: sampleRate),
        probe: DeviceProbe(
          outputID: AudioObjectID(output.id), micID: mic.map { AudioObjectID($0.id) },
          inputDeviceUID: inputDeviceUID, aggregateID: aggregate.deviceID))
      return CaptureStream(
        sampleRate: sampleRate, inputLatencyFrames: inputLatency,
        outputLatencyFrames: outputLatency, layout: layout)
    }

    public func stop() {
      lock.lock()
      let active = self.active
      self.active = nil
      lock.unlock()
      guard let active else { return }
      active.pending?.cancel()
      active.runner.stop()
      for listener in active.listeners { listener.remove() }
      active.aggregate.destroy()
      active.tap?.destroy()
    }

    /// A property listener fired, on `listenerQueue`. Bluetooth transitions
    /// fire several in a burst, some of which change nothing, so one look at
    /// the devices is scheduled `coalesceDelay` after the last notification
    /// and the burst is judged as a whole.
    private func noteNotification(_ selector: AudioObjectPropertySelector) {
      lock.lock()
      defer { lock.unlock() }
      guard var active else { return }
      active.pending?.cancel()
      let item = DispatchWorkItem { [weak self] in self?.evaluateNotification(selector) }
      active.pending = item
      self.active = active
      listenerQueue.asyncAfter(deadline: .now() + Self.coalesceDelay, execute: item)
    }

    /// Resolves the devices again and compares them with the baseline. The
    /// HAL reads run outside the lock; the report goes out only if the same
    /// `start` is still running (the same sink is not enough: a rebuild
    /// reuses it), so a stopped backend never tells a later capture about
    /// the old one's devices.
    private func evaluateNotification(_ selector: AudioObjectPropertySelector) {
      lock.lock()
      guard var current = active else {
        lock.unlock()
        return
      }
      current.pending = nil
      active = current
      lock.unlock()

      let snapshot = current.probe.resolve()
      let name = fourCharCode(OSStatus(bitPattern: selector))
      guard let reason = snapshot.difference(from: current.baseline) else {
        Self.log.info("ignored device notification \(name, privacy: .public)")
        return
      }
      lock.lock()
      let sameCapture = active?.generation == current.generation
      lock.unlock()
      guard sameCapture else { return }
      Self.log.notice(
        "device notification \(name, privacy: .public) reported \(String(describing: reason), privacy: .public)"
      )
      current.sink.reportDeviceChange(reason)
    }
  }
#else
  /// On platforms without Core Audio the live backend exists so callers
  /// compile, and fails at `start`.
  public final class LiveCaptureBackend: CaptureBackend, @unchecked Sendable {
    public init() {}

    public func start(lanes: [AudioLane], inputDeviceUID: String?, sink: LaneFrameSink) throws
      -> CaptureStream
    {
      throw CaptureError.backendFailed("live capture needs macOS (Core Audio)")
    }

    public func stop() {}
  }
#endif
