import Cocoa
import Accelerate
import FlutterMacOS
import ServiceManagement

class MainFlutterWindow: NSWindow {
  private var remoteTexture: RemoteTextureChannel?
  private var background: NexDeskBackground?

  override func awakeFromNib() {
    let flutterViewController = FlutterViewController()
    let windowFrame = self.frame
    self.contentViewController = flutterViewController
    self.setFrame(windowFrame, display: true)

    RegisterGeneratedPlugins(registry: flutterViewController)
    remoteTexture = RemoteTextureChannel(flutterViewController.registrar(forPlugin: "NexDeskTexture"), window: self)
    background = NexDeskBackground(window: self, messenger: flutterViewController.engine.binaryMessenger)

    super.awakeFromNib()
  }
}

private final class NexDeskBackground: NSObject, NSWindowDelegate {
  private weak var window: NSWindow?
  private let channel: FlutterMethodChannel
  private var item: NSStatusItem?

  init(window: NSWindow, messenger: FlutterBinaryMessenger) {
    self.window = window
    self.channel = FlutterMethodChannel(name: "dev.nexdesk/background", binaryMessenger: messenger)
    super.init()
    window.delegate = self
    configureTray(UserDefaults.standard.bool(forKey: "nexdesk.tray"))
    channel.setMethodCallHandler { [weak self] call, reply in
      guard let self = self else {
        reply(FlutterMethodNotImplemented)
        return
      }
      switch call.method {
      case "status":
        var supported = false
        var enabled = false
        if #available(macOS 13.0, *) {
          supported = true
          enabled = SMAppService.mainApp.status == .enabled
        }
        reply([
          "traySupported": true, "trayEnabled": self.item != nil,
          "startupSupported": supported, "startupEnabled": enabled,
        ])
      case "setTray":
        guard let enabled = call.arguments as? Bool else {
          reply(FlutterError(code: "argument", message: nil, details: nil))
          return
        }
        self.configureTray(enabled)
        UserDefaults.standard.set(enabled, forKey: "nexdesk.tray")
        reply(nil)
      case "setStartup":
        guard let enabled = call.arguments as? Bool, #available(macOS 13.0, *) else {
          reply(FlutterError(code: "unavailable", message: nil, details: nil))
          return
        }
        do {
          let service = SMAppService.mainApp
          if enabled && service.status != .enabled {
            try service.register()
            if service.status == .requiresApproval {
              SMAppService.openSystemSettingsLoginItems()
            }
          } else if !enabled && service.status != .notRegistered {
            try service.unregister()
          }
          reply(nil)
        } catch {
          reply(FlutterError(code: "startup", message: nil, details: nil))
        }
      default:
        reply(FlutterMethodNotImplemented)
      }
    }
  }

  private func configureTray(_ enabled: Bool) {
    if !enabled {
      if let item = item { NSStatusBar.system.removeStatusItem(item) }
      item = nil
      return
    }
    guard item == nil else { return }
    let status = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
    status.button?.title = "NexDesk"
    let menu = NSMenu()
    let open = menu.addItem(withTitle: "打开 NexDesk", action: #selector(showWindow), keyEquivalent: "")
    open.target = self
    let exit = menu.addItem(withTitle: "退出 NexDesk", action: #selector(exitApp), keyEquivalent: "q")
    exit.target = self
    status.menu = menu
    item = status
  }

  func windowShouldClose(_ sender: NSWindow) -> Bool {
    guard item != nil else { return true }
    sender.orderOut(nil)
    return false
  }

  @objc private func showWindow() {
    window?.makeKeyAndOrderFront(nil)
    NSApp.activate(ignoringOtherApps: true)
  }

  @objc private func exitApp() { NSApp.terminate(nil) }

  deinit {
    channel.setMethodCallHandler(nil)
    if let item = item { NSStatusBar.system.removeStatusItem(item) }
  }
}

private final class FrameLibrary {
  typealias Sequence = @convention(c) (UInt64) -> UInt64
  typealias Acquire = @convention(c) (UInt64, UnsafeMutableRawPointer) -> UnsafeRawPointer?
  typealias Release = @convention(c) (UnsafeRawPointer) -> Void

  let handle: UnsafeMutableRawPointer
  let sequence: Sequence
  let acquire: Acquire
  let release: Release

  init?(_ path: String) {
    guard path.hasPrefix("/"), URL(fileURLWithPath: path).lastPathComponent == "libnexdesk_ffi.dylib",
          let library = dlopen(path, RTLD_NOW | RTLD_LOCAL) else { return nil }
    guard let sequence = dlsym(library, "nexdesk_m1_frame_sequence"),
          let acquire = dlsym(library, "nexdesk_m1_acquire_frame"),
          let release = dlsym(library, "nexdesk_m1_release_frame") else {
      dlclose(library)
      return nil
    }
    handle = library
    self.sequence = unsafeBitCast(sequence, to: Sequence.self)
    self.acquire = unsafeBitCast(acquire, to: Acquire.self)
    self.release = unsafeBitCast(release, to: Release.self)
  }

  deinit { dlclose(handle) }
}

private final class RemoteTexture: NSObject, FlutterTexture {
  let backend: FrameLibrary
  let viewer: UInt64

  init(_ backend: FrameLibrary, _ viewer: UInt64) {
    self.backend = backend
    self.viewer = viewer
  }

  func copyPixelBuffer() -> Unmanaged<CVPixelBuffer>? {
    // C layout is a pointer followed by two uint32_t dimensions.
    let pointerSize = MemoryLayout<UnsafeRawPointer>.size
    let info = UnsafeMutableRawPointer.allocate(byteCount: pointerSize + 8, alignment: pointerSize)
    defer { info.deallocate() }
    guard let lease = backend.acquire(viewer, info) else { return nil }
    defer { backend.release(lease) }
    let pixels = info.load(as: UnsafeMutableRawPointer.self)
    let width = Int(info.load(fromByteOffset: pointerSize, as: UInt32.self))
    let height = Int(info.load(fromByteOffset: pointerSize + 4, as: UInt32.self))
    guard width > 0, height > 0, width <= 4096, height <= 4096 else { return nil }

    var output: CVPixelBuffer?
    let options: [String: Any] = [kCVPixelBufferIOSurfacePropertiesKey as String: [String: Any]()]
    guard CVPixelBufferCreate(kCFAllocatorDefault, width, height, kCVPixelFormatType_32BGRA,
                              options as CFDictionary, &output) == kCVReturnSuccess,
          let image = output, CVPixelBufferLockBaseAddress(image, []) == kCVReturnSuccess else { return nil }
    defer { CVPixelBufferUnlockBaseAddress(image, []) }
    guard let destination = CVPixelBufferGetBaseAddress(image) else { return nil }
    var source = vImage_Buffer(data: pixels, height: vImagePixelCount(height), width: vImagePixelCount(width), rowBytes: width * 4)
    var target = vImage_Buffer(data: destination, height: vImagePixelCount(height), width: vImagePixelCount(width), rowBytes: CVPixelBufferGetBytesPerRow(image))
    let order: [UInt8] = [2, 1, 0, 3]
    guard vImagePermuteChannels_ARGB8888(&source, &target, order, vImage_Flags(kvImageNoFlags)) == kvImageNoError else { return nil }
    return Unmanaged.passRetained(image)
  }
}

private final class RemoteTextureChannel {
  private let registry: FlutterTextureRegistry
  private let channel: FlutterMethodChannel
  private var texture: RemoteTexture?
  private var textureId: Int64 = 0
  private var sequence: UInt64 = 0
  private var timer: Timer?
  private weak var window: NSWindow?

  init(_ registrar: FlutterPluginRegistrar, window: NSWindow) {
    self.window = window
    registry = registrar.textures
    channel = FlutterMethodChannel(name: "dev.nexdesk/m1_texture", binaryMessenger: registrar.messenger)
    channel.setMethodCallHandler { [weak self] call, result in self?.handle(call, result) }
  }

  deinit {
    channel.setMethodCallHandler(nil)
    reset()
  }

  private func handle(_ call: FlutterMethodCall, _ result: FlutterResult) {
    if call.method == "fullscreen" {
      guard let target = call.arguments as? Bool, let window else {
        result(FlutterError(code: "window_mode_unavailable", message: nil, details: nil))
        return
      }
      if window.styleMask.contains(.fullScreen) != target { window.toggleFullScreen(nil) }
      result(nil)
    } else if call.method == "selectDirectory" {
      guard let window else { result(nil); return }
      let panel = NSOpenPanel()
      panel.canChooseFiles = false
      panel.canChooseDirectories = true
      panel.allowsMultipleSelection = false
      panel.beginSheetModal(for: window) { response in
        result(response == .OK ? panel.url?.path : nil)
      }
    } else if call.method == "detach" {
      if let viewer = call.arguments as? NSNumber, viewer.uint64Value == texture?.viewer { reset() }
      result(nil)
    } else if call.method == "attach" {
      guard let args = call.arguments as? [String: Any], let viewer = args["viewer"] as? NSNumber,
            viewer.int64Value > 0, let library = args["library"] as? String,
            let backend = FrameLibrary(library) else {
        result(FlutterError(code: "native_library_unavailable", message: nil, details: nil))
        return
      }
      reset()
      let source = RemoteTexture(backend, viewer.uint64Value)
      textureId = registry.register(source)
      guard textureId != 0 else {
        result(FlutterError(code: "texture_registration_failed", message: nil, details: nil))
        return
      }
      texture = source
      let timer = Timer(timeInterval: 0.016, repeats: true) { [weak self] _ in self?.advance() }
      RunLoop.main.add(timer, forMode: .common)
      self.timer = timer
      result(textureId)
    } else {
      result(FlutterMethodNotImplemented)
    }
  }

  private func advance() {
    guard let texture else { return }
    let next = texture.backend.sequence(texture.viewer)
    if next > 0 && next != sequence {
      sequence = next
      registry.textureFrameAvailable(textureId)
    }
  }

  private func reset() {
    timer?.invalidate()
    timer = nil
    if textureId != 0 { registry.unregisterTexture(textureId) }
    texture = nil
    textureId = 0
    sequence = 0
  }
}
