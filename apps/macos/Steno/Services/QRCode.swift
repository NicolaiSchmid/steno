import AppKit
import CoreImage
import CoreImage.CIFilterBuiltins

/// Renders the pairing URL as a QR code via `CIQRCodeGenerator`.
enum QRCode {
  /// The code as a PNG, each module `scale` pixels wide with nearest
  /// sampling so the edges stay crisp; the page scales it further with
  /// `image-rendering: pixelated`. Nil when the string is too long to encode.
  static func png(for string: String, scale: CGFloat = 8) -> Data? {
    guard let output = code(for: string) else { return nil }
    let scaled = output.samplingNearest().transformed(
      by: CGAffineTransform(scaleX: scale, y: scale))
    return CIContext().pngRepresentation(
      of: scaled, format: .RGBA8, colorSpace: CGColorSpaceCreateDeviceRGB())
  }

  private static func code(for string: String) -> CIImage? {
    let filter = CIFilter.qrCodeGenerator()
    filter.message = Data(string.utf8)
    filter.correctionLevel = "M"
    return filter.outputImage
  }
}
