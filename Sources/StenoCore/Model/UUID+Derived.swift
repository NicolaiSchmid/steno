import Foundation

extension UUID {
  /// A UUID derived from `base` and `salt`, stable across runs and machines,
  /// so rows the pipeline creates on the meeting's behalf keep their ids on
  /// re-runs: speakers (`speaker-<label>`), segments
  /// (`segment-<lane>-<index>`), the "me" speaker and participant, decisions
  /// (`decision-<index>`) and deliveries (`delivery-<destinationID>`). A
  /// version-4, variant-1 layout so it never collides with random UUIDs by
  /// shape.
  public init(derivedFrom base: UUID, salt: String) {
    var hash: UInt64 = 0xcbf2_9ce4_8422_2325
    for byte in Array(base.uuidString.utf8) + Array(salt.utf8) {
      hash ^= UInt64(byte)
      hash = hash &* 0x0000_0100_0000_01b3
    }
    var second = hash ^ 0x9e37_79b9_7f4a_7c15
    second = second &* 0xbf58_476d_1ce4_e5b9
    second ^= second >> 31
    let bytes =
      withUnsafeBytes(of: hash.bigEndian, Array.init)
      + withUnsafeBytes(of: second.bigEndian, Array.init)
    var uuid = uuid_t(
      bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7], bytes[8],
      bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15])
    uuid.6 = (uuid.6 & 0x0F) | 0x40
    uuid.8 = (uuid.8 & 0x3F) | 0x80
    self.init(uuid: uuid)
  }
}
