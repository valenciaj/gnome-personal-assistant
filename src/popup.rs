//! Ventana rápida de conversación: transcripción, razonamiento y respuesta.

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::app::Command;
use crate::conversation_view::{ConversationView, wrapped_label};
use crate::i18n::tr;

pub struct Popup {
    window: adw::Window,
    view: ConversationView,
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
            .title(tr("Asistente"))
            .default_width(520)
            .default_height(620)
            .hide_on_close(true)
            .build();

        let header = adw::HeaderBar::new();
        let new_button = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text(tr("Nueva conversación"))
            .build();
        header.pack_start(&new_button);
        let explorer_button = gtk::Button::builder()
            .icon_name("view-list-bullet-symbolic")
            .tooltip_text(tr("Notas, tareas e historial"))
            .build();
        header.pack_end(&explorer_button);

        let view = ConversationView::new();

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
            .placeholder_text(tr("Escribe o pulsa el micrófono…"))
            .hexpand(true)
            .build();
        let mic = gtk::Button::builder()
            .icon_name("audio-input-microphone-symbolic")
            .tooltip_text(tr("Grabar"))
            .build();
        let send = gtk::Button::builder()
            .icon_name("mail-send-symbolic")
            .tooltip_text(tr("Enviar"))
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
        body.append(view.widget());
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
        for (button, command) in [
            (&mic, Command::TogglePopupRecording),
            (&new_button, Command::NewTextConversation),
            (&explorer_button, Command::ShowExplorer),
        ] {
            let commands = commands.clone();
            button.connect_clicked(move |_| {
                let _ = commands.try_send(command.clone());
            });
        }

        Self { window, view, status, status_label, entry, mic, send }
    }

    pub fn window(&self) -> &adw::Window {
        &self.window
    }

    pub fn view(&self) -> &ConversationView {
        &self.view
    }

    /// Muestra la ventana. Con el token de activación del atajo, GNOME le da el foco.
    pub fn present(&self, activation_token: Option<&str>) {
        if let Some(token) = activation_token {
            self.window.set_startup_id(token);
        }
        // Dar el foco a la entrada antes de mostrar evita que GTK enfoque (y
        // seleccione entero) el primer mensaje.
        GtkWindowExt::set_focus(&self.window, Some(&self.entry));
        self.window.present();
    }

    pub fn is_active(&self) -> bool {
        self.window.is_visible() && self.window.is_active()
    }

    /// Pregunta si se borra algo; la respuesta vuelve a la tool por `reply`.
    pub fn ask_confirmation(&self, question: &str, reply: async_channel::Sender<bool>) {
        let label = wrapped_label();
        label.set_text(question);
        label.set_selectable(false);

        let cancel = gtk::Button::with_label(&tr("Cancelar"));
        let delete = gtk::Button::builder().label(tr("Borrar")).css_classes(["destructive-action"]).build();
        let buttons = gtk::Box::builder().spacing(6).halign(gtk::Align::End).build();
        buttons.append(&cancel);
        buttons.append(&delete);

        let card = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(10).build();
        card.add_css_class("card");
        card.add_css_class("confirmation");
        card.append(&label);
        card.append(&buttons);
        self.view.append(&card);
        self.set_status(Some(&tr("Esperando confirmación…")));
        delete.grab_focus();

        let answer = Rc::new({
            let status = self.status_label.clone();
            move |confirmed: bool| {
                let _ = reply.try_send(confirmed);
                card.remove(&buttons);
                let result = gtk::Label::builder()
                    .label(if confirmed { tr("Borrado confirmado") } else { tr("Borrado cancelado") })
                    .halign(gtk::Align::End)
                    .css_classes(["dim-label", "caption"])
                    .build();
                card.append(&result);
                status.set_text(&tr("Pensando…"));
            }
        });
        delete.connect_clicked({
            let answer = answer.clone();
            move |_| answer(true)
        });
        cancel.connect_clicked(move |_| answer(false));
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
            self.mic.set_tooltip_text(Some(&tr("Parar y enviar")));
            self.mic.add_css_class("destructive-action");
        } else {
            self.mic.set_icon_name("audio-input-microphone-symbolic");
            self.mic.set_tooltip_text(Some(&tr("Grabar")));
            self.mic.remove_css_class("destructive-action");
        }
    }
}
