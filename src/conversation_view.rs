//! Vista de una conversación (mensajes, razonamiento, tools, errores), compartida
//! por el popup y el explorador.

use adw::prelude::*;
use gtk::glib;

use crate::llm::DisplayItem;
use crate::markdown;
use crate::store::{MessageKind, MessageRecord};

pub const CSS: &str = "
.user-bubble {
    background: alpha(@accent_bg_color, 0.15);
    border-radius: 12px;
    padding: 8px 12px;
}
.assistant-text { padding: 0 4px; }
.reasoning-text { padding: 4px 8px; }
.tool-call {
    border-radius: 8px;
    padding: 6px 10px;
    background: alpha(@card_fg_color, 0.06);
}
.confirmation { padding: 12px; }
.note-body { padding: 0 2px; }
";

#[derive(Clone)]
pub struct ConversationView {
    scroller: gtk::ScrolledWindow,
    messages: gtk::Box,
}

impl ConversationView {
    pub fn new() -> Self {
        let messages = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        let clamp = adw::Clamp::builder().maximum_size(760).child(&messages).build();
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&clamp)
            .build();
        Self { scroller, messages }
    }

    pub fn widget(&self) -> &gtk::ScrolledWindow {
        &self.scroller
    }

    pub fn clear(&self) {
        while let Some(child) = self.messages.first_child() {
            self.messages.remove(&child);
        }
    }

    pub fn add_user(&self, text: &str, audio: Option<&str>) {
        let label = wrapped_label();
        label.set_text(text);
        label.add_css_class("user-bubble");
        let row = gtk::Box::builder().spacing(4).halign(gtk::Align::End).build();
        if let Some(button) = audio.and_then(play_button) {
            button.set_valign(gtk::Align::Center);
            row.append(&button);
        }
        row.append(&label);
        self.append(&row);
    }

    pub fn add_items(&self, items: &[DisplayItem]) {
        for item in items {
            match item {
                DisplayItem::Reasoning(text) => {
                    let label = wrapped_label();
                    label.set_text(text);
                    label.add_css_class("dim-label");
                    label.add_css_class("reasoning-text");
                    let expander = gtk::Expander::builder().label("Razonamiento").child(&label).build();
                    expander.add_css_class("dim-label");
                    self.append(&expander);
                }
                DisplayItem::Text(text) => {
                    let label = markdown_label(text);
                    label.add_css_class("assistant-text");
                    self.append(&label);
                }
                DisplayItem::Tool(result) => self.add_tool(&result.summary, &result.content, result.is_error),
            }
        }
    }

    /// Reconstruye un mensaje guardado en el historial.
    pub fn add_record(&self, record: &MessageRecord) {
        match record.kind {
            MessageKind::User => self.add_user(&record.content, record.audio_path.as_deref()),
            MessageKind::Reasoning => self.add_items(&[DisplayItem::Reasoning(record.content.clone())]),
            MessageKind::Text => self.add_items(&[DisplayItem::Text(record.content.clone())]),
            MessageKind::Tool => {
                self.add_tool(&record.content, record.tool_output.as_deref().unwrap_or_default(), record.is_error)
            }
            MessageKind::Error => self.add_error(&record.content),
        }
    }

    pub fn add_tool(&self, summary: &str, detail: &str, is_error: bool) {
        let label = wrapped_label();
        let icon = if is_error { "⚠" } else { "✓" };
        label.set_text(&format!("{icon} {summary}"));
        label.set_tooltip_text(Some(detail));
        label.add_css_class("tool-call");
        label.add_css_class("caption");
        if is_error {
            label.add_css_class("error");
        }
        self.append(&label);
    }

    pub fn add_error(&self, message: &str) {
        let label = wrapped_label();
        label.set_text(message);
        label.add_css_class("error");
        self.append(&label);
    }

    pub fn append(&self, widget: &impl IsA<gtk::Widget>) {
        self.messages.append(widget);
        // Bajar al final cuando GTK haya calculado el nuevo tamaño.
        let adjustment = self.scroller.vadjustment();
        glib::idle_add_local_once(move || adjustment.set_value(adjustment.upper()));
    }
}

pub fn wrapped_label() -> gtk::Label {
    gtk::Label::builder()
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .xalign(0.0)
        .halign(gtk::Align::Fill)
        .selectable(true)
        .build()
}

pub fn markdown_label(text: &str) -> gtk::Label {
    let label = wrapped_label();
    label.set_markup(&markdown::to_pango(text));
    label
}

/// Botón para escuchar un audio; `None` si el archivo ya no existe.
pub fn play_button(path: &str) -> Option<gtk::Button> {
    if !std::path::Path::new(path).exists() {
        return None;
    }
    let media = gtk::MediaFile::for_filename(path);
    let button = gtk::Button::builder()
        .icon_name("media-playback-start-symbolic")
        .tooltip_text("Escuchar la grabación")
        .css_classes(["flat", "circular"])
        .build();
    media.connect_playing_notify(glib::clone!(
        #[weak]
        button,
        move |media| {
            let icon = if media.is_playing() { "media-playback-pause-symbolic" } else { "media-playback-start-symbolic" };
            button.set_icon_name(icon);
        }
    ));
    button.connect_clicked(move |_| {
        if media.is_playing() {
            media.pause();
        } else {
            if media.is_ended() {
                media.seek(0);
            }
            media.play();
        }
    });
    Some(button)
}
