//! Recordatorios: pendientes y completados, con texto, audios e imágenes.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use super::{Ctx, clear_list, local_time, placeholder, select_by_name, sidebar, split, text_row, toolbar_box};
use crate::app::Command;
use crate::attachment_view::{AddOptions, add_button, attachments_widget};
use crate::conversation_view::markdown_label;
use crate::i18n::{n_, tr, trf};
use crate::store::{Attachment, NewAttachment, NewReminder, Owner, ReminderUpdate, Repeat};

const REPEAT_LABELS: [&str; 5] = [n_("Sin repetición"), n_("Cada día"), n_("Cada semana"), n_("Cada mes"), n_("Cada año")];

fn repeat_label(repeat: Repeat) -> String {
    let index = Repeat::ALL.iter().position(|r| *r == repeat).unwrap_or(0);
    tr(REPEAT_LABELS[index])
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Empty,
    View(i64),
    /// `None` = recordatorio nuevo.
    Edit(Option<i64>),
}

pub struct Page {
    ctx: Ctx,
    root: gtk::Paned,
    pending_button: gtk::ToggleButton,
    list: gtk::ListBox,
    detail: gtk::Stack,
    // Vista
    view_title: gtk::Label,
    view_meta: gtk::Label,
    view_body: gtk::Box,
    view_attachments: gtk::Box,
    done_button: gtk::Button,
    // Edición
    edit_heading: gtk::Label,
    edit_title: gtk::Entry,
    edit_when: gtk::Entry,
    edit_repeat: gtk::DropDown,
    edit_body: gtk::TextBuffer,
    edit_attachments: gtk::Box,
    edit_pending: RefCell<Vec<NewAttachment>>,
    mode: Cell<Mode>,
    refreshing: Cell<bool>,
}

impl Page {
    pub fn new(ctx: Ctx) -> Rc<Self> {
        // ── Panel izquierdo ──
        let pending_button = gtk::ToggleButton::builder().label(tr("Pendientes")).active(true).hexpand(true).build();
        let done_filter = gtk::ToggleButton::builder().label(tr("Completados")).group(&pending_button).hexpand(true).build();
        let filter = gtk::Box::builder().css_classes(["linked"]).hexpand(true).build();
        filter.append(&pending_button);
        filter.append(&done_filter);
        let new_button = gtk::Button::builder().icon_name("list-add-symbolic").tooltip_text(tr("Nuevo recordatorio")).build();
        let tools = toolbar_box();
        tools.append(&filter);
        tools.append(&new_button);
        let list = gtk::ListBox::new();
        let left = sidebar(&tools, &list);

        // ── Vista ──
        let view_title = gtk::Label::builder().xalign(0.0).wrap(true).hexpand(true).selectable(true).css_classes(["title-2"]).build();
        let done_button = gtk::Button::builder().label(tr("Hecho")).css_classes(["suggested-action"]).valign(gtk::Align::Center).build();
        let edit = gtk::Button::builder().icon_name("document-edit-symbolic").tooltip_text(tr("Editar")).valign(gtk::Align::Center).build();
        let delete = gtk::Button::builder().icon_name("user-trash-symbolic").tooltip_text(tr("Borrar")).valign(gtk::Align::Center).build();
        let view_header = toolbar_box();
        view_header.set_margin_start(12);
        view_header.append(&view_title);
        view_header.append(&done_button);
        view_header.append(&edit);
        view_header.append(&delete);
        let view_meta = gtk::Label::builder().xalign(0.0).wrap(true).css_classes(["dim-label"]).build();
        let view_body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let view_attachments = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let view_content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(12)
            .margin_bottom(24)
            .margin_start(12)
            .margin_end(12)
            .build();
        view_content.append(&view_meta);
        view_content.append(&view_body);
        view_content.append(&view_attachments);
        let view_page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        view_page.append(&view_header);
        view_page.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        view_page.append(
            &gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vexpand(true)
                .child(&adw::Clamp::builder().maximum_size(800).child(&view_content).build())
                .build(),
        );

        // ── Edición ──
        let edit_heading = gtk::Label::builder().xalign(0.0).hexpand(true).css_classes(["heading"]).build();
        let cancel = gtk::Button::builder().label(tr("Cancelar")).valign(gtk::Align::Center).build();
        let save = gtk::Button::builder().label(tr("Guardar")).css_classes(["suggested-action"]).valign(gtk::Align::Center).build();
        let edit_header = toolbar_box();
        edit_header.set_margin_start(12);
        edit_header.append(&edit_heading);
        edit_header.append(&cancel);
        edit_header.append(&save);
        let edit_title = gtk::Entry::builder().placeholder_text(tr("Título")).build();
        let edit_when = gtk::Entry::builder().placeholder_text(tr("Fecha y hora (AAAA-MM-DD HH:MM)")).hexpand(true).build();
        let repeat_labels: Vec<String> = REPEAT_LABELS.iter().map(|l| tr(l)).collect();
        let edit_repeat = gtk::DropDown::from_strings(&repeat_labels.iter().map(String::as_str).collect::<Vec<_>>());
        let when_row = gtk::Box::builder().spacing(6).build();
        when_row.append(&edit_when);
        when_row.append(&edit_repeat);
        let edit_body = gtk::TextBuffer::new(None);
        let body_view = gtk::TextView::builder()
            .buffer(&edit_body)
            .wrap_mode(gtk::WrapMode::WordChar)
            .top_margin(8)
            .bottom_margin(8)
            .left_margin(8)
            .right_margin(8)
            .build();
        let body_scroller = gtk::ScrolledWindow::builder().min_content_height(160).vexpand(true).child(&body_view).css_classes(["card"]).build();
        let attachments_row = gtk::Box::builder().spacing(6).build();
        attachments_row.append(&gtk::Label::builder().label(tr("Adjuntos")).css_classes(["heading"]).hexpand(true).xalign(0.0).build());
        let edit_attachments = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).build();
        let edit_content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        for widget in [
            edit_title.upcast_ref::<gtk::Widget>(),
            when_row.upcast_ref(),
            body_scroller.upcast_ref(),
            attachments_row.upcast_ref(),
            edit_attachments.upcast_ref(),
        ] {
            edit_content.append(widget);
        }
        let edit_page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        edit_page.append(&edit_header);
        edit_page.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        edit_page.append(&gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).child(&edit_content).build());

        let detail = gtk::Stack::new();
        detail.add_named(
            &placeholder("alarm-symbolic", &tr("Sin recordatorio"), &tr("Elige un recordatorio o crea uno nuevo.")),
            Some("empty"),
        );
        detail.add_named(&view_page, Some("view"));
        detail.add_named(&edit_page, Some("edit"));

        let root = split(&left, &detail);
        let this = Rc::new(Self {
            ctx,
            root,
            pending_button,
            list,
            detail,
            view_title,
            view_meta,
            view_body,
            view_attachments,
            done_button,
            edit_heading,
            edit_title,
            edit_when,
            edit_repeat,
            edit_body,
            edit_attachments,
            edit_pending: RefCell::default(),
            mode: Cell::new(Mode::Empty),
            refreshing: Cell::new(false),
        });

        let weak = Rc::downgrade(&this);
        let toast = this.ctx.clone();
        attachments_row.append(&add_button(
            AddOptions::ImagesAndAudio,
            this.ctx.stt.clone(),
            Rc::new(move |attachment| {
                if let Some(page) = weak.upgrade() {
                    page.add_attachment(attachment);
                }
            }),
            Rc::new(move |message| toast.toast(&message)),
        ));
        this.pending_button.connect_toggled(glib::clone!(
            #[weak]
            this,
            move |_| {
                this.set_mode(Mode::Empty);
                this.fill_list();
            }
        ));
        this.list.connect_row_selected(glib::clone!(
            #[weak]
            this,
            move |_, row| {
                if !this.refreshing.get()
                    && let Some(id) = row.and_then(|r| r.widget_name().parse().ok())
                {
                    this.show(id);
                }
            }
        ));
        new_button.connect_clicked(glib::clone!(
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
        this.done_button.connect_clicked(glib::clone!(
            #[weak]
            this,
            move |_| this.toggle_done()
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
        this
    }

    pub fn widget(&self) -> &gtk::Paned {
        &self.root
    }

    pub fn list(&self) -> &gtk::ListBox {
        &self.list
    }

    pub fn refresh(self: &Rc<Self>) {
        self.fill_list();
        if let Mode::View(id) = self.mode.get() {
            self.show(id);
        }
    }

    fn fill_list(&self) {
        let done = !self.pending_button.is_active();
        let Some(reminders) = self.ctx.check(self.ctx.store.list_reminders(done)) else { return };
        self.refreshing.set(true);
        clear_list(&self.list);
        let now = chrono::Utc::now().timestamp();
        for reminder in &reminders {
            let mut meta = vec![local_time(reminder.due_at)];
            if !done && reminder.due_at < now {
                meta[0] = trf("⚠ {date}", &[("date", &meta[0])]);
            }
            if reminder.repeat != Repeat::None {
                meta.push(repeat_label(reminder.repeat));
            }
            let audios = reminder.attachments.iter().filter(|a| a.kind == crate::store::AttachmentKind::Audio).count();
            let images = reminder.attachments.len() - audios;
            if audios > 0 {
                meta.push(format!("🎤{audios}"));
            }
            if images > 0 {
                meta.push(format!("🖼{images}"));
            }
            let row = text_row(&reminder.title, &meta.join(" · "));
            row.set_widget_name(&reminder.id.to_string());
            self.list.append(&row);
        }
        if let Mode::View(id) | Mode::Edit(Some(id)) = self.mode.get() {
            select_by_name(&self.list, &id.to_string());
        }
        self.refreshing.set(false);
    }

    fn set_mode(&self, mode: Mode) {
        if !matches!(mode, Mode::View(_)) {
            clear(&self.view_attachments);
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

    fn show(&self, id: i64) {
        let Some(reminder) = self.ctx.check(self.ctx.store.get_reminder(id)) else {
            self.set_mode(Mode::Empty);
            return;
        };
        let changed = self.mode.get() != Mode::View(id);
        self.view_title.set_text(&reminder.title);
        let mut meta = vec![local_time(reminder.due_at), repeat_label(reminder.repeat)];
        if let Some(completed) = reminder.completed {
            meta.push(trf("completado {date}", &[("date", &local_time(completed))]));
        }
        self.view_meta.set_text(&meta.join(" · "));
        self.done_button.set_label(&if reminder.done { tr("Reabrir") } else { tr("Hecho") });
        clear(&self.view_body);
        if !reminder.body.trim().is_empty() {
            let body = markdown_label(&reminder.body);
            body.add_css_class("note-body");
            self.view_body.append(&body);
        }
        if changed || self.view_attachments.first_child().is_none() {
            clear(&self.view_attachments);
            if !reminder.attachments.is_empty() {
                self.view_attachments.append(&attachments_widget(&reminder.attachments, None));
            }
        }
        self.set_mode(Mode::View(id));
    }

    fn toggle_done(&self) {
        let Mode::View(id) = self.mode.get() else { return };
        let Some(reminder) = self.ctx.check(self.ctx.store.get_reminder(id)) else { return };
        if reminder.done {
            self.ctx.check(self.ctx.store.reopen_reminder(id));
        } else {
            // Igual que el botón de la notificación: si se repite, pasa a la siguiente fecha.
            self.ctx.send(Command::ReminderDone(id));
            return;
        }
        self.fill_list();
        self.show(id);
    }

    fn start_editing(self: &Rc<Self>, id: Option<i64>) {
        self.edit_pending.borrow_mut().clear();
        match id {
            Some(id) => {
                let Some(reminder) = self.ctx.check(self.ctx.store.get_reminder(id)) else { return };
                self.edit_heading.set_text(&tr("Editar recordatorio"));
                self.edit_title.set_text(&reminder.title);
                self.edit_when.set_text(&local_time(reminder.due_at));
                self.edit_repeat.set_selected(Repeat::ALL.iter().position(|r| *r == reminder.repeat).unwrap_or(0) as u32);
                self.edit_body.set_text(&reminder.body);
            }
            None => {
                self.list.unselect_all();
                self.edit_heading.set_text(&tr("Nuevo recordatorio"));
                self.edit_title.set_text("");
                let in_an_hour = chrono::Utc::now().timestamp() + 3600;
                self.edit_when.set_text(&local_time(in_an_hour - in_an_hour % 300));
                self.edit_repeat.set_selected(0);
                self.edit_body.set_text("");
            }
        }
        self.set_mode(Mode::Edit(id));
        self.show_edit_attachments();
        self.edit_title.grab_focus();
    }

    fn save(self: &Rc<Self>) {
        let Mode::Edit(id) = self.mode.get() else { return };
        let Some(due_at) = self.ctx.check(crate::tools::parse_local_datetime(&self.edit_when.text())) else { return };
        let repeat = Repeat::ALL[self.edit_repeat.selected() as usize];
        let (start, end) = self.edit_body.bounds();
        let body = self.edit_body.text(&start, &end, false).to_string();
        let title = self.edit_title.text().to_string();
        let store = &self.ctx.store;
        let saved = match id {
            Some(id) => store
                .update_reminder(id, ReminderUpdate { title: Some(title), body: Some(body), due_at: Some(due_at), repeat: Some(repeat) })
                .map(|_| id),
            None => store.create_reminder(NewReminder { title, body, due_at, repeat, conversation_id: None }).and_then(|id| {
                for attachment in self.edit_pending.take() {
                    store.add_attachment(Owner::Reminder(id), &attachment)?;
                }
                Ok(id)
            }),
        };
        if let Some(id) = self.ctx.check(saved) {
            self.pending_button.set_active(true);
            self.mode.set(Mode::View(id));
            self.fill_list();
            self.show(id);
            self.ctx.toast(&tr("Recordatorio guardado"));
        }
    }

    fn delete_current(self: &Rc<Self>) {
        let Mode::View(id) = self.mode.get() else { return };
        let Some(reminder) = self.ctx.check(self.ctx.store.get_reminder(id)) else { return };
        let this = self.clone();
        self.ctx.confirm(
            &trf("¿Borrar «{title}»?", &[("title", &reminder.title)]),
            &tr("El recordatorio y sus adjuntos se borrarán definitivamente."),
            &tr("Borrar"),
            move || {
                if this.ctx.check(this.ctx.store.delete_reminder(id)).is_some() {
                    this.set_mode(Mode::Empty);
                    this.fill_list();
                    this.ctx.toast(&tr("Recordatorio borrado"));
                }
            },
        );
    }

    fn add_attachment(self: &Rc<Self>, attachment: NewAttachment) {
        match self.mode.get() {
            Mode::Edit(Some(id)) => {
                self.ctx.check(self.ctx.store.add_attachment(Owner::Reminder(id), &attachment));
            }
            Mode::Edit(None) => self.edit_pending.borrow_mut().push(attachment),
            _ => return,
        }
        self.show_edit_attachments();
    }

    /// Adjuntos editables. Los pendientes de un recordatorio nuevo llevan ids negativos.
    fn show_edit_attachments(self: &Rc<Self>) {
        clear(&self.edit_attachments);
        let mut attachments: Vec<Attachment> = match self.mode.get() {
            Mode::Edit(Some(id)) => self.ctx.check(self.ctx.store.attachments(Owner::Reminder(id))).unwrap_or_default(),
            _ => Vec::new(),
        };
        attachments.extend(self.edit_pending.borrow().iter().enumerate().map(|(i, a)| Attachment {
            id: -(i as i64) - 1,
            kind: a.kind,
            path: a.path.clone(),
            mime: a.mime.clone(),
            transcript: a.transcript.clone(),
            created: 0,
        }));
        let weak = Rc::downgrade(self);
        let remove = Rc::new(move |id: i64| {
            let Some(page) = weak.upgrade() else { return };
            if id < 0 {
                let index = (-id - 1) as usize;
                if index < page.edit_pending.borrow().len() {
                    page.edit_pending.borrow_mut().remove(index);
                }
            } else {
                page.ctx.check(page.ctx.store.remove_attachment(id));
            }
            let page = page.clone();
            glib::idle_add_local_once(move || page.show_edit_attachments());
        });
        self.edit_attachments.append(&attachments_widget(&attachments, Some(remove)));
    }
}

fn clear(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}
