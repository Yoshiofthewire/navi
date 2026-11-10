/* GTK's native per-process module hook. Loaded only by Navi's selected utilities. */
#include <gtk/gtk.h>
#include <gmodule.h>

static GFile *appearance;
static GtkCssProvider *provider;
static gchar *initial_icons, *initial_cursor;

static void reload(void)
{
    GError *error = NULL;
    GFileInputStream *stream = g_file_read(appearance, NULL, &error);
    if (!stream) { g_clear_error(&error); return; }
    GBytes *bytes = g_input_stream_read_bytes(G_INPUT_STREAM(stream), 65537, NULL, &error);
    g_object_unref(stream);
    if (!bytes) { g_clear_error(&error); return; }
    gsize length;
    const gchar *data = g_bytes_get_data(bytes, &length);
    GKeyFile *keys = g_key_file_new();
    if (length > 65536 || !g_key_file_load_from_data(keys, data, length, G_KEY_FILE_NONE, &error))
        goto done;
    gchar *css = g_key_file_get_string(keys, "Navi", "css", NULL);
    if (!css) goto done;
    gtk_css_provider_load_from_data(provider, css, -1, &error);
    g_free(css);
    if (error) goto done;
    gchar *icons = g_key_file_get_string(keys, "Navi", "icons", NULL);
    gchar *cursor = g_key_file_get_string(keys, "Navi", "cursor", NULL);
    gint size = g_key_file_get_integer(keys, "Navi", "size", NULL);
    g_object_set(gtk_settings_get_default(),
                 "gtk-theme-name", "Adwaita",
                 "gtk-application-prefer-dark-theme", g_key_file_get_boolean(keys, "Navi", "dark", NULL),
                 "gtk-enable-animations", FALSE,
                 "gtk-decoration-layout", ":minimize,maximize,close",
                 "gtk-icon-theme-name", icons && *icons ? icons : initial_icons,
                 "gtk-cursor-theme-name", cursor && *cursor ? cursor : (g_getenv("XCURSOR_THEME") ? g_getenv("XCURSOR_THEME") : initial_cursor),
                 "gtk-cursor-theme-size", CLAMP(size, 16, 64), NULL);
    g_free(icons);
    g_free(cursor);
done:
    if (error) { g_warning("Navi utility appearance: %s", error->message); g_clear_error(&error); }
    g_key_file_unref(keys);
    g_bytes_unref(bytes);
}

static void changed(GFileMonitor *monitor, GFile *file, GFile *other, GFileMonitorEvent event, gpointer data)
{
    (void)monitor; (void)event; (void)data;
    if (g_file_equal(file, appearance) || (other && g_file_equal(other, appearance))) reload();
}

G_MODULE_EXPORT void gtk_module_init(gint *argc, gchar ***argv)
{
    (void)argc; (void)argv;
    const gchar *path = g_getenv("NAVI_UTILITY_APPEARANCE");
    if (!path || !g_path_is_absolute(path) || provider) return;
    appearance = g_file_new_for_path(path);
    g_object_get(gtk_settings_get_default(), "gtk-icon-theme-name", &initial_icons,
                 "gtk-cursor-theme-name", &initial_cursor, NULL);
    provider = gtk_css_provider_new();
    gtk_style_context_add_provider_for_screen(gdk_screen_get_default(), GTK_STYLE_PROVIDER(provider), GTK_STYLE_PROVIDER_PRIORITY_APPLICATION);
    reload();
    /* Watch the directory so atomic replacement and subsequent saves stay visible. */
    GFile *parent = g_file_get_parent(appearance);
    GFileMonitor *monitor = g_file_monitor_directory(parent, G_FILE_MONITOR_WATCH_MOVES, NULL, NULL);
    g_object_unref(parent);
    if (monitor) g_signal_connect(monitor, "changed", G_CALLBACK(changed), NULL);
    /* Provider/monitor/settings live for this utility process, with no background daemon. */
}
