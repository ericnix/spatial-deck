/*
 * padrao_host.h — estruturas e protótipos do shell-host (GTK + WebKit).
 *
 * Papéis do host:
 *   • duas layer surfaces (canvas de fundo, deck de topo) com as páginas
 *     do web/ carregadas por HTTP local;
 *   • ponte JS <-> compositor via socket Unix ($XDG_RUNTIME_DIR/padrao.sock);
 *   • servidor HTTP para os assets da UI, mídia (/m?path=) e ícones (/i/);
 *   • aplicativos (g_app_info), seletor de arquivos e persistência.
 *
 * Mensagens JS -> host (webkit.messageHandlers.p):
 *   ready, view, win_move, win_focus, win_close, save, pick, deck_size, launch
 * Host -> JS: Padrao.emit(<json>)
 */
#ifndef PADRAO_HOST_H
#define PADRAO_HOST_H

#include <glib.h>
#include <gtk/gtk.h>
#include <gtk-layer-shell.h>
/* o RPM do Fedora não traz <webkit/webkit.h>: o guarda-chuva é webkit2 */
#include <webkit2/webkit2.h>

typedef struct HostPage {
	const char *name; /* "canvas" | "deck" */
	GtkWindow *win;
	WebKitWebView *view;
	gboolean ready; /* enviou {t:'ready'} */
} HostPage;

extern HostPage g_canvas_page;
extern HostPage g_deck_page;
extern char *g_shell_dir; /* raiz do web/ (assets da UI) */
extern int g_http_port;   /* porta 127.0.0.1 do servidor HTTP */

/* ---------------- jx.c: JSON mínimo (sem dependências) ---------------- */
size_t jx_escape(char *dst, size_t cap, const char *src);
gboolean jx_str(const char *json, const char *key, char *out, size_t cap);
gboolean jx_num(const char *json, const char *key, double *out);
gboolean jx_int(const char *json, const char *key, long *out);
gboolean jx_raw(const char *json, const char *key,
	const char **start, size_t *len);

/* ---------------- main.c: ponte e estado ---------------- */
HostPage *host_page_by_name(const char *name);
void host_emit(HostPage *page, const char *json);
void host_emit_all(const char *json);
void host_debug_emit(const char *page_name, const char *json);
void host_send_init(HostPage *page);
void host_handle_js(HostPage *page, const char *json);
void host_on_compositor_line(const char *line);

/* estado vindo do compositor (mantido em memória para o 'init') */
void state_set_view(const char *json);
void state_set_out(const char *json);
void state_set_wins(const char *arr_json);
void state_win_add(const char *obj_json);
void state_win_del(long id);
char *state_build_wins(void); /* "[...]" (g_free) */
char *state_build_saved(void); /* {"canvas":...} (g_free) */

/* ações */
void deck_resize(int w, int h);
void do_pick(const char *kind, const char *page_name);
void do_launch(const char *app_id);

/* ícones: PNG renderizado no tema (thread-safe; pode ir ao main loop) */
char *host_icon_png(const char *key, int size, gsize *len);

/* ---------------- ipc.c: socket com o compositor ---------------- */
void ipc_start(void);
void host_send_compositor(const char *fmt, ...) G_GNUC_PRINTF(1, 2);

/* ---------------- http.c: servidor local ---------------- */
int http_start(void);

#endif
