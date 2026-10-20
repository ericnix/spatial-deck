/*
 * main.c — shell-host do Padrão: GTK3 + gtk-layer-shell + WebKitGTK.
 *
 * Duas layer surfaces:
 *   canvas — BACKGROUND, fullscreen, sem teclado (fundo interativo);
 *   deck   — TOP, encostada embaixo e centralizada, cresce via deck_size.
 *
 * Pontes:
 *   JS  --webkit.messageHandlers.p-->  host  --socket Unix-->  compositor
 *   JS  <--Padrao.emit(json)--         host  <--socket Unix--  compositor
 *
 * Também serve a UI por HTTP local, renderiza ícones do tema, lista
 * aplicativos (gio), escolhe arquivos e persiste o estado do canvas.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include <gio/gdesktopappinfo.h>
#include <glib-unix.h>
#include <jsc/jsc.h>

#include "padrao_host.h"

HostPage g_canvas_page = { .name = "canvas" };
HostPage g_deck_page = { .name = "deck" };
char *g_shell_dir = NULL;
int g_http_port = 0;
GHashTable *g_app_icons = NULL; /* id sem ".desktop" -> strv de nomes */

static char *view_json;          /* {"x":..,"y":..,"z":..} */
static char *out_json;           /* {"w":..,"h":..} */
static GHashTable *wins;         /* id -> objeto "win" (str) */
static GHashTable *saved;        /* chave -> JSON persistido */
static char *apps_json;          /* "[...]" */
static GHashTable *icon_cache;   /* "chave:tamanho" -> GBytes (PNG) */
static GMutex icon_lock;
static char *state_dir;

/* ============================ páginas ================================= */

HostPage *host_page_by_name(const char *name) {
	if (name && strcmp(name, "deck") == 0) return &g_deck_page;
	return &g_canvas_page;
}

void host_emit(HostPage *page, const char *json) {
	if (!page || !page->view || !page->ready || !json) return;
	char *js = g_strdup_printf("window.Padrao&&Padrao.emit(%s);", json);
	webkit_web_view_evaluate_javascript(page->view, js, -1,
		NULL, NULL, NULL, NULL, NULL);
	g_free(js);
}

void host_emit_all(const char *json) {
	host_emit(&g_canvas_page, json);
	host_emit(&g_deck_page, json);
}

/* Gancho de depuração (rota /debug/emit do HTTP, só loopback): injeta um
   evento Padrao.emit() numa página a partir de outro thread. */
typedef struct { char *page; char *json; } EmitJob;

static gboolean debug_emit_main(gpointer data) {
	EmitJob *j = data;
	host_emit(host_page_by_name(j->page), j->json);
	g_free(j->page);
	g_free(j->json);
	g_free(j);
	return G_SOURCE_REMOVE;
}

void host_debug_emit(const char *page_name, const char *json) {
	EmitJob *j = g_new(EmitJob, 1);
	j->page = g_strdup(page_name && *page_name ? page_name : "canvas");
	j->json = g_strdup(json);
	if (g_main_context_is_owner(g_main_context_default())) {
		debug_emit_main(j);
	} else {
		g_main_context_invoke(NULL, debug_emit_main, j);
	}
}

/* ============================= estado ================================= */

void state_set_view(const char *json) {
	g_free(view_json);
	view_json = g_strdup(json);
}

void state_set_out(const char *json) {
	g_free(out_json);
	out_json = g_strdup(json);
}

/* varre um array de objetos JSON {"..."}, um a um */
static gboolean scan_object(const char *p, const char **start, size_t *len) {
	if (*p != '{') return FALSE;
	int depth = 0;
	gboolean in_str = FALSE;
	for (const char *q = p; *q; q++) {
		char c = *q;
		if (in_str) {
			if (c == '\\') { q++; continue; }
			if (c == '"') in_str = FALSE;
			continue;
		}
		if (c == '"') { in_str = TRUE; continue; }
		if (c == '{') depth++;
		else if (c == '}') {
			depth--;
			if (depth == 0) {
				*start = p;
				*len = (size_t)(q - p + 1);
				return TRUE;
			}
		}
	}
	return FALSE;
}

void state_set_wins(const char *arr_json) {
	g_hash_table_remove_all(wins);
	const char *p = arr_json;
	while ((p = strchr(p, '{')) != NULL) {
		const char *start;
		size_t len;
		if (!scan_object(p, &start, &len)) break;
		state_win_add(start); /* start é terminado por '\0'? não — copiar */
		p = start + len;
	}
}

void state_win_add(const char *obj_json) {
	long id;
	char *copy = NULL;
	const char *start;
	size_t len;
	if (!scan_object(obj_json, &start, &len)) return;
	copy = g_strndup(start, len);
	if (jx_int(copy, "id", &id)) {
		g_hash_table_insert(wins, GSIZE_TO_POINTER((gsize)(guint64)id),
			copy);
		return;
	}
	g_free(copy);
}

void state_win_del(long id) {
	g_hash_table_remove(wins, GSIZE_TO_POINTER((gsize)id));
}

char *state_build_wins(void) {
	GString *s = g_string_new("[");
	GHashTableIter it;
	gpointer k, v;
	gboolean first = TRUE;
	g_hash_table_iter_init(&it, wins);
	while (g_hash_table_iter_next(&it, &k, &v)) {
		if (!first) g_string_append_c(s, ',');
		g_string_append(s, (const char *)v);
		first = FALSE;
	}
	g_string_append_c(s, ']');
	return g_string_free(s, FALSE);
}

char *state_build_saved(void) {
	GString *s = g_string_new("{");
	GHashTableIter it;
	gpointer k, v;
	gboolean first = TRUE;
	g_hash_table_iter_init(&it, saved);
	while (g_hash_table_iter_next(&it, &k, &v)) {
		if (!first) g_string_append_c(s, ',');
		g_string_append_printf(s, "\"%s\":%s", (const char *)k,
			(const char *)v);
		first = FALSE;
	}
	g_string_append_c(s, '}');
	return g_string_free(s, FALSE);
}

/* --------------------------- persistência ----------------------------- */

static void saved_put(const char *key, const char *data) {
	g_hash_table_insert(saved, g_strdup(key), g_strdup(data));

	char path[1024];
	snprintf(path, sizeof(path), "%s/%s.json", state_dir, key);
	FILE *f = fopen(path, "w");
	if (!f) return;
	fputs(data, f);
	fputc('\n', f);
	fclose(f);
}

static void saved_load(void) {
	GDir *dir = g_dir_open(state_dir, 0, NULL);
	if (!dir) return;
	const char *name;
	while ((name = g_dir_read_name(dir)) != NULL) {
		if (!g_str_has_suffix(name, ".json")) continue;
		char *path = g_build_filename(state_dir, name, NULL);
		char *content = NULL;
		if (g_file_get_contents(path, &content, NULL, NULL) && content) {
			char *key = g_strndup(name, strlen(name) - 5);
			g_hash_table_insert(saved, key, content);
		} else {
			g_free(content);
		}
		g_free(path);
	}
	g_dir_close(dir);
}

/* ============================ JS -> host =============================== */

static void on_js_message(WebKitUserContentManager *ucm,
		WebKitJavascriptResult *result, gpointer data) {
	(void)ucm;
	HostPage *page = data;
	JSCValue *v = webkit_javascript_result_get_js_value(result);
	gchar *str = v ? jsc_value_to_string(v) : NULL;
	if (str) {
		host_handle_js(page, str);
		g_free(str);
	}
}

static char *make_view_json(double x, double y, double z) {
	return g_strdup_printf("{\"x\":%g,\"y\":%g,\"z\":%g}", x, y, z);
}

void host_handle_js(HostPage *page, const char *json) {
	char t[48] = "";
	if (!jx_str(json, "t", t, sizeof(t))) return;

	if (strcmp(t, "ready") == 0) {
		page->ready = TRUE;
		host_send_init(page);
		return;
	}
	if (strcmp(t, "view") == 0) {
		double x = 0, y = 0, z = 1;
		jx_num(json, "x", &x);
		jx_num(json, "y", &y);
		jx_num(json, "z", &z);
		if (z < 0.15) z = 0.15;
		if (z > 4.0) z = 4.0;
		char *v = make_view_json(x, y, z);
		state_set_view(v);
		g_free(v);
		host_send_compositor("{\"t\":\"set_view\",\"x\":%g,\"y\":%g,\"z\":%g}",
			x, y, z);
		return;
	}
	if (strcmp(t, "win_move") == 0 || strcmp(t, "win_focus") == 0
			|| strcmp(t, "win_close") == 0) {
		host_send_compositor("%s", json);
		return;
	}
	if (strcmp(t, "save") == 0) {
		char key[64] = "canvas";
		jx_str(json, "key", key, sizeof(key));
		const char *start;
		size_t len;
		if (jx_raw(json, "data", &start, &len)) {
			char *data = g_strndup(start, len);
			saved_put(key, data);
			g_free(data);
		}
		return;
	}
	if (strcmp(t, "pick") == 0) {
		char kind[32] = "";
		jx_str(json, "kind", kind, sizeof(kind));
		do_pick(kind, page->name);
		return;
	}
	if (strcmp(t, "deck_size") == 0) {
		double w = 0, h = 0;
		jx_num(json, "w", &w);
		jx_num(json, "h", &h);
		deck_resize((int)w, (int)h);
		return;
	}
	if (strcmp(t, "launch") == 0) {
		char app[256] = "";
		if (jx_str(json, "app", app, sizeof(app))) do_launch(app);
		return;
	}
}

void host_send_init(HostPage *page) {
	char *wins_arr = state_build_wins();
	char *saved_obj = state_build_saved();
	char *json = g_strdup_printf(
		"{\"t\":\"init\",\"page\":\"%s\","
		"\"view\":%s,\"out\":%s,"
		"\"wins\":%s,\"apps\":%s,\"saved\":%s,"
		"\"iconBase\":\"/\"}",
		page->name,
		view_json ? view_json : "{\"x\":0,\"y\":0,\"z\":1}",
		out_json ? out_json : "{\"w\":1280,\"h\":720}",
		wins_arr,
		apps_json ? apps_json : "[]",
		saved_obj);
	host_emit(page, json);
	g_free(json);
	g_free(wins_arr);
	g_free(saved_obj);
}

/* ======================= compositor -> host ============================ */

void host_on_compositor_line(const char *line) {
	char t[48] = "";
	if (!jx_str(line, "t", t, sizeof(t))) return;

	if (strcmp(t, "state") == 0) {
		const char *start;
		size_t len;
		if (jx_raw(line, "view", &start, &len)) {
			char *v = g_strndup(start, len);
			state_set_view(v);
			g_free(v);
		}
		if (jx_raw(line, "out", &start, &len)) {
			char *v = g_strndup(start, len);
			state_set_out(v);
			g_free(v);
		}
		if (jx_raw(line, "wins", &start, &len)) {
			char *v = g_strndup(start, len);
			state_set_wins(v);
			g_free(v);
		}
		/* páginas já prontas ganham o quadro completo de novo */
		if (g_canvas_page.ready) host_send_init(&g_canvas_page);
		if (g_deck_page.ready) host_send_init(&g_deck_page);
		return;
	}
	if (strcmp(t, "view") == 0) {
		double x = 0, y = 0, z = 1;
		jx_num(line, "x", &x);
		jx_num(line, "y", &y);
		jx_num(line, "z", &z);
		char *v = make_view_json(x, y, z);
		state_set_view(v);
		g_free(v);
	} else if (strcmp(t, "output") == 0) {
		double w = 0, h = 0;
		jx_num(line, "w", &w);
		jx_num(line, "h", &h);
		char *v = g_strdup_printf("{\"w\":%g,\"h\":%g}", w, h);
		state_set_out(v);
		g_free(v);
	} else if (strcmp(t, "win") == 0) {
		state_win_add(line);
	} else if (strcmp(t, "win_gone") == 0) {
		long id;
		if (jx_int(line, "id", &id)) state_win_del(id);
	}
	host_emit_all(line);
}

/* ============================ ações =================================== */

void deck_resize(int w, int h) {
	static int last_w = -1, last_h = -1;
	if (w <= 0 || h <= 0) return;
	if (w == last_w && h == last_h) return;
	last_w = w;
	last_h = h;
	/* gtk_window_resize não altera a geometria de uma layer surface sem
	   âncoras laterais: o tamanho vem do size request do widget. */
	if (g_deck_page.win)
		gtk_widget_set_size_request(GTK_WIDGET(g_deck_page.win), w, h);
}

void do_pick(const char *kind, const char *page_name) {
	GtkWidget *dlg = gtk_file_chooser_dialog_new(
		kind && strcmp(kind, "video") == 0 ? "Inserir vídeo" : "Inserir mídia",
		NULL, GTK_FILE_CHOOSER_ACTION_OPEN,
		"Cancelar", GTK_RESPONSE_CANCEL,
		"Inserir", GTK_RESPONSE_ACCEPT, NULL);
	GtkFileChooser *fc = GTK_FILE_CHOOSER(dlg);
	gtk_file_chooser_set_select_multiple(fc, TRUE);

	GtkFileFilter *f = gtk_file_filter_new();
	if (kind && strcmp(kind, "image") == 0) {
		gtk_file_filter_add_mime_type(f, "image/*");
	} else if (kind && strcmp(kind, "video") == 0) {
		gtk_file_filter_add_mime_type(f, "video/*");
	} else {
		gtk_file_filter_add_mime_type(f, "image/*");
		gtk_file_filter_add_mime_type(f, "video/*");
	}
	gtk_file_chooser_set_filter(fc, f);

	if (gtk_dialog_run(GTK_DIALOG(dlg)) == GTK_RESPONSE_ACCEPT) {
		GSList *files = gtk_file_chooser_get_filenames(fc);
		GString *s = g_string_new("{\"t\":\"picked\",\"paths\":[");
		guint n = 0;
		for (GSList *l = files; l; l = l->next) {
			char esc[4096];
			jx_escape(esc, sizeof(esc), l->data);
			if (n++) g_string_append_c(s, ',');
			g_string_append_printf(s, "\"%s\"", esc);
		}
		g_string_append(s, "]}");
		host_emit(host_page_by_name(page_name), s->str);
		g_string_free(s, TRUE);
		g_slist_free_full(files, g_free);
	}
	gtk_widget_destroy(dlg);
}

void do_launch(const char *app_id) {
	GError *err = NULL;
	GDesktopAppInfo *info = g_desktop_app_info_new(app_id);
	if (!info && !g_str_has_suffix(app_id, ".desktop")) {
		char *fn = g_strconcat(app_id, ".desktop", NULL);
		info = g_desktop_app_info_new(fn);
		g_free(fn);
	}
	if (info) {
		GAppLaunchContext *ctx = g_app_launch_context_new();
		gboolean ok = g_app_info_launch(G_APP_INFO(info), NULL, ctx, &err);
		g_object_unref(ctx);
		g_object_unref(info);
		if (!ok) {
			g_warning("padrao-host: falha ao abrir %s: %s",
				app_id, err ? err->message : "?");
			g_clear_error(&err);
		}
		return;
	}
	char *argv[] = { (char *)app_id, NULL };
	if (!g_spawn_async(NULL, argv, NULL, G_SPAWN_SEARCH_PATH,
			NULL, NULL, NULL, &err)) {
		g_warning("padrao-host: não achei o app '%s': %s",
			app_id, err ? err->message : "?");
		g_clear_error(&err);
	}
}

/* ========================== aplicativos ================================ */

static const char *FAV_PREFIXES[] = {
	"firefox", "chromium", "brave", "konsole", "org.kde.konsole",
	"org.gnome.Nautilus", "nautilus", "thunar", "org.gnome.Terminal",
	"gnome-terminal", "org.gnome.TextEditor", "gedit", "org.kde.kate",
	"kate", "code", "org.gnome.Calculator", "org.gnome.clocks",
	"vlc", "gimp", "org.gnome.Music", "org.gnome.Photos",
	"org.gnome.Boxes", "org.gnome.Console", "dolphin", "org.kde.dolphin",
	"libreoffice", "org.libreoffice.LibreOffice", "org.gnome.Settings",
	NULL
};

static char *strip_desktop(const char *id) {
	return g_str_has_suffix(id, ".desktop")
		? g_strndup(id, strlen(id) - 8) : g_strdup(id);
}

static gboolean is_fav(const char *id, const char *name) {
	for (int i = 0; FAV_PREFIXES[i]; i++) {
		if (strcasestr(id, FAV_PREFIXES[i])) return TRUE;
		if (name && strcasestr(name, FAV_PREFIXES[i])) return TRUE;
	}
	return FALSE;
}

static char **icon_names_of(GAppInfo *info) {
	GIcon *ic = g_app_info_get_icon(info);
	if (!ic) return NULL;
	if (G_IS_THEMED_ICON(ic)) {
		const char * const *nm = g_themed_icon_get_names(G_THEMED_ICON(ic));
		GPtrArray *out = g_ptr_array_new();
		for (int i = 0; nm && nm[i]; i++)
			g_ptr_array_add(out, g_strdup(nm[i]));
		g_ptr_array_add(out, NULL);
		return (char **)g_ptr_array_free(out, FALSE);
	}
	char *s = g_icon_to_string(ic);
	if (!s) return NULL;
	char *arr[] = { s, NULL };
	char **out = g_strdupv(arr);
	g_free(s);
	return out;
}

/* --------- ícones: cache + renderização (sempre no thread do GTK) ------ */

static char *cache_get(const char *ckey, gsize *len) {
	g_mutex_lock(&icon_lock);
	GBytes *b = g_hash_table_lookup(icon_cache, ckey);
	char *out = NULL;
	if (b) {
		gconstpointer d = g_bytes_get_data(b, len);
		out = g_memdup2(d, *len);
	}
	g_mutex_unlock(&icon_lock);
	return out;
}

static void cache_put(const char *ckey, const void *data, gsize len) {
	g_mutex_lock(&icon_lock);
	g_hash_table_insert(icon_cache, g_strdup(ckey),
		g_bytes_new(data, len));
	g_mutex_unlock(&icon_lock);
}

static gboolean icon_present(GtkIconTheme *theme, const char *name) {
	return name && *name && gtk_icon_theme_has_icon(theme, name);
}

static GdkPixbuf *load_pixbuf(const char *key, int size) {
	GtkIconTheme *theme = gtk_icon_theme_get_default();
	GPtrArray *names = g_ptr_array_new();

	if (icon_present(theme, key)) g_ptr_array_add(names, (gpointer)key);
	char *lower = g_ascii_strdown(key, -1);
	if (g_strcmp0(lower, key) != 0 && icon_present(theme, lower))
		g_ptr_array_add(names, lower);
	/* nomes de ícone do aplicativo (tabela preenchida no scan) */
	char **app_names = g_app_icons
		? g_hash_table_lookup(g_app_icons, key) : NULL;
	if (app_names)
		for (int i = 0; app_names[i]; i++)
			if (icon_present(theme, app_names[i]))
				g_ptr_array_add(names, app_names[i]);

	if (names->len == 0) {
		/* nada concreto: o JS desenha a letra do app no lugar */
		g_ptr_array_free(names, TRUE);
		g_free(lower);
		return NULL;
	}
	g_ptr_array_add(names, NULL);

	GtkIconInfo *info = gtk_icon_theme_choose_icon(theme,
		(const char **)names->pdata, size, 0);
	GError *err = NULL;
	GdkPixbuf *pb = info ? gtk_icon_info_load_icon(info, &err) : NULL;
	if (err) {
		g_warning("padrao-host: ícone %s: %s", key, err->message);
		g_error_free(err);
	}
	if (info) g_object_unref(info);
	g_ptr_array_free(names, TRUE);
	g_free(lower);
	return pb;
}

/* render + grava no cache; retorna PNG (g_free) ou NULL */
static char *render_icon(const char *key, int size, gsize *len) {
	char ckey[320];
	snprintf(ckey, sizeof(ckey), "%s:%d", key, size);
	char *hit = cache_get(ckey, len);
	if (hit) return hit;

	GdkPixbuf *pb = load_pixbuf(key, size);
	if (!pb) return NULL;
	gchar *buf = NULL;
	gsize blen = 0;
	GError *err = NULL;
	if (!gdk_pixbuf_save_to_bufferv(pb, &buf, &blen, "png",
			NULL, NULL, &err)) {
		g_warning("padrao-host: png do ícone: %s", err ? err->message : "?");
		g_clear_error(&err);
		g_object_unref(pb);
		return NULL;
	}
	g_object_unref(pb);
	cache_put(ckey, buf, blen);
	*len = blen;
	return buf; /* alocado pelo gdk-pixbuf com g_malloc */
}

typedef struct {
	char key[320];
	int size;
	char *png;
	gsize len;
	GMutex m;
	GCond c;
	gboolean done;
} IconJob;

static gboolean icon_job_main(gpointer data) {
	IconJob *j = data;
	j->png = render_icon(j->key, j->size, &j->len);
	g_mutex_lock(&j->m);
	j->done = TRUE;
	g_cond_signal(&j->c);
	g_mutex_unlock(&j->m);
	return G_SOURCE_REMOVE;
}

char *host_icon_png(const char *key, int size, gsize *len) {
	*len = 0;
	if (!key || !*key) return NULL;
	char ckey[320];
	snprintf(ckey, sizeof(ckey), "%s:%d", key, size);

	char *hit = cache_get(ckey, len);
	if (hit) return hit;

	/* já no thread do GTK: renderiza direto (senão, dá deadlock) */
	if (g_main_context_is_owner(g_main_context_default()))
		return render_icon(key, size, len);

	/* chamado do thread HTTP: manda a renderização para o thread do GTK */
	IconJob j = { .size = size, .png = NULL, .len = 0, .done = FALSE };
	g_strlcpy(j.key, key, sizeof(j.key));
	g_mutex_init(&j.m);
	g_cond_init(&j.c);
	g_main_context_invoke(NULL, icon_job_main, &j);
	g_mutex_lock(&j.m);
	while (!j.done) g_cond_wait(&j.c, &j.m);
	g_mutex_unlock(&j.m);
	g_mutex_clear(&j.m);
	g_cond_clear(&j.c);

	*len = j.len;
	return j.png;
}

static void prerender(const char *key) {
	static const int sizes[] = { 48, 64 };
	for (size_t i = 0; i < G_N_ELEMENTS(sizes); i++) {
		gsize len = 0;
		char *png = render_icon(key, sizes[i], &len);
		g_free(png);
	}
}

void apps_scan(void) {
	GList *list = g_app_info_get_all();
	GString *s = g_string_new("[");
	guint n = 0;

	for (GList *l = list; l; l = l->next) {
		GAppInfo *info = l->data;
		if (!g_app_info_should_show(info)) continue;
		const char *id = g_app_info_get_id(info);
		if (!id) continue;

		char *stripped = strip_desktop(id);
		const char *name = g_app_info_get_display_name(info);
		char **icon_names = icon_names_of(info);
		if (icon_names)
			g_hash_table_insert(g_app_icons, g_strdup(stripped),
				g_strdupv(icon_names));
		const char *icon = (icon_names && icon_names[0])
			? icon_names[0] : stripped;

		char eid[512], ename[512], eicon[512];
		jx_escape(eid, sizeof(eid), stripped);
		jx_escape(ename, sizeof(ename), name ? name : stripped);
		jx_escape(eicon, sizeof(eicon), icon);

		if (n++) g_string_append_c(s, ',');
		g_string_append_printf(s,
			"{\"id\":\"%s\",\"name\":\"%s\",\"icon\":\"%s\",\"fav\":%s}",
			eid, ename, eicon,
			is_fav(stripped, name) ? "true" : "false");

		prerender(stripped);
		if (g_strcmp0(icon, stripped) != 0) prerender(icon);

		g_free(stripped);
		g_strfreev(icon_names);
	}
	g_string_append_c(s, ']');
	g_list_free_full(list, g_object_unref);

	g_free(apps_json);
	apps_json = g_string_free(s, FALSE);

	char *ev = g_strdup_printf("{\"t\":\"apps\",\"apps\":%s}",
		apps_json ? apps_json : "[]");
	host_emit_all(ev);
	g_free(ev);
}

/* ============================ janelas ================================= */

static void on_window_destroy(GtkWidget *w, gpointer data) {
	(void)w; (void)data;
	gtk_main_quit();
}

static HostPage *create_page(HostPage *page, const char *url,
		GtkLayerShellLayer layer, const char *ns, gboolean deck) {
	page->win = GTK_WINDOW(gtk_window_new(GTK_WINDOW_TOPLEVEL));
	gtk_window_set_decorated(page->win, FALSE);
	gtk_window_set_title(page->win, ns);

	gtk_layer_init_for_window(page->win);
	gtk_layer_set_namespace(page->win, ns);
	gtk_layer_set_layer(page->win, layer);

	if (deck) {
		/* topo, encostada no rodapé, centralizada na horizontal */
		gtk_layer_set_anchor(page->win, GTK_LAYER_SHELL_EDGE_BOTTOM, TRUE);
		gtk_layer_set_anchor(page->win, GTK_LAYER_SHELL_EDGE_TOP, FALSE);
		gtk_layer_set_anchor(page->win, GTK_LAYER_SHELL_EDGE_LEFT, FALSE);
		gtk_layer_set_anchor(page->win, GTK_LAYER_SHELL_EDGE_RIGHT, FALSE);
		gtk_layer_set_exclusive_zone(page->win, 0);
		gtk_layer_set_keyboard_mode(page->win,
			GTK_LAYER_SHELL_KEYBOARD_MODE_ON_DEMAND);
		/* superfície com canal alfa p/ o trapézio ficar transparente */
		GdkScreen *screen = gdk_screen_get_default();
		GdkVisual *visual = screen ? gdk_screen_get_rgba_visual(screen) : NULL;
		if (visual) gtk_widget_set_visual(GTK_WIDGET(page->win), visual);
		gtk_widget_set_app_paintable(GTK_WIDGET(page->win), TRUE);
		/* o tamanho de uma layer surface sem âncoras laterais só muda
		   pelo size request — gtk_window_resize é ignorado e a janela
		   nunca pinta. */
		gtk_widget_set_size_request(GTK_WIDGET(page->win), 820, 66);
	} else {
		for (int e = GTK_LAYER_SHELL_EDGE_LEFT;
				e <= GTK_LAYER_SHELL_EDGE_BOTTOM; e++)
			gtk_layer_set_anchor(page->win, (GtkLayerShellEdge)e, TRUE);
		gtk_layer_set_exclusive_zone(page->win, -1);
		gtk_layer_set_keyboard_mode(page->win,
			GTK_LAYER_SHELL_KEYBOARD_MODE_NONE);
	}

	WebKitUserContentManager *ucm = webkit_user_content_manager_new();
	g_signal_connect(ucm, "script-message-received::p",
		G_CALLBACK(on_js_message), page);
	webkit_user_content_manager_register_script_message_handler(ucm, "p");

	page->view = WEBKIT_WEB_VIEW(webkit_web_view_new_with_user_content_manager(ucm));
	g_object_unref(ucm);

	WebKitSettings *st = webkit_web_view_get_settings(page->view);
	webkit_settings_set_enable_developer_extras(st, TRUE);
	webkit_settings_set_media_playback_requires_user_gesture(st, FALSE);
	webkit_settings_set_enable_smooth_scrolling(st, TRUE);

	GdkRGBA transparent = { 0, 0, 0, 0 };
	webkit_web_view_set_background_color(page->view, &transparent);

	gtk_container_add(GTK_CONTAINER(page->win), GTK_WIDGET(page->view));
	gtk_widget_show_all(GTK_WIDGET(page->win));
	g_signal_connect(page->win, "destroy", G_CALLBACK(on_window_destroy), NULL);

	webkit_web_view_load_uri(page->view, url);
	return page;
}

/* ============================= shell dir =============================== */

static char *resolve_shell_dir(void) {
	const char *env = getenv("PADRAO_SHELL");
	if (env && *env) return g_strdup(env);

	char exe[4096];
	ssize_t n = readlink("/proc/self/exe", exe, sizeof(exe) - 1);
	if (n > 0) {
		exe[n] = '\0';
		char *dir = g_path_get_dirname(exe);
		char *cand = g_build_filename(dir, "web", NULL);
		g_free(dir);
		if (g_file_test(cand, G_FILE_TEST_IS_DIR)) return cand;
		g_free(cand);
	}
	return g_strdup("./web");
}

/* ============================== main ================================== */

static gboolean on_signal(gpointer data) {
	(void)data;
	gtk_main_quit();
	return G_SOURCE_REMOVE;
}

int main(int argc, char *argv[]) {
	gtk_init(&argc, &argv);

	g_shell_dir = resolve_shell_dir();
	char *probe = g_build_filename(g_shell_dir, "canvas", "index.html", NULL);
	if (!g_file_test(probe, G_FILE_TEST_IS_REGULAR)) {
		fprintf(stderr, "padrao-host: UI não encontrada em %s "
			"(defina PADRAO_SHELL)\n", g_shell_dir);
		return 1;
	}
	g_free(probe);

	state_dir = g_build_filename(g_get_user_data_dir(), "padrao", NULL);
	g_mkdir_with_parents(state_dir, 0755);

	g_mutex_init(&icon_lock);
	wins = g_hash_table_new_full(g_direct_hash, g_direct_equal, NULL, g_free);
	saved = g_hash_table_new_full(g_str_hash, g_str_equal, g_free, g_free);
	icon_cache = g_hash_table_new_full(g_str_hash, g_str_equal, g_free,
		(GDestroyNotify)g_bytes_unref);
	g_app_icons = g_hash_table_new_full(g_str_hash, g_str_equal, g_free,
		(GDestroyNotify)g_strfreev);

	saved_load();

	if (http_start() < 0) {
		fprintf(stderr, "padrao-host: servidor HTTP falhou (errno=%d)\n", errno);
		return 1;
	}
	ipc_start();
	apps_scan();

	char url_canvas[256], url_deck[256];
	snprintf(url_canvas, sizeof(url_canvas),
		"http://127.0.0.1:%d/canvas/", g_http_port);
	snprintf(url_deck, sizeof(url_deck),
		"http://127.0.0.1:%d/deck/", g_http_port);

	create_page(&g_canvas_page, url_canvas,
		GTK_LAYER_SHELL_LAYER_BACKGROUND, "padrao-canvas", FALSE);
	create_page(&g_deck_page, url_deck,
		GTK_LAYER_SHELL_LAYER_TOP, "padrao-deck", TRUE);

	g_unix_signal_add(SIGTERM, on_signal, NULL);
	g_unix_signal_add(SIGINT, on_signal, NULL);
	signal(SIGPIPE, SIG_IGN);

	g_print("padrao-host: UI em http://127.0.0.1:%d/ (shell=%s)\n",
		g_http_port, g_shell_dir);

	gtk_main();

	/* encerramento */
	if (g_canvas_page.win) gtk_widget_destroy(GTK_WIDGET(g_canvas_page.win));
	if (g_deck_page.win) gtk_widget_destroy(GTK_WIDGET(g_deck_page.win));
	return 0;
}
