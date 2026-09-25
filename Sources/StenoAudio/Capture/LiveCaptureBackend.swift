import Foundation
import StenoCore

#if canImport(CoreAudio)
  import CoreAudio

  /// The real backend: process tap + private aggregate device + one IOProc.
  /// `.system` comes from the tap, `.mic` and `.mixed` from the first channel
  /// of the selected input device, which the aggregate resamples to the
  /// output device's 48 kHz clock. A default-device change or a sub-device
  /// dying reports device loss; the session then stops cleanly (rebuilding
  /// mid-meeting is v1.1).
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
    }

    private let lock = NSLock()
    private var active: Active?
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

      // Device loss: the default devices changing, or a sub-device dying. The
      // tap mirrors the default output device (where the call plays), the
      // clock follows the system output device (alerts); a change of either
      // moves the far-end alignment, so both are watched.
      var watched: [(AudioObjectID, AudioObjectPropertySelector)] = [
        (.system, kAudioHardwarePropertyDefaultSystemOutputDevice),
        (.system, kAudioHardwarePropertyDefaultOutputDevice),
        (AudioObjectID(output.id), kAudioDevicePropertyDeviceIsAlive),
      ]
      if let mic {
        watched.append((AudioObjectID(mic.id), kAudioDevicePropertyDeviceIsAlive))
        if inputDeviceUID == nil {
          watched.append((.system, kAudioHardwarePropertyDefaultInputDevice))
        }
      }
      let lost: @Sendable () -> Void = { sink.reportDeviceLost() }
      let listeners = watched.compactMap { object, selector in
        try? object.addListener(AudioObjectPropertyAddress(selector), queue: listenerQueue, lost)
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
      active = Active(tap: tap, aggregate: aggregate, runner: runner, listeners: listeners)
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
      active.runner.stop()
      for listener in active.listeners { listener.remove() }
      active.aggregate.destroy()
      active.tap?.destroy()
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
