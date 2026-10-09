//! Ventana rápida de conversación: transcripción, razonamiento y respuesta.

use adw::prelude::*;
use gtk::glib;

use crate::app::Command;
use crate::llm::DisplayItem;
use crate::markdown;

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
";

pub struct Popup {
    window: adw::Window,
    messages: gtk::Box,
    scroller: gtk::ScrolledWindow,
    status: gtk::Box,
    status_label: gtk::Label,
    entry: gtk::Entry,
    mic: gtk::Button,
    send: gtk::Button,
}

impl Popup {
    pub fn new(app: &adw::Application, commands: async_channel::Sender<Command>) -> Self {
        let window = adw::Window::builder()
            .application(app)
            .title("Asistente")
            .default_width(520)
            .default_height(620)
            .hide_on_close(true)
            .build();

        let header = adw::HeaderBar::new();
        let new_button = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text("Nueva conversación")
            .build();
        header.pack_start(&new_button);

        let messages = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&messages)
            .build();

        let status_label = gtk::Label::new(None);
        status_label.add_css_class("dim-label");
        let status = gtk::Box::builder()
            .spacing(8)
            .halign(gtk::Align::Center)
            .margin_bottom(6)
            .visible(false)
            .build();
        status.append(&adw::Spinner::new());
        status.append(&status_label);

        let entry = gtk::Entry::builder()
            .placeholder_text("Escribe o pulsa el micrófono…")
            .hexpand(true)
            .build();
        let mic = gtk::Button::builder()
            .icon_name("audio-input-microphone-symbolic")
            .tooltip_text("Grabar")
            .build();
        let send = gtk::Button::builder()
            .icon_name("paper-plane-symbolic")
            .tooltip_text("Enviar")
            .css_classes(["suggested-action"])
            .build();
        let input = gtk::Box::builder()
            .spacing(6)
            .margin_top(6)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        input.append(&entry);
        input.append(&mic);
        input.append(&send);

        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.append(&scroller);
        body.append(&status);
        body.append(&input);

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&body));
        window.set_content(Some(&toolbar));

        // Esc cierra (oculta) la ventana.
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(glib::clone!(
            #[weak]
            window,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, _| {
                if key == gtk::gdk::Key::Escape {
                    window.close();
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            }
        ));
        window.add_controller(keys);

        let submit = {
            let commands = commands.clone();
            let entry = entry.clone();
            move || {
                let text = entry.text().trim().to_owned();
                if !text.is_empty() {
                    entry.set_text("");
                    let _ = commands.try_send(Command::SendText(text));
                }
            }
        };
        entry.connect_activate({
            let submit = submit.clone();
            move |_| submit()
        });
        send.connect_clicked(move |_| submit());
        mic.connect_clicked({
            let commands = commands.clone();
            move |_| {
                let _ = commands.try_send(Command::TogglePopupRecording);
            }
        });
        new_button.connect_clicked(move |_| {
            let _ = commands.try_send(Command::NewTextConversation);
        });

        Self { window, messages, scroller, status, status_label, entry, mic, send }
    }

    /// Muestra la ventana. Con el token de activación del atajo, GNOME le da el foco.
    pub fn present(&self, activation_token: Option<&str>) {
        if let Some(token) = activation_token {
            self.window.set_startup_id(token);
        }
        self.window.present();
        self.entry.grab_focus();
    }

    pub fn is_active(&self) -> bool {
        self.window.is_visible() && self.window.is_active()
    }

    pub fn clear(&self) {
        while let Some(child) = self.messages.first_child() {
            self.messages.remove(&child);
        }
    }

    pub fn add_user(&self, text: &str) {
        let label = wrapped_label();
        label.set_text(text);
        label.add_css_class("user-bubble");
        label.set_halign(gtk::Align::End);
        self.append(&label);
    }

    pub fn add_items(&self, items: &[DisplayItem]) {
        for item in items {
            match item {
                DisplayItem::Reasoning(text) => {
                    let label = wrapped_label();
                    label.set_text(text);
                    label.add_css_class("dim-label");
                    label.add_css_class("reasoning-text");
                    let expander = gtk::Expander::builder()
                        .label("Razonamiento")
                        .child(&label)
                        .build();
                    expander.add_css_class("dim-label");
                    self.append(&expander);
                }
                DisplayItem::Text(text) => {
                    let label = wrapped_label();
                    label.set_markup(&markdown::to_pango(text));
                    label.add_css_class("assistant-text");
                    self.append(&label);
                }
                DisplayItem::Tool(result) => {
                    let label = wrapped_label();
                    let icon = if result.is_error { "⚠" } else { "⚙" };
                    label.set_text(&format!("{icon} {}", result.call.name));
                    label.set_tooltip_text(Some(&result.content));
                    label.add_css_class("tool-call");
                    label.add_css_class("caption");
                    self.append(&label);
                }
            }
        }
    }

    pub fn add_error(&self, message: &str) {
        let label = wrapped_label();
        label.set_text(message);
        label.add_css_class("error");
        self.append(&label);
    }

    /// `Some(texto)` muestra el indicador de actividad y bloquea la entrada.
    pub fn set_status(&self, status: Option<&str>) {
        self.status.set_visible(status.is_some());
        self.status_label.set_text(status.unwrap_or_default());
        let busy = status.is_some();
        self.entry.set_sensitive(!busy);
        self.send.set_sensitive(!busy);
    }

    pub fn set_recording(&self, recording: bool) {
        if recording {
            self.mic.set_icon_name("media-playback-stop-symbolic");
            self.mic.set_tooltip_text(Some("Parar y enviar"));
            self.mic.add_css_class("destructive-action");
        } else {
            self.mic.set_icon_name("audio-input-microphone-symbolic");
            self.mic.set_tooltip_text(Some("Grabar"));
            self.mic.remove_css_class("destructive-action");
        }
    }

    fn append(&self, widget: &impl IsA<gtk::Widget>) {
        self.messages.append(widget);
        // Bajar al final cuando GTK haya calculado el nuevo tamaño.
        let adjustment = self.scroller.vadjustment();
        glib::idle_add_local_once(move || adjustment.set_value(adjustment.upper()));
    }
}

fn wrapped_label() -> gtk::Label {
    gtk::Label::builder()
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .xalign(0.0)
        .halign(gtk::Align::Fill)
        .selectable(true)
        .build()
}
