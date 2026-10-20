/*
 * jx.c — escape e leitura mínima de JSON (mesma filosofia do compositor,
 * sem nenhuma dependência: o protocolo é sempre JSON de uma linha gerado
 * por nós mesmos).
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "padrao_host.h"

size_t jx_escape(char *dst, size_t cap, const char *src) {
	size_t n = 0;
	for (const unsigned char *p = (const unsigned char *)(src ? src : "");
			*p && n + 7 < cap; p++) {
		switch (*p) {
		case '"':  dst[n++] = '\\'; dst[n++] = '"';  break;
		case '\\': dst[n++] = '\\'; dst[n++] = '\\'; break;
		case '\b': dst[n++] = '\\'; dst[n++] = 'b';  break;
		case '\f': dst[n++] = '\\'; dst[n++] = 'f';  break;
		case '\n': dst[n++] = '\\'; dst[n++] = 'n';  break;
		case '\r': dst[n++] = '\\'; dst[n++] = 'r';  break;
		case '\t': dst[n++] = '\\'; dst[n++] = 't';  break;
		default:
			if (*p < 0x20) {
				n += (size_t)snprintf(dst + n, cap - n, "\\u%04x", *p);
			} else {
				dst[n++] = (char)*p;
			}
		}
	}
	dst[n] = '\0';
	return n;
}

/* Localiza `"key"` no nível superior. */
static const char *find_key(const char *json, const char *key) {
	if (!json || !key) return NULL;
	size_t klen = strlen(key);
	int depth = 0;
	gboolean in_str = FALSE;

	for (const char *p = json; *p; p++) {
		char c = *p;
		if (in_str) {
			if (c == '\\') { p++; continue; }
			if (c == '"') in_str = FALSE;
			continue;
		}
		if (c == '"') {
			/* depth 1 = nível superior dentro do objeto raiz */
			if (depth == 1 && strncmp(p + 1, key, klen) == 0
					&& p[1 + klen] == '"') {
				const char *q = p + 2 + klen;
				while (*q == ' ' || *q == '\t') q++;
				if (*q == ':') return q + 1;
			}
			p++;
			while (*p) {
				if (*p == '\\') p++;
				else if (*p == '"') break;
				p++;
			}
			continue;
		}
		if (c == '{' || c == '[') depth++;
		else if (c == '}' || c == ']') { if (depth > 0) depth--; }
	}
	return NULL;
}

static const char *skip_ws(const char *p) {
	while (*p == ' ' || *p == '\t' || *p == '\n' || *p == '\r') p++;
	return p;
}

gboolean jx_num(const char *json, const char *key, double *out) {
	const char *p = find_key(json, key);
	if (!p) return FALSE;
	p = skip_ws(p);
	char *end = NULL;
	double v = strtod(p, &end);
	if (end == p) return FALSE;
	*out = v;
	return TRUE;
}

gboolean jx_int(const char *json, const char *key, long *out) {
	double d;
	if (!jx_num(json, key, &d)) return FALSE;
	*out = (long)d;
	return TRUE;
}

gboolean jx_str(const char *json, const char *key, char *out, size_t cap) {
	const char *p = find_key(json, key);
	if (!p) return FALSE;
	p = skip_ws(p);
	if (*p != '"') return FALSE;
	p++;
	size_t n = 0;
	while (*p && *p != '"') {
		char c = *p++;
		if (c == '\\' && *p) {
			char e = *p++;
			switch (e) {
			case 'n': c = '\n'; break;
			case 't': c = '\t'; break;
			case 'r': c = '\r'; break;
			case 'b': c = '\b'; break;
			case 'f': c = '\f'; break;
			case 'u':
				if (p[0] && p[1] && p[2] && p[3]) {
					char hex[5] = { p[0], p[1], p[2], p[3], 0 };
					unsigned cp = (unsigned)strtoul(hex, NULL, 16);
					p += 4;
					if (cp < 0x80) {
						if (n + 1 < cap) out[n++] = (char)cp;
					} else if (cp < 0x800) {
						if (n + 2 < cap) {
							out[n++] = (char)(0xC0 | (cp >> 6));
							out[n++] = (char)(0x80 | (cp & 0x3F));
						}
					} else {
						if (n + 3 < cap) {
							out[n++] = (char)(0xE0 | (cp >> 12));
							out[n++] = (char)(0x80 | ((cp >> 6) & 0x3F));
							out[n++] = (char)(0x80 | (cp & 0x3F));
						}
					}
					continue;
				}
				c = e;
				break;
			default: c = e; break;
			}
		}
		if (n + 1 < cap) out[n++] = c;
	}
	out[n] = '\0';
	return TRUE;
}

gboolean jx_raw(const char *json, const char *key,
		const char **start, size_t *len) {
	const char *p = find_key(json, key);
	if (!p) return FALSE;
	p = skip_ws(p);
	if (*p != '{' && *p != '[' && *p != '"') {
		const char *b = p;
		while (*p && *p != ',' && *p != '}' && *p != ']' && *p != ' ') p++;
		*start = b;
		*len = (size_t)(p - b);
		return TRUE;
	}
	if (*p == '"') {
		const char *b = p;
		p++;
		while (*p) {
			if (*p == '\\') p += 2;
			else if (*p == '"') { p++; break; }
			else p++;
		}
		*start = b;
		*len = (size_t)(p - b);
		return TRUE;
	}
	char open = *p, close = (open == '{') ? '}' : ']';
	int depth = 0;
	gboolean in_str = FALSE;
	const char *b = p;
	for (; *p; p++) {
		char c = *p;
		if (in_str) {
			if (c == '\\') { p++; continue; }
			if (c == '"') in_str = FALSE;
			continue;
		}
		if (c == '"') { in_str = TRUE; continue; }
		if (c == open) depth++;
		else if (c == close) {
			depth--;
			if (depth == 0) {
				*start = b;
				*len = (size_t)(p - b + 1);
				return TRUE;
			}
		}
	}
	return FALSE;
}
