/*
 * ipc.c — cliente do socket Unix do compositor ($XDG_RUNTIME_DIR/padrao.sock).
 *
 * Protocolo: JSON por linha (ver compositor/src/ipc.c).
 *   compositor -> host: state, view, win, win_gone, output
 *   host -> compositor: set_view, win_move, win_focus, win_close, quit
 *
 * Reconecta sozinho (1 s) caso o compositor reinicie; cada reconexão recebe
 * o estado completo (send_state do compositor) de graça.
 */
#include <errno.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

#include "padrao_host.h"

static int ipc_fd = -1;
static GIOChannel *ipc_chan;
static guint ipc_watch;
static guint ipc_connect_src;
static guint ipc_flush_src;
static GString *ipc_out;
static GString *ipc_in;

static const char *ipc_socket_path(void) {
	static char path[512];
	if (!path[0]) {
		const char *rt = getenv("XDG_RUNTIME_DIR");
		snprintf(path, sizeof(path), "%s/padrao.sock",
			rt && *rt ? rt : "/tmp");
	}
	return path;
}

/* ------------------------------ saída -------------------------------- */

static void flush_now(void);

static gboolean flush_cb(gpointer data) {
	(void)data;
	ipc_flush_src = 0;
	flush_now();
	return G_SOURCE_REMOVE;
}

static void flush_now(void) {
	if (!ipc_chan || !ipc_out) return;
	while (ipc_out->len > 0) {
		GError *err = NULL;
		gsize n = 0;
		GIOStatus st = g_io_channel_write_chars(ipc_chan,
			ipc_out->str, ipc_out->len, &n, &err);
		if (err) { g_error_free(err); err = NULL; }
		if (st == G_IO_STATUS_NORMAL) {
			g_string_erase(ipc_out, 0, (gssize)n);
			g_io_channel_flush(ipc_chan, NULL);
		} else if (st == G_IO_STATUS_AGAIN) {
			/* socket cheio: tenta de novo em breve */
			if (!ipc_flush_src)
				ipc_flush_src = g_timeout_add(20, flush_cb, NULL);
			return;
		} else {
			return; /* erro: o watch de HUP cuida do resto */
		}
	}
}

static void try_flush(void) {
	if (ipc_flush_src) {
		g_source_remove(ipc_flush_src);
		ipc_flush_src = 0;
	}
	flush_now();
}

void host_send_compositor(const char *fmt, ...) {
	char buf[16384];
	va_list ap;
	va_start(ap, fmt);
	int n = vsnprintf(buf, sizeof(buf) - 2, fmt, ap);
	va_end(ap);
	if (n <= 0) return;
	if ((size_t)n >= sizeof(buf) - 2) n = (int)sizeof(buf) - 3;
	buf[n++] = '\n';
	buf[n] = '\0';

	if (ipc_fd < 0 || !ipc_out) return;
	g_string_append_len(ipc_out, buf, n);
	try_flush();
}

/* ---------------------------- entrada -------------------------------- */

static void disconnect(void) {
	if (ipc_watch) { g_source_remove(ipc_watch); ipc_watch = 0; }
	if (ipc_chan) { g_io_channel_unref(ipc_chan); ipc_chan = NULL; }
	if (ipc_fd >= 0) { close(ipc_fd); ipc_fd = -1; }
	if (ipc_out) g_string_truncate(ipc_out, 0);
	if (ipc_in) g_string_truncate(ipc_in, 0);
}

static gboolean schedule_reconnect(gpointer data) {
	(void)data;
	ipc_connect_src = 0;
	if (ipc_fd < 0) ipc_start();
	return G_SOURCE_REMOVE;
}

static gboolean on_ipc(GIOChannel *chan, GIOCondition cond, gpointer data) {
	(void)data;
	if (cond & (G_IO_HUP | G_IO_ERR)) {
		ipc_watch = 0;
		disconnect();
		if (!ipc_connect_src)
			ipc_connect_src = g_timeout_add(1000,
				schedule_reconnect, NULL);
		return G_SOURCE_REMOVE;
	}
	if (!(cond & G_IO_IN)) return G_SOURCE_CONTINUE;

	gchar buf[8192];
	gsize n = 0;
	GError *err = NULL;
	GIOStatus st = g_io_channel_read_chars(chan, buf, sizeof(buf), &n, &err);
	if (err) { g_error_free(err); err = NULL; }

	if (st == G_IO_STATUS_NORMAL && n > 0) {
		g_string_append_len(ipc_in, buf, (gssize)n);
		/* processa linhas completas */
		for (;;) {
			char *nl = strchr(ipc_in->str, '\n');
			if (!nl) break;
			*nl = '\0';
			if (ipc_in->str[0]) host_on_compositor_line(ipc_in->str);
			g_string_erase(ipc_in, 0, (gssize)(nl - ipc_in->str + 1));
		}
		if (ipc_in->len > 1 << 20) g_string_truncate(ipc_in, 0);
	} else if (st == G_IO_STATUS_EOF ||
			(st == G_IO_STATUS_ERROR && n == 0)) {
		ipc_watch = 0;
		disconnect();
		if (!ipc_connect_src)
			ipc_connect_src = g_timeout_add(1000,
				schedule_reconnect, NULL);
		return G_SOURCE_REMOVE;
	}
	return G_SOURCE_CONTINUE;
}

/* ----------------------------- conexão ------------------------------- */

static gboolean try_connect(gpointer data) {
	(void)data;
	ipc_connect_src = 0;
	if (ipc_fd >= 0) return G_SOURCE_REMOVE;

	const char *path = ipc_socket_path();
	int fd = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
	if (fd < 0) return G_SOURCE_REMOVE;

	struct sockaddr_un addr = { .sun_family = AF_UNIX };
	if (strlen(path) >= sizeof(addr.sun_path)) { close(fd); return G_SOURCE_REMOVE; }
	strcpy(addr.sun_path, path);

	if (connect(fd, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
		close(fd);
		ipc_connect_src = g_timeout_add(1000, (GSourceFunc)try_connect, NULL);
		return G_SOURCE_REMOVE;
	}

	ipc_fd = fd;
	ipc_chan = g_io_channel_unix_new(fd);
	g_io_channel_set_encoding(ipc_chan, NULL, NULL);
	g_io_channel_set_close_on_unref(ipc_chan, FALSE);
	g_io_channel_set_flags(ipc_chan, G_IO_FLAG_NONBLOCK, NULL);

	if (ipc_watch) g_source_remove(ipc_watch);
	ipc_watch = g_io_add_watch(ipc_chan,
		G_IO_IN | G_IO_HUP | G_IO_ERR, (GIOFunc)on_ipc, NULL);

	host_send_compositor("{\"t\":\"hello\"}");
	return G_SOURCE_REMOVE;
}

void ipc_start(void) {
	if (!ipc_out) ipc_out = g_string_new(NULL);
	if (!ipc_in) ipc_in = g_string_new(NULL);
	if (ipc_fd >= 0) return;
	if (ipc_connect_src) return;
	ipc_connect_src = g_timeout_add(200, (GSourceFunc)try_connect, NULL);
}
