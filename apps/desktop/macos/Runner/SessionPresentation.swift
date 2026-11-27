import AppKit
import CoreImage
import CoreVideo
import Metal

func configureSessionContent(_ window: NSWindow, controller: NSViewController, visibleFrame: NSRect?) {
  // AppKit resizes the window to the controller's view when this property is set.
  window.contentViewController = controller
  guard let visible = visibleFrame, !visible.isEmpty else {
    window.contentMinSize = NSSize(width: 640, height: 360)
    window.setContentSize(NSSize(width: 1060, height: 600))
    window.center()
    return
  }
  let available = window.contentRect(forFrameRect: visible).size
  let width = floor(min(1440, available.width * 0.92, available.height * 0.92 * 16 / 9))
  let size = NSSize(width: max(1, width), height: max(1, floor(width * 9 / 16)))
  window.contentMinSize = NSSize(width: min(640, size.width), height: min(360, size.height))
  window.setContentSize(size)
  window.setFrameOrigin(NSPoint(x: floor(visible.midX - window.frame.width / 2),
                                y: floor(visible.midY - window.frame.height / 2)))
}

/// Widths and heights here are backing pixels, independent of AppKit points.
struct VideoPresentation: Equatable {
  let sourceWidth, sourceHeight, targetWidth, targetHeight: Int
  let liftShadows: Bool

  static let standard = VideoPresentation(sourceWidth: 0, sourceHeight: 0, targetWidth: 0,
                                          targetHeight: 0, liftShadows: false)

  func enlarges(width: Int, height: Int) -> Bool {
    guard width == sourceWidth, height == sourceHeight, width > 0, height > 0,
          targetWidth > width, targetHeight > height, targetWidth <= 4096, targetHeight <= 4096,
          targetWidth * targetHeight <= 4096 * 2304 else { return false }
    // The two aspect-preserving dimensions may round independently by half a pixel.
    return abs(targetWidth * height - targetHeight * width) <= max(width, height)
  }
}

final class VideoBufferPool {
  private var pool: CVPixelBufferPool?
  private var dimensions = CGSize.zero

  func buffer(width: Int, height: Int) -> CVPixelBuffer? {
    let size = CGSize(width: width, height: height)
    if size != dimensions {
      let attributes: [String: Any] = [
        kCVPixelBufferWidthKey as String: width,
        kCVPixelBufferHeightKey as String: height,
        kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA,
        kCVPixelBufferMetalCompatibilityKey as String: true,
        kCVPixelBufferIOSurfacePropertiesKey as String: [String: Any](),
      ]
      var next: CVPixelBufferPool?
      guard CVPixelBufferPoolCreate(nil, nil, attributes as CFDictionary, &next) == kCVReturnSuccess
      else { return nil }
      pool = next
      dimensions = size
    }
    var result: CVPixelBuffer?
    let bound = [kCVPixelBufferPoolAllocationThresholdKey as String: 4] as CFDictionary
    guard let pool,
          CVPixelBufferPoolCreatePixelBufferWithAuxAttributes(nil, pool, bound, &result) == kCVReturnSuccess,
          let result else { return nil }
    CVBufferSetAttachment(result, kCVImageBufferCGColorSpaceKey, Self.sRGB, .shouldPropagate)
    return result
  }

  private static let sRGB = CGColorSpace(name: CGColorSpace.sRGB)!
}

final class MacVideoRenderer {
  private let output = VideoBufferPool()
  // libyuv already produced display-encoded RGB. Filtering these samples directly
  // preserves the SDR transfer function and the antialiasing of desktop glyphs.
  private static let context: CIContext = {
    let options: [CIContextOption: Any] = [
      .workingColorSpace: NSNull(), .outputColorSpace: NSNull(), .cacheIntermediates: false,
    ]
    if let device = MTLCreateSystemDefaultDevice() { return CIContext(mtlDevice: device, options: options) }
    return CIContext(options: options)
  }()

  // A single gain for R/G/B preserves neutral greys and the hue of dark colors.
  // The midpoint, highlights and both endpoints are identical to the standard view.
  private static let cubeSize = 33
  private static let shadowCube: Data = {
    let n = cubeSize
    var values: [Float] = []
    values.reserveCapacity(n * n * n * 4)
    for b in 0..<n {
      for g in 0..<n {
        for r in 0..<n {
          let peak = Float(max(r, g, b)) / Float(n - 1)
          let fade = max(0, 1 - 2 * peak)
          let gain = 1 + 1.35 * fade * fade * fade
          values.append(Float(r) / Float(n - 1) * gain)
          values.append(Float(g) / Float(n - 1) * gain)
          values.append(Float(b) / Float(n - 1) * gain)
          values.append(1)
        }
      }
    }
    return values.withUnsafeBytes { Data($0) }
  }()

  func present(_ source: CVPixelBuffer, as settings: VideoPresentation) -> CVPixelBuffer? {
    let w = CVPixelBufferGetWidth(source), h = CVPixelBufferGetHeight(source)
    let enlarge = settings.enlarges(width: w, height: h)
    guard enlarge || settings.liftShadows else { return source }
    let width = enlarge ? settings.targetWidth : w
    let height = enlarge ? settings.targetHeight : h
    guard let destination = output.buffer(width: width, height: height) else { return nil }
    var image = CIImage(cvPixelBuffer: source, options: [.colorSpace: NSNull()])
    let bounds = CGRect(x: 0, y: 0, width: width, height: height)
    if enlarge {
      let sy = CGFloat(height) / CGFloat(h)
      let sx = CGFloat(width) / CGFloat(w)
      image = image.clampedToExtent().applyingFilter("CILanczosScaleTransform", parameters: [
        kCIInputScaleKey: sy, kCIInputAspectRatioKey: sx / sy,
      ]).cropped(to: bounds)
    }
    if settings.liftShadows {
      image = image.applyingFilter("CIColorCube", parameters: [
        "inputCubeDimension": Self.cubeSize, "inputCubeData": Self.shadowCube,
      ])
    }
    Self.context.render(image, to: destination, bounds: bounds, colorSpace: nil)
    return destination
  }
}
