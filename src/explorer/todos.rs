//! Listas de tareas: marcar, añadir, editar y borrar.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use super::{Ctx, clear_list, local_time, placeholder, select_by_name, sidebar, split, toolbar_box};
use crate::store::{Priority, Todo, TodoFilter, TodoStatus, TodoUpdate};
use crate::i18n::{n_, tr, trf};

const PRIORITIES: [&str; 4] = [n_("Ninguna"), n_("Baja"), n_("Media"), n_("Alta")];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Selection {
    /// Pendientes de todas las listas.
    AllPending,
    List(i64),
}

pub struct Page {
    ctx: Ctx,
    root: gtk::Paned,
    lists: gtk::ListBox,
    detail: gtk::Stack,
    title: gtk::Label,
    show_done: gtk::ToggleButton,
    rename: gtk::Button,
    delete: gtk::Button,
    add_entry: gtk::Entry,
    todos: gtk::ListBox,
    selection: Cell<Option<Selection>>,
    refreshing: Cell<bool>,
}

impl Page {
    pub fn new(ctx: Ctx) -> Rc<Self> {
        let heading = gtk::Label::builder().label(tr("Listas")).xalign(0.0).hexpand(true).margin_start(6).css_classes(["heading"]).build();
        let new_list = gtk::Button::builder().icon_name("list-add-symbolic").tooltip_text(tr("Nueva lista")).build();
        let tools = toolbar_box();
        tools.append(&heading);
        tools.append(&new_list);
        let lists = gtk::ListBox::new();
        let left = sidebar(&tools, &lists);

        let title = gtk::Label::builder().xalign(0.0).hexpand(true).css_classes(["title-3"]).build();
        let show_done = gtk::ToggleButton::builder()
            .icon_name("object-select-symbolic")
            .tooltip_text(tr("Mostrar las completadas"))
            .valign(gtk::Align::Center)
            .build();
        let rename = gtk::Button::builder().icon_name("document-edit-symbolic").tooltip_text(tr("Renombrar la lista")).valign(gtk::Align::Center).build();
        let delete = gtk::Button::builder().icon_name("user-trash-symbolic").tooltip_text(tr("Borrar la lista")).valign(gtk::Align::Center).build();
        let header = toolbar_box();
        header.set_margin_start(12);
        header.append(&title);
        header.append(&show_done);
        header.append(&rename);
        header.append(&delete);

        let add_entry = gtk::Entry::builder()
            .placeholder_text(tr("Añadir tarea…"))
            .primary_icon_name("list-add-symbolic")
            .build();
        let todos = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();
        let body = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(12)
            .margin_bottom(24)
            .margin_start(12)
            .margin_end(12)
            .build();
        body.append(&add_entry);
        body.append(&todos);
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&adw::Clamp::builder().maximum_size(800).child(&body).build())
            .build();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&header);
        content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        content.append(&scroller);

        let detail = gtk::Stack::new();
        detail.add_named(&placeholder("object-select-symbolic", &tr("Sin lista"), &tr("Elige una lista o crea una nueva.")), Some("empty"));
        detail.add_named(&content, Some("detail"));

        let root = split(&left, &detail);
        let this = Rc::new(Self {
            ctx,
            root,
            lists,
            detail,
            title,
            show_done,
            rename,
            delete,
            add_entry,
            todos,
            selection: Cell::new(Some(Selection::AllPending)),
            refreshing: Cell::new(false),
        });

        this.lists.connect_row_selected(glib::clone!(
            #[weak]
            this,
            move |_, row| {
                if this.refreshing.get() {
                    return;
                }
                let selection = row.and_then(|r| parse_selection(&r.widget_name()));
                this.selection.set(selection);
                this.show();
            }
        ));
        new_list.connect_clicked(glib::clone!(
            #[weak]
            this,
            move |_| {
                let page = this.clone();
                this.ctx.ask_text(&tr("Nueva lista"), "", &tr("Crear"), move |name| {
                    if let Some(id) = page.ctx.check(page.ctx.store.create_todo_list(&name)) {
                        page.selection.set(Some(Selection::List(id)));
                        page.refresh();
                    }
                });
            }
        ));
        this.show_done.connect_toggled(glib::clone!(
            #[weak]
            this,
            move |_| this.show()
        ));
        this.rename.connect_clicked(glib::clone!(
            #[weak]
            this,
            move |_| this.rename_list()
        ));
        this.delete.connect_clicked(glib::clone!(
            #[weak]
            this,
            move |_| this.delete_list()
        ));
        this.add_entry.connect_activate(glib::clone!(
            #[weak]
            this,
            move |entry| {
                let Some(Selection::List(id)) = this.selection.get() else { return };
                let text = entry.text().trim().to_owned();
                if text.is_empty() {
                    return;
                }
                let Some(list) = this.ctx.check(this.ctx.store.todo_list(id)) else { return };
                let todo = crate::store::NewTodo { list: list.name, text, ..Default::default() };
                if this.ctx.check(this.ctx.store.add_todo(todo)).is_some() {
                    entry.set_text("");
                    this.refresh();
                }
            }
        ));
        this
    }

    pub fn widget(&self) -> &gtk::Paned {
        &self.root
    }

    pub fn list(&self) -> &gtk::ListBox {
        &self.lists
    }

    pub fn refresh(self: &Rc<Self>) {
        let Some(lists) = self.ctx.check(self.ctx.store.list_todo_lists()) else { return };
        let pending: i64 = lists.iter().map(|l| l.pending).sum();
        self.refreshing.set(true);
        clear_list(&self.lists);
        self.lists.append(&list_row("all", &tr("Todas las pendientes"), pending, "view-list-symbolic"));
        for list in &lists {
            self.lists.append(&list_row(&format!("list-{}", list.id), &list.name, list.pending, "checkbox-checked-symbolic"));
        }
        let name = match self.selection.get() {
            Some(Selection::AllPending) => Some("all".to_owned()),
            Some(Selection::List(id)) => Some(format!("list-{id}")),
            None => None,
        };
        if !name.is_some_and(|n| select_by_name(&self.lists, &n)) {
            self.selection.set(None);
        }
        self.refreshing.set(false);
        self.show();
    }

    /// Muestra las tareas de la selección actual.
    fn show(self: &Rc<Self>) {
        let Some(selection) = self.selection.get() else {
            self.detail.set_visible_child_name("empty");
            return;
        };
        let is_list = matches!(selection, Selection::List(_));
        let (title, filter) = match selection {
            Selection::AllPending => (tr("Todas las pendientes"), TodoFilter::default()),
            Selection::List(id) => {
                let Some(list) = self.ctx.check(self.ctx.store.todo_list(id)) else { return };
                let status = if self.show_done.is_active() { TodoStatus::All } else { TodoStatus::Pending };
                (list.name.clone(), TodoFilter { list: Some(list.name), status, ..Default::default() })
            }
        };
        self.title.set_text(&title);
        self.show_done.set_visible(is_list);
        self.rename.set_visible(is_list);
        self.delete.set_visible(is_list);
        self.add_entry.set_visible(is_list);

        let Some(todos) = self.ctx.check(self.ctx.store.list_todos(&filter)) else { return };
        clear_list(&self.todos);
        if todos.is_empty() {
            let row = adw::ActionRow::builder().title(tr("No hay tareas")).sensitive(false).build();
            self.todos.append(&row);
        }
        for todo in &todos {
            self.todos.append(&self.todo_row(todo, !is_list));
        }
        self.detail.set_visible_child_name("detail");
    }

    fn todo_row(self: &Rc<Self>, todo: &Todo, show_list: bool) -> adw::ActionRow {
        let row = adw::ActionRow::builder().use_markup(false).title(&todo.text).build();
        row.set_title_lines(3);
        let subtitle = todo_subtitle(todo, show_list);
        if !subtitle.is_empty() {
            row.set_subtitle(&subtitle);
        }
        if todo.done {
            row.add_css_class("dim-label");
        }

        let check = gtk::CheckButton::builder().active(todo.done).valign(gtk::Align::Center).build();
        row.add_prefix(&check);
        row.set_activatable_widget(Some(&check));
        let id = todo.id;
        check.connect_toggled(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |check| {
                if this.ctx.check(this.ctx.store.set_todo_done(id, check.is_active())).is_some() {
                    // Fuera del manejador: la fila se va a destruir al refrescar.
                    glib::idle_add_local_once(move || this.refresh());
                }
            }
        ));

        let edit = gtk::Button::builder().icon_name("document-edit-symbolic").tooltip_text(tr("Editar")).valign(gtk::Align::Center).css_classes(["flat"]).build();
        edit.connect_clicked(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.edit_todo(id)
        ));
        let delete = gtk::Button::builder().icon_name("user-trash-symbolic").tooltip_text(tr("Borrar")).valign(gtk::Align::Center).css_classes(["flat"]).build();
        let text = todo.text.clone();
        delete.connect_clicked(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| {
                if this.ctx.check(this.ctx.store.delete_todo(id)).is_some() {
                    this.ctx.toast(&trf("Tarea borrada: {text}", &[("text", &text)]));
                    this.refresh();
                }
            }
        ));
        row.add_suffix(&edit);
        row.add_suffix(&delete);
        row
    }

    fn edit_todo(self: &Rc<Self>, id: i64) {
        let Some(todo) = self.ctx.check(self.ctx.store.get_todo(id)) else { return };
        let Some(lists) = self.ctx.check(self.ctx.store.list_todo_lists()) else { return };

        let text = adw::EntryRow::builder().title(tr("Texto")).text(&todo.text).build();
        let due = adw::EntryRow::builder().title(tr("Fecha límite (AAAA-MM-DD [HH:MM])")).text(todo.due.as_deref().unwrap_or_default()).build();
        let priority = adw::ComboRow::builder().title(tr("Prioridad")).model(&gtk::StringList::new(&PRIORITIES.map(tr).iter().map(String::as_str).collect::<Vec<_>>())).build();
        priority.set_selected(todo.priority.to_db() as u32);
        let reminder = adw::EntryRow::builder()
            .title(tr("Aviso (AAAA-MM-DD HH:MM)"))
            .text(todo.reminder_at.map(local_time).unwrap_or_default())
            .build();
        let names: Vec<&str> = lists.iter().map(|l| l.name.as_str()).collect();
        let list = adw::ComboRow::builder().title(tr("Lista")).model(&gtk::StringList::new(&names)).build();
        list.set_selected(lists.iter().position(|l| l.id == todo.list_id).unwrap_or(0) as u32);

        let group = adw::PreferencesGroup::new();
        for row in [text.upcast_ref::<gtk::Widget>(), due.upcast_ref(), priority.upcast_ref(), reminder.upcast_ref(), list.upcast_ref()] {
            group.add(row);
        }
        let page = adw::PreferencesPage::new();
        page.add(&group);

        let cancel = gtk::Button::with_label(&tr("Cancelar"));
        let save = gtk::Button::builder().label(tr("Guardar")).css_classes(["suggested-action"]).build();
        let header = adw::HeaderBar::builder().show_end_title_buttons(false).show_start_title_buttons(false).build();
        header.pack_start(&cancel);
        header.pack_end(&save);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&page));
        let dialog = adw::Dialog::builder().title(tr("Editar tarea")).content_width(460).child(&toolbar).build();

        cancel.connect_clicked(glib::clone!(
            #[weak]
            dialog,
            move |_| {
                dialog.close();
            }
        ));
        save.connect_clicked(glib::clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            dialog,
            move |_| {
                let empty_to_none = |s: String| (!s.trim().is_empty()).then_some(s);
                let reminder_at = match empty_to_none(reminder.text().to_string()) {
                    None => Ok(None),
                    Some(r) => crate::tools::parse_reminder(&r).map(Some),
                };
                let Some(reminder_at) = this.ctx.check(reminder_at) else { return };
                let update = TodoUpdate {
                    text: Some(text.text().to_string()),
                    due: Some(empty_to_none(due.text().to_string())),
                    priority: Some(Priority::from_db(priority.selected().into())),
                    reminder_at: Some(reminder_at),
                    list: names_at(&list),
                };
                if this.ctx.check(this.ctx.store.update_todo(id, update)).is_some() {
                    dialog.close();
                    this.refresh();
                }
            }
        ));
        dialog.present(Some(&self.ctx.window));
    }

    fn rename_list(self: &Rc<Self>) {
        let Some(Selection::List(id)) = self.selection.get() else { return };
        let Some(list) = self.ctx.check(self.ctx.store.todo_list(id)) else { return };
        let this = self.clone();
        self.ctx.ask_text(&tr("Renombrar lista"), &list.name, &tr("Renombrar"), move |name| {
            if this.ctx.check(this.ctx.store.rename_todo_list(id, &name)).is_some() {
                this.refresh();
            }
        });
    }

    fn delete_list(self: &Rc<Self>) {
        let Some(Selection::List(id)) = self.selection.get() else { return };
        let Some(list) = self.ctx.check(self.ctx.store.todo_list(id)) else { return };
        let this = self.clone();
        self.ctx.confirm(
            &trf("¿Borrar la lista «{name}»?", &[("name", &list.name)]),
            &trf("Se borrarán también sus {count} tareas.", &[("count", &list.total)]),
            &tr("Borrar"),
            move || {
                if this.ctx.check(this.ctx.store.delete_todo_list(id)).is_some() {
                    this.selection.set(Some(Selection::AllPending));
                    this.refresh();
                }
            },
        );
    }
}

fn parse_selection(name: &str) -> Option<Selection> {
    match name {
        "all" => Some(Selection::AllPending),
        _ => name.strip_prefix("list-")?.parse().ok().map(Selection::List),
    }
}

fn list_row(name: &str, title: &str, pending: i64, icon: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder().use_markup(false).title(title).activatable(true).build();
    row.set_widget_name(name);
    row.add_prefix(&gtk::Image::from_icon_name(icon));
    if pending > 0 {
        row.add_suffix(&gtk::Label::builder().label(pending.to_string()).css_classes(["dim-label", "numeric"]).build());
    }
    row
}

fn names_at(combo: &adw::ComboRow) -> Option<String> {
    combo.selected_item().and_downcast::<gtk::StringObject>().map(|s| s.string().to_string())
}

fn todo_subtitle(todo: &Todo, show_list: bool) -> String {
    let mut parts = Vec::new();
    if let Some(due) = &todo.due {
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let overdue = !todo.done && due.get(..10).is_some_and(|d| d < today.as_str());
        parts.push(if overdue { trf("⚠ Vencida: {date}", &[("date", due)]) } else { format!("📅 {due}") });
    }
    match todo.priority {
        Priority::High => parts.push(tr("Prioridad alta")),
        Priority::Medium => parts.push(tr("Prioridad media")),
        Priority::Low => parts.push(tr("Prioridad baja")),
        Priority::None => {}
    }
    if let Some(reminder) = todo.reminder_at {
        parts.push(format!("🔔 {}", local_time(reminder)));
    }
    if show_list {
        parts.push(todo.list.clone());
    }
    parts.join(" · ")
}
