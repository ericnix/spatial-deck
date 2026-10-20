#define _GNU_SOURCE /* strcasestr */
/*
 * http.c — servidor HTTP local (127.0.0.1) que serve a UI do shell.
 *
 * Rotas:
 *   /canvas/…  /deck/…  /shared/…   arquivos de g_shell_dir (web/)
 *   /m?path=/abs/arquivo.mp4        mídia do usuário (com Range p/ vídeo)
 *   /i/<nome>?s=64                  ícone do tema GTK em PNG
 *   /debug/emit?json=…&page=deck    gancho de depuração (injeta Padrao.emit)
 *   /                                redirect para /canvas/
 *
 * Um thread por conexão (poucas conexões, tudo bloqueante com timeout);
 * o cache/render de ícones é marshalado para o thread principal do GTK.
 */
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <unistd.h>

#include "padrao_host.h"

static int listen_fd = -1;

/* ----------------------------- utilidades ---------------------------- */

static void send_all(int fd, const void *buf, size_t len) {
	const char *p = buf;
	size_t off = 0;
	while (off < len) {
		ssize_t n = send(fd, p + off, len - off, MSG_NOSIGNAL);
		if (n < 0) {
			if (errno == EINTR) continue;
			return;
		}
		off += (size_t)n;
	}
}

static const char *mime_for(const char *path) {
	const char *dot = strrchr(path, '.');
	if (!dot) return "application/octet-stream";
	dot++;
	static const struct { const char *ext, *mime; } tab[] = {
		{ "html", "text/html; charset=utf-8" },
		{ "htm",  "text/html; charset=utf-8" },
		{ "css",  "text/css; charset=utf-8" },
		{ "js",   "text/javascript; charset=utf-8" },
		{ "mjs",  "text/javascript; charset=utf-8" },
		{ "json", "application/json" },
		{ "txt",  "text/plain; charset=utf-8" },
		{ "svg",  "image/svg+xml" },
		{ "png",  "image/png" },
		{ "jpg",  "image/jpeg" },
		{ "jpeg", "image/jpeg" },
		{ "gif",  "image/gif" },
		{ "webp", "image/webp" },
		{ "ico",  "image/x-icon" },
		{ "mp4",  "video/mp4" },
		{ "m4v",  "video/mp4" },
		{ "webm", "video/webm" },
		{ "ogv",  "video/ogg" },
		{ "mov",  "video/quicktime" },
		{ "mp3",  "audio/mpeg" },
		{ "ogg",  "audio/ogg" },
		{ "oga",  "audio/ogg" },
		{ "wav",  "audio/wav" },
		{ "flac", "audio/flac" },
		{ "woff2","font/woff2" },
		{ NULL, NULL }
	};
	for (int i = 0; tab[i].ext; i++)
		if (g_ascii_strcasecmp(dot, tab[i].ext) == 0) return tab[i].mime;
	return "application/octet-stream";
}

static void reply_buffer(int fd, int code, const char *status,
		const char *mime, const void *body, size_t len) {
	char head[1024];
	int hl = snprintf(head, sizeof(head),
		"HTTP/1.1 %d %s\r\n"
		"Content-Type: %s\r\n"
		"Content-Length: %zu\r\n"
		"Cache-Control: no-cache\r\n"
		"Connection: close\r\n\r\n",
		code, status, mime, len);
	send_all(fd, head, (size_t)hl);
	if (len && body) send_all(fd, body, len);
}

static void reply_404(int fd) {
	static const char body[] = "404\n";
	reply_buffer(fd, 404, "Not Found", "text/plain", body, sizeof(body) - 1);
}

/* --------------------------- arquivo/Range ---------------------------- */

/* Interpreta "bytes=a-b" | "bytes=a-" | "bytes=-n". */
static gboolean parse_range(const char *hdr, gint64 total,
		gint64 *out_start, gint64 *out_end) {
	if (!hdr) return FALSE;
	while (*hdr == ' ') hdr++;
	if (strncmp(hdr, "bytes=", 6) != 0) return FALSE;
	hdr += 6;
	while (*hdr == ' ') hdr++;
	char *endp = NULL;
	if (*hdr == '-') {
		gint64 suffix = strtoll(hdr + 1, &endp, 10);
		if (suffix <= 0) return FALSE;
		*out_start = total - suffix;
		if (*out_start < 0) *out_start = 0;
		*out_end = total - 1;
		return TRUE;
	}
	gint64 start = strtoll(hdr, &endp, 10);
	if (endp == hdr || *endp != '-') return FALSE;
	gint64 end = total - 1;
	if (*(endp + 1)) {
		end = strtoll(endp + 1, NULL, 10);
		if (end >= total) end = total - 1;
	}
	if (start < 0 || start > end) return FALSE;
	*out_start = start;
	*out_end = end;
	return TRUE;
}

static gboolean serve_file(int fd, const char *fs_path, const char *range_hdr) {
	struct stat st;
	if (stat(fs_path, &st) < 0 || !S_ISREG(st.st_mode)) return FALSE;

	int f = open(fs_path, O_RDONLY | O_CLOEXEC);
	if (f < 0) return FALSE;

	gint64 start = 0, end = st.st_size - 1;
	gboolean partial = parse_range(range_hdr, st.st_size, &start, &end);
	if (st.st_size == 0) { start = 0; end = -1; }
	gint64 count = (end >= start) ? end - start + 1 : 0;

	char head[1024], extra[256] = "";
	int code = 200;
	const char *status = "OK";
	if (partial) {
		code = 206;
		status = "Partial Content";
		snprintf(extra, sizeof(extra),
			"Content-Range: bytes %lld-%lld/%lld\r\n",
			(long long)start, (long long)end, (long long)st.st_size);
	}
	int hl = snprintf(head, sizeof(head),
		"HTTP/1.1 %d %s\r\n"
		"Content-Type: %s\r\n"
		"Content-Length: %lld\r\n"
		"Accept-Ranges: bytes\r\n"
		"%s"
		"Cache-Control: no-cache\r\n"
		"Connection: close\r\n\r\n",
		code, status, mime_for(fs_path), (long long)count, extra);
	send_all(fd, head, (size_t)hl);

	if (count > 0) {
		if (lseek(f, (off_t)start, SEEK_SET) < 0) { close(f); return TRUE; }
		char buf[65536];
		gint64 left = count;
		while (left > 0) {
			size_t want = left > (gint64)sizeof(buf)
				? sizeof(buf) : (size_t)left;
			ssize_t n = read(f, buf, want);
			if (n < 0) {
				if (errno == EINTR) continue;
				break;
			}
			if (n == 0) break;
			send_all(fd, buf, (size_t)n);
			left -= n;
		}
	}
	close(f);
	return TRUE;
}

/* --------------------------- decode de URL ---------------------------- */

static char *url_decode(const char *s) {
	return g_uri_unescape_string(s, NULL);
}

/* ------------------------------- ícones ------------------------------- */

static gboolean serve_icon(int fd, const char *name, int size) {
	gsize len = 0;
	char *png = host_icon_png(name, size, &len);
	if (!png) { reply_404(fd); return TRUE; }
	reply_buffer(fd, 200, "OK", "image/png", png, len);
	g_free(png);
	return TRUE;
}

/* ------------------------------ requisição ---------------------------- */

static gboolean header_value(const char *req, const char *key, char *out, size_t cap) {
	size_t klen = strlen(key);
	const char *p = req;
	while ((p = strcasestr(p, key)) != NULL) {
		if ((p == req || p[-1] == '\n') && p[klen] == ':') {
			p += klen + 1;
			while (*p == ' ' || *p == '\t') p++;
			size_t n = 0;
			while (*p && *p != '\r' && *p != '\n' && n + 1 < cap)
				out[n++] = *p++;
			out[n] = '\0';
			return TRUE;
		}
		p += klen;
	}
	return FALSE;
}

static void handle_request(int fd, const char *req) {
	/* primeira linha: METHOD TARGET HTTP/1.x */
	char method[8] = "", target[2048] = "";
	if (sscanf(req, "%7s %2047s", method, target) != 2
			|| strcmp(method, "GET") != 0) {
		static const char body[] = "só GET\r\n";
		reply_buffer(fd, 405, "Method Not Allowed", "text/plain",
			body, sizeof(body) - 1);
		return;
	}

	char range[256] = "";
	header_value(req, "Range", range, sizeof(range));

	char *q = strchr(target, '?');
	char *raw_path = q ? g_strndup(target, (size_t)(q - target))
		: g_strdup(target);
	char *path = url_decode(raw_path);
	char *query = q ? url_decode(q + 1) : NULL;
	g_free(raw_path);
	(void)q;

	/* / — página de índice do shell */
	if (strcmp(path, "/") == 0) {
		static const char body[] =
			"<!doctype html><meta charset=utf-8>"
			"<title>Padrão</title><body style=\"font:15px system-ui;"
			"background:#14161c;color:#dfe3ea;padding:32px\">"
			"<h2>Padrão</h2><p><a style=\"color:#7cc4ff\" href=/canvas/>"
			"canvas</a> · <a style=\"color:#7cc4ff\" href=/deck/>deck</a></p>";
		reply_buffer(fd, 200, "OK", "text/html; charset=utf-8",
			body, sizeof(body) - 1);
		goto out;
	}

	/* /i/<nome>?s=64 — ícone do tema */
	if (g_str_has_prefix(path, "/i/")) {
		int size = 48;
		if (query) {
			char *s = strstr(query, "s=");
			if (s) size = atoi(s + 2);
		}
		if (size < 16) size = 16;
		if (size > 256) size = 256;
		char *name = g_strndup(path + 3, strlen(path + 3));
		serve_icon(fd, name, size);
		g_free(name);
		goto out;
	}

	/* /m?path=/abs/arquivo — mídia do usuário */
	if (strcmp(path, "/m") == 0) {
		char *fp = NULL;
		if (query && strncmp(query, "path=", 5) == 0) fp = query + 5;
		if (!fp || fp[0] != '/') { reply_404(fd); goto out; }
		if (strstr(fp, "..")) { reply_404(fd); goto out; }
		if (!serve_file(fd, fp, range)) reply_404(fd);
		goto out;
	}

	/* assets da UI: /canvas/… /deck/… /shared/… */
	if (g_str_has_prefix(path, "/canvas") || g_str_has_prefix(path, "/deck")
			|| g_str_has_prefix(path, "/shared")) {
		const char *rel = path + 1; /* sem a barra inicial */
		if (rel[strlen(rel) - 1] == '/') {
			char *idx = g_strconcat(rel, "index.html", NULL);
			char *fs = g_build_filename(g_shell_dir, idx, NULL);
			if (!serve_file(fd, fs, range)) reply_404(fd);
			g_free(idx); g_free(fs);
			goto out;
		}
		char *fs = g_build_filename(g_shell_dir, rel, NULL);
		if (!serve_file(fd, fs, range)) reply_404(fd);
		g_free(fs);
		goto out;
	}

	/* /debug/emit?json=<urlencoded>&page=deck — gancho de depuração:
	   injeta um evento Padrao.emit() numa página (testes sem input) */
	if (strcmp(path, "/debug/emit") == 0) {
		const char *jp = query ? strstr(query, "json=") : NULL;
		if (!jp) { reply_404(fd); goto out; }
		char *json = url_decode(jp + 5);
		const char *pp = strstr(query, "page=");
		char page[32] = "";
		if (pp) {
			snprintf(page, sizeof(page), "%s", pp + 5);
			char *amp = strchr(page, '&');
			if (amp) *amp = '\0';
		}
		if (json && *json) host_debug_emit(page, json);
		g_free(json);
		static const char body[] = "ok\n";
		reply_buffer(fd, 200, "OK", "text/plain", body, sizeof(body) - 1);
		goto out;
	}

	reply_404(fd);

out:
	g_free(path);
	g_free(query);
}

/* ------------------------------- threads ------------------------------ */

static gpointer conn_thread(gpointer data) {
	int fd = GPOINTER_TO_INT(data);

	struct timeval tv = { .tv_sec = 10, .tv_usec = 0 };
	setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));
	setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &tv, sizeof(tv));

	char buf[16384];
	size_t used = 0;
	while (used < sizeof(buf) - 1) {
		ssize_t n = read(fd, buf + used, sizeof(buf) - 1 - used);
		if (n < 0) {
			if (errno == EINTR) continue;
			break;
		}
		if (n == 0) break;
		used += (size_t)n;
		buf[used] = '\0';
		if (strstr(buf, "\r\n\r\n") || strstr(buf, "\n\n")) break;
	}
	if (used > 0) {
		buf[used] = '\0';
		handle_request(fd, buf);
	}
	close(fd);
	return NULL;
}

static gpointer accept_loop(gpointer data) {
	(void)data;
	for (;;) {
		int fd = accept(listen_fd, NULL, NULL);
		if (fd < 0) {
			if (errno == EINTR) continue;
			g_usleep(50000);
			continue;
		}
		GThread *t = g_thread_new("padrao-http-conn", conn_thread,
			GINT_TO_POINTER(fd));
		g_thread_unref(t);
	}
	return NULL;
}

int http_start(void) {
	listen_fd = socket(AF_INET, SOCK_STREAM | SOCK_CLOEXEC, 0);
	if (listen_fd < 0) return -1;
	int one = 1;
	setsockopt(listen_fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one));

	struct sockaddr_in sa = {
		.sin_family = AF_INET,
		.sin_port = 0, /* efêmera */
	};
	sa.sin_addr.s_addr = htonl(INADDR_LOOPBACK);

	if (bind(listen_fd, (struct sockaddr *)&sa, sizeof(sa)) < 0
			|| listen(listen_fd, 16) < 0) {
		close(listen_fd);
		listen_fd = -1;
		return -1;
	}
	socklen_t sl = sizeof(sa);
	if (getsockname(listen_fd, (struct sockaddr *)&sa, &sl) < 0) {
		close(listen_fd);
		listen_fd = -1;
		return -1;
	}
	g_http_port = ntohs(sa.sin_port);

	GThread *t = g_thread_new("padrao-http", accept_loop, NULL);
	g_thread_unref(t);
	return 0;
}
