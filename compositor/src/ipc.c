/*
 * ipc.c — protocolo JSON por socket Unix ($XDG_RUNTIME_DIR/padrao.sock).
 *
 * Único cliente: o shell-host (GTK+WebKit), que espelha os eventos para as
 * páginas JS e aggrega os comandos vindas da UI (câmera, foco, fechar…).
 *
 * compositor → host:  state, view, win, win_gone, output
 * host → compositor:  set_view, win_move, win_focus, win_close, quit
 */
#include <errno.h>
#include <fcntl.h>
#include <math.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

#include <wlr/util/log.h>

#include "padrao.h"

static const char *ipc_socket_path(void) {
	static char path[512];
	if (!path[0]) {
		const char *rt = getenv("XDG_RUNTIME_DIR");
		snprintf(path, sizeof(path), "%s/%s.sock",
			rt && *rt ? rt : "/tmp", PADRAO_SOCK_NAME);
	}
	return path;
}

/* ------------------------- saída (buffered) ------------------------- */

static void conn_update_interest(struct padrao_server *server) {
	if (!server->ipc_conn_src) return;
	int mask = WL_EVENT_READABLE;
	if (server->ipc_out_len > server->ipc_out_sent) mask |= WL_EVENT_WRITABLE;
	wl_event_source_fd_update(server->ipc_conn_src, mask);
}

static void out_append(struct padrao_server *server, const char *data, size_t len) {
	if (server->ipc_fd < 0 || len == 0) return;
	size_t pending = server->ipc_out_len - server->ipc_out_sent;
	char *n = realloc(server->ipc_out, pending + len + 1);
	if (!n) return;
	if (server->ipc_out_sent > 0 && pending > 0) {
		memmove(n, n + server->ipc_out_sent, pending);
	}
	server->ipc_out = n;
	memcpy(n + pending, data, len);
	server->ipc_out_len = pending + len;
	server->ipc_out_sent = 0;
	server->ipc_out[server->ipc_out_len] = '\0';
	conn_update_interest(server);
}

static void conn_try_write(struct padrao_server *server) {
	while (server->ipc_out_sent < server->ipc_out_len) {
		ssize_t n = write(server->ipc_fd,
			server->ipc_out + server->ipc_out_sent,
			server->ipc_out_len - server->ipc_out_sent);
		if (n > 0) {
			server->ipc_out_sent += (size_t)n;
		} else if (n < 0 && (errno == EAGAIN || errno == EWOULDBLOCK)) {
			break;
		} else {
			if (n < 0) wlr_log(WLR_ERROR, "ipc write: %s", strerror(errno));
			return;
		}
	}
	/* esvaziou */
	server->ipc_out_len = server->ipc_out_sent = 0;
	conn_update_interest(server);
}

void padrao_ipc_broadcast(struct padrao_server *server, const char *json) {
	if (server->ipc_fd < 0) return;
	size_t len = strlen(json);
	out_append(server, json, len);
	out_append(server, "\n", 1);
	conn_try_write(server);
}

void padrao_ipc_broadcastf(struct padrao_server *server, const char *fmt, ...) {
	char buf[8192];
	va_list ap;
	va_start(ap, fmt);
	int n = vsnprintf(buf, sizeof(buf), fmt, ap);
	va_end(ap);
	if (n <= 0) return;
	if ((size_t)n >= sizeof(buf)) n = (int)sizeof(buf) - 1;
	padrao_ipc_broadcast(server, buf);
}

/* ---------------------------- estado -------------------------------- */

static void send_state(struct padrao_server *server) {
	char buf[65536];
	size_t n = (size_t)snprintf(buf, sizeof(buf),
		"{\"t\":\"state\",\"view\":{\"x\":%.0f,\"y\":%.0f,\"z\":%g}",
		server->view_x, server->view_y, server->zoom);

	struct wlr_box box = { 0 };
	if (server->layout) wlr_output_layout_get_box(server->layout, NULL, &box);
	n += (size_t)snprintf(buf + n, sizeof(buf) - n,
		",\"out\":{\"w\":%d,\"h\":%d,\"scale\":%g}", box.width, box.height, server->zoom);

	n += (size_t)snprintf(buf + n, sizeof(buf) - n, ",\"wins\":[");
	bool first = true;
	struct padrao_view *view;
	wl_list_for_each(view, &server->views, link) {
		if (!view->mapped) continue;
		struct wlr_box geo = view->toplevel->base->geometry;
		char title[1024], app[512];
		padrao_json_escape(title, sizeof(title),
			view->toplevel->title ? view->toplevel->title : "");
		padrao_json_escape(app, sizeof(app),
			view->toplevel->app_id ? view->toplevel->app_id : "");
		n += (size_t)snprintf(buf + n, sizeof(buf) - n,
			"%s{\"id\":%ld,\"app\":\"%s\",\"title\":\"%s\","
			"\"x\":%.0f,\"y\":%.0f,\"w\":%d,\"h\":%d,\"focused\":%s}",
			first ? "" : ",", (long)(uintptr_t)view, app, title,
			view->wx, view->wy, geo.width, geo.height,
			server->focused == view ? "true" : "false");
		first = false;
		if (n > sizeof(buf) - 4096) break; /* trava de segurança */
	}
	n += (size_t)snprintf(buf + n, sizeof(buf) - n, "]}");

	out_append(server, buf, n);
	out_append(server, "\n", 1);
	conn_try_write(server);
}

/* --------------------------- recepção ------------------------------- */

static void handle_line(struct padrao_server *server, const char *line) {
	char t[48] = { 0 };
	if (!padrao_json_get_str(line, "t", t, sizeof(t))) return;

	if (strcmp(t, "hello") == 0 || strcmp(t, "state_req") == 0) {
		send_state(server);
		return;
	}
	if (strcmp(t, "set_view") == 0) {
		double x, y, z;
		bool has_x = padrao_json_get_num(line, "x", &x);
		bool has_y = padrao_json_get_num(line, "y", &y);
		bool has_z = padrao_json_get_num(line, "z", &z);
		if (has_x) server->view_x = nearbyint(x);
		if (has_y) server->view_y = nearbyint(y);
		if (has_z) padrao_apply_zoom(server, z);
		padrao_ipc_broadcastf(server,
			"{\"t\":\"view\",\"x\":%.0f,\"y\":%.0f,\"z\":%g}",
			server->view_x, server->view_y, server->zoom);
		padrao_apply_camera_all(server);
		return;
	}
	if (strcmp(t, "win_move") == 0) {
		long id, x, y;
		if (!padrao_json_get_int(line, "id", &id)) return;
		if (!padrao_json_get_int(line, "x", &x)) return;
		if (!padrao_json_get_int(line, "y", &y)) return;
		struct padrao_view *view;
		wl_list_for_each(view, &server->views, link) {
			if ((long)(uintptr_t)view == id) {
				view->wx = x;
				view->wy = y;
				padrao_apply_camera_view(view);
				padrao_broadcast_win(server, view);
				break;
			}
		}
		return;
	}
	if (strcmp(t, "win_focus") == 0) {
		long id;
		if (!padrao_json_get_int(line, "id", &id)) return;
		struct padrao_view *view;
		wl_list_for_each(view, &server->views, link) {
			if ((long)(uintptr_t)view == id) {
				padrao_focus_view(view);
				break;
			}
		}
		return;
	}
	if (strcmp(t, "win_close") == 0) {
		long id;
		if (!padrao_json_get_int(line, "id", &id)) return;
		struct padrao_view *view;
		wl_list_for_each(view, &server->views, link) {
			if ((long)(uintptr_t)view == id) {
				padrao_close_view(view);
				break;
			}
		}
		return;
	}
	if (strcmp(t, "quit") == 0) {
		padrao_request_quit(server);
		return;
	}
	wlr_log(WLR_DEBUG, "ipc: mensagem desconhecida '%s'", t);
}

static void conn_teardown(struct padrao_server *server) {
	if (server->ipc_conn_src) {
		wl_event_source_remove(server->ipc_conn_src);
		server->ipc_conn_src = NULL;
	}
	if (server->ipc_fd >= 0) {
		close(server->ipc_fd);
		server->ipc_fd = -1;
	}
	server->ipc_in_len = 0;
	server->ipc_out_len = server->ipc_out_sent = 0;
	wlr_log(WLR_INFO, "shell-host desconectado");
}

static int conn_readable(int fd, uint32_t mask, void *data) {
	struct padrao_server *server = data;
	if (mask & (WL_EVENT_HANGUP | WL_EVENT_ERROR)) {
		conn_teardown(server);
		return 0;
	}
	if (!(mask & WL_EVENT_READABLE)) return 0;

	ssize_t n = read(fd, server->ipc_in + server->ipc_in_len,
		sizeof(server->ipc_in) - 1 - server->ipc_in_len);
	if (n <= 0) {
		if (n < 0 && (errno == EAGAIN || errno == EINTR)) return 0;
		conn_teardown(server);
		return 0;
	}
	server->ipc_in_len += (size_t)n;
	server->ipc_in[server->ipc_in_len] = '\0';

	char *start = server->ipc_in;
	char *nl;
	while ((nl = strchr(start, '\n')) != NULL) {
		*nl = '\0';
		if (*start) handle_line(server, start);
		start = nl + 1;
	}
	/* move o resto para o início */
	size_t rest = server->ipc_in_len - (size_t)(start - server->ipc_in);
	memmove(server->ipc_in, start, rest);
	server->ipc_in_len = rest;
	if (server->ipc_in_len >= sizeof(server->ipc_in) - 1) {
		wlr_log(WLR_ERROR, "ipc: linha muito longa — resetando");
		server->ipc_in_len = 0;
	}
	return 0;
}

static int listen_readable(int fd, uint32_t mask, void *data) {
	struct padrao_server *server = data;
	if (mask & (WL_EVENT_HANGUP | WL_EVENT_ERROR)) return 0;

	int cfd = accept(fd, NULL, NULL);
	if (cfd < 0) return 0;

	if (server->ipc_fd >= 0) {
		wlr_log(WLR_INFO, "ipc: nova conexão substitui a anterior");
		conn_teardown(server);
	}
	int flags = fcntl(cfd, F_GETFL, 0);
	fcntl(cfd, F_SETFL, flags | O_NONBLOCK);
	server->ipc_fd = cfd;
	server->ipc_in_len = 0;
	server->ipc_out_len = server->ipc_out_sent = 0;
	server->ipc_conn_src = wl_event_loop_add_fd(server->loop, cfd,
		WL_EVENT_READABLE, conn_readable, server);
	wlr_log(WLR_INFO, "shell-host conectado ao IPC");
	send_state(server);
	return 0;
}

void padrao_ipc_init(struct padrao_server *server) {
	const char *path = ipc_socket_path();
	unlink(path); /* socket óbvio de execuções anteriores */

	int fd = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
	if (fd < 0) {
		wlr_log(WLR_ERROR, "ipc: socket: %s", strerror(errno));
		return;
	}
	struct sockaddr_un addr = { .sun_family = AF_UNIX };
	if (strlen(path) >= sizeof(addr.sun_path)) {
		wlr_log(WLR_ERROR, "ipc: caminho longo demais: %s", path);
		close(fd);
		return;
	}
	strcpy(addr.sun_path, path);
	if (bind(fd, (struct sockaddr *)&addr, sizeof(addr)) < 0
			|| listen(fd, 4) < 0) {
		wlr_log(WLR_ERROR, "ipc: bind/listen %s: %s", path, strerror(errno));
		close(fd);
		return;
	}
	int flags = fcntl(fd, F_GETFL, 0);
	fcntl(fd, F_SETFL, flags | O_NONBLOCK);

	server->ipc_listen_fd = fd;
	server->ipc_listen_src = wl_event_loop_add_fd(server->loop, fd,
		WL_EVENT_READABLE, listen_readable, server);
	wlr_log(WLR_INFO, "ipc escutando em %s", path);
}
