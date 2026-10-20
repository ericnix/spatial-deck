/*
 * main.c — inicialização do compositor Padrão.
 *
 * Cria backend/renderer, protocolos Wayland, scene graph, câmera, IPC e
 * inicia o loop de eventos. O shell-host (GTK+WebKit) é iniciado externamente
 * pelo run.sh com WAYLAND_DISPLAY=padrao.
 */
#define _GNU_SOURCE

#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <limits.h>

#include <math.h>
#include <pthread.h>
#include <gtk/gtk.h>
#include <webkit2/webkit2.h>
#include <gtk-layer-shell.h>
#include <wlr/types/wlr_alpha_modifier_v1.h>
#include <wlr/types/wlr_data_device.h>
#include <wlr/types/wlr_fractional_scale_v1.h>
#include <wlr/types/wlr_linux_dmabuf_v1.h>
#include <wlr/types/wlr_screencopy_v1.h>
#include <wlr/types/wlr_single_pixel_buffer_v1.h>
#include <wlr/types/wlr_subcompositor.h>
#include <wlr/types/wlr_viewporter.h>
#include <wlr/types/wlr_xdg_output_v1.h>
#include <wlr/util/log.h>

#include "padrao.h"

void padrao_schedule_frame(struct padrao_server *server) {
	struct padrao_output *output;
	wl_list_for_each(output, &server->outputs, link) {
		wlr_output_schedule_frame(output->wlr_output);
	}
}

/* Pedido de encerramento. Não usamos wl_display_terminate() porque ele
 * impede o event loop de continuar despachando eventos — e a camada web
 * (cliente GTK deste próprio compositor) precisa desse loop vivo para
 * se desmontar com elegância. */
void padrao_request_quit(struct padrao_server *server) {
	server->finished = true;
}

static int handle_signal(int sig, void *data) {
	struct padrao_server *server = data;
	(void)sig;
	wlr_log(WLR_INFO, "sinal recebido — encerrando o Padrão");
	padrao_request_quit(server);
	return 0;
}

/* O wlroots destrói o output layout no display_destroy do display, e isso
 * acontece ANTES de o backend finalizar as saídas. Sem zerar o ponteiro,
 * o output_destroy() consultaria um layout já liberado — SIGSEGV no
 * shutdown (wlr_output_layout_get_box num layout freed).
 * Este listener é registrado depois do do wlroots (que se inscreve ao criar
 * o layout), então roda depois dele: o layout já foi liberado e nós só
 * marcamos o ponteiro como inválido. */
static void handle_display_destroy(struct wl_listener *listener, void *data) {
	(void)data;
	struct padrao_server *server =
		wl_container_of(listener, server, display_destroy);
	server->layout = NULL;
}

/* Nome fixo de socket Wayland: $XDG_RUNTIME_DIR/padrao.
   Se restou de uma execução anterior, remove e tenta de novo.
   wl_display_add_socket() devolve 0 em sucesso. */
static int add_wayland_socket(struct wl_display *display) {
	if (wl_display_add_socket(display, PADRAO_SOCK_NAME) == 0)
		return 0;

	const char *rt = getenv("XDG_RUNTIME_DIR");
	if (rt) {
		char path[512];
		snprintf(path, sizeof(path), "%s/%s", rt, PADRAO_SOCK_NAME);
		unlink(path);
		if (wl_display_add_socket(display, PADRAO_SOCK_NAME) == 0)
			return 0;
	}
	return -1;
}

/* ------------------------------------------------------------------
 * Camada web — uma layer surface criada pelo próprio compositor.
 *
 * A janela GTK+WebKit conecta no socket do PRÓPRIO compositor
 * (WAYLAND_DISPLAY=padrao) e, via gtk-layer-shell, vira uma layer
 * surface do Padrão: a página local aparece como fundo da tela, sem
 * processo extra.
 *
 * Por que numa thread própria: o GDK abre a conexão e faz um roundtrip
 * para buscar os globals do servidor. Esse servidor é o próprio
 * compositor, que só responde dentro do seu event loop (thread
 * principal). Se a camada web rodasse na mesma thread, o roundtrip
 * ficaria esperando uma resposta que ninguém pode dar → deadlock.
 * Com a thread separada, a principal atende o roundtrip e a thread da
 * web monta a camada e bombeia o loop do GTK.
 * ------------------------------------------------------------------ */

/* Procura o arquivo: absoluto, ou relativo ao cwd, ao diretório do
 * binário ou ao diretório pai dele (build/ vs raiz do projeto). */
static char *try_resolve(const char *base, const char *file) {
	char buf[PATH_MAX];
	if (base && *base) {
		snprintf(buf, sizeof(buf), "%s/%s", base, file);
	} else {
		snprintf(buf, sizeof(buf), "%s", file);
	}
	return realpath(buf, NULL);
}

static char *resolve_web_file(const char *file) {
	char *p = try_resolve(NULL, file);
	if (p) return p;

	char exe[PATH_MAX];
	ssize_t n = readlink("/proc/self/exe", exe, sizeof(exe) - 1);
	if (n <= 0) return NULL;
	exe[n] = '\0';
	char *slash = strrchr(exe, '/');
	if (!slash) return NULL;
	*slash = '\0';
	p = try_resolve(exe, file);
	if (p) return p;
	slash = strrchr(exe, '/');
	if (!slash) return NULL;
	*slash = '\0';
	return try_resolve(exe, file);
}

/* Em qual camada do compositor a página web entra. O protocolo
 * layer-shell tem 4 camadas (background/bottom/top/overlay) e o
 * compositor tem 5 (NORMAL fica no meio, para as janelas xdg-toplevel).
 * Padrão: "bottom" — acima do canvas do shell (que é opaco e ficaria
 * por cima) e abaixo das janelas. */
static GtkLayerShellLayer web_layer_from_env(void) {
	const char *name = getenv("PADRAO_WEB_LAYER");
	if (!name || !*name) name = "bottom";
	if (strcmp(name, "background") == 0) return GTK_LAYER_SHELL_LAYER_BACKGROUND;
	if (strcmp(name, "top") == 0) return GTK_LAYER_SHELL_LAYER_TOP;
	if (strcmp(name, "overlay") == 0) return GTK_LAYER_SHELL_LAYER_OVERLAY;
	return GTK_LAYER_SHELL_LAYER_BOTTOM;
}

static void web_load_changed(WebKitWebView *view, WebKitLoadEvent event,
		void *data) {
	(void)view; (void)data;
	wlr_log(WLR_INFO, "web: carregamento=%d", (int)event);
}

static void web_view_size(GtkWidget *widget, GdkRectangle *alloc, void *data) {
	(void)data;
	static int last_w = -1, last_h = -1;
	if (alloc->width == last_w && alloc->height == last_h) return;
	last_w = alloc->width; last_h = alloc->height;
	wlr_log(WLR_INFO, "web: view %dx%d", alloc->width, alloc->height);
}

static void *web_layer_thread(void *data) {
	struct padrao_server *server = data;

	/* Contexto de eventos da thread: o GDK registra nele a fonte do
	 * Wayland. Guardamos também o contexto global — algumas partes do
	 * GDK/WebKit ainda usam o contexto padrão global, e é nele que
	 * chega o configure do compositor (sem ele a camada nunca recebe o
	 * tamanho e nunca manda buffer). */
	GMainContext *global = g_main_context_default();
	GMainContext *ctx = g_main_context_new();
	g_main_context_push_thread_default(ctx);

	/* O WebKit/GDK usa OpenGL (radeonsi) in-process por padrão; neste
	 * ambiente o driver GL do Mesa corrompe o heap — e o estrago só
	 * aparece no teardown, derrubando o compositor no shutdown. A camada
	 * web renderiza por software (Cairo) e o GDK fica sem GL:
	 * PADRAO_WEB_GPU=1 reativa a GPU. */
	if (!getenv("PADRAO_WEB_GPU")) {
		g_setenv("GDK_GL", "disable", TRUE);
		g_setenv("WEBKIT_DISABLE_COMPOSITING_MODE", "1", TRUE);
		g_setenv("WEBKIT_DISABLE_DMABUF_RENDERER", "1", TRUE);
	}

	if (!gtk_init_check(NULL, NULL)) {
		wlr_log(WLR_ERROR, "web: gtk_init falhou — camada web desligada");
		goto out;
	}

	const char *file = getenv("PADRAO_WEB_FILE");
	if (!file || !*file) file = "web/index.html";
	char *abs = resolve_web_file(file);
	if (!abs) {
		wlr_log(WLR_ERROR, "web: não achei %s — camada web desligada", file);
		goto out;
	}
	char *uri = g_filename_to_uri(abs, NULL, NULL);
	free(abs);
	if (!uri) goto out;

	GtkWidget *window = gtk_window_new(GTK_WINDOW_TOPLEVEL);
	gtk_layer_init_for_window(GTK_WINDOW(window));
	gtk_layer_set_layer(GTK_WINDOW(window), web_layer_from_env());
	gtk_layer_set_namespace(GTK_WINDOW(window), "padrao-web");
	gtk_layer_set_anchor(GTK_WINDOW(window), GTK_LAYER_SHELL_EDGE_TOP, TRUE);
	gtk_layer_set_anchor(GTK_WINDOW(window), GTK_LAYER_SHELL_EDGE_BOTTOM, TRUE);
	gtk_layer_set_anchor(GTK_WINDOW(window), GTK_LAYER_SHELL_EDGE_LEFT, TRUE);
	gtk_layer_set_anchor(GTK_WINDOW(window), GTK_LAYER_SHELL_EDGE_RIGHT, TRUE);
	gtk_layer_set_exclusive_zone(GTK_WINDOW(window), -1);

	GtkWidget *view = webkit_web_view_new();
	g_signal_connect(view, "load-changed", G_CALLBACK(web_load_changed), NULL);
	g_signal_connect(view, "size-allocate", G_CALLBACK(web_view_size), NULL);
	webkit_web_view_load_uri(WEBKIT_WEB_VIEW(view), uri);
	g_free(uri);
	gtk_container_add(GTK_CONTAINER(window), view);
	gtk_widget_show_all(window);

	wlr_log(WLR_INFO, "web: camada WebKit com %s", file);

	/* loop do GTK: processa o Wayland do cliente e o WebKit */
	while (server->running) {
		bool worked = false;
		if (g_main_context_pending(ctx)) {
			g_main_context_iteration(ctx, FALSE);
			worked = true;
		}
		if (global != ctx && g_main_context_pending(global)) {
			g_main_context_iteration(global, FALSE);
			worked = true;
		}
		if (!worked) g_usleep(5000);
	}

	/* Não destruímos a janela aqui: o GDK pode ficar esperando uma
	 * resposta do compositor, que a esta altura já está encerrando.
	 * O processo está saindo — o servidor Wayland derruba a layer
	 * surface junto. */
	while (g_main_context_pending(ctx)) {
		g_main_context_iteration(ctx, FALSE);
	}

out:
	g_main_context_pop_thread_default(ctx);
	g_main_context_unref(ctx);
	return NULL;
}

int main(int argc, char *argv[]) {
	(void)argc; (void)argv;
	wlr_log_init(WLR_INFO, NULL);

	struct padrao_server server = {
		.view_x = 0, .view_y = 0, .zoom = 1.0,
		.grab = PADRAO_GRAB_NONE,
		.ipc_listen_fd = -1, .ipc_fd = -1,
	};
	wl_list_init(&server.views);
	wl_list_init(&server.outputs);
	wl_list_init(&server.layer_surfaces);

	server.display = wl_display_create();
	if (!server.display) {
		fprintf(stderr, "padrao: falha ao criar wl_display\n");
		return 1;
	}
	server.loop = wl_display_get_event_loop(server.display);

	server.backend = wlr_backend_autocreate(server.loop, &server.session);
	if (!server.backend) {
		fprintf(stderr, "padrao: backend indisponível "
			"(rode em TTY, ou aninhado com WAYLAND_DISPLAY/DISPLAY)\n");
		return 1;
	}
	server.renderer = wlr_renderer_autocreate(server.backend);
	if (!server.renderer) {
		fprintf(stderr, "padrao: renderer indisponível\n");
		return 1;
	}
	wlr_renderer_init_wl_display(server.renderer, server.display);
	server.allocator = wlr_allocator_autocreate(server.backend, server.renderer);

	/* ---------------- protocolos ---------------- */
	wlr_compositor_create(server.display, 6, server.renderer);
	wlr_subcompositor_create(server.display);
	wlr_data_device_manager_create(server.display);
	wlr_viewporter_create(server.display);
	wlr_fractional_scale_manager_v1_create(server.display, 1);
	wlr_alpha_modifier_v1_create(server.display);
	wlr_single_pixel_buffer_manager_v1_create(server.display);
	wlr_screencopy_manager_v1_create(server.display); /* permite grim/padrao-shot */
	struct wlr_linux_dmabuf_v1 *dmabuf =
		wlr_linux_dmabuf_v1_create_with_renderer(server.display, 4, server.renderer);
	server.presentation = wlr_presentation_create(server.display,
		server.backend, 1);

	/* ---------------- scene graph ---------------- */
	server.layout = wlr_output_layout_create(server.display);
	server.display_destroy.notify = handle_display_destroy;
	wl_display_add_destroy_listener(server.display, &server.display_destroy);
	server.scene = wlr_scene_create();
	server.scene_layout = wlr_scene_attach_output_layout(server.scene,
		server.layout);
	/* zxdg_output_manager_v1: clientes (grim, GTK) enxergam o layout lógico */
	wlr_xdg_output_manager_v1_create(server.display, server.layout);
	if (dmabuf) wlr_scene_set_linux_dmabuf_v1(server.scene, dmabuf);

	for (int i = 0; i < PADRAO_LAYER_COUNT; i++) {
		server.layers[i] = wlr_scene_tree_create(&server.scene->tree);
	}
	/* janelas ficam dentro da camada normal, acima das layer surfaces de
	   fundo e abaixo das de topo (deck) */
	server.views_tree = wlr_scene_tree_create(server.layers[PADRAO_LAYER_NORMAL]);

	/* ---------------- shells ---------------- */
	server.xdg_shell = wlr_xdg_shell_create(server.display, 6);
	server.new_toplevel.notify = padrao_handle_new_toplevel;
	wl_signal_add(&server.xdg_shell->events.new_toplevel,
		&server.new_toplevel);

	server.layer_shell = wlr_layer_shell_v1_create(server.display, 5) /* v4+: keyboard on_demand */;
	server.new_layer.notify = padrao_handle_new_layer;
	wl_signal_add(&server.layer_shell->events.new_surface, &server.new_layer);

	/* ---------------- seat e cursor ---------------- */
	server.seat = wlr_seat_create(server.display, "seat0");
	/* request_set_cursor/selection são ligados em padrao_input_init() */

	server.cursor = wlr_cursor_create();
	wlr_cursor_attach_output_layout(server.cursor, server.layout);
	server.xcursor_mgr = wlr_xcursor_manager_create(NULL, 24);
	wlr_xcursor_manager_load(server.xcursor_mgr, 24);

	/* saídas novas vindas do backend */
	server.new_output.notify = padrao_handle_new_output;
	wl_signal_add(&server.backend->events.new_output, &server.new_output);

	padrao_input_init(&server);

	/* ---------------- IPC (shell-host) ---------------- */
	padrao_ipc_init(&server);

	/* ---------------- sinais ---------------- */
	wl_event_loop_add_signal(server.loop, SIGINT, handle_signal, &server);
	wl_event_loop_add_signal(server.loop, SIGTERM, handle_signal, &server);
	signal(SIGPIPE, SIG_IGN);

	/* ---------------- sobe tudo ---------------- */
	if (!wlr_backend_start(server.backend)) {
		fprintf(stderr, "padrao: falha ao iniciar backend\n");
		return 1;
	}
	const char *sock = PADRAO_SOCK_NAME;
	if (add_wayland_socket(server.display) != 0) {
		fprintf(stderr, "padrao: falha ao criar socket Wayland\n");
		return 1;
	}
	setenv("WAYLAND_DISPLAY", sock, 1);
	wlr_log(WLR_INFO, "Padrão pronto — WAYLAND_DISPLAY=%s, IPC=%s/%s.sock",
		sock, getenv("XDG_RUNTIME_DIR") ? getenv("XDG_RUNTIME_DIR") : "/tmp",
		PADRAO_SOCK_NAME);

	/* ---------------- camada web (WebKitGTK) ----------------
	 * Sobe depois do setenv: o GDK conecta no nosso próprio socket
	 * (WAYLAND_DISPLAY=padrao) e vira uma layer surface do compositor. */
	server.running = true;
	if (pthread_create(&server.web_thread, NULL, web_layer_thread, &server) == 0) {
		server.web_thread_started = true;
	}

	/* Loop manual (em vez de wl_display_run): precisamos continuar
	 * servindo o Wayland depois do pedido de encerramento, para que a
	 * camada web (cliente GTK deste proprio compositor) se desmonte sem
	 * ficar esperando resposta. */
	while (!server.finished) {
		wl_display_flush_clients(server.display);
		wl_event_loop_dispatch(server.loop, 50);
	}

	/* ---------------- limpeza ----------------
	   wlroots 0.20 exige listas de listeners vazias nos shells no
	   display_destroy: destacamos tudo que registramos. */
	server.running = false;
	if (server.web_thread_started) {
		/* Espera a thread do GTK terminar com o loop Wayland ainda
		 * rodando: sem isso o teardown do GL corre em paralelo com a
		 * limpeza do display e o heap acaba corrompido (o driver do Mesa
		 * estraga a memoria do processo nesse caminho). */
		struct timespec deadline;
		clock_gettime(CLOCK_REALTIME, &deadline);
		deadline.tv_sec += 5;
		while (server.web_thread_started) {
			wl_display_flush_clients(server.display);
			wl_event_loop_dispatch(server.loop, 50);
			struct timespec now;
			clock_gettime(CLOCK_REALTIME, &now);
			if (now.tv_sec > deadline.tv_sec) {
				pthread_detach(server.web_thread);
				server.web_thread_started = false;
				break;
			}
			if (pthread_tryjoin_np(server.web_thread, NULL) == 0) {
				server.web_thread_started = false;
			}
		}
	}
	wl_display_destroy_clients(server.display);
	wl_list_remove(&server.new_toplevel.link);
	wl_list_remove(&server.new_layer.link);
	wl_list_remove(&server.new_input.link);
	wl_list_remove(&server.cursor_motion.link);
	wl_list_remove(&server.cursor_motion_abs.link);
	wl_list_remove(&server.cursor_button.link);
	wl_list_remove(&server.cursor_axis.link);
	wl_list_remove(&server.cursor_frame.link);
	wl_list_remove(&server.request_set_cursor.link);
	wl_list_remove(&server.request_set_selection.link);
	wl_list_remove(&server.new_output.link);
	if (server.ipc_out) free(server.ipc_out);
	wlr_cursor_destroy(server.cursor);
	wlr_xcursor_manager_destroy(server.xcursor_mgr);
	wl_display_destroy(server.display);
	wlr_log(WLR_INFO, "Padrão encerrado");
	return 0;
}
