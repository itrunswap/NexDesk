#include "background.h"

#include <gdk/gdk.h>
#ifdef GDK_WINDOWING_X11
#include <gdk/gdkx.h>
#endif
#include <glib/gstdio.h>

#include <cerrno>
#include <cstring>
#include <string>

namespace {
std::string StartupCommand() {
  g_autofree gchar* path = g_file_read_link("/proc/self/exe", nullptr);
  if (!path || !g_path_is_absolute(path) || strpbrk(path, "\r\n")) return {};
  std::string command = "\"";
  for (const char* ch = path; *ch; ++ch) {
    if (strchr("\"\\$`", *ch)) command.push_back('\\');
    if (*ch == '%') command.push_back('%');
    command.push_back(*ch);
  }
  command.push_back('"');
  return command;
}

bool RemoveFile(const gchar* path) {
  return g_remove(path) == 0 || errno == ENOENT;
}

bool Startup(bool enabled, bool update) {
  g_autofree gchar* path = g_build_filename(
      g_get_user_config_dir(), "autostart", "dev.nexdesk.desktop.desktop", nullptr);
  const auto command = StartupCommand();
  if (command.empty()) return false;
  g_autoptr(GKeyFile) file = g_key_file_new();
  if (!update) {
    if (!g_key_file_load_from_file(file, path, G_KEY_FILE_NONE, nullptr)) return false;
    g_autofree gchar* existing = g_key_file_get_string(file, "Desktop Entry", "Exec", nullptr);
    return existing && command == existing;
  }
  if (!enabled) return RemoveFile(path);
  g_autofree gchar* directory = g_path_get_dirname(path);
  if (g_mkdir_with_parents(directory, 0700) != 0) return false;
  g_key_file_set_string(file, "Desktop Entry", "Type", "Application");
  g_key_file_set_string(file, "Desktop Entry", "Name", "NexDesk");
  g_key_file_set_string(file, "Desktop Entry", "Exec", command.c_str());
  return g_key_file_save_to_file(file, path, nullptr);
}

bool TrayPreference(bool enabled, bool update) {
  g_autofree gchar* path = g_build_filename(
      g_get_user_config_dir(), "nexdesk", "tray-enabled", nullptr);
  if (!update) return g_file_test(path, G_FILE_TEST_IS_REGULAR);
  if (!enabled) return RemoveFile(path);
  g_autofree gchar* directory = g_path_get_dirname(path);
  return g_mkdir_with_parents(directory, 0700) == 0 &&
      g_file_set_contents(path, "1\n", 2, nullptr);
}

G_GNUC_BEGIN_IGNORE_DEPRECATIONS
class Background {
 public:
  Background(FlView* view, GtkWindow* window) : window_(window) {
    g_signal_connect(window_, "delete-event", G_CALLBACK(Close), this);
    g_autoptr(FlStandardMethodCodec) codec = fl_standard_method_codec_new();
    channel_ = fl_method_channel_new(
        fl_engine_get_binary_messenger(fl_view_get_engine(view)),
        "dev.nexdesk/background", FL_METHOD_CODEC(codec));
    fl_method_channel_set_method_call_handler(channel_, Handle, this, nullptr);
    if (TraySupported() && TrayPreference(false, false)) ConfigureTray(true);
  }

  ~Background() {
    fl_method_channel_set_method_call_handler(channel_, nullptr, nullptr, nullptr);
    g_clear_object(&channel_);
    ConfigureTray(false);
  }

 private:
  bool TraySupported() const {
#ifdef GDK_WINDOWING_X11
    return GDK_IS_X11_SCREEN(gtk_window_get_screen(window_));
#else
    return false;
#endif
  }

  bool TrayVisible() const { return icon_ && gtk_status_icon_is_embedded(icon_); }

  void ConfigureTray(bool enabled) {
    if (enabled && !icon_) {
      icon_ = gtk_status_icon_new_from_icon_name("video-display");
      gtk_status_icon_set_tooltip_text(icon_, "NexDesk");
      g_signal_connect(icon_, "activate", G_CALLBACK(Activate), this);
      g_signal_connect(icon_, "popup-menu", G_CALLBACK(Popup), this);
      g_signal_connect(icon_, "notify::embedded", G_CALLBACK(Embedded), this);
    } else if (!enabled && icon_) {
      g_signal_handlers_disconnect_by_data(icon_, this);
      gtk_status_icon_set_visible(icon_, FALSE);
      g_clear_object(&icon_);
    }
  }

  static gboolean Close(GtkWidget* widget, GdkEvent*, gpointer data) {
    const auto* self = static_cast<Background*>(data);
    if (!self->TrayVisible()) return FALSE;
    gtk_widget_hide(widget);
    return TRUE;
  }

  static void Activate(GtkStatusIcon*, gpointer data) {
    gtk_window_present(static_cast<Background*>(data)->window_);
  }

  static void Embedded(GObject*, GParamSpec*, gpointer data) {
    const auto* self = static_cast<Background*>(data);
    if (!self->TrayVisible() && !gtk_widget_get_visible(GTK_WIDGET(self->window_))) {
      gtk_window_present(self->window_);
    }
  }

  static void Popup(GtkStatusIcon* icon, guint button, guint time, gpointer data) {
    auto* self = static_cast<Background*>(data);
    GtkWidget* menu = gtk_menu_new();
    GtkWidget* open = gtk_menu_item_new_with_label("打开 NexDesk");
    GtkWidget* exit = gtk_menu_item_new_with_label("退出 NexDesk");
    gtk_menu_shell_append(GTK_MENU_SHELL(menu), open);
    gtk_menu_shell_append(GTK_MENU_SHELL(menu), exit);
    g_signal_connect_swapped(open, "activate", G_CALLBACK(gtk_window_present), self->window_);
    auto* app = gtk_window_get_application(self->window_);
    g_signal_connect_swapped(exit, "activate", G_CALLBACK(g_application_quit), app);
    g_signal_connect(menu, "selection-done", G_CALLBACK(gtk_widget_destroy), nullptr);
    gtk_widget_show_all(menu);
    gtk_menu_popup(GTK_MENU(menu), nullptr, nullptr,
                   gtk_status_icon_position_menu, icon, button, time);
  }

  static void Handle(FlMethodChannel*, FlMethodCall* call, gpointer data) {
    auto* self = static_cast<Background*>(data);
    const gchar* method = fl_method_call_get_name(call);
    if (g_strcmp0(method, "status") == 0) {
      g_autoptr(FlValue) value = fl_value_new_map();
      fl_value_set_string_take(value, "traySupported", fl_value_new_bool(self->TraySupported()));
      fl_value_set_string_take(value, "trayEnabled", fl_value_new_bool(self->icon_ != nullptr));
      fl_value_set_string_take(value, "trayVisible", fl_value_new_bool(self->TrayVisible()));
      fl_value_set_string_take(value, "startupSupported", fl_value_new_bool(!StartupCommand().empty()));
      fl_value_set_string_take(value, "startupEnabled", fl_value_new_bool(Startup(false, false)));
      fl_method_call_respond_success(call, value, nullptr);
      return;
    }
    FlValue* arguments = fl_method_call_get_args(call);
    if (!arguments || fl_value_get_type(arguments) != FL_VALUE_TYPE_BOOL) {
      fl_method_call_respond_error(call, "argument", nullptr, nullptr, nullptr);
      return;
    }
    const bool enabled = fl_value_get_bool(arguments);
    if (g_strcmp0(method, "setStartup") == 0 && Startup(enabled, true)) {
      fl_method_call_respond_success(call, nullptr, nullptr);
    } else if (g_strcmp0(method, "setTray") == 0 && self->TraySupported() &&
               TrayPreference(enabled, true)) {
      self->ConfigureTray(enabled);
      fl_method_call_respond_success(call, nullptr, nullptr);
    } else {
      fl_method_call_respond_error(call, "background", nullptr, nullptr, nullptr);
    }
  }

  GtkWindow* const window_;
  FlMethodChannel* channel_ = nullptr;
  GtkStatusIcon* icon_ = nullptr;
};
G_GNUC_END_IGNORE_DEPRECATIONS
}  // namespace

void nexdesk_register_background(FlView* view, GtkWindow* window) {
  g_object_set_data_full(G_OBJECT(window), "nexdesk-background",
      new Background(view, window),
      [](gpointer state) { delete static_cast<Background*>(state); });
}
