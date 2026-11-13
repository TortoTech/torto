import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

let args = CommandLine.arguments
guard args.count == 3,
      let source = CGImageSourceCreateWithURL(URL(fileURLWithPath: args[1]) as CFURL, nil),
      let image = CGImageSourceCreateImageAtIndex(source, 0, nil),
      let context = CGContext(data: nil, width: 1024, height: 1024, bitsPerComponent: 8,
                              bytesPerRow: 0, space: CGColorSpace(name: CGColorSpace.sRGB)!,
                              bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue)
else { fatalError("Could not load the Torto icon") }
context.setFillColor(red: 7 / 255, green: 193 / 255, blue: 96 / 255, alpha: 1)
context.fill(CGRect(x: 0, y: 0, width: 1024, height: 1024))
context.draw(image, in: CGRect(x: 0, y: 0, width: 1024, height: 1024))
guard let result = context.makeImage(),
      let output = CGImageDestinationCreateWithURL(URL(fileURLWithPath: args[2]) as CFURL,
                                                   UTType.png.identifier as CFString, 1, nil)
else { fatalError("Could not create the app icon") }
CGImageDestinationAddImage(output, result, nil)
guard CGImageDestinationFinalize(output) else { fatalError("Could not write the app icon") }
