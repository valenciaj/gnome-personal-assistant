#!/usr/bin/env bash
# Instala la versión release para el usuario actual (sin sudo):
#   ~/.local/bin/gnome-personal-assistant, el .desktop y el icono.
# Con --uninstall lo quita (los datos y la configuración se conservan).
set -euo pipefail
cd "$(dirname "$0")/.."

app_id=io.github.valenciaj.PersonalAssistant
bin_dir="$HOME/.local/bin"
data="${XDG_DATA_HOME:-$HOME/.local/share}"
bin="$bin_dir/gnome-personal-assistant"

if [[ "${1:-}" == "--uninstall" ]]; then
    "$bin" quit 2>/dev/null || true
    rm -f "$bin" "$data/applications/$app_id.desktop" "$data/icons/hicolor/scalable/apps/$app_id.svg" \
        "${XDG_CONFIG_HOME:-$HOME/.config}/autostart/$app_id.desktop"
    update-desktop-database "$data/applications" 2>/dev/null || true
    echo "Desinstalado (los datos siguen en $data/personal-assistant)"
    exit 0
fi

cargo build --release
# Cerrar la instancia en marcha antes de sustituir el binario.
"$bin" quit 2>/dev/null || target/debug/gnome-personal-assistant quit 2>/dev/null || true
install -Dm755 target/release/gnome-personal-assistant "$bin"
mkdir -p "$data/applications" "$data/icons/hicolor/scalable/apps"
sed "s|^Exec=.*|Exec=$bin|" "data/$app_id.desktop" > "$data/applications/$app_id.desktop"
install -m644 "data/icons/$app_id.svg" "$data/icons/hicolor/scalable/apps/"
update-desktop-database "$data/applications" 2>/dev/null || true
gtk-update-icon-cache -q -t "$data/icons/hicolor" 2>/dev/null || true
# Si el autoarranque ya estaba activado, que apunte al binario instalado.
autostart="${XDG_CONFIG_HOME:-$HOME/.config}/autostart/$app_id.desktop"
[[ -f "$autostart" ]] && sed -i "s|^Exec=.*|Exec=$bin|" "$autostart"
echo "Instalado en $bin"
