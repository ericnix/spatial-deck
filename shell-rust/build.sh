#!/usr/bin/env bash
# Compila o shell GTK3 + WebKitGTK do desktop Rust em target/.
set -euo pipefail

cd "$(dirname "$0")/.."

# Sysroot local com GTK/WebKit (ver deps/fetch-deps.sh e deps/env.sh).
# shellcheck source=deps/env.sh
. ./deps/env.sh >/dev/null 2>&1 || true
export LD_LIBRARY_PATH="$PADRAO_SYSROOT/usr/lib64:$PADRAO_SYSROOT/usr/lib:${LD_LIBRARY_PATH:-}"

mkdir -p target
cc -O2 -o target/spatial-deck-shell shell-rust/main.c \
	-Wl,-rpath,"$PADRAO_SYSROOT/usr/lib64" \
	-Wl,-rpath,"$PADRAO_SYSROOT/usr/lib" \
	$(pkg-config --cflags --libs gtk+-3.0 webkit2gtk-4.1)
