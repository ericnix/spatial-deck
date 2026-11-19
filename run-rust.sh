#!/usr/bin/env bash
# Sobe o desktop Rust aninhado na sessão gráfica atual:
# compositor Smithay + shell GTK/WebKit com o frontend WASM.
set -euo pipefail

cd "$(dirname "$0")"

if [[ ! -f web/pkg/spatial_deck_frontend.js ]]; then
	./build-wasm.sh
fi

cargo build --workspace

# O shell usa GTK/WebKit do sysroot local (deps/env.sh).
# shellcheck source=deps/env.sh
. ./deps/env.sh >/dev/null 2>&1 || true
export LD_LIBRARY_PATH="$PADRAO_SYSROOT/usr/lib64:$PADRAO_SYSROOT/usr/lib:${LD_LIBRARY_PATH:-}"

if [[ ! -x target/spatial-deck-shell || shell-rust/main.c -nt target/spatial-deck-shell ]]; then
	./shell-rust/build.sh
fi

export RUST_LOG="${RUST_LOG:-info}"

COMP_LOG="${TMPDIR:-/tmp}/spatial-deck.log"
SHELL_LOG="${TMPDIR:-/tmp}/spatial-deck-shell.log"
SERV_LOG="${TMPDIR:-/tmp}/spatial-deck-services.log"
SOCK="/tmp/spatial-deck.sock"
RUNTIME="${XDG_RUNTIME_DIR:?XDG_RUNTIME_DIR ausente}"
# Só força um tema quando SPATIAL_DECK_THEME é passado explicitamente; sem
# isso vale o tema persistido em settings.json (precedência da URL).
if [ -n "${SPATIAL_DECK_THEME:-}" ]; then
	URL="${SPATIAL_DECK_URL:-http://127.0.0.1:7338/?theme=${SPATIAL_DECK_THEME}}"
else
	URL="${SPATIAL_DECK_URL:-http://127.0.0.1:7338/}"
fi
COMP_PID=""
SHELL_PID=""
SERV_PID=""
GUARD_PID=""

cleanup() {
	[ -n "$GUARD_PID" ] && kill "$GUARD_PID" 2>/dev/null || true
	[ -n "$SERV_PID" ] && kill "$SERV_PID" 2>/dev/null || true
	[ -n "$SHELL_PID" ] && kill "$SHELL_PID" 2>/dev/null || true
	[ -n "$COMP_PID" ] && kill "$COMP_PID" 2>/dev/null || true
	wait 2>/dev/null || true
	rm -f "$SOCK" "$RUNTIME/spatial-deck" "$RUNTIME/spatial-deck.lock"
}
trap cleanup EXIT INT TERM

rm -f "$SOCK" "$RUNTIME/spatial-deck" "$RUNTIME/spatial-deck.lock"

echo "[run-rust] compositor (log: $COMP_LOG)"
./target/debug/spatial-deck-compositor >"$COMP_LOG" 2>&1 &
COMP_PID=$!

# Espera o socket Wayland e o servidor HTTP do frontend.
ready=0
for _ in $(seq 1 150); do
	if [ -S "$RUNTIME/spatial-deck" ] && [ -S "$SOCK" ]; then
		if (exec 3<>/dev/tcp/127.0.0.1/7338) 2>/dev/null; then
			exec 3>&-
			ready=1
			break
		fi
	fi
	kill -0 "$COMP_PID" 2>/dev/null || break
	sleep 0.1
done
if [ "$ready" != 1 ]; then
	echo "[run-rust] compositor não subiu; veja $COMP_LOG" >&2
	tail -n 20 "$COMP_LOG" >&2 || true
	exit 1
fi

echo "[run-rust] shell (log: $SHELL_LOG)"
WAYLAND_DISPLAY=spatial-deck \
GDK_BACKEND=wayland \
GTK_USE_PORTAL=0 \
	./target/spatial-deck-shell "$URL" >"$SHELL_LOG" 2>&1 &
SHELL_PID=$!

# Serviços D-Bus do shell (notificações). Não entra no vigia: se ele cair, o
# desktop continua funcionando sem notificações.
echo "[run-rust] serviços D-Bus (log: $SERV_LOG)"
./target/debug/spatial-deck-services >"$SERV_LOG" 2>&1 &
SERV_PID=$!

echo "[run-rust] Spatial Deck rodando — WAYLAND_DISPLAY=spatial-deck, UI=$URL"
echo "[run-rust] Ctrl-C encerra (compositor + shell + serviços)"

# Traz a janela do compositor para a frente. Sem isso o gerenciador de
# janelas pode abri-la atrás do terminal, e os cliques do mouse vão para a
# janela de cima em vez do desktop.
if [ -n "${DISPLAY:-}" ] && command -v wmctrl >/dev/null 2>&1; then
	for _ in $(seq 1 60); do
		if wmctrl -l 2>/dev/null | grep -q 'Spatial Deck$'; then
			wmctrl -a 'Spatial Deck' 2>/dev/null || true
			# `above` garante que a janela fique realmente no topo da pilha
			# (o KWin pode ignorar só o pedido de ativação e manter o
			# terminal por cima, fazendo os cliques irem para ele).
			wmctrl -r 'Spatial Deck' -b add,above 2>/dev/null || true
			break
		fi
		sleep 0.1
	done

	# Guardião: reativa o deck quando o ponteiro está sobre ele e o KWin
	# entregou o foco a outra janela. Sem isso os cliques passam a ir para a
	# janela atrás (o deck continua desenhado por cima, mas "não responde").
	if command -v python3 >/dev/null 2>&1 && [[ -f ./deck-guard.py ]]; then
		env -u LD_LIBRARY_PATH python3 ./deck-guard.py 'Spatial Deck' >/dev/null 2>&1 &
		GUARD_PID=$!
	fi
fi

# Se um dos dois cair, encerra o outro.
while kill -0 "$COMP_PID" 2>/dev/null && kill -0 "$SHELL_PID" 2>/dev/null; do
	if [ -n "$SERV_PID" ] && ! kill -0 "$SERV_PID" 2>/dev/null; then
		echo "[run-rust] serviços D-Bus saíram (log: $SERV_LOG)" >&2
		SERV_PID=""
	fi
	sleep 0.5
done

kill -0 "$COMP_PID" 2>/dev/null || echo "[run-rust] compositor saiu (log: $COMP_LOG)" >&2
kill -0 "$SHELL_PID" 2>/dev/null || echo "[run-rust] shell saiu (log: $SHELL_LOG)" >&2
