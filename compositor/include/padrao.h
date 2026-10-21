/*
 * padrao.h — estruturas e protótipos do compositor Padrão.
 *
 * Modelo do desktop:
 *   • Mundo infinito em unidades lógicas; a câmera é {view_x, view_y, zoom}.
 *   • zoom == escala de saída (wlr_output_state_set_scale): ao aplicar zoom,
 *     o viewport lógico cresce/reduz e TODO o conteúdo (janelas, canvas,
 *     deck) escala junto, nítido, sem matemática por superfície.
 *   • Tela(lógica) = mundo - view. Logo, posição de janela na scene:
 *     node = round(mundo - view) — tudo aditivo, sem floats na scene.
 *   • O canvas (superfície de fundo) desenha o mesmo mundo em CSS; o deck é
 *     uma layer surface de topo redimensionável.
 *   • IPC: socket Unix ($XDG_RUNTIME_DIR/padrao.sock), linhas JSON, feito
 *     pelo shell-host (GTK+WebKit), que também serve a UI web por HTTP.
 */
#ifndef PADRAO_H
#define PADRAO_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <pthread.h>
#include <wayland-server-core.h>

#include <wlr/backend.h>
#include <wlr/render/allocator.h>
#include <wlr/render/wlr_renderer.h>
#include <wlr/types/wlr_compositor.h>
#include <wlr/types/wlr_cursor.h>
#include <wlr/types/wlr_layer_shell_v1.h>
#include <wlr/types/wlr_output.h>
#include <wlr/types/wlr_output_layout.h>
#include <wlr/types/wlr_presentation_time.h>
#include <wlr/types/wlr_scene.h>
#include <wlr/types/wlr_seat.h>
#include <wlr/types/wlr_xcursor_manager.h>
#include <wlr/types/wlr_xdg_shell.h>

#define PADRAO_SOCK_NAME "padrao"

/* camadas wlr_layer_shell v1 (ZWLR_LAYER_SHELL_V1_LAYER_*) */
enum padrao_layer_kind {
	PADRAO_LAYER_BACKGROUND = 0,
	PADRAO_LAYER_BOTTOM,
	PADRAO_LAYER_NORMAL,
	PADRAO_LAYER_TOP,
	PADRAO_LAYER_OVERLAY,
	PADRAO_LAYER_COUNT,
};

struct padrao_server;
struct padrao_output;
struct padrao_view;
struct padrao_layer;

/* ------------------------------------------------------------------ */
struct padrao_server {
	struct wl_display *display;
	struct wl_event_loop *loop;
	struct wlr_backend *backend;
	struct wlr_session *session;
	struct wlr_renderer *renderer;
	struct wlr_allocator *allocator;
	struct wlr_output_layout *layout;
	struct wlr_scene_output_layout *scene_layout;
	struct wlr_scene *scene;
	struct wlr_scene_tree *layers[PADRAO_LAYER_COUNT];
	struct wlr_scene_tree *views_tree;
	struct wlr_seat *seat;
	struct wlr_cursor *cursor;
	struct wlr_xcursor_manager *xcursor_mgr;
	struct wlr_xdg_shell *xdg_shell;
	struct wlr_layer_shell_v1 *layer_shell;
	struct wlr_presentation *presentation;

	/* câmera do mundo (unidades lógicas inteiras) */
	double view_x, view_y;
	double zoom; /* 0.5 .. 3.0 — aplicado como escala de saída */

	/* transformação vinda do canvas (zoom + deslocamento em px de tela):
	 *   screen_x = (window_x * scale) + offset_x
	 *   screen_y = (window_y * scale) + offset_y */
	double canvas_scale;
	double canvas_off_x;
	double canvas_off_y;

	/* estado de interação */
	enum { PADRAO_GRAB_NONE, PADRAO_GRAB_MOVE, PADRAO_GRAB_RESIZE,
	       PADRAO_GRAB_PAN } grab;
	struct padrao_view *grabbed;
	double grab_sx, grab_sy; /* cursor na origem do grab (lógico) */
	double grab_wx, grab_wy; /* posição do mundo no início do grab */
	double grab_gw, grab_gh; /* geometria no início do resize */
	uint32_t grab_button;    /* botão que originou o grab */
	uint32_t resize_edges;
	bool cursor_client_set;  /* imagem de cursor definida por cliente */
	bool cursor_default_set; /* cursor padrão já aplicado */

	struct wl_list outputs; /* padrao_output.link */
	struct wl_list views;   /* padrao_view.link   */
	struct wl_list layer_surfaces; /* padrao_layer.link */
	struct padrao_view *focused;
	struct wlr_surface *focused_layer; /* layer surface com teclado */
	uint32_t focus_serial;             /* p/ ciclo Alt+Tab */

	/* IPC com o shell-host */
	int ipc_listen_fd;
	struct wl_event_source *ipc_listen_src;
	int ipc_fd;
	struct wl_event_source *ipc_conn_src;
	char ipc_in[16384];
	size_t ipc_in_len;
	char *ipc_out;
	size_t ipc_out_len, ipc_out_sent;
	pid_t host_pid;

	/* camada web (WebKitGTK): thread própria, cliente do próprio
	 * compositor — ver main.c */
	volatile bool running;
	volatile bool finished;   /* pediu para encerrar */
	pthread_t web_thread;
	bool web_thread_started;

	/* listeners do servidor */
	struct wl_listener display_destroy;
	struct wl_listener new_output;
	struct wl_listener new_input;
	struct wl_listener cursor_motion;
	struct wl_listener cursor_motion_abs;
	struct wl_listener cursor_button;
	struct wl_listener cursor_axis;
	struct wl_listener cursor_frame;
	struct wl_listener request_set_cursor;
	struct wl_listener request_set_selection;
	struct wl_listener new_toplevel;
	struct wl_listener new_layer;
};

/* ------------------------------------------------------------------ */
struct padrao_output {
	struct padrao_server *server;
	struct wlr_output *wlr_output;
	struct wlr_scene_output *scene_output;
	struct wl_list link;
	struct wl_listener frame;
	struct wl_listener request_state;
	struct wl_listener destroy;
	bool layers_dirty;
};

struct padrao_view {
	struct padrao_server *server;
	struct wlr_xdg_toplevel *toplevel;
	struct wlr_scene_tree *tree;
	double wx, wy; /* posição no mundo (canto da geometria xdg) */
	bool mapped;
	struct wl_list link;

	struct wl_listener map;
	struct wl_listener unmap;
	struct wl_listener destroy;
	struct wl_listener commit;
	/* toplevel->events.destroy dispara antes de destroy_xdg_toplevel()
	   verificar que os listeners do toplevel foram desanexados; o destroy
	   da xdg_surface (view->destroy) só vem depois. */
	struct wl_listener toplevel_destroy;
	struct wl_listener request_move;
	struct wl_listener request_resize;
	struct wl_listener set_title;
	struct wl_listener set_app_id;
};

struct padrao_layer {
	struct padrao_server *server;
	struct wlr_scene_layer_surface_v1 *scene_layer;
	struct wlr_layer_surface_v1 *surface;
	struct padrao_output *output;
	struct wl_list link;
	struct wl_listener commit;
	struct wl_listener destroy;
	bool have_size; /* já enviamos um configure com tamanho */
	int layer;      /* camada interna (PADRAO_LAYER_*) */
};

/* ------------------------------------------------------------------ */
/* main.c */
void padrao_schedule_frame(struct padrao_server *server);
void padrao_request_quit(struct padrao_server *server);
/* view.c */
void padrao_apply_canvas_transform(struct padrao_server *server);
void padrao_apply_cluster(struct padrao_server *server, const char *placements);

/* output.c */
void padrao_handle_new_output(struct wl_listener *listener, void *data);
void padrao_arrange_layers(struct padrao_output *output);
void padrao_apply_zoom(struct padrao_server *server, double zoom);
void padrao_broadcast_output(struct padrao_server *server);

/* view.c */
void padrao_handle_new_toplevel(struct wl_listener *listener, void *data);
void padrao_focus_view(struct padrao_view *view);
void padrao_focus_layer(struct padrao_server *server,
	struct wlr_layer_surface_v1 *layer_surface);
void padrao_close_view(struct padrao_view *view);
void padrao_broadcast_win(struct padrao_server *server, struct padrao_view *view);
void padrao_apply_camera_view(struct padrao_view *view);
void padrao_apply_camera_all(struct padrao_server *server);
struct padrao_view *padrao_view_at(struct padrao_server *server,
	double lx, double ly, double *sx, double *sy);
void padrao_cycle_focus(struct padrao_server *server, int dir);

/* input.c */
void padrao_input_init(struct padrao_server *server);

/* layer.c */
void padrao_handle_new_layer(struct wl_listener *listener, void *data);

/* ipc.c */
void padrao_ipc_init(struct padrao_server *server);
void padrao_ipc_broadcast(struct padrao_server *server, const char *json);
void padrao_ipc_broadcastf(struct padrao_server *server, const char *fmt, ...);

/* util.c (JSON mínimo sem dependências) */
size_t padrao_json_escape(char *dst, size_t cap, const char *src);
/* extrai valor de "key": "texto" ou "key": número — true se achou */
bool padrao_json_get_str(const char *json, const char *key, char *out, size_t cap);
bool padrao_json_get_num(const char *json, const char *key, double *out);
bool padrao_json_get_int(const char *json, const char *key, long *out);
/* extrai o valor bruto de "key": <objeto|array|...> (offset+length em json) */
bool padrao_json_get_raw(const char *json, const char *key, const char **start, size_t *len);

/* mundo <-> tela (lógico) */
static inline double padrao_world_to_screen_x(struct padrao_server *s, double wx) {
	return wx - s->view_x;
}
static inline double padrao_world_to_screen_y(struct padrao_server *s, double wy) {
	return wy - s->view_y;
}

#endif
