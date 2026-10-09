# Personal Assistant — Especificación

App ID: `io.github.valenciaj.PersonalAssistant` · Binario: `gnome-personal-assistant`
Plataforma objetivo: Ubuntu 26.04, GNOME 50, Wayland. Distribución: `.deb`.

## 1. Arquitectura

Un único proceso Rust (sin extensión JS de GNOME Shell):

| Pieza | Tecnología |
|---|---|
| UI | GTK4 + libadwaita (`gtk4`, `libadwaita` crates) |
| Icono de bandeja | StatusNotifierItem (`ksni`) — visible gracias a `ubuntu-appindicators` |
| Atajos globales | Portal XDG GlobalShortcuts (`ashpd`) — soporta Activated/Deactivated (mantener pulsado) |
| Audio | GStreamer: `pipewiresrc → audioconvert → level → opusenc → oggmux` (Ogg/Opus) |
| Reproducción | `gtk::MediaFile` (GStreamer) |
| HTTP | `reqwest` + rustls, `tokio` en hilo aparte, puente con el main loop de glib |
| Datos | SQLite (`rusqlite`, bundled, FTS5) en `~/.local/share/personal-assistant/` |
| Audios | `~/.local/share/personal-assistant/audio/*.ogg` |
| Credenciales | Llavero GNOME / Secret Service (`oo7`) |
| Ajustes | `~/.config/personal-assistant/config.toml` (editable desde Preferencias; se descartó GSettings para no depender de esquemas compilados) |
| Markdown | `pulldown-cmark` → `GtkTextView` con tags; edición con GtkSourceView 5 |
| i18n | gettext (`gettext-rs`), es + en |
| Notificaciones | `GNotification` |
| Autoarranque | `.desktop` en `~/.config/autostart` (opción en ajustes), proceso residente |

Instancia única (`GApplication`): `gnome-personal-assistant toggle|show|reopen` reenvía acciones a la instancia en marcha.

## 2. Flujo principal

1. Disparador: atajo global (por defecto **Super+Alt+Espacio**), clic en el menú de bandeja, o texto escrito en el popup.
2. Modo de grabación (configurable): **toggle** (pulsar/pulsar), **push-to-talk** (mantener), **auto-stop por silencio** (VAD por nivel RMS + duración de silencio configurables).
3. Feedback durante la grabación: icono de bandeja cambia (reposo / grabando / pensando / error), pitido al empezar/parar, ventanita flotante con tiempo + nivel del micro (botones Parar/Cancelar).
4. Transcripción vía API cloud → texto.
5. LLM con tools → respuesta (bucle agente hasta que no haya más tool calls).
6. Resultado: popup (transcripción, razonamiento plegable, respuesta en Markdown, tool calls realizadas) + notificación GNOME. El popup **se queda abierto** (Esc cierra) y permite escribir o volver a grabar para continuar esa conversación.
7. Cada atajo inicia **siempre una conversación nueva**. Segundo atajo global: **reabrir el último popup**.
8. Todo se guarda en el historial de conversaciones.

Nota Wayland: las ventanas no se pueden posicionar ni forzar "siempre encima"; Mutter decide su ubicación.

## 3. Proveedores

### Transcripción (STT)
- **OpenAI** (`/v1/audio/transcriptions`): modelos `gpt-4o-transcribe`, `gpt-4o-mini-transcribe`, `whisper-1`…
- **Compatible OpenAI** (base_url + key opcional + modelo).
- Idioma: **autodetectar** por defecto; configurable a un código ISO.

### LLM
- **Anthropic** (Messages API nativa, tool use, thinking).
- **OpenAI** (function calling, `reasoning_effort`).
- **Ollama** (`/api/chat` con tools, `think`).
- **Genérico compatible OpenAI** (base_url, key, modelo; `reasoning_effort` opcional).

Por proveedor se configura:
- **Modelo**: desplegable rellenado desde la API (`/v1/models`, `/api/tags`) con botón refrescar + entrada manual.
- **Razonamiento**: selector adaptado al proveedor (Anthropic: desactivado / effort bajo-medio-alto-máx; OpenAI y genérico: `reasoning_effort` none/minimal/low/medium/high; Ollama: on/off). Se muestra el razonamiento recibido en una sección plegable del popup.
- Prompt de sistema editable (con fecha/hora actual y zona horaria inyectadas siempre).

API keys en el llavero de GNOME, nunca en GSettings.

## 4. Modelo de datos

- **Cuaderno** (notebook): id, nombre, creado.
- **Nota**: id, cuaderno_id, título (generado por la IA), contenido Markdown, etiquetas (n:m), transcripción original, audio (ruta, opcional), conversación origen, creado, modificado. Búsqueda FTS5 sobre título+contenido+transcripción.
- **Lista TODO**: id, nombre, creado.
- **TODO**: id, lista_id, texto, hecho, fecha límite (fecha u fecha+hora), prioridad (alta/media/baja/ninguna), recordatorio (timestamp, notificado), creado, completado.
- **Conversación**: id, título, creado; **Mensaje**: rol, contenido, razonamiento, tool calls/resultados, audio (opcional), transcripción, proveedor/modelo, timestamp.

Retención de audio (configurable): guardar solo si se crea una nota / guardar siempre en el historial; borrado automático tras N días (0 = nunca) de audios no enlazados a notas.

## 5. Tools de la IA

Crear: `create_note`, `create_notebook`, `create_todo_list`, `add_todo`.
Consultar: `search_notes`, `get_note`, `list_notebooks`, `list_todo_lists`, `list_todos` (filtros: lista, pendientes, rango de fechas, prioridad), `list_tags`.
Editar: `update_note` (título, contenido, añadir texto, mover de cuaderno, etiquetas), `update_todo` (texto, fecha, prioridad, recordatorio), `complete_todo`, `rename_list`.
Borrar: `delete_note`, `delete_todo`, `delete_todo_list`, `delete_notebook` — **requieren confirmación** en el popup (botones Confirmar/Cancelar) antes de ejecutarse.

`create_note` puede adjuntar el audio y la transcripción de la grabación actual.

## 6. Recordatorios

Proceso residente con temporizador: al llegar `recordatorio` de un TODO se lanza notificación con acciones "Hecho" / "Posponer 10 min". TODOs con solo fecha → recordatorio a la hora por defecto configurable (p.ej. 09:00).

## 7. Interfaz

**Ventana principal** (`AdwNavigationSplitView`), barra lateral:
- Conversaciones (historial, reabrir, continuar, borrar)
- Notas (filtros por cuaderno y etiqueta, búsqueda, vista Markdown renderizada / edición, reproducir audio, transcripción)
- TODOs (listas, añadir/editar/marcar/borrar, fecha, prioridad, recordatorio)
- Edición completa manual de todo.

**Preferencias** (`AdwPreferencesDialog`): proveedores STT/LLM (key, base_url, modelo, razonamiento, probar conexión), prompt de sistema, modo de grabación + parámetros VAD, dispositivo de micrófono, sonidos, idioma STT, retención de audio, hora por defecto de recordatorios, autoarranque, atajos (abre el diálogo del portal).

**Popup** de conversación rápida y **ventanita de grabación** (ver §2).

**Menú de bandeja**: Grabar/Parar, Escribir…, Última conversación, Abrir asistente, Preferencias, Salir.

## 8. Fases

1. **MVP**: app GTK residente + bandeja + atajo (portal) + grabación GStreamer + STT OpenAI + LLM (los 4 proveedores, modelo y razonamiento desde un config mínimo) + popup + notificación.
2. **Datos y tools**: SQLite, migraciones, todas las tools, confirmación de borrado, historial de conversaciones, retención de audio.
3. **Explorador**: ventana principal con Conversaciones, Notas, TODOs; edición completa, Markdown, reproducción.
4. **Preferencias**: ventana de preferencias sobre config.toml, llavero, listado de modelos, modos de grabación, VAD, micrófono, ventanita, sonidos.
5. **Remate**: recordatorios, i18n (es/en), autoarranque, `.deb` (`cargo-deb`).

Git local con un commit por fase.

## 9. Ampliación: recordatorios y adjuntos (2026-10-09)

- **Adjuntos múltiples** (tabla `attachments`): audios e imágenes en notas, recordatorios y
  mensajes. Un archivo puede estar en varias filas (el audio de un mensaje copiado a una nota).
- La IA elige qué adjuntos copiar al crear/editar notas y recordatorios: `attach` =
  `current` | `conversation` | `selected` (+ `attachment_ids`); `list_conversation_attachments`
  muestra los de la conversación. Cada mensaje enviado al modelo indica los ids de sus adjuntos.
- **Recordatorios**: título, texto Markdown, fecha y hora, repetición (diaria, semanal, mensual,
  anual; calculada desde la fecha original), audios e imágenes. Al llegar la hora: notificación
  (Hecho / primera opción de posponer), ventana con el contenido y alarma que se repite hasta
  2 minutos. Opciones de posponer configurables. Los atendidos pasan a «Completados».
- **Imágenes**: pegar, archivo, arrastrar, captura de pantalla (portal), cámara (v4l2). La IA las ve
  (ajuste «El modelo admite imágenes» por proveedor); se envían reducidas a 1568 px en JPEG.
- Atajo **Super+Alt+S**: captura de pantalla y empieza a grabar.
- Retención: los audios del historial se conservan 24 h con «solo notas» (para poder convertir
  la conversación en nota) o N días con «siempre»; los de notas y recordatorios, siempre.
