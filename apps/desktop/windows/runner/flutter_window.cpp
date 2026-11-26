#include "flutter_window.h"

#include <shellapi.h>
#include <winver.h>

#include <array>
#include <optional>
#include <string>
#include <vector>

#include <flutter/standard_method_codec.h>

#include "flutter/generated_plugin_registrant.h"
#include "resource.h"

#pragma comment(lib, "version.lib")

namespace {
constexpr wchar_t kPreferences[] = L"Software\\NexDesk";
constexpr wchar_t kLoginItems[] = L"Software\\Microsoft\\Windows\\CurrentVersion\\Run";
constexpr UINT kTrayId = 1;
constexpr UINT kTrayEvent = WM_APP + 61;
constexpr UINT kCloseSession = WM_APP + 62;
constexpr UINT kSessionClosed = WM_APP + 63;
constexpr UINT kOpen = 1;
constexpr UINT kExit = 2;

bool TrayPreference() {
  DWORD value = 0;
  DWORD size = sizeof(value);
  return RegGetValueW(HKEY_CURRENT_USER, kPreferences, L"Tray",
                      RRF_RT_REG_DWORD, nullptr, &value, &size) == ERROR_SUCCESS &&
         value == 1;
}

bool StoreTrayPreference(bool enabled) {
  HKEY key = nullptr;
  if (RegCreateKeyExW(HKEY_CURRENT_USER, kPreferences, 0, nullptr, 0,
      KEY_SET_VALUE, nullptr, &key, nullptr) != ERROR_SUCCESS) return false;
  const DWORD value = enabled ? 1 : 0;
  const auto result = RegSetValueExW(key, L"Tray", 0, REG_DWORD,
      reinterpret_cast<const BYTE*>(&value), sizeof(value));
  RegCloseKey(key);
  return result == ERROR_SUCCESS;
}

std::wstring ExecutablePath() {
  std::array<wchar_t, 32768> path{};
  const DWORD length = GetModuleFileNameW(nullptr, path.data(),
                                         static_cast<DWORD>(path.size()));
  if (length == 0 || length >= path.size()) return {};
  return std::wstring(path.data(), length);
}

std::wstring StartupCommand() {
  const auto path = ExecutablePath();
  return path.empty() ? std::wstring{} : L"\"" + path + L"\"";
}

std::string ApplicationVersion() {
  const auto path = ExecutablePath();
  if (path.empty()) return {};
  const DWORD length = GetFileVersionInfoSizeW(path.c_str(), nullptr);
  if (length == 0) return {};
  std::vector<BYTE> data(length);
  if (!GetFileVersionInfoW(path.c_str(), 0, length, data.data())) return {};

  void* value = nullptr;
  UINT size = 0;
  if (!VerQueryValueW(data.data(), L"\\", &value, &size) || !value ||
      size < sizeof(VS_FIXEDFILEINFO)) return {};
  const auto* info = static_cast<const VS_FIXEDFILEINFO*>(value);
  if (info->dwSignature != VS_FFI_SIGNATURE) return {};
  return std::to_string(HIWORD(info->dwProductVersionMS)) + "." +
         std::to_string(LOWORD(info->dwProductVersionMS)) + "." +
         std::to_string(HIWORD(info->dwProductVersionLS));
}

bool StartsAtLogin() {
  std::array<wchar_t, 32768> value{};
  DWORD size = static_cast<DWORD>(value.size() * sizeof(wchar_t));
  return RegGetValueW(HKEY_CURRENT_USER, kLoginItems, L"NexDesk", RRF_RT_REG_SZ,
      nullptr, value.data(), &size) == ERROR_SUCCESS && StartupCommand() == value.data();
}

bool ConfigureStartup(bool enabled) {
  HKEY key = nullptr;
  if (RegCreateKeyExW(HKEY_CURRENT_USER, kLoginItems, 0, nullptr, 0,
      KEY_SET_VALUE, nullptr, &key, nullptr) != ERROR_SUCCESS) return false;
  LSTATUS status = ERROR_INVALID_PARAMETER;
  if (!enabled) {
    status = RegDeleteValueW(key, L"NexDesk");
    if (status == ERROR_FILE_NOT_FOUND) status = ERROR_SUCCESS;
  } else if (const auto command = StartupCommand(); !command.empty()) {
    status = RegSetValueExW(key, L"NexDesk", 0, REG_SZ,
        reinterpret_cast<const BYTE*>(command.c_str()),
        static_cast<DWORD>((command.size() + 1) * sizeof(wchar_t)));
  }
  RegCloseKey(key);
  return status == ERROR_SUCCESS;
}

int64_t SessionId(const flutter::EncodableValue* value) {
  if (!value) return 0;
  if (const auto* id = std::get_if<int64_t>(value)) return *id;
  if (const auto* id = std::get_if<int32_t>(value)) return *id;
  return 0;
}

std::wstring SessionTitle(const std::string& text) {
  if (text.empty() || text.size() > 1024) return {};
  const int length = static_cast<int>(text.size());
  const int count = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS,
                                        text.data(), length, nullptr, 0);
  if (count <= 0) return {};
  std::wstring output(count, L'\0');
  MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, text.data(), length,
                      output.data(), count);
  return output;
}
}  // namespace

FlutterWindow::FlutterWindow(const flutter::DartProject& project,
                             FlutterWindow* home, int64_t session_id)
    : project_(project), home_(home), session_id_(session_id) {}

FlutterWindow::~FlutterWindow() { Destroy(); }

bool FlutterWindow::OnCreate() {
  if (!Win32Window::OnCreate()) {
    return false;
  }

  RECT frame = GetClientArea();

  // The size here must match the window dimensions to avoid unnecessary surface
  // creation / destruction in the startup path.
  flutter_controller_ = std::make_unique<flutter::FlutterViewController>(
      frame.right - frame.left, frame.bottom - frame.top, project_);
  // Ensure that basic setup of the controller was successful.
  if (!flutter_controller_->engine() || !flutter_controller_->view()) {
    return false;
  }
  RegisterPlugins(flutter_controller_->engine());
  auto* registrar = flutter::PluginRegistrarManager::GetInstance()
                        ->GetRegistrar<flutter::PluginRegistrarWindows>(
                            flutter_controller_->engine()->GetRegistrarForPlugin("NexDeskM1"));
  preview_texture_ = std::make_unique<PreviewTexture>(registrar, GetHandle());
  if (!home_) {
    taskbar_created_ = RegisterWindowMessageW(L"TaskbarCreated");
    ConfigureTray(TrayPreference());
    RegisterBackgroundChannel();
  }
  RegisterSessionChannel();
  SetChildContent(flutter_controller_->view()->GetNativeWindow());

  flutter_controller_->engine()->SetNextFrameCallback([&]() {
    this->Show();
  });

  // Flutter can complete the first frame before the "show window" callback is
  // registered. The following call ensures a frame is pending to ensure the
  // window is shown. It is a no-op if the first frame hasn't completed yet.
  flutter_controller_->ForceRedraw();

  return true;
}

void FlutterWindow::OnDestroy() {
  if (home_ && session_channel_ && home_->GetHandle()) {
    PostMessageW(home_->GetHandle(), kSessionClosed,
                 static_cast<WPARAM>(session_id_), 0);
  }
  session_channel_.reset();
  sessions_.clear();
  ConfigureTray(false);
  background_channel_.reset();
  preview_texture_.reset();
  if (flutter_controller_) {
    flutter_controller_ = nullptr;
  }

  Win32Window::OnDestroy();
}

void FlutterWindow::RegisterSessionChannel() {
  session_channel_ = std::make_unique<flutter::MethodChannel<flutter::EncodableValue>>(
      flutter_controller_->engine()->messenger(), "dev.nexdesk/session_window",
      &flutter::StandardMethodCodec::GetInstance());
  session_channel_->SetMethodCallHandler([this](const auto& call, auto result) {
    const auto& method = call.method_name();
    bool success = false;
    if (!home_ && method == "open") {
      success = OpenSession(call.arguments());
    } else if (!home_ && (method == "focus" || method == "requestClose")) {
      success = FocusSession(SessionId(call.arguments()), method == "requestClose");
    } else if (home_ && method == "close") {
      // Let the channel reply before the message loop destroys this engine.
      success = PostMessageW(GetHandle(), kCloseSession, 0, 0) != 0;
    } else {
      result->NotImplemented();
      return;
    }
    result->Success(flutter::EncodableValue(success));
  });
}

bool FlutterWindow::OpenSession(const flutter::EncodableValue* arguments) {
  if (!arguments) return false;
  const auto* map = std::get_if<flutter::EncodableMap>(arguments);
  if (!map) return false;
  const auto id_entry = map->find(flutter::EncodableValue("id"));
  const auto title_entry = map->find(flutter::EncodableValue("title"));
  const auto launch_entry = map->find(flutter::EncodableValue("launch"));
  if (id_entry == map->end() || title_entry == map->end() ||
      launch_entry == map->end()) return false;
  const auto id = SessionId(&id_entry->second);
  const auto* title = std::get_if<std::string>(&title_entry->second);
  const auto* launch = std::get_if<std::string>(&launch_entry->second);
  if (id <= 0 || !title || !launch || launch->empty() || launch->size() > 16384 ||
      sessions_.count(id)) return false;
  const auto name = SessionTitle(*title);
  if (name.empty()) return false;

  flutter::DartProject project(project_);
  project.set_dart_entrypoint_arguments({"--nexdesk-session-window", *launch});
  auto window = std::make_unique<FlutterWindow>(project, this, id);
  const auto offset = 40 + static_cast<unsigned int>(sessions_.size() % 8) * 26;
  if (!window->Create(name, Point(offset, offset), Size(1060, 720))) return false;
  sessions_.emplace(id, std::move(window));
  return true;
}

bool FlutterWindow::FocusSession(int64_t id, bool request_close) {
  const auto item = sessions_.find(id);
  if (item == sessions_.end()) return false;
  const auto window = item->second->GetHandle();
  if (!window) return false;
  ShowWindow(window, IsIconic(window) ? SW_RESTORE : SW_SHOW);
  SetForegroundWindow(window);
  return !request_close || PostMessageW(window, WM_CLOSE, 0, 0) != 0;
}

bool FlutterWindow::ConfigureTray(bool enabled) {
  if (enabled == tray_enabled_) return true;
  NOTIFYICONDATAW data{};
  data.cbSize = sizeof(data);
  data.hWnd = GetHandle();
  data.uID = kTrayId;
  if (enabled) {
    data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    data.uCallbackMessage = kTrayEvent;
    data.hIcon = LoadIconW(GetModuleHandleW(nullptr), MAKEINTRESOURCEW(IDI_APP_ICON));
    wcscpy_s(data.szTip, L"NexDesk");
  }
  if (!Shell_NotifyIconW(enabled ? NIM_ADD : NIM_DELETE, &data) && enabled) {
    return false;
  }
  tray_enabled_ = enabled;
  return true;
}

void FlutterWindow::ShowTrayMenu(HWND window) {
  const HMENU menu = CreatePopupMenu();
  if (!menu) return;
  AppendMenuW(menu, MF_STRING, kOpen, L"打开 NexDesk");
  AppendMenuW(menu, MF_STRING, kExit, L"退出 NexDesk");
  POINT point{};
  GetCursorPos(&point);
  SetForegroundWindow(window);
  const UINT chosen = TrackPopupMenu(
      menu, TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
      point.x, point.y, 0, window, nullptr);
  DestroyMenu(menu);
  if (chosen == kOpen) {
    ShowWindow(window, SW_RESTORE);
    SetForegroundWindow(window);
  } else if (chosen == kExit) {
    DestroyWindow(window);
  }
}

void FlutterWindow::RegisterBackgroundChannel() {
  background_channel_ = std::make_unique<flutter::MethodChannel<flutter::EncodableValue>>(
      flutter_controller_->engine()->messenger(), "dev.nexdesk/background",
      &flutter::StandardMethodCodec::GetInstance());
  background_channel_->SetMethodCallHandler([this](const auto& call, auto result) {
    const auto& name = call.method_name();
    if (name == "appVersion") {
      const auto version = ApplicationVersion();
      if (version.empty()) result->Success();
      else result->Success(flutter::EncodableValue(version));
      return;
    }
    if (name == "status") {
      result->Success(flutter::EncodableValue(flutter::EncodableMap{
        {flutter::EncodableValue("traySupported"), flutter::EncodableValue(true)},
        {flutter::EncodableValue("trayEnabled"), flutter::EncodableValue(tray_enabled_)},
        {flutter::EncodableValue("startupSupported"), flutter::EncodableValue(true)},
        {flutter::EncodableValue("startupEnabled"), flutter::EncodableValue(StartsAtLogin())},
      }));
      return;
    }
    const bool* enabled = call.arguments() ? std::get_if<bool>(call.arguments()) : nullptr;
    if (!enabled) {
      result->Error("argument");
    } else if (name == "setStartup") {
      if (ConfigureStartup(*enabled)) result->Success();
      else result->Error("startup");
    } else if (name == "setTray") {
      const bool previous = tray_enabled_;
      if (ConfigureTray(*enabled) && StoreTrayPreference(*enabled)) result->Success();
      else {
        ConfigureTray(previous);
        result->Error("tray");
      }
    } else {
      result->NotImplemented();
    }
  });
}

LRESULT
FlutterWindow::MessageHandler(HWND hwnd, UINT const message,
                              WPARAM const wparam,
                              LPARAM const lparam) noexcept {
  if (message == kCloseSession && home_) {
    DestroyWindow(hwnd);
    return 0;
  }
  if (message == kSessionClosed && !home_) {
    const auto id = static_cast<int64_t>(wparam);
    const auto item = sessions_.find(id);
    if (item != sessions_.end() && !item->second->GetHandle()) {
      sessions_.erase(item);
      if (session_channel_) {
        session_channel_->InvokeMethod(
            "closed", std::make_unique<flutter::EncodableValue>(id));
      }
    }
    return 0;
  }
  if (message == WM_CLOSE) {
    if (home_ && session_channel_) {
      session_channel_->InvokeMethod("requestClose", nullptr);
      return 0;
    }
    if (!home_ && (tray_enabled_ || !sessions_.empty())) {
      ShowWindow(hwnd, tray_enabled_ ? SW_HIDE : SW_MINIMIZE);
      return 0;
    }
  }
  if (message == kTrayEvent && wparam == kTrayId) {
    if (lparam == WM_RBUTTONUP) ShowTrayMenu(hwnd);
    if (lparam == WM_LBUTTONDBLCLK) {
      ShowWindow(hwnd, SW_RESTORE);
      SetForegroundWindow(hwnd);
    }
    return 0;
  }
  if (taskbar_created_ != 0 && message == taskbar_created_ && tray_enabled_) {
    tray_enabled_ = false;
    if (!ConfigureTray(true)) ShowWindow(hwnd, SW_RESTORE);
    return 0;
  }
  if (message == WM_TIMER && preview_texture_ &&
      preview_texture_->OnTimer(wparam)) {
    return 0;
  }
  // Give Flutter, including plugins, an opportunity to handle window messages.
  if (flutter_controller_) {
    std::optional<LRESULT> result =
        flutter_controller_->HandleTopLevelWindowProc(hwnd, message, wparam,
                                                      lparam);
    if (result) {
      return *result;
    }
  }

  switch (message) {
    case WM_FONTCHANGE:
      if (flutter_controller_) flutter_controller_->engine()->ReloadSystemFonts();
      break;
  }

  return Win32Window::MessageHandler(hwnd, message, wparam, lparam);
}
