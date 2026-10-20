#!/usr/bin/env bash
# Bootstrap de dependências de desenvolvimento do Padrão Desktop.
# Baixa RPMs como usuário comum (dnf5 download, sem sudo) e extrai para um
# sysroot fora da árvore do projeto (sem espaços no caminho, exigido pelo
# pkg-config com PKG_CONFIG_SYSROOT_DIR).
set -euo pipefail

DEPS="${PADRAO_DEPS:-$HOME/.local/share/padrao}"
RPMS="$DEPS/rpms"
SYSROOT="$DEPS/sysroot"

PKGS=(
  # núcleo do compositor (wlroots + Wayland + input)
  wlroots-devel
  wayland-devel
  wayland-protocols-devel
  libxkbcommon-devel
  pixman-devel
  libinput-devel
  systemd-devel
  libseat-devel
  libdisplay-info-devel
  libdrm-devel
  mesa-libgbm-devel
  mesa-libEGL-devel
  mesa-libGLES2-devel
  mesa-libGL-devel
  # shell-host (GTK3 + layer-shell + WebKit)
  gtk3-devel
  webkit2gtk4.1-devel
  gtk-layer-shell-devel
)

mkdir -p "$RPMS"
if ! ls "$RPMS"/*.rpm >/dev/null 2>&1; then
  echo "==> Baixando pacotes de desenvolvimento (x86_64, todas as dependências)..."
  dnf5 download --resolve --alldeps --arch=x86_64 --skip-unavailable \
    --destdir="$RPMS" "${PKGS[@]}"
else
  echo "==> RPMs já presentes em $RPMS — pulando download"
fi

echo "==> Extraindo para o sysroot $SYSROOT"
rm -rf "$SYSROOT"
mkdir -p "$SYSROOT"
# Alguns RPMs criam diretórios em modo 0555; extração é feita em passes
# tolerantes com reabertura de permissões de escrita entre elas.
pass() {
  for rpm in "$RPMS"/*.rpm; do
    ( cd "$SYSROOT" && rpm2cpio "$rpm" | cpio -idmu --quiet ) 2>/dev/null || true
  done
  chmod -R u+w "$SYSROOT" 2>/dev/null || true
}
pass
pass

echo "==> Sysroot pronto em $SYSROOT"
