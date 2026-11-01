import AppKit

let destination = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
for points in [16, 32, 128, 256, 512] {
    for scale in [1, 2] {
        let pixels = points * scale
        let bitmap = NSBitmapImageRep(
            bitmapDataPlanes: nil, pixelsWide: pixels, pixelsHigh: pixels,
            bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true,
            isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0
        )!
        let context = NSGraphicsContext(bitmapImageRep: bitmap)!
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = context
        context.cgContext.scaleBy(x: CGFloat(pixels) / 1024, y: CGFloat(pixels) / 1024)
        NSColor(calibratedRed: 0.12, green: 0.17, blue: 0.24, alpha: 1).setFill()
        NSBezierPath(roundedRect: NSRect(x: 64, y: 64, width: 896, height: 896),
                     xRadius: 200, yRadius: 200).fill()
        NSColor(calibratedRed: 0.65, green: 0.91, blue: 0.82, alpha: 1).setStroke()
        let letter = NSBezierPath()
        letter.move(to: NSPoint(x: 312, y: 292))
        letter.line(to: NSPoint(x: 312, y: 732))
        letter.line(to: NSPoint(x: 712, y: 292))
        letter.line(to: NSPoint(x: 712, y: 732))
        letter.lineWidth = 100
        letter.lineJoinStyle = .round
        letter.lineCapStyle = .round
        letter.stroke()
        NSGraphicsContext.restoreGraphicsState()
        let suffix = scale == 2 ? "@2x" : ""
        let file = destination.appendingPathComponent("icon_\(points)x\(points)\(suffix).png")
        try bitmap.representation(using: .png, properties: [:])!.write(to: file)
    }
}
