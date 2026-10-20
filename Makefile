# Padrão Desktop — build
#
# Uso (dentro de padrao/):
#   make            # compila compositor + host
#   make run        # inicia em modo aninhado (dentro da sessão atual)
#   make clean
#
# As dependências vivem em ~/.local/share/padrao/{rpms,sysroot} e são
# baixadas sem sudo por deps/fetch-deps.sh (veja deps/env.sh).
# Os caminhos são relativos de propósito: o diretório do projeto contém
# espaços, e o make não lida bem com espaços em $(wildcard)/$(CURDIR).

CC      ?= gcc
pc      = $(shell bash -c '. deps/env.sh >/dev/null 2>&1; pkg-config $(1)')

WARN    := -Wall -Wextra -Wno-unused-parameter -Wno-missing-field-initializers
OPT     := -O2 -g

# ------------------------- compositor -------------------------
WLR_PKGS    := wlroots-0.20 wayland-server xkbcommon
# a camada web (WebKitGTK) é criada dentro do próprio compositor
WEB_PKGS    := gtk+-3.0 gtk-layer-shell-0 webkit2gtk-4.1
COMP_CFLAGS := -Icompositor/include -Icompositor/protocol -DWLR_USE_UNSTABLE \
               $(call pc,--cflags $(WLR_PKGS) $(WEB_PKGS)) \
               $(WARN) -std=gnu11
COMP_LIBS   := $(call pc,--libs $(WLR_PKGS) $(WEB_PKGS)) -lm -lpthread
COMP_SRCS   := $(wildcard compositor/src/*.c)

# Headers de protocolo gerados (wayland-scanner), necessários porque os
# headers wlroots incluem "wlr-*-protocol.h" com aspas (não instalados
# nos RPMs de dev).
PROTO_DIR   := compositor/protocol
PROTO_HDRS  := $(PROTO_DIR)/wlr-layer-shell-unstable-v1-protocol.h \
               $(PROTO_DIR)/wlr-output-power-management-unstable-v1-protocol.h

$(PROTO_DIR)/%-protocol.h: $(PROTO_DIR)/%.xml
	bash -c '. deps/env.sh >/dev/null 2>&1; wayland-scanner server-header $< $@'

# --------------------------- host -----------------------------
HOST_PKGS    := gtk+-3.0 gtk-layer-shell-0 webkit2gtk-4.1 glib-2.0 gio-2.0 \
                gio-unix-2.0 javascriptcoregtk-4.1
HOST_CFLAGS  := $(call pc,--cflags $(HOST_PKGS)) $(WARN) -std=gnu11
HOST_LIBS    := $(call pc,--libs $(HOST_PKGS))
HOST_SRCS    := $(wildcard shell-host/src/*.c)

# --------------------------- shot (opcional) ------------------
# cliente Wayland que fala wlr-screencopy-v1 para capturar a saída do
# Padrão (alternativa autossuficiente ao grim do sistema).
SHOT_XML     := compositor/protocol/wlr-screencopy-unstable-v1.xml
SHOT_PROTO_C := compositor/protocol/wlr-screencopy-unstable-v1-protocol.c
SHOT_PROTO_H := compositor/protocol/wlr-screencopy-unstable-v1-client-protocol.h
SHOT_CFLAGS  := -Icompositor/protocol $(call pc,--cflags wayland-client libpng) \
                $(WARN) -std=gnu11
SHOT_LIBS    := $(call pc,--libs wayland-client libpng)

all: padrao-compositor padrao-host

padrao-compositor: $(COMP_SRCS) compositor/include/padrao.h $(PROTO_HDRS)
	$(CC) $(OPT) $(COMP_CFLAGS) -o $@ $(COMP_SRCS) $(COMP_LIBS)

padrao-host: $(HOST_SRCS)
	$(CC) $(OPT) $(HOST_CFLAGS) -o $@ $(HOST_SRCS) $(HOST_LIBS)

# header de cliente (o alvo acima só gera o de server)
$(SHOT_PROTO_H): $(SHOT_XML)
	bash -c '. deps/env.sh >/dev/null 2>&1; wayland-scanner client-header $< $@'
$(SHOT_PROTO_C): $(SHOT_XML)
	bash -c '. deps/env.sh >/dev/null 2>&1; wayland-scanner private-code $< $@'

tools/padrao-shot: tools/padrao-shot.c $(SHOT_PROTO_C) $(SHOT_PROTO_H)
	$(CC) $(OPT) $(SHOT_CFLAGS) -o $@ tools/padrao-shot.c $(SHOT_PROTO_C) $(SHOT_LIBS)

run: all
	bash run.sh

clean:
	rm -f padrao-compositor padrao-host tools/padrao-shot

.PHONY: all clean run
