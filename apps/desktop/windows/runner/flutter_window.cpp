#include "flutter_window.h"

#include <shellapi.h>

#include <array>
#include <optional>
#include <string>

#include <flutter/standard_method_codec.h>

#include "flutter/generated_plugin_registrant.h"
#include "resource.h"

namespace {
constexpr wchar_t kPreferences[] = L"Software\\NexDesk";
constexpr wchar_t kLoginItems[] = L"Software\\Microsoft\\Windows\\CurrentVersion\\Run";
constexpr UINT kTrayId = 1;
constexpr UINT kTrayEvent = WM_APP + 61;
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

std::wstring StartupCommand() {
  std::array<wchar_t, 32768> path{};
  const DWORD length = GetModuleFileNameW(nullptr, path.data(),
                                         static_cast<DWORD>(path.size()));
  if (length == 0 || length >= path.size()) return {};
  return L"\"" + std::wstring(path.data(), length) + L"\"";
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
}  // namespace

FlutterWindow::FlutterWindow(const flutter::DartProject& project)
    : project_(project) {}

FlutterWindow::~FlutterWindow() {}

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
  taskbar_created_ = RegisterWindowMessageW(L"TaskbarCreated");
  ConfigureTray(TrayPreference());
  RegisterBackgroundChannel();
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
  ConfigureTray(false);
  background_channel_.reset();
  preview_texture_.reset();
  if (flutter_controller_) {
    flutter_controller_ = nullptr;
  }

  Win32Window::OnDestroy();
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
  if (message == WM_CLOSE && tray_enabled_) {
    ShowWindow(hwnd, SW_HIDE);
    return 0;
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
      flutter_controller_->engine()->ReloadSystemFonts();
      break;
  }

  return Win32Window::MessageHandler(hwnd, message, wparam, lparam);
}
