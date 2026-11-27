import AppKit
import CoreVideo

@main
enum CheckMacPresentation {
  static func main() {
    _ = NSApplication.shared
    for screen in [NSRect(x: 0, y: 24, width: 1440, height: 850),
                   NSRect(x: -1280, y: 140, width: 1280, height: 700),
                   NSRect(x: 100, y: 50, width: 600, height: 360)] {
      let window = NSWindow(contentRect: .zero, styleMask: [.titled, .closable, .resizable],
                            backing: .buffered, defer: false)
      let controller = NSViewController()
      controller.view = NSView(frame: .zero)
      configureSessionContent(window, controller: controller, visibleFrame: screen)
      let size = window.contentRect(forFrameRect: window.frame).size
      precondition(screen.contains(window.frame))
      precondition(size.width >= min(500, screen.width * 0.8))
      precondition(window.contentMinSize.width <= size.width && window.contentMinSize.height <= size.height)
      precondition(controller.view.frame.size == size && abs(size.width - size.height * 16 / 9) < 2)
      print("usable screen \(screen), initial content \(size) points")
    }

    let width = 320, height = 48
    let levels: [UInt8] = [0, 8, 17, 32, 48, 64, 96, 128, 224, 255]
    let pool = VideoBufferPool()
    let source = pool.buffer(width: width, height: height)!
    precondition(CVPixelBufferLockBaseAddress(source, []) == kCVReturnSuccess)
    let base = CVPixelBufferGetBaseAddress(source)!.assumingMemoryBound(to: UInt8.self)
    let stride = CVPixelBufferGetBytesPerRow(source)
    for y in 0..<height {
      for x in 0..<width {
        let offset = y * stride + x * 4
        let level: UInt8 = y < height / 2 ? levels[x / 32] : (x < 160 ? 32 : 224)
        for c in 0..<3 { base[offset + c] = level }
        if y >= height / 2 && x < 80 {
          base[offset] = 90; base[offset + 1] = 60; base[offset + 2] = 30
        }
        base[offset + 3] = 255
      }
    }
    CVPixelBufferUnlockBaseAddress(source, [])

    let renderer = MacVideoRenderer()
    let original = renderer.present(source, as: .standard)!
    precondition(original === source)
    let fit = VideoPresentation(sourceWidth: width, sourceHeight: height,
                                targetWidth: 480, targetHeight: 72, liftShadows: false)
    precondition(!fit.enlarges(width: 640, height: 360))
    precondition(!VideoPresentation(sourceWidth: width, sourceHeight: height, targetWidth: 500,
                                    targetHeight: 72, liftShadows: false).enlarges(width: width, height: height))
    let drawn = renderer.present(source, as: fit)!
    precondition(CVPixelBufferGetWidth(drawn) == 480 && CVPixelBufferGetHeight(drawn) == 72)
    let lifted = renderer.present(source, as: VideoPresentation(
      sourceWidth: width, sourceHeight: height, targetWidth: 480, targetHeight: 72, liftShadows: true))!

    var maximumError = 0, previous = -1
    for (i, level) in levels.enumerated() {
      let x = i * 48 + 24
      let normal = pixel(drawn, x, 15), liftedTone = pixel(lifted, x, 15)
      maximumError = max(maximumError, normal.prefix(3).map { abs($0 - Int(level)) }.max()!)
      precondition(normal[3] == 255 && liftedTone[3] == 255)
      precondition(liftedTone[0] == liftedTone[1] && liftedTone[1] == liftedTone[2])
      precondition(liftedTone[1] >= previous)
      if level == 17 { precondition((29...34).contains(liftedTone[1])) }
      if level == 0 || level >= 128 { precondition(abs(liftedTone[1] - Int(level)) <= 1) }
      previous = liftedTone[1]
    }
    let color = pixel(drawn, 40, 55)
    precondition(zip(color, [90, 60, 30, 255]).allSatisfy { abs($0.0 - $0.1) <= 1 })
    let edge = pixel(drawn, 240, 55)[1] - pixel(drawn, 239, 55)[1]
    // At this pixel phase ordinary bilinear interpolation gives 128 out of 192.
    precondition(maximumError <= 1 && edge > 128)
    print("SDR grey max error \(maximumError)/255, 1.5x text edge \(edge)/192, lifted 17 -> \(pixel(lifted, 120, 15)[1])")
  }

  static func pixel(_ image: CVPixelBuffer, _ x: Int, _ y: Int) -> [Int] {
    precondition(CVPixelBufferLockBaseAddress(image, .readOnly) == kCVReturnSuccess)
    defer { CVPixelBufferUnlockBaseAddress(image, .readOnly) }
    let ptr = CVPixelBufferGetBaseAddress(image)!.assumingMemoryBound(to: UInt8.self)
    let offset = y * CVPixelBufferGetBytesPerRow(image) + x * 4
    return (0..<4).map { Int(ptr[offset + $0]) }
  }
}
