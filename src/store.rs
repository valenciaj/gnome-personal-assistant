//! Almacenamiento en SQLite: notas, cuadernos, etiquetas, listas de tareas,
//! tareas y conversaciones. Los audios son archivos en `audio_dir()`.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;
use serde_json::Value;

use crate::i18n::{tr, trf};

/// Cada entrada lleva el esquema de la versión N-1 a la N (PRAGMA user_version).
const MIGRATIONS: &[&str] = &[r#"
CREATE TABLE notebooks (
    id      INTEGER PRIMARY KEY,
    name    TEXT NOT NULL UNIQUE COLLATE NOCASE,
    created INTEGER NOT NULL
);
CREATE TABLE conversations (
    id       INTEGER PRIMARY KEY,
    title    TEXT NOT NULL,
    provider TEXT NOT NULL,
    model    TEXT NOT NULL,
    system   TEXT NOT NULL,
    native   TEXT NOT NULL DEFAULT '[]',
    created  INTEGER NOT NULL,
    modified INTEGER NOT NULL
);
CREATE TABLE messages (
    id              INTEGER PRIMARY KEY,
    conversation_id INTEGER NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    kind            TEXT NOT NULL,   -- user | reasoning | text | tool | error
    content         TEXT NOT NULL,
    tool_name       TEXT,
    tool_input      TEXT,
    tool_output     TEXT,
    is_error        INTEGER NOT NULL DEFAULT 0,
    audio_path      TEXT,
    created         INTEGER NOT NULL
);
CREATE INDEX messages_conversation ON messages(conversation_id);
CREATE TABLE notes (
    id              INTEGER PRIMARY KEY,
    notebook_id     INTEGER REFERENCES notebooks(id) ON DELETE SET NULL,
    title           TEXT NOT NULL,
    content         TEXT NOT NULL,
    transcript      TEXT,
    audio_path      TEXT,
    conversation_id INTEGER REFERENCES conversations(id) ON DELETE SET NULL,
    created         INTEGER NOT NULL,
    modified        INTEGER NOT NULL
);
CREATE TABLE tags (
    id   INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE COLLATE NOCASE
);
CREATE TABLE note_tags (
    note_id INTEGER NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    tag_id  INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
    PRIMARY KEY (note_id, tag_id)
);
CREATE VIRTUAL TABLE notes_fts USING fts5(
    title, content, transcript, content='notes', content_rowid='id',
    tokenize='unicode61 remove_diacritics 2'
);
CREATE TRIGGER notes_ai AFTER INSERT ON notes BEGIN
    INSERT INTO notes_fts(rowid, title, content, transcript)
    VALUES (new.id, new.title, new.content, coalesce(new.transcript, ''));
END;
CREATE TRIGGER notes_ad AFTER DELETE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, title, content, transcript)
    VALUES ('delete', old.id, old.title, old.content, coalesce(old.transcript, ''));
END;
CREATE TRIGGER notes_au AFTER UPDATE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, title, content, transcript)
    VALUES ('delete', old.id, old.title, old.content, coalesce(old.transcript, ''));
    INSERT INTO notes_fts(rowid, title, content, transcript)
    VALUES (new.id, new.title, new.content, coalesce(new.transcript, ''));
END;
CREATE TABLE todo_lists (
    id      INTEGER PRIMARY KEY,
    name    TEXT NOT NULL UNIQUE COLLATE NOCASE,
    created INTEGER NOT NULL
);
CREATE TABLE todos (
    id          INTEGER PRIMARY KEY,
    list_id     INTEGER NOT NULL REFERENCES todo_lists(id) ON DELETE CASCADE,
    text        TEXT NOT NULL,
    done        INTEGER NOT NULL DEFAULT 0,
    due         TEXT,              -- 'YYYY-MM-DD' o 'YYYY-MM-DD HH:MM' (hora local)
    priority    INTEGER NOT NULL DEFAULT 0,
    reminder_at INTEGER,           -- unix
    reminded    INTEGER NOT NULL DEFAULT 0,
    created     INTEGER NOT NULL,
    completed   INTEGER
);
CREATE INDEX todos_list ON todos(list_id);
"#, r#"
CREATE TABLE reminders (
    id              INTEGER PRIMARY KEY,
    title           TEXT NOT NULL,
    body            TEXT NOT NULL DEFAULT '',
    due_at          INTEGER NOT NULL,   -- próximo aviso (unix)
    anchor_at       INTEGER NOT NULL,   -- primera fecha; las repeticiones se calculan desde aquí
    repeat          TEXT NOT NULL DEFAULT 'none',
    done            INTEGER NOT NULL DEFAULT 0,
    notified        INTEGER NOT NULL DEFAULT 0,
    conversation_id INTEGER REFERENCES conversations(id) ON DELETE SET NULL,
    created         INTEGER NOT NULL,
    completed       INTEGER
);
CREATE INDEX reminders_due ON reminders(done, due_at);
-- Audios e imágenes. Un mismo archivo puede estar en varias filas (el audio de un
-- mensaje copiado a una nota); el archivo se borra cuando ninguna fila lo usa.
CREATE TABLE attachments (
    id          INTEGER PRIMARY KEY,
    kind        TEXT NOT NULL,          -- audio | image
    path        TEXT NOT NULL,
    mime        TEXT NOT NULL,
    transcript  TEXT,
    note_id     INTEGER REFERENCES notes(id) ON DELETE CASCADE,
    reminder_id INTEGER REFERENCES reminders(id) ON DELETE CASCADE,
    message_id  INTEGER REFERENCES messages(id) ON DELETE CASCADE,
    created     INTEGER NOT NULL
);
CREATE INDEX attachments_note ON attachments(note_id);
CREATE INDEX attachments_reminder ON attachments(reminder_id);
CREATE INDEX attachments_message ON attachments(message_id);
INSERT INTO attachments (kind, path, mime, transcript, note_id, created)
    SELECT 'audio', audio_path, 'audio/ogg', transcript, id, created FROM notes WHERE audio_path IS NOT NULL;
INSERT INTO attachments (kind, path, mime, transcript, message_id, created)
    SELECT 'audio', audio_path, 'audio/ogg', content, id, created FROM messages WHERE audio_path IS NOT NULL;
ALTER TABLE notes DROP COLUMN audio_path;
ALTER TABLE messages DROP COLUMN audio_path;
"#];

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    None,
    Low,
    Medium,
    High,
}

impl Priority {
    pub fn from_db(value: i64) -> Self {
        match value {
            1 => Self::Low,
            2 => Self::Medium,
            3 => Self::High,
            _ => Self::None,
        }
    }

    pub fn to_db(self) -> i64 {
        self as i64
    }

    pub fn parse(text: &str) -> Result<Self> {
        Ok(match text.trim().to_lowercase().as_str() {
            "" | "none" | "ninguna" => Self::None,
            "low" | "baja" => Self::Low,
            "medium" | "media" => Self::Medium,
            "high" | "alta" => Self::High,
            other => bail!(trf("Prioridad desconocida: {value} (usa high, medium, low o none)", &[("value", &other)])),
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Notebook {
    pub id: i64,
    pub name: String,
    pub notes: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AttachmentKind {
    Audio,
    Image,
}

impl AttachmentKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Audio => "audio",
            Self::Image => "image",
        }
    }

    fn parse(text: &str) -> Self {
        if text == "image" { Self::Image } else { Self::Audio }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Attachment {
    pub id: i64,
    pub kind: AttachmentKind,
    #[serde(skip)]
    pub path: String,
    pub mime: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript: Option<String>,
    pub created: i64,
}

#[derive(Debug, Clone)]
pub struct NewAttachment {
    pub kind: AttachmentKind,
    pub path: String,
    pub mime: String,
    pub transcript: Option<String>,
}

/// A qué pertenece un adjunto.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    Note(i64),
    Reminder(i64),
    Message(i64),
}

impl Owner {
    fn column(self) -> (&'static str, i64) {
        match self {
            Self::Note(id) => ("note_id", id),
            Self::Reminder(id) => ("reminder_id", id),
            Self::Message(id) => ("message_id", id),
        }
    }
}

/// Adjunto de un mensaje del usuario en una conversación.
#[derive(Debug, Clone)]
pub struct ConversationAttachment {
    pub attachment: Attachment,
    pub message_id: i64,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Note {
    pub id: i64,
    pub title: String,
    pub content: String,
    pub notebook: Option<String>,
    pub tags: Vec<String>,
    pub attachments: Vec<Attachment>,
    pub created: i64,
    pub modified: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct NoteSummary {
    pub id: i64,
    pub title: String,
    pub notebook: Option<String>,
    pub tags: Vec<String>,
    pub snippet: String,
    pub audios: i64,
    pub images: i64,
    pub modified: i64,
}

#[derive(Debug, Default)]
pub struct NewNote {
    pub title: String,
    pub content: String,
    pub notebook: Option<String>,
    pub tags: Vec<String>,
    pub conversation_id: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Repeat {
    #[default]
    None,
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

impl Repeat {
    pub const ALL: [Self; 5] = [Self::None, Self::Daily, Self::Weekly, Self::Monthly, Self::Yearly];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Daily => "daily",
            Self::Weekly => "weekly",
            Self::Monthly => "monthly",
            Self::Yearly => "yearly",
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|r| r.as_str() == text.trim())
            .ok_or_else(|| anyhow::anyhow!(trf("Repetición desconocida: {value} (usa none, daily, weekly, monthly o yearly)", &[("value", &text)])))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Reminder {
    pub id: i64,
    pub title: String,
    pub body: String,
    pub due_at: i64,
    pub anchor_at: i64,
    pub repeat: Repeat,
    pub done: bool,
    pub notified: bool,
    pub attachments: Vec<Attachment>,
    pub created: i64,
    pub completed: Option<i64>,
}

#[derive(Debug, Default)]
pub struct NewReminder {
    pub title: String,
    pub body: String,
    pub due_at: i64,
    pub repeat: Repeat,
    pub conversation_id: Option<i64>,
}

#[derive(Debug, Default)]
pub struct ReminderUpdate {
    pub title: Option<String>,
    pub body: Option<String>,
    /// Cambia también la fecha base de las repeticiones y vuelve a avisar.
    pub due_at: Option<i64>,
    pub repeat: Option<Repeat>,
}

#[derive(Debug, Default)]
pub struct NoteUpdate {
    pub title: Option<String>,
    pub content: Option<String>,
    pub append: Option<String>,
    /// `Some("")` saca la nota de su cuaderno.
    pub notebook: Option<String>,
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Default)]
pub struct NoteFilter {
    pub query: Option<String>,
    pub notebook: Option<String>,
    pub tag: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TodoList {
    pub id: i64,
    pub name: String,
    pub pending: i64,
    pub total: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Todo {
    pub id: i64,
    pub list_id: i64,
    pub list: String,
    pub text: String,
    pub done: bool,
    pub due: Option<String>,
    pub priority: Priority,
    pub reminder_at: Option<i64>,
    pub created: i64,
    pub completed: Option<i64>,
}

#[derive(Debug, Default)]
pub struct NewTodo {
    pub list: String,
    pub text: String,
    pub due: Option<String>,
    pub priority: Option<Priority>,
    pub reminder_at: Option<i64>,
}

#[derive(Debug, Default)]
pub struct TodoUpdate {
    pub text: Option<String>,
    /// `Some(None)` quita la fecha.
    pub due: Option<Option<String>>,
    pub priority: Option<Priority>,
    pub reminder_at: Option<Option<i64>>,
    pub list: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TodoStatus {
    #[default]
    Pending,
    Done,
    All,
}

#[derive(Debug, Default)]
pub struct TodoFilter {
    pub list: Option<String>,
    pub status: TodoStatus,
    /// Comparación de texto sobre `due`: 'YYYY-MM-DD' incluye todo el día.
    pub due_from: Option<String>,
    pub due_to: Option<String>,
    pub min_priority: Option<Priority>,
}

#[derive(Debug, Clone)]
pub struct ConversationInfo {
    pub id: i64,
    pub title: String,
    pub provider: String,
    pub model: String,
    pub system: String,
    pub native: Vec<Value>,
    pub created: i64,
    pub modified: i64,
}

#[derive(Debug, Clone)]
pub struct MessageRecord {
    pub kind: MessageKind,
    pub content: String,
    pub tool_name: Option<String>,
    pub tool_input: Option<String>,
    pub tool_output: Option<String>,
    pub is_error: bool,
    /// Solo al leer del historial; para guardarlos, `Store::add_attachment`.
    pub attachments: Vec<Attachment>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    User,
    Reasoning,
    Text,
    Tool,
    Error,
}

impl MessageKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Reasoning => "reasoning",
            Self::Text => "text",
            Self::Tool => "tool",
            Self::Error => "error",
        }
    }

    fn parse(text: &str) -> Self {
        match text {
            "user" => Self::User,
            "reasoning" => Self::Reasoning,
            "tool" => Self::Tool,
            "error" => Self::Error,
            _ => Self::Text,
        }
    }
}

impl MessageRecord {
    pub fn new(kind: MessageKind, content: impl Into<String>) -> Self {
        Self {
            kind,
            content: content.into(),
            tool_name: None,
            tool_input: None,
            tool_output: None,
            is_error: false,
            attachments: Vec::new(),
        }
    }
}

/// Acceso compartido a la base de datos (hilo de GTK y tools en tokio).
#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path)
            .with_context(|| trf("No se pudo abrir {path}", &[("path", &path.display())]))?;
        Self::init(conn)
    }

    pub fn in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "foreign_keys", true)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        for (i, migration) in MIGRATIONS.iter().enumerate().skip(version as usize) {
            let tx = conn.transaction()?;
            tx.execute_batch(migration)
                .with_context(|| format!("Fallo en la migración {}", i + 1))?;
            tx.pragma_update(None, "user_version", i as i64 + 1)?;
            tx.commit()?;
        }
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    // ── Cuadernos ──────────────────────────────────────────────────────────

    pub fn list_notebooks(&self) -> Result<Vec<Notebook>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT nb.id, nb.name, count(n.id) FROM notebooks nb
             LEFT JOIN notes n ON n.notebook_id = nb.id
             GROUP BY nb.id ORDER BY nb.name",
        )?;
        let rows = stmt.query_map([], |r| Ok(Notebook { id: r.get(0)?, name: r.get(1)?, notes: r.get(2)? }))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn create_notebook(&self, name: &str) -> Result<i64> {
        let name = non_empty(name, &tr("El cuaderno necesita un nombre"))?;
        let conn = self.conn();
        conn.execute("INSERT INTO notebooks (name, created) VALUES (?1, ?2)", params![name, now()])
            .with_context(|| trf("Ya existe un cuaderno llamado «{name}»", &[("name", &name)]))?;
        Ok(conn.last_insert_rowid())
    }

    pub fn rename_notebook(&self, id: i64, name: &str) -> Result<()> {
        let name = non_empty(name, &tr("El cuaderno necesita un nombre"))?;
        let changed = self.conn().execute("UPDATE notebooks SET name = ?1 WHERE id = ?2", params![name, id])?;
        expect_changed(changed, Kind::Notebook, id)
    }

    /// Borra el cuaderno; sus notas quedan sin cuaderno.
    pub fn delete_notebook(&self, id: i64) -> Result<()> {
        let changed = self.conn().execute("DELETE FROM notebooks WHERE id = ?1", [id])?;
        expect_changed(changed, Kind::Notebook, id)
    }

    pub fn notebook_name(&self, id: i64) -> Result<String> {
        self.conn()
            .query_row("SELECT name FROM notebooks WHERE id = ?1", [id], |r| r.get(0))
            .optional()?
            .with_context(|| missing(Kind::Notebook, id))
    }

    fn notebook_id_or_create(conn: &Connection, name: &str) -> Result<i64> {
        if let Some(id) = conn
            .query_row("SELECT id FROM notebooks WHERE name = ?1", [name], |r| r.get(0))
            .optional()?
        {
            return Ok(id);
        }
        conn.execute("INSERT INTO notebooks (name, created) VALUES (?1, ?2)", params![name, now()])?;
        Ok(conn.last_insert_rowid())
    }

    // ── Notas ──────────────────────────────────────────────────────────────

    pub fn create_note(&self, note: NewNote) -> Result<i64> {
        let title = non_empty(&note.title, &tr("La nota necesita un título"))?;
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let notebook_id = match note.notebook.as_deref().map(str::trim) {
            Some(name) if !name.is_empty() => Some(Self::notebook_id_or_create(&tx, name)?),
            _ => None,
        };
        let ts = now();
        tx.execute(
            "INSERT INTO notes (notebook_id, title, content, conversation_id, created, modified)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            params![notebook_id, title, note.content, note.conversation_id, ts],
        )?;
        let id = tx.last_insert_rowid();
        Self::set_tags(&tx, id, &note.tags)?;
        tx.commit()?;
        Ok(id)
    }

    pub fn get_note(&self, id: i64) -> Result<Note> {
        let conn = self.conn();
        let note = conn
            .query_row(
                "SELECT n.id, n.title, n.content, nb.name, n.created, n.modified
                 FROM notes n LEFT JOIN notebooks nb ON nb.id = n.notebook_id WHERE n.id = ?1",
                [id],
                |r| {
                    Ok(Note {
                        id: r.get(0)?,
                        title: r.get(1)?,
                        content: r.get(2)?,
                        notebook: r.get(3)?,
                        tags: Vec::new(),
                        attachments: Vec::new(),
                        created: r.get(4)?,
                        modified: r.get(5)?,
                    })
                },
            )
            .optional()?
            .with_context(|| missing(Kind::Note, id))?;
        Ok(Note {
            tags: Self::note_tags(&conn, id)?,
            attachments: Self::attachments_of(&conn, Owner::Note(id))?,
            ..note
        })
    }

    pub fn update_note(&self, id: i64, update: NoteUpdate) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let exists: bool = tx.query_row("SELECT count(*) FROM notes WHERE id = ?1", [id], |r| r.get::<_, i64>(0))? > 0;
        if !exists {
            bail!(missing(Kind::Note, id));
        }
        if let Some(title) = update.title {
            tx.execute("UPDATE notes SET title = ?1 WHERE id = ?2", params![non_empty(&title, &tr("El título no puede estar vacío"))?, id])?;
        }
        if let Some(content) = update.content {
            tx.execute("UPDATE notes SET content = ?1 WHERE id = ?2", params![content, id])?;
        }
        if let Some(extra) = update.append {
            tx.execute(
                "UPDATE notes SET content = CASE WHEN content = '' THEN ?1 ELSE content || char(10) || char(10) || ?1 END
                 WHERE id = ?2",
                params![extra, id],
            )?;
        }
        if let Some(notebook) = update.notebook {
            let notebook_id = match notebook.trim() {
                "" => None,
                name => Some(Self::notebook_id_or_create(&tx, name)?),
            };
            tx.execute("UPDATE notes SET notebook_id = ?1 WHERE id = ?2", params![notebook_id, id])?;
        }
        if let Some(tags) = update.tags {
            Self::set_tags(&tx, id, &tags)?;
        }
        tx.execute("UPDATE notes SET modified = ?1 WHERE id = ?2", params![now(), id])?;
        tx.commit()?;
        Ok(())
    }

    /// Borra la nota. Sus archivos los recoge después la limpieza de huérfanos.
    pub fn delete_note(&self, id: i64) -> Result<()> {
        let conn = self.conn();
        let changed = conn.execute("DELETE FROM notes WHERE id = ?1", [id])?;
        expect_changed(changed, Kind::Note, id)?;
        conn.execute("DELETE FROM tags WHERE id NOT IN (SELECT tag_id FROM note_tags)", [])?;
        Ok(())
    }

    pub fn search_notes(&self, filter: &NoteFilter) -> Result<Vec<NoteSummary>> {
        let conn = self.conn();
        let mut sql = String::from(
            "SELECT n.id, n.title, nb.name, substr(n.content, 1, 160),
                    (SELECT count(*) FROM attachments a WHERE a.note_id = n.id AND a.kind = 'audio'),
                    (SELECT count(*) FROM attachments a WHERE a.note_id = n.id AND a.kind = 'image'),
                    n.modified
             FROM notes n LEFT JOIN notebooks nb ON nb.id = n.notebook_id WHERE 1",
        );
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(query) = filter.query.as_deref().and_then(fts_query) {
            sql.push_str(" AND n.id IN (SELECT rowid FROM notes_fts WHERE notes_fts MATCH ?)");
            args.push(Box::new(query));
        }
        if let Some(notebook) = filter.notebook.as_deref().filter(|s| !s.trim().is_empty()) {
            sql.push_str(" AND nb.name = ? COLLATE NOCASE");
            args.push(Box::new(notebook.trim().to_owned()));
        }
        if let Some(tag) = filter.tag.as_deref().filter(|s| !s.trim().is_empty()) {
            sql.push_str(
                " AND n.id IN (SELECT nt.note_id FROM note_tags nt JOIN tags t ON t.id = nt.tag_id
                               WHERE t.name = ? COLLATE NOCASE)",
            );
            args.push(Box::new(tag.trim().to_owned()));
        }
        sql.push_str(" ORDER BY n.modified DESC LIMIT ?");
        args.push(Box::new(filter.limit.unwrap_or(50).clamp(1, 500)));

        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(args.iter().map(|a| a.as_ref())), |r| {
            Ok(NoteSummary {
                id: r.get(0)?,
                title: r.get(1)?,
                notebook: r.get(2)?,
                tags: Vec::new(),
                snippet: r.get(3)?,
                audios: r.get(4)?,
                images: r.get(5)?,
                modified: r.get(6)?,
            })
        })?;
        let mut notes: Vec<NoteSummary> = rows.collect::<rusqlite::Result<_>>()?;
        for note in &mut notes {
            note.tags = Self::note_tags(&conn, note.id)?;
        }
        Ok(notes)
    }

    pub fn list_tags(&self) -> Result<Vec<(String, i64)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT t.name, count(nt.note_id) FROM tags t JOIN note_tags nt ON nt.tag_id = t.id
             GROUP BY t.id ORDER BY t.name",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn note_tags(conn: &Connection, note_id: i64) -> Result<Vec<String>> {
        let mut stmt = conn.prepare_cached(
            "SELECT t.name FROM tags t JOIN note_tags nt ON nt.tag_id = t.id WHERE nt.note_id = ?1 ORDER BY t.name",
        )?;
        let rows = stmt.query_map([note_id], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn set_tags(conn: &Connection, note_id: i64, tags: &[String]) -> Result<()> {
        conn.execute("DELETE FROM note_tags WHERE note_id = ?1", [note_id])?;
        for tag in tags.iter().map(|t| t.trim().trim_start_matches('#')).filter(|t| !t.is_empty()) {
            conn.execute("INSERT OR IGNORE INTO tags (name) VALUES (?1)", [tag])?;
            conn.execute(
                "INSERT OR IGNORE INTO note_tags (note_id, tag_id) SELECT ?1, id FROM tags WHERE name = ?2",
                params![note_id, tag],
            )?;
        }
        conn.execute("DELETE FROM tags WHERE id NOT IN (SELECT tag_id FROM note_tags)", [])?;
        Ok(())
    }

    // ── Listas de tareas ───────────────────────────────────────────────────

    pub fn list_todo_lists(&self) -> Result<Vec<TodoList>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT l.id, l.name, coalesce(sum(t.done = 0), 0), count(t.id) FROM todo_lists l
             LEFT JOIN todos t ON t.list_id = l.id GROUP BY l.id ORDER BY l.name",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(TodoList { id: r.get(0)?, name: r.get(1)?, pending: r.get(2)?, total: r.get(3)? })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn create_todo_list(&self, name: &str) -> Result<i64> {
        let name = non_empty(name, &tr("La lista necesita un nombre"))?;
        let conn = self.conn();
        conn.execute("INSERT INTO todo_lists (name, created) VALUES (?1, ?2)", params![name, now()])
            .with_context(|| trf("Ya existe una lista llamada «{name}»", &[("name", &name)]))?;
        Ok(conn.last_insert_rowid())
    }

    pub fn rename_todo_list(&self, id: i64, name: &str) -> Result<()> {
        let name = non_empty(name, &tr("La lista necesita un nombre"))?;
        let changed = self.conn().execute("UPDATE todo_lists SET name = ?1 WHERE id = ?2", params![name, id])?;
        expect_changed(changed, Kind::List, id)
    }

    /// Borra la lista con todas sus tareas.
    pub fn delete_todo_list(&self, id: i64) -> Result<()> {
        let changed = self.conn().execute("DELETE FROM todo_lists WHERE id = ?1", [id])?;
        expect_changed(changed, Kind::List, id)
    }

    pub fn todo_list(&self, id: i64) -> Result<TodoList> {
        self.list_todo_lists()?
            .into_iter()
            .find(|l| l.id == id)
            .with_context(|| missing(Kind::List, id))
    }

    fn list_id_or_create(conn: &Connection, name: &str) -> Result<i64> {
        if let Some(id) = conn
            .query_row("SELECT id FROM todo_lists WHERE name = ?1", [name], |r| r.get(0))
            .optional()?
        {
            return Ok(id);
        }
        conn.execute("INSERT INTO todo_lists (name, created) VALUES (?1, ?2)", params![name, now()])?;
        Ok(conn.last_insert_rowid())
    }

    // ── Tareas ─────────────────────────────────────────────────────────────

    /// Añade una tarea; si la lista no existe, la crea.
    pub fn add_todo(&self, todo: NewTodo) -> Result<i64> {
        let text = non_empty(&todo.text, &tr("La tarea necesita un texto"))?;
        let list = non_empty(&todo.list, &tr("Indica la lista de la tarea"))?;
        let due = todo.due.as_deref().map(normalize_due).transpose()?;
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let list_id = Self::list_id_or_create(&tx, list)?;
        tx.execute(
            "INSERT INTO todos (list_id, text, due, priority, reminder_at, created) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![list_id, text, due, todo.priority.unwrap_or(Priority::None).to_db(), todo.reminder_at, now()],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(id)
    }

    pub fn get_todo(&self, id: i64) -> Result<Todo> {
        let conn = self.conn();
        conn.query_row(&format!("{TODO_SELECT} WHERE t.id = ?1"), [id], todo_from_row)
            .optional()?
            .with_context(|| missing(Kind::Todo, id))
    }

    pub fn update_todo(&self, id: i64, update: TodoUpdate) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let exists: bool = tx.query_row("SELECT count(*) FROM todos WHERE id = ?1", [id], |r| r.get::<_, i64>(0))? > 0;
        if !exists {
            bail!(missing(Kind::Todo, id));
        }
        if let Some(text) = update.text {
            tx.execute("UPDATE todos SET text = ?1 WHERE id = ?2", params![non_empty(&text, &tr("El texto no puede estar vacío"))?, id])?;
        }
        if let Some(due) = update.due {
            let due = due.as_deref().map(normalize_due).transpose()?;
            // Con fecha nueva, el aviso derivado de ella vuelve a estar pendiente.
            tx.execute("UPDATE todos SET due = ?1, reminded = 0 WHERE id = ?2", params![due, id])?;
        }
        if let Some(priority) = update.priority {
            tx.execute("UPDATE todos SET priority = ?1 WHERE id = ?2", params![priority.to_db(), id])?;
        }
        if let Some(reminder) = update.reminder_at {
            tx.execute("UPDATE todos SET reminder_at = ?1, reminded = 0 WHERE id = ?2", params![reminder, id])?;
        }
        if let Some(list) = update.list {
            let list_id = Self::list_id_or_create(&tx, non_empty(&list, &tr("Indica la lista"))?)?;
            tx.execute("UPDATE todos SET list_id = ?1 WHERE id = ?2", params![list_id, id])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn set_todo_done(&self, id: i64, done: bool) -> Result<()> {
        let completed = done.then(now);
        let changed = self.conn().execute(
            "UPDATE todos SET done = ?1, completed = ?2 WHERE id = ?3",
            params![done, completed, id],
        )?;
        expect_changed(changed, Kind::Todo, id)
    }

    /// Tareas pendientes con fecha o aviso cuyo recordatorio aún no se ha mostrado.
    pub fn todos_to_remind(&self) -> Result<Vec<Todo>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "{TODO_SELECT} WHERE t.done = 0 AND t.reminded = 0 AND (t.reminder_at IS NOT NULL OR t.due IS NOT NULL)"
        ))?;
        let rows = stmt.query_map([], todo_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn mark_reminded(&self, id: i64) -> Result<()> {
        self.conn().execute("UPDATE todos SET reminded = 1 WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Vuelve a avisar de la tarea en `until` (unix).
    pub fn snooze_todo(&self, id: i64, until: i64) -> Result<()> {
        let changed =
            self.conn().execute("UPDATE todos SET reminder_at = ?1, reminded = 0 WHERE id = ?2", params![until, id])?;
        expect_changed(changed, Kind::Todo, id)
    }

    pub fn delete_todo(&self, id: i64) -> Result<()> {
        let changed = self.conn().execute("DELETE FROM todos WHERE id = ?1", [id])?;
        expect_changed(changed, Kind::Todo, id)
    }

    pub fn list_todos(&self, filter: &TodoFilter) -> Result<Vec<Todo>> {
        let conn = self.conn();
        let mut sql = format!("{TODO_SELECT} WHERE 1");
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(list) = filter.list.as_deref().filter(|s| !s.trim().is_empty()) {
            sql.push_str(" AND l.name = ? COLLATE NOCASE");
            args.push(Box::new(list.trim().to_owned()));
        }
        match filter.status {
            TodoStatus::Pending => sql.push_str(" AND t.done = 0"),
            TodoStatus::Done => sql.push_str(" AND t.done = 1"),
            TodoStatus::All => {}
        }
        if let Some(from) = &filter.due_from {
            sql.push_str(" AND t.due >= ?");
            args.push(Box::new(normalize_due(from)?));
        }
        if let Some(to) = &filter.due_to {
            // 'YYYY-MM-DD' debe incluir las tareas con hora de ese mismo día.
            let to = normalize_due(to)?;
            let bound = if to.len() == 10 { format!("{to} 99") } else { to };
            sql.push_str(" AND t.due <= ?");
            args.push(Box::new(bound));
        }
        if let Some(priority) = filter.min_priority {
            sql.push_str(" AND t.priority >= ?");
            args.push(Box::new(priority.to_db()));
        }
        sql.push_str(" ORDER BY t.done, t.due IS NULL, t.due, t.priority DESC, t.id");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(args.iter().map(|a| a.as_ref())), todo_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ── Conversaciones ─────────────────────────────────────────────────────

    pub fn create_conversation(&self, title: &str, provider: &str, model: &str, system: &str) -> Result<i64> {
        let title: String = title.chars().take(80).collect();
        let conn = self.conn();
        let ts = now();
        conn.execute(
            "INSERT INTO conversations (title, provider, model, system, created, modified)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            params![title, provider, model, system, ts],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn save_native(&self, id: i64, native: &[Value]) -> Result<()> {
        self.conn().execute(
            "UPDATE conversations SET native = ?1, modified = ?2 WHERE id = ?3",
            params![serde_json::to_string(native)?, now(), id],
        )?;
        Ok(())
    }

    pub fn add_message(&self, conversation_id: i64, message: &MessageRecord) -> Result<i64> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO messages (conversation_id, kind, content, tool_name, tool_input, tool_output, is_error, created)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                conversation_id,
                message.kind.as_str(),
                message.content,
                message.tool_name,
                message.tool_input,
                message.tool_output,
                message.is_error,
                now()
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn get_conversation(&self, id: i64) -> Result<ConversationInfo> {
        let conn = self.conn();
        let (info, native) = conn
            .query_row(
                "SELECT id, title, provider, model, system, native, created, modified FROM conversations WHERE id = ?1",
                [id],
                |r| {
                    Ok((
                        ConversationInfo {
                            id: r.get(0)?,
                            title: r.get(1)?,
                            provider: r.get(2)?,
                            model: r.get(3)?,
                            system: r.get(4)?,
                            native: Vec::new(),
                            created: r.get(6)?,
                            modified: r.get(7)?,
                        },
                        r.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()?
            .with_context(|| missing(Kind::Conversation, id))?;
        Ok(ConversationInfo { native: serde_json::from_str(&native)?, ..info })
    }

    pub fn list_conversations(&self, limit: i64) -> Result<Vec<ConversationInfo>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, title, provider, model, system, created, modified FROM conversations
             ORDER BY modified DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit], |r| {
            Ok(ConversationInfo {
                id: r.get(0)?,
                title: r.get(1)?,
                provider: r.get(2)?,
                model: r.get(3)?,
                system: r.get(4)?,
                native: Vec::new(),
                created: r.get(5)?,
                modified: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn messages(&self, conversation_id: i64) -> Result<Vec<MessageRecord>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT kind, content, tool_name, tool_input, tool_output, is_error, id
             FROM messages WHERE conversation_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([conversation_id], |r| {
            Ok(MessageRecord {
                kind: MessageKind::parse(&r.get::<_, String>(0)?),
                content: r.get(1)?,
                tool_name: r.get(2)?,
                tool_input: r.get(3)?,
                tool_output: r.get(4)?,
                is_error: r.get(5)?,
                attachments: Vec::new(),
            })
            .map(|record| (record, r.get::<_, i64>(6)))
        })?;
        let mut records = Vec::new();
        for row in rows {
            let (mut record, id) = row?;
            record.attachments = Self::attachments_of(&conn, Owner::Message(id?))?;
            records.push(record);
        }
        Ok(records)
    }

    /// Borra la conversación y sus mensajes; las notas que salieron de ella se conservan.
    pub fn delete_conversation(&self, id: i64) -> Result<()> {
        let changed = self.conn().execute("DELETE FROM conversations WHERE id = ?1", [id])?;
        expect_changed(changed, Kind::Conversation, id)
    }

    // ── Adjuntos ───────────────────────────────────────────────────────────

    pub fn add_attachment(&self, owner: Owner, attachment: &NewAttachment) -> Result<i64> {
        let conn = self.conn();
        let (column, id) = owner.column();
        conn.execute(
            &format!("INSERT INTO attachments (kind, path, mime, transcript, {column}, created) VALUES (?1, ?2, ?3, ?4, ?5, ?6)"),
            params![attachment.kind.as_str(), attachment.path, attachment.mime, attachment.transcript, id, now()],
        )?;
        let attachment_id = conn.last_insert_rowid();
        Self::touch_owner(&conn, owner)?;
        Ok(attachment_id)
    }

    /// Copia adjuntos existentes (por ejemplo, los de mensajes) a otro dueño.
    pub fn copy_attachments(&self, ids: &[i64], owner: Owner) -> Result<usize> {
        let conn = self.conn();
        let (column, owner_id) = owner.column();
        let mut copied = 0;
        for id in ids {
            copied += conn.execute(
                &format!(
                    "INSERT INTO attachments (kind, path, mime, transcript, {column}, created)
                     SELECT kind, path, mime, transcript, ?1, ?2 FROM attachments WHERE id = ?3"
                ),
                params![owner_id, now(), id],
            )?;
        }
        Self::touch_owner(&conn, owner)?;
        Ok(copied)
    }

    pub fn remove_attachment(&self, id: i64) -> Result<()> {
        let conn = self.conn();
        let owner: Option<(Option<i64>, Option<i64>)> = conn
            .query_row("SELECT note_id, reminder_id FROM attachments WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        conn.execute("DELETE FROM attachments WHERE id = ?1", [id])?;
        match owner {
            Some((Some(note), _)) => Self::touch_owner(&conn, Owner::Note(note))?,
            Some((_, Some(reminder))) => Self::touch_owner(&conn, Owner::Reminder(reminder))?,
            _ => {}
        }
        Ok(())
    }

    pub fn attachments(&self, owner: Owner) -> Result<Vec<Attachment>> {
        Self::attachments_of(&self.conn(), owner)
    }

    fn attachments_of(conn: &Connection, owner: Owner) -> Result<Vec<Attachment>> {
        let (column, id) = owner.column();
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT id, kind, path, mime, transcript, created FROM attachments WHERE {column} = ?1 ORDER BY id"
        ))?;
        let rows = stmt.query_map([id], attachment_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Tras cambiar adjuntos: fecha de modificación y, en notas, la transcripción
    /// que indexa la búsqueda.
    fn touch_owner(conn: &Connection, owner: Owner) -> Result<()> {
        match owner {
            Owner::Note(id) => {
                conn.execute(
                    "UPDATE notes SET modified = ?1, transcript = (
                         SELECT group_concat(transcript, char(10)) FROM attachments
                         WHERE note_id = ?2 AND kind = 'audio' AND transcript IS NOT NULL)
                     WHERE id = ?2",
                    params![now(), id],
                )?;
            }
            Owner::Reminder(_) | Owner::Message(_) => {}
        }
        Ok(())
    }

    /// Adjuntos de los mensajes del usuario de una conversación, en orden.
    pub fn conversation_attachments(&self, conversation_id: i64) -> Result<Vec<ConversationAttachment>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT a.id, a.kind, a.path, a.mime, a.transcript, a.created, m.id, m.content
             FROM attachments a JOIN messages m ON m.id = a.message_id
             WHERE m.conversation_id = ?1 ORDER BY a.id",
        )?;
        let rows = stmt.query_map([conversation_id], |r| {
            Ok(ConversationAttachment { attachment: attachment_from_row(r)?, message_id: r.get(6)?, message: r.get(7)? })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Archivos que usa alguna nota o recordatorio (no se borran nunca solos).
    pub fn kept_attachment_paths(&self) -> Result<HashSet<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT path FROM attachments WHERE note_id IS NOT NULL OR reminder_id IS NOT NULL")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Todos los archivos con alguna fila que los use.
    pub fn attachment_paths(&self) -> Result<HashSet<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT path FROM attachments")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Quita del historial un archivo que se va a borrar.
    pub fn forget_message_file(&self, path: &str) -> Result<()> {
        self.conn().execute("DELETE FROM attachments WHERE path = ?1 AND message_id IS NOT NULL", [path])?;
        Ok(())
    }

    // ── Recordatorios ──────────────────────────────────────────────────────

    pub fn create_reminder(&self, reminder: NewReminder) -> Result<i64> {
        let title = non_empty(&reminder.title, &tr("El recordatorio necesita un título"))?;
        let conn = self.conn();
        conn.execute(
            "INSERT INTO reminders (title, body, due_at, anchor_at, repeat, conversation_id, created)
             VALUES (?1, ?2, ?3, ?3, ?4, ?5, ?6)",
            params![title, reminder.body, reminder.due_at, reminder.repeat.as_str(), reminder.conversation_id, now()],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn get_reminder(&self, id: i64) -> Result<Reminder> {
        let conn = self.conn();
        let reminder = conn
            .query_row(&format!("{REMINDER_SELECT} WHERE id = ?1"), [id], reminder_from_row)
            .optional()?
            .with_context(|| missing(Kind::Reminder, id))?;
        Ok(Reminder { attachments: Self::attachments_of(&conn, Owner::Reminder(id))?, ..reminder })
    }

    pub fn update_reminder(&self, id: i64, update: ReminderUpdate) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let exists: bool = tx.query_row("SELECT count(*) FROM reminders WHERE id = ?1", [id], |r| r.get::<_, i64>(0))? > 0;
        if !exists {
            bail!(missing(Kind::Reminder, id));
        }
        if let Some(title) = update.title {
            tx.execute("UPDATE reminders SET title = ?1 WHERE id = ?2", params![non_empty(&title, &tr("El título no puede estar vacío"))?, id])?;
        }
        if let Some(body) = update.body {
            tx.execute("UPDATE reminders SET body = ?1 WHERE id = ?2", params![body, id])?;
        }
        if let Some(due_at) = update.due_at {
            tx.execute(
                "UPDATE reminders SET due_at = ?1, anchor_at = ?1, notified = 0, done = 0, completed = NULL WHERE id = ?2",
                params![due_at, id],
            )?;
        }
        if let Some(repeat) = update.repeat {
            tx.execute("UPDATE reminders SET repeat = ?1 WHERE id = ?2", params![repeat.as_str(), id])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Hecho: si se repite, pasa a la siguiente fecha; si no, a «Completados».
    pub fn complete_reminder(&self, id: i64) -> Result<Reminder> {
        let reminder = self.get_reminder(id)?;
        let conn = self.conn();
        match crate::reminders::next_occurrence(reminder.anchor_at, reminder.repeat, now()) {
            Some(next) => conn.execute("UPDATE reminders SET due_at = ?1, notified = 0 WHERE id = ?2", params![next, id])?,
            None => conn.execute("UPDATE reminders SET done = 1, completed = ?1 WHERE id = ?2", params![now(), id])?,
        };
        drop(conn);
        self.get_reminder(id)
    }

    pub fn reopen_reminder(&self, id: i64) -> Result<()> {
        let changed = self.conn().execute("UPDATE reminders SET done = 0, completed = NULL WHERE id = ?1", [id])?;
        expect_changed(changed, Kind::Reminder, id)
    }

    /// Vuelve a avisar en `until` sin cambiar la base de las repeticiones.
    pub fn snooze_reminder(&self, id: i64, until: i64) -> Result<()> {
        let changed = self
            .conn()
            .execute("UPDATE reminders SET due_at = ?1, notified = 0, done = 0 WHERE id = ?2", params![until, id])?;
        expect_changed(changed, Kind::Reminder, id)
    }

    pub fn mark_reminder_notified(&self, id: i64) -> Result<()> {
        self.conn().execute("UPDATE reminders SET notified = 1 WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn delete_reminder(&self, id: i64) -> Result<()> {
        let changed = self.conn().execute("DELETE FROM reminders WHERE id = ?1", [id])?;
        expect_changed(changed, Kind::Reminder, id)
    }

    /// Pendientes (por fecha) o completados (los últimos primero), con sus adjuntos.
    pub fn list_reminders(&self, done: bool) -> Result<Vec<Reminder>> {
        let conn = self.conn();
        let order = if done { "completed DESC" } else { "due_at" };
        let mut stmt = conn.prepare(&format!("{REMINDER_SELECT} WHERE done = ?1 ORDER BY {order}"))?;
        let rows = stmt.query_map([done], reminder_from_row)?;
        let mut reminders: Vec<Reminder> = rows.collect::<rusqlite::Result<_>>()?;
        for reminder in &mut reminders {
            reminder.attachments = Self::attachments_of(&conn, Owner::Reminder(reminder.id))?;
        }
        Ok(reminders)
    }

    /// Recordatorios cuyo aviso ha llegado y aún no se ha mostrado.
    pub fn reminders_due(&self, now: i64) -> Result<Vec<Reminder>> {
        let ids: Vec<i64> = {
            let conn = self.conn();
            let mut stmt = conn.prepare("SELECT id FROM reminders WHERE done = 0 AND notified = 0 AND due_at <= ?1")?;
            let rows = stmt.query_map([now], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        ids.into_iter().map(|id| self.get_reminder(id)).collect()
    }
}

const REMINDER_SELECT: &str = "SELECT id, title, body, due_at, anchor_at, repeat, done, notified, created, completed FROM reminders";

fn reminder_from_row(r: &Row) -> rusqlite::Result<Reminder> {
    Ok(Reminder {
        id: r.get(0)?,
        title: r.get(1)?,
        body: r.get(2)?,
        due_at: r.get(3)?,
        anchor_at: r.get(4)?,
        repeat: Repeat::parse(&r.get::<_, String>(5)?).unwrap_or_default(),
        done: r.get(6)?,
        notified: r.get(7)?,
        attachments: Vec::new(),
        created: r.get(8)?,
        completed: r.get(9)?,
    })
}

fn attachment_from_row(r: &Row) -> rusqlite::Result<Attachment> {
    Ok(Attachment {
        id: r.get(0)?,
        kind: AttachmentKind::parse(&r.get::<_, String>(1)?),
        path: r.get(2)?,
        mime: r.get(3)?,
        transcript: r.get(4)?,
        created: r.get(5)?,
    })
}

const TODO_SELECT: &str = "SELECT t.id, t.list_id, l.name, t.text, t.done, t.due, t.priority, t.reminder_at,
       t.created, t.completed FROM todos t JOIN todo_lists l ON l.id = t.list_id";

fn todo_from_row(r: &Row) -> rusqlite::Result<Todo> {
    Ok(Todo {
        id: r.get(0)?,
        list_id: r.get(1)?,
        list: r.get(2)?,
        text: r.get(3)?,
        done: r.get(4)?,
        due: r.get(5)?,
        priority: Priority::from_db(r.get(6)?),
        reminder_at: r.get(7)?,
        created: r.get(8)?,
        completed: r.get(9)?,
    })
}

fn non_empty<'a>(text: &'a str, message: &str) -> Result<&'a str> {
    let text = text.trim();
    if text.is_empty() {
        bail!("{message}");
    }
    Ok(text)
}

#[derive(Clone, Copy)]
enum Kind {
    Notebook,
    Note,
    List,
    Todo,
    Conversation,
    Reminder,
}

fn missing(kind: Kind, id: i64) -> String {
    let message = match kind {
        Kind::Notebook => tr("No existe el cuaderno {id}"),
        Kind::Note => tr("No existe la nota {id}"),
        Kind::List => tr("No existe la lista {id}"),
        Kind::Todo => tr("No existe la tarea {id}"),
        Kind::Conversation => tr("No existe la conversación {id}"),
        Kind::Reminder => tr("No existe el recordatorio {id}"),
    };
    message.replace("{id}", &id.to_string())
}

fn expect_changed(changed: usize, kind: Kind, id: i64) -> Result<()> {
    if changed == 0 {
        bail!(missing(kind, id));
    }
    Ok(())
}

/// Acepta 'YYYY-MM-DD', 'YYYY-MM-DD HH:MM' o 'YYYY-MM-DDTHH:MM[:SS]'.
pub fn normalize_due(text: &str) -> Result<String> {
    let text = text.trim();
    if let Ok(date) = chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d") {
        return Ok(date.format("%Y-%m-%d").to_string());
    }
    for format in ["%Y-%m-%d %H:%M", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S"] {
        if let Ok(datetime) = chrono::NaiveDateTime::parse_from_str(text, format) {
            return Ok(datetime.format("%Y-%m-%d %H:%M").to_string());
        }
    }
    bail!(trf("Fecha no válida: «{date}» (usa YYYY-MM-DD o YYYY-MM-DD HH:MM)", &[("date", &text)]))
}

/// Convierte texto libre en una consulta FTS5: cada palabra como prefijo, todas obligatorias.
fn fts_query(text: &str) -> Option<String> {
    let terms: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{w}\"*"))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_crud_and_search() -> Result<()> {
        let store = Store::in_memory()?;
        let id = store.create_note(NewNote {
            title: "Reunión con Ana".into(),
            content: "Hablar del presupuesto de diseño".into(),
            notebook: Some("Trabajo".into()),
            tags: vec!["#reuniones".into(), "ana".into()],
            ..Default::default()
        })?;
        let note = store.get_note(id)?;
        assert_eq!(note.notebook.as_deref(), Some("Trabajo"));
        assert_eq!(note.tags, ["ana", "reuniones"]);

        // Sin tildes y por prefijo.
        let found = store.search_notes(&NoteFilter { query: Some("reunion presu".into()), ..Default::default() })?;
        assert_eq!(found.len(), 1);

        store.update_note(id, NoteUpdate { append: Some("Y del calendario".into()), tags: Some(vec!["ana".into()]), ..Default::default() })?;
        let note = store.get_note(id)?;
        assert!(note.content.ends_with("diseño\n\nY del calendario"));
        assert_eq!(store.list_tags()?, [("ana".to_owned(), 1)]);
        let found = store.search_notes(&NoteFilter { query: Some("calendario".into()), ..Default::default() })?;
        assert_eq!(found.len(), 1);

        store.delete_note(id)?;
        assert!(store.get_note(id).is_err());
        assert!(store.list_tags()?.is_empty());
        assert_eq!(store.list_notebooks()?[0].notes, 0);
        Ok(())
    }

    #[test]
    fn todos_filters() -> Result<()> {
        let store = Store::in_memory()?;
        let bread = store.add_todo(NewTodo { list: "Compra".into(), text: "Pan".into(), due: Some("2026-10-10".into()), ..Default::default() })?;
        store.add_todo(NewTodo {
            list: "compra".into(),
            text: "Leche".into(),
            due: Some("2026-10-10T18:30".into()),
            priority: Some(Priority::High),
            ..Default::default()
        })?;
        store.add_todo(NewTodo { list: "Trabajo".into(), text: "Informe".into(), due: Some("2026-10-12".into()), ..Default::default() })?;

        let lists = store.list_todo_lists()?;
        assert_eq!(lists.len(), 2, "«compra» reutiliza la lista «Compra»");

        let day = store.list_todos(&TodoFilter {
            due_from: Some("2026-10-10".into()),
            due_to: Some("2026-10-10".into()),
            ..Default::default()
        })?;
        assert_eq!(day.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(), ["Pan", "Leche"]);

        store.set_todo_done(bread, true)?;
        let pending = store.list_todos(&TodoFilter { list: Some("Compra".into()), ..Default::default() })?;
        assert_eq!(pending.len(), 1);
        let high = store.list_todos(&TodoFilter { min_priority: Some(Priority::Medium), status: TodoStatus::All, ..Default::default() })?;
        assert_eq!(high[0].text, "Leche");

        store.delete_todo_list(lists[0].id)?;
        assert_eq!(store.list_todos(&TodoFilter { status: TodoStatus::All, ..Default::default() })?.len(), 1);
        Ok(())
    }

    #[test]
    fn conversations_roundtrip() -> Result<()> {
        let store = Store::in_memory()?;
        let id = store.create_conversation("Hola", "anthropic", "claude-opus-5-5", "sys")?;
        store.add_message(id, &MessageRecord::new(MessageKind::User, "Hola"))?;
        store.save_native(id, &[serde_json::json!({"role": "user", "content": "Hola"})])?;
        let conversation = store.get_conversation(id)?;
        assert_eq!(conversation.native.len(), 1);
        assert_eq!(store.messages(id)?.len(), 1);
        store.delete_conversation(id)?;
        assert!(store.list_conversations(10)?.is_empty());
        Ok(())
    }

    #[test]
    fn due_dates() {
        assert_eq!(normalize_due("2026-10-09").unwrap(), "2026-10-09");
        assert_eq!(normalize_due("2026-10-09T08:05:00").unwrap(), "2026-10-09 08:05");
        assert!(normalize_due("mañana").is_err());
    }
}
