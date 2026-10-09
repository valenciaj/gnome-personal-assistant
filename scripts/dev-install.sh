#!/usr/bin/env bash
# Compila e instala el .desktop apuntando al binario de desarrollo.
# El portal de atajos y las notificaciones identifican la app por este archivo.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build
bin="$PWD/target/debug/gnome-personal-assistant"
apps="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
mkdir -p "$apps"
sed "s|@BIN@|$bin|" data/io.github.valenciaj.PersonalAssistant.desktop.in \
    > "$apps/io.github.valenciaj.PersonalAssistant.desktop"
update-desktop-database "$apps" 2>/dev/null || true
desktop-file-validate "$apps/io.github.valenciaj.PersonalAssistant.desktop"
echo "Instalado: $apps/io.github.valenciaj.PersonalAssistant.desktop → $bin"
