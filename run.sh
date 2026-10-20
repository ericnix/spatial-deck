#!/usr/bin/env bash
# Sobe o desktop Padrão aninhado na sessão Wayland atual.
# Uso: bash run.sh   (ou ./run.sh)
set -euo pipefail
cd "$(dirname "$0")"

# Bibliotecas do sysroot (wlroots/GTK/WebKit extraídos sem sudo) em runtime.
# shellcheck source=deps/env.sh
. ./deps/env.sh >/dev/null 2>&1 || true
export LD_LIBRARY_PATH="$PADRAO_SYSROOT/usr/lib64:$PADRAO_SYSROOT/usr/lib:${LD_LIBRARY_PATH:-}"

SOCK="${XDG_RUNTIME_DIR:?XDG_RUNTIME_DIR ausente}/padrao.sock"
HOST_LOG="${TMPDIR:-/tmp}/padrao-host.log"
COMP_LOG="${TMPDIR:-/tmp}/padrao-compositor.log"
HOST_PID=""
COMP_PID=""

cleanup() {
  [ -n "$HOST_PID" ] && kill "$HOST_PID" 2>/dev/null || true
  [ -n "$COMP_PID" ] && kill "$COMP_PID" 2>/dev/null || true
  wait 2>/dev/null || true
  rm -f "$SOCK"
}
trap cleanup EXIT INT TERM

rm -f "$SOCK"

# 1. compositor
# Binários do meson (build/) têm preferência; senão usa os do make.
COMP_BIN="./padrao-compositor"
HOST_BIN="./padrao-host"
[ -x "./build/padrao-compositor" ] && COMP_BIN="./build/padrao-compositor"
[ -x "./build/padrao-host" ] && HOST_BIN="./build/padrao-host"

echo "[run] iniciando $COMP_BIN (log: $COMP_LOG)"

# O renderer GLES2 do Mesa (radeonsi) corrompe o heap neste ambiente e
# aborta o compositor no shutdown ("corrupted double-linked list").
# vulkan e pixman saem limpos. Como o wlroots não faz fallback quando
# WLR_RENDERER está fixado, tentamos o vulkan e caímos para o pixman (CPU).
start_compositor() {
  WLR_RENDERER="$1" "$COMP_BIN" >"$COMP_LOG" 2>&1 &
  COMP_PID=$!
  for _ in $(seq 1 100); do
    [ -S "$SOCK" ] && return 0
    kill -0 "$COMP_PID" 2>/dev/null || return 1
    sleep 0.1
  done
  return 1
}

if ! start_compositor "${WLR_RENDERER:-vulkan}"; then
  echo "[run] renderer '${WLR_RENDERER:-vulkan}' indisponível; tentando pixman"
  rm -f "$SOCK"
  start_compositor pixman || {
    echo "[run] compositor morreu; veja $COMP_LOG"; exit 1
  }
fi

# 2. espera o socket do IPC
for _ in $(seq 1 100); do
  [ -S "$SOCK" ] && break
  kill -0 "$COMP_PID" 2>/dev/null || { echo "[run] compositor morreu; veja $COMP_LOG"; exit 1; }
  sleep 0.1
done
[ -S "$SOCK" ] || { echo "[run] socket $SOCK não apareceu; veja $COMP_LOG"; exit 1; }

# 3. host shell — cliente do compositor Padrão (socket Wayland "padrao").
#    O compositor ficou com o WAYLAND_DISPLAY externo (aninhamento), então o
#    host precisa dele apontando para o display interno.
echo "[run] iniciando $HOST_BIN (log: $HOST_LOG)"
export WAYLAND_DISPLAY=padrao
export GDK_BACKEND=wayland
export PADRAO_SHELL="$PWD/web"
"$HOST_BIN" >"$HOST_LOG" 2>&1 &
HOST_PID=$!

echo "[run] Padrão rodando — host em WAYLAND_DISPLAY=padrao, IPC=$SOCK"
echo "[run] Ctrl-C encerra (compositor + host)"

# 4. vigia; se um morrer, derruba o outro
while kill -0 "$COMP_PID" 2>/dev/null && kill -0 "$HOST_PID" 2>/dev/null; do
  sleep 1
done
echo "[run] processo terminou:"
kill -0 "$COMP_PID" 2>/dev/null || echo "  compositor saiu (log: $COMP_LOG)"
kill -0 "$HOST_PID" 2>/dev/null || echo "  host saiu (log: $HOST_LOG)"
