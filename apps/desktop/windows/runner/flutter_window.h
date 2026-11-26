#ifndef RUNNER_FLUTTER_WINDOW_H_
#define RUNNER_FLUTTER_WINDOW_H_

#include <flutter/dart_project.h>
#include <flutter/encodable_value.h>
#include <flutter/flutter_view_controller.h>
#include <flutter/method_channel.h>

#include <cstdint>
#include <map>
#include <memory>

#include "win32_window.h"
#include "preview_texture.h"

// A window that does nothing but host a Flutter view.
class FlutterWindow : public Win32Window {
 public:
  // Creates a new FlutterWindow hosting a Flutter view running |project|.
  explicit FlutterWindow(const flutter::DartProject& project,
                         FlutterWindow* home = nullptr, int64_t session_id = 0);
  virtual ~FlutterWindow();

 protected:
  // Win32Window:
  bool OnCreate() override;
  void OnDestroy() override;
  LRESULT MessageHandler(HWND window, UINT const message, WPARAM const wparam,
                         LPARAM const lparam) noexcept override;

 private:
  bool ConfigureTray(bool enabled);
  void ShowTrayMenu(HWND window);
  void RegisterBackgroundChannel();
  void RegisterSessionChannel();
  bool OpenSession(const flutter::EncodableValue* arguments);
  bool FocusSession(int64_t id, bool request_close = false);
  // The project to run.
  flutter::DartProject project_;

  // The Flutter instance hosted by this window.
  std::unique_ptr<flutter::FlutterViewController> flutter_controller_;
  std::unique_ptr<PreviewTexture> preview_texture_;
  std::unique_ptr<flutter::MethodChannel<flutter::EncodableValue>>
      background_channel_;
  std::unique_ptr<flutter::MethodChannel<flutter::EncodableValue>>
      session_channel_;
  FlutterWindow* const home_;
  const int64_t session_id_;
  std::map<int64_t, std::unique_ptr<FlutterWindow>> sessions_;
  bool tray_enabled_ = false;
  UINT taskbar_created_ = 0;
};

#endif  // RUNNER_FLUTTER_WINDOW_H_
