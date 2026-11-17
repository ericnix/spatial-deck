#!/usr/bin/env bash
# Compila o frontend Rust para WebAssembly e gera web/pkg/.
#
# Preferência: wasm-pack (o comando canônico do projeto).
# Fallback: cargo + wasm-bindgen-cli, útil quando wasm-pack não está instalado.
set -euo pipefail

cd "$(dirname "$0")"

if command -v wasm-pack >/dev/null 2>&1; then
    exec wasm-pack build frontend --target web --out-dir ../web/pkg
fi

echo "wasm-pack não encontrado; usando cargo + wasm-bindgen-cli" >&2
cargo build -p frontend --target wasm32-unknown-unknown --release

if ! command -v wasm-bindgen >/dev/null 2>&1; then
    cat >&2 <<'EOF'
wasm-bindgen-cli não encontrado.

Instale uma das duas opções e rode de novo:
  cargo install wasm-pack
  cargo install wasm-bindgen-cli --version 0.2.129

O fallback exige a mesma versão do crate wasm-bindgen usada em frontend/Cargo.toml.
EOF
    exit 1
fi

mkdir -p web/pkg
wasm-bindgen \
    target/wasm32-unknown-unknown/release/spatial_deck_frontend.wasm \
    --target web \
    --out-dir web/pkg
