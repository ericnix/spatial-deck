/*
 * view.c — janelas (xdg-shell): ciclo de vida, posição no mundo, câmera e
 * foco.
 *
 * Espaço do mundo (lógico) = tela + câmera. Toda vez que a câmera ou a
 * posição muda, o node da scene vai para round(mundo - view). Não há
 * escalamento por janela: o zoom é escala de saída (ver output.c).
 */
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <wlr/util/log.h>

#include "padrao.h"

/* ---------------------------- helpers ------------------------------- */

/* wayland-util.h não tem is_first/is_last (nunca teve em libwayland) */
static inline bool list_is_last(struct wl_list *elem, struct wl_list *head) {
	return elem->next == head;
}
static inline bool list_is_first(struct wl_list *elem, struct wl_list *head) {
	return elem == head->next;
}

/* Posicao de uma janela na tela, a partir do transform do canvas:
 *   screen_x = (window_x * scale) + offset_x
 *   screen_y = (window_y * scale) + offset_y
 * O no do scene graph usa coordenadas LOGICAS (a escala da saida aplica
 * o resto), por isso a divisao pela escala. */
void padrao_apply_camera_view(struct padrao_view *view) {
	if (!view->tree) return;
	struct padrao_server *server = view->server;
	double scale = server->canvas_scale > 0 ? server->canvas_scale : 1.0;
	double sx = view->wx * scale + server->canvas_off_x;
	double sy = view->wy * scale + server->canvas_off_y;
	wlr_scene_node_set_position(&view->tree->node,
		(int)lround(sx / scale), (int)lround(sy / scale));
}

void padrao_apply_camera_all(struct padrao_server *server) {
	struct padrao_view *view;
	wl_list_for_each(view, &server->views, link) {
		padrao_apply_camera_view(view);
	}
	padrao_schedule_frame(server);
}

/* Reposiciona as janelas conforme a transformação do canvas:
 *
 *   screen_x = (window_x * scale) + offset_x
 *   screen_y = (window_y * scale) + offset_y
 *
 * A posição do nó do scene graph é LÓGICA (a escala da saída aplica o
 * resto), por isso dividimos pela escala antes de posicionar. O offset
 * vem em px de tela, então também é convertido. */
void padrao_apply_canvas_transform(struct padrao_server *server) {
	struct padrao_view *view;
	wl_list_for_each(view, &server->views, link) {
		padrao_apply_camera_view(view);
	}
	padrao_schedule_frame(server);
}

/* Cluster/tiling: reposiciona um conjunto de janelas de uma vez.
 * O placements é o texto JSON de uma lista:
 *   [{"id":1,"x":10,"y":20}, ...]
 * As coordenadas são do mundo (as mesmas do canvas). */
void padrao_apply_cluster(struct padrao_server *server, const char *placements) {
	if (!placements) return;
	const char *p = placements;
	while ((p = strchr(p, '{')) != NULL) {
		const char *end = strchr(p, '}');
		if (!end) break;
		char obj[512];
		size_t n = (size_t)(end - p) + 1;
		if (n >= sizeof(obj)) n = sizeof(obj) - 1;
		memcpy(obj, p, n);
		obj[n] = '\0';

		long id;
		double x, y;
		if (padrao_json_get_int(obj, "id", &id)
				&& padrao_json_get_num(obj, "x", &x)
				&& padrao_json_get_num(obj, "y", &y)) {
			struct padrao_view *view;
			wl_list_for_each(view, &server->views, link) {
				if ((long)(uintptr_t)view != id) continue;
				view->wx = x;
				view->wy = y;
				padrao_apply_camera_view(view);
				padrao_broadcast_win(server, view);
				break;
			}
		}
		p = end + 1;
	}
	padrao_schedule_frame(server);
}

static void broadcast_win(struct padrao_server *server, struct padrao_view *view) {
	struct wlr_box geo = { 0 };
	if (view->toplevel->base->surface->mapped) {
		geo = view->toplevel->base->geometry;
	}
	char title[512], app[256];
	padrao_json_escape(title, sizeof(title),
		view->toplevel->title ? view->toplevel->title : "");
	padrao_json_escape(app, sizeof(app),
		view->toplevel->app_id ? view->toplevel->app_id : "");
	padrao_ipc_broadcastf(server,
		"{\"t\":\"win\",\"id\":%ld,\"app\":\"%s\",\"title\":\"%s\","
		"\"x\":%.0f,\"y\":%.0f,\"w\":%d,\"h\":%d,\"focused\":%s}",
		(long)(uintptr_t)view, app, title,
		view->wx, view->wy, geo.width, geo.height,
		(server->focused == view) ? "true" : "false");
}

void padrao_broadcast_win(struct padrao_server *server, struct padrao_view *view) {
	broadcast_win(server, view);
}

/* ------------------------------ foco -------------------------------- */

void padrao_focus_view(struct padrao_view *view) {
	struct padrao_server *server = view->server;
	struct wlr_surface *prev =
		server->seat->keyboard_state.focused_surface;

	if (prev == view->toplevel->base->surface) return;
	/* desativa a anterior */
	if (server->focused && server->focused != view
			&& server->focused->mapped) {
		wlr_xdg_toplevel_set_activated(server->focused->toplevel, false);
	}
	server->focused = view;
	server->focused_layer = NULL;

	wlr_scene_node_raise_to_top(&view->tree->node);
	wlr_xdg_toplevel_set_activated(view->toplevel, true);

	struct wlr_keyboard *kb = wlr_seat_get_keyboard(server->seat);
	if (kb) {
		wlr_seat_keyboard_notify_enter(server->seat,
			view->toplevel->base->surface,
			kb->keycodes, kb->num_keycodes, &kb->modifiers);
	} else {
		wlr_seat_keyboard_notify_enter(server->seat,
			view->toplevel->base->surface, NULL, 0, NULL);
	}
	broadcast_win(server, view);
	/* as demais janelas mudam "focused" para false: reenvia todas */
	struct padrao_view *v;
	wl_list_for_each(v, &server->views, link) {
		if (v != view && v->mapped) broadcast_win(server, v);
	}
}

void padrao_close_view(struct padrao_view *view) {
	wlr_xdg_toplevel_send_close(view->toplevel);
}

/* Foco de teclado em layer surface (deck com busca, painis etc). */
void padrao_focus_layer(struct padrao_server *server,
		struct wlr_layer_surface_v1 *layer_surface) {
	struct wlr_keyboard *kb = wlr_seat_get_keyboard(server->seat);
	struct wlr_surface *surf = layer_surface->surface;
	if (server->seat->keyboard_state.focused_surface == surf) return;

	/* desativa janela focada, se houver */
	if (server->focused && server->focused->mapped) {
		wlr_xdg_toplevel_set_activated(server->focused->toplevel, false);
	}
	server->focused = NULL;
	server->focused_layer = surf;
	if (kb) {
		wlr_seat_keyboard_notify_enter(server->seat, surf,
			kb->keycodes, kb->num_keycodes, &kb->modifiers);
	} else {
		wlr_seat_keyboard_notify_enter(server->seat, surf, NULL, 0, NULL);
	}
}

/* --------------------------- Alt+Tab -------------------------------- */

void padrao_cycle_focus(struct padrao_server *server, int dir) {
	if (wl_list_empty(&server->views)) return;
	struct padrao_view *start = server->focused;
	struct padrao_view *view = start;

	do {
		if (dir > 0) {
			view = view
				? (list_is_last(&view->link, &server->views)
					? wl_container_of(server->views.next, view, link)
					: wl_container_of(view->link.next, view, link))
				: wl_container_of(server->views.next, view, link);
		} else {
			view = view
				? (list_is_first(&view->link, &server->views)
					? wl_container_of(server->views.prev, view, link)
					: wl_container_of(view->link.prev, view, link))
				: wl_container_of(server->views.prev, view, link);
		}
		if (view && view->mapped) {
			padrao_focus_view(view);
			return;
		}
	} while (view != start);
}

/* ------------------------ ciclo de vida ------------------------------ */

static void view_commit(struct wl_listener *listener, void *data) {
	(void)data;
	struct padrao_view *view = wl_container_of(listener, view, commit);
	struct wlr_xdg_surface *xdg = view->toplevel->base;

	/* wlroots exige um configure na primeira commit do toplevel */
	if (xdg->initial_commit) {
		wlr_xdg_surface_schedule_configure(xdg);
		return;
	}
	if (view->mapped) broadcast_win(view->server, view);
}

static void view_map(struct wl_listener *listener, void *data) {
	(void)data;
	struct padrao_view *view = wl_container_of(listener, view, map);
	struct padrao_server *server = view->server;
	view->mapped = true;
	wl_list_insert(&server->views, &view->link);

	/* posiciona no centro da área visível (mundo) */
	struct wlr_box box = { 0 };
	wlr_output_layout_get_box(server->layout, NULL, &box);
	struct wlr_box geo = view->toplevel->base->geometry;
	if (geo.width == 0) { geo.width = 800; geo.height = 600; }
	view->wx = server->view_x + (box.width - geo.width) / 2.0;
	view->wy = server->view_y + (box.height - geo.height) / 2.0;
	/* pequena escalonada para várias janelas novas não se empilharem */
	struct padrao_view *v;
	int n = 0;
	wl_list_for_each(v, &server->views, link) {
		if (v != view) n++;
	}
	view->wx += (n % 8) * 28;
	view->wy += (n % 8) * 28;

	padrao_apply_camera_view(view);
	padrao_focus_view(view);
	broadcast_win(server, view);
}

static void view_unmap(struct wl_listener *listener, void *data) {
	(void)data;
	struct padrao_view *view = wl_container_of(listener, view, unmap);
	struct padrao_server *server = view->server;
	view->mapped = false;
	if (server->focused == view) server->focused = NULL;
	if (server->grabbed == view) {
		server->grabbed = NULL;
		server->grab = PADRAO_GRAB_NONE;
	}
	wl_list_remove(&view->link);
	wl_list_init(&view->link);
	padrao_ipc_broadcastf(server, "{\"t\":\"win_gone\",\"id\":%ld}",
		(long)(uintptr_t)view);
	/* foca a próxima mapeada */
	struct padrao_view *v;
	wl_list_for_each(v, &server->views, link) {
		if (v->mapped) { padrao_focus_view(v); break; }
	}
}

static void view_toplevel_destroy(struct wl_listener *listener, void *data) {
	(void)data;
	struct padrao_view *view =
		wl_container_of(listener, view, toplevel_destroy);
	/* destroy_xdg_toplevel() emite este evento e só então verifica (com
	   assert) que nenhum listener do toplevel sobrou. view_destroy —
	   ligado ao destroy da xdg_surface — ainda não rodou, então quem
	   remove os listeners do toplevel é este handler. O próprio listener
	   precisa sair da lista: o assert cobre events.destroy também. */
	wl_list_remove(&view->toplevel_destroy.link);
	wl_list_init(&view->toplevel_destroy.link);
	wl_list_remove(&view->request_move.link);
	wl_list_init(&view->request_move.link);
	wl_list_remove(&view->request_resize.link);
	wl_list_init(&view->request_resize.link);
	wl_list_remove(&view->set_title.link);
	wl_list_init(&view->set_title.link);
	wl_list_remove(&view->set_app_id.link);
	wl_list_init(&view->set_app_id.link);
}

static void view_destroy(struct wl_listener *listener, void *data) {
	(void)data;
	struct padrao_view *view = wl_container_of(listener, view, destroy);
	wl_list_remove(&view->map.link);
	wl_list_remove(&view->unmap.link);
	wl_list_remove(&view->destroy.link);
	wl_list_remove(&view->commit.link);
	wl_list_remove(&view->toplevel_destroy.link);
	/* já desanexados (e re-inicializados) por view_toplevel_destroy() */
	wl_list_remove(&view->request_move.link);
	wl_list_remove(&view->request_resize.link);
	wl_list_remove(&view->set_title.link);
	wl_list_remove(&view->set_app_id.link);
	if (view->mapped) wl_list_remove(&view->link);
	free(view);
}

static void view_request_move(struct wl_listener *listener, void *data) {
	(void)data;
	struct padrao_view *view = wl_container_of(listener, view, request_move);
	struct padrao_server *server = view->server;
	/* só inicia move se há um botão pressionado (padrão xdg) */
	if (server->seat->pointer_state.button_count == 0) return;
	server->grab = PADRAO_GRAB_MOVE;
	server->grabbed = view;
	server->grab_wx = view->wx;
	server->grab_wy = view->wy;
	server->grab_sx = server->cursor->x;
	server->grab_sy = server->cursor->y;
}

static void view_request_resize(struct wl_listener *listener, void *data) {
	struct wlr_xdg_toplevel_resize_event *ev = data;
	struct padrao_view *view = wl_container_of(listener, view, request_resize);
	struct padrao_server *server = view->server;
	if (server->seat->pointer_state.button_count == 0) return;
	server->grab = PADRAO_GRAB_RESIZE;
	server->grabbed = view;
	server->grab_sx = server->cursor->x;
	server->grab_sy = server->cursor->y;
	server->grab_wx = view->wx;
	server->grab_wy = view->wy;
	server->resize_edges = ev->edges;
}

static void view_set_title(struct wl_listener *listener, void *data) {
	(void)data;
	struct padrao_view *view = wl_container_of(listener, view, set_title);
	if (view->mapped) broadcast_win(view->server, view);
}

static void view_set_app_id(struct wl_listener *listener, void *data) {
	(void)data;
	struct padrao_view *view = wl_container_of(listener, view, set_app_id);
	if (view->mapped) broadcast_win(view->server, view);
}

void padrao_handle_new_toplevel(struct wl_listener *listener, void *data) {
	struct wlr_xdg_toplevel *toplevel = data;
	struct padrao_server *server =
		wl_container_of(listener, server, new_toplevel);

	/* new_toplevel só entrega toplevels; a base é a xdg_surface da janela */
	struct wlr_xdg_surface *xdg = toplevel->base;
	if (!xdg) return;

	struct padrao_view *view = calloc(1, sizeof(*view));
	if (!view) return;
	view->server = server;
	view->toplevel = toplevel;
	view->tree = wlr_scene_xdg_surface_create(server->views_tree, xdg);
	view->wx = 0;
	view->wy = 0;
	wl_list_init(&view->link);

	view->map.notify = view_map;
	wl_signal_add(&xdg->surface->events.map, &view->map);
	view->unmap.notify = view_unmap;
	wl_signal_add(&xdg->surface->events.unmap, &view->unmap);
	view->commit.notify = view_commit;
	wl_signal_add(&xdg->surface->events.commit, &view->commit);
	view->destroy.notify = view_destroy;
	wl_signal_add(&xdg->events.destroy, &view->destroy);
	view->toplevel_destroy.notify = view_toplevel_destroy;
	wl_signal_add(&xdg->toplevel->events.destroy, &view->toplevel_destroy);
	view->request_move.notify = view_request_move;
	wl_signal_add(&xdg->toplevel->events.request_move, &view->request_move);
	view->request_resize.notify = view_request_resize;
	wl_signal_add(&xdg->toplevel->events.request_resize, &view->request_resize);
	view->set_title.notify = view_set_title;
	wl_signal_add(&xdg->toplevel->events.set_title, &view->set_title);
	view->set_app_id.notify = view_set_app_id;
	wl_signal_add(&xdg->toplevel->events.set_app_id, &view->set_app_id);

	if (xdg->initial_commit) {
		wlr_xdg_surface_schedule_configure(xdg);
	}
}
