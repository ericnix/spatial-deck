# Padrão Desktop — build (meson + ninja)
#
# Alternativa ao Makefile, com o mesmo resultado:
#   source deps/env.sh      # aponta pkg-config/headers para o sysroot local
#   meson setup build
#   ninja -C build
#
# O `deps/env.sh` é obrigatório antes do setup: é ele que define
# PKG_CONFIG_LIBDIR/PKG_CONFIG_SYSROOT_DIR para que o pkg-config encontre o
# wlroots extraído em ~/.local/share/padrao/sysroot (sem sudo, sem /usr).
#
# Sem o env, o meson falha em "wlroots-0.20 not found".

project('padrao', 'c',
  version: '0.1.0',
  default_options: ['c_std=gnu11', 'warning_level=2'])

# Meson >= 1.0 não expõe mais a global `cc`; use get_compiler().
cc = meson.get_compiler('c')

# ------------------------- compositor (wlroots) ---------------------
wlroots        = dependency('wlroots-0.20')
wayland_server = dependency('wayland-server')
xkbcommon      = dependency('xkbcommon')
m              = cc.find_library('m', required: false)

# --------------------------- host (GTK/WebKit) ------------------------
gtk       = dependency('gtk+-3.0')
gtk_layer = dependency('gtk-layer-shell-0')
webkit    = dependency('webkit2gtk-4.1')
glib      = dependency('glib-2.0')
gio       = dependency('gio-2.0')
gio_unix  = dependency('gio-unix-2.0')
jscore    = dependency('javascriptcoregtk-4.1')

# --------------------------- ferramenta shot --------------------------
wayland_client = dependency('wayland-client')
png            = dependency('libpng')

# compositor/protocol entra no include path porque os headers do wlroots
# incluem "wlr-*-protocol.h" com aspas, e os RPMs de dev não instalam esses
# headers (gerados por wayland-scanner).
compositor_inc = include_directories('compositor/include', 'compositor/protocol')
host_inc       = include_directories('shell-host/src')

compositor_sources = files(
  'compositor/src/input.c',
  'compositor/src/ipc.c',
  'compositor/src/layer.c',
  'compositor/src/main.c',
  'compositor/src/output.c',
  'compositor/src/util.c',
  'compositor/src/view.c',
)

executable('padrao-compositor', compositor_sources,
  include_directories: compositor_inc,
  c_args: ['-DWLR_USE_UNSTABLE', '-Wno-unused-parameter', '-Wno-missing-field-initializers'],
  dependencies: [wlroots, wayland_server, xkbcommon, m, gtk, gtk_layer, webkit],
  install: true)

host_sources = files(
  'shell-host/src/http.c',
  'shell-host/src/ipc.c',
  'shell-host/src/jx.c',
  'shell-host/src/main.c',
)

executable('padrao-host', host_sources,
  include_directories: host_inc,
  c_args: ['-Wno-unused-parameter', '-Wno-missing-field-initializers'],
  dependencies: [gtk, gtk_layer, webkit, glib, gio, gio_unix, jscore],
  install: true)

executable('padrao-shot',
  files('tools/padrao-shot.c', 'compositor/protocol/wlr-screencopy-unstable-v1-protocol.c'),
  include_directories: include_directories('compositor/protocol'),
  c_args: ['-Wno-unused-parameter', '-Wno-missing-field-initializers'],
  dependencies: [wayland_client, png],
  install: true)
