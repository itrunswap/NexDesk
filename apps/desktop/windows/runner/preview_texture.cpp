#include "preview_texture.h"

#include <flutter/standard_method_codec.h>
#include <shobjidl.h>
#include <wrl/client.h>

#include <exception>
#include <filesystem>
#include <optional>
#include <string>
#include <utility>

#include "nexdesk_ffi.h"

namespace {

constexpr UINT_PTR kPreviewTimerId = 0x4E44;

// Each texture and every in-flight frame lease keep this module loaded.
struct Backend {
  explicit Backend(HMODULE handle) : module(handle) {}
  ~Backend() { ::FreeLibrary(module); }

  HMODULE module;
  decltype(&nexdesk_m1_frame_sequence) sequence = nullptr;
  decltype(&nexdesk_m1_acquire_frame) acquire = nullptr;
  decltype(&nexdesk_m1_release_frame) release = nullptr;
};

std::shared_ptr<Backend> LoadBackend(const std::string& path) {
  const auto absolute = std::filesystem::u8path(path);
  if (!absolute.is_absolute() || absolute.filename() != L"nexdesk_ffi.dll") {
    return nullptr;
  }
  const auto module = ::LoadLibraryExW(
      absolute.c_str(), nullptr,
      LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS);
  if (!module) {
    return nullptr;
  }
  auto backend = std::make_shared<Backend>(module);
  backend->sequence = reinterpret_cast<decltype(backend->sequence)>(
      ::GetProcAddress(module, "nexdesk_m1_frame_sequence"));
  backend->acquire = reinterpret_cast<decltype(backend->acquire)>(
      ::GetProcAddress(module, "nexdesk_m1_acquire_frame"));
  backend->release = reinterpret_cast<decltype(backend->release)>(
      ::GetProcAddress(module, "nexdesk_m1_release_frame"));
  if (!backend->sequence || !backend->acquire || !backend->release) {
    return nullptr;
  }
  return backend;
}

struct FrameLease {
  explicit FrameLease(std::shared_ptr<Backend> library)
      : backend(std::move(library)) {}

  ~FrameLease() {
    if (handle) {
      backend->release(handle);
    }
  }

  std::shared_ptr<Backend> backend;
  const void* handle = nullptr;
  FlutterDesktopPixelBuffer pixels{};
};

const FlutterDesktopPixelBuffer* CopyFrame(
    const std::shared_ptr<Backend>& backend, uint64_t viewer) {
  auto lease = std::make_unique<FrameLease>(backend);
  NexDeskPreviewFrame native{};
  lease->handle = backend->acquire(viewer, &native);
  if (!lease->handle) {
    return nullptr;
  }
  lease->pixels.buffer = native.pixels;
  lease->pixels.width = native.width;
  lease->pixels.height = native.height;
  lease->pixels.release_callback = [](void* context) {
    delete static_cast<FrameLease*>(context);
  };
  lease->pixels.release_context = lease.get();
  return &lease.release()->pixels;
}

std::optional<uint64_t> ViewerId(const flutter::EncodableValue& value) {
  if (const auto* id = std::get_if<int64_t>(&value); id && *id > 0) {
    return static_cast<uint64_t>(*id);
  }
  if (const auto* id = std::get_if<int32_t>(&value); id && *id > 0) {
    return static_cast<uint64_t>(*id);
  }
  return std::nullopt;
}

std::optional<std::string> SelectFolder(HWND window) {
  Microsoft::WRL::ComPtr<IFileOpenDialog> dialog;
  if (FAILED(::CoCreateInstance(__uuidof(FileOpenDialog), nullptr,
                                CLSCTX_INPROC_SERVER, IID_PPV_ARGS(&dialog)))) {
    return std::nullopt;
  }
  FILEOPENDIALOGOPTIONS options = 0;
  if (FAILED(dialog->GetOptions(&options)) ||
      FAILED(dialog->SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM)) ||
      FAILED(dialog->Show(window))) {
    return std::nullopt;
  }
  Microsoft::WRL::ComPtr<IShellItem> item;
  PWSTR path = nullptr;
  if (FAILED(dialog->GetResult(&item)) ||
      FAILED(item->GetDisplayName(SIGDN_FILESYSPATH, &path))) {
    return std::nullopt;
  }
  const std::unique_ptr<wchar_t, decltype(&::CoTaskMemFree)> value(path, ::CoTaskMemFree);
  return std::filesystem::path(value.get()).u8string();
}

}  // namespace

struct PreviewTexture::Texture {
  Texture(std::shared_ptr<Backend> library, uint64_t viewer_id)
      : backend(std::move(library)),
        viewer(viewer_id),
        variant(flutter::PixelBufferTexture(
            [library = backend, viewer_id](size_t, size_t) {
              return CopyFrame(library, viewer_id);
            })) {}

  std::shared_ptr<Backend> backend;
  uint64_t viewer;
  flutter::TextureVariant variant;
};

PreviewTexture::PreviewTexture(flutter::PluginRegistrarWindows* registrar,
                               HWND window)
    : window_(window),
      registrar_(registrar->texture_registrar()),
      channel_(registrar->messenger(), "dev.nexdesk/m1_texture",
               &flutter::StandardMethodCodec::GetInstance()) {
  channel_.SetMethodCallHandler([this](const auto& call, auto result) {
    if (call.method_name() == "fullscreen") {
      const auto* enabled = std::get_if<bool>(call.arguments());
      if (enabled && SetFullscreen(*enabled)) result->Success();
      else result->Error("window_mode_unavailable");
      return;
    }
    if (call.method_name() == "selectDirectory") {
      try {
        const auto selected = SelectFolder(window_);
        if (selected) result->Success(flutter::EncodableValue(*selected));
        else result->Success();
      } catch (const std::exception&) {
        result->Error("folder_picker_unavailable");
      }
      return;
    }
    if (call.method_name() == "detach") {
      if (texture_ && call.arguments() &&
          ViewerId(*call.arguments()) == texture_->viewer) {
        Reset();
      }
      result->Success();
      return;
    }
    if (call.method_name() != "attach") {
      result->NotImplemented();
      return;
    }
    const auto* args = std::get_if<flutter::EncodableMap>(call.arguments());
    if (!args) {
      result->Error("invalid_arguments");
      return;
    }
    const auto id_entry = args->find(flutter::EncodableValue("viewer"));
    const auto path_entry = args->find(flutter::EncodableValue("library"));
    if (id_entry == args->end() || path_entry == args->end()) {
      result->Error("invalid_arguments");
      return;
    }
    const auto id = ViewerId(id_entry->second);
    const auto* path = std::get_if<std::string>(&path_entry->second);
    if (!id || !path) {
      result->Error("invalid_arguments");
      return;
    }
    try {
      auto backend = LoadBackend(*path);
      if (!backend) {
        result->Error("native_library_unavailable");
        return;
      }
      Reset();
      texture_ = std::make_shared<Texture>(std::move(backend), *id);
      texture_id_ = registrar_->RegisterTexture(&texture_->variant);
      if (texture_id_ == -1) {
        Reset();
        result->Error("texture_registration_failed");
        return;
      }
      timer_ = ::SetTimer(window_, kPreviewTimerId, 16, nullptr);
      if (!timer_) {
        Reset();
        result->Error("texture_timer_failed");
        return;
      }
      result->Success(flutter::EncodableValue(texture_id_));
    } catch (const std::exception&) {
      Reset();
      result->Error("texture_initialization_failed");
    }
  });
}

bool PreviewTexture::SetFullscreen(bool enabled) {
  if (fullscreen_ == enabled) return true;
  if (enabled) {
    placement_.length = sizeof(placement_);
    MONITORINFO monitor{};
    monitor.cbSize = sizeof(monitor);
    if (!::GetWindowPlacement(window_, &placement_) ||
        !::GetMonitorInfoW(::MonitorFromWindow(window_, MONITOR_DEFAULTTONEAREST), &monitor)) {
      return false;
    }
    window_style_ = ::GetWindowLongPtrW(window_, GWL_STYLE);
    ::SetWindowLongPtrW(window_, GWL_STYLE, window_style_ & ~WS_OVERLAPPEDWINDOW);
    const RECT bounds = monitor.rcMonitor;
    if (!::SetWindowPos(window_, nullptr, bounds.left, bounds.top, bounds.right - bounds.left,
                        bounds.bottom - bounds.top, SWP_NOZORDER | SWP_FRAMECHANGED)) {
      ::SetWindowLongPtrW(window_, GWL_STYLE, window_style_);
      return false;
    }
  } else {
    ::SetWindowLongPtrW(window_, GWL_STYLE, window_style_);
    if (!::SetWindowPlacement(window_, &placement_) ||
        !::SetWindowPos(window_, nullptr, 0, 0, 0, 0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED)) {
      return false;
    }
  }
  fullscreen_ = enabled;
  return true;
}

PreviewTexture::~PreviewTexture() {
  channel_.SetMethodCallHandler(nullptr);
  Reset();
}

bool PreviewTexture::OnTimer(WPARAM timer) {
  if (!texture_ || timer_ == 0 || timer != timer_) {
    return false;
  }
  const uint64_t current = texture_->backend->sequence(texture_->viewer);
  if (current > 0 && current != sequence_) {
    registrar_->MarkTextureFrameAvailable(texture_id_);
    sequence_ = current;
  }
  return true;
}

void PreviewTexture::Reset() {
  if (timer_) {
    ::KillTimer(window_, timer_);
    timer_ = 0;
  }
  if (texture_id_ != -1) {
    registrar_->UnregisterTexture(texture_id_,
                                  [keep_alive = std::move(texture_)]() {});
    texture_id_ = -1;
  } else {
    texture_.reset();
  }
  sequence_ = 0;
}
