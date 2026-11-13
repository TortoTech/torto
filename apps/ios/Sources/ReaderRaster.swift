import CoreGraphics
import Foundation
import torto_androidFFI

/// CGImage retains immutable Rust pixels; dropping the UniFFI page is safe.
struct ReaderRaster: @unchecked Sendable {
    let image: CGImage
    let id: UInt64

    init(_ page: MobilePage) throws {
        let width = Int(page.width()), height = Int(page.height())
        guard width > 0, height > 0, width <= 16_384, height <= 16_384 else {
            throw RasterError.invalidFrame
        }
        var bytes: UnsafePointer<UInt8>?
        var count = 0
        guard let token = torto_ios_retain_pixels(page.id(), &bytes, &count) else {
            throw RasterError.invalidFrame
        }
        guard let bytes, count == width * height * 4 else {
            torto_ios_release_pixels(token)
            throw RasterError.invalidFrame
        }
        guard let provider = CGDataProvider(dataInfo: UnsafeMutableRawPointer(mutating: token),
                                            data: bytes, size: count, releaseData: { token, _, _ in
            torto_ios_release_pixels(token)
        }) else {
            torto_ios_release_pixels(token)
            throw RasterError.invalidFrame
        }
        guard let image = CGImage(width: width, height: height, bitsPerComponent: 8, bitsPerPixel: 32,
                                  bytesPerRow: width * 4, space: CGColorSpace(name: CGColorSpace.sRGB)!,
                                  bitmapInfo: [.byteOrder32Big, CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue)],
                                  provider: provider, decode: nil, shouldInterpolate: true,
                                  intent: .defaultIntent) else { throw RasterError.invalidFrame }
        self.image = image
        self.id = page.rasterId()
    }

    enum RasterError: LocalizedError {
        case invalidFrame
        var errorDescription: String? { String(localized: "Unable to display this page.") }
    }
}
