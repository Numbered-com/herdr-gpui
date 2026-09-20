// Build the native icon from the supplied raster export, preserving SVG effects.
import AppKit
import CoreImage
import Foundation

let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
let assets = root.appendingPathComponent("assets/icons")
let source = try Data(contentsOf: assets.appendingPathComponent("herdr-1024.png"))
guard let image = NSBitmapImageRep(data: source)?.cgImage,
  image.width == 1024, image.height == 1024
else {
  fatalError("Expected a 1024x1024 PNG icon")
}
let temporary = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
try FileManager.default.createDirectory(at: temporary, withIntermediateDirectories: true)
defer { try? FileManager.default.removeItem(at: temporary) }

// Map luminance to a saturated red palette while retaining the original alpha
// and shading. The supplied stable PNG remains unchanged.
let red = CIImage(cgImage: image).applyingFilter(
  "CIColorMatrix",
  parameters: [
    "inputRVector": CIVector(x: 0.1382, y: 0.4649, z: 0.0469, w: 0),
    "inputGVector": CIVector(x: 0.0255, y: 0.0858, z: 0.0087, w: 0),
    "inputBVector": CIVector(x: 0.0255, y: 0.0858, z: 0.0087, w: 0),
    "inputAVector": CIVector(x: 0, y: 0, z: 0, w: 1),
    "inputBiasVector": CIVector(x: 0.25, y: 0.02, z: 0.035, w: 0),
  ])
guard let redImage = CIContext().createCGImage(red, from: red.extent) else {
  fatalError("Unable to generate the red worktree icon")
}
let redPNG = NSBitmapImageRep(cgImage: redImage).representation(using: .png, properties: [:])!
try redPNG.write(to: assets.appendingPathComponent("herdr-worktree-1024.png"))

for (name, image, source) in [("Herdr", image, source), ("Herdr-worktree", redImage, redPNG)] {
  let iconset = temporary.appendingPathComponent("\(name).iconset")
  try FileManager.default.createDirectory(at: iconset, withIntermediateDirectories: true)
  for size in [16, 32, 128, 256, 512] {
    for scale in [1, 2] {
      let pixels = size * scale
      let context = CGContext(
        data: nil, width: pixels, height: pixels,
        bitsPerComponent: 8, bytesPerRow: pixels * 4,
        space: CGColorSpace(name: CGColorSpace.sRGB)!,
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
      context.interpolationQuality = .high
      context.draw(image, in: CGRect(x: 0, y: 0, width: pixels, height: pixels))
      let bitmap = NSBitmapImageRep(cgImage: context.makeImage()!)
      let png = pixels == 1024 ? source : bitmap.representation(using: .png, properties: [:])!
      let suffix = scale == 2 ? "@2x" : ""
      try png.write(to: iconset.appendingPathComponent("icon_\(size)x\(size)\(suffix).png"))
    }
  }
  let iconutil = Process()
  iconutil.executableURL = URL(fileURLWithPath: "/usr/bin/iconutil")
  iconutil.arguments = [
    "-c", "icns", iconset.path, "-o", assets.appendingPathComponent("\(name).icns").path,
  ]
  try iconutil.run()
  iconutil.waitUntilExit()
  precondition(iconutil.terminationStatus == 0, "iconutil failed")
  print("Generated assets/icons/\(name).icns")
}
