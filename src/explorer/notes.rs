//! Notas: buscar, filtrar por cuaderno o etiqueta, leer, escuchar y editar.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use sourceview5::prelude::*;

use super::{Ctx, clear_list, local_time, placeholder, select_by_name, sidebar, split, text_row, toolbar_box};
use crate::conversation_view::markdown_label;
use crate::store::{NewNote, NoteFilter, NoteUpdate};

const ALL_NOTEBOOKS: &str = "Todos los cuadernos";
const ALL_TAGS: &str = "Todas las etiquetas";

/// Qué muestra el panel derecho.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Empty,
    View(i64),
    /// `None` = nota nueva.
    Edit(Option<i64>),
}

pub struct Page {
    ctx: Ctx,
    root: gtk::Paned,
    search: gtk::SearchEntry,
    notebooks: gtk::DropDown,
    tags: gtk::DropDown,
    list: gtk::ListBox,
    detail: gtk::Stack,
    // Vista
    view_title: gtk::Label,
    view_meta: gtk::Label,
    view_extra: gtk::Box,
    view_body: gtk::Box,
    // Edición
    edit_heading: gtk::Label,
    edit_title: gtk::Entry,
    edit_notebook: gtk::Entry,
    edit_tags: gtk::Entry,
    edit_buffer: sourceview5::Buffer,
    mode: Cell<Mode>,
    refreshing: Cell<bool>,
    actions: gio::SimpleActionGroup,
    /// Reproductor de la nota abierta, para pararlo al cambiar de nota.
    media: RefCell<Option<gtk::MediaFile>>,
}

impl Page {
    pub fn new(ctx: Ctx) -> Rc<Self> {
        // ── Panel izquierdo ──
        let search = gtk::SearchEntry::builder().placeholder_text("Buscar notas").hexpand(true).build();
        let new_note = gtk::Button::builder().icon_name("document-new-symbolic").tooltip_text("Nueva nota").build();
        let menu = gio::Menu::new();
        menu.append(Some("Nuevo cuaderno…"), Some("notes.new-notebook"));
        menu.append(Some("Renombrar cuaderno…"), Some("notes.rename-notebook"));
        menu.append(Some("Borrar cuaderno…"), Some("notes.delete-notebook"));
        let notebook_menu = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .tooltip_text("Cuadernos")
            .menu_model(&menu)
            .build();
        let row1 = toolbar_box();
        row1.append(&search);
        row1.append(&new_note);
        row1.append(&notebook_menu);

        let notebooks = gtk::DropDown::from_strings(&[ALL_NOTEBOOKS]);
        notebooks.set_hexpand(true);
        let tags = gtk::DropDown::from_strings(&[ALL_TAGS]);
        tags.set_hexpand(true);
        let row2 = toolbar_box();
        row2.set_margin_top(0);
        row2.set_homogeneous(true);
        row2.append(&notebooks);
        row2.append(&tags);

        let tools = gtk::Box::new(gtk::Orientation::Vertical, 0);
        tools.append(&row1);
        tools.append(&row2);
        let list = gtk::ListBox::new();
        let left = sidebar(&tools, &list);

        // ── Vista de una nota ──
        let view_title = gtk::Label::builder().xalign(0.0).wrap(true).hexpand(true).selectable(true).css_classes(["title-2"]).build();
        let edit = gtk::Button::builder().icon_name("document-edit-symbolic").tooltip_text("Editar").valign(gtk::Align::Center).build();
        let delete = gtk::Button::builder().icon_name("user-trash-symbolic").tooltip_text("Borrar").valign(gtk::Align::Center).build();
        let view_header = toolbar_box();
        view_header.set_margin_start(12);
        view_header.append(&view_title);
        view_header.append(&edit);
        view_header.append(&delete);
        let view_meta = gtk::Label::builder().xalign(0.0).wrap(true).css_classes(["dim-label", "caption"]).build();
        let view_extra = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).build();
        let view_body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let view_content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(12)
            .margin_bottom(24)
            .margin_start(12)
            .margin_end(12)
            .build();
        view_content.append(&view_meta);
        view_content.append(&view_extra);
        view_content.append(&view_body);
        let view_scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&adw::Clamp::builder().maximum_size(800).child(&view_content).build())
            .build();
        let view_page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        view_page.append(&view_header);
        view_page.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        view_page.append(&view_scroller);

        // ── Edición ──
        let edit_heading = gtk::Label::builder().xalign(0.0).hexpand(true).css_classes(["heading"]).build();
        let cancel = gtk::Button::builder().label("Cancelar").valign(gtk::Align::Center).build();
        let save = gtk::Button::builder().label("Guardar").css_classes(["suggested-action"]).valign(gtk::Align::Center).build();
        let edit_header = toolbar_box();
        edit_header.set_margin_start(12);
        edit_header.append(&edit_heading);
        edit_header.append(&cancel);
        edit_header.append(&save);
        let edit_title = gtk::Entry::builder().placeholder_text("Título").build();
        let edit_notebook = gtk::Entry::builder().placeholder_text("Cuaderno (opcional)").hexpand(true).build();
        let edit_tags = gtk::Entry::builder().placeholder_text("Etiquetas, separadas por comas").hexpand(true).build();
        let meta_row = gtk::Box::builder().spacing(6).homogeneous(true).build();
        meta_row.append(&edit_notebook);
        meta_row.append(&edit_tags);
        let edit_buffer = sourceview5::Buffer::new(None);
        if let Some(language) = sourceview5::LanguageManager::default().language("markdown") {
            edit_buffer.set_language(Some(&language));
        }
        follow_dark_mode(&edit_buffer);
        let editor = sourceview5::View::builder()
            .buffer(&edit_buffer)
            .wrap_mode(gtk::WrapMode::WordChar)
            .top_margin(8)
            .bottom_margin(8)
            .left_margin(8)
            .right_margin(8)
            .build();
        let editor_scroller = gtk::ScrolledWindow::builder().vexpand(true).child(&editor).css_classes(["card"]).build();
        let edit_content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        edit_content.append(&edit_title);
        edit_content.append(&meta_row);
        edit_content.append(&editor_scroller);
        let edit_page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        edit_page.append(&edit_header);
        edit_page.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        edit_page.append(&edit_content);

        let detail = gtk::Stack::new();
        detail.add_named(
            &placeholder("accessories-text-editor-symbolic", "Sin nota", "Elige una nota o crea una nueva."),
            Some("empty"),
        );
        detail.add_named(&view_page, Some("view"));
        detail.add_named(&edit_page, Some("edit"));

        let root = split(&left, &detail);
        let actions = gio::SimpleActionGroup::new();
        root.insert_action_group("notes", Some(&actions));

        let this = Rc::new(Self {
            ctx,
            root,
            search,
            notebooks,
            tags,
            list,
            detail,
            view_title,
            view_meta,
            view_extra,
            view_body,
            edit_heading,
            edit_title,
            edit_notebook,
            edit_tags,
            edit_buffer,
            mode: Cell::new(Mode::Empty),
            refreshing: Cell::new(false),
            actions,
            media: RefCell::new(None),
        });
        this.connect(&new_note, &edit, &delete, &cancel, &save);
        this
    }

    fn connect(
        self: &Rc<Self>,
        new_note: &gtk::Button,
        edit: &gtk::Button,
        delete: &gtk::Button,
        cancel: &gtk::Button,
        save: &gtk::Button,
    ) {
        let this = self;
        this.search.connect_search_changed(glib::clone!(
            #[weak]
            this,
            move |_| this.fill_list()
        ));
        for dropdown in [&this.notebooks, &this.tags] {
            dropdown.connect_selected_notify(glib::clone!(
                #[weak]
                this,
                move |_| {
                    if !this.refreshing.get() {
                        this.update_actions();
                        this.fill_list();
                    }
                }
            ));
        }
        this.list.connect_row_selected(glib::clone!(
            #[weak]
            this,
            move |_, row| {
                if this.refreshing.get() {
                    return;
                }
                if let Some(id) = row.and_then(|r| r.widget_name().parse().ok()) {
                    this.show(id);
                }
            }
        ));
        new_note.connect_clicked(glib::clone!(
            #[weak]
            this,
            move |_| this.start_editing(None)
        ));
        edit.connect_clicked(glib::clone!(
            #[weak]
            this,
            move |_| {
                if let Mode::View(id) = this.mode.get() {
                    this.start_editing(Some(id));
                }
            }
        ));
        delete.connect_clicked(glib::clone!(
            #[weak]
            this,
            move |_| this.delete_current()
        ));
        cancel.connect_clicked(glib::clone!(
            #[weak]
            this,
            move |_| match this.mode.get() {
                Mode::Edit(Some(id)) => this.show(id),
                _ => this.set_mode(Mode::Empty),
            }
        ));
        save.connect_clicked(glib::clone!(
            #[weak]
            this,
            move |_| this.save()
        ));

        let action = gio::SimpleAction::new("new-notebook", None);
        action.connect_activate(glib::clone!(
            #[weak]
            this,
            move |_, _| {
                let page = this.clone();
                this.ctx.ask_text("Nuevo cuaderno", "", "Crear", move |name| {
                    if page.ctx.check(page.ctx.store.create_notebook(&name)).is_some() {
                        page.refresh();
                    }
                });
            }
        ));
        this.actions.add_action(&action);
        let action = gio::SimpleAction::new("rename-notebook", None);
        action.connect_activate(glib::clone!(
            #[weak]
            this,
            move |_, _| this.rename_notebook()
        ));
        this.actions.add_action(&action);
        let action = gio::SimpleAction::new("delete-notebook", None);
        action.connect_activate(glib::clone!(
            #[weak]
            this,
            move |_, _| this.delete_notebook()
        ));
        this.actions.add_action(&action);
        this.update_actions();
    }

    pub fn widget(&self) -> &gtk::Paned {
        &self.root
    }

    pub fn list(&self) -> &gtk::ListBox {
        &self.list
    }

    pub fn refresh(&self) {
        self.refreshing.set(true);
        if let Some(notebooks) = self.ctx.check(self.ctx.store.list_notebooks()) {
            let names: Vec<String> = notebooks.into_iter().map(|n| n.name).collect();
            refill_dropdown(&self.notebooks, ALL_NOTEBOOKS, &names);
        }
        if let Some(tags) = self.ctx.check(self.ctx.store.list_tags()) {
            let names: Vec<String> = tags.into_iter().map(|(name, _)| name).collect();
            refill_dropdown(&self.tags, ALL_TAGS, &names);
        }
        self.refreshing.set(false);
        self.update_actions();
        self.fill_list();
        // No se pisa lo que se está editando.
        if let Mode::View(id) = self.mode.get() {
            self.show(id);
        }
    }

    fn fill_list(&self) {
        let filter = NoteFilter {
            query: Some(self.search.text().to_string()),
            notebook: dropdown_choice(&self.notebooks),
            tag: dropdown_choice(&self.tags),
            limit: Some(500),
        };
        let Some(notes) = self.ctx.check(self.ctx.store.search_notes(&filter)) else { return };
        self.refreshing.set(true);
        clear_list(&self.list);
        for note in &notes {
            let mut meta = vec![local_time(note.modified)];
            meta.extend(note.notebook.clone());
            meta.extend(note.tags.iter().map(|t| format!("#{t}")));
            if note.has_audio {
                meta.push("🎤".into());
            }
            let row = text_row(&note.title, &format!("{}\n{}", plain_snippet(&note.snippet), meta.join(" · ")));
            row.set_widget_name(&note.id.to_string());
            self.list.append(&row);
        }
        let current = match self.mode.get() {
            Mode::View(id) | Mode::Edit(Some(id)) => Some(id),
            _ => None,
        };
        if let Some(id) = current {
            select_by_name(&self.list, &id.to_string());
        }
        self.refreshing.set(false);
    }

    fn set_mode(&self, mode: Mode) {
        if !matches!(mode, Mode::View(_)) {
            self.stop_media();
        }
        self.mode.set(mode);
        self.detail.set_visible_child_name(match mode {
            Mode::Empty => "empty",
            Mode::View(_) => "view",
            Mode::Edit(_) => "edit",
        });
        if mode == Mode::Empty {
            self.list.unselect_all();
        }
    }

    fn stop_media(&self) {
        if let Some(media) = self.media.borrow_mut().take() {
            media.pause();
        }
    }

    fn show(&self, id: i64) {
        let Some(note) = self.ctx.check(self.ctx.store.get_note(id)) else {
            self.set_mode(Mode::Empty);
            return;
        };
        if self.mode.get() != Mode::View(id) {
            self.stop_media();
        }
        self.view_title.set_text(&note.title);
        let mut meta = Vec::new();
        meta.extend(note.notebook.clone().map(|n| format!("📓 {n}")));
        if !note.tags.is_empty() {
            meta.push(note.tags.iter().map(|t| format!("#{t}")).collect::<Vec<_>>().join(" "));
        }
        meta.push(format!("Creada {}", local_time(note.created)));
        if note.modified != note.created {
            meta.push(format!("modificada {}", local_time(note.modified)));
        }
        self.view_meta.set_text(&meta.join(" · "));

        // El audio solo se recrea al cambiar de nota, para no cortar la reproducción.
        if self.mode.get() != Mode::View(id) || self.view_extra.first_child().is_none() {
            while let Some(child) = self.view_extra.first_child() {
                self.view_extra.remove(&child);
            }
            if let Some(path) = note.audio_path.as_deref().filter(|p| std::path::Path::new(p).exists()) {
                let media = gtk::MediaFile::for_filename(path);
                self.view_extra.append(&gtk::MediaControls::new(Some(&media)));
                *self.media.borrow_mut() = Some(media);
            }
            if let Some(transcript) = &note.transcript {
                let label = crate::conversation_view::wrapped_label();
                label.set_text(transcript);
                label.add_css_class("dim-label");
                self.view_extra.append(&gtk::Expander::builder().label("Transcripción original").child(&label).build());
            }
        }

        while let Some(child) = self.view_body.first_child() {
            self.view_body.remove(&child);
        }
        let body = markdown_label(&note.content);
        body.add_css_class("note-body");
        self.view_body.append(&body);

        self.set_mode(Mode::View(id));
    }

    pub fn edit_selected(&self) {
        if let Mode::View(id) = self.mode.get() {
            self.start_editing(Some(id));
        }
    }

    fn start_editing(&self, id: Option<i64>) {
        match id {
            Some(id) => {
                let Some(note) = self.ctx.check(self.ctx.store.get_note(id)) else { return };
                self.edit_heading.set_text("Editar nota");
                self.edit_title.set_text(&note.title);
                self.edit_notebook.set_text(note.notebook.as_deref().unwrap_or_default());
                self.edit_tags.set_text(&note.tags.join(", "));
                self.edit_buffer.set_text(&note.content);
            }
            None => {
                self.list.unselect_all();
                self.edit_heading.set_text("Nueva nota");
                self.edit_title.set_text("");
                self.edit_notebook.set_text(&dropdown_choice(&self.notebooks).unwrap_or_default());
                self.edit_tags.set_text("");
                self.edit_buffer.set_text("");
            }
        }
        self.set_mode(Mode::Edit(id));
        self.edit_title.grab_focus();
    }

    fn save(&self) {
        let Mode::Edit(id) = self.mode.get() else { return };
        let title = self.edit_title.text().to_string();
        let notebook = self.edit_notebook.text().trim().to_owned();
        let tags: Vec<String> = self
            .edit_tags
            .text()
            .split(',')
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty())
            .collect();
        let (start, end) = self.edit_buffer.bounds();
        let content = self.edit_buffer.text(&start, &end, false).to_string();
        let store = &self.ctx.store;
        let saved = match id {
            Some(id) => store
                .update_note(
                    id,
                    NoteUpdate { title: Some(title), content: Some(content), notebook: Some(notebook), tags: Some(tags), append: None },
                )
                .map(|_| id),
            None => store.create_note(NewNote {
                title,
                content,
                notebook: (!notebook.is_empty()).then_some(notebook),
                tags,
                ..Default::default()
            }),
        };
        if let Some(id) = self.ctx.check(saved) {
            self.mode.set(Mode::View(id));
            self.refresh();
            self.show(id);
            self.ctx.toast("Nota guardada");
        }
    }

    fn delete_current(self: &Rc<Self>) {
        let Mode::View(id) = self.mode.get() else { return };
        let Some(note) = self.ctx.check(self.ctx.store.get_note(id)) else { return };
        let this = self.clone();
        self.ctx.confirm(
            &format!("¿Borrar «{}»?", note.title),
            "La nota y su grabación se borrarán definitivamente.",
            "Borrar",
            move || {
                this.stop_media();
                let store = &this.ctx.store;
                if let Some(audio) = this.ctx.check(store.delete_note(id)) {
                    if let Some(audio) = audio
                        && !store.referenced_audio_paths().unwrap_or_default().contains(&audio)
                    {
                        let _ = std::fs::remove_file(audio);
                    }
                    this.set_mode(Mode::Empty);
                    this.refresh();
                    this.ctx.toast("Nota borrada");
                }
            },
        );
    }

    fn selected_notebook_id(&self) -> Option<(i64, String)> {
        let name = dropdown_choice(&self.notebooks)?;
        let notebooks = self.ctx.store.list_notebooks().ok()?;
        notebooks.into_iter().find(|n| n.name == name).map(|n| (n.id, n.name))
    }

    fn update_actions(&self) {
        let has_notebook = dropdown_choice(&self.notebooks).is_some();
        for name in ["rename-notebook", "delete-notebook"] {
            if let Some(action) = self.actions.lookup_action(name).and_downcast::<gio::SimpleAction>() {
                action.set_enabled(has_notebook);
            }
        }
    }

    fn rename_notebook(self: &Rc<Self>) {
        let Some((id, name)) = self.selected_notebook_id() else { return };
        let this = self.clone();
        self.ctx.ask_text("Renombrar cuaderno", &name, "Renombrar", move |new_name| {
            if this.ctx.check(this.ctx.store.rename_notebook(id, &new_name)).is_some() {
                this.refresh();
                select_dropdown(&this.notebooks, &new_name);
            }
        });
    }

    fn delete_notebook(self: &Rc<Self>) {
        let Some((id, name)) = self.selected_notebook_id() else { return };
        let this = self.clone();
        self.ctx.confirm(
            &format!("¿Borrar el cuaderno «{name}»?"),
            "Sus notas no se borran: se quedan sin cuaderno.",
            "Borrar",
            move || {
                if this.ctx.check(this.ctx.store.delete_notebook(id)).is_some() {
                    this.notebooks.set_selected(0);
                    this.refresh();
                }
            },
        );
    }
}

/// Primera línea con texto, sin la sintaxis de Markdown más habitual.
fn plain_snippet(markdown: &str) -> String {
    let line = markdown.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default();
    line.trim_start_matches(|c: char| "#>-+* ".contains(c)).chars().filter(|c| !"*_`".contains(*c)).collect()
}

/// Elemento elegido en un desplegable de filtro; `None` si es la opción «todos».
fn dropdown_choice(dropdown: &gtk::DropDown) -> Option<String> {
    if dropdown.selected() == 0 {
        return None;
    }
    dropdown.selected_item().and_downcast::<gtk::StringObject>().map(|s| s.string().to_string())
}

/// Rellena un desplegable conservando la opción elegida si sigue existiendo.
fn refill_dropdown(dropdown: &gtk::DropDown, all: &str, names: &[String]) {
    let current = dropdown_choice(dropdown);
    let mut items = vec![all];
    items.extend(names.iter().map(String::as_str));
    dropdown.set_model(Some(&gtk::StringList::new(&items)));
    let index = current.and_then(|c| names.iter().position(|n| *n == c)).map_or(0, |i| i + 1);
    dropdown.set_selected(index as u32);
}

fn select_dropdown(dropdown: &gtk::DropDown, name: &str) {
    let Some(model) = dropdown.model() else { return };
    for i in 0..model.n_items() {
        if model.item(i).and_downcast::<gtk::StringObject>().is_some_and(|s| s.string() == name) {
            dropdown.set_selected(i);
            return;
        }
    }
}

/// Usa el esquema de colores claro u oscuro según el sistema.
fn follow_dark_mode(buffer: &sourceview5::Buffer) {
    let style = adw::StyleManager::default();
    let apply = glib::clone!(
        #[weak]
        buffer,
        move |style: &adw::StyleManager| {
            let name = if style.is_dark() { "Adwaita-dark" } else { "Adwaita" };
            buffer.set_style_scheme(sourceview5::StyleSchemeManager::default().scheme(name).as_ref());
        }
    );
    apply(&style);
    style.connect_dark_notify(apply);
}
