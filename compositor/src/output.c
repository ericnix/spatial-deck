/*
 * output.c — saídas: criação, frames, escala (zoom) e arrange de layers.
 *
 * O zoom é aplicado como escala de saída (wlr_output_state_set_scale).
 * Com isso o viewport lógico cresce/reduz e todo o conteúdo escala junto —
 * janelas re-renderizadas em resolução nativa (nítido), canvas e deck
 * acompanhando, pois todos vivem em unidades lógicas.
 */
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>

#include <wlr/util/log.h>

#include "padrao.h"

static void broadcast_output_state(struct padrao_server *server) {
	struct wlr_box box = { 0 };
	if (server->layout) wlr_output_layout_get_box(server->layout, NULL, &box);
	padrao_ipc_broadcastf(server,
		"{\"t\":\"output\",\"w\":%d,\"h\":%d,\"scale\":%g}",
		box.width, box.height, server->zoom);
}

void padrao_broadcast_output(struct padrao_server *server) {
	broadcast_output_state(server);
}

/* ------------------------------------------------------------------ */
static void output_frame(struct wl_listener *listener, void *data) {
	(void)data;
	struct padrao_output *output = wl_container_of(listener, output, frame);
	if (!output->scene_output) return;

	if (output->layers_dirty) padrao_arrange_layers(output);
	/* comite sempre: o screencopy agenda frames próprios e pode pedir
	   um commit que o teste de needs_frame sozinho não antecipa */
	wlr_scene_output_commit(output->scene_output, NULL);
	/* O wlroots 0.20 só limpa needs_frame quando habilita/desabilita a
	 * saída. Com ele marcado, wlr_output_update_needs_frame() não emite
	 * mais o sinal needs_frame e o scene output para de agendar frames —
	 * aí o screencopy (grim/padrao-shot) fica esperando um commit que
	 * nunca chega e a captura trava. Limpando depois do commit, cada
	 * pedido de frame volta a ser agendado. */
	output->wlr_output->needs_frame = false;
	struct timespec now;
	clock_gettime(CLOCK_MONOTONIC, &now);
	wlr_scene_output_send_frame_done(output->scene_output, &now);
}

static void output_request_state(struct wl_listener *listener, void *data) {
	struct padrao_output *output =
		wl_container_of(listener, output, request_state);
	const struct wlr_output_event_request_state *ev = data;
	wlr_output_commit_state(output->wlr_output, ev->state);
}

static void output_destroy(struct wl_listener *listener, void *data) {
	(void)data;
	struct padrao_output *output = wl_container_of(listener, output, destroy);
	struct padrao_server *server = output->server;
	wlr_log(WLR_INFO, "saída removida: %s", output->wlr_output->name);

	/* A saída precisa sair do layout: no wlroots 0.20 o layout NÃO se
	 * inscreve em output->events.destroy, então o wlr_output_layout_output
	 * ficaria apontando para um wlr_output destruído (use-after-free em
	 * qualquer wlr_output_layout_get_box() posterior). É o que o
	 * examples/output-layout.c do próprio wlroots faz. */
	if (server->layout) {
		wlr_output_layout_remove(server->layout, output->wlr_output);
	}

	wl_list_remove(&output->frame.link);
	wl_list_remove(&output->request_state.link);
	wl_list_remove(&output->destroy.link);
	wl_list_remove(&output->link);

	/* layers que apontavam para esta saída voltam a ficar sem dono */
	struct padrao_layer *layer;
	wl_list_for_each(layer, &output->server->layer_surfaces, link) {
		if (layer->output == output) layer->output = NULL;
	}
	free(output);
	/* scene_output é destruído automaticamente junto com o wlr_output */
	broadcast_output_state(server);
}

void padrao_handle_new_output(struct wl_listener *listener, void *data) {
	struct padrao_server *server = wl_container_of(listener, server, new_output);
	struct wlr_output *wlr_output = data;

	wlr_output_init_render(wlr_output, server->allocator, server->renderer);

	struct wlr_output_mode *mode = wlr_output_preferred_mode(wlr_output);
	struct wlr_output_state state;
	wlr_output_state_init(&state);
	wlr_output_state_set_enabled(&state, true);
	if (mode) wlr_output_state_set_mode(&state, mode);
	wlr_output_state_set_scale(&state, (float)server->zoom);
	if (!wlr_output_commit_state(wlr_output, &state)) {
		wlr_log(WLR_ERROR, "falha ao configurar saída %s", wlr_output->name);
		wlr_output_state_finish(&state);
		return;
	}
	wlr_output_state_finish(&state);

	struct padrao_output *output = calloc(1, sizeof(*output));
	if (!output) return;
	output->server = server;
	output->wlr_output = wlr_output;
	output->layers_dirty = true;

	output->scene_output = wlr_scene_output_create(server->scene, wlr_output);
	struct wlr_output_layout_output *l_output =
		wlr_output_layout_add_auto(server->layout, wlr_output);
	if (server->scene_layout && l_output && output->scene_output) {
		wlr_scene_output_layout_add_output(server->scene_layout,
			l_output, output->scene_output);
	}

	output->frame.notify = output_frame;
	wl_signal_add(&wlr_output->events.frame, &output->frame);
	output->request_state.notify = output_request_state;
	wl_signal_add(&wlr_output->events.request_state, &output->request_state);
	output->destroy.notify = output_destroy;
	wl_signal_add(&wlr_output->events.destroy, &output->destroy);

	wl_list_insert(&server->outputs, &output->link);

	/* layers criadas sem saída ainda ganham esta */
	struct padrao_layer *layer;
	wl_list_for_each(layer, &server->layer_surfaces, link) {
		if (!layer->output) layer->output = output;
	}
	padrao_arrange_layers(output);
	broadcast_output_state(server);
	wlr_log(WLR_INFO, "saída: %s (%dx%d, escala %.2f)",
		wlr_output->name, wlr_output->width, wlr_output->height,
		(double)wlr_output->scale);
}

/* ------------------------------------------------------------------ */
/* zoom = escala de saída */
void padrao_apply_zoom(struct padrao_server *server, double zoom) {
	if (zoom < 0.5) zoom = 0.5;
	if (zoom > 3.0) zoom = 3.0;
	zoom = nearbyint(zoom * 20.0) / 20.0; /* passos de 0.05 */
	if (fabs(zoom - server->zoom) < 1e-9) return;

	server->zoom = zoom;
	struct padrao_output *output;
	wl_list_for_each(output, &server->outputs, link) {
		struct wlr_output_state state;
		wlr_output_state_init(&state);
		wlr_output_state_set_enabled(&state, true);
		wlr_output_state_set_scale(&state, (float)zoom);
		wlr_output_commit_state(output->wlr_output, &state);
		wlr_output_state_finish(&state);

		/* o viewport lógico mudou: layers precisam de re-arrange */
		output->layers_dirty = true;
		padrao_arrange_layers(output);
	}
	/* posição de janelas no mundo NÃO muda (espaço aditivo) — só o
	   viewport. Transmite câmera e saída para a UI. */
	padrao_ipc_broadcastf(server,
		"{\"t\":\"view\",\"x\":%.0f,\"y\":%.0f,\"z\":%g}",
		server->view_x, server->view_y, server->zoom);
	broadcast_output_state(server);
	padrao_schedule_frame(server);
}
