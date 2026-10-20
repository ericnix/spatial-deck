/*
 * input.c — teclado, ponteiro e gestos de câmera.
 *
 * Atalhos globais (funcionam sobre qualquer superfície):
 *   Alt+Tab / Alt+Shift+Tab .... alterna janelas
 *   Alt+F4 ..................... fecha a janela focada
 *   Super+arrastar ............. desloca a câmera (mundo)
 *   Super+roda ................. zoom (escala de saída)
 * Todo o resto é repassado às superfícies via wlr_seat.
 */
#include <linux/input-event-codes.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <xkbcommon/xkbcommon.h>

#include <wlr/types/wlr_data_device.h>
#include <wlr/types/wlr_keyboard.h>
#include <wlr/types/wlr_pointer.h>
#include <wlr/util/log.h>

#include "padrao.h"

/* keycodes pressionados por nós (para engolir o release correspondente) */
#define HANDLED_WORDS 32
static uint32_t swallowed_keys[HANDLED_WORDS];

static bool key_is_swallowed(uint32_t keycode) {
	return !!(swallowed_keys[keycode / 32] & (1u << (keycode % 32)));
}
static void key_set_swallowed(uint32_t keycode, bool on) {
	if (on) swallowed_keys[keycode / 32] |= (1u << (keycode % 32));
	else swallowed_keys[keycode / 32] &= ~(1u << (keycode % 32));
}

/* ------------------------------------------------------------------ */
/* dono de um node da scene: janela (data) ou layer surface            */
static struct padrao_view *view_for_node(struct padrao_server *server,
		struct wlr_scene_node *node) {
	for (struct wlr_scene_node *n = node; n; ) {
		if (n->data) {
			struct padrao_view *candidate = n->data;
			struct padrao_view *v;
			wl_list_for_each(v, &server->views, link) {
				if (v == candidate) return v;
			}
		}
		n = n->parent ? &n->parent->node : NULL;
	}
	return NULL;
}

static struct wlr_layer_surface_v1 *layer_for_node(struct padrao_server *server,
		struct wlr_scene_node *node) {
	for (struct wlr_scene_node *n = node; n; ) {
		struct padrao_layer *layer;
		wl_list_for_each(layer, &server->layer_surfaces, link) {
			if (layer->scene_layer && layer->scene_layer->tree
					&& &layer->scene_layer->tree->node == n) {
				return layer->surface;
			}
		}
		n = n->parent ? &n->parent->node : NULL;
	}
	return NULL;
}

struct padrao_view *padrao_view_at(struct padrao_server *server,
		double lx, double ly, double *sx, double *sy) {
	double nx, ny;
	struct wlr_scene_node *node = wlr_scene_node_at(
		&server->scene->tree.node, lx, ly, &nx, &ny);
	if (!node) return NULL;
	struct padrao_view *view = view_for_node(server, node);
	if (view && sx) *sx = nx;
	if (view && sy) *sy = ny;
	return view;
}

/* ------------------------------------------------------------------ */
/* cursor                                                              */
static void set_default_cursor(struct padrao_server *server) {
	if (!server->cursor_client_set && server->cursor_default_set) return;
	wlr_cursor_set_xcursor(server->cursor, server->xcursor_mgr, "default");
	server->cursor_client_set = false;
	server->cursor_default_set = true;
}

static void broadcast_view(struct padrao_server *server) {
	padrao_ipc_broadcastf(server,
		"{\"t\":\"view\",\"x\":%.0f,\"y\":%.0f,\"z\":%g}",
		server->view_x, server->view_y, server->zoom);
}

/* hit-test + repasse de movimento/botões às superfícies */
static void process_cursor(struct padrao_server *server, uint32_t time_msec) {
	double nx, ny;
	struct wlr_scene_node *node = wlr_scene_node_at(
		&server->scene->tree.node, server->cursor->x, server->cursor->y,
		&nx, &ny);

	struct wlr_surface *surface = NULL;
	struct padrao_view *view = NULL;
	struct wlr_layer_surface_v1 *layer = NULL;

	if (node && node->type == WLR_SCENE_NODE_BUFFER) {
		struct wlr_scene_surface *ss = wlr_scene_surface_try_from_buffer(
			wlr_scene_buffer_from_node(node));
		if (ss) {
			surface = ss->surface;
			view = view_for_node(server, node);
			if (!view) layer = layer_for_node(server, node);
		}
	}

	if (surface != server->seat->pointer_state.focused_surface) {
		/* trocou de superfície: volta ao cursor padrão; se for layer
		   interativa, o cliente logo em seguida define o seu */
		if (!surface || !server->cursor_client_set) set_default_cursor(server);
	}

	if (surface) {
		wlr_seat_pointer_notify_enter(server->seat, surface, nx, ny);
		wlr_seat_pointer_notify_motion(server->seat, time_msec, nx, ny);
	} else {
		wlr_seat_pointer_notify_clear_focus(server->seat);
		if (!server->cursor_client_set) set_default_cursor(server);
	}
	(void)view; (void)layer;
}

static void cursor_motion(struct wl_listener *listener, void *data) {
	struct padrao_server *server =
		wl_container_of(listener, server, cursor_motion);
	struct wlr_pointer_motion_event *ev = data;
	wlr_cursor_move(server->cursor, &ev->pointer->base,
		ev->delta_x, ev->delta_y);

	if (server->grab == PADRAO_GRAB_PAN) {
		server->view_x = nearbyint(server->grab_wx
			- (server->cursor->x - server->grab_sx));
		server->view_y = nearbyint(server->grab_wy
			- (server->cursor->y - server->grab_sy));
		broadcast_view(server);
		padrao_apply_camera_all(server);
		return;
	}
	if (server->grab == PADRAO_GRAB_MOVE && server->grabbed) {
		struct padrao_view *view = server->grabbed;
		view->wx = server->grab_wx + (server->cursor->x - server->grab_sx);
		view->wy = server->grab_wy + (server->cursor->y - server->grab_sy);
		padrao_apply_camera_view(view);
		padrao_broadcast_win(server, view);
		return;
	}
	if (server->grab == PADRAO_GRAB_RESIZE && server->grabbed) {
		struct padrao_view *view = server->grabbed;
		double dx = server->cursor->x - server->grab_sx;
		double dy = server->cursor->y - server->grab_sy;
		double w = server->grab_gw, h = server->grab_gh;
		if (server->resize_edges & WLR_EDGE_RIGHT)  w += dx;
		if (server->resize_edges & WLR_EDGE_LEFT)   w -= dx;
		if (server->resize_edges & WLR_EDGE_BOTTOM) h += dy;
		if (server->resize_edges & WLR_EDGE_TOP)    h -= dy;
		if (w < 80) w = 80;
		if (h < 60) h = 60;
		if (server->resize_edges & WLR_EDGE_LEFT) {
			view->wx = server->grab_wx + (server->grab_gw - w);
		}
		if (server->resize_edges & WLR_EDGE_TOP) {
			view->wy = server->grab_wy + (server->grab_gh - h);
		}
		wlr_xdg_toplevel_set_size(view->toplevel,
			(uint32_t)w, (uint32_t)h);
		padrao_apply_camera_view(view);
		padrao_broadcast_win(server, view);
		return;
	}
	process_cursor(server, ev->time_msec);
}

static void cursor_motion_absolute(struct wl_listener *listener, void *data) {
	struct padrao_server *server =
		wl_container_of(listener, server, cursor_motion_abs);
	struct wlr_pointer_motion_absolute_event *ev = data;
	wlr_cursor_warp_absolute(server->cursor, &ev->pointer->base, ev->x, ev->y);
	if (server->grab == PADRAO_GRAB_NONE) {
		process_cursor(server, ev->time_msec);
	}
}

static void cursor_button(struct wl_listener *listener, void *data) {
	struct padrao_server *server =
		wl_container_of(listener, server, cursor_button);
	struct wlr_pointer_button_event *ev = data;
	bool pressed = ev->state == WL_POINTER_BUTTON_STATE_PRESSED;

	if (pressed && server->grab == PADRAO_GRAB_NONE
			&& ev->button == BTN_LEFT) {
		struct wlr_keyboard *kb = wlr_seat_get_keyboard(server->seat);
		uint32_t mods = kb ? wlr_keyboard_get_modifiers(kb) : 0;
		if (mods & WLR_MODIFIER_LOGO) {
			/* Super+arrastar: desloca a câmera do mundo */
			server->grab = PADRAO_GRAB_PAN;
			server->grab_sx = server->cursor->x;
			server->grab_sy = server->cursor->y;
			server->grab_wx = server->view_x;
			server->grab_wy = server->view_y;
			server->grab_button = ev->button;
			return; /* engole o press */
		}
	}

	if (!pressed) {
		bool was_grab = server->grab != PADRAO_GRAB_NONE;
		uint32_t grab_button = server->grab_button;
		server->grab = PADRAO_GRAB_NONE;
		server->grabbed = NULL;
		/* se o grab de câmera/pan começou aqui, engole o release */
		if (was_grab && grab_button == ev->button
				&& grab_button != 0) {
			return;
		}
		/* client-initiated move/resize: repassa o release normalmente */
	}

	/* ao clicar: foco + interatividade de teclado de layers */
	double nx, ny;
	struct wlr_scene_node *node = wlr_scene_node_at(
		&server->scene->tree.node, server->cursor->x, server->cursor->y,
		&nx, &ny);
	if (node && node->type == WLR_SCENE_NODE_BUFFER) {
		struct wlr_scene_surface *ss = wlr_scene_surface_try_from_buffer(
			wlr_scene_buffer_from_node(node));
		if (ss) {
			struct padrao_view *view = view_for_node(server, node);
			struct wlr_layer_surface_v1 *layer = view
				? NULL : layer_for_node(server, node);
			if (view) {
				padrao_focus_view(view);
			} else if (layer
					&& layer->current.keyboard_interactive
						!= ZWLR_LAYER_SURFACE_V1_KEYBOARD_INTERACTIVITY_NONE) {
				padrao_focus_layer(server, layer);
			}
		}
	}

	wlr_seat_pointer_notify_button(server->seat, ev->time_msec,
		ev->button, ev->state);
}

static void cursor_axis(struct wl_listener *listener, void *data) {
	struct padrao_server *server =
		wl_container_of(listener, server, cursor_axis);
	struct wlr_pointer_axis_event *ev = data;

	/* Super+roda: zoom global */
	struct wlr_keyboard *kb = wlr_seat_get_keyboard(server->seat);
	uint32_t mods = kb ? wlr_keyboard_get_modifiers(kb) : 0;
	if (mods & WLR_MODIFIER_LOGO) {
		double dir = ev->delta > 0 ? 1.0 : -1.0;
		if (dir > 0) padrao_apply_zoom(server, server->zoom * (1.0 / 1.12));
		else padrao_apply_zoom(server, server->zoom * 1.12);
		return;
	}

	wlr_seat_pointer_notify_axis(server->seat, ev->time_msec,
		ev->orientation, ev->delta, ev->delta_discrete, ev->source,
		ev->relative_direction);
}

static void cursor_frame(struct wl_listener *listener, void *data) {
	(void)data;
	struct padrao_server *server =
		wl_container_of(listener, server, cursor_frame);
	wlr_seat_pointer_notify_frame(server->seat);
}

/* ------------------------------------------------------------------ */
/* teclado                                                             */
/* listeners estáveis por teclado */
struct kb_listener {
	struct wl_listener key;
	struct wl_listener modifiers;
	struct wl_listener destroy;
	struct wlr_keyboard *kb;
	struct padrao_server *server;
};
/* até 8 teclados — suficiente; falha silenciosa além disso */
#define MAX_KB 8
static struct kb_listener kb_listeners[MAX_KB];
static int kb_count;

static void keyboard_key(struct wl_listener *listener, void *data) {
	struct kb_listener *kl = wl_container_of(listener, kl, key);
	struct padrao_server *server = kl->server;
	struct wlr_keyboard *kb = kl->kb;
	struct wlr_keyboard_key_event *ev = data;
	bool release = ev->state == WL_KEYBOARD_KEY_STATE_RELEASED;
	uint32_t keycode = ev->keycode; /* keycode Wayland (sem +8) */

	if (release) {
		if (keycode < 1024 && key_is_swallowed(keycode)) {
			key_set_swallowed(keycode, false);
			return; /* engole o release de atalho tratado */
		}
	} else {
		const xkb_keysym_t *syms;
		int nsyms = xkb_state_key_get_syms(kb->xkb_state,
			ev->keycode + 8, &syms);
		uint32_t mods = wlr_keyboard_get_modifiers(kb);
		bool alt = mods & WLR_MODIFIER_ALT;
		bool shift = mods & WLR_MODIFIER_SHIFT;

		for (int i = 0; i < nsyms; i++) {
			xkb_keysym_t sym = syms[i];
			bool handled = false;
			if (alt && sym == XKB_KEY_Tab) {
				padrao_cycle_focus(server, shift ? -1 : 1);
				handled = true;
			} else if (alt && sym == XKB_KEY_F4) {
				if (server->focused) padrao_close_view(server->focused);
				handled = true;
			}
			if (handled) {
				key_set_swallowed(keycode, true);
				return; /* engole o press */
			}
		}
	}

	wlr_seat_set_keyboard(server->seat, kb);
	wlr_seat_keyboard_notify_key(server->seat, ev->time_msec,
		ev->keycode, ev->state);
}

static void keyboard_modifiers(struct wl_listener *listener, void *data) {
	(void)data;
	struct kb_listener *kl = wl_container_of(listener, kl, modifiers);
	wlr_seat_set_keyboard(kl->server->seat, kl->kb);
	wlr_seat_keyboard_notify_modifiers(kl->server->seat, &kl->kb->modifiers);
}

/* O teclado pode sumir a qualquer momento (device removido) ou no
 * shutdown (seat destruído). O wlroots exige as listas de listeners vazias
 * em wlr_keyboard_finish() — senão aborta com
 *   "wlr_keyboard_finish: Assertion `wl_list_empty(&kb->events.key.listener_list)' failed".
 * O destroy do input device é emitido com wl_signal_emit_mutable, então dá
 * para soltar tudo durante a emissão. */
static void keyboard_destroy(struct wl_listener *listener, void *data) {
	(void)data;
	struct kb_listener *kl = wl_container_of(listener, kl, destroy);
	wl_list_remove(&kl->key.link);
	wl_list_init(&kl->key.link);
	wl_list_remove(&kl->modifiers.link);
	wl_list_init(&kl->modifiers.link);
	wl_list_remove(&kl->destroy.link);
	wl_list_init(&kl->destroy.link);
	kl->kb = NULL;
}

static void keyboard_new(struct padrao_server *server,
		struct wlr_input_device *dev) {
	struct wlr_keyboard *kb = wlr_keyboard_from_input_device(dev);

	struct xkb_context *context = xkb_context_new(XKB_CONTEXT_NO_FLAGS);
	if (!context) return;
	struct xkb_rule_names rules = { 0 };
	rules.model = "pc105";
	rules.layout = getenv("PADRAO_KB_LAYOUT")
		? getenv("PADRAO_KB_LAYOUT")
		: (getenv("XKB_DEFAULT_LAYOUT") ? getenv("XKB_DEFAULT_LAYOUT") : "us");
	rules.variant = getenv("PADRAO_KB_VARIANT");
	rules.options = getenv("PADRAO_KB_OPTIONS");
	struct xkb_keymap *keymap = xkb_keymap_new_from_names(context,
		&rules, XKB_KEYMAP_COMPILE_NO_FLAGS);
	if (keymap) {
		wlr_keyboard_set_keymap(kb, keymap);
		xkb_keymap_unref(keymap);
	}
	xkb_context_unref(context);
	wlr_keyboard_set_repeat_info(kb, 25, 600);

	if (kb_count < MAX_KB) {
		struct kb_listener *kl = &kb_listeners[kb_count++];
		kl->kb = kb;
		kl->server = server;
		kl->key.notify = keyboard_key;
		wl_signal_add(&kb->events.key, &kl->key);
		kl->modifiers.notify = keyboard_modifiers;
		wl_signal_add(&kb->events.modifiers, &kl->modifiers);
		kl->destroy.notify = keyboard_destroy;
		wl_signal_add(&kb->base.events.destroy, &kl->destroy);
	}
	wlr_seat_set_keyboard(server->seat, kb);
}

/* ------------------------------------------------------------------ */
static void handle_new_input(struct wl_listener *listener, void *data) {
	struct padrao_server *server =
		wl_container_of(listener, server, new_input);
	struct wlr_input_device *dev = data;
	switch (dev->type) {
	case WLR_INPUT_DEVICE_KEYBOARD:
		keyboard_new(server, dev);
		break;
	case WLR_INPUT_DEVICE_POINTER:
		wlr_cursor_attach_input_device(server->cursor, dev);
		break;
	default:
		/* touch/tablet: fora do escopo v1 */
		break;
	}
	/* mantém as capabilities do seat coerentes */
	uint32_t caps = WL_SEAT_CAPABILITY_KEYBOARD;
	if (server->cursor) caps |= WL_SEAT_CAPABILITY_POINTER;
	wlr_seat_set_capabilities(server->seat, caps);
}

static void handle_request_set_cursor(struct wl_listener *listener, void *data) {
	struct padrao_server *server =
		wl_container_of(listener, server, request_set_cursor);
	struct wlr_seat_pointer_request_set_cursor_event *ev = data;
	if (server->seat->pointer_state.focused_client == ev->seat_client) {
		wlr_cursor_set_surface(server->cursor, ev->surface,
			ev->hotspot_x, ev->hotspot_y);
		server->cursor_client_set = ev->surface != NULL;
		server->cursor_default_set = false;
		if (!ev->surface) set_default_cursor(server);
	}
}

static void handle_request_set_selection(struct wl_listener *listener,
		void *data) {
	struct padrao_server *server =
		wl_container_of(listener, server, request_set_selection);
	struct wlr_seat_request_set_selection_event *ev = data;
	wlr_seat_set_selection(server->seat, ev->source, ev->serial);
}

void padrao_input_init(struct padrao_server *server) {
	server->new_input.notify = handle_new_input;
	wl_signal_add(&server->backend->events.new_input, &server->new_input);

	server->cursor_motion.notify = cursor_motion;
	wl_signal_add(&server->cursor->events.motion, &server->cursor_motion);
	server->cursor_motion_abs.notify = cursor_motion_absolute;
	wl_signal_add(&server->cursor->events.motion_absolute,
		&server->cursor_motion_abs);
	server->cursor_button.notify = cursor_button;
	wl_signal_add(&server->cursor->events.button, &server->cursor_button);
	server->cursor_axis.notify = cursor_axis;
	wl_signal_add(&server->cursor->events.axis, &server->cursor_axis);
	server->cursor_frame.notify = cursor_frame;
	wl_signal_add(&server->cursor->events.frame, &server->cursor_frame);

	server->request_set_cursor.notify = handle_request_set_cursor;
	wl_signal_add(&server->seat->events.request_set_cursor,
		&server->request_set_cursor);
	server->request_set_selection.notify = handle_request_set_selection;
	wl_signal_add(&server->seat->events.request_set_selection,
		&server->request_set_selection);

	wlr_xcursor_manager_load(server->xcursor_mgr, 24);
	set_default_cursor(server);
}
