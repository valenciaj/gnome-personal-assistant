//! Tools que la IA puede usar sobre notas y tareas.
//!
//! Los argumentos de cada tool son structs con `JsonSchema`: el esquema que ve el
//! modelo y la validación al recibir la llamada salen de la misma definición.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use schemars::JsonSchema;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::app::Command;
use crate::llm::{ToolCall, ToolExecutor, ToolOutput, ToolSpec};
use crate::store::{
    NewNote, NewTodo, NoteFilter, NoteUpdate, Priority, Store, Todo, TodoFilter, TodoStatus, TodoUpdate,
};

/// Se añade al prompt de sistema de cada conversación.
pub const GUIDANCE: &str = "\
Tienes tools para gestionar las notas del usuario (con cuadernos y etiquetas) y sus \
listas de tareas. Úsalas cuando pida apuntar, recordar, consultar, cambiar o borrar \
algo. Si la nota sale de un mensaje de voz, adjunta el audio. Convierte las fechas \
relativas («mañana», «el viernes») a fechas absolutas a partir de la fecha actual. \
Para editar o borrar, busca primero el elemento y usa su id; nunca inventes ids. \
Los borrados piden confirmación al usuario automáticamente: no se la pidas tú antes. \
Tras usar tools, resume en una frase lo que has hecho.";

/// Datos del mensaje que está procesando el agente.
#[derive(Debug, Default, Clone)]
pub struct TurnContext {
    pub conversation_id: Option<i64>,
    pub audio_path: Option<String>,
    pub transcript: Option<String>,
}

pub struct Toolbox {
    store: Store,
    commands: async_channel::Sender<Command>,
    context: TurnContext,
    audio_used: AtomicBool,
}

impl Toolbox {
    pub fn new(store: Store, commands: async_channel::Sender<Command>, context: TurnContext) -> Self {
        Self { store, commands, context, audio_used: AtomicBool::new(false) }
    }

    /// Si alguna nota se ha quedado con el audio del mensaje.
    pub fn audio_used(&self) -> bool {
        self.audio_used.load(Ordering::Relaxed)
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
                "Borrado cancelado",
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
                let attach = a.attach_audio.unwrap_or(false) && self.context.audio_path.is_some();
                let id = store.create_note(NewNote {
                    title: a.title.clone(),
                    content: a.content,
                    notebook: a.notebook,
                    tags: a.tags.unwrap_or_default(),
                    transcript: attach.then(|| self.context.transcript.clone()).flatten(),
                    audio_path: attach.then(|| self.context.audio_path.clone()).flatten(),
                    conversation_id: self.context.conversation_id,
                })?;
                if attach {
                    self.audio_used.store(true, Ordering::Relaxed);
                }
                let suffix = if attach { " (con audio)" } else { "" };
                Ok(ToolOutput::new(json!({ "id": id, "audio_attached": attach }), format!("Nota creada: «{}»{suffix}", a.title)))
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
                        "has_audio": note.has_audio,
                        "transcript": note.transcript,
                        "created": local_time(note.created),
                        "modified": local_time(note.modified),
                    }),
                    format!("Nota leída: «{}»", note.title),
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
                Ok(ToolOutput::new(json!({ "notes": notes }), format!("Búsqueda de notas: {count} resultados")))
            }
            "update_note" => {
                let a: UpdateNote = args(input)?;
                store.update_note(
                    a.id,
                    NoteUpdate { title: a.title, content: a.content, append: a.append, notebook: a.notebook, tags: a.tags },
                )?;
                let note = store.get_note(a.id)?;
                Ok(ToolOutput::new(json!({ "updated": true }), format!("Nota actualizada: «{}»", note.title)))
            }
            "delete_note" => {
                let a: Id = args(input)?;
                let note = store.get_note(a.id)?;
                let store = store.clone();
                self.delete_with_confirmation(
                    format!("¿Borrar la nota «{}»?", note.title),
                    format!("Nota borrada: «{}»", note.title),
                    move || {
                        if let Some(audio) = store.delete_note(a.id)?
                            && !store.referenced_audio_paths()?.contains(&audio)
                        {
                            let _ = std::fs::remove_file(&audio);
                        }
                        Ok(())
                    },
                )
                .await
            }
            "list_notebooks" => {
                let notebooks = store.list_notebooks()?;
                Ok(ToolOutput::new(json!({ "notebooks": notebooks }), "Cuadernos consultados"))
            }
            "create_notebook" => {
                let a: Name = args(input)?;
                let id = store.create_notebook(&a.name)?;
                Ok(ToolOutput::new(json!({ "id": id }), format!("Cuaderno creado: «{}»", a.name.trim())))
            }
            "rename_notebook" => {
                let a: Rename = args(input)?;
                store.rename_notebook(a.id, &a.name)?;
                Ok(ToolOutput::new(json!({ "renamed": true }), format!("Cuaderno renombrado: «{}»", a.name.trim())))
            }
            "delete_notebook" => {
                let a: Id = args(input)?;
                let name = store.notebook_name(a.id)?;
                let store = store.clone();
                self.delete_with_confirmation(
                    format!("¿Borrar el cuaderno «{name}»? Sus notas se conservan, sin cuaderno."),
                    format!("Cuaderno borrado: «{name}»"),
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
                Ok(ToolOutput::new(json!({ "tags": tags }), "Etiquetas consultadas"))
            }
            "list_todo_lists" => {
                let lists = store.list_todo_lists()?;
                Ok(ToolOutput::new(json!({ "lists": lists }), "Listas consultadas"))
            }
            "create_todo_list" => {
                let a: Name = args(input)?;
                let id = store.create_todo_list(&a.name)?;
                Ok(ToolOutput::new(json!({ "id": id }), format!("Lista creada: «{}»", a.name.trim())))
            }
            "rename_todo_list" => {
                let a: Rename = args(input)?;
                store.rename_todo_list(a.id, &a.name)?;
                Ok(ToolOutput::new(json!({ "renamed": true }), format!("Lista renombrada: «{}»", a.name.trim())))
            }
            "delete_todo_list" => {
                let a: Id = args(input)?;
                let list = store.todo_list(a.id)?;
                let store = store.clone();
                self.delete_with_confirmation(
                    format!("¿Borrar la lista «{}» y sus {} tareas?", list.name, list.total),
                    format!("Lista borrada: «{}»", list.name),
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
                Ok(ToolOutput::new(json!({ "id": id }), format!("Tarea añadida a «{}»: {}", a.list.trim(), a.text.trim())))
            }
            "list_todos" => {
                let a: ListTodos = args(input)?;
                let status = match a.status.as_deref().unwrap_or("pending") {
                    "pending" => TodoStatus::Pending,
                    "done" => TodoStatus::Done,
                    "all" => TodoStatus::All,
                    other => bail!("Estado desconocido: {other} (usa pending, done o all)"),
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
                Ok(ToolOutput::new(json!({ "todos": todos }), format!("Tareas consultadas: {count}")))
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
                Ok(ToolOutput::new(json!({ "todo": todo_json(&todo) }), format!("Tarea actualizada: {}", todo.text)))
            }
            "complete_todo" => {
                let a: CompleteTodo = args(input)?;
                let done = a.done.unwrap_or(true);
                store.set_todo_done(a.id, done)?;
                let todo = store.get_todo(a.id)?;
                let verb = if done { "completada" } else { "pendiente de nuevo" };
                Ok(ToolOutput::new(json!({ "done": done }), format!("Tarea {verb}: {}", todo.text)))
            }
            "delete_todo" => {
                let a: Id = args(input)?;
                let todo = store.get_todo(a.id)?;
                let store = store.clone();
                self.delete_with_confirmation(
                    format!("¿Borrar la tarea «{}» de «{}»?", todo.text, todo.list),
                    format!("Tarea borrada: {}", todo.text),
                    move || store.delete_todo(a.id),
                )
                .await
            }
            other => Err(anyhow!("La tool {other} no existe")),
        }
    }
}

impl ToolExecutor for Toolbox {
    async fn execute(&self, call: &ToolCall) -> Result<ToolOutput> {
        self.run(&call.name, call.input.clone()).await
    }
}

fn args<T: DeserializeOwned>(input: Value) -> Result<T> {
    serde_json::from_value(input).context("Argumentos no válidos")
}

fn local_time(unix: i64) -> String {
    chrono::DateTime::from_timestamp(unix, 0)
        .map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
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
    /// Adjunta el audio y la transcripción del mensaje de voz actual.
    attach_audio: Option<bool>,
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
        let store = Store::in_memory()?;
        let (commands, requests) = async_channel::unbounded();
        let toolbox = Toolbox::new(
            store.clone(),
            commands,
            TurnContext { audio_path: Some("/tmp/x.ogg".into()), transcript: Some("hola".into()), ..Default::default() },
        );
        let created = toolbox
            .run("create_note", json!({ "title": "Idea", "content": "algo", "attach_audio": true }))
            .await?;
        assert!(toolbox.audio_used());
        let id = created.content["id"].as_i64().unwrap();

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
}
