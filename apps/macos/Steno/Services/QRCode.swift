import AppKit
import CoreImage
import CoreImage.CIFilterBuiltins

/// Renders the pairing URL as a QR code image via `CIQRCodeGenerator`.
enum QRCode {
  static func image(for string: String, side: CGFloat = 240) -> NSImage? {
    let filter = CIFilter.qrCodeGenerator()
    filter.message = Data(string.utf8)
    filter.correctionLevel = "M"
    guard let output = filter.outputImage else { return nil }
    let scale = side / max(output.extent.width, 1)
    let scaled = output.transformed(by: CGAffineTransform(scaleX: scale, y: scale))
    let representation = NSCIImageRep(ciImage: scaled)
    let image = NSImage(size: representation.size)
    image.addRepresentation(representation)
    return image
  }
}
