# Asistente personal para GNOME

Asistente por voz en Rust (GTK4 + libadwaita). Ver [SPEC.md](SPEC.md).

## Instalación

```sh
cargo install cargo-deb   # una vez
cargo deb                 # genera target/debian/gnome-personal-assistant_*.deb
sudo apt install ./target/debian/gnome-personal-assistant_*.deb
```

Si antes usaste `scripts/dev-install.sh`, borra
`~/.local/share/applications/io.github.valenciaj.PersonalAssistant.desktop`: tiene
prioridad sobre el del paquete y apunta al binario de desarrollo.

En el primer arranque se activa el inicio automático con la sesión (se puede
desactivar en Preferencias). El icono de bandeja necesita la extensión
AppIndicator, que Ubuntu trae activada.

## Desarrollo

```sh
# Dependencias del sistema (Ubuntu)
sudo apt install build-essential pkg-config libgtk-4-dev libadwaita-1-dev \
  libgtksourceview-5-dev libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
  gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-pipewire \
  gettext desktop-file-utils

./scripts/dev-install.sh             # compila e instala el .desktop y el icono
./target/debug/gnome-personal-assistant
```

La primera vez GNOME pide confirmar los atajos globales
(por defecto **Super+Alt+Espacio** para grabar y **Super+Alt+R** para reabrir).

## Configuración

Desde **Preferencias** (menú del icono o botón del explorador). Se guarda en
`~/.config/personal-assistant/config.toml`, que también se puede editar a mano y se
relee en cada interacción. Las API keys se guardan en el llavero de GNOME; si se
escriben en config.toml, al arrancar se mueven al llavero. También se aceptan las
variables `ANTHROPIC_API_KEY`, `OPENAI_API_KEY` y `OPENAI_COMPATIBLE_API_KEY`.

```sh
gnome-personal-assistant models   # modelos disponibles del proveedor activo
gnome-personal-assistant toggle   # grabar / parar (útil en atajos personalizados)
gnome-personal-assistant history  # conversaciones guardadas
gnome-personal-assistant open 12  # reabrir y continuar la conversación 12
gnome-personal-assistant explorer # ventana de notas, tareas e historial
gnome-personal-assistant quit
```

## Datos

`~/.local/share/personal-assistant/`: base de datos `assistant.db` (SQLite) y
grabaciones en `audio/`. Qué audios se conservan lo decide `[audio]` en la
configuración: `retention = "with_notes"` (solo los adjuntos a notas) o `"always"`,
y `delete_after_days` para el historial (los de notas no se borran nunca).

## Modo desarrollo

```sh
ASSISTANT_DEV=1 cargo run                    # instancia aparte, sin bandeja ni atajos
ASSISTANT_SNAPSHOT=/tmp/shots cargo run      # capturas PNG de todas las ventanas

# Capturas sin abrir ventanas en el escritorio:
gtk4-broadwayd :7 &
GDK_BACKEND=broadway BROADWAY_DISPLAY=:7 ASSISTANT_SNAPSHOT=/tmp/shots cargo run
```

Combínalos con `XDG_DATA_HOME`/`XDG_CONFIG_HOME` para usar datos de prueba.

## Traducciones

Las cadenas del código están en español y se marcan con `tr("…")`, `trf("…", &[…])`
o `n_("…")`. `po/en.po` las traduce al inglés, que se usa con cualquier idioma del
sistema distinto del español (o se fuerza en Preferencias). Tras cambiar textos:

```sh
./scripts/update-po.sh   # extrae a po/messages.pot y actualiza po/en.po
```

El catálogo se compila en `build.rs` y va incrustado en el binario.
