#if canImport(CoreAudio)
  import CoreAudio
  import Foundation

  /// A failed Core Audio call: which property or function, on which object,
  /// with which `OSStatus`.
  public struct CoreAudioError: Error, Sendable, Equatable, CustomStringConvertible {
    public var operation: String
    public var objectID: AudioObjectID
    public var status: OSStatus

    public init(operation: String, objectID: AudioObjectID, status: OSStatus) {
      self.operation = operation
      self.objectID = objectID
      self.status = status
    }

    public var description: String {
      "\(operation) on audio object \(objectID) failed: \(status) (\(fourCharCode(status)))"
    }
  }

  /// Renders an `OSStatus` as its four-character code when it is one
  /// (`'!obj'`, `'who?'`), else as the number.
  func fourCharCode(_ status: OSStatus) -> String {
    let value = UInt32(bitPattern: status)
    let bytes = [
      UInt8((value >> 24) & 0xff), UInt8((value >> 16) & 0xff), UInt8((value >> 8) & 0xff),
      UInt8(value & 0xff),
    ]
    guard bytes.allSatisfy({ $0 >= 0x20 && $0 < 0x7f }) else { return String(status) }
    return "'" + String(decoding: bytes, as: UTF8.self) + "'"
  }

  extension AudioObjectPropertyAddress {
    init(
      _ selector: AudioObjectPropertySelector,
      scope: AudioObjectPropertyScope = kAudioObjectPropertyScopeGlobal,
      element: AudioObjectPropertyElement = kAudioObjectPropertyElementMain
    ) {
      self.init(mSelector: selector, mScope: scope, mElement: element)
    }
  }

  /// Typed read, write and listen helpers over `AudioObjectGetPropertyData`
  /// and friends, in the AudioCap style: one call per property, errors as
  /// `CoreAudioError`.
  extension AudioObjectID {
    static let system = AudioObjectID(kAudioObjectSystemObject)
    static let unknown = AudioObjectID(kAudioObjectUnknown)

    var isValid: Bool { self != .unknown }

    func propertySize(_ address: AudioObjectPropertyAddress) throws -> UInt32 {
      var address = address
      var size: UInt32 = 0
      let status = AudioObjectGetPropertyDataSize(self, &address, 0, nil, &size)
      guard status == noErr else {
        throw CoreAudioError(
          operation: "size of \(fourCharCode(OSStatus(bitPattern: address.mSelector)))",
          objectID: self, status: status)
      }
      return size
    }

    /// Reads a fixed-size value. `T` must be a trivial type or a Core
    /// Foundation reference (`CFString`).
    func read<T>(_ address: AudioObjectPropertyAddress, defaultValue: T) throws -> T {
      var address = address
      var size = UInt32(MemoryLayout<T>.size)
      var value = defaultValue
      let status = withUnsafeMutablePointer(to: &value) { pointer in
        AudioObjectGetPropertyData(self, &address, 0, nil, &size, pointer)
      }
      guard status == noErr else {
        throw CoreAudioError(
          operation: "read \(fourCharCode(OSStatus(bitPattern: address.mSelector)))",
          objectID: self, status: status)
      }
      return value
    }

    /// Reads a fixed-size value with a qualifier (a PID, a UID).
    func read<T, Q>(_ address: AudioObjectPropertyAddress, qualifier: Q, defaultValue: T) throws
      -> T
    {
      var address = address
      var qualifier = qualifier
      var size = UInt32(MemoryLayout<T>.size)
      var value = defaultValue
      let status = withUnsafeMutablePointer(to: &qualifier) { qualifierPointer in
        withUnsafeMutablePointer(to: &value) { pointer in
          AudioObjectGetPropertyData(
            self, &address, UInt32(MemoryLayout<Q>.size), qualifierPointer, &size, pointer)
        }
      }
      guard status == noErr else {
        throw CoreAudioError(
          operation: "read \(fourCharCode(OSStatus(bitPattern: address.mSelector)))",
          objectID: self, status: status)
      }
      return value
    }

    func readString(_ address: AudioObjectPropertyAddress) throws -> String {
      try read(address, defaultValue: "" as CFString) as String
    }

    func readUInt32(_ address: AudioObjectPropertyAddress) throws -> UInt32 {
      try read(address, defaultValue: UInt32(0))
    }

    func readBool(_ address: AudioObjectPropertyAddress) throws -> Bool {
      try readUInt32(address) != 0
    }

    func readFloat64(_ address: AudioObjectPropertyAddress) throws -> Float64 {
      try read(address, defaultValue: Float64(0))
    }

    func readObjectID(_ address: AudioObjectPropertyAddress) throws -> AudioObjectID {
      try read(address, defaultValue: AudioObjectID.unknown)
    }

    /// Reads an array of trivial values (`[AudioObjectID]`).
    func readArray<T>(_ address: AudioObjectPropertyAddress, of type: T.Type) throws -> [T] {
      var address = address
      var size = try propertySize(address)
      let capacity = Int(size) / MemoryLayout<T>.stride
      guard capacity > 0 else { return [] }
      let raw = UnsafeMutablePointer<T>.allocate(capacity: capacity)
      defer { raw.deallocate() }
      let status = AudioObjectGetPropertyData(self, &address, 0, nil, &size, raw)
      guard status == noErr else {
        throw CoreAudioError(
          operation: "read \(fourCharCode(OSStatus(bitPattern: address.mSelector)))",
          objectID: self, status: status)
      }
      let count = Int(size) / MemoryLayout<T>.stride
      return Array(UnsafeBufferPointer(start: raw, count: count))
    }

    /// The channel count of every buffer in a variable-size `AudioBufferList`
    /// property such as `kAudioDevicePropertyStreamConfiguration`.
    func readBufferChannelCounts(_ address: AudioObjectPropertyAddress) throws -> [Int] {
      var address = address
      var size = try propertySize(address)
      guard size > 0 else { return [] }
      let raw = UnsafeMutableRawPointer.allocate(
        byteCount: Int(size), alignment: MemoryLayout<AudioBufferList>.alignment)
      defer { raw.deallocate() }
      let status = AudioObjectGetPropertyData(self, &address, 0, nil, &size, raw)
      guard status == noErr else {
        throw CoreAudioError(
          operation: "read \(fourCharCode(OSStatus(bitPattern: address.mSelector)))",
          objectID: self, status: status)
      }
      let list = UnsafeMutableAudioBufferListPointer(
        raw.assumingMemoryBound(to: AudioBufferList.self))
      return list.map { Int($0.mNumberChannels) }
    }

    func write<T>(_ address: AudioObjectPropertyAddress, value: T) throws {
      var address = address
      var value = value
      let status = withUnsafeMutablePointer(to: &value) { pointer in
        AudioObjectSetPropertyData(self, &address, 0, nil, UInt32(MemoryLayout<T>.size), pointer)
      }
      guard status == noErr else {
        throw CoreAudioError(
          operation: "write \(fourCharCode(OSStatus(bitPattern: address.mSelector)))",
          objectID: self, status: status)
      }
    }

    /// Registers `block` on `queue` for `address`; the returned token removes
    /// it. The block runs on `queue`, never on the IO thread.
    func addListener(
      _ address: AudioObjectPropertyAddress, queue: DispatchQueue,
      _ block: @escaping @Sendable () -> Void
    ) throws -> AudioPropertyListenerToken {
      var address = address
      let listener: AudioObjectPropertyListenerBlock = { _, _ in block() }
      let status = AudioObjectAddPropertyListenerBlock(self, &address, queue, listener)
      guard status == noErr else {
        throw CoreAudioError(
          operation: "listen \(fourCharCode(OSStatus(bitPattern: address.mSelector)))",
          objectID: self, status: status)
      }
      return AudioPropertyListenerToken(
        objectID: self, address: address, queue: queue, block: listener)
    }
  }

  /// One registered property listener; `remove()` unregisters it once.
  public final class AudioPropertyListenerToken: @unchecked Sendable {
    private let objectID: AudioObjectID
    private var address: AudioObjectPropertyAddress
    private let queue: DispatchQueue
    private let block: AudioObjectPropertyListenerBlock
    private let removed = NSLock()
    private var isRemoved = false

    init(
      objectID: AudioObjectID, address: AudioObjectPropertyAddress, queue: DispatchQueue,
      block: @escaping AudioObjectPropertyListenerBlock
    ) {
      self.objectID = objectID
      self.address = address
      self.queue = queue
      self.block = block
    }

    public func remove() {
      removed.lock()
      defer { removed.unlock() }
      guard !isRemoved else { return }
      isRemoved = true
      AudioObjectRemovePropertyListenerBlock(objectID, &address, queue, block)
    }

    deinit { remove() }
  }
#endif
