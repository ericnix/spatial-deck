/*
 * padrao-shot — screenshot da saída do Padrão via wlr-screencopy-v1.
 *
 * Uso (com o Padrão rodando):
 *   WAYLAND_DISPLAY=padrao ./tools/padrao-shot out.png
 *
 * Por que não grim: grim é um binário do sistema que enxerga qualquer
 * display Wayland; este tool é o caminho canônico do Padrão, compilado
 * contra o mesmo sysroot, e serve de referência mínima para o protocolo
 * screencopy (bind manager -> capturar frame -> copiar buffer -> PNG).
 *
 * Protocolo:
 *   1. bind zwlr_screencopy_manager_v1;
 *   2. copy(output, overlay=0) -> zwlr_screencopy_frame_v1;
 *   3. no "ready": alocar wl_buffer do tamanho/formato pedidos, copy_pixels;
 *   4. no "buffer_done": copiar a wl_buffer linha a linha e salvar PNG.
 */
#define _GNU_SOURCE /* memfd_create */
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#include <wayland-client-core.h>
#include <wayland-client-protocol.h>
#include <wayland-client.h>

#include <png.h>

#include "wlr-screencopy-unstable-v1-client-protocol.h"

typedef struct {
	struct wl_output *output;
	uint32_t name;
	int done;
} ShotOutput;

typedef struct {
	struct wl_registry *registry;
	struct zwlr_screencopy_manager_v1 *manager;
	ShotOutput *outputs;
	size_t n_outputs;

	/* estado da captura */
	struct zwlr_screencopy_frame_v1 *frame;
	struct wl_shm *shm;
	uint32_t width, height, stride;
	uint32_t format;
	int overlay;
	int bound;      /* frame pediu buffer */
	int ready;      /* frame sinalizou ready */
	int failed;
	int y_invert;   /* linhas de baixo pra cima (flag do protocolo) */
	void *pixels;   /* cópia em CPU (malloc) */
	int exit_code;
	int running;
} Shot;

/* ------------------------------- wl_shm -------------------------------- */

static int shm_pool_fd(size_t size) {
	char name[64];
	snprintf(name, sizeof(name), "/padrao-shot-%d", getpid());
	int fd = memfd_create(name, MFD_CLOEXEC);
	if (fd < 0) return -1;
	if (ftruncate(fd, (off_t)size) < 0) { close(fd); return -1; }
	return fd;
}

static struct wl_buffer *shm_buffer(Shot *s, void **data, size_t size) {
	int fd = shm_pool_fd(size);
	if (fd < 0) return NULL;
	struct wl_shm_pool *pool = wl_shm_create_pool(s->shm, fd, (int)size);
	if (!pool) { close(fd); return NULL; }
	*data = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	close(fd); /* o pool segura o mapeamento */
	if (*data == MAP_FAILED) { wl_shm_pool_destroy(pool); return NULL; }
	struct wl_buffer *buf = wl_shm_pool_create_buffer(pool, 0,
		(int)s->width, (int)s->height, (int)s->stride, s->format);
	wl_shm_pool_destroy(pool);
	return buf;
}

/* --------------------------- frame callbacks --------------------------- */

static void frame_buffer(void *data, struct zwlr_screencopy_frame_v1 *f,
		uint32_t format, uint32_t w, uint32_t h, uint32_t stride) {
	Shot *s = data;
	s->format = format;
	s->width = w;
	s->height = h;
	s->stride = stride;
	s->bound = 1;
}

static void frame_flags(void *data, struct zwlr_screencopy_frame_v1 *f,
		uint32_t flags) {
	Shot *s = data;
	/* ZWLR_SCREENCOPY_FRAME_V1_FLAGS_Y_INVERT = 1: linhas de baixo pra
	   cima; para PNG viramos de volta no save. */
	s->y_invert = (flags & 1) ? 1 : 0;
}

static void frame_ready(void *data, struct zwlr_screencopy_frame_v1 *f,
		uint32_t tv_sec_hi, uint32_t tv_sec_lo, uint32_t tv_nsec) {
	Shot *s = data;
	s->ready = 1;
}

static void frame_failed(void *data, struct zwlr_screencopy_frame_v1 *f) {
	Shot *s = data;
	s->failed = 1;
}

/* damage: só informativo; não usamos (captura o frame inteiro) */
static void frame_damage(void *data, struct zwlr_screencopy_frame_v1 *f,
		uint32_t x, uint32_t y, uint32_t w, uint32_t h) {
	(void)data; (void)f; (void)x; (void)y; (void)w; (void)h;
}

/* linux_dmabuf: o compositor anuncia suporte, mas nós usamos wl_shm */
static void frame_linux_dmabuf(void *data,
		struct zwlr_screencopy_frame_v1 *f,
		uint32_t format, uint32_t w, uint32_t h) {
	(void)data; (void)f; (void)format; (void)w; (void)h;
}

/* buffer_done: todos os tipos de buffer reportados — hora de pedir copy */
static void frame_buffer_done(void *data,
		struct zwlr_screencopy_frame_v1 *f) {
	(void)data; (void)f;
}

static const struct zwlr_screencopy_frame_v1_listener frame_listener = {
	.buffer = frame_buffer,
	.flags = frame_flags,
	.ready = frame_ready,
	.failed = frame_failed,
	.damage = frame_damage,
	.linux_dmabuf = frame_linux_dmabuf,
	.buffer_done = frame_buffer_done,
};

/* ----------------------------- registry -------------------------------- */

static void registry_global(void *data, struct wl_registry *reg,
		uint32_t name, const char *iface, uint32_t version) {
	Shot *s = data;
	if (strcmp(iface, wl_shm_interface.name) == 0) {
		s->shm = wl_registry_bind(reg, name, &wl_shm_interface, 1);
	} else if (strcmp(iface, wl_output_interface.name) == 0) {
		s->outputs = realloc(s->outputs,
			(s->n_outputs + 1) * sizeof(ShotOutput));
		ShotOutput *o = &s->outputs[s->n_outputs++];
		o->output = wl_registry_bind(reg, name, &wl_output_interface, 1);
		o->name = name;
		o->done = 0;
	} else if (strcmp(iface,
			zwlr_screencopy_manager_v1_interface.name) == 0) {
		uint32_t bindv = version < 3 ? version : 3;
		s->manager = wl_registry_bind(reg, name,
			&zwlr_screencopy_manager_v1_interface, bindv);
	}
}

static void registry_global_remove(void *data, struct wl_registry *reg,
		uint32_t name) {
}

static const struct wl_registry_listener registry_listener = {
	.global = registry_global,
	.global_remove = registry_global_remove,
};

/* -------------------------------- PNG ---------------------------------- */

static int save_png(const char *path, const Shot *s) {
	FILE *f = fopen(path, "wb");
	if (!f) { fprintf(stderr, "padrao-shot: %s: %s\n", path, strerror(errno)); return 1; }
	png_structp png = png_create_write_struct(PNG_LIBPNG_VER_STRING,
		NULL, NULL, NULL);
	png_infop info = png_create_info_struct(png);
	if (!png || !info || setjmp(png_jmpbuf(png))) {
		if (png) png_destroy_write_struct(&png, info ? &info : NULL);
		fclose(f);
		return 1;
	}
	png_init_io(png, f);
	png_set_IHDR(png, info, s->width, s->height, 8,
		PNG_COLOR_TYPE_RGB, PNG_INTERLACE_NONE,
		PNG_COMPRESSION_TYPE_DEFAULT, PNG_FILTER_TYPE_DEFAULT);
	png_write_info(png, info);

	/* O screencopy entrega RGBX little-endian (fmt 0x34324258 = "XB24",
	   byte de alfa 0xff no fim): na memória a ordem é R,G,B,X.
	   Canais válidos em p[0],p[1],p[2]; descartamos o alfa em p[3]. */
	uint8_t *row = malloc((size_t)s->width * 3);
	for (uint32_t y = 0; y < s->height; y++) {
		/* y_invert: o compositor entrega linhas de baixo pra cima */
		uint32_t src = s->y_invert ? (s->height - 1 - y) : y;
		const uint8_t *sp = (const uint8_t *)s->pixels + (size_t)src * s->stride;
		for (uint32_t x = 0; x < s->width; x++) {
			row[x * 3 + 0] = sp[x * 4 + 0]; /* R */
			row[x * 3 + 1] = sp[x * 4 + 1]; /* G */
			row[x * 3 + 2] = sp[x * 4 + 2]; /* B */
		}
		png_write_row(png, row);
	}
	free(row);
	png_write_end(png, NULL);
	png_destroy_write_struct(&png, &info);
	fclose(f);
	return 0;
}

/* ------------------------------- main ---------------------------------- */

int main(int argc, char *argv[]) {
	const char *out = argc > 1 ? argv[1] : "padrao-shot.png";

	Shot s = { 0 };
	struct wl_display *display = wl_display_connect(NULL);
	if (!display) {
		fprintf(stderr, "padrao-shot: não conectei no Wayland "
			"(WAYLAND_DISPLAY=%s?)\n",
			getenv("WAYLAND_DISPLAY") ? getenv("WAYLAND_DISPLAY") : "(vazio)");
		return 1;
	}

	s.registry = wl_display_get_registry(display);
	wl_registry_add_listener(s.registry, &registry_listener, &s);
	wl_display_roundtrip(display); /* globals */
	wl_display_roundtrip(display); /* binds + wl_shm done */

	if (!s.manager || !s.shm || s.n_outputs == 0) {
		fprintf(stderr, "padrao-shot: faltou globals "
			"(manager=%p shm=%p outputs=%zu)\n",
			(void *)s.manager, (void *)s.shm, s.n_outputs);
		return 1;
	}

	/* primeira saída */
	s.frame = zwlr_screencopy_manager_v1_capture_output(s.manager,
		0 /* overlay */, s.outputs[0].output);
	zwlr_screencopy_frame_v1_add_listener(s.frame, &frame_listener, &s);

	/* espera o compositor descrever o frame (buffer/flags) */
	while (!s.failed && !s.bound) {
		wl_display_flush(display);
		if (wl_display_dispatch(display) < 0) break;
	}

	if (s.failed || !s.bound) {
		fprintf(stderr, "padrao-shot: captura falhou\n");
		return 1;
	}

	size_t size = (size_t)s.stride * s.height;
	void *data = NULL;
	struct wl_buffer *buf = shm_buffer(&s, &data, size);
	if (!buf || !data) {
		fprintf(stderr, "padrao-shot: sem buffer shm (%ux%u stride %u)\n",
			s.width, s.height, s.stride);
		return 1;
	}
	s.pixels = data;

	/* pede a cópia e espera o buffer chegar à GPU */
	zwlr_screencopy_frame_v1_copy(s.frame, buf);
	while (!s.failed && !s.ready) {
		wl_display_flush(display);
		if (wl_display_dispatch(display) < 0) break;
	}

	if (s.failed) {
		fprintf(stderr, "padrao-shot: copy falhou\n");
		return 1;
	}

	int rc = save_png(out, &s);
	if (rc == 0)
		fprintf(stderr, "padrao-shot: %s (%ux%u)\n", out, s.width, s.height);

	/* limpeza (o display é destruído no exit; só garante o munmap) */
	munmap(data, size);
	wl_buffer_destroy(buf);
	zwlr_screencopy_frame_v1_destroy(s.frame);
	wl_display_disconnect(display);
	return rc;
}
