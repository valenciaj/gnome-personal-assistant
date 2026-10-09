//! Tools que la IA puede usar sobre notas, tareas y recordatorios.
//!
//! Los argumentos de cada tool son structs con `JsonSchema`: el esquema que ve el
//! modelo y la validación al recibir la llamada salen de la misma definición.

use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use schemars::JsonSchema;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::app::Command;
use crate::llm::{ToolCall, ToolExecutor, ToolOutput, ToolSpec};
use crate::store::{
    Attachment, NewNote, NewReminder, NewTodo, NoteFilter, NoteUpdate, Owner, Priority, Reminder, ReminderUpdate,
    Repeat, Store, Todo, TodoFilter, TodoStatus, TodoUpdate,
};
use crate::i18n::{tr, trf};

/// Se añade al prompt de sistema de cada conversación.
pub const GUIDANCE: &str = "\
Tienes tools para gestionar las notas del usuario (con cuadernos, etiquetas, audios e \
imágenes), sus listas de tareas y sus recordatorios (avisos a una hora concreta, que \
pueden repetirse y llevar audio e imágenes). Úsalas cuando pida apuntar, recordar, \
consultar, cambiar o borrar algo. Usa un recordatorio cuando quiera que le avises a una \
hora; una tarea, cuando sea algo pendiente de hacer.\n\n\
Los mensajes del usuario pueden llevar adjuntos (grabaciones de voz e imágenes); al \
final de cada mensaje se indican sus ids. Al crear o editar una nota o un recordatorio \
puedes adjuntar con `attach`: \"current\" (los del mensaje actual), \"conversation\" \
(todos los de la conversación) o \"selected\" con `attachment_ids`. Si te pide convertir \
la conversación en nota, adjunta los de los mensajes con contenido, no el del mensaje que \
solo da esa orden; si pide unos concretos o todos, haz eso. Usa \
list_conversation_attachments para ver las transcripciones si dudas.\n\n\
Convierte las fechas relativas («mañana», «el viernes») a fechas absolutas a partir de \
la fecha actual. Para editar o borrar, busca primero el elemento y usa su id; nunca \
inventes ids. Los borrados piden confirmación al usuario automáticamente: no se la \
pidas tú antes. Tras usar tools, resume en una frase lo que has hecho.";

/// Datos del mensaje que está procesando el agente.
#[derive(Debug, Default, Clone)]
pub struct TurnContext {
    pub conversation_id: Option<i64>,
    /// Adjuntos del mensaje actual del usuario.
    pub attachment_ids: Vec<i64>,
}

pub struct Toolbox {
    store: Store,
    commands: async_channel::Sender<Command>,
    context: TurnContext,
}

impl Toolbox {
    pub fn new(store: Store, commands: async_channel::Sender<Command>, context: TurnContext) -> Self {
        Self { store, commands, context }
    }

    /// Ids de los adjuntos que hay que copiar según `attach` y `attachment_ids`.
    fn chosen_attachments(&self, attach: Option<&str>, ids: Option<Vec<i64>>) -> Result<Vec<i64>> {
        Ok(match attach.unwrap_or(if ids.is_some() { "selected" } else { "none" }) {
            "none" => Vec::new(),
            "current" => self.context.attachment_ids.clone(),
            "conversation" => match self.context.conversation_id {
                Some(id) => self.store.conversation_attachments(id)?.into_iter().map(|a| a.attachment.id).collect(),
                None => Vec::new(),
            },
            "selected" => ids.unwrap_or_default(),
            other => bail!(trf("Valor de attach desconocido: {value} (usa none, current, conversation o selected)", &[("value", &other)])),
        })
    }

    /// Copia los adjuntos elegidos y devuelve un sufijo para el resumen.
    fn attach(&self, owner: Owner, attach: Option<&str>, ids: Option<Vec<i64>>) -> Result<String> {
        let chosen = self.chosen_attachments(attach, ids)?;
        if chosen.is_empty() {
            return Ok(String::new());
        }
        let copied = self.store.copy_attachments(&chosen, owner)?;
        Ok(trf(" ({count} adjuntos)", &[("count", &copied)]))
    }

    /// Pide confirmación en el popup. Si nadie responde en 5 minutos, se cancela.
    async fn confirm(&self, question: String) -> bool {
        let (reply, answer) = async_channel::bounded(1);
        if self.commands.send(Command::ConfirmDeletion { question, reply }).await.is_err() {
            return false;
        }
        matches!(tokio::time::timeout(Duration::from_secs(300), answer.recv()).await, Ok(Ok(true)))
    }

    async fn delete_with_confirmation(
        &self,
        question: String,
        done: String,
        delete: impl FnOnce() -> Result<()>,
    ) -> Result<ToolOutput> {
        if !self.confirm(question).await {
            return Ok(ToolOutput::new(
                json!({ "deleted": false, "reason": "El usuario ha cancelado el borrado" }),
                tr("Borrado cancelado"),
            ));
        }
        delete()?;
        Ok(ToolOutput::new(json!({ "deleted": true }), done))
    }

    async fn run(&self, name: &str, input: Value) -> Result<ToolOutput> {
        let store = &self.store;
        match name {
            "create_note" => {
                let a: CreateNote = args(input)?;
                let id = store.create_note(NewNote {
                    title: a.title.clone(),
                    content: a.content,
                    notebook: a.notebook,
                    tags: a.tags.unwrap_or_default(),
                    conversation_id: self.context.conversation_id,
                })?;
                let suffix = self.attach(Owner::Note(id), a.attach.as_deref(), a.attachment_ids)?;
                Ok(ToolOutput::new(
                    json!({ "id": id, "attachments": attachments_json(&store.attachments(Owner::Note(id))?) }),
                    trf("Nota creada: «{title}»{attachments}", &[("title", &a.title), ("attachments", &suffix)]),
                ))
            }
            "list_conversation_attachments" => {
                let Some(conversation) = self.context.conversation_id else {
                    return Ok(ToolOutput::new(json!({ "attachments": [] }), tr("Adjuntos consultados")));
                };
                let items: Vec<Value> = store
                    .conversation_attachments(conversation)?
                    .into_iter()
                    .map(|c| {
                        json!({
                            "id": c.attachment.id,
                            "kind": c.attachment.kind,
                            "transcript": c.attachment.transcript,
                            "message_id": c.message_id,
                            "message": c.message.chars().take(200).collect::<String>(),
                            "in_current_message": self.context.attachment_ids.contains(&c.attachment.id),
                            "time": local_time(c.attachment.created),
                        })
                    })
                    .collect();
                Ok(ToolOutput::new(json!({ "attachments": items }), tr("Adjuntos consultados")))
            }
            "get_note" => {
                let a: Id = args(input)?;
                let note = store.get_note(a.id)?;
                Ok(ToolOutput::new(
                    json!({
                        "id": note.id,
                        "title": note.title,
                        "content": note.content,
                        "notebook": note.notebook,
                        "tags": note.tags,
                        "attachments": attachments_json(&note.attachments),
                        "created": local_time(note.created),
                        "modified": local_time(note.modified),
                    }),
                    trf("Nota leída: «{title}»", &[("title", &note.title)]),
                ))
            }
            "search_notes" => {
                let a: SearchNotes = args(input)?;
                let notes = store.search_notes(&NoteFilter {
                    query: a.query,
                    notebook: a.notebook,
                    tag: a.tag,
                    limit: a.limit,
                })?;
                let count = notes.len();
                let notes: Vec<Value> = notes
                    .into_iter()
                    .map(|n| {
                        json!({
                            "id": n.id,
                            "title": n.title,
                            "notebook": n.notebook,
                            "tags": n.tags,
                            "snippet": n.snippet,
                            "modified": local_time(n.modified),
                        })
                    })
                    .collect();
                Ok(ToolOutput::new(json!({ "notes": notes }), trf("Búsqueda de notas: {count} resultados", &[("count", &count)])))
            }
            "update_note" => {
                let a: UpdateNote = args(input)?;
                store.update_note(
                    a.id,
                    NoteUpdate { title: a.title, content: a.content, append: a.append, notebook: a.notebook, tags: a.tags },
                )?;
                let suffix = self.attach(Owner::Note(a.id), a.attach.as_deref(), a.attachment_ids)?;
                let note = store.get_note(a.id)?;
                Ok(ToolOutput::new(
                    json!({ "updated": true, "attachments": attachments_json(&note.attachments) }),
                    trf("Nota actualizada: «{title}»{attachments}", &[("title", &note.title), ("attachments", &suffix)]),
                ))
            }
            "delete_note" => {
                let a: Id = args(input)?;
                let note = store.get_note(a.id)?;
                let store = store.clone();
                self.delete_with_confirmation(
                    trf("¿Borrar la nota «{title}»?", &[("title", &note.title)]),
                    trf("Nota borrada: «{title}»", &[("title", &note.title)]),
                    move || store.delete_note(a.id),
                )
                .await
            }
            "list_notebooks" => {
                let notebooks = store.list_notebooks()?;
                Ok(ToolOutput::new(json!({ "notebooks": notebooks }), tr("Cuadernos consultados")))
            }
            "create_notebook" => {
                let a: Name = args(input)?;
                let id = store.create_notebook(&a.name)?;
                Ok(ToolOutput::new(json!({ "id": id }), trf("Cuaderno creado: «{name}»", &[("name", &a.name.trim())])))
            }
            "rename_notebook" => {
                let a: Rename = args(input)?;
                store.rename_notebook(a.id, &a.name)?;
                Ok(ToolOutput::new(json!({ "renamed": true }), trf("Cuaderno renombrado: «{name}»", &[("name", &a.name.trim())])))
            }
            "delete_notebook" => {
                let a: Id = args(input)?;
                let name = store.notebook_name(a.id)?;
                let store = store.clone();
                self.delete_with_confirmation(
                    trf("¿Borrar el cuaderno «{name}»? Sus notas se conservan, sin cuaderno.", &[("name", &name)]),
                    trf("Cuaderno borrado: «{name}»", &[("name", &name)]),
                    move || store.delete_notebook(a.id),
                )
                .await
            }
            "list_tags" => {
                let tags: Vec<Value> = store
                    .list_tags()?
                    .into_iter()
                    .map(|(name, notes)| json!({ "name": name, "notes": notes }))
                    .collect();
                Ok(ToolOutput::new(json!({ "tags": tags }), tr("Etiquetas consultadas")))
            }
            "list_todo_lists" => {
                let lists = store.list_todo_lists()?;
                Ok(ToolOutput::new(json!({ "lists": lists }), tr("Listas consultadas")))
            }
            "create_todo_list" => {
                let a: Name = args(input)?;
                let id = store.create_todo_list(&a.name)?;
                Ok(ToolOutput::new(json!({ "id": id }), trf("Lista creada: «{name}»", &[("name", &a.name.trim())])))
            }
            "rename_todo_list" => {
                let a: Rename = args(input)?;
                store.rename_todo_list(a.id, &a.name)?;
                Ok(ToolOutput::new(json!({ "renamed": true }), trf("Lista renombrada: «{name}»", &[("name", &a.name.trim())])))
            }
            "delete_todo_list" => {
                let a: Id = args(input)?;
                let list = store.todo_list(a.id)?;
                let store = store.clone();
                self.delete_with_confirmation(
                    trf("¿Borrar la lista «{name}» y sus {count} tareas?", &[("name", &list.name), ("count", &list.total)]),
                    trf("Lista borrada: «{name}»", &[("name", &list.name)]),
                    move || store.delete_todo_list(a.id),
                )
                .await
            }
            "add_todo" => {
                let a: AddTodo = args(input)?;
                let id = store.add_todo(NewTodo {
                    list: a.list.clone(),
                    text: a.text.clone(),
                    due: a.due.filter(|d| !d.trim().is_empty()),
                    priority: a.priority.as_deref().map(Priority::parse).transpose()?,
                    reminder_at: a.reminder.as_deref().filter(|r| !r.trim().is_empty()).map(parse_reminder).transpose()?,
                })?;
                Ok(ToolOutput::new(json!({ "id": id }), trf("Tarea añadida a «{list}»: {text}", &[("list", &a.list.trim()), ("text", &a.text.trim())])))
            }
            "list_todos" => {
                let a: ListTodos = args(input)?;
                let status = match a.status.as_deref().unwrap_or("pending") {
                    "pending" => TodoStatus::Pending,
                    "done" => TodoStatus::Done,
                    "all" => TodoStatus::All,
                    other => bail!(trf("Estado desconocido: {value} (usa pending, done o all)", &[("value", &other)])),
                };
                let todos = store.list_todos(&TodoFilter {
                    list: a.list,
                    status,
                    due_from: a.due_from,
                    due_to: a.due_to,
                    min_priority: a.min_priority.as_deref().map(Priority::parse).transpose()?,
                })?;
                let count = todos.len();
                let todos: Vec<Value> = todos.iter().map(todo_json).collect();
                Ok(ToolOutput::new(json!({ "todos": todos }), trf("Tareas consultadas: {count}", &[("count", &count)])))
            }
            "update_todo" => {
                let a: UpdateTodo = args(input)?;
                let empty_to_none = |s: String| (!s.trim().is_empty()).then_some(s);
                store.update_todo(
                    a.id,
                    TodoUpdate {
                        text: a.text,
                        due: a.due.map(empty_to_none),
                        priority: a.priority.as_deref().map(Priority::parse).transpose()?,
                        reminder_at: a
                            .reminder
                            .map(empty_to_none)
                            .map(|r| r.as_deref().map(parse_reminder).transpose())
                            .transpose()?,
                        list: a.list,
                    },
                )?;
                let todo = store.get_todo(a.id)?;
                Ok(ToolOutput::new(json!({ "todo": todo_json(&todo) }), trf("Tarea actualizada: {text}", &[("text", &todo.text)])))
            }
            "complete_todo" => {
                let a: CompleteTodo = args(input)?;
                let done = a.done.unwrap_or(true);
                store.set_todo_done(a.id, done)?;
                let todo = store.get_todo(a.id)?;
                let summary = if done { tr("Tarea completada: {text}") } else { tr("Tarea pendiente de nuevo: {text}") };
                Ok(ToolOutput::new(json!({ "done": done }), summary.replace("{text}", &todo.text)))
            }
            "delete_todo" => {
                let a: Id = args(input)?;
                let todo = store.get_todo(a.id)?;
                let store = store.clone();
                self.delete_with_confirmation(
                    trf("¿Borrar la tarea «{text}» de «{list}»?", &[("text", &todo.text), ("list", &todo.list)]),
                    trf("Tarea borrada: {text}", &[("text", &todo.text)]),
                    move || store.delete_todo(a.id),
                )
                .await
            }
            "create_reminder" => {
                let a: CreateReminder = args(input)?;
                let id = store.create_reminder(NewReminder {
                    title: a.title.clone(),
                    body: a.text.unwrap_or_default(),
                    due_at: parse_local_datetime(&a.at)?,
                    repeat: a.repeat.as_deref().map(Repeat::parse).transpose()?.unwrap_or_default(),
                    conversation_id: self.context.conversation_id,
                })?;
                let suffix = self.attach(Owner::Reminder(id), a.attach.as_deref(), a.attachment_ids)?;
                let reminder = store.get_reminder(id)?;
                Ok(ToolOutput::new(
                    json!({ "reminder": reminder_json(&reminder) }),
                    trf(
                        "Recordatorio creado: «{title}», {date}{attachments}",
                        &[("title", &reminder.title), ("date", &local_time(reminder.due_at)), ("attachments", &suffix)],
                    ),
                ))
            }
            "list_reminders" => {
                let a: ListReminders = args(input)?;
                let mut reminders = Vec::new();
                match a.status.as_deref().unwrap_or("pending") {
                    "pending" => reminders.extend(store.list_reminders(false)?),
                    "done" => reminders.extend(store.list_reminders(true)?),
                    "all" => {
                        reminders.extend(store.list_reminders(false)?);
                        reminders.extend(store.list_reminders(true)?);
                    }
                    other => bail!(trf("Estado desconocido: {value} (usa pending, done o all)", &[("value", &other)])),
                }
                let count = reminders.len();
                let items: Vec<Value> = reminders.iter().map(reminder_json).collect();
                Ok(ToolOutput::new(json!({ "reminders": items }), trf("Recordatorios consultados: {count}", &[("count", &count)])))
            }
            "get_reminder" => {
                let a: Id = args(input)?;
                let reminder = store.get_reminder(a.id)?;
                Ok(ToolOutput::new(
                    json!({ "reminder": reminder_json(&reminder) }),
                    trf("Recordatorio leído: «{title}»", &[("title", &reminder.title)]),
                ))
            }
            "update_reminder" => {
                let a: UpdateReminder = args(input)?;
                store.update_reminder(
                    a.id,
                    ReminderUpdate {
                        title: a.title,
                        body: a.text,
                        due_at: a.at.as_deref().map(parse_local_datetime).transpose()?,
                        repeat: a.repeat.as_deref().map(Repeat::parse).transpose()?,
                    },
                )?;
                let suffix = self.attach(Owner::Reminder(a.id), a.attach.as_deref(), a.attachment_ids)?;
                let reminder = store.get_reminder(a.id)?;
                Ok(ToolOutput::new(
                    json!({ "reminder": reminder_json(&reminder) }),
                    trf("Recordatorio actualizado: «{title}»{attachments}", &[("title", &reminder.title), ("attachments", &suffix)]),
                ))
            }
            "complete_reminder" => {
                let a: Id = args(input)?;
                let reminder = store.complete_reminder(a.id)?;
                let summary = if reminder.done {
                    trf("Recordatorio completado: «{title}»", &[("title", &reminder.title)])
                } else {
                    trf("Recordatorio hecho; el siguiente es el {date}", &[("date", &local_time(reminder.due_at))])
                };
                Ok(ToolOutput::new(json!({ "reminder": reminder_json(&reminder) }), summary))
            }
            "delete_reminder" => {
                let a: Id = args(input)?;
                let reminder = store.get_reminder(a.id)?;
                let store = store.clone();
                self.delete_with_confirmation(
                    trf("¿Borrar el recordatorio «{title}»?", &[("title", &reminder.title)]),
                    trf("Recordatorio borrado: «{title}»", &[("title", &reminder.title)]),
                    move || store.delete_reminder(a.id),
                )
                .await
            }
            other => Err(anyhow!(trf("La tool {name} no existe", &[("name", &other)]))),
        }
    }
}

impl ToolExecutor for Toolbox {
    async fn execute(&self, call: &ToolCall) -> Result<ToolOutput> {
        self.run(&call.name, call.input.clone()).await
    }
}

fn args<T: DeserializeOwned>(input: Value) -> Result<T> {
    serde_json::from_value(input).with_context(|| tr("Argumentos no válidos"))
}

fn local_time(unix: i64) -> String {
    chrono::DateTime::from_timestamp(unix, 0)
        .map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

fn attachments_json(attachments: &[Attachment]) -> Vec<Value> {
    attachments
        .iter()
        .map(|a| json!({ "id": a.id, "kind": a.kind, "transcript": a.transcript }))
        .collect()
}

fn reminder_json(reminder: &Reminder) -> Value {
    json!({
        "id": reminder.id,
        "title": reminder.title,
        "text": reminder.body,
        "at": local_time(reminder.due_at),
        "repeat": reminder.repeat,
        "done": reminder.done,
        "attachments": attachments_json(&reminder.attachments),
    })
}

/// 'YYYY-MM-DD HH:MM' (o solo la fecha, a las 9:00) en hora local → unix.
pub(crate) fn parse_local_datetime(text: &str) -> Result<i64> {
    let text = text.trim();
    if text.len() == 10 {
        return parse_reminder(&format!("{text} 09:00"));
    }
    parse_reminder(text)
}

/// 'YYYY-MM-DD HH:MM' en hora local → unix.
pub(crate) fn parse_reminder(text: &str) -> Result<i64> {
    let normalized = crate::store::normalize_due(text)?;
    let naive = chrono::NaiveDateTime::parse_from_str(&normalized, "%Y-%m-%d %H:%M")
        .map_err(|_| anyhow!("El recordatorio necesita fecha y hora (YYYY-MM-DD HH:MM)"))?;
    naive
        .and_local_timezone(chrono::Local)
        .earliest()
        .map(|t| t.timestamp())
        .ok_or_else(|| anyhow!("Hora no válida en la zona horaria local: {text}"))
}

fn todo_json(todo: &Todo) -> Value {
    json!({
        "id": todo.id,
        "list": todo.list,
        "text": todo.text,
        "done": todo.done,
        "due": todo.due,
        "priority": todo.priority,
        "reminder": todo.reminder_at.map(local_time),
    })
}

// ── Argumentos ─────────────────────────────────────────────────────────────

#[derive(Deserialize, JsonSchema)]
struct NoArgs {}

#[derive(Deserialize, JsonSchema)]
struct Id {
    /// Id del elemento.
    id: i64,
}

#[derive(Deserialize, JsonSchema)]
struct Name {
    /// Nombre.
    name: String,
}

#[derive(Deserialize, JsonSchema)]
struct Rename {
    /// Id del elemento.
    id: i64,
    /// Nombre nuevo.
    name: String,
}

#[derive(Deserialize, JsonSchema)]
struct CreateNote {
    /// Título corto y descriptivo.
    title: String,
    /// Contenido en Markdown.
    content: String,
    /// Cuaderno; se crea si no existe. Omítelo si el usuario no indica ninguno.
    notebook: Option<String>,
    /// Etiquetas, sin '#'.
    tags: Option<Vec<String>>,
    /// Qué adjuntos (audios e imágenes) copiar: none (por defecto), current (los del mensaje actual), conversation (todos los de la conversación) o selected (los de attachment_ids).
    attach: Option<String>,
    /// Ids de adjuntos concretos (con attach = selected).
    attachment_ids: Option<Vec<i64>>,
}

#[derive(Deserialize, JsonSchema)]
struct SearchNotes {
    /// Palabras a buscar en título, contenido y transcripción (por prefijo, sin distinguir tildes).
    query: Option<String>,
    /// Limitar a este cuaderno.
    notebook: Option<String>,
    /// Limitar a esta etiqueta.
    tag: Option<String>,
    /// Máximo de resultados (por defecto 50).
    limit: Option<i64>,
}

#[derive(Deserialize, JsonSchema)]
struct UpdateNote {
    /// Id de la nota.
    id: i64,
    /// Título nuevo.
    title: Option<String>,
    /// Contenido nuevo completo (sustituye al actual).
    content: Option<String>,
    /// Texto que se añade al final del contenido.
    append: Option<String>,
    /// Mover a este cuaderno (se crea si no existe); cadena vacía para sacarla del cuaderno.
    notebook: Option<String>,
    /// Lista completa de etiquetas (sustituye a las actuales).
    tags: Option<Vec<String>>,
    /// Adjuntos que añadir: current, conversation o selected (con attachment_ids).
    attach: Option<String>,
    /// Ids de adjuntos concretos (con attach = selected).
    attachment_ids: Option<Vec<i64>>,
}

#[derive(Deserialize, JsonSchema)]
struct CreateReminder {
    /// Título corto: de qué hay que avisar.
    title: String,
    /// Detalles en Markdown.
    text: Option<String>,
    /// Cuándo avisar: 'YYYY-MM-DD HH:MM' (hora local).
    at: String,
    /// none (por defecto), daily, weekly, monthly o yearly.
    repeat: Option<String>,
    /// Qué adjuntos copiar: none, current, conversation o selected (con attachment_ids).
    attach: Option<String>,
    /// Ids de adjuntos concretos (con attach = selected).
    attachment_ids: Option<Vec<i64>>,
}

#[derive(Deserialize, JsonSchema)]
struct UpdateReminder {
    /// Id del recordatorio.
    id: i64,
    /// Título nuevo.
    title: Option<String>,
    /// Texto nuevo completo.
    text: Option<String>,
    /// Nueva fecha y hora 'YYYY-MM-DD HH:MM'; lo vuelve a dejar pendiente.
    at: Option<String>,
    /// none, daily, weekly, monthly o yearly.
    repeat: Option<String>,
    /// Adjuntos que añadir: current, conversation o selected (con attachment_ids).
    attach: Option<String>,
    /// Ids de adjuntos concretos (con attach = selected).
    attachment_ids: Option<Vec<i64>>,
}

#[derive(Deserialize, JsonSchema)]
struct ListReminders {
    /// pending (por defecto), done o all.
    status: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct AddTodo {
    /// Nombre de la lista; se crea si no existe.
    list: String,
    /// Texto de la tarea.
    text: String,
    /// Fecha límite: 'YYYY-MM-DD' o 'YYYY-MM-DD HH:MM'.
    due: Option<String>,
    /// high, medium, low o none.
    priority: Option<String>,
    /// Momento del aviso: 'YYYY-MM-DD HH:MM'.
    reminder: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct ListTodos {
    /// Limitar a esta lista.
    list: Option<String>,
    /// pending (por defecto), done o all.
    status: Option<String>,
    /// Fecha límite desde (incluida): 'YYYY-MM-DD'.
    due_from: Option<String>,
    /// Fecha límite hasta (incluida): 'YYYY-MM-DD'.
    due_to: Option<String>,
    /// Prioridad mínima: low, medium o high.
    min_priority: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct UpdateTodo {
    /// Id de la tarea.
    id: i64,
    /// Texto nuevo.
    text: Option<String>,
    /// Fecha límite nueva ('YYYY-MM-DD' o 'YYYY-MM-DD HH:MM'); cadena vacía para quitarla.
    due: Option<String>,
    /// high, medium, low o none.
    priority: Option<String>,
    /// Aviso nuevo ('YYYY-MM-DD HH:MM'); cadena vacía para quitarlo.
    reminder: Option<String>,
    /// Mover a esta lista (se crea si no existe).
    list: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct CompleteTodo {
    /// Id de la tarea.
    id: i64,
    /// false para volver a marcarla como pendiente.
    done: Option<bool>,
}

fn spec<T: JsonSchema>(name: &str, description: &str) -> ToolSpec {
    let mut settings = schemars::generate::SchemaSettings::draft2020_12();
    settings.inline_subschemas = true;
    let schema = settings.into_generator().into_root_schema_for::<T>();
    ToolSpec { name: name.into(), description: description.into(), schema: clean_schema(schema.to_value()) }
}

/// Deja el esquema en lo que entienden todos los proveedores: sin metadatos y con
/// los campos opcionales como tipos simples (Option<T> genera `["T", "null"]`).
fn clean_schema(mut schema: Value) -> Value {
    fn walk(value: &mut Value) {
        match value {
            Value::Object(map) => {
                if let Some(Value::Array(types)) = map.get("type") {
                    let non_null: Vec<Value> = types.iter().filter(|t| *t != "null").cloned().collect();
                    if non_null.len() == 1 {
                        map.insert("type".into(), non_null[0].clone());
                    }
                }
                map.remove("format");
                map.values_mut().for_each(walk);
            }
            Value::Array(items) => items.iter_mut().for_each(walk),
            _ => {}
        }
    }
    if let Some(map) = schema.as_object_mut() {
        map.remove("$schema");
        map.remove("title");
        map.entry("properties").or_insert_with(|| json!({}));
    }
    walk(&mut schema);
    schema
}

pub fn specs() -> Vec<ToolSpec> {
    vec![
        spec::<CreateNote>("create_note", "Crea una nota."),
        spec::<SearchNotes>("search_notes", "Busca notas por texto, cuaderno o etiqueta. Sin filtros devuelve las más recientes."),
        spec::<Id>("get_note", "Devuelve una nota completa."),
        spec::<UpdateNote>("update_note", "Modifica una nota: título, contenido, añadir texto, cuaderno o etiquetas."),
        spec::<Id>("delete_note", "Borra una nota (pide confirmación al usuario)."),
        spec::<NoArgs>("list_notebooks", "Lista los cuadernos con su número de notas."),
        spec::<Name>("create_notebook", "Crea un cuaderno de notas vacío."),
        spec::<Rename>("rename_notebook", "Cambia el nombre de un cuaderno."),
        spec::<Id>("delete_notebook", "Borra un cuaderno; sus notas se conservan sin cuaderno (pide confirmación)."),
        spec::<NoArgs>("list_tags", "Lista las etiquetas con su número de notas."),
        spec::<NoArgs>("list_todo_lists", "Lista las listas de tareas con sus tareas pendientes y totales."),
        spec::<Name>("create_todo_list", "Crea una lista de tareas vacía."),
        spec::<Rename>("rename_todo_list", "Cambia el nombre de una lista de tareas."),
        spec::<Id>("delete_todo_list", "Borra una lista y todas sus tareas (pide confirmación)."),
        spec::<AddTodo>("add_todo", "Añade una tarea a una lista."),
        spec::<ListTodos>("list_todos", "Lista tareas con filtros. Por defecto, solo las pendientes de todas las listas."),
        spec::<UpdateTodo>("update_todo", "Modifica una tarea: texto, fecha, prioridad, aviso o lista."),
        spec::<CompleteTodo>("complete_todo", "Marca una tarea como hecha (o pendiente con done=false)."),
        spec::<Id>("delete_todo", "Borra una tarea (pide confirmación)."),
        spec::<NoArgs>(
            "list_conversation_attachments",
            "Lista los audios e imágenes de los mensajes de esta conversación, con sus transcripciones.",
        ),
        spec::<CreateReminder>("create_reminder", "Crea un recordatorio: un aviso a una hora concreta, que puede repetirse."),
        spec::<ListReminders>("list_reminders", "Lista recordatorios. Por defecto, los pendientes ordenados por fecha."),
        spec::<Id>("get_reminder", "Devuelve un recordatorio completo."),
        spec::<UpdateReminder>("update_reminder", "Modifica un recordatorio: título, texto, fecha, repetición o adjuntos."),
        spec::<Id>("complete_reminder", "Marca un recordatorio como hecho (si se repite, pasa a la siguiente fecha)."),
        spec::<Id>("delete_reminder", "Borra un recordatorio (pide confirmación)."),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemas_are_plain_objects() {
        for tool in specs() {
            let schema = &tool.schema;
            assert_eq!(schema["type"], "object", "{}", tool.name);
            assert!(schema.get("$schema").is_none() && schema.get("$defs").is_none(), "{}", tool.name);
            assert!(!schema.to_string().contains("null"), "{}: {schema}", tool.name);
        }
        let note = specs().into_iter().find(|t| t.name == "create_note").unwrap().schema;
        assert_eq!(note["required"], json!(["title", "content"]));
        assert_eq!(note["properties"]["tags"]["type"], "array");
    }

    #[tokio::test]
    async fn create_and_delete_with_confirmation() -> Result<()> {
        use crate::store::{AttachmentKind, MessageKind, MessageRecord, NewAttachment};
        let store = Store::in_memory()?;
        let conversation = store.create_conversation("c", "anthropic", "m", "s")?;
        let mut ids = Vec::new();
        for text in ["primera idea", "pásalo a nota"] {
            let message = store.add_message(conversation, &MessageRecord::new(MessageKind::User, text))?;
            let audio = NewAttachment {
                kind: AttachmentKind::Audio,
                path: format!("/tmp/{text}.ogg"),
                mime: "audio/ogg".into(),
                transcript: Some(text.into()),
            };
            ids.push(store.add_attachment(Owner::Message(message), &audio)?);
        }
        let (commands, requests) = async_channel::unbounded();
        let context = TurnContext { conversation_id: Some(conversation), attachment_ids: vec![ids[1]] };
        let toolbox = Toolbox::new(store.clone(), commands, context);

        // Solo el audio con contenido, no el de la orden.
        let created = toolbox
            .run("create_note", json!({ "title": "Idea", "content": "algo", "attach": "selected", "attachment_ids": [ids[0]] }))
            .await?;
        let id = created.content["id"].as_i64().unwrap();
        let note = store.get_note(id)?;
        assert_eq!(note.attachments.len(), 1);
        assert_eq!(note.attachments[0].transcript.as_deref(), Some("primera idea"));
        // La búsqueda encuentra la transcripción.
        assert_eq!(store.search_notes(&NoteFilter { query: Some("primera".into()), ..Default::default() })?.len(), 1);
        // Todos los de la conversación.
        let all = toolbox.run("create_note", json!({ "title": "Todo", "content": "x", "attach": "conversation" })).await?;
        assert_eq!(store.get_note(all.content["id"].as_i64().unwrap())?.attachments.len(), 2);

        // Responde «no» a la confirmación y luego «sí».
        for (answer, deleted) in [(false, false), (true, true)] {
            let confirmer = tokio::spawn({
                let requests = requests.clone();
                async move {
                    if let Ok(Command::ConfirmDeletion { reply, .. }) = requests.recv().await {
                        reply.send(answer).await.unwrap();
                    }
                }
            });
            let result = toolbox.run("delete_note", json!({ "id": id })).await?;
            confirmer.await?;
            assert_eq!(result.content["deleted"], deleted);
        }
        assert!(store.get_note(id).is_err());
        Ok(())
    }

    #[tokio::test]
    async fn reminders_lifecycle() -> Result<()> {
        let store = Store::in_memory()?;
        let (commands, _requests) = async_channel::unbounded();
        let toolbox = Toolbox::new(store.clone(), commands, TurnContext::default());
        let created = toolbox
            .run("create_reminder", json!({ "title": "Pastilla", "at": "2026-01-01 08:00", "repeat": "daily" }))
            .await?;
        let id = created.content["reminder"]["id"].as_i64().unwrap();
        let now = chrono::Utc::now().timestamp();
        assert_eq!(store.reminders_due(now)?.len(), 1);
        // Se repite: al completarlo pasa a una fecha futura y sigue pendiente.
        let done = toolbox.run("complete_reminder", json!({ "id": id })).await?;
        assert_eq!(done.content["reminder"]["done"], false);
        assert!(store.get_reminder(id)?.due_at > now);
        // Sin repetición pasa a completados.
        toolbox.run("update_reminder", json!({ "id": id, "repeat": "none" })).await?;
        toolbox.run("complete_reminder", json!({ "id": id })).await?;
        assert_eq!(store.list_reminders(true)?.len(), 1);
        assert!(toolbox.run("create_reminder", json!({ "title": "x", "at": "mañana" })).await.is_err());
        Ok(())
    }
}
