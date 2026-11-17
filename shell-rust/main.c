/*
 * main.c — shell do desktop Rust: uma única janela GTK3 + WebKitGTK.
 *
 * O compositor Smithay reconhece esta janela pelo app_id
 * "spatial-deck-shell" e a fixa em (0,0), no tamanho da saída, abaixo das
 * janelas dos aplicativos. O frontend WASM cuida do canvas, do deck e do
 * WebSocket com o compositor; aqui só hospedamos a página.
 */
#define _GNU_SOURCE
#include <gtk/gtk.h>
#include <webkit2/webkit2.h>

static void on_destroy(GtkWidget *widget, gpointer data) {
	(void)widget;
	(void)data;
	gtk_main_quit();
}

static void on_load_changed(WebKitWebView *view, WebKitLoadEvent event, gpointer data) {
	(void)view;
	(void)data;
	const char *name = "?";
	switch (event) {
	case WEBKIT_LOAD_STARTED: name = "started"; break;
	case WEBKIT_LOAD_REDIRECTED: name = "redirected"; break;
	case WEBKIT_LOAD_COMMITTED: name = "committed"; break;
	case WEBKIT_LOAD_FINISHED: name = "finished"; break;
	}
	g_print("[shell] load %s\n", name);
}

static gboolean on_load_failed(WebKitWebView *view, WebKitLoadEvent event,
		const char *uri, GError *error, gpointer data) {
	(void)view;
	(void)event;
	(void)data;
	g_printerr("[shell] falha ao carregar %s: %s\n", uri, error->message);
	return FALSE;
}

/*
 * O botão direito pertence ao desktop: na visão geral ele fecha a janela
 * apontada e no canvas não deve abrir nada. O menu nativo do WebKit
 * (voltar/recarregar/inspecionar elemento) cria um popup GTK que toma o
 * grab do ponteiro e deixa o desktop aninhado sem resposta. Retornar TRUE
 * suprime o menu; o evento continua chegando à página e ao compositor.
 */
static gboolean on_context_menu(WebKitWebView *view, WebKitContextMenu *menu,
		GdkEvent *event, WebKitHitTestResult *hit_test, gpointer data) {
	(void)view;
	(void)menu;
	(void)event;
	(void)hit_test;
	(void)data;
	return TRUE;
}

int main(int argc, char **argv) {
	/* GTK3 deriva o app_id Wayland do nome do programa/classe. */
	g_set_prgname("spatial-deck-shell");
	gdk_set_program_class("spatial-deck-shell");

	gtk_init(&argc, &argv);

	const char *url = argc > 1 ? argv[1] : "http://127.0.0.1:7338/";

	GtkWidget *window = gtk_window_new(GTK_WINDOW_TOPLEVEL);
	gtk_window_set_decorated(GTK_WINDOW(window), FALSE);
	gtk_window_set_title(GTK_WINDOW(window), "Spatial Deck Shell");
	gtk_window_set_default_size(GTK_WINDOW(window), 1280, 720);
	gtk_window_set_resizable(GTK_WINDOW(window), TRUE);
	gtk_window_set_skip_taskbar_hint(GTK_WINDOW(window), TRUE);

	WebKitWebView *view = WEBKIT_WEB_VIEW(webkit_web_view_new());
	WebKitSettings *settings = webkit_web_view_get_settings(view);
	webkit_settings_set_enable_developer_extras(settings, TRUE);
	webkit_settings_set_enable_smooth_scrolling(settings, TRUE);
	webkit_settings_set_media_playback_requires_user_gesture(settings, FALSE);
	webkit_settings_set_enable_write_console_messages_to_stdout(settings, TRUE);

	if (g_getenv("SPATIAL_DECK_SOFTWARE")) {
		webkit_settings_set_hardware_acceleration_policy(
			settings, WEBKIT_HARDWARE_ACCELERATION_POLICY_NEVER);
	}

	/* Evita o flash branco antes da página pintar o canvas. */
	GdkRGBA background = { 0.027, 0.039, 0.067, 1.0 };
	webkit_web_view_set_background_color(view, &background);

	gtk_container_add(GTK_CONTAINER(window), GTK_WIDGET(view));
	g_signal_connect(window, "destroy", G_CALLBACK(on_destroy), NULL);
	g_signal_connect(view, "load-changed", G_CALLBACK(on_load_changed), NULL);
	g_signal_connect(view, "load-failed", G_CALLBACK(on_load_failed), NULL);
	g_signal_connect(view, "context-menu", G_CALLBACK(on_context_menu), NULL);

	gtk_widget_show_all(window);
	webkit_web_view_load_uri(view, url);

	gtk_main();
	return 0;
}
