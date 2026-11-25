import Flutter
import UIKit
import WebKit

@main
@objc class AppDelegate: FlutterAppDelegate, FlutterImplicitEngineDelegate {
  override func application(
    _ application: UIApplication,
    didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?
  ) -> Bool {
    return super.application(application, didFinishLaunchingWithOptions: launchOptions)
  }

  func didInitializeImplicitFlutterEngine(_ engineBridge: FlutterImplicitEngineBridge) {
    GeneratedPluginRegistrant.register(with: engineBridge.pluginRegistry)
    guard let registrar = engineBridge.pluginRegistry.registrar(
      forPlugin: "NexDeskMobile"
    ) else { return }
    let profile = FlutterMethodChannel(
      name: "dev.nexdesk/mobile_profile", binaryMessenger: registrar.messenger()
    )
    profile.setMethodCallHandler { call, reply in
      switch call.method {
      case "getDirectory":
        reply(UserDefaults.standard.string(forKey: "nexdesk.directory"))
      case "saveDirectory":
        guard let value = call.arguments as? String, let origin = directoryOrigin(value) else {
          reply(FlutterError(code: "origin", message: "Invalid HTTPS origin.", details: nil))
          return
        }
        UserDefaults.standard.set(origin.absoluteString, forKey: "nexdesk.directory")
        reply(nil)
      default:
        reply(FlutterMethodNotImplemented)
      }
    }
    registrar.register(
      NexDeskWebFactory(messenger: registrar.messenger()), withId: "dev.nexdesk/mobile_web"
    )
  }
}

private func httpsComponents(_ string: String) -> URLComponents? {
  guard let parts = URLComponents(string: string), parts.scheme == "https",
    let host = parts.host, !host.isEmpty, parts.user == nil, parts.password == nil,
    parts.port.map({ (1...65535).contains($0) }) ?? true
  else { return nil }
  return parts
}

private func directoryOrigin(_ string: String) -> URL? {
  guard var parts = httpsComponents(string), parts.query == nil, parts.fragment == nil,
    parts.path.isEmpty || parts.path == "/"
  else { return nil }
  parts.path = ""
  if parts.port == 443 { parts.port = nil }
  return parts.url
}

private final class NexDeskWebFactory: NSObject, FlutterPlatformViewFactory {
  private let messenger: FlutterBinaryMessenger

  init(messenger: FlutterBinaryMessenger) {
    self.messenger = messenger
  }

  func createArgsCodec() -> FlutterMessageCodec & NSObjectProtocol {
    FlutterStandardMessageCodec.sharedInstance()
  }

  func create(
    withFrame frame: CGRect, viewIdentifier viewId: Int64, arguments args: Any?
  ) -> FlutterPlatformView {
    let settings = args as? [String: Any]
    let origin = (settings?["origin"] as? String).flatMap(directoryOrigin)
    return NexDeskWebView(frame: frame, id: viewId, origin: origin, messenger: messenger)
  }
}

private final class NexDeskWebView: NSObject, FlutterPlatformView, WKNavigationDelegate {
  private let container: UIView
  private let web: WKWebView
  private let errorLabel = UILabel()
  private let origin: URL?
  private let events: FlutterMethodChannel
  private var foreground = true

  init(frame: CGRect, id: Int64, origin: URL?, messenger: FlutterBinaryMessenger) {
    self.origin = origin
    self.container = UIView(frame: frame)
    let settings = WKWebViewConfiguration()
    settings.allowsInlineMediaPlayback = true
    settings.mediaTypesRequiringUserActionForPlayback = .audio
    settings.preferences.javaScriptCanOpenWindowsAutomatically = false
    self.web = WKWebView(frame: .zero, configuration: settings)
    self.events = FlutterMethodChannel(
      name: "dev.nexdesk/mobile_web/\(id)", binaryMessenger: messenger
    )
    super.init()

    web.navigationDelegate = self
    web.translatesAutoresizingMaskIntoConstraints = false
    container.addSubview(web)
    NSLayoutConstraint.activate([
      web.leadingAnchor.constraint(equalTo: container.leadingAnchor),
      web.trailingAnchor.constraint(equalTo: container.trailingAnchor),
      web.topAnchor.constraint(equalTo: container.topAnchor),
      web.bottomAnchor.constraint(equalTo: container.bottomAnchor),
    ])
    errorLabel.translatesAutoresizingMaskIntoConstraints = false
    errorLabel.textAlignment = .center
    errorLabel.textColor = .label
    errorLabel.numberOfLines = 0
    errorLabel.isHidden = true
    container.addSubview(errorLabel)
    NSLayoutConstraint.activate([
      errorLabel.leadingAnchor.constraint(equalTo: container.leadingAnchor, constant: 24),
      errorLabel.trailingAnchor.constraint(equalTo: container.trailingAnchor, constant: -24),
      errorLabel.centerYAnchor.constraint(equalTo: container.centerYAnchor),
    ])

    events.setMethodCallHandler { [weak self] call, reply in
      guard call.method == "foreground" else {
        reply(FlutterMethodNotImplemented)
        return
      }
      self?.foreground = call.arguments as? Bool ?? false
      self?.notifyActivity()
      reply(nil)
    }

    if let origin = origin,
      var entry = URLComponents(url: origin, resolvingAgainstBaseURL: false)
    {
      entry.path = "/"
      entry.queryItems = [URLQueryItem(name: "client", value: "mobile")]
      if let url = entry.url { web.load(URLRequest(url: url)) }
    } else {
      showError("目录地址无效。")
    }
  }

  func view() -> UIView { container }

  private func notifyActivity() {
    let event = foreground ? "nexdesk:resume" : "nexdesk:suspend"
    web.evaluateJavaScript("window.dispatchEvent(new Event('\(event)'));")
  }

  private func showError(_ text: String) {
    web.isHidden = true
    errorLabel.text = text
    errorLabel.isHidden = false
  }

  @available(iOS 13.0, *)
  func webView(
    _ webView: WKWebView, decidePolicyFor navigationAction: WKNavigationAction,
    decisionHandler: @escaping (WKNavigationActionPolicy) -> Void
  ) {
    guard navigationAction.targetFrame != nil,
      let request = navigationAction.request.url,
      let url = httpsComponents(request.absoluteString),
      let origin = origin,
      url.host?.lowercased() == origin.host?.lowercased(),
      (url.port ?? 443) == (origin.port ?? 443)
    else { decisionHandler(.cancel); return }
    decisionHandler(.allow)
  }

  func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
    notifyActivity()
  }

  func webView(
    _ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!,
    withError error: Error
  ) {
    if (error as NSError).code == NSURLErrorCancelled { return }
    showError("无法访问目录服务。请检查网络和 HTTPS 证书。")
  }

  func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
    showError("页面已暂停，请重新加载连接服务。")
  }

  deinit {
    events.setMethodCallHandler(nil)
    web.navigationDelegate = nil
    web.stopLoading()
  }
}
