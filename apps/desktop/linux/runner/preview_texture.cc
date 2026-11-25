#include "preview_texture.h"

#include <dlfcn.h>

#include "nexdesk_ffi.h"

struct RemoteTexture {
  FlPixelBufferTexture parent;
  void* library;
  decltype(&nexdesk_m1_acquire_frame) acquire;
  decltype(&nexdesk_m1_release_frame) release;
  decltype(&nexdesk_m1_frame_sequence) sequence;
  const void* lease;
  uint64_t viewer;
};

struct RemoteTextureClass { FlPixelBufferTextureClass parent_class; };

G_DEFINE_TYPE(RemoteTexture, remote_texture, fl_pixel_buffer_texture_get_type())

static gboolean copy_pixels(FlPixelBufferTexture* object, const uint8_t** pixels,
                            uint32_t* width, uint32_t* height, GError**) {
  auto* texture = reinterpret_cast<RemoteTexture*>(object);
  NexDeskPreviewFrame frame{};
  const auto lease = texture->acquire(texture->viewer, &frame);
  if (texture->lease) texture->release(texture->lease);
  texture->lease = lease;
  if (!lease) {
    static constexpr uint8_t black[] = {0, 0, 0, 255};
    *pixels = black;
    *width = 1;
    *height = 1;
    return TRUE;
  }
  *pixels = frame.pixels;
  *width = frame.width;
  *height = frame.height;
  return TRUE;
}

static void free_texture(GObject* object) {
  auto* texture = reinterpret_cast<RemoteTexture*>(object);
  if (texture->lease) texture->release(texture->lease);
  if (texture->library) dlclose(texture->library);
  G_OBJECT_CLASS(remote_texture_parent_class)->finalize(object);
}

static void remote_texture_init(RemoteTexture*) {}

static void remote_texture_class_init(RemoteTextureClass* type) {
  FL_PIXEL_BUFFER_TEXTURE_CLASS(type)->copy_pixels = copy_pixels;
  G_OBJECT_CLASS(type)->finalize = free_texture;
}

namespace {
class Bridge {
 public:
  Bridge(FlTextureRegistrar* registrar, FlView* view)
      : registrar_(FL_TEXTURE_REGISTRAR(g_object_ref(registrar))), view_(view) {}
  ~Bridge() { Reset(); g_object_unref(registrar_); }

  void Handle(FlMethodCall* call) {
    const auto* method = fl_method_call_get_name(call);
    auto* args = fl_method_call_get_args(call);
    if (g_strcmp0(method, "fullscreen") == 0 || g_strcmp0(method, "selectDirectory") == 0) {
      auto* owner = gtk_widget_get_toplevel(GTK_WIDGET(view_));
      if (!GTK_IS_WINDOW(owner)) {
        fl_method_call_respond_error(call, "window_unavailable", nullptr, nullptr, nullptr);
        return;
      }
      if (g_strcmp0(method, "fullscreen") == 0) {
        if (!args || fl_value_get_type(args) != FL_VALUE_TYPE_BOOL) {
          fl_method_call_respond_error(call, "invalid_arguments", nullptr, nullptr, nullptr);
          return;
        }
        if (fl_value_get_bool(args)) gtk_window_fullscreen(GTK_WINDOW(owner));
        else gtk_window_unfullscreen(GTK_WINDOW(owner));
        fl_method_call_respond_success(call, nullptr, nullptr);
      } else {
        auto* picker = gtk_file_chooser_native_new("选择目录", GTK_WINDOW(owner),
            GTK_FILE_CHOOSER_ACTION_SELECT_FOLDER, "打开", "取消");
        g_autoptr(FlValue) selected = nullptr;
        if (gtk_native_dialog_run(GTK_NATIVE_DIALOG(picker)) == GTK_RESPONSE_ACCEPT) {
          g_autofree gchar* path = gtk_file_chooser_get_filename(GTK_FILE_CHOOSER(picker));
          g_autofree gchar* utf8 = path ? g_filename_to_utf8(path, -1, nullptr, nullptr, nullptr) : nullptr;
          if (utf8) selected = fl_value_new_string(utf8);
        }
        g_object_unref(picker);
        fl_method_call_respond_success(call, selected, nullptr);
      }
      return;
    }
    if (g_strcmp0(method, "detach") == 0) {
      if (texture_ && args && fl_value_get_type(args) == FL_VALUE_TYPE_INT &&
          fl_value_get_int(args) == static_cast<int64_t>(texture_->viewer)) Reset();
      fl_method_call_respond_success(call, nullptr, nullptr);
      return;
    }
    if (g_strcmp0(method, "attach") != 0) {
      fl_method_call_respond_not_implemented(call, nullptr);
      return;
    }
    if (!args || fl_value_get_type(args) != FL_VALUE_TYPE_MAP) {
      fl_method_call_respond_error(call, "invalid_arguments", nullptr, nullptr, nullptr);
      return;
    }
    const auto viewer = fl_value_lookup_string(args, "viewer");
    const auto library = fl_value_lookup_string(args, "library");
    if (!viewer || fl_value_get_type(viewer) != FL_VALUE_TYPE_INT || fl_value_get_int(viewer) < 1 ||
        !library || fl_value_get_type(library) != FL_VALUE_TYPE_STRING) {
      fl_method_call_respond_error(call, "invalid_arguments", nullptr, nullptr, nullptr);
      return;
    }
    const auto path = fl_value_get_string(library);
    g_autofree gchar* name = g_path_get_basename(path);
    if (!g_path_is_absolute(path) || g_strcmp0(name, "libnexdesk_ffi.so") != 0) {
      fl_method_call_respond_error(call, "native_library_unavailable", nullptr, nullptr, nullptr);
      return;
    }
    auto* candidate = reinterpret_cast<RemoteTexture*>(g_object_new(remote_texture_get_type(), nullptr));
    candidate->library = dlopen(path, RTLD_NOW | RTLD_LOCAL);
    candidate->viewer = static_cast<uint64_t>(fl_value_get_int(viewer));
    if (candidate->library) {
      candidate->acquire = reinterpret_cast<decltype(candidate->acquire)>(dlsym(candidate->library, "nexdesk_m1_acquire_frame"));
      candidate->release = reinterpret_cast<decltype(candidate->release)>(dlsym(candidate->library, "nexdesk_m1_release_frame"));
      candidate->sequence = reinterpret_cast<decltype(candidate->sequence)>(dlsym(candidate->library, "nexdesk_m1_frame_sequence"));
    }
    if (!candidate->acquire || !candidate->release || !candidate->sequence ||
        !fl_texture_registrar_register_texture(registrar_, FL_TEXTURE(candidate))) {
      g_object_unref(candidate);
      fl_method_call_respond_error(call, "texture_registration_failed", nullptr, nullptr, nullptr);
      return;
    }
    Reset();
    texture_ = candidate;
    timer_ = g_timeout_add(16, [](gpointer context) -> gboolean {
      static_cast<Bridge*>(context)->Advance();
      return G_SOURCE_CONTINUE;
    }, this);
    g_autoptr(FlValue) id = fl_value_new_int(fl_texture_get_id(FL_TEXTURE(candidate)));
    fl_method_call_respond_success(call, id, nullptr);
  }

 private:
  void Reset() {
    if (timer_) { g_source_remove(timer_); timer_ = 0; }
    if (texture_) {
      fl_texture_registrar_unregister_texture(registrar_, FL_TEXTURE(texture_));
      g_object_unref(texture_);
      texture_ = nullptr;
    }
    sequence_ = 0;
  }

  void Advance() {
    const auto current = texture_->sequence(texture_->viewer);
    if (current != 0 && current != sequence_) {
      sequence_ = current;
      fl_texture_registrar_mark_texture_frame_available(registrar_, FL_TEXTURE(texture_));
    }
  }

  FlTextureRegistrar* registrar_;
  FlView* view_;
  RemoteTexture* texture_ = nullptr;
  guint timer_ = 0;
  uint64_t sequence_ = 0;
};
}  // namespace

void nexdesk_register_texture_bridge(FlView* view) {
  g_autoptr(FlPluginRegistrar) registrar = fl_plugin_registry_get_registrar_for_plugin(FL_PLUGIN_REGISTRY(view), "NexDeskTexture");
  g_autoptr(FlStandardMethodCodec) codec = fl_standard_method_codec_new();
  auto* channel = fl_method_channel_new(fl_plugin_registrar_get_messenger(registrar), "dev.nexdesk/m1_texture", FL_METHOD_CODEC(codec));
  auto* bridge = new Bridge(fl_plugin_registrar_get_texture_registrar(registrar), view);
  fl_method_channel_set_method_call_handler(channel, [](FlMethodChannel*, FlMethodCall* call, gpointer data) {
    static_cast<Bridge*>(data)->Handle(call);
  }, bridge, [](gpointer data) { delete static_cast<Bridge*>(data); });
  g_object_set_data_full(G_OBJECT(view), "nexdesk-texture-channel", channel, g_object_unref);
}
