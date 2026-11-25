package dev.nexdesk.nexdesk_desktop

import android.app.Activity
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.net.http.SslError
import android.view.Gravity
import android.view.View
import android.webkit.PermissionRequest
import android.webkit.RenderProcessGoneDetail
import android.webkit.SslErrorHandler
import android.webkit.ValueCallback
import android.webkit.WebChromeClient
import android.webkit.WebResourceError
import android.webkit.WebResourceRequest
import android.webkit.WebView
import android.webkit.WebViewClient
import android.widget.FrameLayout
import android.widget.TextView
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.BinaryMessenger
import io.flutter.plugin.common.MethodChannel
import io.flutter.plugin.common.StandardMessageCodec
import io.flutter.plugin.platform.PlatformView
import io.flutter.plugin.platform.PlatformViewFactory
import java.net.URI

class MainActivity : FlutterActivity() {
    private var chooser: ValueCallback<Array<Uri>>? = null

    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        val messenger = flutterEngine.dartExecutor.binaryMessenger
        val profile = getSharedPreferences("nexdesk_mobile", Context.MODE_PRIVATE)
        MethodChannel(messenger, "dev.nexdesk/mobile_profile").setMethodCallHandler { call, reply ->
            when (call.method) {
                "getDirectory" -> reply.success(profile.getString("directory", null))
                "saveDirectory" -> {
                    val origin = (call.arguments as? String)?.let(::directoryOrigin)
                    if (origin == null) reply.error("origin", "Invalid HTTPS origin.", null)
                    else {
                        profile.edit().putString("directory", origin.toString()).apply()
                        reply.success(null)
                    }
                }
                else -> reply.notImplemented()
            }
        }
        flutterEngine.platformViewsController.registry.registerViewFactory(
            "dev.nexdesk/mobile_web",
            object : PlatformViewFactory(StandardMessageCodec.INSTANCE) {
                override fun create(context: Context, id: Int, args: Any?): PlatformView {
                    val origin = ((args as? Map<*, *>)?.get("origin") as? String)
                        ?.let(::directoryOrigin)
                    return ControllerView(context, messenger, id, origin, ::chooseFile)
                }
            },
        )
    }

    @Suppress("DEPRECATION")
    private fun chooseFile(callback: ValueCallback<Array<Uri>>, intent: Intent): Boolean {
        chooser?.onReceiveValue(null)
        chooser = callback
        return try {
            startActivityForResult(intent, FILE_PICKER)
            true
        } catch (_: android.content.ActivityNotFoundException) {
            chooser = null
            callback.onReceiveValue(null)
            true
        }
    }

    @Deprecated("The WebView file chooser returns an activity result.")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        if (requestCode != FILE_PICKER) {
            super.onActivityResult(requestCode, resultCode, data)
            return
        }
        val files = if (resultCode == Activity.RESULT_OK) {
            WebChromeClient.FileChooserParams.parseResult(resultCode, data)
        } else null
        val callback = chooser
        chooser = null
        callback?.onReceiveValue(files)
    }

    override fun onDestroy() {
        chooser?.onReceiveValue(null)
        chooser = null
        super.onDestroy()
    }

    private companion object {
        const val FILE_PICKER = 1407
    }
}

private fun httpsUrl(value: String): URI? = runCatching { URI(value) }.getOrNull()?.takeIf {
    it.scheme == "https" && it.host != null && it.rawUserInfo == null &&
        (it.port == -1 || it.port in 1..65535)
}

private fun directoryOrigin(value: String): URI? {
    val url = httpsUrl(value) ?: return null
    if (url.rawPath !in listOf("", "/") || url.rawQuery != null || url.rawFragment != null) {
        return null
    }
    val port = if (url.port == 443) -1 else url.port
    return URI("https", null, url.host, port, null, null, null)
}

private fun sameOrigin(origin: URI?, candidate: String): Boolean {
    val url = httpsUrl(candidate) ?: return false
    return origin != null && origin.host.equals(url.host, ignoreCase = true) &&
        (if (origin.port == -1) 443 else origin.port) ==
        (if (url.port == -1) 443 else url.port)
}

private class ControllerView(
    context: Context,
    messenger: BinaryMessenger,
    id: Int,
    origin: URI?,
    chooseFile: (ValueCallback<Array<Uri>>, Intent) -> Boolean,
) : PlatformView {
    private val panel = FrameLayout(context)
    private val web = WebView(context)
    private val message = TextView(context)
    private val events = MethodChannel(messenger, "dev.nexdesk/mobile_web/$id")
    private var foreground = true
    private var rendererGone = false

    init {
        panel.addView(web, FrameLayout.LayoutParams(-1, -1))
        message.gravity = Gravity.CENTER
        message.setPadding(24, 24, 24, 24)
        message.visibility = View.GONE
        panel.addView(message, FrameLayout.LayoutParams(-1, -1))
        web.settings.javaScriptEnabled = true
        web.settings.domStorageEnabled = true
        web.settings.allowFileAccess = false
        web.settings.allowContentAccess = false
        web.settings.javaScriptCanOpenWindowsAutomatically = false
        web.settings.mediaPlaybackRequiresUserGesture = false
        web.webViewClient = object : WebViewClient() {
            override fun shouldOverrideUrlLoading(view: WebView, request: WebResourceRequest) =
                !sameOrigin(origin, request.url.toString())

            override fun onPageFinished(view: WebView, url: String) {
                if (sameOrigin(origin, url)) notifyActivity()
            }

            override fun onReceivedError(
                view: WebView,
                request: WebResourceRequest,
                error: WebResourceError,
            ) {
                if (request.isForMainFrame) showError("无法访问目录服务。请检查地址和网络。")
            }

            override fun onReceivedSslError(view: WebView, handler: SslErrorHandler, error: SslError) {
                handler.cancel()
                if (sameOrigin(origin, error.url)) {
                    showError("目录证书未受信任。请核对域名和设备的证书设置。")
                }
            }

            override fun onRenderProcessGone(view: WebView, detail: RenderProcessGoneDetail): Boolean {
                rendererGone = true
                panel.removeView(web)
                web.destroy()
                showError("页面已暂停，请重新加载连接服务。")
                return true
            }
        }
        web.webChromeClient = object : WebChromeClient() {
            override fun onPermissionRequest(request: PermissionRequest) {
                request.deny()
            }

            override fun onShowFileChooser(
                view: WebView,
                callback: ValueCallback<Array<Uri>>,
                parameters: FileChooserParams,
            ): Boolean = chooseFile(callback, parameters.createIntent())
        }
        events.setMethodCallHandler { call, reply ->
            if (call.method == "foreground") {
                foreground = call.arguments == true
                notifyActivity()
                reply.success(null)
            } else reply.notImplemented()
        }
        if (origin == null) showError("目录地址无效。")
        else web.loadUrl("$origin/?client=mobile")
    }

    private fun notifyActivity() {
        if (rendererGone) return
        val event = if (foreground) "nexdesk:resume" else "nexdesk:suspend"
        web.evaluateJavascript("window.dispatchEvent(new Event('$event'));", null)
    }

    private fun showError(text: String) {
        if (!rendererGone) web.visibility = View.INVISIBLE
        message.text = text
        message.visibility = View.VISIBLE
    }

    override fun getView(): View = panel

    override fun dispose() {
        events.setMethodCallHandler(null)
        if (!rendererGone) {
            web.stopLoading()
            panel.removeView(web)
            web.destroy()
        }
    }
}
