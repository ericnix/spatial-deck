#!/usr/bin/env bash
# Captura os estados do deck (open/grid) do desktop em execução.
#
# Uso:
#   tools/capturar.sh [porta]
#
# O compositor precisa estar rodando (./run.sh). A porta é a do servidor
# HTTP do shell-host; sem argumento, ela é lida do log do host.
set -euo pipefail

cd "$(dirname "$0")/.."

OUT_DIR="docs/screenshots"
mkdir -p "$OUT_DIR"

HOST_LOG="${TMPDIR:-/tmp}/padrao-host.log"

# ---------------------------------------------------------------- porta ---
PORT="${1:-}"
if [ -z "$PORT" ] && [ -r "$HOST_LOG" ]; then
	PORT=$(grep -oE "http://127\.0\.0\.1:[0-9]+" "$HOST_LOG" | head -1 |
		grep -oE "[0-9]+$" || true)
fi
if [ -z "$PORT" ]; then
	echo "erro: não achei a porta do shell-host." >&2
	echo "       suba o desktop com ./run.sh e tente de novo," >&2
	echo "       ou passe a porta: tools/capturar.sh <porta>" >&2
	exit 1
fi

# ------------------------------------------------- utilitário de captura ---
SHOT=""
for cand in build/padrao-shot tools/padrao-shot; do
	if [ -x "$cand" ]; then SHOT="$cand"; break; fi
done
if [ -z "$SHOT" ]; then
	echo "erro: padrao-shot não encontrado." >&2
	echo "       compile com: make tools/padrao-shot" >&2
	exit 1
fi

# ------------------------------------------------------------- ambiente ---
# o padrao-shot fala com o compositor (socket WAYLAND_DISPLAY=padrao) e
# precisa das libs do sysroot local
# shellcheck source=../deps/env.sh
. deps/env.sh >/dev/null 2>&1 || true
export LD_LIBRARY_PATH="$PADRAO_SYSROOT/usr/lib64:$PADRAO_SYSROOT/usr/lib:${LD_LIBRARY_PATH:-}"
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/1000}"
export WAYLAND_DISPLAY=padrao

# o JSON do gancho de depuração, url-encoded (constante por estado)
JSON_OPEN='%7B%22t%22%3A%22dbg%22%2C%22state%22%3A%22open%22%7D'
JSON_GRID='%7B%22t%22%3A%22dbg%22%2C%22state%22%3A%22grid%22%7D'

capturar() {   # capturar <json> <arquivo> <rótulo>
	local json="$1" out="$2" label="$3"
	echo "=== Capturando estado $label ==="
	curl -sf "http://127.0.0.1:$PORT/debug/emit?page=deck&json=$json" >/dev/null
	sleep 1   # a animação do deck leva ~320ms; 1s dá folga
	"$SHOT" "$OUT_DIR/$out"
}

# checagem prévia: sem o host rodando a porta do log é velha e o curl
# falharia com um código de saída críptico
if ! curl -sf "http://127.0.0.1:$PORT/debug/emit?page=deck&json=$JSON_OPEN" \
	>/dev/null 2>&1; then
	echo "erro: o shell-host não respondeu em 127.0.0.1:$PORT." >&2
	echo "       suba o desktop com ./run.sh e tente de novo" >&2
	echo "       (a porta é lida de $HOST_LOG — pode ser de uma" >&2
	echo "        execução anterior)." >&2
	exit 1
fi

capturar "$JSON_OPEN" "padrao-deck-aberto.png" "OPEN"
capturar "$JSON_GRID" "padrao-deck-grade.png" "GRID"

echo "Capturas concluídas com sucesso em $OUT_DIR/"
