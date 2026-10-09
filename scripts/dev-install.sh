#!/usr/bin/env bash
# Compila e instala el .desktop y el icono apuntando al binario de desarrollo.
# El portal de atajos y las notificaciones identifican la app por el .desktop.
set -euo pipefail
cd "$(dirname "$0")/.."

app_id=io.github.valenciaj.PersonalAssistant
cargo build
bin="$PWD/target/debug/gnome-personal-assistant"
data="${XDG_DATA_HOME:-$HOME/.local/share}"
mkdir -p "$data/applications" "$data/icons/hicolor/scalable/apps"
sed "s|^Exec=.*|Exec=$bin|" "data/$app_id.desktop" > "$data/applications/$app_id.desktop"
cp "data/icons/$app_id.svg" "$data/icons/hicolor/scalable/apps/"
update-desktop-database "$data/applications" 2>/dev/null || true
gtk-update-icon-cache -q -t "$data/icons/hicolor" 2>/dev/null || true
desktop-file-validate "$data/applications/$app_id.desktop"
echo "Instalado: $data/applications/$app_id.desktop → $bin"
