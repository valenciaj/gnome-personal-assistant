//! Ventana principal: explorar y editar conversaciones, notas y tareas.

mod conversations;
mod notes;
mod todos;

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::app::Command;
use crate::store::Store;
use crate::i18n::tr;

/// Lo que comparten las páginas del explorador.
#[derive(Clone)]
pub struct Ctx {
    pub store: Store,
    pub commands: async_channel::Sender<Command>,
    pub window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
}

impl Ctx {
    pub fn toast(&self, message: &str) {
        self.toasts.add_toast(adw::Toast::new(message));
    }

    /// Muestra el error, si lo hay, como aviso en la ventana.
    pub fn check<T>(&self, result: anyhow::Result<T>) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(err) => {
                log::warn!("{err:#}");
                self.toast(&format!("{err:#}"));
                None
            }
        }
    }

    pub fn send(&self, command: Command) {
        let _ = self.commands.try_send(command);
    }

    /// Diálogo de confirmación para acciones destructivas.
    pub fn confirm(&self, heading: &str, body: &str, action: &str, on_confirm: impl Fn() + 'static) {
        let dialog = adw::AlertDialog::new(Some(heading), Some(body));
        dialog.add_response("cancel", &tr("Cancelar"));
        dialog.add_response("confirm", action);
        dialog.set_response_appearance("confirm", adw::ResponseAppearance::Destructive);
        dialog.set_close_response("cancel");
        dialog.connect_response(Some("confirm"), move |_, _| on_confirm());
        dialog.present(Some(&self.window));
    }

    /// Pide un texto (nombre de cuaderno, lista…).
    pub fn ask_text(&self, heading: &str, initial: &str, action: &str, on_accept: impl Fn(String) + 'static) {
        let dialog = adw::AlertDialog::new(Some(heading), None);
        let entry = gtk::Entry::builder().text(initial).activates_default(true).build();
        dialog.set_extra_child(Some(&entry));
        dialog.add_response("cancel", &tr("Cancelar"));
        dialog.add_response("accept", action);
        dialog.set_response_appearance("accept", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("accept"));
        dialog.set_close_response("cancel");
        dialog.connect_response(Some("accept"), move |_, _| {
            let text = entry.text().trim().to_owned();
            if !text.is_empty() {
                on_accept(text);
            }
        });
        dialog.present(Some(&self.window));
    }
}

pub struct Explorer {
    window: adw::ApplicationWindow,
    stack: adw::ViewStack,
    conversations: Rc<conversations::Page>,
    notes: Rc<notes::Page>,
    todos: Rc<todos::Page>,
}

impl Explorer {
    pub fn new(app: &adw::Application, store: Store, commands: async_channel::Sender<Command>) -> Rc<Self> {
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title(tr("Asistente personal"))
            .default_width(1100)
            .default_height(720)
            .width_request(720)
            .height_request(480)
            .hide_on_close(true)
            .build();
        let toasts = adw::ToastOverlay::new();
        let ctx = Ctx { store, commands, window: window.clone(), toasts: toasts.clone() };

        let conversations = conversations::Page::new(ctx.clone());
        let notes = notes::Page::new(ctx.clone());
        let todos = todos::Page::new(ctx.clone());

        let stack = adw::ViewStack::new();
        stack.add_titled_with_icon(notes.widget(), Some("notes"), &tr("Notas"), "accessories-text-editor-symbolic");
        stack.add_titled_with_icon(todos.widget(), Some("todos"), &tr("Tareas"), "object-select-symbolic");
        stack.add_titled_with_icon(
            conversations.widget(),
            Some("conversations"),
            &tr("Conversaciones"),
            "user-available-symbolic",
        );

        let switcher = adw::ViewSwitcher::builder().stack(&stack).policy(adw::ViewSwitcherPolicy::Wide).build();
        let header = adw::HeaderBar::builder().title_widget(&switcher).build();
        let preferences = gtk::Button::builder().icon_name("preferences-system-symbolic").tooltip_text(tr("Preferencias")).build();
        let commands = ctx.commands.clone();
        preferences.connect_clicked(move |_| {
            let _ = commands.try_send(Command::ShowPreferences);
        });
        header.pack_end(&preferences);

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&stack));
        toasts.set_child(Some(&toolbar));
        window.set_content(Some(&toasts));

        let this = Rc::new(Self { window, stack, conversations, notes, todos });
        // Al volver a la ventana, recoger lo que haya cambiado la IA mientras tanto.
        this.window.connect_is_active_notify(glib::clone!(
            #[weak]
            this,
            move |window| {
                if window.is_active() {
                    this.refresh();
                }
            }
        ));
        this
    }

    pub fn present(&self) {
        self.refresh();
        self.window.present();
    }

    pub fn window(&self) -> &adw::ApplicationWindow {
        &self.window
    }

    /// Cambia de pestaña («notes», «todos», «conversations») y elige el primer elemento.
    pub fn show_page(&self, name: &str) {
        self.stack.set_visible_child_name(name);
        let list = match name {
            "notes" => self.notes.list(),
            "todos" => self.todos.list(),
            _ => self.conversations.list(),
        };
        if let Some(row) = list.row_at_index(if name == "todos" { 1 } else { 0 }) {
            list.select_row(Some(&row));
        }
    }

    /// Abre en edición la nota seleccionada (para las capturas de desarrollo).
    pub fn edit_selected_note(&self) {
        self.notes.edit_selected();
    }

    pub fn refresh(&self) {
        if !self.window.is_visible() {
            return;
        }
        self.conversations.refresh();
        self.notes.refresh();
        self.todos.refresh();
    }
}

/// Fecha y hora local legible de un timestamp unix.
pub fn local_time(unix: i64) -> String {
    chrono::DateTime::from_timestamp(unix, 0)
        .map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

/// Panel vacío para cuando no hay nada seleccionado.
fn placeholder(icon: &str, title: &str, description: &str) -> adw::StatusPage {
    adw::StatusPage::builder().icon_name(icon).title(title).description(description).build()
}

/// Lista lateral con su barra de herramientas encima.
fn sidebar(toolbar: &impl IsA<gtk::Widget>, list: &gtk::ListBox) -> gtk::Box {
    list.add_css_class("navigation-sidebar");
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(list)
        .build();
    let pane = gtk::Box::builder().orientation(gtk::Orientation::Vertical).width_request(300).build();
    pane.append(toolbar);
    pane.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    pane.append(&scroller);
    pane
}

/// Dos paneles: lista a la izquierda, detalle a la derecha.
fn split(left: &impl IsA<gtk::Widget>, right: &impl IsA<gtk::Widget>) -> gtk::Paned {
    gtk::Paned::builder()
        .start_child(left)
        .end_child(right)
        .resize_start_child(false)
        .shrink_start_child(false)
        .shrink_end_child(false)
        .position(320)
        .build()
}

/// Fila de lista con título y subtítulo en texto plano (sin interpretar marcado).
fn text_row(title: &str, subtitle: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder().use_markup(false).title(title).subtitle(subtitle).activatable(true).build();
    row.set_title_lines(2);
    row.set_subtitle_lines(3);
    row
}

fn toolbar_box() -> gtk::Box {
    gtk::Box::builder().spacing(6).margin_top(6).margin_bottom(6).margin_start(6).margin_end(6).build()
}

/// Selecciona la fila cuyo nombre (`widget_name`) es `id`.
fn select_by_name(list: &gtk::ListBox, id: &str) -> bool {
    let mut child = list.first_child();
    while let Some(widget) = child {
        if let Some(row) = widget.downcast_ref::<gtk::ListBoxRow>()
            && row.widget_name() == id
        {
            list.select_row(Some(row));
            return true;
        }
        child = widget.next_sibling();
    }
    false
}

fn clear_list(list: &gtk::ListBox) {
    list.remove_all();
}
