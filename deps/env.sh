# Ambiente de build do Padrão Desktop. Source deste arquivo antes de compilar:
#   source deps/env.sh
# Usa o sysroot local (headers/bibliotecas extraídos sem sudo) e o fallback
# do sistema para .pc que já existam instalados.
PADRAO_DEPS="${PADRAO_DEPS:-$HOME/.local/share/padrao}"
PADRAO_SYSROOT="$PADRAO_DEPS/sysroot"

export PADRAO_SYSROOT
export PKG_CONFIG_SYSROOT_DIR="$PADRAO_SYSROOT"
export PKG_CONFIG_LIBDIR="$PADRAO_SYSROOT/usr/lib64/pkgconfig:$PADRAO_SYSROOT/usr/lib/pkgconfig:$PADRAO_SYSROOT/usr/share/pkgconfig"
export PKG_CONFIG_PATH=""
