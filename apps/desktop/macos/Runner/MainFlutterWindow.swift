import Cocoa
import CoreVideo
import FlutterMacOS
import ServiceManagement

class MainFlutterWindow: NSWindow {
  private var remoteTexture: RemoteTextureChannel?
  private var background: NexDeskBackground?
  private var sessions: SessionWindows?

  override func awakeFromNib() {
    let flutterViewController = FlutterViewController()
    let windowFrame = self.frame
    self.contentViewController = flutterViewController
    self.setFrame(windowFrame, display: true)
    self.tabbingMode = .disallowed

    RegisterGeneratedPlugins(registry: flutterViewController)
    remoteTexture = RemoteTextureChannel(flutterViewController.registrar(forPlugin: "NexDeskTexture"), window: self)
    let messenger = flutterViewController.engine.binaryMessenger
    let sessions = SessionWindows(messenger: messenger)
    self.sessions = sessions
    background = NexDeskBackground(window: self, messenger: messenger) { [weak sessions] in
      sessions?.hasWindows == true
    }

    super.awakeFromNib()
  }
}

private final class SessionWindows {
  private let channel: FlutterMethodChannel
  private var windows: [Int64: SessionFlutterWindow] = [:]

  var hasWindows: Bool { !windows.isEmpty }

  init(messenger: FlutterBinaryMessenger) {
    channel = FlutterMethodChannel(name: "dev.nexdesk/session_window", binaryMessenger: messenger)
    channel.setMethodCallHandler { [weak self] call, reply in
      guard let self else { reply(false); return }
      switch call.method {
      case "open":
        guard let values = call.arguments as? [String: Any],
              let id = (values["id"] as? NSNumber)?.int64Value, id > 0,
              let title = values["title"] as? String, !title.isEmpty, title.utf8.count <= 1024,
              let launch = values["launch"] as? String, !launch.isEmpty, launch.utf8.count <= 16384,
              self.windows[id] == nil else { reply(false); return }
        let window = SessionFlutterWindow(title: title, launch: launch) { [weak self] in
          guard let self else { return }
          self.windows.removeValue(forKey: id)
          self.channel.invokeMethod("closed", arguments: id)
        }
        self.windows[id] = window
        window.show()
        reply(true)
      case "focus", "requestClose":
        guard let id = (call.arguments as? NSNumber)?.int64Value,
              let window = self.windows[id] else { reply(false); return }
        window.show()
        if call.method == "requestClose" { window.requestClose() }
        reply(true)
      default:
        reply(FlutterMethodNotImplemented)
      }
    }
  }

  deinit { channel.setMethodCallHandler(nil) }
}

private final class SessionFlutterWindow: NSObject, NSWindowDelegate {
  private let window: NSWindow
  private let channel: FlutterMethodChannel
  private var remoteTexture: RemoteTextureChannel?
  private let onClosed: () -> Void

  init(title: String, launch: String, onClosed: @escaping () -> Void) {
    let project = FlutterDartProject()
    project.dartEntrypointArguments = ["--nexdesk-session-window", launch]
    let controller = FlutterViewController(project: project)
    let window = NSWindow(
      contentRect: NSRect(x: 0, y: 0, width: 1060, height: 720),
      styleMask: [.titled, .closable, .miniaturizable, .resizable],
      backing: .buffered, defer: false)
    self.window = window
    self.onClosed = onClosed
    channel = FlutterMethodChannel(name: "dev.nexdesk/session_window", binaryMessenger: controller.engine.binaryMessenger)
    super.init()
    window.title = title
    window.tabbingMode = .disallowed
    window.isReleasedWhenClosed = false
    window.contentViewController = controller
    window.delegate = self
    window.center()
    RegisterGeneratedPlugins(registry: controller)
    remoteTexture = RemoteTextureChannel(controller.registrar(forPlugin: "NexDeskTexture"), window: window)
    channel.setMethodCallHandler { [weak self] call, reply in
      guard call.method == "close" else { reply(FlutterMethodNotImplemented); return }
      reply(true)
      DispatchQueue.main.async { [weak self] in self?.window.close() }
    }
  }

  func show() {
    if window.isMiniaturized { window.deminiaturize(nil) }
    window.makeKeyAndOrderFront(nil)
    NSApp.activate(ignoringOtherApps: true)
  }

  func windowShouldClose(_ sender: NSWindow) -> Bool {
    channel.invokeMethod("requestClose", arguments: nil)
    return false
  }

  func requestClose() { window.performClose(nil) }

  func windowWillClose(_ notification: Notification) {
    channel.setMethodCallHandler(nil)
    remoteTexture = nil
    onClosed()
  }
}

private final class NexDeskBackground: NSObject, NSWindowDelegate {
  private weak var window: NSWindow?
  private let channel: FlutterMethodChannel
  private var item: NSStatusItem?
  private let hasSessions: () -> Bool

  init(window: NSWindow, messenger: FlutterBinaryMessenger, hasSessions: @escaping () -> Bool) {
    self.window = window
    self.hasSessions = hasSessions
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
      case "appVersion":
        reply(Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String)
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
    if item != nil {
      sender.orderOut(nil)
      return false
    }
    if hasSessions() {
      sender.miniaturize(nil)
      return false
    }
    return true
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
  typealias CopyBGRA = @convention(c) (UInt64, UInt32, UInt32, UnsafeMutableRawPointer, UInt32, Int) -> Bool

  let handle: UnsafeMutableRawPointer
  let sequence: Sequence
  let dimensions: Sequence
  let copyBGRA: CopyBGRA

  init?(_ path: String) {
    guard path.hasPrefix("/"), URL(fileURLWithPath: path).lastPathComponent == "libnexdesk_ffi.dylib",
          let library = dlopen(path, RTLD_NOW | RTLD_LOCAL) else { return nil }
    guard let sequence = dlsym(library, "nexdesk_m1_frame_sequence"),
          let dimensions = dlsym(library, "nexdesk_m1_frame_size"),
          let copyBGRA = dlsym(library, "nexdesk_m1_copy_bgra") else {
      dlclose(library)
      return nil
    }
    handle = library
    self.sequence = unsafeBitCast(sequence, to: Sequence.self)
    self.dimensions = unsafeBitCast(dimensions, to: Sequence.self)
    self.copyBGRA = unsafeBitCast(copyBGRA, to: CopyBGRA.self)
  }

  deinit { dlclose(handle) }
}

private final class RemoteTexture: NSObject, FlutterTexture {
  let backend: FrameLibrary
  let viewer: UInt64
  private var pool: CVPixelBufferPool?
  private var poolDimensions: UInt64 = 0

  init(_ backend: FrameLibrary, _ viewer: UInt64) {
    self.backend = backend
    self.viewer = viewer
  }

  func copyPixelBuffer() -> Unmanaged<CVPixelBuffer>? {
    let dimensions = backend.dimensions(viewer)
    let width = UInt32(dimensions >> 32)
    let height = UInt32(truncatingIfNeeded: dimensions)
    guard width > 0, height > 0, width <= 4096, height <= 4096 else { return nil }
    if poolDimensions != dimensions {
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
      poolDimensions = dimensions
    }
    var output: CVPixelBuffer?
    let bounds = [kCVPixelBufferPoolAllocationThresholdKey as String: 4] as CFDictionary
    guard let pool, CVPixelBufferPoolCreatePixelBufferWithAuxAttributes(nil, pool, bounds, &output) == kCVReturnSuccess,
          let image = output, CVPixelBufferLockBaseAddress(image, []) == kCVReturnSuccess else { return nil }
    defer { CVPixelBufferUnlockBaseAddress(image, []) }
    guard let destination = CVPixelBufferGetBaseAddress(image) else { return nil }
    let stride = CVPixelBufferGetBytesPerRow(image)
    guard backend.copyBGRA(viewer, width, height, destination, UInt32(stride), stride * Int(height))
    else { return nil }
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

  private func handle(_ call: FlutterMethodCall, _ result: @escaping FlutterResult) {
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
