# Asistente personal para GNOME

Asistente por voz en Rust (GTK4 + libadwaita). Ver [SPEC.md](SPEC.md).

## Desarrollo

```sh
# Dependencias del sistema (Ubuntu)
sudo apt install build-essential pkg-config libgtk-4-dev libadwaita-1-dev \
  libgtksourceview-5-dev libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
  gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-pipewire \
  gettext desktop-file-utils

./scripts/dev-install.sh             # compila e instala el .desktop
./target/debug/gnome-personal-assistant
```

La primera vez GNOME pide confirmar los atajos globales
(por defecto **Super+Alt+Espacio** para grabar y **Super+Alt+R** para reabrir).

## Configuración (provisional, hasta la fase 4)

`~/.config/personal-assistant/config.toml` se crea al primer arranque y se relee
en cada interacción. Las API keys pueden ir en el archivo o en las variables
`ANTHROPIC_API_KEY`, `OPENAI_API_KEY` y `OPENAI_COMPATIBLE_API_KEY`.

```sh
gnome-personal-assistant models   # modelos disponibles del proveedor activo
gnome-personal-assistant toggle   # grabar / parar (útil en atajos personalizados)
gnome-personal-assistant history  # conversaciones guardadas
gnome-personal-assistant open 12  # reabrir y continuar la conversación 12
gnome-personal-assistant quit
```

## Datos

`~/.local/share/personal-assistant/`: base de datos `assistant.db` (SQLite) y
grabaciones en `audio/`. Qué audios se conservan lo decide `[audio]` en la
configuración: `retention = "with_notes"` (solo los adjuntos a notas) o `"always"`,
y `delete_after_days` para el historial (los de notas no se borran nunca).
