#ifndef RUNNER_PREVIEW_TEXTURE_H_
#define RUNNER_PREVIEW_TEXTURE_H_

#include <flutter/method_channel.h>
#include <flutter/plugin_registrar_windows.h>
#include <windows.h>

#include <memory>

class PreviewTexture {
 public:
  PreviewTexture(flutter::PluginRegistrarWindows* registrar, HWND window);
  ~PreviewTexture();

  PreviewTexture(const PreviewTexture&) = delete;
  PreviewTexture& operator=(const PreviewTexture&) = delete;

  bool OnTimer(WPARAM timer);

 private:
  struct Texture;
  void Reset();
  bool SetFullscreen(bool enabled);

  HWND window_;
  flutter::TextureRegistrar* registrar_;
  flutter::MethodChannel<flutter::EncodableValue> channel_;
  std::shared_ptr<Texture> texture_;
  int64_t texture_id_ = -1;
  uint64_t sequence_ = 0;
  UINT_PTR timer_ = 0;
  LONG_PTR window_style_ = 0;
  WINDOWPLACEMENT placement_{};
  bool fullscreen_ = false;
};

#endif  // RUNNER_PREVIEW_TEXTURE_H_
