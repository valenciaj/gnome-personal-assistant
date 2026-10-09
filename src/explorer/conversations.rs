//! Historial de conversaciones: leerlas, continuarlas y borrarlas.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use super::{Ctx, clear_list, local_time, placeholder, select_by_name, sidebar, split, text_row, toolbar_box};
use crate::app::Command;
use crate::conversation_view::ConversationView;
use crate::store::ConversationInfo;
use crate::i18n::tr;

pub struct Page {
    ctx: Ctx,
    root: gtk::Paned,
    search: gtk::SearchEntry,
    list: gtk::ListBox,
    detail: gtk::Stack,
    title: gtk::Label,
    subtitle: gtk::Label,
    view: ConversationView,
    conversations: RefCell<Vec<ConversationInfo>>,
    selected: Cell<Option<i64>>,
    refreshing: Cell<bool>,
}

impl Page {
    pub fn new(ctx: Ctx) -> Rc<Self> {
        let search = gtk::SearchEntry::builder().placeholder_text(tr("Buscar conversaciones")).hexpand(true).build();
        let tools = toolbar_box();
        tools.append(&search);
        let list = gtk::ListBox::new();
        let left = sidebar(&tools, &list);

        let title = gtk::Label::builder().xalign(0.0).wrap(true).css_classes(["title-3"]).build();
        let subtitle = gtk::Label::builder().xalign(0.0).css_classes(["dim-label", "caption"]).build();
        let titles = gtk::Box::builder().orientation(gtk::Orientation::Vertical).hexpand(true).build();
        titles.append(&title);
        titles.append(&subtitle);
        let resume = gtk::Button::builder()
            .label(tr("Continuar"))
            .tooltip_text(tr("Abrir en el popup para seguir la conversación"))
            .css_classes(["suggested-action"])
            .valign(gtk::Align::Center)
            .build();
        let delete = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text(tr("Borrar la conversación"))
            .valign(gtk::Align::Center)
            .build();
        let header = toolbar_box();
        header.set_margin_start(12);
        header.append(&titles);
        header.append(&resume);
        header.append(&delete);

        let view = ConversationView::new();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&header);
        content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        content.append(view.widget());

        let detail = gtk::Stack::new();
        detail.add_named(
            &placeholder("user-available-symbolic", &tr("Sin conversación"), &tr("Elige una conversación de la lista.")),
            Some("empty"),
        );
        detail.add_named(&content, Some("detail"));

        let root = split(&left, &detail);
        let this = Rc::new(Self {
            ctx,
            root,
            search,
            list,
            detail,
            title,
            subtitle,
            view,
            conversations: RefCell::default(),
            selected: Cell::new(None),
            refreshing: Cell::new(false),
        });

        this.search.connect_search_changed(glib::clone!(
            #[weak]
            this,
            move |_| this.fill_list()
        ));
        this.list.connect_row_selected(glib::clone!(
            #[weak]
            this,
            move |_, row| {
                if this.refreshing.get() {
                    return;
                }
                match row.and_then(|r| r.widget_name().parse().ok()) {
                    Some(id) => this.show(id),
                    None => this.detail.set_visible_child_name("empty"),
                }
            }
        ));
        resume.connect_clicked(glib::clone!(
            #[weak]
            this,
            move |_| {
                if let Some(id) = this.selected.get() {
                    this.ctx.send(Command::OpenConversation(id));
                }
            }
        ));
        delete.connect_clicked(glib::clone!(
            #[weak]
            this,
            move |_| this.delete_selected()
        ));
        this
    }

    pub fn widget(&self) -> &gtk::Paned {
        &self.root
    }

    pub fn list(&self) -> &gtk::ListBox {
        &self.list
    }

    pub fn refresh(&self) {
        if let Some(conversations) = self.ctx.check(self.ctx.store.list_conversations(500)) {
            *self.conversations.borrow_mut() = conversations;
        }
        self.fill_list();
        if let Some(id) = self.selected.get() {
            self.show(id);
        }
    }

    fn fill_list(&self) {
        self.refreshing.set(true);
        clear_list(&self.list);
        let query = self.search.text().to_lowercase();
        for c in self.conversations.borrow().iter().filter(|c| c.title.to_lowercase().contains(&query)) {
            let row = text_row(&c.title, &format!("{} · {}", local_time(c.modified), c.model));
            row.set_widget_name(&c.id.to_string());
            self.list.append(&row);
        }
        let still_there = self.selected.get().is_some_and(|id| select_by_name(&self.list, &id.to_string()));
        if !still_there {
            self.selected.set(None);
            self.detail.set_visible_child_name("empty");
        }
        self.refreshing.set(false);
    }

    fn show(&self, id: i64) {
        let Some(info) = self.ctx.check(self.ctx.store.get_conversation(id)) else { return };
        let Some(records) = self.ctx.check(self.ctx.store.messages(id)) else { return };
        self.selected.set(Some(id));
        self.title.set_text(&info.title);
        self.subtitle.set_text(&format!(
            "{} · {} · {}",
            local_time(info.created),
            crate::config::LlmProvider::from_id(&info.provider).map_or(info.provider.clone(), |p| p.label()),
            info.model
        ));
        self.view.clear();
        for record in &records {
            self.view.add_record(record);
        }
        self.detail.set_visible_child_name("detail");
    }

    fn delete_selected(self: &Rc<Self>) {
        let Some(id) = self.selected.get() else { return };
        let this = self.clone();
        self.ctx.confirm(
            &tr("¿Borrar la conversación?"),
            &tr("Se borran sus mensajes y grabaciones. Las notas y tareas creadas en ella se conservan."),
            &tr("Borrar"),
            move || {
                // Sus archivos los borra la limpieza periódica si nada más los usa.
                if this.ctx.check(this.ctx.store.delete_conversation(id)).is_some() {
                    this.selected.set(None);
                    this.refresh();
                    this.ctx.toast(&tr("Conversación borrada"));
                }
            },
        );
    }
}
