/*
 * layer.c — layer-shell (superfícies de painel): o canvas de fundo e o deck
 * de topo são criados pelo shell-host por aqui; waybar/swaybg também funcionam.
 *
 * O arrange é disparado quando uma superfície commita algo de layout
 * (current.committed != 0) ou no primeiro configure — assim não há loop de
 * configure ↔ commit.
 */
#include <stdlib.h>

#include <wlr/util/log.h>

#include "padrao.h"

/* O protocolo layer-shell tem 4 camadas (background=0, bottom=1, top=2,
 * overlay=3). O compositor tem 5: NORMAL fica no meio, onde vivem as
 * janelas xdg-toplevel. Sem este mapeia, o deck (protocolo top=2) caía
 * na camada NORMAL, junto das janelas. */
static int layer_from_protocol(int proto_layer) {
	switch (proto_layer) {
	case 0: return PADRAO_LAYER_BACKGROUND;
	case 1: return PADRAO_LAYER_BOTTOM;
	case 2: return PADRAO_LAYER_TOP;
	case 3: return PADRAO_LAYER_OVERLAY;
	default: return PADRAO_LAYER_NORMAL;
	}
}

static void layer_commit(struct wl_listener *listener, void *data) {
	(void)data;
	struct padrao_layer *layer = wl_container_of(listener, layer, commit);
	if (!layer->output) return;
	/* o cliente pode trocar a camada da superfície */
	int now = layer_from_protocol((int)layer->surface->current.layer);
	if (now != layer->layer) {
		layer->layer = now;
		if (layer->output) layer->output->layers_dirty = true;
	}
	if (layer->surface->current.committed != 0 || !layer->have_size) {
		padrao_arrange_layers(layer->output);
	}
}

static void layer_destroy(struct wl_listener *listener, void *data) {
	(void)data;
	struct padrao_layer *layer = wl_container_of(listener, layer, destroy);
	struct padrao_output *output = layer->output;

	wl_list_remove(&layer->commit.link);
	wl_list_remove(&layer->destroy.link);
	wl_list_remove(&layer->link);
	free(layer);

	if (output) {
		output->layers_dirty = true;
		padrao_arrange_layers(output);
	}
}

void padrao_handle_new_layer(struct wl_listener *listener, void *data) {
	struct wlr_layer_surface_v1 *surface = data;
	struct padrao_server *server =
		wl_container_of(listener, server, new_layer);

	/* escolhe a saída: a pedida pelo cliente, senão a primeira */
	struct padrao_output *output = NULL;
	struct padrao_output *o;
	if (surface->output) {
		wl_list_for_each(o, &server->outputs, link) {
			if (o->wlr_output == surface->output) { output = o; break; }
		}
	}
	if (!output && !wl_list_empty(&server->outputs)) {
		output = wl_container_of(server->outputs.next, output, link);
		surface->output = output->wlr_output;
	}

	int wm = layer_from_protocol((int)surface->pending.layer);
	if (wm < 0 || wm >= PADRAO_LAYER_COUNT) wm = PADRAO_LAYER_NORMAL;

	struct padrao_layer *layer = calloc(1, sizeof(*layer));
	if (!layer) return;
	layer->server = server;
	layer->surface = surface;
	layer->output = output;
	layer->layer = wm;
	layer->scene_layer =
		wlr_scene_layer_surface_v1_create(server->layers[wm], surface);

	layer->commit.notify = layer_commit;
	wl_signal_add(&surface->surface->events.commit, &layer->commit);
	layer->destroy.notify = layer_destroy;
	wl_signal_add(&surface->events.destroy, &layer->destroy);

	wl_list_insert(&server->layer_surfaces, &layer->link);

	wlr_log(WLR_INFO, "layer surface: ns=%s layer=%d %dx%d",
		surface->namespace ? surface->namespace : "?",
		(int)surface->pending.layer,
		surface->pending.desired_width, surface->pending.desired_height);

	if (output) {
		output->layers_dirty = true;
		/* o primeiro commit (sem buffer) chama layer_commit → configure */
	}
}

/* Re-posiciona todas as layer surfaces da saída, acumulando zonas
   exclusivas na ordem: fundo → bottom → normal → topo → overlay. */
void padrao_arrange_layers(struct padrao_output *output) {
	if (!output || !output->server) return;
	struct padrao_server *server = output->server;

	struct wlr_box full = { 0 };
	wlr_output_layout_get_box(server->layout, output->wlr_output, &full);

	struct wlr_box usable = full;
	/* ordem clássica: fundo → bottom → normal → top → overlay */
	for (int wm = 0; wm < PADRAO_LAYER_COUNT; wm++) {
		struct padrao_layer *layer;
		wl_list_for_each(layer, &server->layer_surfaces, link) {
			if (layer->output != output) continue;
			if (layer->layer != wm) continue;
			if (!layer->scene_layer) continue;
			wlr_scene_layer_surface_v1_configure(layer->scene_layer,
				&full, &usable);
			layer->have_size = true;
		}
	}
	output->layers_dirty = false;
}
