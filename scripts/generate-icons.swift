// Render the limited SVG vocabulary used by our original icon with native APIs.
import AppKit
import Foundation

final class IconRenderer: NSObject, XMLParserDelegate {
    let context: CGContext
    init(_ context: CGContext) { self.context = context }

    func parser(_ parser: XMLParser, didStartElement name: String,
                namespaceURI: String?, qualifiedName: String?,
                attributes a: [String: String]) {
        func number(_ key: String) -> CGFloat { CGFloat(Double(a[key]!)!) }
        func color(_ value: String) -> CGColor {
            let hex = UInt32(value.dropFirst(), radix: 16)!
            return CGColor(colorSpace: CGColorSpace(name: CGColorSpace.sRGB)!,
                components: [CGFloat((hex >> 16) & 255) / 255,
                             CGFloat((hex >> 8) & 255) / 255,
                             CGFloat(hex & 255) / 255, 1])!
        }
        let path: CGPath
        switch name {
        case "svg", "title": return
        case "rect":
            path = CGPath(roundedRect: CGRect(x: number("x"), y: number("y"),
                width: number("width"), height: number("height")),
                cornerWidth: number("rx"), cornerHeight: number("rx"), transform: nil)
        case "circle":
            let r = number("r")
            path = CGPath(ellipseIn: CGRect(x: number("cx") - r, y: number("cy") - r,
                width: 2 * r, height: 2 * r), transform: nil)
        case "polyline":
            precondition(a["stroke-linecap"] == "round" && a["stroke-linejoin"] == "round")
            let points = a["points"]!.split(separator: " ").map { pair -> CGPoint in
                let xy = pair.split(separator: ",").map { Double($0)! }
                return CGPoint(x: xy[0], y: xy[1])
            }
            let line = CGMutablePath()
            line.addLines(between: points)
            path = line
        default: fatalError("Unsupported SVG element: \(name)")
        }
        context.addPath(path)
        if let stroke = a["stroke"] {
            context.setStrokeColor(color(stroke))
            context.setLineWidth(number("stroke-width"))
            context.setLineCap(.round)
            context.setLineJoin(.round)
            context.strokePath()
        } else {
            context.setFillColor(color(a["fill"]!))
            context.fillPath()
        }
    }
}

let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
let assets = root.appendingPathComponent("assets/icons")
let source = try Data(contentsOf: assets.appendingPathComponent("herdr.svg"))
let temporary = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
let iconset = temporary.appendingPathComponent("Herdr.iconset")
try FileManager.default.createDirectory(at: iconset, withIntermediateDirectories: true)
defer { try? FileManager.default.removeItem(at: temporary) }

for size in [16, 32, 128, 256, 512] {
    for scale in [1, 2] {
        let pixels = size * scale
        let context = CGContext(data: nil, width: pixels, height: pixels,
            bitsPerComponent: 8, bytesPerRow: pixels * 4,
            space: CGColorSpace(name: CGColorSpace.sRGB)!,
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
        context.translateBy(x: 0, y: CGFloat(pixels))
        context.scaleBy(x: CGFloat(pixels) / 1024, y: -CGFloat(pixels) / 1024)
        let renderer = IconRenderer(context)
        let parser = XMLParser(data: source)
        parser.delegate = renderer
        precondition(parser.parse(), "Invalid icon SVG")
        let bitmap = NSBitmapImageRep(cgImage: context.makeImage()!)
        let png = bitmap.representation(using: .png, properties: [:])!
        let suffix = scale == 2 ? "@2x" : ""
        try png.write(to: iconset.appendingPathComponent("icon_\(size)x\(size)\(suffix).png"))
        if pixels == 1024 {
            try png.write(to: assets.appendingPathComponent("herdr-1024.png"))
        }
    }
}
let iconutil = Process()
iconutil.executableURL = URL(fileURLWithPath: "/usr/bin/iconutil")
iconutil.arguments = ["-c", "icns", iconset.path, "-o", assets.appendingPathComponent("Herdr.icns").path]
try iconutil.run()
iconutil.waitUntilExit()
precondition(iconutil.terminationStatus == 0, "iconutil failed")
print("Generated assets/icons/herdr-1024.png and assets/icons/Herdr.icns")
