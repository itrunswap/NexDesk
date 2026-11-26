#include "my_application.h"

#include <flutter_linux/flutter_linux.h>
#ifdef GDK_WINDOWING_X11
#include <gdk/gdkx.h>
#endif

#include <cstring>
#include <map>

#include "flutter/generated_plugin_registrant.h"
#include "background.h"
#include "preview_texture.h"

struct _MyApplication {
  GtkApplication parent_instance;
  char** dart_entrypoint_arguments;
};

G_DEFINE_TYPE(MyApplication, my_application, GTK_TYPE_APPLICATION)

// Called when first Flutter frame received.
static void first_frame_cb(FlView* view, gpointer) {
  auto* window = GTK_WINDOW(gtk_widget_get_toplevel(GTK_WIDGET(view)));
  gtk_window_present(window);
}

static GtkWindow* create_window(GtkApplication* application, const gchar* title,
                                int width, int height) {
  auto* window = GTK_WINDOW(gtk_application_window_new(application));
  // Use a header bar when running in GNOME as this is the common style used
  // by applications and is the setup most users will be using (e.g. Ubuntu
  // desktop).
  // If running on X and not using GNOME then just use a traditional title bar
  // in case the window manager does more exotic layout, e.g. tiling.
  // If running on Wayland assume the header bar will work (may need changing
  // if future cases occur).
  gboolean use_header_bar = TRUE;
#ifdef GDK_WINDOWING_X11
  GdkScreen* screen = gtk_window_get_screen(window);
  if (GDK_IS_X11_SCREEN(screen)) {
    const gchar* wm_name = gdk_x11_screen_get_window_manager_name(screen);
    if (g_strcmp0(wm_name, "GNOME Shell") != 0) {
      use_header_bar = FALSE;
    }
  }
#endif
  if (use_header_bar) {
    GtkHeaderBar* header_bar = GTK_HEADER_BAR(gtk_header_bar_new());
    gtk_widget_show(GTK_WIDGET(header_bar));
    gtk_header_bar_set_title(header_bar, title);
    gtk_header_bar_set_show_close_button(header_bar, TRUE);
    gtk_window_set_titlebar(window, GTK_WIDGET(header_bar));
  }
  gtk_window_set_title(window, title);
  gtk_window_set_default_size(window, width, height);
  return window;
}

static FlView* install_view(GtkWindow* window, char** arguments) {
  g_autoptr(FlDartProject) project = fl_dart_project_new();
  fl_dart_project_set_dart_entrypoint_arguments(project, arguments);

  FlView* view = fl_view_new(project);
  GdkRGBA background_color;
  // Background defaults to black, override it here if necessary, e.g. #00000000
  // for transparent.
  gdk_rgba_parse(&background_color, "#000000");
  fl_view_set_background_color(view, &background_color);
  gtk_widget_show(GTK_WIDGET(view));
  gtk_container_add(GTK_CONTAINER(window), GTK_WIDGET(view));

  // Show the window when Flutter renders.
  // Requires the view to be realized so we can start rendering.
  g_signal_connect(view, "first-frame", G_CALLBACK(first_frame_cb), nullptr);
  gtk_widget_realize(GTK_WIDGET(view));

  fl_register_plugins(FL_PLUGIN_REGISTRY(view));
  nexdesk_register_texture_bridge(view);
  gtk_widget_grab_focus(GTK_WIDGET(view));
  return view;
}

namespace {

FlMethodChannel* session_channel(FlView* view) {
  g_autoptr(FlStandardMethodCodec) codec = fl_standard_method_codec_new();
  return fl_method_channel_new(
      fl_engine_get_binary_messenger(fl_view_get_engine(view)),
      "dev.nexdesk/session_window", FL_METHOD_CODEC(codec));
}

gint64 session_id(FlValue* value) {
  return value && fl_value_get_type(value) == FL_VALUE_TYPE_INT
             ? fl_value_get_int(value)
             : 0;
}

const gchar* bounded_string(FlValue* value, size_t limit) {
  if (!value || fl_value_get_type(value) != FL_VALUE_TYPE_STRING) return nullptr;
  const gchar* text = fl_value_get_string(value);
  return text[0] && std::strlen(text) <= limit ? text : nullptr;
}

void respond(FlMethodCall* call, bool success) {
  g_autoptr(FlValue) value = fl_value_new_bool(success);
  fl_method_call_respond_success(call, value, nullptr);
}

class SessionWindows {
 public:
  SessionWindows(FlView* view, GtkWindow* home)
      : home_(home), channel_(session_channel(view)) {
    fl_method_channel_set_method_call_handler(channel_, Handle, this, nullptr);
    g_signal_connect(home, "delete-event", G_CALLBACK(CloseHome), this);
  }

  ~SessionWindows() {
    closing_ = true;
    while (!windows_.empty()) {
      gtk_widget_destroy(GTK_WIDGET(windows_.begin()->second));
    }
    fl_method_channel_set_method_call_handler(channel_, nullptr, nullptr, nullptr);
    g_object_unref(channel_);
  }

 private:
  struct Child {
    SessionWindows* owner;
    gint64 id;
    FlMethodChannel* channel;
  };

  bool Open(FlValue* args) {
    if (!args || fl_value_get_type(args) != FL_VALUE_TYPE_MAP) return false;
    const auto id = session_id(fl_value_lookup_string(args, "id"));
    const auto* title = bounded_string(fl_value_lookup_string(args, "title"), 1024);
    const auto* launch = bounded_string(fl_value_lookup_string(args, "launch"), 16384);
    if (id <= 0 || !title || !launch || windows_.count(id)) return false;

    auto* window = create_window(gtk_window_get_application(home_), title, 1060, 720);
    gtk_window_set_position(window, GTK_WIN_POS_CENTER);
    gchar kind[] = "--nexdesk-session-window";
    g_autofree gchar* descriptor = g_strdup(launch);
    gchar* arguments[] = {kind, descriptor, nullptr};
    auto* view = install_view(window, arguments);
    auto* child = new Child{this, id, session_channel(view)};
    g_object_set_data_full(G_OBJECT(window), "nexdesk-session-child", child, [](gpointer data) {
      auto* child = static_cast<Child*>(data);
      g_object_unref(child->channel);
      delete child;
    });
    g_signal_connect(window, "delete-event", G_CALLBACK(RequestClose), child);
    g_signal_connect(window, "destroy", G_CALLBACK(ChildClosed), child);
    fl_method_channel_set_method_call_handler(child->channel, HandleChild, window, nullptr);
    windows_.emplace(id, window);
    return true;
  }

  bool Focus(gint64 id, bool request_close) {
    const auto window = windows_.find(id);
    if (window == windows_.end()) return false;
    gtk_window_present(window->second);
    if (request_close) gtk_window_close(window->second);
    return true;
  }

  static void Handle(FlMethodChannel*, FlMethodCall* call, gpointer data) {
    auto* self = static_cast<SessionWindows*>(data);
    const auto* name = fl_method_call_get_name(call);
    auto* args = fl_method_call_get_args(call);
    if (g_strcmp0(name, "open") == 0) {
      respond(call, self->Open(args));
    } else if (g_strcmp0(name, "focus") == 0 ||
               g_strcmp0(name, "requestClose") == 0) {
      respond(call, self->Focus(session_id(args), g_strcmp0(name, "requestClose") == 0));
    } else {
      fl_method_call_respond_not_implemented(call, nullptr);
    }
  }

  static void HandleChild(FlMethodChannel*, FlMethodCall* call, gpointer data) {
    if (g_strcmp0(fl_method_call_get_name(call), "close") != 0) {
      fl_method_call_respond_not_implemented(call, nullptr);
      return;
    }
    respond(call, true);
    g_idle_add_full(G_PRIORITY_DEFAULT_IDLE, [](gpointer window) -> gboolean {
      gtk_widget_destroy(GTK_WIDGET(window));
      return G_SOURCE_REMOVE;
    }, g_object_ref(data), g_object_unref);
  }

  static gboolean RequestClose(GtkWidget*, GdkEvent*, gpointer data) {
    auto* child = static_cast<Child*>(data);
    fl_method_channel_invoke_method(child->channel, "requestClose", nullptr,
                                    nullptr, nullptr, nullptr);
    return TRUE;
  }

  static void ChildClosed(GtkWidget*, gpointer data) {
    auto* child = static_cast<Child*>(data);
    auto* owner = child->owner;
    fl_method_channel_set_method_call_handler(child->channel, nullptr, nullptr, nullptr);
    owner->windows_.erase(child->id);
    if (!owner->closing_) {
      g_autoptr(FlValue) id = fl_value_new_int(child->id);
      fl_method_channel_invoke_method(owner->channel_, "closed", id,
                                      nullptr, nullptr, nullptr);
    }
  }

  static gboolean CloseHome(GtkWidget* window, GdkEvent*, gpointer data) {
    if (static_cast<SessionWindows*>(data)->windows_.empty()) return FALSE;
    gtk_window_iconify(GTK_WINDOW(window));
    return TRUE;
  }

  GtkWindow* home_;
  FlMethodChannel* channel_;
  std::map<gint64, GtkWindow*> windows_;
  bool closing_ = false;
};

}  // namespace

// Implements GApplication::activate.
static void my_application_activate(GApplication* application) {
  MyApplication* self = MY_APPLICATION(application);
  auto* window = create_window(GTK_APPLICATION(application), "NexDesk", 820, 600);
  auto* view = install_view(window, self->dart_entrypoint_arguments);
  nexdesk_register_background(view, window);
  g_object_set_data_full(G_OBJECT(window), "nexdesk-session-windows",
                         new SessionWindows(view, window), [](gpointer data) {
    delete static_cast<SessionWindows*>(data);
  });
}

// Implements GApplication::local_command_line.
static gboolean my_application_local_command_line(GApplication* application,
                                                  gchar*** arguments,
                                                  int* exit_status) {
  MyApplication* self = MY_APPLICATION(application);
  // Strip out the first argument as it is the binary name.
  self->dart_entrypoint_arguments = g_strdupv(*arguments + 1);

  g_autoptr(GError) error = nullptr;
  if (!g_application_register(application, nullptr, &error)) {
    g_warning("Failed to register: %s", error->message);
    *exit_status = 1;
    return TRUE;
  }

  g_application_activate(application);
  *exit_status = 0;

  return TRUE;
}

// Implements GApplication::startup.
static void my_application_startup(GApplication* application) {
  // MyApplication* self = MY_APPLICATION(object);

  // Perform any actions required at application startup.

  G_APPLICATION_CLASS(my_application_parent_class)->startup(application);
}

// Implements GApplication::shutdown.
static void my_application_shutdown(GApplication* application) {
  // MyApplication* self = MY_APPLICATION(object);

  // Perform any actions required at application shutdown.

  G_APPLICATION_CLASS(my_application_parent_class)->shutdown(application);
}

// Implements GObject::dispose.
static void my_application_dispose(GObject* object) {
  MyApplication* self = MY_APPLICATION(object);
  g_clear_pointer(&self->dart_entrypoint_arguments, g_strfreev);
  G_OBJECT_CLASS(my_application_parent_class)->dispose(object);
}

static void my_application_class_init(MyApplicationClass* klass) {
  G_APPLICATION_CLASS(klass)->activate = my_application_activate;
  G_APPLICATION_CLASS(klass)->local_command_line =
      my_application_local_command_line;
  G_APPLICATION_CLASS(klass)->startup = my_application_startup;
  G_APPLICATION_CLASS(klass)->shutdown = my_application_shutdown;
  G_OBJECT_CLASS(klass)->dispose = my_application_dispose;
}

static void my_application_init(MyApplication*) {}

MyApplication* my_application_new() {
  // Set the program name to the application ID, which helps various systems
  // like GTK and desktop environments map this running application to its
  // corresponding .desktop file. This ensures better integration by allowing
  // the application to be recognized beyond its binary name.
  g_set_prgname(APPLICATION_ID);

  return MY_APPLICATION(g_object_new(my_application_get_type(),
                                     "application-id", APPLICATION_ID, "flags",
                                     G_APPLICATION_NON_UNIQUE, nullptr));
}
